//! The real `ritornelctl` against the real `ritornel-server`, each started
//! as a person would start it.
//!
//! `ritornelctl` is the binary Cargo built for this test. `ritornel-server`
//! belongs to another package, so Cargo does not hand this test its path:
//! it is taken from beside `ritornelctl`, where a build of the workspace
//! puts it. `mise run test` builds the workspace. `cargo test --package
//! ritornelctl` alone does not build the server, and would use whatever
//! server an earlier build left there.
//!
//! Nothing here sleeps. Each wait is for a process to print a line or to
//! end, and is bounded by [`WAIT`]: a process that hangs is killed and
//! fails its test with a message that says what it was waited for.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use serde_json::Value;

/// How long a test waits for a process to do what it should before it
/// kills the process and fails. It is a bound, not a wait: a process that
/// works never comes near it.
const WAIT: Duration = Duration::from_secs(60);

/// The exit code `ritornelctl` keeps for "no server is running".
const NO_SERVER: i32 = 4;

/// A directory that is removed when the test ends.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "ritornelctl-status-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&path).unwrap();
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

/// How a process ended, and what it printed.
#[derive(Debug)]
struct Ended {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// `ritornelctl` with these arguments, and nothing in its environment that
/// this test did not put there.
fn ritornelctl(args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ritornelctl"));
    command.args(args).env_remove("RITORNEL_DATA_DIR");
    command
}

/// Runs a command that should end by itself. It is killed, and the test
/// fails, if it has not ended within [`WAIT`].
fn run(mut command: Command) -> Ended {
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
    Ended {
        // Both pipes are at their end, so the process has ended.
        code: child.wait().unwrap().code(),
        stdout,
        stderr,
    }
}

/// `ritornelctl status` on a data directory, with more arguments.
fn status(data_dir: &Path, extra: &[&str]) -> Ended {
    let mut command = ritornelctl(&["status", "--data-dir"]);
    command.arg(data_dir).args(extra);
    run(command)
}

/// The `ritornel-server` that was built beside `ritornelctl`.
fn server_binary() -> PathBuf {
    let binary = Path::new(env!("CARGO_BIN_EXE_ritornelctl")).with_file_name("ritornel-server");
    assert!(
        binary.exists(),
        "there is no ritornel-server beside ritornelctl at {}. Build the workspace: `mise run \
         test` does, and `cargo test --package ritornelctl` alone does not",
        binary.display()
    );
    binary
}

/// A `ritornel-server` that is running, on a port the system picked. It is
/// killed if a test ends without stopping it, so a failed test leaves no
/// process behind.
struct Server {
    child: Child,
    stdout: Receiver<String>,
    stderr: Receiver<String>,
    /// The server ID from the ready line.
    server_id: String,
}

impl Server {
    /// Starts a server on `data_dir` and waits for its ready line.
    fn start(data_dir: &Path) -> Self {
        let mut child = Command::new(server_binary())
            .arg("serve")
            .arg("--data-dir")
            .arg(data_dir)
            .args(["--listen", "127.0.0.1:0"])
            .env_remove("RITORNEL_DATA_DIR")
            .env_remove("RITORNEL_LISTEN")
            .env_remove("RITORNEL_LOG_LEVEL")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = lines(child.stdout.take().unwrap());
        let stderr = lines(child.stderr.take().unwrap());
        let ready = stdout.recv_timeout(WAIT);
        let Some(server_id) = ready
            .as_deref()
            .ok()
            .and_then(|line| line.strip_prefix(&format!("ready pid={} server_id=", child.id())))
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
        }
    }

    /// Sends the server a signal, by name: `TERM`, `KILL`.
    fn signal(&self, name: &str) {
        let sent = Command::new("kill")
            .arg(format!("-{name}"))
            .arg(self.child.id().to_string())
            .status()
            .unwrap();
        assert!(sent.success());
    }

