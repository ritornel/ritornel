//! Finding the server on this machine. See "Starting, finding and stopping"
//! in docs/design/server.md, steps 1 and 2.
//!
//! 1. Resolve the data directory: `--data-dir`, then `RITORNEL_DATA_DIR`,
//!    then the platform's default.
//! 2. Try the lock on `server.lock`. A server that is alive holds it, and
//!    if it can be had there is none, whatever `server.json` says: nothing
//!    is sent anywhere.
//! 3. Read `server.json`, and believe it only if it names the pid the lock
//!    file names. Ask that endpoint for its public descriptor, and if the
//!    server ID is the one the file names, read the credential file.
//! 4. To connect, make the connection, look at the lock and both files
//!    again, and only if nothing has changed send the credential.
//!
//! The lock is what a `server.json` left behind cannot fake. A server that
//! was killed leaves the file, its port goes to whoever takes it, and its
//! ID is public, so anything can answer there as that server. It cannot
//! hold the lock: only the data directory's owner can open the lock file.
//!
//! Starting a server when none answers, and stopping one, belong to the
//! desktop's launcher, in [`crate::launch`].

use std::fs::{File, TryLockError};
use std::io::{ErrorKind, Read};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How a program reads an environment variable.
pub use ritornel_contract::local::Env;
use ritornel_contract::local::{self, ServerJson};
use ritornel_contract::{CONTRACT_VERSION, PublicDescriptor, SocketTicket};
use tokio::net::TcpStream;

use crate::{Client, Credential, Error, NotRunning, http};

/// How long a server on this machine gets to answer for its descriptor
/// before the client takes it for gone.
const ANSWER_WAIT: Duration = Duration::from_secs(2);

/// Where the data directory is: `flag` (`--data-dir`), then
/// `RITORNEL_DATA_DIR`, then the platform's default, exactly as the server
/// resolves it. A relative path is taken from the current directory.
pub fn data_dir(flag: Option<&Path>, env: Env<'_>) -> Result<PathBuf, Error> {
    let chosen = local::data_dir(flag, env).ok_or(Error::NoDataDir)?;
    std::path::absolute(&chosen).map_err(|source| Error::File {
        doing: "resolve",
        path: chosen,
        source,
    })
}

/// What the lock on a data directory says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lock {
    /// Nothing holds it: no server is running on the directory.
    Free,
    /// A server holds it, and so is alive.
    Held {
        /// The pid the server wrote into the lock file. `None` for the
        /// instant between its taking the lock and writing it.
        pid: Option<u32>,
    },
}

fn file_error(doing: &'static str, path: &Path) -> impl FnOnce(std::io::Error) -> Error {
    let path = path.to_owned();
    move |source| Error::File {
        doing,
        path,
        source,
    }
}

/// Whether the lock or the files of a data directory are simply not there.
/// A data directory that does not exist has no server either.
fn missing(error: &std::io::Error) -> bool {
    matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory)
}

/// Asks the lock on `data_dir` whether a server is alive there.
///
/// It takes the lock shared, without waiting, and lets it go at once. A
/// server has the lock to itself, so that fails exactly while one is
/// alive. A server that starts at the same instant finds the lock taken
/// for those few microseconds and tries again, so asking disturbs nothing.
/// The pid is read after the lock is tried: read first, it could be a dead
/// server's, with a new one starting in between.
///
/// This is here, and not in the contract, because it reads the disk, and
/// the contract only says what the lock file is called and what is in it.
pub fn probe(data_dir: &Path) -> Result<Lock, Error> {
    let path = data_dir.join(local::SERVER_LOCK);
    let mut file = match File::open(&path) {
        Ok(file) => file,
        Err(error) if missing(&error) => return Ok(Lock::Free),
        Err(error) => return Err(file_error("open", &path)(error)),
    };
    match file.try_lock_shared() {
        // Dropping the file lets it go.
        Ok(()) => Ok(Lock::Free),
        Err(TryLockError::WouldBlock) => {
            let mut text = String::new();
            file.read_to_string(&mut text)
                .map_err(file_error("read", &path))?;
            Ok(Lock::Held {
                pid: local::lock_holder(&text),
            })
        }
        Err(TryLockError::Error(error)) => Err(file_error("lock", &path)(error)),
    }
}

