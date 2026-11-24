//! The real `ritornel-server` binary, started as a launcher or a person
//! would start it, on a data directory of its own.
//!
//! Nothing here sleeps. A test waits for the server's ready line, which the
//! server prints only once it can be found and stopped, and for the process
//! to end. Each wait is bounded by [`WAIT`]: a server that hangs is killed
//! and fails its test with a message that says what it was waited for.
//!
//! Every server here listens on a port the system picks, so no test needs
//! the default port.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use ritornel_engine::{OsEntropy, ServerStore, SessionToken, SystemClock};
use serde_json::Value;

/// How long a test waits for a server to do what it should before it kills
/// the server and fails. It is a bound, not a wait: a server that works
/// never comes near it.
const WAIT: Duration = Duration::from_secs(60);

/// A directory that is removed when the test ends.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let path = tempfile::Builder::new()
            .prefix("ritornel-server-process-")
            .tempdir()
            .unwrap()
            .keep();
        Self(path)
    }

    /// The data directory. The server creates it.
    fn data_dir(&self) -> PathBuf {
        self.0.join("data")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `ritornel-server serve` on `data_dir`, on a port the system picks, with
/// nothing in its environment that this test did not put there.
fn serve(data_dir: &Path, extra: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ritornel-server"));
    command
        .arg("serve")
        .arg("--data-dir")
        .arg(data_dir)
        .args(["--listen", "127.0.0.1:0"])
        .args(extra)
        .env("RITORNEL_TEST_COMPUTER_NAME", "Invented Desk")
        .env_remove("RITORNEL_DATA_DIR")
        .env_remove("RITORNEL_LISTEN")
        .env_remove("RITORNEL_LOG_LEVEL")
        .env_remove("RITORNEL_BACKUP_DIR")
        .env_remove("RITORNEL_NAME")
        // Never the API key and secret of whoever runs the tests.
        .env_remove("SCROBL_API_KEY")
        .env_remove("SCROBL_API_SECRET")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

/// Reads a pipe on a thread of its own and hands over each line, so that
/// whoever wants them can stop waiting. The channel closes at the pipe's
/// end, which comes when the process ends.
fn lines(pipe: impl Read + Send + 'static) -> Receiver<String> {
    let (send, lines) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(pipe).lines() {
            // The test has gone only if it has failed already.
            if send.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    lines
}

/// The rest of what a pipe carries, up to its end. `None` if the end did
/// not come within [`WAIT`].
fn rest(lines: &Receiver<String>) -> Option<String> {
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

/// Runs a command that should end by itself, and returns how it ended and
/// what it printed. It is killed, and the test fails, if it has not ended
/// within [`WAIT`].
fn run(mut command: Command) -> Stopped {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = lines(child.stdout.take().unwrap());
    let stderr = lines(child.stderr.take().unwrap());
    let (Some(stdout), Some(stderr)) = (rest(&stdout), rest(&stderr)) else {
        let _ = child.kill();
        let _ = child.wait();
        panic!("{command:?} did not end within {WAIT:?}");
    };
    Stopped {
        // Both pipes are at their end, so the process has ended.
        code: child.wait().unwrap().code(),
        stdout,
        stderr,
    }
}

/// A server that is running. It is killed if a test ends without stopping
/// it, so a failed test leaves no process behind.
struct Server {
    child: Child,
    stdout: Receiver<String>,
    stderr: Receiver<String>,
    pid: u32,
    /// The server ID from the ready line.
    server_id: String,
}

/// How a server ended, and what it printed after its ready line.
#[derive(Debug)]
struct Stopped {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

impl Server {
    /// Starts `ritornel-server serve` on `data_dir` and waits for its ready
    /// line.
    fn start(data_dir: &Path, extra: &[&str]) -> Self {
        Self::spawn(serve(data_dir, extra))
    }

    /// Starts a server and waits for its ready line.
    fn spawn(mut command: Command) -> Self {
        let mut child = command.spawn().unwrap();
        let stdout = lines(child.stdout.take().unwrap());
        let stderr = lines(child.stderr.take().unwrap());
        let pid = child.id();
        let ready = stdout.recv_timeout(WAIT);
        let Some(server_id) = ready
            .as_deref()
            .ok()
            .and_then(|line| line.strip_prefix(&format!("ready pid={pid} server_id=")))
        else {
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "no ready line within {WAIT:?}. stdout: {ready:?}, stderr: {:?}",
                rest(&stderr)
            );
        };
        Self {
            server_id: server_id.to_owned(),
            child,
            stdout,
            stderr,
            pid,
        }
    }

    /// Sends the server a signal, by name: `TERM`, `INT`.
    #[cfg(unix)]
    fn signal(&self, name: &str) {
        let sent = Command::new("kill")
            .arg(format!("-{name}"))
            .arg(self.pid.to_string())
            .status()
            .unwrap();
        assert!(sent.success());
    }

    /// Asks the server to stop as Windows does in place of `TERM`:
    /// `taskkill /PID` without `/F` asks its hidden window to close. A
    /// process with no console of its own has no Ctrl+C to be sent, so
    /// `INT` is not one of them.
    #[cfg(windows)]
    fn signal(&self, name: &str) {
        assert_eq!(name, "TERM", "only TERM has a Windows equivalent here");
        let sent = Command::new("taskkill")
            .arg("/PID")
            .arg(self.pid.to_string())
            .output()
            .unwrap();
        assert!(sent.status.success(), "{sent:?}");
    }

    /// Closes the server's standard input.
    fn close_stdin(&mut self) {
        drop(self.child.stdin.take());
    }

    /// Waits for the server to end. It is killed, and the test fails, if
    /// it has not ended within [`WAIT`].
    fn wait(mut self) -> Stopped {
        // Both pipes end when the process does.
        let (Some(stdout), Some(stderr)) = (rest(&self.stdout), rest(&self.stderr)) else {
            // Dropping `self` kills it.
            panic!("the server did not stop within {WAIT:?} of being asked to");
        };
        Stopped {
            code: self.child.wait().unwrap().code(),
            stdout,
            stderr,
        }
    }

    /// Stops the server with `TERM` and checks that it stopped cleanly.
    fn stop(self, data_dir: &Path) {
        self.signal("TERM");
        assert_clean_stop(&self.wait(), data_dir);
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // After `wait` these do nothing.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// `ritornel-server serve` on `data_dir` with no `--listen`, so that the
/// environment and `config.toml` decide where it listens.
fn without_listen_flag(data_dir: &Path, variables: &[(&str, &str)]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ritornel-server"));
    command
        .arg("serve")
        .arg("--data-dir")
        .arg(data_dir)
        .env_remove("RITORNEL_DATA_DIR")
        .env_remove("RITORNEL_LISTEN")
        .env_remove("RITORNEL_LOG_LEVEL")
        .env_remove("RITORNEL_BACKUP_DIR")
        .env_remove("RITORNEL_NAME")
        .envs(variables.iter().copied())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn server_json(data_dir: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(data_dir.join("server.json")).unwrap()).unwrap()
}

/// Every log line in the data directory, oldest file first.
fn log_lines(data_dir: &Path) -> Vec<Value> {
    let mut files: Vec<_> = std::fs::read_dir(data_dir.join("logs"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    files.sort();
    files
        .iter()
        .flat_map(|file| {
            std::fs::read_to_string(file)
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect::<Vec<Value>>()
        })
        .collect()
}

fn messages(lines: &[Value]) -> Vec<&str> {
    lines
        .iter()
        .map(|line| line["message"].as_str().unwrap())
        .collect()
}

/// The reason a server logs when it is stopped by [`Server::signal`] with
/// `TERM`.
const TERM_REASON: &str = if cfg!(windows) {
    "asked to close"
} else {
    "SIGTERM"
};

/// Whether only the owner can open a file or directory: the modes `0600`
/// and `0700` on Unix, and an access list that names the owner alone on
/// Windows, which the system's own access check for another user must
/// agree with.
fn private(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o077 == 0
    }
    #[cfg(windows)]
    {
        use ritornel_windows::private::{another_user_may_open, path_is_private};
        let private = path_is_private(path).unwrap();
        assert_eq!(another_user_may_open(path).unwrap(), !private, "{path:?}");
        private
    }
}

fn credential(data_dir: &Path) -> String {
    std::fs::read_to_string(data_dir.join("credential")).unwrap()
}

/// Whether a stopped server's database takes `token` as the local
/// credential.
fn accepts(data_dir: &Path, token: &str) -> bool {
    let store = ServerStore::open(
        &data_dir.join("server.db"),
        Arc::new(SystemClock),
        Arc::new(OsEntropy),
    )
    .unwrap();
    store
        .is_local_credential(&SessionToken::new(token.trim()))
        .unwrap()
}

fn assert_clean_stop(stopped: &Stopped, data_dir: &Path) {
    assert_eq!(stopped.code, Some(0), "{stopped:?}");
    assert_eq!(stopped.stdout, "", "{stopped:?}");
    assert_eq!(stopped.stderr, "", "{stopped:?}");
    assert!(!data_dir.join("server.json").exists());
    // The write-ahead log was emptied, and SQLite removes an empty one.
    assert!(!data_dir.join("server.db-wal").exists());
}

#[test]
fn a_second_server_on_the_same_directory_is_refused_and_the_first_stops_cleanly() {
    let scratch = Scratch::new();
    let data_dir = scratch.data_dir();
    let first = Server::start(&data_dir, &[]);
    let named = server_json(&data_dir);
    let endpoint = named["endpoint"].as_str().unwrap();
    let port: u16 = endpoint
        .strip_prefix("127.0.0.1:")
        .unwrap()
        .parse()
        .unwrap();
    assert_ne!(port, 0);
    assert_eq!(
        named,
        serde_json::json!({
            "pid": first.pid,
            "endpoint": endpoint,
            "serverId": first.server_id,
            "contractVersion": ritornel_contract::CONTRACT_VERSION,
        })
    );

    let second = run(serve(&data_dir, &[]));
    assert_eq!(second.code, Some(3), "{second:?}");
    assert_eq!(second.stdout, "");
    assert_eq!(
        second.stderr,
        format!(
            "ritornel-server: data directory in use by pid {} at {endpoint}: {}\n",
            first.pid,
            data_dir.display()
        )
    );
    // Being refused changed nothing the first server relies on.
    assert_eq!(server_json(&data_dir)["pid"], first.pid);

    first.signal("TERM");
    assert_clean_stop(&first.wait(), &data_dir);

    // With the first gone the directory is free again.
    Server::start(&data_dir, &[]).stop(&data_dir);
}

#[test]
fn a_restart_keeps_the_identity_and_the_credential() {
    let scratch = Scratch::new();
    let data_dir = scratch.data_dir();

    let first = Server::start(&data_dir, &[]);
    let server_id = first.server_id.clone();
    let first_stop = if cfg!(windows) { "TERM" } else { "INT" };
    first.signal(first_stop);
    assert_clean_stop(&first.wait(), &data_dir);

    let token = credential(&data_dir);
    assert_eq!(token.trim().len(), 64);
    assert!(private(&data_dir.join("credential")));
    assert!(private(&data_dir));
    assert!(private(&data_dir.join("server.db")));
    assert!(accepts(&data_dir, &token));

    let second = Server::start(&data_dir, &[]);
    assert_eq!(second.server_id, server_id);
    second.stop(&data_dir);
    assert_eq!(credential(&data_dir), token);
    assert!(accepts(&data_dir, &token));

    let lines = log_lines(&data_dir);
    let reasons: Vec<_> = lines
        .iter()
        .filter(|line| line["message"] == "stopping")
        .map(|line| line["reason"].as_str().unwrap())
        .collect();
    let first_reason = if cfg!(windows) { TERM_REASON } else { "SIGINT" };
    assert_eq!(reasons, [first_reason, TERM_REASON]);
}

#[test]
fn a_name_given_in_the_settings_wins_over_the_stored_one_and_is_kept_after() {
    let scratch = Scratch::new();
    let data_dir = scratch.data_dir();
    Server::start(&data_dir, &[]).stop(&data_dir);
    Server::start(&data_dir, &["--name", "Workbench Speakers"]).stop(&data_dir);
    Server::start(&data_dir, &[]).stop(&data_dir);
    std::fs::write(data_dir.join("config.toml"), "name = \"Porch\"\n").unwrap();
    Server::start(&data_dir, &[]).stop(&data_dir);

    let lines = log_lines(&data_dir);
    let names: Vec<_> = lines
        .iter()
        .filter(|line| line["message"] == "ready")
        .map(|line| line["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "Invented Desk",
            "Workbench Speakers",
            "Workbench Speakers",
            "Porch"
        ]
    );
}

#[test]
fn a_lost_credential_file_is_replaced_at_the_next_start() {
    let scratch = Scratch::new();
    let data_dir = scratch.data_dir();
    let first = Server::start(&data_dir, &[]);
    let server_id = first.server_id.clone();
    first.stop(&data_dir);
    let lost = credential(&data_dir);
    std::fs::remove_file(data_dir.join("credential")).unwrap();

    let second = Server::start(&data_dir, &[]);
    assert_eq!(second.server_id, server_id);
    second.stop(&data_dir);

    let replacement = credential(&data_dir);
    assert_ne!(replacement, lost);
    assert!(private(&data_dir.join("credential")));
    assert!(accepts(&data_dir, &replacement));
    assert!(!accepts(&data_dir, &lost));
    let lines = log_lines(&data_dir);
    assert!(
        lines.iter().any(|line| line["level"] == "warn"
            && line["message"]
                .as_str()
                .unwrap()
                .starts_with("the credential file was missing")),
        "{lines:?}"
    );
}

#[test]
fn an_owned_server_stops_when_its_standard_input_closes() {
    let scratch = Scratch::new();
    let data_dir = scratch.data_dir();
    let mut server = Server::start(&data_dir, &["--owned"]);
    assert!(data_dir.join("server.json").exists());

    server.close_stdin();
    assert_clean_stop(&server.wait(), &data_dir);
    let lines = log_lines(&data_dir);
    let stopping = lines
        .iter()
        .find(|line| line["message"] == "stopping")
        .unwrap();
    assert_eq!(stopping["reason"], "standard input closed");
}

#[test]
fn an_owned_server_still_stops_on_a_signal() {
    let scratch = Scratch::new();
    let data_dir = scratch.data_dir();
    // Its standard input stays open, with a thread of the server waiting on
    // it, for as long as this test holds the other end.
    let server = Server::start(&data_dir, &["--owned"]);
    server.stop(&data_dir);
}

#[test]
fn a_server_nobody_owns_outlives_its_standard_input() {
    let scratch = Scratch::new();
    let data_dir = scratch.data_dir();
    let mut server = Server::start(&data_dir, &[]);
    server.close_stdin();
    // It is still there to be stopped the ordinary way, and that is the
    // reason it gives.
    server.signal("TERM");
    assert_clean_stop(&server.wait(), &data_dir);
    let lines = log_lines(&data_dir);
    let stopping = lines
        .iter()
        .find(|line| line["message"] == "stopping")
        .unwrap();
    assert_eq!(stopping["reason"], TERM_REASON);
}

#[test]
fn every_log_line_is_json_with_the_runs_boot_id_and_no_secret() {
    let scratch = Scratch::new();
    let data_dir = scratch.data_dir();
    for _ in 0..2 {
        Server::start(&data_dir, &[]).stop(&data_dir);
    }

    let lines = log_lines(&data_dir);
    assert_eq!(
        messages(&lines),
        [
            "starting",
            "server.db is open",
            "first start: created this server's identity",
            "this server is named after its computer",
            "local credential",
            "listening",
            "ready",
            "stopping",
            "stopped",
            "starting",
            "server.db is open",
            "local credential",
            "listening",
            "ready",
            "stopping",
            "stopped",
        ]
    );
    for line in &lines {
        assert_eq!(line["level"], "info", "{line}");
        assert_eq!(line["ts"].as_str().unwrap().len(), 24, "{line}");
        assert_eq!(line["boot_id"].as_str().unwrap().len(), 36, "{line}");
    }
    // One boot ID a run, and a new one for the next.
    let (first, second) = lines.split_at(9);
    assert!(
        first
            .iter()
            .all(|line| line["boot_id"] == first[0]["boot_id"])
    );
    assert!(
        second
            .iter()
            .all(|line| line["boot_id"] == second[0]["boot_id"])
    );
    assert_ne!(first[0]["boot_id"], second[0]["boot_id"]);

    assert_eq!(lines[1]["sqlite"], "3.54.1");
    assert_eq!(lines[1]["schema_version"], 2);
    assert_eq!(lines[1]["migrations_applied"], 2);
    assert_eq!(lines[10]["migrations_applied"], 0);
    assert_eq!(lines[3]["name"], "Invented Desk");
    assert_eq!(lines[4]["credential"], "issued");
    assert_eq!(lines[11]["credential"], "kept");
    // Each run listened where the system put it.
    for listening in [&lines[5], &lines[12]] {
        let endpoint = listening["endpoint"].as_str().unwrap();
        assert!(endpoint.starts_with("127.0.0.1:"), "{endpoint}");
    }

    let token = credential(&data_dir);
    let logs = std::fs::read_dir(data_dir.join("logs"))
        .unwrap()
        .map(|entry| std::fs::read_to_string(entry.unwrap().path()).unwrap())
        .collect::<String>();
    assert!(!logs.contains(token.trim()));
}

#[test]
fn a_setting_comes_from_its_flag_then_the_environment_then_the_file() {
    let scratch = Scratch::new();
    let data_dir = scratch.data_dir();
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::write(data_dir.join("config.toml"), "log_level = \"error\"\n").unwrap();
    let levels = || -> Vec<String> {
        let lines = log_lines(&data_dir);
        std::fs::remove_dir_all(data_dir.join("logs")).unwrap();
        lines
            .iter()
            .map(|line| line["level"].as_str().unwrap().to_owned())
            .collect()
    };

    // The file alone: a run with no errors logs nothing.
    Server::start(&data_dir, &[]).stop(&data_dir);
    assert_eq!(levels(), [""; 0]);

    // The environment beats the file. Losing the credential file is the one
    // thing a healthy start warns about.
    std::fs::remove_file(data_dir.join("credential")).unwrap();
    let mut command = serve(&data_dir, &[]);
    command.env("RITORNEL_LOG_LEVEL", "warn");
    Server::spawn(command).stop(&data_dir);
    assert_eq!(levels(), ["warn"]);

    // The flag beats both.
    let mut command = serve(&data_dir, &["--log-level", "info"]);
    command.env("RITORNEL_LOG_LEVEL", "warn");
    Server::spawn(command).stop(&data_dir);
    assert_eq!(levels(), ["info"; 7]);
}

#[test]
fn the_data_directory_can_come_from_the_environment() {
    let scratch = Scratch::new();
    let data_dir = scratch.data_dir();
    let mut command = Command::new(env!("CARGO_BIN_EXE_ritornel-server"));
    command
        .arg("serve")
        .env("RITORNEL_DATA_DIR", &data_dir)
        .env("RITORNEL_LISTEN", "127.0.0.1:0")
        .env_remove("RITORNEL_LOG_LEVEL")
        .env_remove("RITORNEL_BACKUP_DIR")
        .env_remove("RITORNEL_NAME")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let server = Server::spawn(command);
    assert_eq!(server_json(&data_dir)["pid"], server.pid);
    server.stop(&data_dir);
}

#[test]
fn a_start_that_fails_exits_with_1_and_leaves_the_directory_free() {
    let scratch = Scratch::new();
    let data_dir = scratch.data_dir();
    Server::start(&data_dir, &[]).stop(&data_dir);

    // A setting this server does not have. It is read once the lock is held.
    std::fs::write(data_dir.join("config.toml"), "cache_dir = \"/big/disk\"\n").unwrap();
    let refused = run(serve(&data_dir, &[]));
    assert_eq!(refused.code, Some(1), "{refused:?}");
    assert_eq!(refused.stdout, "");
    assert!(
        refused
            .stderr
            .starts_with("ritornel-server: config.toml: unknown field `cache_dir`"),
        "{refused:?}"
    );
    assert!(!data_dir.join("server.json").exists());

    // An address beyond loopback, wherever it is set.
    std::fs::write(data_dir.join("config.toml"), "listen = \"0.0.0.0:4747\"\n").unwrap();
    // The flag the other tests give would win over both, so it is left out.
    let refused = run(without_listen_flag(
        &data_dir,
        &[("RITORNEL_LISTEN", "192.168.1.20:4747")],
    ));
    assert_eq!(refused.code, Some(1), "{refused:?}");
    assert_eq!(
        refused.stderr,
        "ritornel-server: RITORNEL_LISTEN must be 127.0.0.1 and a port, such as \
         127.0.0.1:4747, or private and a port, such as private:4747: the server listens on \
         loopback or the private network only\n"
    );
    let refused = run(without_listen_flag(&data_dir, &[]));
    assert_eq!(refused.code, Some(1), "{refused:?}");
    assert_eq!(
        refused.stderr,
        "ritornel-server: listen in config.toml must be 127.0.0.1 and a port, such as \
         127.0.0.1:4747, or private and a port, such as private:4747: the server listens on \
         loopback or the private network only\n"
    );
    assert!(!data_dir.join("server.json").exists());

    std::fs::remove_file(data_dir.join("config.toml")).unwrap();
    Server::start(&data_dir, &[]).stop(&data_dir);
}

#[test]
fn a_database_that_cannot_be_opened_stops_the_start_and_the_log_says_why() {
    let scratch = Scratch::new();
    let data_dir = scratch.data_dir();
    Server::start(&data_dir, &[]).stop(&data_dir);
    let before = log_lines(&data_dir).len();

    std::fs::write(
        data_dir.join("server.db"),
        "this is not a database, not at all",
    )
    .unwrap();
    let refused = run(serve(&data_dir, &[]));
    assert_eq!(refused.code, Some(1), "{refused:?}");
    assert_eq!(refused.stdout, "");
    let stderr = refused.stderr;
    assert!(
        stderr.starts_with("ritornel-server: server.db: "),
        "{stderr}"
    );
    assert!(!data_dir.join("server.json").exists());

    let lines = log_lines(&data_dir);
    assert_eq!(
        messages(&lines[before..]),
        ["starting", "stopped on an error"]
    );
    let last = lines.last().unwrap();
    assert_eq!(last["level"], "error");
    assert_eq!(
        format!("ritornel-server: {}\n", last["error"].as_str().unwrap()),
        stderr
    );
}