    /// Sends the server a signal and waits for it to end. It is killed, and
    /// the test fails, if it has not ended within [`WAIT`].
    fn end(mut self, signal: &str) -> Ended {
        self.signal(signal);
        // Both pipes end when the process does.
        let (Some(stdout), Some(stderr)) = (rest(&self.stdout), rest(&self.stderr)) else {
            // Dropping `self` kills it.
            panic!("the server did not end within {WAIT:?} of SIG{signal}");
        };
        Ended {
            code: self.child.wait().unwrap().code(),
            stdout,
            stderr,
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // After `end` these do nothing.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// Every log line in the data directory, as text.
fn logs(data_dir: &Path) -> String {
    std::fs::read_dir(data_dir.join("logs"))
        .unwrap()
        .map(|entry| std::fs::read_to_string(entry.unwrap().path()).unwrap())
        .collect()
}

fn no_server(data_dir: &Path) -> String {
    format!(
        "ritornelctl: no server is running on {}\n",
        data_dir.display()
    )
}

#[test]
fn status_prints_what_the_running_server_reports_and_fails_once_it_has_stopped() {
    let scratch = Scratch::new();
    let data_dir = scratch.data_dir();
    let server = Server::start(&data_dir);
    let server_id = server.server_id.clone();
    let named = read_json(&data_dir.join("server.json"));
    assert_eq!(named["serverId"], server_id.as_str());

    // For a person: one fact a line.
    let printed = status(&data_dir, &[]);
    assert_eq!((printed.code, printed.stderr.as_str()), (Some(0), ""));
    let facts: Vec<(&str, &str)> = printed
        .stdout
        .lines()
        .map(|line| {
            let (label, value) = line.split_at(18);
            (label.trim_end(), value)
        })
        .collect();
    let logs_dir = data_dir.join("logs");
    assert_eq!(facts.len(), 10, "{}", printed.stdout);
    assert_eq!(
        facts[..7],
        [
            ("Server ID", server_id.as_str()),
            ("Name", "Ritornel server"),
            ("Role", "unassigned"),
            ("Server version", env!("CARGO_PKG_VERSION")),
            ("Contract version", "0"),
            ("SQLite version", "3.54.1"),
            ("Server schema", "5"),
        ]
    );
    assert_eq!(facts[7], ("Library", "none"));
    assert_eq!(facts[8].0, "Free disk, data");
    assert_ne!(facts[8].1, "unknown");
    assert_eq!(facts[9], ("Log path", logs_dir.to_str().unwrap()));

    // For a program: the status as the server sent it.
    let printed = status(&data_dir, &["--json"]);
    assert_eq!((printed.code, printed.stderr.as_str()), (Some(0), ""));
    let json: Value = serde_json::from_str(&printed.stdout).unwrap();
    let free = json["freeDisk"]["dataBytes"].as_u64().unwrap();
    assert!(free > 0);
    assert_eq!(
        json,
        serde_json::json!({
            "serverId": server_id,
            "name": "Ritornel server",
            "role": "unassigned",
            "versions": {
                "server": env!("CARGO_PKG_VERSION"),
                "contract": 0,
                "sqlite": "3.54.1",
                "serverSchema": 5,
            },
            "freeDisk": { "dataBytes": free },
            "logPath": logs_dir.to_str().unwrap(),
        })
    );

    // The data directory may come from the environment instead.
    let mut from_env = ritornelctl(&["status", "--json"]);
    from_env.env("RITORNEL_DATA_DIR", &data_dir);
    let printed = run(from_env);
    assert_eq!(printed.code, Some(0), "{printed:?}");
    assert_eq!(
        serde_json::from_str::<Value>(&printed.stdout).unwrap()["serverId"],
        server_id.as_str()
    );

    // The server knows who asked, and the credential is in nothing either
    // program wrote.
    let token = std::fs::read_to_string(data_dir.join("credential")).unwrap();
    let logs = logs(&data_dir);
    assert!(
        logs.contains(r#""message":"hello","session":1,"client":"ritornelctl""#),
        "{logs}"
    );
    for written in [&logs, &printed.stdout, &named.to_string()] {
        assert!(!written.contains(token.trim()));
    }

    // Asking did not stop it, and stopping it is a clean stop.
    let stopped = server.end("TERM");
    assert_eq!(
        (
            stopped.code,
            stopped.stdout.as_str(),
            stopped.stderr.as_str()
        ),
        (Some(0), "", "")
    );
    assert!(!data_dir.join("server.json").exists());

    let after = status(&data_dir, &[]);
    assert_eq!(after.code, Some(NO_SERVER));
    assert_eq!(after.stdout, "");
    assert_eq!(after.stderr, no_server(&data_dir));
}

/// The facts a command printed for a person: each line's label and value.
fn facts(printed: &Ended) -> Vec<(&str, &str)> {
    assert_eq!(
        (printed.code, printed.stderr.as_str()),
        (Some(0), ""),
        "{printed:?}"
    );
    printed
        .stdout
        .lines()
        .map(|line| {
            let (label, value) = line.split_at(18);
            (label.trim_end(), value)
        })
        .collect()
}

#[test]
fn library_create_makes_the_running_server_a_hub_and_a_second_one_is_refused() {
    let scratch = Scratch::new();
    let data_dir = scratch.data_dir();
    let dir = data_dir.to_str().unwrap();
    let server = Server::start(&data_dir);

    let created = run(ritornelctl(&["library", "create", "--data-dir", dir]));
    let created = facts(&created);
    let [("Library ID", library_id), ("Library revision", "1")] = created[..] else {
        panic!("{created:?}");
    };
    assert_eq!(library_id.len(), 36, "{library_id}");

    // The status shows what now exists.
    let printed = status(&data_dir, &[]);
    let shown = facts(&printed);
    assert_eq!(shown.len(), 12, "{}", printed.stdout);
    assert_eq!(shown[2], ("Role", "hub"));
    assert_eq!(
        shown[6..10],
        [
            ("Server schema", "5"),
            ("Library schema", "4"),
            ("Library ID", library_id),
            ("Library revision", "1"),
        ]
    );
    let json: Value = serde_json::from_str(&status(&data_dir, &["--json"]).stdout).unwrap();
    assert_eq!(json["role"], "hub");
    assert_eq!(json["versions"]["librarySchema"], 4);
    assert_eq!(
        json["library"],
        serde_json::json!({ "libraryId": library_id, "revision": 1 })
    );

    // A server holds one library. Asking again is another command, and it
    // is refused.
    let again = run(ritornelctl(&["library", "create", "--data-dir", dir]));
    assert_eq!((again.code, again.stdout.as_str()), (Some(1), ""));
    assert_eq!(
        again.stderr,
        "ritornelctl: the server answered with an error: conflict: there is a library on this \
         server already\n"
    );

    // The server that starts next on the directory opens the same library.
    let stopped = server.end("TERM");
    assert_eq!(
        (stopped.code, stopped.stderr.as_str()),
        (Some(0), ""),
        "{stopped:?}"
    );
    let server = Server::start(&data_dir);
    let printed = status(&data_dir, &[]);
    let shown = facts(&printed);
    assert_eq!(shown[2], ("Role", "hub"));
    assert_eq!(shown[8], ("Library ID", library_id));
    assert_eq!(shown[9], ("Library revision", "1"));
    assert_eq!(server.end("TERM").code, Some(0));
}

#[test]
fn library_create_prints_its_receipt_as_json_when_asked() {
    let scratch = Scratch::new();
    let data_dir = scratch.data_dir();
    let server = Server::start(&data_dir);

    let mut command = ritornelctl(&["library", "create", "--json"]);
    command.env("RITORNEL_DATA_DIR", &data_dir);
    let printed = run(command);
    assert_eq!((printed.code, printed.stderr.as_str()), (Some(0), ""));
    let json: Value = serde_json::from_str(&printed.stdout).unwrap();
    let mut fields: Vec<_> = json.as_object().unwrap().keys().collect();
    fields.sort_unstable();
    assert_eq!(fields, ["commandId", "libraryId", "revision", "status"]);
    assert_eq!(
        (&json["status"], &json["revision"]),
        (&"done".into(), &1.into())
    );
    assert_eq!(server.end("TERM").code, Some(0));
}

#[test]
fn with_no_server_status_says_so_and_starts_none() {
    let scratch = Scratch::new();
    let data_dir = scratch.data_dir();
    for extra in [&[][..], &["--json"]] {
        let none = status(&data_dir, extra);
        assert_eq!(none.code, Some(NO_SERVER), "{none:?}");
        assert_eq!(none.stdout, "");
        assert_eq!(none.stderr, no_server(&data_dir));
    }
    // It made nothing: not a server, and not even the directory.
    assert!(!data_dir.exists());
}

#[test]
fn a_server_that_was_killed_is_not_taken_for_a_running_one_and_its_port_is_not_asked() {
    let scratch = Scratch::new();
    let data_dir = scratch.data_dir();
    let killed = Server::start(&data_dir).end("KILL");
    assert_eq!(killed.code, None);
    // It had no chance to clear up after itself.
    let left = read_json(&data_dir.join("server.json"));
    let endpoint = left["endpoint"].as_str().unwrap();

    // Something else takes its port, as anything on the machine may, and
    // waits to be spoken to as that server. If the port has gone to
    // another program already, that program is the something else.
    let impersonator = std::net::TcpListener::bind(endpoint);
    if let Err(taken) = &impersonator {
        assert_eq!(taken.kind(), std::io::ErrorKind::AddrInUse, "{taken}");
    }

    for extra in [&[][..], &["--json"]] {
        let after = status(&data_dir, extra);
        assert_eq!(after.code, Some(NO_SERVER), "{after:?}");
        assert_eq!(after.stdout, "");
        assert_eq!(
            after.stderr,
            format!(
                "ritornelctl: no server is running on {} (server.json is left by one that was \
                 killed)\n",
                data_dir.display()
            )
        );
    }

    // Nothing connected to it: not for the descriptor, and so not with
    // the credential either.
    if let Ok(impersonator) = impersonator {
        impersonator.set_nonblocking(true).unwrap();
        let connected = impersonator.accept();
        assert_eq!(
            connected.as_ref().map_err(std::io::Error::kind).err(),
            Some(std::io::ErrorKind::WouldBlock),
            "{connected:?}"
        );
    }
}

#[test]
fn a_wrong_command_line_exits_with_2_and_help_with_0() {
    let wrong = run(ritornelctl(&["start"]));
    assert_eq!(wrong.code, Some(2));
    assert_eq!(wrong.stdout, "");
    assert_eq!(
        wrong.stderr,
        "ritornelctl: unknown argument; try `ritornelctl --help`\n"
    );

    let help = run(ritornelctl(&["--help"]));
    assert_eq!((help.code, help.stderr.as_str()), (Some(0), ""));
    assert!(
        help.stdout.contains("ritornelctl status"),
        "{}",
        help.stdout
    );
}
