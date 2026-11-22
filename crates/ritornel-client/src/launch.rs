//! Starting a server on this machine when none is running, and stopping
//! one that was started here: the desktop's launcher. See "Starting,
//! finding and stopping" in docs/design/server.md, steps 2 and 3, "Second
//! start" and "App quit".
//!
//! 1. Find the server, as [`local::find`] does. If one answers, use it.
//! 2. If none is running, spawn `ritornel-server serve --data-dir <dir>
//!    --owned`, hold its standard input, and wait for its ready line.
//! 3. If it says the directory is in use, another launcher won the race:
//!    go back to 1.
//!
//! A server that was found and not started here is never stopped from
//! here. One that was started here stops when its standard input closes,
//! so it cannot outlive this program, however this program ends.
//!
//! A command never launches: a server that died with the command would
//! take the command's job with it.

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};
use tokio::sync::oneshot;

use ritornel_contract::local as agreed;

use crate::local::{self, LocalServer};
use crate::{Error, NotRunning};

/// How long a server that was asked to stop gets before it is killed. What
/// it keeps is durable, so a kill loses only what a crash would.
pub const STOP_WAIT: Duration = Duration::from_secs(15);

/// How long finding keeps trying while another program's server holds the
/// lock and has not said where it listens, before the caller is told so.
const STARTING_WAIT: Duration = Duration::from_secs(2);

/// How long a caller waits for readiness. A pre-migration backup may take
/// longer, so the child keeps running when the wait ends.
const READY_WAIT: Duration = Duration::from_secs(120);

/// How long stderr gets to deliver the startup diagnostic after stdout closes.
const DIAGNOSTIC_WAIT: Duration = Duration::from_secs(2);

/// How often a wait looks again.
const POLL: Duration = Duration::from_millis(25);

/// How many times a start that lost the race goes back to finding.
const RACES: usize = 3;

/// The most of a server's standard error that is kept, to say why it did
/// not start.
const STDERR_KEPT: u64 = 16 * 1024;

/// The server program a launcher starts.
#[derive(Debug, Clone)]
pub struct Program {
    /// The `ritornel-server` executable.
    pub path: PathBuf,
    /// Arguments after `serve --data-dir <dir> --owned`.
    pub args: Vec<OsString>,
    /// Environment variables to set for it, beside the ones it inherits.
    pub env: Vec<(OsString, OsString)>,
}

impl Program {
    /// The executable at `path`, with nothing added.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            args: Vec::new(),
            env: Vec::new(),
        }
    }
}

/// Finds the server on one data directory and, when none is running,
/// starts one that it then owns.
#[derive(Debug)]
pub struct Launcher {
    data_dir: PathBuf,
    program: Option<Program>,
    /// The server started here, from its spawn on, so that a stop can
    /// reach it before it is ready.
    owned: Mutex<Option<Owned>>,
    /// One launch at a time. A second caller waits, and then finds the
    /// server the first one started.
    launching: tokio::sync::Mutex<()>,
}

impl Launcher {
    /// A launcher for `data_dir` that starts `program` when no server is
    /// running. With no program it only finds.
    pub fn new(data_dir: PathBuf, program: Option<Program>) -> Self {
        Self {
            data_dir,
            program,
            owned: Mutex::new(None),
            launching: tokio::sync::Mutex::new(()),
        }
    }

    /// The data directory.
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// Whether a launch is under way.
    pub fn is_launching(&self) -> bool {
        self.launching.try_lock().is_err()
    }

    /// The pid of the server this launcher started, while it holds one.
    pub fn owned_pid(&self) -> Option<u32> {
        self.live_owned().as_ref().map(Owned::pid)
    }

