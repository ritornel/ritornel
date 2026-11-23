//! The real `ritornelctl` playing a folder on the real `ritornel-server`
//! through the null output, which makes no sound, and the server killed in
//! the middle of a track: on the next run the queue and the position are
//! back, nothing plays, the listen is closed as a crash with at most five
//! seconds lost, and it reaches history once, however often it is sent.
//! See "Recording a listen" in docs/internals/playback.md.
//!
//! As in `scan.rs`, `ritornel-server` is taken from beside `ritornelctl`,
//! and nothing here sleeps: each wait is for a process to print a line or
//! to end, or for an answer from the server, and is bounded.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use serde_json::Value;

/// How long a test waits for a process before it kills it and fails.
const WAIT: Duration = Duration::from_secs(60);

/// What a server that `std::process::abort` ended exits with: no code on
/// Unix, where SIGABRT ended it, and on Windows the fast-fail status
/// (`0xC0000409`) that abort ends a process with.
const ABORTED: Option<i32> = if cfg!(windows) {
    Some(-1_073_740_791)
} else {
    None
};

/// How long a track of the invented album is.
const TRACK_SECONDS: u32 = 40;

/// Where the dying server dies: past this position in the track.
const DIES_AT_MS: u64 = 7_000;

/// How often a listen is saved while it plays.
const CHECKPOINT_MS: u64 = 5_000;

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let path = tempfile::Builder::new()
            .prefix("ritornelctl-play-")
            .tempdir()
            .unwrap()
            .keep();
        Self(path)
    }

    fn data_dir(&self) -> PathBuf {
        self.0.join("data")
    }

    /// A folder of two invented tracks: forty seconds of tone each, as
    /// 16-bit WAV at 8 kHz, written here so that no fixture has to be that
    /// long.
    fn music(&self) -> PathBuf {
        self.album([TRACK_SECONDS, TRACK_SECONDS])
    }

    /// Two invented tracks of these lengths in seconds.
    fn album(&self, seconds: [u32; 2]) -> PathBuf {
        let folder = self.0.canonicalize().unwrap().join("music/Invented Album");
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
    (child.wait().unwrap().code(), stdout, stderr)
}