fn read(path: &Path) -> Result<Option<String>, Error> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if missing(&error) => Ok(None),
        Err(error) => Err(file_error("read", path)(error)),
    }
}

/// What the server that is alive on `data_dir` wrote in `server.json`. It
/// contacts nothing.
///
/// It is an error unless a server holds the lock and `server.json` is that
/// server's own: both name the same pid.
fn alive(data_dir: &Path) -> Result<ServerJson, Error> {
    let not_running = |why| Error::NotRunning {
        data_dir: data_dir.to_owned(),
        why,
    };
    let path = data_dir.join(local::SERVER_JSON);
    let holder = match probe(data_dir)? {
        Lock::Held { pid } => pid,
        Lock::Free if path.exists() => return Err(not_running(NotRunning::Killed)),
        Lock::Free => return Err(not_running(NotRunning::NoServerJson)),
    };
    let Some(text) = read(&path)? else {
        return Err(not_running(NotRunning::Starting));
    };
    let named: ServerJson = match serde_json::from_str(&text) {
        // A local server listens on loopback, and a file that says
        // otherwise must not send the credential somewhere else.
        Ok(named @ ServerJson { endpoint, .. }) if endpoint.ip().is_loopback() => named,
        _ => return Err(Error::ServerJson { path }),
    };
    // A server takes the lock, writes its pid, and only then removes what
    // an earlier one left. Until it has, the file is not its own.
    if holder != Some(named.pid) {
        return Err(not_running(NotRunning::Starting));
    }
    Ok(named)
}

/// A server that is running on this machine, and the credential to talk to
/// it with.
#[derive(Debug)]
pub struct LocalServer {
    /// Where it listens.
    pub endpoint: SocketAddr,
    /// What it says about itself.
    pub descriptor: PublicDescriptor,
    data_dir: PathBuf,
    named: ServerJson,
    credential: Credential,
}

impl LocalServer {
    /// The process that held the directory lock when this server was found.
    pub fn pid(&self) -> u32 {
        self.named.pid
    }

    /// Opens a connection with the local credential. The first call must
    /// then be [`Client::hello`].
    ///
    /// The connection is made first, and then the lock and `server.json`
    /// are looked at again. If the same server holds the lock before and
    /// after, it was alive, with its port, while the connection was made,
    /// so the connection is to it, and only then is the credential sent.
    pub async fn connect(&self) -> Result<Client, Error> {
        let stream = self.reach().await?;
        Client::upgrade(stream, self.endpoint, &self.credential).await
    }

    /// Trades the local credential for a ticket that opens one socket,
    /// for something that cannot send the credential itself: the desktop's
    /// shell asks for one each time its page connects.
    ///
    /// The credential is sent only once the connection is known to be to
    /// the server, exactly as in [`connect`](Self::connect).
    pub async fn ticket(&self) -> Result<SocketTicket, Error> {
        let stream = self.reach().await?;
        http::ticket_over(stream, self.endpoint, &self.credential).await
    }

    /// A connection that is known to be to the server, with nothing sent
    /// on it yet.
    async fn reach(&self) -> Result<TcpStream, Error> {
        let stream = http::connect(self.endpoint).await?;
        if alive(&self.data_dir)? != self.named {
            // Another server has the directory now.
            return Err(Error::NotRunning {
                data_dir: self.data_dir.clone(),
                why: NotRunning::Starting,
            });
        }
        Ok(stream)
    }

    /// The local owner's credential, for the routes that are not the
    /// socket. Whoever sends it somewhere must know what is there:
    /// [`connect`](Self::connect) makes sure, and this does not.
    pub fn credential(&self) -> &Credential {
        &self.credential
    }
}