    /// Finds the server running on the data directory, or starts one if
    /// none is and there is a program to start. A server that is found is
    /// used, whoever started it.
    ///
    /// It must be called on a Tokio runtime. It may be dropped at any
    /// point: a server it has spawned is already held by the launcher.
    pub async fn find_or_start(&self) -> Result<LocalServer, Error> {
        let _one_at_a_time = self.launching.lock().await;
        let mut races = 0;
        let mut starting_since = None;
        loop {
            let error = match local::find(&self.data_dir).await {
                Ok(server) => return Ok(server),
                Err(error) => error,
            };
            match (&error, &self.program) {
                (
                    Error::NotRunning {
                        why: NotRunning::NoServerJson | NotRunning::Killed,
                        ..
                    },
                    Some(program),
                ) if races < RACES => {
                    // A cancelled or timed-out caller may have left its
                    // server starting up, even before it acquired the lock.
                    if self.owned_pid().is_some() {
                        return Err(self.still_starting());
                    }
                    races += 1;
                    self.start(program, READY_WAIT).await?;
                }
                // Another program's server is on its way up.
                (
                    Error::NotRunning {
                        why: NotRunning::Starting,
                        ..
                    },
                    _,
                ) if starting_since.get_or_insert_with(Instant::now).elapsed() < STARTING_WAIT => {
                    tokio::time::sleep(POLL).await;
                }
                _ => return Err(error),
            }
        }
    }

    fn still_starting(&self) -> Error {
        Error::NotRunning {
            data_dir: self.data_dir.clone(),
            why: NotRunning::Starting,
        }
    }

    /// Spawns a server and waits for it to be ready. `Ok` when it is, and
    /// when it found the directory in use: either way there is a server to
    /// find now.
    async fn start(&self, program: &Program, wait: Duration) -> Result<(), Error> {
        let (owned, ready) = Owned::spawn(program, &self.data_dir)?;
        self.owned().replace(owned);
        self.await_ready(ready, wait).await
    }

    async fn await_ready(&self, ready: Ready, wait: Duration) -> Result<(), Error> {
        let ready_line = tokio::time::timeout(wait, ready.said_ready).await;
        match ready_line {
            Ok(Ok(())) => return Ok(()),
            Err(_) => return Err(self.still_starting()),
            Ok(Err(_)) => {}
        }
        let owned = self.owned().take();
        let stopped = if let Some(owned) = owned {
            Some(
                tokio::task::spawn_blocking(move || owned.stop(STOP_WAIT))
                    .await
                    .map_err(|error| Error::Start(error.to_string()))?,
            )
        } else {
            None
        };
        if matches!(stopped, Some(Stopped::Exited(status)) if status.code() == Some(i32::from(agreed::EXIT_IN_USE)))
        {
            return Ok(());
        }
        let said = tokio::time::timeout(DIAGNOSTIC_WAIT, ready.stderr)
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or_default();
        let line = said.lines().rev().find(|line| !line.trim().is_empty());
        Err(Error::Start(line.map_or_else(
            || "it ended before it was ready".to_owned(),
            |line| line.trim_start_matches("ritornel-server: ").to_owned(),
        )))
    }

    /// Stops the server this launcher started, if it holds one, and waits
    /// for it: it closes the server's standard input, and kills it if it
    /// has not ended after `wait`. A server that was found is left
    /// running. It blocks.
    pub fn stop_owned(&self, wait: Duration) -> Option<Stopped> {
        let owned = self.live_owned().take()?;
        Some(owned.stop(wait))
    }

    fn live_owned(&self) -> MutexGuard<'_, Option<Owned>> {
        let mut slot = self.owned();
        slot.take_if(Owned::has_exited);
        slot
    }

    fn owned(&self) -> MutexGuard<'_, Option<Owned>> {
        self.owned.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// How a server that was asked to stop ended.
#[derive(Debug)]
pub enum Stopped {
    /// It ended by itself, as this says.
    Exited(ExitStatus),
    /// It had not ended in time and was killed.
    Killed,
}

/// A server process started here. Dropping it closes the server's standard
/// input, which stops it, and does not wait.
#[derive(Debug)]
struct Owned {
    child: Child,
    stdin: Option<ChildStdin>,
}

/// What a spawned server says before it is ready: the ready line, or the
/// end of its output and what it wrote on standard error.
struct Ready {
    said_ready: oneshot::Receiver<()>,
    stderr: oneshot::Receiver<String>,
}