fn json(data_dir: &Path, args: &[&str]) -> Value {
    let mut args = args.to_vec();
    args.push("--json");
    let (code, stdout, stderr) = ritornelctl(data_dir, &args);
    assert_eq!(code, Some(0), "{args:?}: {stderr}");
    serde_json::from_str(&stdout).unwrap()
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
        .stderr(Stdio::null())
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

    /// Waits for the server to end by itself: `None` when a signal ended
    /// it, as an abort does.
    fn ended(mut self) -> Option<i32> {
        assert!(
            rest(&self.stdout).is_some(),
            "the server did not end within {WAIT:?}"
        );
        self.child.wait().unwrap().code()
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
        assert!(sent.success());
        assert_eq!(self.ended(), Some(0));
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Runs a server that is to die before or just after it is ready, and
/// waits for it to.
fn run_until_it_dies(data_dir: &Path, variables: &[(&str, &str)]) -> Option<i32> {
    let (mut child, stdout) = spawn(data_dir, variables);
    assert!(
        rest(&stdout).is_some(),
        "the server did not die within {WAIT:?}"
    );
    child.wait().unwrap().code()
}

/// A whole-hour zone in which it is now between noon and one, as `TZ`
/// names it: a server put there has eleven hours before its "today" ends.
fn a_zone_at_noon() -> String {
    let hour = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        / 3600
        % 24;
    // From UTC-11 to UTC+12; the Etc names count the other way.
    match 12 - i64::try_from(hour).unwrap() {
        0 => "Etc/UTC".to_owned(),
        east if east > 0 => format!("Etc/GMT-{east}"),
        west => format!("Etc/GMT+{}", -west),
    }
}

#[test]
fn ritornelctl_play_plays_a_folder_start_to_finish_and_every_listen_reaches_history() {
    let scratch = Scratch::new();
    let (data_dir, music) = (scratch.data_dir(), scratch.music());
    let folder = music.to_str().unwrap();
    // Fifty times real time: the album is over in under a second. The
    // server's zone is one far from midnight, so that "today" cannot turn
    // between the plays and the question of what was played on it.
    let zone = a_zone_at_noon();
    let server = Server::start(
        &data_dir,
        &[("RITORNEL_TEST_NULL_OUTPUT_SPEED", "50"), ("TZ", &zone)],
    );
    assert_eq!(ritornelctl(&data_dir, &["library", "create"]).0, Some(0));
    assert_eq!(ritornelctl(&data_dir, &["scan", folder]).0, Some(0));

    let (code, stdout, stderr) = ritornelctl(&data_dir, &["play", folder, "--follow"]);
    assert_eq!((code, stderr.as_str()), (Some(0), ""));
    assert!(stdout.contains("  1/2  "), "{stdout}");
    assert!(stdout.contains("  2/2  "), "{stdout}");
    assert!(stdout.contains("Status            Stopped"), "{stdout}");

    let recent = until(&data_dir, &["history"], |recent| recent["undelivered"] == 0);
    let plays = recent["plays"].as_array().unwrap();
    assert_eq!(plays.len(), 2);
    for play in plays {
        assert_eq!(play["msPlayed"], u64::from(TRACK_SECONDS) * 1000);
        assert_eq!(play["counts"], true);
        assert_eq!(play["outcome"], "listened");
        assert_eq!(play["clockBasis"], "hub_age");
        assert_eq!(play["zone"], zone.as_str());
        assert_eq!(play["listens"][0]["endReason"], "finished");
    }
    // Newest first: the second track began on its own when the first
    // ended.
    assert_eq!(plays[1]["listens"][0]["startReason"], "user_pick");
    assert_eq!(plays[0]["listens"][0]["startReason"], "auto_advance");
    assert!(plays[0]["at"].as_i64() > plays[1]["at"].as_i64());
    // "On this day" has both, under today.
    let today = json(&data_dir, &["history", "on-this-day"]);
    assert_eq!(today["plays"].as_array().unwrap().len(), 2);
    // A rebuild finds nothing to change.
    let rebuilt = json(&data_dir, &["history", "rebuild"]);
    assert_eq!(
        (rebuilt["changed"].as_u64(), rebuilt["after"].as_u64()),
        (Some(0), Some(2))
    );
    server.stop();
}

#[test]
fn a_server_killed_mid_track_comes_back_paused_and_its_listen_reaches_history_once_as_a_crash() {
    let scratch = Scratch::new();
    let (data_dir, music) = (scratch.data_dir(), scratch.music());
    let folder = music.to_str().unwrap();

    // At real-time speed, so that the five-second checkpoints are what
    // they are in use. The server dies seven seconds into the first track,
    // two after its last checkpoint, saving nothing more.
    let dies_at = DIES_AT_MS.to_string();
    let dying = Server::start(
        &data_dir,
        &[("RITORNEL_TEST_FAIL_AT_POSITION_MS", dies_at.as_str())],
    );
    assert_eq!(ritornelctl(&data_dir, &["library", "create"]).0, Some(0));
    assert_eq!(ritornelctl(&data_dir, &["scan", folder]).0, Some(0));
    assert_eq!(ritornelctl(&data_dir, &["play", folder]).0, Some(0));
    assert_eq!(dying.ended(), ABORTED);

    // The next run closes the listen as a crash, and dies itself just
    // after the library has taken it in and before server.db lets it go.
    assert_eq!(
        run_until_it_dies(&data_dir, &[("RITORNEL_TEST_FAIL_AFTER_INGEST", "1")]),
        ABORTED
    );

    // The run after that is a normal one.
    let server = Server::start(&data_dir, &[("RITORNEL_TEST_NULL_OUTPUT_SPEED", "20")]);
    // The queue and the position are back, and nothing plays.
    let now = json(&data_dir, &["now"]);
    assert_eq!(now["status"], "paused");
    assert_eq!(
        (now["index"].as_u64(), now["queueLength"].as_u64()),
        (Some(0), Some(2))
    );
    let position = now["positionMs"].as_u64().unwrap();
    assert!(
        (CHECKPOINT_MS..DIES_AT_MS).contains(&position),
        "the position saved with the last checkpoint: {position}"
    );
    assert_eq!(
        json(&data_dir, &["now"])["positionMs"].as_u64(),
        Some(position)
    );

    // The crash's listen was sent again by this run, and history has it
    // once: one play, one listen, closed as a crash with what its last
    // checkpoint saved.
    let recent = until(&data_dir, &["history"], |recent| recent["undelivered"] == 0);
    let [play] = recent["plays"].as_array().unwrap().as_slice() else {
        panic!("one play: {recent}");
    };
    let [listen] = play["listens"].as_array().unwrap().as_slice() else {
        panic!("one listen: {play}");
    };
    assert_eq!(listen["endReason"], "crash");
    // Sent by the next run on the same boot of the machine, it is still
    // dated by its age, where the kernel's boot ID can be read.
    let expected_basis = if cfg!(any(target_os = "linux", target_os = "macos", windows)) {
        "hub_age"
    } else {
        "server_wall"
    };
    assert_eq!(play["clockBasis"], expected_basis);
    let saved = listen["msPlayed"].as_u64().unwrap();
    assert!(
        saved + CHECKPOINT_MS >= DIES_AT_MS && saved <= DIES_AT_MS,
        "at most five seconds lost: {saved} saved of {DIES_AT_MS}"
    );
    assert_eq!(listen["endPositionMs"].as_u64(), Some(position));
    // Sent twice, it was the second send that found it there: the library
    // was not written again.
    let revision = json(&data_dir, &["status"])["library"]["revision"].clone();

    // Resumed, the track carries on from the checkpoint as a new listen
    // that continues the one the crash closed.
    json(&data_dir, &["resume"]);
    until(&data_dir, &["now"], |now| {
        now["positionMs"]
            .as_u64()
            .is_some_and(|at| at > position + 2_000)
    });
    json(&data_dir, &["stop"]);
    let recent = until(&data_dir, &["history"], |recent| {
        recent["undelivered"] == 0
            && recent["plays"][0]["listens"]
                .as_array()
                .is_some_and(|all| all.len() == 2)
    });
    let play = &recent["plays"][0];
    assert_eq!(play["listens"][1]["startReason"], "resume");
    assert_eq!(play["listens"][1]["continues"], listen["listenId"]);
    assert_eq!(
        play["listens"][1]["startPositionMs"].as_u64(),
        Some(position)
    );
    assert_eq!(play["listens"][1]["endReason"], "stopped");
    // The resumed play is dated where the first listen began.
    assert_eq!(play["listens"][0]["startedAt"], listen["startedAt"]);
    assert_ne!(
        json(&data_dir, &["status"])["library"]["revision"],
        revision
    );
    server.stop();
}

#[test]
fn a_server_killed_just_after_a_join_keeps_both_listens_and_the_boundary_between_them() {
    let scratch = Scratch::new();
    let data_dir = scratch.data_dir();
    // A three-second track joined to a forty-second one, at real time. The
    // server dies four seconds into the second, which the first never
    // reaches, before the second's first five-second save.
    let music = scratch.album([3, TRACK_SECONDS]);
    let folder = music.to_str().unwrap();
    let dying = Server::start(&data_dir, &[("RITORNEL_TEST_FAIL_AT_POSITION_MS", "4000")]);
    assert_eq!(ritornelctl(&data_dir, &["library", "create"]).0, Some(0));
    assert_eq!(ritornelctl(&data_dir, &["scan", folder]).0, Some(0));
    assert_eq!(ritornelctl(&data_dir, &["play", folder]).0, Some(0));
    assert_eq!(dying.ended(), ABORTED);

    let server = Server::start(&data_dir, &[("RITORNEL_TEST_NULL_OUTPUT_SPEED", "20")]);
    let now = json(&data_dir, &["now"]);
    assert_eq!(
        (now["status"].as_str(), now["index"].as_u64()),
        (Some("paused"), Some(1))
    );
    // Each listen reaches history once: the first whole, the second closed
    // as a crash with what was saved as it began, beginning where the first
    // ended.
    let recent = until(&data_dir, &["history"], |recent| recent["undelivered"] == 0);
    let [second, first] = recent["plays"].as_array().unwrap().as_slice() else {
        panic!("two plays: {recent}");
    };
    let ([first], [second]) = (
        first["listens"].as_array().unwrap().as_slice(),
        second["listens"].as_array().unwrap().as_slice(),
    ) else {
        panic!("a listen each: {recent}");
    };
    assert_eq!(
        (first["endReason"].as_str(), first["msPlayed"].as_u64()),
        (Some("finished"), Some(3_000))
    );
    assert_eq!(second["endReason"], "crash");
    assert_eq!(second["startReason"], "auto_advance");
    let saved = second["msPlayed"].as_u64().unwrap();
    assert!(
        saved + CHECKPOINT_MS >= 4_000,
        "at most five seconds lost: {saved}"
    );
    let gap = second["startedAt"].as_i64().unwrap() - first["endedAt"].as_i64().unwrap();
    assert!(
        gap.abs() <= 1,
        "the second began {gap} ms after the first ended"
    );
    server.stop();
}
