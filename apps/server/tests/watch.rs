//! Watching with the system's own watcher (FSEvents on macOS, inotify on
//! Linux) on a temporary folder: a file copied in is scanned in without
//! being asked. The system's latency is not counted on: the test waits for
//! the rescan's job to end, and is cut off only after [`WAIT`].

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ritornel_client::Client;
use ritornel_client::contract::methods::{LibrarySetWatching, LibraryStats};
use ritornel_client::contract::{
    AutoTrigger, CONTRACT_VERSION, ClientKind, Event, HelloParams, Id, NoParams, SessionKind,
    Topic, WatchState, WatchingParams,
};
use ritornel_engine::{OsEntropy, SystemClock};
use ritornel_server::{Bounds, HeldDir, Listen, Run, Server, WatchRun};

const WAIT: Duration = Duration::from_secs(60);

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let path = tempfile::Builder::new()
            .prefix("ritornel-watch-")
            .tempdir()
            .unwrap()
            .keep();
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn key() -> Id {
    ritornel_client::idempotency_key().unwrap()
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/audio")
        .join(name)
}

#[tokio::test]
async fn a_file_copied_into_a_watched_root_is_scanned_in_on_its_own() {
    tokio::time::timeout(WAIT, async {
        let scratch = Scratch::new();
        let run = Run {
            watch: WatchRun {
                quiet: Duration::from_millis(200),
                ..WatchRun::on()
            },
            ..Run::default()
        };
        let server = Server::start_run(
            HeldDir::take(scratch.0.join("data")).unwrap(),
            Listen::on_port(0),
            Arc::new(SystemClock),
            Arc::new(OsEntropy),
            Bounds::default(),
            run,
        )
        .await
        .unwrap();
        let (outgoing, incoming) = server.connect_in_memory(SessionKind::Full);
        let mut client = Client::over_memory(outgoing, incoming);
        client
            .hello(&HelloParams {
                name: "watch-test".to_owned(),
                kind: ClientKind::Cli,
                version: "1".to_owned(),
                contract_version: CONTRACT_VERSION,
            })
            .await
            .unwrap();
        let folder = scratch.0.join("music");
        std::fs::create_dir_all(folder.join("Tin Weather")).unwrap();
        std::fs::copy(fixture("a.flac"), folder.join("Tin Weather/01.flac")).unwrap();
        client.create_library(key()).await.unwrap();
        let source = client
            .add_source(key(), "music", folder.to_str().unwrap())
            .await
            .unwrap()
            .source;
        client.subscribe(&[Topic::Jobs]).await.unwrap();
        let first = client
            .start_scan(key(), source.source_id)
            .await
            .unwrap()
            .job;
        loop {
            if let Event::Jobs(event) = client.next_event().await.unwrap()
                && event.job.job_id == first.job_id
                && event.job.state.ended()
            {
                break;
            }
        }
        // The root is being watched before anything changes in it.
        loop {
            let status = client.status().await.unwrap().watch.unwrap();
            if status.folders.first().map(|folder| folder.state) == Some(WatchState::Watching) {
                assert!(status.on);
                break;
            }
            tokio::task::yield_now().await;
        }

        std::fs::copy(fixture("a.retagged.mp3"), folder.join("Tin Weather/02.mp3")).unwrap();
        let rescan = loop {
            if let Event::Jobs(event) = client.next_event().await.unwrap()
                && event.job.job_id != first.job_id
                && event.job.state.ended()
            {
                break event.job;
            }
        };
        assert_eq!(rescan.scan.unwrap().new, 1);
        let stats = client.call::<LibraryStats>(&NoParams {}).await.unwrap();
        assert_eq!(stats.files, 2);
        // A copy the system reports in parts more than the quiet period
        // apart (Windows does, under load) is rescanned again, so the last
        // automatic rescan is the one waited for or a later one. A job's
        // id begins with the millisecond it was made in, and a second
        // rescan is made only once the first has ended.
        let shown = client.status().await.unwrap().watch.unwrap();
        let last = shown.folders[0].last_auto.as_ref().unwrap();
        assert_eq!(last.trigger, AutoTrigger::Change);
        assert!(
            last.job_id >= Some(rescan.job_id),
            "{:?} is older than {:?}",
            last.job_id,
            rescan.job_id
        );

        // Turned off from Settings, the hub says so; turned on, it watches.
        let set = |on: bool| WatchingParams {
            idempotency_key: key(),
            on,
        };
        let off = client
            .call::<LibrarySetWatching>(&set(false))
            .await
            .unwrap();
        assert!(!off.watch.on && !off.watch.enabled);
        assert_eq!(off.watch.off.as_deref(), Some("watching is turned off"));
        let on = client.call::<LibrarySetWatching>(&set(true)).await.unwrap();
        assert!(on.watch.enabled);
        while !client.status().await.unwrap().watch.unwrap().on {
            tokio::task::yield_now().await;
        }
        server.stop().await.unwrap();
    })
    .await
    .unwrap();
}
