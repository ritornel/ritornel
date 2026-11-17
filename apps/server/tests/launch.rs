//! The desktop's launcher, `ritornel_client::launch`, with the real
//! `ritornel-server`: it starts a server only when none is running, owns
//! only the one it started, loses a race to start one gracefully, and stops
//! its own cleanly. See "Starting, finding and stopping" in
//! docs/design/server.md.
//!
//! Every server here plays through the null output, makes no sound, stays
//! out of the system's media session, and listens on a port the system
//! picks.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use ritornel_client::Client;
use ritornel_client::contract::methods::{PlaybackGet, PlaybackPlay, SourcesTracks};
use ritornel_client::contract::{
    CONTRACT_VERSION, ClientKind, Event, HelloParams, NoParams, PlaySource, PlaybackPlayParams,
    PlaybackStatus, SourcesTracksParams, Topic,
};
use ritornel_client::launch::{Launcher, Program, STOP_WAIT, Stopped};

const WAIT: Duration = Duration::from_secs(60);

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "ritornel-launch-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path.canonicalize().unwrap())
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

/// The server as the app would start it, but quiet and on any port.
fn program() -> Program {
    let mut program = Program::new(env!("CARGO_BIN_EXE_ritornel-server"));
    program.args = ["--listen", "127.0.0.1:0", "--output", "null"]
        .map(OsString::from)
        .to_vec();
    program.env = vec![("RITORNEL_TEST_NO_MEDIA_SESSION".into(), "1".into())];
    program
}

fn launcher(data_dir: &Path) -> Launcher {
    Launcher::new(data_dir.to_owned(), Some(program()))
}

async fn hello(client: &Client) {
    client
        .hello(&HelloParams {
            name: "launch-test".to_owned(),
            kind: ClientKind::Desktop,
            version: "1".to_owned(),
            contract_version: CONTRACT_VERSION,
        })
        .await
        .unwrap();
}

fn assert_stopped_cleanly(stopped: Option<Stopped>, data_dir: &Path) {
    assert!(
        matches!(&stopped, Some(Stopped::Exited(status)) if status.success()),
        "{stopped:?}"
    );
    assert!(!data_dir.join("server.json").exists());
    // Both write-ahead logs were emptied, and SQLite removes an empty one.
    assert!(!data_dir.join("server.db-wal").exists());
    assert!(!data_dir.join("library.db-wal").exists());
}

