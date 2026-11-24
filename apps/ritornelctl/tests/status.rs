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
//! Each wait is for a process to print a line, report a state, or end.
//! [`WAIT`] bounds the waits, and a process that hangs is killed.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod support;

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

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
        let path = tempfile::Builder::new()
            .prefix("ritornelctl-status-")
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
    /// Its exit status as the system says it, which names a signal. A
    /// failure shows it, through `Debug`, which dead-code analysis ignores.
    #[allow(dead_code)]
    status: String,
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
    // Both pipes are at their end, so the process has ended. No test has
    // ritornelctl ended by a signal: one that was, was ended from outside
    // the test, and the failure says so rather than as a code.
    let ended = child.wait().unwrap();
    assert!(
        ended.code().is_some(),
        "{command:?} was ended from outside: {ended}\n--- stdout\n{stdout}--- stderr\n{stderr}"
    );
    Ended {
        code: ended.code(),
        status: ended.to_string(),
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
    let binary = Path::new(env!("CARGO_BIN_EXE_ritornelctl"))
        .with_file_name(format!("ritornel-server{}", std::env::consts::EXE_SUFFIX));
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
        Self::start_with(data_dir, &[])
    }

    /// As [`start`](Self::start), with these variables set.
    fn start_with(data_dir: &Path, variables: &[(&str, &str)]) -> Self {
        let mut child = Command::new(server_binary())
            .arg("serve")
            .arg("--data-dir")
            .arg(data_dir)
            .args(["--listen", "127.0.0.1:0"])
            .env_remove("RITORNEL_DATA_DIR")
            .env_remove("RITORNEL_LISTEN")
            .env_remove("RITORNEL_LOG_LEVEL")
            .env_remove("RITORNEL_BACKUP_DIR")
            .env_remove("RITORNEL_NAME")
            .env_remove("RITORNEL_CACHE_SIZE")
            .env("RITORNEL_TEST_COMPUTER_NAME", "Invented Desk")
            .envs(variables.iter().copied())
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
    /// On Windows `TERM` is `taskkill /PID`, which asks the server's hidden
    /// window to close, and `KILL` is `taskkill /F /PID`, which ends it.
    fn signal(&self, name: &str) {
        let sent = if cfg!(windows) {
            let mut taskkill = Command::new("taskkill");
            match name {
                "TERM" => {}
                "KILL" => {
                    taskkill.arg("/F");
                }
                other => panic!("no Windows equivalent of SIG{other}"),
            }
            taskkill
                .arg("/PID")
                .arg(self.child.id().to_string())
                .output()
                .unwrap()
                .status
        } else {
            Command::new("kill")
                .arg(format!("-{name}"))
                .arg(self.child.id().to_string())
                .status()
                .unwrap()
        };
        assert!(sent.success(), "asking it to stop: {sent}");
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
        let ended = self.child.wait().unwrap();
        Ended {
            code: ended.code(),
            status: ended.to_string(),
            stdout,
            stderr,
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        support::say_if_gone(&mut self.child, Some(&self.stderr));
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
    let contract = ritornel_client::contract::CONTRACT_VERSION.to_string();
    assert_eq!(facts.len(), 13, "{}", printed.stdout);
    assert_eq!(
        facts[..7],
        [
            ("Server ID", server_id.as_str()),
            ("Name", "Invented Desk"),
            ("Role", "unassigned"),
            ("Server version", env!("CARGO_PKG_VERSION")),
            ("Contract version", contract.as_str()),
            ("SQLite version", "3.54.1"),
            ("Server schema", "2"),
        ]
    );
    assert_eq!(facts[7], ("Library", "none"));
    assert_eq!(facts[8].0, "Free disk, data");
    assert_ne!(facts[8].1, "unknown");
    assert_eq!(facts[9], ("Log path", logs_dir.to_str().unwrap()));
    assert_eq!(
        facts[10],
        ("Watching", "off: this server is not the library's hub")
    );
    assert_eq!(
        facts[11],
        (
            "Backup",
            "none: no backup folder is named (backup_dir in config.toml)"
        )
    );
    assert_eq!(facts[12], ("Record", "advancing"));

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
            "name": "Invented Desk",
            "role": "unassigned",
            "versions": {
                "server": env!("CARGO_PKG_VERSION"),
                "contract": ritornel_client::contract::CONTRACT_VERSION,
                "sqlite": "3.54.1",
                "serverSchema": 2,
            },
            "freeDisk": { "dataBytes": free },
            "logPath": logs_dir.to_str().unwrap(),
            "watch": {
                "on": false,
                "enabled": true,
                "off": "this server is not the library's hub",
                "folders": [],
            },
            "backup": {},
            "record": { "problems": [], "notes": [] },
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
        (Some(0), "", ""),
        "{stopped:?}"
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

    // Creating the library wakes the watcher. Wait for it to report that
    // it is on before checking how status prints the new hub.
    let deadline = Instant::now() + WAIT;
    let printed = loop {
        let printed = status(&data_dir, &[]);
        let shown = facts(&printed);
        assert_eq!(shown.len(), 15, "{}", printed.stdout);
        if shown[12] == ("Watching", "on") {
            break printed;
        }
        assert!(
            Instant::now() < deadline,
            "watching did not come on within {WAIT:?}. Last status:\n{}",
            printed.stdout
        );
        // sleep: between looks; the deadline bounds them.
        std::thread::sleep(Duration::from_millis(10));
    };
    let shown = facts(&printed);
    let json: Value = serde_json::from_str(&status(&data_dir, &["--json"]).stdout).unwrap();
    let library_schema = json["versions"]["librarySchema"].as_i64().unwrap();
    assert!(library_schema >= 2);
    let library_schema_text = library_schema.to_string();
    assert_eq!(shown[2], ("Role", "hub"));
    assert_eq!(
        shown[6..10],
        [
            ("Server schema", "2"),
            ("Library schema", library_schema_text.as_str()),
            ("Library ID", library_id),
            ("Library revision", "1"),
        ]
    );
    assert_eq!(json["role"], "hub");
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
    let stopped = server.end("TERM");
    assert_eq!(stopped.code, Some(0), "{stopped:?}");
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
    let stopped = server.end("TERM");
    assert_eq!(stopped.code, Some(0), "{stopped:?}");
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
    // A signal ended it on Unix, with no code; `taskkill /F` ends a process
    // with 1.
    assert_eq!(
        killed.code,
        if cfg!(windows) { Some(1) } else { None },
        "{killed:?}"
    );
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

#[test]
fn server_stop_stops_the_server_cleanly_and_then_finds_none() {
    let scratch = Scratch::new();
    let data_dir = scratch.data_dir();
    let mut server = Server::start(&data_dir);

    let mut command = ritornelctl(&["server", "stop", "--data-dir"]);
    command.arg(&data_dir);
    let stopped = run(command);
    assert_eq!(stopped.code, Some(0), "{stopped:?}");
    assert_eq!(stopped.stdout, "The server has stopped.\n");
    // It stopped as a signal stops it, and cleanly.
    assert_eq!(server.child.wait().unwrap().code(), Some(0));
    assert!(
        logs(&data_dir).contains("\"stopped\""),
        "{}",
        logs(&data_dir)
    );
    assert!(!data_dir.join("server.json").exists());

    let mut command = ritornelctl(&["server", "stop", "--data-dir"]);
    command.arg(&data_dir);
    let none = run(command);
    assert_eq!(none.code, Some(NO_SERVER), "{none:?}");
    assert_eq!(none.stderr, no_server(&data_dir));
}

#[test]
fn settings_say_what_gave_each_and_the_wake_setting_is_changed_from_a_terminal() {
    let scratch = Scratch::new();
    let data_dir = scratch.data_dir();
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::write(data_dir.join("config.toml"), "cache_size = \"512 MiB\"\n").unwrap();
    let server = Server::start_with(&data_dir, &[("RITORNEL_NAME", "Kitchen Speakers")]);
    let settings = |extra: &[&str]| {
        let mut command = ritornelctl(&["settings", "--data-dir"]);
        command.arg(&data_dir).args(extra);
        let ended = run(command);
        assert_eq!(ended.code, Some(0), "{ended:?}");
        ended.stdout
    };
    let read = || serde_json::from_str::<Value>(&settings(&["--json"])).unwrap();

    let shown = read();
    assert_eq!(
        shown["name"],
        json!({ "value": "Kitchen Speakers", "setBy": "environment" })
    );
    assert_eq!(
        shown["cacheBytes"],
        json!({ "value": 512 << 20, "setBy": "file" })
    );
    assert_eq!(
        shown["listen"],
        json!({ "value": { "network": "loopback", "port": 0 }, "setBy": "flag" })
    );
    assert_eq!(
        shown["logLevel"],
        json!({ "value": "info", "setBy": "default" })
    );
    assert_eq!(
        shown["backupDir"],
        json!({ "value": null, "setBy": "default" })
    );
    assert_eq!(
        shown["awake"],
        json!({ "value": { "mode": "while_active", "minutes": 30 }, "setBy": "default" })
    );

    let mut command = ritornelctl(&["settings", "awake", "never", "--data-dir"]);
    command.arg(&data_dir);
    let set = run(command);
    assert_eq!(set.code, Some(0), "{set:?}");
    assert!(
        set.stdout
            .contains("Awake             never (set by a call)\n"),
        "{set:?}"
    );
    let text = settings(&[]);
    assert!(
        text.contains("Name              Kitchen Speakers (environment)\n"),
        "{text}"
    );
    assert!(
        text.contains("Cache             512 MiB (config.toml)\n"),
        "{text}"
    );

    // The change is kept with the server. The name stays too, though
    // nothing gives it now, as the name a server already had.
    let stopped = server.end("TERM");
    assert_eq!(stopped.code, Some(0), "{stopped:?}");
    let server = Server::start(&data_dir);
    let shown = read();
    assert_eq!(
        shown["awake"],
        json!({ "value": { "mode": "never" }, "setBy": "call" })
    );
    assert_eq!(
        shown["name"],
        json!({ "value": "Kitchen Speakers", "setBy": "default" })
    );
    assert_eq!(shown["cacheBytes"]["setBy"], "file");
    let stopped = server.end("TERM");
    assert_eq!(stopped.code, Some(0), "{stopped:?}");
}