impl Owned {
    fn spawn(program: &Program, data_dir: &Path) -> Result<(Self, Ready), Error> {
        let mut command = Command::new(&program.path);
        // The pipes are all it needs.
        crate::platform::start_without_console(&mut command);
        let mut child = command
            .arg("serve")
            .arg("--data-dir")
            .arg(data_dir)
            .arg("--owned")
            .args(&program.args)
            .envs(program.env.iter().map(|(name, value)| (name, value)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| Error::Start(format!("{}: {error}", program.path.display())))?;
        let stdin = child.stdin.take();
        let (stdout, stderr) = (child.stdout.take(), child.stderr.take());
        let owned = Self { child, stdin };
        let (said, said_ready) = oneshot::channel();
        let (diagnosis, stderr_text) = oneshot::channel();
        // Each pipe is read to its end, so the server never blocks on a
        // full one. The channel closes when standard output ends.
        let reading = std::thread::Builder::new()
            .name("server stdout".to_owned())
            .spawn(move || {
                let Some(stdout) = stdout else { return };
                let mut reader = BufReader::new(stdout);
                let mut line = Vec::new();
                while let Ok(1..) = reader.read_until(b'\n', &mut line) {
                    if line
                        .split(|&byte| matches!(byte, b' ' | b'\r' | b'\n'))
                        .next()
                        == Some(agreed::READY.as_bytes())
                    {
                        let _ = said.send(());
                        let _ = std::io::copy(&mut reader, &mut std::io::sink());
                        break;
                    }
                    line.clear();
                }
            });
        let stderr = std::thread::Builder::new()
            .name("server stderr".to_owned())
            .spawn(move || {
                let mut text = String::new();
                if let Some(mut stderr) = stderr {
                    let _ = (&mut stderr).take(STDERR_KEPT).read_to_string(&mut text);
                    let _ = std::io::copy(&mut stderr, &mut std::io::sink());
                }
                let _ = diagnosis.send(text);
            });
        match (reading, stderr) {
            (Ok(_), Ok(_)) => Ok((
                owned,
                Ready {
                    said_ready,
                    stderr: stderr_text,
                },
            )),
            (Err(error), _) | (_, Err(error)) => {
                let _killed = owned.stop(Duration::ZERO);
                Err(Error::Start(error.to_string()))
            }
        }
    }

    fn has_exited(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(Some(_)))
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Closes the server's standard input, waits up to `wait` for it to
    /// end, and kills it if it has not.
    fn stop(mut self, wait: Duration) -> Stopped {
        drop(self.stdin.take());
        let deadline = Instant::now() + wait;
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => return Stopped::Exited(status),
                Ok(None) if Instant::now() < deadline => std::thread::sleep(POLL),
                _ => break,
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        Stopped::Killed
    }
}

impl Drop for Owned {
    fn drop(&mut self) {
        // The pipe closes with `stdin`. One that has ended is reaped.
        let _ = self.child.try_wait();
    }
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::io::Write;

    use ritornel_contract::{CONTRACT_VERSION, Id, Limits, PublicDescriptor, ServerRole};