#[tokio::test]
async fn the_first_launch_starts_and_owns_a_server_and_the_next_attaches_to_it() {
    tokio::time::timeout(WAIT, async {
        let scratch = Scratch::new();
        let data_dir = scratch.data_dir();
        let first = launcher(&data_dir);
        let found = first.find_or_start().await.unwrap();
        let pid = first.owned_pid().unwrap();
        hello(&found.connect().await.unwrap()).await;

        // Another app on the same directory uses that server and owns none.
        let second = launcher(&data_dir);
        let attached = second.find_or_start().await.unwrap();
        assert_eq!(attached.descriptor.server_id, found.descriptor.server_id);
        assert_eq!(second.owned_pid(), None);
        assert!(second.stop_owned(STOP_WAIT).is_none());
        // Finding again starts nothing new.
        first.find_or_start().await.unwrap();
        assert_eq!(first.owned_pid(), Some(pid));

        assert_stopped_cleanly(first.stop_owned(STOP_WAIT), &data_dir);
        assert_eq!(first.owned_pid(), None);
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn two_launches_at_once_end_on_one_server_and_one_owns_it() {
    tokio::time::timeout(WAIT, async {
        let scratch = Scratch::new();
        let data_dir = scratch.data_dir();
        let (one, other) = (launcher(&data_dir), launcher(&data_dir));
        let (a, b) = tokio::join!(one.find_or_start(), other.find_or_start());
        let (a, b) = (a.unwrap(), b.unwrap());
        assert_eq!(a.descriptor.server_id, b.descriptor.server_id);
        let owners: Vec<u32> = [one.owned_pid(), other.owned_pid()]
            .into_iter()
            .flatten()
            .collect();
        assert_eq!(owners.len(), 1, "{owners:?}");
        let stopped = one
            .stop_owned(STOP_WAIT)
            .or_else(|| other.stop_owned(STOP_WAIT));
        assert_stopped_cleanly(stopped, &data_dir);
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn a_server_started_by_hand_is_used_and_never_stopped() {
    let scratch = Scratch::new();
    let data_dir = scratch.data_dir();
    let mut by_hand = Command::new(env!("CARGO_BIN_EXE_ritornel-server"))
        .arg("serve")
        .arg("--data-dir")
        .arg(&data_dir)
        .args(["--listen", "127.0.0.1:0", "--output", "none"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut ready = String::new();
    BufReader::new(by_hand.stdout.take().unwrap())
        .read_line(&mut ready)
        .unwrap();
    assert!(ready.starts_with("ready "), "{ready:?}");

    let launched = tokio::time::timeout(WAIT, async {
        let launcher = launcher(&data_dir);
        let found = launcher.find_or_start().await.unwrap();
        assert_eq!(launcher.owned_pid(), None);
        assert!(launcher.stop_owned(STOP_WAIT).is_none());
        drop(launcher);
        // Still there, for the app that comes next.
        let again = ritornel_client::local::find(&data_dir).await.unwrap();
        assert_eq!(again.descriptor.server_id, found.descriptor.server_id);
    })
    .await;
    let _ = Command::new("kill").arg(by_hand.id().to_string()).status();
    assert!(by_hand.wait().unwrap().success());
    launched.unwrap();
}

/// One track of forty seconds of tone, as 16-bit WAV at 8 kHz.
fn long_track(folder: &Path) {
    let (rate, seconds): (u32, u32) = (8000, 40);
    let frames = rate * seconds;
    let data = frames * 2;
    let mut bytes = Vec::with_capacity(44 + data as usize);
    for part in [
        &b"RIFF"[..],
        &(36 + data).to_le_bytes(),
        b"WAVEfmt ",
        &16_u32.to_le_bytes(),
        &1_u16.to_le_bytes(),
        &1_u16.to_le_bytes(),
        &rate.to_le_bytes(),
        &(rate * 2).to_le_bytes(),
        &2_u16.to_le_bytes(),
        &16_u16.to_le_bytes(),
        b"data",
        &data.to_le_bytes(),
    ] {
        bytes.extend_from_slice(part);
    }
    for frame in 0..frames {
        let t = f64::from(frame) / f64::from(rate);
        let sample = (0.25 * (2.0 * std::f64::consts::PI * 440.0 * t).sin() * 32767.0) as i16;
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    std::fs::create_dir_all(folder).unwrap();
    std::fs::File::create(folder.join("01 Tone.wav"))
        .unwrap()
        .write_all(&bytes)
        .unwrap();
}

#[tokio::test]
async fn quitting_pauses_what_plays_and_the_next_start_is_where_it_stopped() {
    tokio::time::timeout(WAIT, async {
        let scratch = Scratch::new();
        let data_dir = scratch.data_dir();
        let folder = scratch.0.join("music/Invented");
        long_track(&folder);

        let launcher = launcher(&data_dir);
        let mut client = launcher
            .find_or_start()
            .await
            .unwrap()
            .connect()
            .await
            .unwrap();
        hello(&client).await;
        let key = || ritornel_client::idempotency_key().unwrap();
        client.create_library(key()).await.unwrap();
        let source = client
            .add_source(key(), "music", scratch.0.join("music").to_str().unwrap())
            .await
            .unwrap()
            .source;
        client.subscribe(&[Topic::Jobs]).await.unwrap();
        let job = client
            .start_scan(key(), source.source_id)
            .await
            .unwrap()
            .job;
        loop {
            if let Event::Jobs(event) = client.next_event().await.unwrap()
                && event.job.job_id == job.job_id
                && event.job.state.ended()
            {
                break;
            }
        }
        let tracks = client
            .call::<SourcesTracks>(&SourcesTracksParams {
                path: folder.to_str().unwrap().to_owned(),
            })
            .await
            .unwrap()
            .tracks;
        client.subscribe(&[Topic::Progress]).await.unwrap();
        client
            .call::<PlaybackPlay>(&PlaybackPlayParams {
                idempotency_key: key(),
                source: PlaySource::Tracks {
                    ids: tracks.iter().map(|track| track.appearance_id).collect(),
                },
                start: None,
                context: None,
                shuffle: None,
            })
            .await
            .unwrap();
        loop {
            if let Event::Progress(progress) = client.next_event().await.unwrap()
                && progress.playing
                && progress.position_ms >= 1_000
            {
                break;
            }
        }

        // The app quits.
        drop(client);
        assert_stopped_cleanly(launcher.stop_owned(STOP_WAIT), &data_dir);

        // The next start finds the track paused where it was.
        let client = launcher
            .find_or_start()
            .await
            .unwrap()
            .connect()
            .await
            .unwrap();
        hello(&client).await;
        let state = client.call::<PlaybackGet>(&NoParams {}).await.unwrap();
        assert_eq!(state.status, PlaybackStatus::Paused);
        assert!(
            (1_000..40_000).contains(&state.position_ms),
            "{}",
            state.position_ms
        );
        drop(client);
        assert_stopped_cleanly(launcher.stop_owned(STOP_WAIT), &data_dir);
    })
    .await
    .unwrap();
}
