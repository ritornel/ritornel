//! Shared helpers for the `ritornelctl` integration tests.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use serde_json::Value;

/// How long a test waits for a process to do what it should before it kills
/// the process and fails. It is a bound, not a wait: a process that works
/// never comes near it.
pub(crate) const WAIT: Duration = Duration::from_secs(60);

/// What a server that `std::process::abort` ended exits with: no code on
/// Unix, where SIGABRT ended it, and on Windows the fast-fail status
/// (`0xC0000409`) that abort ends a process with.
pub(crate) const ABORTED: Option<i32> = if cfg!(windows) {
    Some(-1_073_740_791)
} else {
    None
};

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

    /// The data directory. The server creates it.
    pub(crate) fn data_dir(&self) -> PathBuf {
        self.0.join("data")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Writes `seconds` of a sine tone at `hz` Hz as 16-bit mono WAV at 8 kHz.
pub(crate) fn write_wav(path: &Path, seconds: u32, hz: f64) {
    let rate: u32 = 8000;
    let frames = rate * seconds;
    let mut bytes = Vec::with_capacity(44 + frames as usize * 2);
    let data = frames * 2;
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16_u32.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&rate.to_le_bytes());
    bytes.extend_from_slice(&(rate * 2).to_le_bytes());
    bytes.extend_from_slice(&2_u16.to_le_bytes());
    bytes.extend_from_slice(&16_u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data.to_le_bytes());
    for frame in 0..frames {
        let t = f64::from(frame) / f64::from(rate);
        let sample = (0.25 * (2.0 * std::f64::consts::PI * hz * t).sin() * 32767.0) as i16;
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    std::fs::File::create(path)
        .unwrap()
        .write_all(&bytes)
        .unwrap();
}

/// Reads a pipe on a thread of its own and hands over each line, so that
/// whoever wants them can stop waiting. The channel closes at the pipe's
/// end, which comes when the process ends.
pub(crate) fn lines(pipe: impl Read + Send + 'static) -> Receiver<String> {
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

/// How a process ended, and what it printed.
#[derive(Debug)]
pub(crate) struct Ended {
    pub(crate) code: Option<i32>,
    /// Its exit status as the system says it, which names a signal. A
    /// failure shows it, through `Debug`, which dead-code analysis ignores.
    #[allow(dead_code)]
    pub(crate) status: String,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
}

/// `ritornelctl` with these arguments, and nothing in its environment that
/// this test did not put there.
pub(crate) fn ritornelctl_command(args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ritornelctl"));
    command
        .args(args)
        .env_remove("RITORNEL_DATA_DIR")
        .env_remove("RITORNEL_BACKUP_DIR")
        .env_remove("RITORNEL_NAME");
    command
}

/// Runs a command that should end by itself. It is killed, and the test
/// fails, if it has not ended within [`WAIT`].
pub(crate) fn run(mut command: Command) -> Ended {
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

/// Runs `ritornelctl` with these arguments on a data directory, and returns
/// its exit code, its standard output and its standard error.
pub(crate) fn ritornelctl(data_dir: &Path, args: &[&str]) -> (Option<i32>, String, String) {
    let mut command = ritornelctl_command(args);
    command.arg("--data-dir").arg(data_dir);
    let ended = run(command);
    (ended.code, ended.stdout, ended.stderr)
}

/// Runs `ritornelctl` and returns what it printed. Unless it succeeded,
/// the test fails with its exit code and everything it printed.
pub(crate) fn ok(data_dir: &Path, args: &[&str]) -> String {
    let (code, stdout, stderr) = ritornelctl(data_dir, args);
    assert_eq!(
        code,
        Some(0),
        "ritornelctl {args:?}\n--- stdout\n{stdout}--- stderr\n{stderr}"
    );
    stdout
}

/// `ritornelctl` with `--json`, which must succeed, as the JSON it printed.
pub(crate) fn json(data_dir: &Path, args: &[&str]) -> Value {
    let mut args = args.to_vec();
    args.push("--json");
    serde_json::from_str(&ok(data_dir, &args)).unwrap()
}

/// Asks the server, again and again, until `done` says so, for at most
/// [`WAIT`]. Each ask is a whole `ritornelctl` run, which is the wait.
pub(crate) fn until(data_dir: &Path, args: &[&str], done: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + WAIT;
    loop {
        let answer = json(data_dir, args);
        if done(&answer) {
            return answer;
        }
        assert!(
            Instant::now() < deadline,
            "{args:?} never got there: {answer}"
        );
    }
}

/// As [`until`], pausing `pause` between looks.
pub(crate) fn until_every(
    data_dir: &Path,
    args: &[&str],
    pause: Duration,
    done: impl Fn(&Value) -> bool,
) -> Value {
    let deadline = Instant::now() + WAIT;
    loop {
        let answer = json(data_dir, args);
        if done(&answer) {
            return answer;
        }
        assert!(
            Instant::now() < deadline,
            "{args:?} never got there: {answer}"
        );
        // sleep: between looks; the deadline bounds them.
        std::thread::sleep(pause);
    }
}

/// The `ritornel-server` that was built beside `ritornelctl`.
pub(crate) fn server_binary() -> PathBuf {
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

/// A `ritornel-server serve` command on `data_dir` and a system-picked port,
/// with ambient `RITORNEL_*` and `SCROBL_*` variables cleared.
pub(crate) fn server_command(data_dir: &Path) -> Command {
    let mut command = Command::new(server_binary());
    command
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
        .env_remove("SCROBL_API_KEY")
        .env_remove("SCROBL_API_SECRET")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        // The server logs to a file and says on stderr only what went wrong,
        // which then shows with the failing test.
        .stderr(Stdio::inherit());
    command
}

/// As [`server_command`], playing through the null output and leaving the
/// system's media session alone.
pub(crate) fn null_output_command(data_dir: &Path, variables: &[(&str, &str)]) -> Command {
    let mut command = server_command(data_dir);
    command
        .args(["--output", "null"])
        // A test that plays leaves the media keys where they were.
        .env("RITORNEL_TEST_NO_MEDIA_SESSION", "1")
        .envs(variables.iter().copied());
    command
}

/// A `ritornel-server` that is running, on a port the system picked. It is
/// killed if a test ends without stopping it, so a failed test leaves no
/// process behind.
pub(crate) struct Server {
    child: Child,
    stdout: Receiver<String>,
    stderr: Option<Receiver<String>>,
    /// The server ID from the ready line.
    pub(crate) server_id: String,
}

impl Server {
    /// Starts a server on `data_dir`, with these variables in its
    /// environment, and waits for its ready line.
    pub(crate) fn start(data_dir: &Path, variables: &[(&str, &str)]) -> Self {
        let mut command = server_command(data_dir);
        command.envs(variables.iter().copied());
        Self::start_command(command, false)
    }

    /// Starts a server on `data_dir` using the null output, with these
    /// variables in its environment, and waits for its ready line.
    pub(crate) fn start_null_output(data_dir: &Path, variables: &[(&str, &str)]) -> Self {
        Self::start_command(null_output_command(data_dir, variables), false)
    }

    /// Starts a server on `data_dir` with captured stderr and an invented
    /// computer name, and waits for its ready line.
    pub(crate) fn start_captured(data_dir: &Path, variables: &[(&str, &str)]) -> Self {
        let mut command = server_command(data_dir);
        command
            .env("RITORNEL_TEST_COMPUTER_NAME", "Invented Desk")
            .envs(variables.iter().copied());
        Self::start_command(command, true)
    }

    /// Starts a prepared `ritornel-server` command and waits for its ready
    /// line.
    pub(crate) fn start_command(mut command: Command, capture_stderr: bool) -> Self {
        if capture_stderr {
            command.stderr(Stdio::piped());
        }
        let mut child = command.spawn().unwrap();
        let stdout = lines(child.stdout.take().unwrap());
        let stderr = capture_stderr.then(|| lines(child.stderr.take().unwrap()));
        let ready = stdout.recv_timeout(WAIT);
        let Some(server_id) = ready
            .as_deref()
            .ok()
            .and_then(|line| line.strip_prefix(&format!("ready pid={} server_id=", child.id())))
        else {
            let _ = child.kill();
            let _ = child.wait();
            if let Some(stderr) = stderr.as_ref() {
                panic!(
                    "no ready line within {WAIT:?}. stdout: {ready:?}, stderr: {:?}",
                    rest(stderr)
                );
            }
            panic!("no ready line within {WAIT:?}: {ready:?}");
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
    pub(crate) fn signal(&self, name: &str) {
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

    /// Waits for the server to end by itself, and says how it did: its
    /// exit status names the signal when one ended it, as an abort does.
    pub(crate) fn ended(mut self) -> ExitStatus {
        assert!(
            rest(&self.stdout).is_some(),
            "the server did not end within {WAIT:?}"
        );
        self.child.wait().unwrap()
    }

    /// Stops the server with SIGTERM and waits for it to end cleanly.
    pub(crate) fn stop(self) {
        self.signal("TERM");
        let ended = self.ended();
        assert!(ended.success(), "the server ended with {ended}");
    }

    /// Sends the server a signal and waits for it to end, returning its
    /// exit status and captured stdout/stderr.
    pub(crate) fn end(mut self, signal: &str) -> Ended {
        self.signal(signal);
        let Some(stdout) = rest(&self.stdout) else {
            // Dropping `self` kills it.
            panic!("the server did not end within {WAIT:?} of SIG{signal}");
        };
        let stderr = match self.stderr.as_ref() {
            Some(pipe) => rest(pipe).unwrap_or_else(|| {
                panic!("the server did not end within {WAIT:?} of SIG{signal}");
            }),
            None => String::new(),
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
        say_if_gone(&mut self.child, self.stderr.as_ref());
        // After `ended`/`end` these do nothing.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Every log line in the data directory, as text.
pub(crate) fn logs(data_dir: &Path) -> String {
    std::fs::read_dir(data_dir.join("logs"))
        .unwrap()
        .map(|entry| std::fs::read_to_string(entry.unwrap().path()).unwrap())
        .collect()
}

/// Says, while a test is failing, why its server is gone if it is: how it
/// ended, which names the signal when one ended it, and what it wrote to
/// stderr, from `stderr` when the test reads it: the pipe ends with the
/// process, so what is still on its way is waited for, five seconds at
/// most in all. A command that found no server running then fails beside the
/// reason. It says nothing of a server that is running or that ended
/// cleanly.
// What it says is for the failure's output, which nextest shows.
#[allow(clippy::print_stderr)]
pub(crate) fn say_if_gone(child: &mut Child, stderr: Option<&Receiver<String>>) {
    if !std::thread::panicking() {
        return;
    }
    let Ok(Some(status)) = child.try_wait() else {
        return;
    };
    if status.success() {
        return;
    }
    eprintln!(
        "the test's ritornel-server, pid {}, had ended: {status}",
        child.id()
    );
    if let Some(stderr) = stderr {
        let until = Instant::now() + Duration::from_secs(5);
        while let Ok(line) = stderr.recv_timeout(until.saturating_duration_since(Instant::now())) {
            eprintln!("ritornel-server: {line}");
        }
    }
}
