//! Shared helpers for the `ritornel-server` integration tests.

use std::io::{BufRead, BufReader, Read};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use ritornel_client::contract::{CONTRACT_VERSION, ClientKind, HelloParams, Id, SessionKind};
use ritornel_client::{Client, Credential};
use ritornel_engine::{OsEntropy, SystemClock};
use ritornel_server::{Bounds, HeldDir, Listen, Server};

/// How long a test may take before it is failed. It is a bound, not a
/// wait: a test that passes never comes near it.
pub(crate) const WAIT: Duration = Duration::from_secs(60);

/// Runs a test, and fails it if it has not finished in [`WAIT`].
pub(crate) async fn bounded(test: impl Future<Output = ()>) {
    tokio::time::timeout(WAIT, test)
        .await
        .unwrap_or_else(|_| panic!("the test did not finish within {WAIT:?}"));
}

/// A directory that is removed when the test ends.
pub(crate) struct Scratch(pub(crate) PathBuf);

impl Scratch {
    pub(crate) fn new(prefix: &str) -> Self {
        let path = tempfile::Builder::new()
            .prefix(prefix)
            .tempdir()
            .unwrap()
            .keep();
        Self(path)
    }

    pub(crate) fn canonical(prefix: &str) -> Self {
        let path = tempfile::Builder::new()
            .prefix(prefix)
            .tempdir()
            .unwrap()
            .keep();
        Self(path.canonicalize().unwrap())
    }

    /// The data directory. The server creates it.
    pub(crate) fn data_dir(&self) -> PathBuf {
        self.0.join("data")
    }