    use super::*;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "ritornel-client-launch-{}-{name}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        /// A program that is a script, standing in for the server: `unix`,
        /// for the shell, on Unix, and `windows`, a batch file, on Windows.
        fn program(&self, unix: &str, windows: &str) -> Program {
            Program::new(crate::platform::script(
                &self.0.join("fake-server"),
                unix,
                windows,
            ))
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[tokio::test]
    async fn a_server_that_fails_to_start_is_reported_in_its_own_words() {
        let scratch = Scratch::new("fails");
        let program = scratch.program(
            "echo 'ritornel-server: server.db is newer than this program' >&2; exit 1",
            "1>&2 echo ritornel-server: server.db is newer than this program\nexit /b 1",
        );
        let launcher = Launcher::new(scratch.0.join("data"), Some(program));
        let failed = launcher.find_or_start().await.unwrap_err();
        assert!(
            matches!(&failed, Error::Start(why) if why == "server.db is newer than this program"),
            "{failed:?}"
        );
        assert_eq!(launcher.owned_pid(), None);
    }

    #[tokio::test]
    async fn a_start_that_keeps_losing_the_race_ends_and_says_none_is_running() {
        let scratch = Scratch::new("races");
        let program = scratch.program("exit 3", "exit /b 3");
        let launcher = Launcher::new(scratch.0.join("data"), Some(program));
        let found = launcher.find_or_start().await.unwrap_err();
        assert!(
            matches!(
                found,
                Error::NotRunning {
                    why: NotRunning::NoServerJson,
                    ..
                }
            ),
            "{found:?}"
        );
    }

    #[tokio::test]
    async fn with_no_program_it_only_finds() {
        let scratch = Scratch::new("find-only");
        let launcher = Launcher::new(scratch.0.join("data"), None);
        let found = launcher.find_or_start().await.unwrap_err();
        assert!(matches!(found, Error::NotRunning { .. }), "{found:?}");
        assert!(!scratch.0.join("data").exists());
    }

    #[tokio::test]
    async fn a_diagnostic_can_arrive_after_the_readiness_wait() {
        let scratch = Scratch::new("late-diagnostic");
        let launcher = Launcher::new(scratch.0.clone(), None);
        let (stdout, said_ready) = oneshot::channel();
        let (diagnosis, stderr) = oneshot::channel();
        drop(stdout);
        // Stdout already closed. Even with no readiness time left, stderr gets its own wait.
        let (result, ()) = tokio::join!(
            launcher.await_ready(Ready { said_ready, stderr }, Duration::ZERO),
            async {
                tokio::time::sleep(Duration::from_millis(10)).await;
                let _ = diagnosis.send("ritornel-server: library unavailable".to_owned());
            }
        );
        assert!(matches!(result, Err(Error::Start(why)) if why == "library unavailable"));
    }

    #[cfg(unix)]
    #[test]
    fn stdout_is_drained_through_non_utf8_output_before_ready() {
        let scratch = Scratch::new("bytes-before-ready");
        let program = scratch.program(r"printf '\377\n'; echo 'ready pid=1 server_id=x'", "");
        let (owned, ready) = Owned::spawn(&program, &scratch.0).unwrap();
        let result = ready.said_ready.blocking_recv();
        owned.stop(STOP_WAIT);
        assert!(result.is_ok(), "{result:?}");
    }

    #[tokio::test]
    async fn a_slow_start_can_outlive_the_wait_and_keeps_the_same_child() {
        let scratch = Scratch::new("no-ready");
        let program = scratch.program("cat >/dev/null", "findstr \"^\" >nul\nexit /b 0");
        let launcher = Launcher::new(scratch.0.clone(), Some(program.clone()));
        let result = launcher.start(&program, Duration::from_millis(50)).await;
        assert!(
            matches!(
                result,
                Err(Error::NotRunning {
                    why: NotRunning::Starting,
                    ..
                })
            ),
            "{result:?}"
        );
        let pid = launcher.owned_pid().unwrap();
        let retry = tokio::time::timeout(Duration::from_secs(5), launcher.find_or_start())
            .await
            .unwrap();
        assert!(
            matches!(
                retry,
                Err(Error::NotRunning {
                    why: NotRunning::Starting,
                    ..
                })
            ),
            "{retry:?}"
        );
        assert_eq!(launcher.owned_pid(), Some(pid));
        assert!(matches!(
            launcher.stop_owned(STOP_WAIT),
            Some(Stopped::Exited(_))
        ));
    }

    #[tokio::test]
    async fn an_exited_child_is_no_longer_reported_as_owned() {
        let scratch = Scratch::new("child-exited");
        let program = scratch.program("exit 3", "exit /b 3");
        let launcher = Launcher::new(scratch.0.clone(), None);
        let (mut owned, _ready) = Owned::spawn(&program, &scratch.0).unwrap();
        owned.child.wait().unwrap();
        launcher.owned().replace(owned);
        assert_eq!(launcher.owned_pid(), None);
        launcher.find_or_start().await.unwrap_err();
        assert!(launcher.stop_owned(STOP_WAIT).is_none());
    }

    #[test]
    fn a_server_is_stopped_by_its_pipe_and_killed_if_it_ignores_it() {
        let scratch = Scratch::new("stops");
        // It ends when its standard input does, as an owned server does.
        let polite = scratch.program(
            "echo ready pid=$$ server_id=x; cat >/dev/null",
            "echo ready pid=1 server_id=x\nfindstr \"^\" >nul\nexit /b 0",
        );
        let (owned, ready) = Owned::spawn(&polite, &scratch.0).unwrap();
        ready.said_ready.blocking_recv().unwrap();
        let stopped = owned.stop(STOP_WAIT);
        assert!(
            matches!(&stopped, Stopped::Exited(status) if status.success()),
            "{stopped:?}"
        );

        // On Windows a batch file that spins, which starts nothing that
        // would outlive it.
        let deaf = scratch.program(
            "echo ready pid=$$ server_id=x; exec sleep 60",
            "echo ready pid=1 server_id=x\n:spin\ngoto spin",
        );
        let (owned, ready) = Owned::spawn(&deaf, &scratch.0).unwrap();
        ready.said_ready.blocking_recv().unwrap();
        let begun = Instant::now();
        assert!(matches!(
            owned.stop(Duration::from_millis(200)),
            Stopped::Killed
        ));
        assert!(begun.elapsed() < Duration::from_secs(10));
    }

    /// Answers every request on `listener` with `descriptor`, as a server
    /// of another release would.
    fn answer(listener: std::net::TcpListener, descriptor: &PublicDescriptor) {
        let body = serde_json::to_string(descriptor).unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut request = [0; 4096];
                let _ = stream.read(&mut request);
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
    }

    #[tokio::test]
    async fn a_server_of_another_release_is_reported_and_nothing_is_started() {
        let scratch = Scratch::new("another-release");
        let data_dir = scratch.0.join("data");
        std::fs::create_dir_all(&data_dir).unwrap();
        let server_id: Id = "11111111-1111-1111-1111-111111111111".parse().unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = listener.local_addr().unwrap();
        answer(
            listener,
            &PublicDescriptor {
                server_id,
                name: "Old".to_owned(),
                role: ServerRole::Hub,
                contract_version: CONTRACT_VERSION + 1,
                capabilities: Vec::new(),
                limits: Limits {
                    max_message_bytes: 1 << 20,
                    max_views: 0,
                    max_window_rows: 0,
                    view_idle_ms: 0,
                },
                lists: Vec::new(),
                art_sizes: Vec::new(),
            },
        );
        // It holds the lock and has said where it is, as a running server
        // of any release does.
        let lock = data_dir.join(agreed::SERVER_LOCK);
        std::fs::write(&lock, "4312\n").unwrap();
        let held = File::open(&lock).unwrap();
        crate::testing::hold_as_a_server(&held);
        std::fs::write(
            data_dir.join(agreed::SERVER_JSON),
            format!(
                r#"{{"pid":4312,"endpoint":"{endpoint}","serverId":"{server_id}","contractVersion":{}}}"#,
                CONTRACT_VERSION + 1
            ),
        )
        .unwrap();

        // Starting this would fail the test.
        let started = scratch.0.join("started");
        let program = scratch.program(
            &format!("touch '{}'; exit 1", started.display()),
            &format!("type nul > \"{}\"\nexit /b 1", started.display()),
        );
        let launcher = Launcher::new(data_dir, Some(program));
        let found = tokio::time::timeout(Duration::from_secs(30), launcher.find_or_start())
            .await
            .unwrap()
            .unwrap_err();
        assert!(
            matches!(found, Error::ContractVersion { server } if server == CONTRACT_VERSION + 1),
            "{found:?}"
        );
        assert!(!started.exists());
        drop(held);
    }
}
