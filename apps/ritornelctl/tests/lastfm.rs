//! The real `ritornelctl` and `ritornel-server` against a fake Last.fm on a
//! loopback socket, with an invented account: signing in, the zones a play
//! is dated by, the import of a history killed part way and carried on, a
//! second sync that writes nothing, the loved tracks, the report, and a
//! play scrobbled once whose echo joins it when it comes back. No request
//! leaves this machine, and no key, secret or session key reaches a log.
//!
//! As in `play.rs`, `ritornel-server` is taken from beside `ritornelctl`,
//! and nothing here sleeps but to wait for something, bounded.

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

use ritornel_lastfm::fake::{API_KEY, API_SECRET, FakeLastfm, FakeScrobble, SESSION_KEY, TOKEN};
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

/// 2009-06-30 23:30 UTC: 1 July in Helsinki, 30 June in New York.
const MIDSUMMER_2009: u64 = 1_246_404_600;

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let path = tempfile::Builder::new()
            .prefix("ritornelctl-lastfm-")
            .tempdir()
            .unwrap()
            .keep();
        Self(path)
    }

    fn data_dir(&self) -> PathBuf {
        self.0.join("data")
    }

    /// A folder of two invented tracks by an invented artist: forty
    /// seconds of tone each, as 16-bit AIFF at 8 kHz with ID3 tags.
    fn music(&self) -> PathBuf {
        let folder = self.0.canonicalize().unwrap().join("music/Invented Album");
        std::fs::create_dir_all(&folder).unwrap();
        for (name, title, number, hz) in [
            ("01 First.aiff", "First Light", "1", 440.0),
            ("02 Second.aiff", "Second Wind", "2", 660.0),
        ] {
            write_aiff(&folder.join(name), title, number, hz);
        }
        folder
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// One ID3v2.3 text frame: its id, its size, no flags, and the text in
/// ISO-8859-1.
fn frame(id: &[u8; 4], text: &str) -> Vec<u8> {
    let mut frame = id.to_vec();
    frame.extend_from_slice(&u32::try_from(text.len() + 1).unwrap().to_be_bytes());
    frame.extend_from_slice(&[0, 0, 0]);
    frame.extend_from_slice(text.as_bytes());
    frame
}

/// An ID3v2.3 tag of these frames.
fn id3(frames: &[Vec<u8>]) -> Vec<u8> {
    let body: Vec<u8> = frames.concat();
    let size = u32::try_from(body.len()).unwrap();
    let mut tag = b"ID3\x03\x00\x00".to_vec();
    // The size is synchsafe: seven bits a byte.
    tag.extend_from_slice(&[
        u8::try_from((size >> 21) & 0x7f).unwrap(),
        u8::try_from((size >> 14) & 0x7f).unwrap(),
        u8::try_from((size >> 7) & 0x7f).unwrap(),
        u8::try_from(size & 0x7f).unwrap(),
    ]);
    tag.extend_from_slice(&body);
    tag
}

/// An AIFF file of forty seconds of tone, 16-bit at 8 kHz, with an `ID3 `
/// chunk naming the track, its artist, album and place. AIFF because
/// symphonia reads an ID3 tag there and not in a WAV file.
fn write_aiff(path: &Path, title: &str, number: &str, hz: f64) {
    let rate: u32 = 8000;
    let frames = rate * TRACK_SECONDS;
    let data = frames * 2;
    let mut tag = id3(&[
        frame(b"TIT2", title),
        frame(b"TPE1", "Invented Artist"),
        frame(b"TALB", "Invented Album"),
        frame(b"TRCK", number),
    ]);
    if tag.len() % 2 == 1 {
        tag.push(0);
    }
    let tag_len = u32::try_from(tag.len()).unwrap();
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"FORM");
    bytes.extend_from_slice(&(4 + 26 + 8 + tag_len + 16 + data).to_be_bytes());
    bytes.extend_from_slice(b"AIFFCOMM");
    bytes.extend_from_slice(&18_u32.to_be_bytes());
    bytes.extend_from_slice(&1_u16.to_be_bytes());
    bytes.extend_from_slice(&frames.to_be_bytes());
    bytes.extend_from_slice(&16_u16.to_be_bytes());
    // 8000 as an 80-bit extended float: 1.953125 times two to the 12th.
    bytes.extend_from_slice(&[0x40, 0x0b, 0xfa, 0, 0, 0, 0, 0, 0, 0]);
    bytes.extend_from_slice(b"ID3 ");
    bytes.extend_from_slice(&tag_len.to_be_bytes());
    bytes.extend_from_slice(&tag);
    bytes.extend_from_slice(b"SSND");
    bytes.extend_from_slice(&(8 + data).to_be_bytes());
    bytes.extend_from_slice(&[0; 8]);
    for frame in 0..frames {
        let t = f64::from(frame) / f64::from(rate);
        let sample = (0.25 * (2.0 * std::f64::consts::PI * hz * t).sin() * 32767.0) as i16;
        bytes.extend_from_slice(&sample.to_be_bytes());
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

/// Asks again and again until `done` says so, for at most [`WAIT`].
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
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// A `ritornel-server` on the null output, sending everything for Last.fm
/// to the fake.
struct Server {
    child: Child,
    stdout: Receiver<String>,
}

fn spawn(data_dir: &Path, fake: &FakeLastfm, more: &[(&str, &str)]) -> (Child, Receiver<String>) {
    let binary = Path::new(env!("CARGO_BIN_EXE_ritornelctl"))
        .with_file_name(format!("ritornel-server{}", std::env::consts::EXE_SUFFIX));
    assert!(
        binary.exists(),
        "there is no ritornel-server beside ritornelctl: build the workspace"
    );
    let url = fake.base_url();
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
        .env("SCROBL_API_KEY", API_KEY)
        .env("SCROBL_API_SECRET", API_SECRET)
        .env("RITORNEL_TEST_LASTFM_URL", &url)
        .env("RITORNEL_TEST_LASTFM_CHUNK", "7")
        .env("RITORNEL_TEST_LASTFM_PAGE", "3")
        .env("RITORNEL_TEST_NULL_OUTPUT_SPEED", "50")
        .envs(more.iter().copied())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let stdout = lines(child.stdout.take().unwrap());
    (child, stdout)
}

impl Server {
    fn start(data_dir: &Path, fake: &FakeLastfm, more: &[(&str, &str)]) -> Self {
        let (mut child, stdout) = spawn(data_dir, fake, more);
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

/// An invented history: plays of the invented album and of an artist the
/// library does not have, every hour back from yesterday, and one at
/// midsummer 2009.
fn history(now: u64) -> Vec<FakeScrobble> {
    let mut scrobbles: Vec<FakeScrobble> = (0..40)
        .map(|i| {
            let at = now - 86_400 - i * 3_600;
            match i % 4 {
                0 => FakeScrobble::new(at, "Invented Artist", "Invented Album", "First Light"),
                1 => FakeScrobble::new(at, "invented artist", "Invented Album", "SECOND WIND"),
                2 => FakeScrobble::new(at, "Invented Artist", "", "Second Wind (Live)"),
                _ => FakeScrobble::new(at, "Nobody Here", "Elsewhere", "Not Owned"),
            }
        })
        .collect();
    scrobbles.push(FakeScrobble::new(
        MIDSUMMER_2009,
        "Invented Artist",
        "Invented Album",
        "First Light",
    ));
    scrobbles
}

fn now_s() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// Every line of every log file the server wrote.
fn logs(data_dir: &Path) -> String {
    let mut text = String::new();
    for entry in std::fs::read_dir(data_dir.join("logs")).unwrap() {
        text.push_str(&std::fs::read_to_string(entry.unwrap().path()).unwrap());
    }
    text
}

/// What `history artist` or `history album` prints for `name` is the
/// sentence its page shows, `history.artist` or `history.album` as the
/// desktop reads it, and both agree with `plays`, every play as
/// `ritornelctl history` lists it: their count, their first and last days
/// and their years, of the plays that count and are bound to the invented
/// album, as only those reach a page. Every invented play here is of that
/// album and that artist, so the two say the same.
fn page_agrees_with_the_plays(data_dir: &Path, what: &str, name: &str, plays: &Value) {
    let page = json(data_dir, &["history", what, name]);
    let (code, text, stderr) = ritornelctl(data_dir, &["history", what, name]);
    assert_eq!(code, Some(0), "{stderr}");
    let said: String = page["sentence"]
        .as_array()
        .unwrap()
        .iter()
        .map(|phrase| phrase["text"].as_str().unwrap())
        .collect();
    assert_eq!(text.lines().next(), Some(said.as_str()), "{text}");

    // The plays a page counts, from the list of every play.
    let mine: Vec<&Value> = plays["plays"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|play| {
            play["counts"] == true
                && play["excluded"] == false
                && play["track"]["recordingId"].is_string()
                && play["track"]["artist"] == "Invented Artist"
        })
        .collect();
    let mut days: Vec<&str> = mine
        .iter()
        .map(|play| play["localDate"].as_str().unwrap())
        .collect();
    days.sort_unstable();
    assert_eq!(page["plays"], mine.len(), "{page}");
    assert_eq!(
        mine.len(),
        23,
        "the album's two songs ten times each, the play of 2009 and the two heard here"
    );
    assert_eq!(page["firstPlayed"], days[0]);
    assert_eq!(page["lastPlayed"], *days.last().unwrap());
    let mut years = std::collections::BTreeMap::<String, u64>::new();
    for day in &days {
        *years.entry(day[..4].to_owned()).or_default() += 1;
    }
    let paged: std::collections::BTreeMap<String, u64> = page["years"]
        .as_array()
        .unwrap()
        .iter()
        .map(|year| (year["year"].to_string(), year["plays"].as_u64().unwrap()))
        .collect();
    assert_eq!(paged, years);
    for (year, count) in &years {
        assert!(
            text.lines()
                .any(|line| line.starts_with(year.as_str()) && line.contains(&format!(" {count} "))),
            "{year}: {count} in {text}"
        );
    }

    // The play of 2009 began the history, so the album was there from
    // your first weeks; and the plays of the last day are in the last year.
    assert!(
        said.starts_with("With you since your first weeks, July 2009"),
        "{said}"
    );
    let recent = mine
        .iter()
        .filter(|play| play["at"].as_i64().unwrap() > (now_s() as i64 - 365 * 86_400) * 1000)
        .count();
    assert_eq!(page["lastYear"], recent);
    if said.contains("in the last year") {
        assert!(
            said.ends_with(&format!(" {recent} plays in the last year.")),
            "{said}"
        );
    }
}

#[test]
fn a_last_fm_history_is_imported_through_a_kill_and_a_play_is_scrobbled_once() {
    let scratch = Scratch::new();
    let (data_dir, music) = (scratch.data_dir(), scratch.music());
    let folder = music.to_str().unwrap();
    let fake = FakeLastfm::start("someone", history(now_s()));
    fake.set_loved(vec![(
        "Invented Artist",
        "First Light",
        now_s() - 7 * 86_400,
    )]);
    fake.approve_after(1);

    // The server dies straight after the library commits the second batch
    // of the sync.
    let dying = Server::start(
        &data_dir,
        &fake,
        &[("RITORNEL_TEST_FAIL_AFTER_IMPORT_BATCH", "2")],
    );
    assert_eq!(ritornelctl(&data_dir, &["library", "create"]).0, Some(0));
    assert_eq!(ritornelctl(&data_dir, &["scan", folder]).0, Some(0));

    // Signing in prints the address to approve, then the account.
    let status = json(&data_dir, &["lastfm", "status"]);
    assert_eq!(
        (
            status["available"].as_bool(),
            status["configured"].as_bool()
        ),
        (Some(true), Some(true))
    );
    assert_eq!(status.get("account"), None);
    let (code, stdout, stderr) = ritornelctl(&data_dir, &["lastfm", "signin"]);
    assert_eq!((code, stderr.as_str()), (Some(0), ""), "{stdout}");
    assert!(stdout.contains(TOKEN), "the address to approve: {stdout}");
    assert!(
        stdout.ends_with("Signed in to Last.fm as someone.\n"),
        "{stdout}"
    );
    let (_, status, _) = ritornelctl(&data_dir, &["lastfm", "status"]);
    assert!(status.contains("Account           someone\n"), "{status}");

    // A sync waits for the zones.
    let (code, _, stderr) = ritornelctl(&data_dir, &["lastfm", "sync"]);
    assert_eq!(code, Some(1));
    assert!(stderr.contains("history zone add"), "{stderr}");
    for zone in [
        &["history", "zone", "add", "America/New_York"][..],
        &[
            "history",
            "zone",
            "add",
            "Europe/Helsinki",
            "--from",
            "2009-06-01",
            "--until",
            "2009-08-01",
        ],
    ] {
        assert_eq!(ritornelctl(&data_dir, zone).0, Some(0), "{zone:?}");
    }
    let zones = json(&data_dir, &["history", "zones"]);
    assert_eq!(zones["periods"][1]["from"], "2009-06-01");

    // Started, it dies part way.
    assert_eq!(
        ritornelctl(&data_dir, &["lastfm", "sync", "--no-wait"]).0,
        Some(0)
    );
    assert_eq!(dying.ended(), ABORTED);

    // The next run carries the sync on from its cursor, by itself.
    let server = Server::start(&data_dir, &fake, &[]);
    let jobs = until(&data_dir, &["jobs"], |jobs| {
        jobs["jobs"][0]["state"] == "succeeded"
    });
    let summary = &jobs["jobs"][0]["sync"];
    assert_eq!(summary["sourceTotal"], 41);
    assert_eq!(summary["stored"], 41, "{summary}");
    assert!(summary["new"].as_u64().unwrap() < 41, "{summary}");
    assert_eq!(summary["loved"], 1);

    // A second sync with nothing new writes nothing.
    let revision = json(&data_dir, &["status"])["library"]["revision"].clone();
    let again = json(&data_dir, &["lastfm", "sync"]);
    assert_eq!(again["state"], "succeeded");
    assert_eq!(again["sync"]["new"], 0);
    assert_eq!(
        json(&data_dir, &["status"])["library"]["revision"],
        revision
    );

    // The play of 2009 falls on 1 July, in Helsinki.
    let day = json(&data_dir, &["history", "on-this-day", "--day", "07-01"]);
    let play = &day["plays"][0];
    assert_eq!(play["localDate"], "2009-07-01");
    assert_eq!(play["zone"], "Europe/Helsinki");
    assert_eq!(play["clockBasis"], "source_utc");
    assert_eq!(play["track"]["title"], "First Light");

    // The report: the album's two songs match, whatever their case; the
    // live one is doubtful; the stranger is not in the library.
    let report = json(&data_dir, &["lastfm", "report"]);
    assert_eq!(
        (report["plays"].as_u64(), report["matchedPlays"].as_u64()),
        (Some(41), Some(21))
    );
    assert_eq!(report["doubtful"][0]["title"], "Second Wind (Live)");
    assert_eq!(
        report["doubtful"][0]["candidates"][0]["title"],
        "Second Wind"
    );
    assert_eq!(report["absentPlays"], 10);
    let (_, text, _) = ritornelctl(&data_dir, &["lastfm", "report"]);
    assert!(
        text.contains("Plays matched     21 of 41 (51.2%)"),
        "{text}"
    );

    // The loved track shows as loved.
    let track = play["track"]["appearanceId"].as_str().unwrap();
    let (code, shown, _) = ritornelctl(&data_dir, &["show", track]);
    assert_eq!(code, Some(0));
    assert!(
        shown.contains("Loved             by lastfm:someone"),
        "{shown}"
    );

    // Scrobbling on, the album played: each play that counts is sent once,
    // and its echo joins it when the next sync brings it back.
    json(&data_dir, &["lastfm", "scrobbling", "on"]);
    assert_eq!(
        ritornelctl(&data_dir, &["play", folder, "--follow"]).0,
        Some(0)
    );
    let deadline = Instant::now() + WAIT;
    while fake.scrobbled().len() < 2 {
        assert!(Instant::now() < deadline, "{:?}", fake.scrobbled());
        std::thread::sleep(Duration::from_millis(100));
    }
    let sent = fake.scrobbled();
    assert_eq!(
        sent.iter().map(|s| s.title.as_str()).collect::<Vec<_>>(),
        ["First Light", "Second Wind"]
    );
    assert_eq!(sent[0].artist, "Invented Artist");
    assert_eq!(sent[0].album, "Invented Album");
    let status = until(&data_dir, &["lastfm", "status"], |status| {
        status["deliveries"][0]["state"] == "accepted"
    });
    assert_eq!(status["deliveries"][0]["count"], 2);

    let synced = json(&data_dir, &["lastfm", "sync"]);
    assert_eq!(synced["sync"]["new"], 2);
    let report = json(&data_dir, &["lastfm", "report"]);
    assert_eq!(report["echoes"], 2);
    assert_eq!(report["plays"], 41, "the echoes add no play");
    let recent = json(&data_dir, &["history", "--limit", "2"]);
    for play in recent["plays"].as_array().unwrap() {
        assert_eq!(play["clockBasis"], "hub_age", "{play}");
    }
    // Sent once: nothing more goes after the echoes are back.
    assert_eq!(fake.scrobbled().len(), 2);

    // What the artist's page and the album's say agrees with the plays.
    let plays = json(&data_dir, &["history", "--limit", "500"]);
    for (what, name) in [("artist", "invented artist"), ("album", "Invented Album")] {
        page_agrees_with_the_plays(&data_dir, what, name, &plays);
    }
    let (code, _, stderr) = ritornelctl(&data_dir, &["history", "artist", "Nobody Here"]);
    assert_eq!(code, Some(1), "a stranger has no page: {stderr}");
    server.stop();

    // Nothing the server keeps secret reached a log.
    let written = logs(&data_dir);
    for secret in [API_KEY, API_SECRET, SESSION_KEY, TOKEN] {
        assert!(!written.contains(secret), "a secret in the log");
    }
}

#[test]
fn the_hub_imports_on_its_own_when_it_starts_and_a_failure_waits_for_the_next_trigger() {
    let scratch = Scratch::new();
    let data_dir = scratch.data_dir();
    let fake = FakeLastfm::start("someone", history(now_s()));

    // Set up, and never imported: the next is due now.
    let server = Server::start(&data_dir, &fake, &[]);
    assert_eq!(ritornelctl(&data_dir, &["library", "create"]).0, Some(0));
    let (code, stdout, stderr) = ritornelctl(&data_dir, &["lastfm", "signin", "--json"]);
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(
        serde_json::from_str::<Value>(&stdout).unwrap()["account"],
        "someone"
    );
    assert!(stderr.contains(TOKEN), "{stderr}");
    let (_, text, _) = ritornelctl(&data_dir, &["lastfm", "status"]);
    assert!(
        text.contains("Next import       only by hand: no zones"),
        "{text}"
    );
    let zone = ["history", "zone", "add", "America/New_York"];
    assert_eq!(ritornelctl(&data_dir, &zone).0, Some(0));
    let status = json(&data_dir, &["lastfm", "status"]);
    assert!(status["import"]["next"].is_i64(), "{status}");
    server.stop();

    // Last.fm cannot be reached: the import the hub starts with fails,
    // once, and the next waits for a trigger.
    let dead = {
        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}/2.0/", closed.local_addr().unwrap())
    };
    let server = Server::start(
        &data_dir,
        &fake,
        &[("RITORNEL_TEST_LASTFM_URL", dead.as_str())],
    );
    until(&data_dir, &["jobs"], |jobs| {
        jobs["jobs"][0]["state"] == "failed"
    });
    let status = json(&data_dir, &["lastfm", "status"]);
    let import = &status["import"];
    let ended = import["last"]["ended"].as_i64().unwrap();
    assert_eq!(import["last"]["state"], "failed", "{status}");
    assert!(import["last"]["failure"].is_string(), "{status}");
    assert_eq!(import["next"], ended + 6 * 3_600_000, "{status}");
    assert_eq!(import["onOpenAfter"], ended + 3_600_000, "{status}");
    let (_, text, _) = ritornelctl(&data_dir, &["lastfm", "status"]);
    assert!(text.contains(", failed: Last.fm: "), "{text}");
    assert!(text.contains(", or when the app opens after "), "{text}");
    let jobs = json(&data_dir, &["jobs"]);
    assert_eq!(
        jobs["jobs"][0]["state"], "failed",
        "nothing is tried again before a trigger: {jobs}"
    );
    server.stop();

    // Reachable again, the hub imports the history when it starts, with
    // no command.
    let server = Server::start(&data_dir, &fake, &[]);
    let jobs = until(&data_dir, &["jobs"], |jobs| {
        jobs["jobs"][0]["state"] == "succeeded"
    });
    assert_eq!(jobs["jobs"][0]["sync"]["stored"], 41, "{jobs}");
    let status = json(&data_dir, &["lastfm", "status"]);
    assert_eq!(status["imported"], 41);
    assert_eq!(status["import"]["last"]["state"], "succeeded", "{status}");
    let (_, text, _) = ritornelctl(&data_dir, &["lastfm", "status"]);
    assert!(text.contains(" UTC, succeeded\n"), "{text}");
    server.stop();

    let written = logs(&data_dir);
    assert!(written.contains("a Last.fm import starts on its own"));
    assert!(written.contains("a Last.fm sync failed: the next import tries again"));
    for secret in [API_KEY, API_SECRET, SESSION_KEY, TOKEN] {
        assert!(!written.contains(secret), "a secret in the log");
    }
}

/// Waits until the fake has been told of `count` tracks playing now, for
/// at most [`WAIT`], and returns what each sent.
fn shown(fake: &FakeLastfm, count: usize) -> Vec<std::collections::HashMap<String, String>> {
    let deadline = Instant::now() + WAIT;
    loop {
        let sent = fake.now_playing();
        if sent.len() >= count {
            return sent;
        }
        assert!(Instant::now() < deadline, "{sent:?}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Whether `ritornelctl now` has the playing track at `ms` or past it.
fn past(ms: u64) -> impl Fn(&Value) -> bool {
    move |now| now["positionMs"].as_u64().is_some_and(|at| at >= ms)
}

#[test]
fn what_begins_to_play_is_shown_as_playing_now_only_while_scrobbling_is_on() {
    let scratch = Scratch::new();
    let (data_dir, music) = (scratch.data_dir(), scratch.music());
    let folder = music.to_str().unwrap();
    let fake = FakeLastfm::start("someone", Vec::new());
    // At real time, so that a track is heard long enough to be shown.
    let server = Server::start(
        &data_dir,
        &fake,
        &[("RITORNEL_TEST_NULL_OUTPUT_SPEED", "1")],
    );
    assert_eq!(ritornelctl(&data_dir, &["library", "create"]).0, Some(0));
    assert_eq!(ritornelctl(&data_dir, &["scan", folder]).0, Some(0));
    assert_eq!(ritornelctl(&data_dir, &["lastfm", "signin"]).0, Some(0));

    // Scrobbling off: a track heard for seconds is shown nowhere.
    assert_eq!(ritornelctl(&data_dir, &["play", folder]).0, Some(0));
    until(&data_dir, &["now"], past(2_500));
    assert!(fake.now_playing().is_empty(), "{:?}", fake.now_playing());

    // Turned on, the next track to begin is shown once, under the names
    // its scrobble would carry.
    json(&data_dir, &["lastfm", "scrobbling", "on"]);
    json(&data_dir, &["next"]);
    let sent = shown(&fake, 1);
    let field = |name: &str| sent[0].get(name).map(String::as_str);
    assert_eq!(field("artist"), Some("Invented Artist"));
    assert_eq!(field("track"), Some("Second Wind"));
    assert_eq!(field("album"), Some("Invented Album"));
    assert_eq!(
        field("albumArtist"),
        None,
        "the album's artist is the track's"
    );
    assert_eq!(field("trackNumber"), Some("2"));
    assert_eq!(field("duration"), Some(TRACK_SECONDS.to_string().as_str()));
    assert_eq!(field("sk"), Some(SESSION_KEY));
    until(&data_dir, &["now"], past(3_000));
    assert_eq!(fake.now_playing().len(), 1, "shown once while it plays");

    // A pause shows nothing; playing again shows it again.
    json(&data_dir, &["pause"]);
    std::thread::sleep(Duration::from_millis(1_500));
    assert_eq!(fake.now_playing().len(), 1);
    json(&data_dir, &["resume"]);
    let sent = shown(&fake, 2);
    assert_eq!(
        sent[1].get("track").map(String::as_str),
        Some("Second Wind")
    );
    json(&data_dir, &["stop"]);
    std::thread::sleep(Duration::from_millis(1_500));
    assert_eq!(fake.now_playing().len(), 2);
    assert!(
        fake.scrobbled().is_empty(),
        "nothing played long enough to count"
    );
    server.stop();

    let written = logs(&data_dir);
    for secret in [API_KEY, API_SECRET, SESSION_KEY, TOKEN] {
        assert!(!written.contains(secret), "a secret in the log");
    }
}
