//! Watching with the system's own watcher (FSEvents on macOS, inotify on
//! Linux) on a temporary folder: a file copied in is scanned in without
//! being asked. The system's latency is not counted on: the test waits for
//! the rescan's job to end, and is cut off only after [`WAIT`].

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ritornel_client::Client;
use ritornel_client::contract::methods::{LibrarySetWatching, LibraryStats};
use ritornel_client::contract::{
    AutoTrigger, CONTRACT_VERSION, ClientKind, Event, HelloParams, JobState, NoParams, SessionKind,
    Topic, WatchState, WatchingParams,
};
use ritornel_engine::{OsEntropy, SystemClock};
use ritornel_server::{Bounds, HeldDir, Listen, Run, Server, WatchRun};

use super::support::{Scratch, WAIT, key};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/audio")
        .join(name)
}

#[tokio::test]
async fn a_file_copied_into_a_watched_root_is_scanned_in_on_its_own() {
    let waiting = Cell::new("the server to start");
    tokio::time::timeout(WAIT, async {
        let scratch = Scratch::new("ritornel-watch-");
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
        waiting.set("the initial scan to end");
        loop {
            if let Event::Jobs(event) = client.next_event().await.unwrap()
                && event.job.job_id == first.job_id
                && event.job.state.ended()
            {
                break;
            }
        }
        // The root is being watched before anything changes in it.
        waiting.set("the root to be watched");
        loop {
            let status = client.status().await.unwrap().watch.unwrap();
            if status.folders.first().map(|folder| folder.state) == Some(WatchState::Watching) {
                assert!(status.on);
                break;
            }
            tokio::task::yield_now().await;
        }

        std::fs::copy(fixture("a.retagged.mp3"), folder.join("Tin Weather/02.mp3")).unwrap();
        // An early automatic rescan before the copy is settled or readable
        // can finish with nothing new, or with the file unreadable, before a
        // later rescan picks it up. Wait until the copied file is added and
        // readable in the library.
        waiting.set("an automatic rescan to add the copied file");
        let mut added = 0;
        let rescan = loop {
            if let Event::Jobs(event) = client.next_event().await.unwrap()
                && event.job.job_id != first.job_id
                && event.job.state.ended()
            {
                assert_eq!(
                    event.job.state,
                    JobState::Succeeded,
                    "automatic rescan did not succeed: {:?}",
                    event.job
                );
                added += event.job.scan.unwrap().new;
                if added > 0 {
                    let stats = client.call::<LibraryStats>(&NoParams {}).await.unwrap();
                    if stats.files == 2 && stats.files_unreadable == 0 {
                        break event.job;
                    }
                }
            }
        };
        assert_eq!(added, 1);
        // Follow-up events for the same copy can trigger another rescan
        // after this one, so the last automatic rescan is the one waited
        // for or a later one, triggered by a change or a lost-events rescan.
        // A job's id begins with the millisecond it was made in, and a
        // second rescan is made only once the first has ended.
        let shown = client.status().await.unwrap().watch.unwrap();
        let last = shown.folders[0].last_auto.as_ref().unwrap();
        assert!(
            matches!(last.trigger, AutoTrigger::Change | AutoTrigger::Lost),
            "unexpected automatic trigger: {:?}",
            last.trigger
        );
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
        waiting.set("watching to turn back on");
        while !client.status().await.unwrap().watch.unwrap().on {
            tokio::task::yield_now().await;
        }
        waiting.set("the server to stop");
        server.stop().await.unwrap();
    })
    .await
    .unwrap_or_else(|_| panic!("timed out after {WAIT:?} waiting for {}", waiting.get()));
}