    pub(crate) fn root(&self) -> PathBuf {
        self.0.join("root")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The local credential's token, as a client on this machine reads it.
pub(crate) fn token_in(data_dir: &Path) -> String {
    std::fs::read_to_string(data_dir.join("credential"))
        .unwrap()
        .trim()
        .to_owned()
}

/// What a test joins the client to the server with.
#[derive(Clone, Copy)]
pub(crate) enum Via {
    Memory,
    Socket,
}

/// Defines, for each test named, a module with the test run both ways.
macro_rules! on_both_transports {
    ($($test:ident)*) => {$(
        mod $test {
            use crate::support::{Via, bounded};

            #[tokio::test]
            async fn in_memory() {
                bounded(super::$test(Via::Memory)).await;
            }

            #[tokio::test]
            async fn over_a_socket() {
                bounded(super::$test(Via::Socket)).await;
            }
        }
    )*};
}
pub(crate) use on_both_transports;

/// Invents a library on a data directory with the server's own hidden
/// command, as a developer would.
pub(crate) fn invent(data_dir: &Path, root: &Path, extra: &[&str]) {
    let output = Command::new(env!("CARGO_BIN_EXE_ritornel-server"))
        .arg("invent")
        .arg("--data-dir")
        .arg(data_dir)
        .arg("--root")
        .arg(root)
        .args(extra)
        .env_remove("RITORNEL_DATA_DIR")
        .output()
        .unwrap();
    // A process a signal ended has no code, and may have said nothing.
    assert!(
        output.status.success(),
        "ritornel-server invent: {}\n--- stderr\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A server running inside the test, on a directory of its own.
pub(crate) struct Running {
    pub(crate) server: Server,
    pub(crate) scratch: Scratch,
    hello: HelloParams,
}

impl Running {
    /// Starts a server on a port the system picks.
    pub(crate) async fn start(prefix: &str) -> Self {
        Self::start_with(prefix, Bounds::default()).await
    }

    /// Starts a server with bounds small enough for a test to reach.
    pub(crate) async fn start_with(prefix: &str, bounds: Bounds) -> Self {
        let scratch = Scratch::new(prefix);
        Self::on_scratch(scratch, bounds).await
    }

    /// Starts a server on an already-prepared scratch directory.
    pub(crate) async fn on_scratch(scratch: Scratch, bounds: Bounds) -> Self {
        let server = Server::start_with(
            HeldDir::take(scratch.data_dir()).unwrap(),
            Listen::on_port(0),
            Arc::new(SystemClock),
            Arc::new(OsEntropy),
            bounds,
        )
        .await
        .unwrap();
        Self {
            server,
            scratch,
            hello: HelloParams {
                name: "test".to_owned(),
                kind: ClientKind::Cli,
                version: "1".to_owned(),
                contract_version: CONTRACT_VERSION,
            },
        }
    }

    /// Sets the `hello` parameters used by [`greeted`](Self::greeted).
    pub(crate) fn with_hello(mut self, hello: HelloParams) -> Self {
        self.hello = hello;
        self
    }

    /// Sets the client name used by [`greeted`](Self::greeted).
    pub(crate) fn with_client_name(mut self, name: &str) -> Self {
        self.hello.name = name.to_owned();
        self
    }

    pub(crate) fn data_dir(&self) -> PathBuf {
        self.scratch.data_dir()
    }

    pub(crate) fn endpoint(&self) -> SocketAddr {
        self.server.endpoint()
    }

    pub(crate) fn token(&self) -> String {
        token_in(&self.data_dir())
    }

    /// The local credential, as a client on this machine reads it.
    pub(crate) fn credential(&self) -> Credential {
        Credential::new(self.token())
    }

    /// A client that has not said hello yet.
    pub(crate) async fn connect(&self, via: Via) -> Client {
        match via {
            Via::Memory => {
                let (outgoing, incoming) = self.server.connect_in_memory(SessionKind::Full);
                Client::over_memory(outgoing, incoming)
            }
            Via::Socket => Client::connect(self.endpoint(), &self.credential())
                .await
                .unwrap(),
        }
    }

    /// A client that has said hello.
    pub(crate) async fn greeted(&self, via: Via) -> Client {
        let client = self.connect(via).await;
        client.hello(&self.hello).await.unwrap();
        client
    }

    /// A new ticket, asked for as a program on this machine asks.
    pub(crate) async fn ticket(&self) -> String {
        ritornel_client::ticket(self.endpoint(), &self.credential())
            .await
            .unwrap()
            .ticket
    }
}

pub(crate) fn key() -> Id {
    ritornel_client::idempotency_key().unwrap()
}

/// An album of three tracks from the audio fixtures, in the scratch
/// directory.
pub(crate) fn album(scratch: &Scratch) -> PathBuf {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/audio");
    let folder = scratch.0.join("music");
    for (to, fixture) in [
        ("Tin Weather/01.flac", "a.flac"),
        ("Tin Weather/02.flac", "b.flac"),
        ("Tin Weather/03.mp3", "a.mp3"),
    ] {
        let to = folder.join(to);
        std::fs::create_dir_all(to.parent().unwrap()).unwrap();
        std::fs::copy(fixtures.join(fixture), to).unwrap();
    }
    folder.canonicalize().unwrap()
}

/// Reads a pipe on a thread of its own, forwarding each line into `send`.
pub(crate) fn pipe_lines(pipe: impl Read + Send + 'static, send: Sender<String>) {
    std::thread::spawn(move || {
        for line in BufReader::new(pipe).lines() {
            // The test has gone only if it has failed already.
            if send.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
}

/// Reads a pipe on a thread of its own and hands over each line, so that
/// whoever wants them can stop waiting. The channel closes at the pipe's
/// end, which comes when the process ends.
pub(crate) fn lines(pipe: impl Read + Send + 'static) -> Receiver<String> {
    let (send, lines) = mpsc::channel();
    pipe_lines(pipe, send);
    lines
}

/// The rest of what a pipe carries, up to its end. `None` if the end did
/// not come within [`WAIT`].
pub(crate) fn rest(lines: &Receiver<String>) -> Option<String> {
    let mut text = String::new();
    loop {
        match lines.recv_timeout(WAIT) {
            Ok(line) => {
                text.push_str(&line);
                text.push('\n');
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return Some(text),
            Err(mpsc::RecvTimeoutError::Timeout) => return None,
        }
    }
}

/// Sends a server process a signal by name (`TERM`, or `INT` on Unix).
/// On Windows, `TERM` runs `taskkill /PID` without `/F`, which asks the
/// server's hidden window to close; `INT` has no equivalent for a process
/// without a console.
pub(crate) fn signal(pid: u32, name: &str) {
    if cfg!(windows) {
        assert_eq!(name, "TERM", "only TERM has a Windows equivalent here");
        let sent = Command::new("taskkill")
            .args(["/PID", &pid.to_string()])
            .output()
            .unwrap();
        assert!(sent.status.success(), "asking it to stop: {sent:?}");
    } else {
        let sent = Command::new("kill")
            .args([&format!("-{name}"), &pid.to_string()])
            .output()
            .unwrap();
        assert!(sent.status.success(), "asking it to stop: {sent:?}");
    }
}

/// Asks a server process to stop cleanly (`SIGTERM` on Unix, `taskkill /PID`
/// on Windows).
pub(crate) fn terminate(pid: u32) {
    signal(pid, "TERM");
}