/// Finds the server running on `data_dir`. It fails with
/// [`Error::NotRunning`] if there is none, and never starts one.
///
/// Nothing is contacted unless a server holds the directory's lock and
/// `server.json` is that server's, and the only thing asked for then is
/// the public descriptor, which takes no credential.
///
/// It must be called on a Tokio runtime.
pub async fn find(data_dir: &Path) -> Result<LocalServer, Error> {
    let not_running = |why| Error::NotRunning {
        data_dir: data_dir.to_owned(),
        why,
    };
    let named = alive(data_dir)?;
    if named.contract_version != CONTRACT_VERSION {
        return Err(Error::ContractVersion {
            server: named.contract_version,
        });
    }

    let endpoint = named.endpoint;
    let descriptor = match tokio::time::timeout(ANSWER_WAIT, http::descriptor(endpoint)).await {
        Ok(Ok(descriptor)) if descriptor.server_id == named.server_id => descriptor,
        Ok(Ok(_) | Err(Error::Protocol(_) | Error::Refused { .. })) => {
            return Err(not_running(NotRunning::AnotherServer(endpoint)));
        }
        Ok(Err(_)) | Err(_) => return Err(not_running(NotRunning::NoAnswer(endpoint))),
    };
    if descriptor.contract_version != CONTRACT_VERSION {
        return Err(Error::ContractVersion {
            server: descriptor.contract_version,
        });
    }

    let path = data_dir.join(local::CREDENTIAL);
    let credential = read(&path)?.ok_or_else(|| Error::File {
        doing: "read",
        path,
        source: ErrorKind::NotFound.into(),
    })?;
    Ok(LocalServer {
        endpoint,
        descriptor,
        data_dir: data_dir.to_owned(),
        named,
        credential: Credential::new(credential),
    })
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;

    /// A directory that is removed when the test ends.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "ritornel-client-unit-{}-{name}",
                std::process::id()
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn no_env(_: &str) -> Option<OsString> {
        None
    }

    #[test]
    fn the_data_directory_is_resolved_as_the_server_resolves_it() {
        // Absolute on this platform: Windows takes `/from` as on the
        // current drive.
        let (flag, env) = if cfg!(windows) {
            (r"C:\from\flag", r"C:\from\env")
        } else {
            ("/from/flag", "/from/env")
        };
        assert_eq!(
            data_dir(Some(Path::new(flag)), &no_env).unwrap(),
            Path::new(flag)
        );
        assert_eq!(
            data_dir(Some(Path::new("some/where")), &no_env).unwrap(),
            std::env::current_dir().unwrap().join("some/where")
        );
        let from_env = |name: &str| (name == local::DATA_DIR_VAR).then(|| OsString::from(env));
        assert_eq!(data_dir(None, &from_env).unwrap(), Path::new(env));
        assert!(matches!(data_dir(None, &no_env), Err(Error::NoDataDir)));
    }

    /// Whether `done` comes true within a few seconds. A lock let go is not
    /// free at once while another test spawns a process: the child carries
    /// this process's open files, and their locks, until its exec closes
    /// them.
    fn soon(done: impl Fn() -> bool) -> bool {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !done() {
            if std::time::Instant::now() > deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        true
    }

    /// Holds a data directory's lock as a server does, with this pid in
    /// the lock file.
    fn hold(dir: &Path, pid: &str) -> File {
        let path = dir.join(local::SERVER_LOCK);
        std::fs::write(&path, pid).unwrap();
        let file = File::open(&path).unwrap();
        assert!(
            soon(|| crate::platform::try_hold_as_a_server(&file)),
            "the lock stays held"
        );
        file
    }

    fn server_json(pid: u32, endpoint: &str) -> String {
        format!(
            r#"{{"pid":{pid},"endpoint":"{endpoint}","serverId":"11111111-1111-1111-1111-111111111111","contractVersion":0}}"#
        )
    }

    fn why(found: Result<LocalServer, Error>) -> NotRunning {
        match found {
            Err(Error::NotRunning { why, .. }) => why,
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn a_directory_with_no_server_json_has_no_server() {
        let scratch = Scratch::new("empty");
        for dir in [scratch.0.clone(), scratch.0.join("never/made")] {
            assert_eq!(probe(&dir).unwrap(), Lock::Free);
            let found = find(&dir).await;
            let Err(Error::NotRunning { data_dir, why }) = found else {
                panic!("{found:?}");
            };
            assert_eq!((data_dir, why), (dir, NotRunning::NoServerJson));
        }
    }

    #[test]
    fn the_lock_says_whether_a_server_is_alive_and_which() {
        let scratch = Scratch::new("lock");
        // A lock file nobody holds is what a dead server leaves.
        std::fs::write(scratch.0.join(local::SERVER_LOCK), "4312\n").unwrap();
        assert_eq!(probe(&scratch.0).unwrap(), Lock::Free);

        let held = hold(&scratch.0, "4312\n");
        assert_eq!(probe(&scratch.0).unwrap(), Lock::Held { pid: Some(4312) });
        // Asking took nothing from the holder, however often.
        assert_eq!(probe(&scratch.0).unwrap(), Lock::Held { pid: Some(4312) });
        drop(held);
        assert!(soon(|| probe(&scratch.0).unwrap() == Lock::Free));

        // A server that has the lock and has not written its pid yet.
        let _held = hold(&scratch.0, "");
        assert_eq!(probe(&scratch.0).unwrap(), Lock::Held { pid: None });
    }

    #[tokio::test]
    async fn a_server_json_nobody_alive_wrote_is_not_believed_and_nothing_is_asked() {
        let scratch = Scratch::new("killed");
        // Something is listening where the file points. It would be asked,
        // if the file were believed.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = listener.local_addr().unwrap().to_string();
        let asked = || listener.accept().is_ok();

        // Left by a server that was killed: its lock file, unheld.
        std::fs::write(
            scratch.0.join(local::SERVER_JSON),
            server_json(4312, &endpoint),
        )
        .unwrap();
        std::fs::write(scratch.0.join(local::SERVER_LOCK), "4312\n").unwrap();
        assert_eq!(why(find(&scratch.0).await), NotRunning::Killed);
        // Not even a lock file.
        std::fs::remove_file(scratch.0.join(local::SERVER_LOCK)).unwrap();
        assert_eq!(why(find(&scratch.0).await), NotRunning::Killed);

        // A new server has the lock and has not removed the old file yet:
        // it has written its own pid, or not even that.
        for holder in ["4313\n", ""] {
            let _held = hold(&scratch.0, holder);
            assert_eq!(
                why(find(&scratch.0).await),
                NotRunning::Starting,
                "{holder:?}"
            );
        }
        // It has removed the file and not written its own.
        std::fs::remove_file(scratch.0.join(local::SERVER_JSON)).unwrap();
        let _held = hold(&scratch.0, "4313\n");
        assert_eq!(why(find(&scratch.0).await), NotRunning::Starting);

        assert!(!asked());
    }

    #[tokio::test]
    async fn a_newer_contract_is_reported_even_without_a_readable_descriptor() {
        let scratch = Scratch::new("newer-schema");
        let _held = hold(&scratch.0, "1\n");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let mut named: serde_json::Value =
            serde_json::from_str(&server_json(1, &listener.local_addr().unwrap().to_string()))
                .unwrap();
        named["contractVersion"] = (CONTRACT_VERSION + 1).into();
        std::fs::write(scratch.0.join(local::SERVER_JSON), named.to_string()).unwrap();
        let result = find(&scratch.0).await;
        assert!(
            matches!(result, Err(Error::ContractVersion {server}) if server == CONTRACT_VERSION + 1),
            "{result:?}"
        );
    }

    #[tokio::test]
    async fn a_server_json_that_is_not_one_or_names_another_machine_is_refused() {
        let scratch = Scratch::new("wrong");
        let _held = hold(&scratch.0, "1\n");
        let path = scratch.0.join(local::SERVER_JSON);
        for wrong in [
            String::new(),
            "{}".to_owned(),
            server_json(1, "localhost:4747"),
            server_json(1, "192.0.2.7:4747"),
        ] {
            std::fs::write(&path, &wrong).unwrap();
            let found = find(&scratch.0).await;
            assert!(
                matches!(&found, Err(Error::ServerJson { path: named }) if *named == path),
                "{wrong}: {found:?}"
            );
        }
    }

    #[tokio::test]
    async fn the_server_that_holds_the_lock_is_asked_and_one_that_does_not_answer_is_not_running() {
        let scratch = Scratch::new("silent");
        let _held = hold(&scratch.0, "4312\n");
        // Nothing listens where it says. Another program may take the port
        // at any moment, so either answer is right.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = listener.local_addr().unwrap();
        drop(listener);
        std::fs::write(
            scratch.0.join(local::SERVER_JSON),
            server_json(4312, &endpoint.to_string()),
        )
        .unwrap();
        let why = why(find(&scratch.0).await);
        assert!(
            why == NotRunning::NoAnswer(endpoint) || why == NotRunning::AnotherServer(endpoint),
            "{why:?}"
        );
    }
}
