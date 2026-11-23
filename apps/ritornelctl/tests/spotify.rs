//! The real `ritornelctl spotify import` against the real
//! `ritornel-server`: an invented archive is read here, its kept records
//! are staged on the server, and the dry run reports what applying them
//! would do, with the library unwritten and no value of a dropped field
//! anywhere in the data directory. See "Spotify streams" in
//! docs/internals/history.md.
//!
//! As in `status.rs`, `ritornel-server` is taken from beside `ritornelctl`,
//! and each wait is bounded.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use serde_json::{Value, json};

/// How long a test waits for a process before it kills it and fails.
const WAIT: Duration = Duration::from_secs(60);

/// A directory that is removed when the test ends.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "ritornelctl-spotify-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn data_dir(&self) -> PathBuf {
        self.0.join("data")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
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

/// Runs `ritornelctl` on a data directory, and returns its exit code, its
/// output and its errors. It is killed if it has not ended within [`WAIT`].
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

/// A running `ritornel-server` with no player, killed if the test ends
/// without stopping it.
struct Server {
    child: Child,
}

impl Server {
    fn start(data_dir: &Path) -> Self {
        let mut child = Command::new(server_binary())
            .arg("serve")
            .arg("--data-dir")
            .arg(data_dir)
            .args(["--listen", "127.0.0.1:0", "--output", "none"])
            .env_remove("RITORNEL_DATA_DIR")
            .env_remove("RITORNEL_LISTEN")
            .env_remove("RITORNEL_BACKUP_DIR")
            .env_remove("SCROBL_API_KEY")
            .env_remove("SCROBL_API_SECRET")
            .env("RITORNEL_TEST_COMPUTER_NAME", "Invented Desk")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = lines(child.stdout.take().unwrap());
        let ready = stdout.recv_timeout(WAIT);
        if !ready
            .as_deref()
            .is_ok_and(|line| line.starts_with("ready "))
        {
            let _ = child.kill();
            let _ = child.wait();
            panic!("no ready line within {WAIT:?}: {ready:?}");
        }
        Self { child }
    }

    fn stop(mut self, data_dir: &Path) {
        assert_eq!(ritornelctl(data_dir, &["server", "stop"]).0, Some(0));
        let _ = self.child.wait();
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// An invented row, every field Spotify gives filled in, each that is
/// never kept with a marked value.
fn row(ts: &str, ms: i64, title: &str) -> Value {
    json!({
        "ts": ts,
        "username": "MARKED_USERNAME",
        "platform": "iOS 12.1 (MARKED_DEVICE)",
        "ms_played": ms,
        "conn_country": "MARKED_COUNTRY",
        "ip_addr": "MARKED_IP",
        "ip_addr_decrypted": "MARKED_IP_DECRYPTED",
        "user_agent_decrypted": "MARKED_AGENT",
        "master_metadata_track_name": title,
        "master_metadata_album_artist_name": "Calla Brandt",
        "master_metadata_album_album_name": "Glasshouse",
        "spotify_track_uri": format!("spotify:track:{}", title.replace(' ', "")),
        "episode_name": null,
        "episode_show_name": null,
        "spotify_episode_uri": null,
        "audiobook_title": null,
        "audiobook_uri": null,
        "audiobook_chapter_uri": null,
        "audiobook_chapter_title": null,
        "reason_start": "trackdone",
        "reason_end": "trackdone",
        "shuffle": false,
        "skipped": false,
        "offline": false,
        "offline_timestamp": 0,
        "incognito_mode": false,
        "MARKED_FIELD": "MARKED_VALUE",
    })
}

/// An invented archive: an evening of three streams, one reported twice.
fn archive(path: &Path) {
    let rows = json!([
        row("2019-03-15T21:14:09Z", 221_000, "Low Tide"),
        row("2019-03-15T21:14:09Z", 221_000, "Low Tide"),
        row("2019-03-15T21:14:21Z", 12_000, "Paper Lanterns"),
        row("2019-03-15T21:18:30Z", 249_000, "Night Ferry"),
    ]);
    let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
    zip.start_file(
        "Spotify Extended Streaming History/Streaming_History_Audio_2019.json",
        zip::write::SimpleFileOptions::default(),
    )
    .unwrap();
    zip.write_all(&serde_json::to_vec(&rows).unwrap()).unwrap();
    zip.finish().unwrap();
}

/// Every file under a folder, and whether any holds `mark`.
fn holds(folder: &Path, mark: &[u8]) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut folders = vec![folder.to_owned()];
    while let Some(folder) = folders.pop() {
        for entry in std::fs::read_dir(&folder).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                folders.push(path);
            } else if std::fs::read(&path)
                .unwrap()
                .windows(mark.len())
                .any(|window| window == mark)
            {
                found.push(path);
            }
        }
    }
    found
}

#[test]
fn an_import_is_staged_and_dry_run_and_writes_nothing_to_the_library() {
    let scratch = Scratch::new();
    let data_dir = scratch.data_dir();
    let server = Server::start(&data_dir);
    assert_eq!(ritornelctl(&data_dir, &["library", "create"]).0, Some(0));
    let revision = |data_dir: &Path| {
        let (code, out, _) = ritornelctl(data_dir, &["status", "--json"]);
        assert_eq!(code, Some(0));
        serde_json::from_str::<Value>(&out).unwrap()["library"]["revision"].clone()
    };
    let before = revision(&data_dir);
    let archive_path = scratch.0.join("my_spotify_data.zip");
    archive(&archive_path);

    let (code, out, err) = ritornelctl(
        &data_dir,
        &["spotify", "import", archive_path.to_str().unwrap()],
    );
    assert_eq!(code, Some(0), "{out}{err}");
    for line in [
        "Streams of music  4\n",
        "Dry run\n",
        "Listens           3 in 4 streams\n",
        "A, at the start   0 joined, 0 second reports\n",
    ] {
        assert!(out.contains(line), "{line}{out}");
    }
    assert!(!out.replace("MARKED_FIELD", "").contains("MARKED"), "{out}");

    let (code, out, _) = ritornelctl(
        &data_dir,
        &[
            "spotify",
            "import",
            archive_path.to_str().unwrap(),
            "--json",
        ],
    );
    assert_eq!(code, Some(0));
    let answer: Value = serde_json::from_str(&out).unwrap();
    let report = &answer["run"]["report"];
    assert_eq!(report["streams"], 4);
    assert_eq!(report["events"], 3);
    // Low Tide and Night Ferry each ran to their end and count; Paper
    // Lanterns played twelve seconds and does not.
    let year = &report["years"][0];
    assert_eq!(
        (year["year"].clone(), year["streams"].clone()),
        (json!(2019), json!(3))
    );
    assert_eq!(year["aloneCounted"], 2);
    assert_eq!(year["aloneNotCounted"], 1);
    assert_eq!(answer["job"]["state"], "succeeded");
    assert_eq!(answer["job"]["target"]["type"], "import_run");

    // Nothing was written to the library, and no value of a field that is
    // never kept is anywhere in the data directory: not in the staged run,
    // a database, a log or a job.
    assert_eq!(revision(&data_dir), before);
    let marked = holds(&data_dir, b"MARKED");
    assert!(marked.is_empty(), "{marked:?}");
    assert!(
        std::fs::read_dir(data_dir.join("imports"))
            .unwrap()
            .next()
            .is_some()
    );

    // A staged run does not outlive the server.
    server.stop(&data_dir);
    let server = Server::start(&data_dir);
    assert!(!data_dir.join("imports").exists());
    server.stop(&data_dir);
}
