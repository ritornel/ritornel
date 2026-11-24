//! The real `ritornelctl backup` and `export` on the real
//! `ritornel-server`. A backup taken while the server plays is restored
//! into another data directory, where the history it holds is the history
//! a rebuild makes; with no backup folder named there is no backup,
//! nothing is written beside the data in its place, and the status says
//! so. An export is read back as plain JSON Lines and CSV. A listen that
//! cannot be saved stops the record, and the status says so while the
//! music goes on. See "Backups and pre-migration snapshots" in
//! docs/internals/server.md and "Export" in
//! docs/internals/history.md.
//!
//! As in `play.rs`, `ritornel-server` is taken from beside `ritornelctl`,
//! and nothing here sleeps: each wait is for a process to print a line or
//! to end, or for an answer from the server, and is bounded.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod support;

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use serde_json::Value;

/// How long a test waits for a process before it kills it and fails.
const WAIT: Duration = Duration::from_secs(60);

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let path = tempfile::Builder::new()
            .prefix("ritornelctl-backup-")
            .tempdir()
            .unwrap()
            .keep();
        support::close_scratch(&path);
        Self(path)
    }

    fn data_dir(&self) -> PathBuf {
        self.0.join("data")
    }

    /// A folder of two invented tracks of these lengths in seconds.
    fn album(&self, name: &str, seconds: [u32; 2]) -> PathBuf {
        let folder = self.0.canonicalize().unwrap().join("music").join(name);
        std::fs::create_dir_all(&folder).unwrap();
        let names = [("01 First.wav", 440.0), ("02 Second.wav", 660.0)];
        for ((name, hz), seconds) in names.into_iter().zip(seconds) {
            write_wav(&folder.join(name), seconds, hz);
        }
        folder
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn write_wav(path: &Path, seconds: u32, hz: f64) {
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

fn lines(pipe: impl Read + Send + 'static) -> Receiver<String> {
    let (send, lines) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(pipe).lines() {
            if send.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    lines
}

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

fn ritornelctl(data_dir: &Path, args: &[&str]) -> (Option<i32>, String, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ritornelctl"))
        .args(args)
        .arg("--data-dir")
        .arg(data_dir)
        .env_remove("RITORNEL_DATA_DIR")
        .env_remove("RITORNEL_BACKUP_DIR")
        .env_remove("RITORNEL_NAME")
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
        panic!("ritornelctl {args:?} did not end within {WAIT:?}");
    };
    let ended = child.wait().unwrap();
    // No test has ritornelctl ended by a signal: one that was, was ended from
    // outside the test, and the failure says so rather than as a code.
    assert!(
        ended.code().is_some(),
        "ritornelctl {args:?} was ended from outside: {ended}\n--- stdout\n{stdout}--- stderr\n{stderr}"
    );
    (ended.code(), stdout, stderr)
}

fn json(data_dir: &Path, args: &[&str]) -> Value {
    let mut args = args.to_vec();
    args.push("--json");
    serde_json::from_str(&ok(data_dir, &args)).unwrap()
}

/// Runs `ritornelctl` and returns what it printed. Unless it succeeded,
/// the test fails with its exit code and everything it printed.
fn ok(data_dir: &Path, args: &[&str]) -> String {
    let (code, stdout, stderr) = ritornelctl(data_dir, args);
    assert_eq!(
        code,
        Some(0),
        "ritornelctl {args:?}\n--- stdout\n{stdout}--- stderr\n{stderr}"
    );
    stdout
}

/// Asks the server, again and again, until `done` says so, for at most
/// [`WAIT`]. Each ask is a whole `ritornelctl` run, which is the wait.
fn until(data_dir: &Path, args: &[&str], done: impl Fn(&Value) -> bool) -> Value {
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

/// A `ritornel-server` on the null output.
struct Server {
    child: Child,
    stdout: Receiver<String>,
}

fn spawn(data_dir: &Path, variables: &[(&str, &str)]) -> (Child, Receiver<String>) {
    let binary = Path::new(env!("CARGO_BIN_EXE_ritornelctl"))
        .with_file_name(format!("ritornel-server{}", std::env::consts::EXE_SUFFIX));
    assert!(
        binary.exists(),
        "there is no ritornel-server beside ritornelctl: build the workspace"
    );
    let mut child = Command::new(binary)
        .arg("serve")
        .arg("--data-dir")
        .arg(data_dir)
        .args(["--listen", "127.0.0.1:0", "--output", "null"])
        .env_remove("RITORNEL_DATA_DIR")
        .env_remove("RITORNEL_LISTEN")
        .env_remove("RITORNEL_LOG_LEVEL")
        .env_remove("RITORNEL_BACKUP_DIR")
        .env_remove("RITORNEL_NAME")
        // A test that plays leaves the media keys where they were.
        .env("RITORNEL_TEST_NO_MEDIA_SESSION", "1")
        .envs(variables.iter().copied())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        // The server logs to a file and says on stderr only what went wrong,
        // which then shows with the failing test.
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let stdout = lines(child.stdout.take().unwrap());
    (child, stdout)
}

impl Server {
    fn start(data_dir: &Path, variables: &[(&str, &str)]) -> Self {
        let (mut child, stdout) = spawn(data_dir, variables);
        let ready = stdout.recv_timeout(WAIT);
        if !ready
            .as_deref()
            .is_ok_and(|line| line.starts_with("ready pid="))
        {
            let _ = child.kill();
            let _ = child.wait();
            panic!("no ready line within {WAIT:?}: {ready:?}");
        }
        Self { child, stdout }
    }

    /// Waits for the server to end by itself, and says how it did: its
    /// exit status names the signal when one ended it, as an abort does.
    fn ended(mut self) -> ExitStatus {
        assert!(
            rest(&self.stdout).is_some(),
            "the server did not end within {WAIT:?}"
        );
        self.child.wait().unwrap()
    }

    fn stop(self) {
        // `taskkill /PID` without `/F` is what Windows has for SIGTERM: it
        // asks the server's hidden window to close.
        let sent = if cfg!(windows) {
            Command::new("taskkill")
                .arg("/PID")
                .arg(self.child.id().to_string())
                .output()
                .unwrap()
                .status
        } else {
            Command::new("kill")
                .arg("-TERM")
                .arg(self.child.id().to_string())
                .status()
                .unwrap()
        };
        assert!(sent.success(), "asking it to stop: {sent}");
        let ended = self.ended();
        assert!(ended.success(), "the server ended with {ended}");
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        support::say_if_gone(&mut self.child, None);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn with_no_backup_folder_there_is_no_backup_and_nothing_is_written_beside_the_data() {
    let scratch = Scratch::new();
    let data_dir = scratch.data_dir();
    let server = Server::start(&data_dir, &[]);
    ok(&data_dir, &["library", "create"]);

    let status = json(&data_dir, &["status"]);
    assert_eq!(status["backup"], serde_json::json!({}), "{status}");
    let stdout = ok(&data_dir, &["status"]);
    assert!(
        stdout.contains("Backup            none: no backup folder is named"),
        "{stdout}"
    );

    let (code, stdout, stderr) = ritornelctl(&data_dir, &["backup"]);
    assert_eq!((code, stdout.as_str()), (Some(1), ""));
    assert!(stderr.contains("no backup folder is named"), "{stderr}");
    server.stop();

    // Nothing was written in its place: the data directory holds what it
    // held, and no backups.
    assert!(!data_dir.join("backups").exists());
    let mut names: Vec<_> = std::fs::read_dir(&data_dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| !name.starts_with("server.") && !name.starts_with("library."))
        .collect();
    names.sort();
    assert_eq!(names, ["cache", "credential", "logs"]);
}

#[test]
fn a_backup_taken_while_the_server_plays_is_restored_elsewhere_with_its_history_whole() {
    let scratch = Scratch::new();
    let data_dir = scratch.data_dir();
    let place = scratch.0.join("backups elsewhere");
    std::fs::create_dir_all(&place).unwrap();
    let short = scratch.album("Short Album", [40, 40]);
    let long = scratch.album("Long Album", [600, 600]);

    // Twenty times real time: the short album is over in four seconds and
    // the long one plays on for a minute.
    let server = Server::start(
        &data_dir,
        &[
            ("RITORNEL_TEST_NULL_OUTPUT_SPEED", "20"),
            ("RITORNEL_BACKUP_DIR", place.to_str().unwrap()),
        ],
    );
    ok(&data_dir, &["library", "create"]);
    for folder in [&short, &long] {
        ok(&data_dir, &["scan", folder.to_str().unwrap()]);
    }
    let status = json(&data_dir, &["status"]);
    assert_eq!(status["backup"]["place"], place.to_str().unwrap());
    assert!(status["backup"].get("last").is_none(), "{status}");

    let (code, _, stderr) = ritornelctl(&data_dir, &["play", short.to_str().unwrap(), "--follow"]);
    assert_eq!((code, stderr.as_str()), (Some(0), ""));
    until(&data_dir, &["history"], |recent| recent["undelivered"] == 0);

    // The long album plays, and the backup is taken while it does.
    ok(&data_dir, &["play", long.to_str().unwrap()]);
    until(&data_dir, &["now"], |now| {
        now["status"] == "playing" && now["positionMs"].as_u64() > Some(0)
    });
    let backed_up = json(&data_dir, &["backup"]);
    let now = json(&data_dir, &["now"]);
    assert_eq!(now["status"], "playing", "playback goes on: {now}");
    let backup = PathBuf::from(backed_up["backup"]["path"].as_str().unwrap());
    assert_eq!(backup.parent(), Some(place.as_path()));
    let status = json(&data_dir, &["status"]);
    assert_eq!(
        status["backup"]["last"]["path"],
        backed_up["backup"]["path"]
    );
    assert_eq!(status["backup"]["last"]["reason"], "on_demand");
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(backup.join("backup.json")).unwrap()).unwrap();
    assert_eq!(
        manifest["files"],
        serde_json::json!(["server.db", "library.db"])
    );
    assert_eq!(manifest["libraryId"], status["library"]["libraryId"]);
    let history = json(&data_dir, &["history"]);
    let played = history["plays"].as_array().unwrap().len();
    assert_eq!(played, 2);
    let before_backup: Vec<Value> = history["plays"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|play| play["listens"].as_array().unwrap())
        .map(|listen| listen["listenId"].clone())
        .collect();
    server.stop();
    // The backup is not beside the data.
    assert!(!data_dir.join("backups").exists());

    // Another machine: an empty data directory with the backup's two
    // databases put in it.
    let restored = scratch.0.join("restored");
    std::fs::create_dir_all(&restored).unwrap();
    for name in ["server.db", "library.db"] {
        std::fs::copy(backup.join(name), restored.join(name)).unwrap();
    }
    let server = Server::start(&restored, &[]);
    let status = json(&restored, &["status"]);
    assert_eq!(status["serverId"], manifest["serverId"]);
    assert_eq!(status["library"]["libraryId"], manifest["libraryId"]);
    // The listen that was playing when the backup was taken was in
    // server.db, and arrives too. Then the history the server kept is the
    // one a rebuild would make.
    let recent = until(&restored, &["history"], |recent| recent["undelivered"] == 0);
    let plays = recent["plays"].as_array().unwrap();
    assert_eq!(plays.len(), played + 1, "{recent}");
    // Newest first: the long album's first track, which was playing, and
    // which the backup's server.db still held as a listen in progress.
    let [listen] = plays[0]["listens"].as_array().unwrap().as_slice() else {
        panic!("one listen: {recent}");
    };
    assert_eq!(plays[0]["durationMs"], 600_000);
    assert_eq!(listen["endReason"], "crash");
    assert!(
        !before_backup.contains(&listen["listenId"]),
        "it was not in the library before: {recent}"
    );
    let rebuilt = json(&restored, &["history", "rebuild", "--dry-run"]);
    assert_eq!(rebuilt["changed"].as_u64(), Some(0), "{rebuilt}");
    assert_eq!(rebuilt["before"], rebuilt["after"]);
    server.stop();
    // Nothing needed migrating, so nothing was copied beside it.
    assert!(!restored.join("backups").exists());
}

#[test]
fn an_export_is_read_back_without_ritornel_and_holds_a_skipped_listen_with_where_it_was_played_from()
 {
    let scratch = Scratch::new();
    let data_dir = scratch.data_dir();
    let album = scratch.album("Long Album", [600, 600]);
    let out = scratch.0.join("exports");
    std::fs::create_dir_all(&out).unwrap();
    let server = Server::start(&data_dir, &[("RITORNEL_TEST_NULL_OUTPUT_SPEED", "20")]);
    ok(&data_dir, &["library", "create"]);
    let folder = album.to_str().unwrap();
    ok(&data_dir, &["scan", folder]);

    // The first track is skipped a little way in, and the second stopped.
    ok(&data_dir, &["play", folder]);
    until(&data_dir, &["now"], |now| {
        now["status"] == "playing" && now["positionMs"].as_u64() > Some(0)
    });
    ok(&data_dir, &["next"]);
    // A track stopped before any of it is heard leaves no listen: the
    // second is stopped once some of it has played.
    until(&data_dir, &["now"], |now| {
        now["index"] == 1 && now["positionMs"].as_u64() > Some(0)
    });
    ok(&data_dir, &["stop"]);
    until(&data_dir, &["history"], |recent| {
        recent["undelivered"] == 0 && recent["plays"].as_array().unwrap().len() == 2
    });

    let exported = json(&data_dir, &["export", out.to_str().unwrap()]);
    assert_eq!(exported["observations"], 2, "{exported}");
    let path = PathBuf::from(exported["path"].as_str().unwrap());
    assert_eq!(path.parent(), Some(out.as_path()));
    server.stop();

    // A listening history is its owner's alone: a file's mode is the
    // system's to say.
    #[cfg(unix)]
    for file in std::fs::read_dir(&path).unwrap() {
        use std::os::unix::fs::PermissionsExt;
        let file = file.unwrap();
        let mode = file.metadata().unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "{:?}", file.file_name());
    }

    // From here on, only what is in the folder.
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(path.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["format"], "ritornel-record");
    let lines: Vec<Value> = std::fs::read_to_string(path.join("observations.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let skipped = lines
        .iter()
        .find(|line| line["listen"]["endReason"] == "next")
        .unwrap();
    assert_eq!(skipped["kind"], "listen");
    assert_eq!(skipped["listen"]["startReason"], "user_pick");
    assert_eq!(
        skipped["listen"]["playedFrom"]["kind"],
        skipped["record"]["contextKind"]
    );
    assert!(skipped["listen"]["msPlayed"].as_u64() > Some(0));
    assert!(
        skipped["item"]["key"]
            .as_str()
            .unwrap()
            .ends_with("01 First.wav")
    );
    let plays = std::fs::read_to_string(path.join("plays.csv")).unwrap();
    let mut rows = plays.lines();
    assert!(
        rows.next()
            .unwrap()
            .starts_with("play,listener,played_at_utc,")
    );
    assert_eq!(rows.count(), 2);
}

#[test]
fn while_a_listen_cannot_be_saved_playback_goes_on_and_the_status_says_so_and_since_when() {
    let scratch = Scratch::new();
    let data_dir = scratch.data_dir();
    let album = scratch.album("Long Album", [600, 600]);
    // Past a minute into a track, every save of its listen fails, as a full
    // disk's would. It plays in real time, so that the next track's first
    // minute, while its saves work again, outlasts any wait for the status
    // to say so.
    let server = Server::start(
        &data_dir,
        &[
            ("RITORNEL_TEST_NULL_OUTPUT_SPEED", "1"),
            ("RITORNEL_TEST_FAIL_SAVES_AFTER_MS", "60000"),
        ],
    );
    ok(&data_dir, &["library", "create"]);
    let folder = album.to_str().unwrap();
    ok(&data_dir, &["scan", folder]);
    let status = json(&data_dir, &["status"]);
    assert_eq!(status["record"]["problems"], serde_json::json!([]));
    let stdout = ok(&data_dir, &["status"]);
    assert!(stdout.contains("Record            advancing"), "{stdout}");

    ok(&data_dir, &["play", folder]);
    // A seek is saved at once: there, the save fails.
    ok(&data_dir, &["seek", "1:01"]);
    let status = until(&data_dir, &["status"], |status| {
        !status["record"]["problems"].as_array().unwrap().is_empty()
    });
    let problem = &status["record"]["problems"][0];
    assert_eq!(problem["kind"], "listen_unsaved", "{status}");
    assert_eq!(problem["dismissable"], false);
    assert!(problem["since"].as_i64() > Some(0));
    // The music goes on.
    let before = json(&data_dir, &["now"])["positionMs"].as_u64().unwrap();
    let now = until(&data_dir, &["now"], |now| {
        now["positionMs"].as_u64().unwrap() > before
    });
    assert_eq!(now["status"], "playing");
    let stdout = ok(&data_dir, &["status"]);
    assert!(
        stdout.contains("Record            The listen playing can’t be saved, since "),
        "{stdout}"
    );
    // Dismissing sets aside only scrobbles: this stays until put right.
    ok(&data_dir, &["status", "--dismiss"]);
    let status = json(&data_dir, &["status"]);
    assert_eq!(status["record"]["problems"][0]["kind"], "listen_unsaved");

    // Stopped, the next track's first seconds save again, and the sign
    // goes.
    ok(&data_dir, &["next"]);
    until(&data_dir, &["status"], |status| {
        status["record"]["problems"] == serde_json::json!([])
    });
    server.stop();
}
