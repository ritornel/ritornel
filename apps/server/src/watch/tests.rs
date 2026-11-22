//! Watching, with the events handed in and the clock set: what each kind
//! of change becomes, when a rescan goes, and what it never does. The scans
//! the watcher queues are run here, one at a time, as the worker would.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use ritornel_contract::{
    AutoTrigger, CONTRACT_VERSION, Frame, Id, ListKind, Query, Request, RequestId, Row,
    ScanSummary, SessionKind, WatchState, WatchedKind,
};
use ritornel_engine::{JobState, OsEntropy, ScanSettings, ServerStore, scan_summary};
use serde_json::{Value, json};

use super::{Backend, Folder, Signal, Watch, WatchRun, tick};
use crate::Bounds;
use crate::data_dir::DataDir;
use crate::dispatch::{Core, Stores};
use crate::session::Session;
use crate::testing::{Scratch, SetClock};

/// 2025-11-16 12:00 UTC.
const T0: i64 = 1_763_294_400_000;
const SECOND: i64 = 1_000;
const HOUR: i64 = 60 * 60 * SECOND;

/// The ID `a.flac` gives its artist.
const MOSS_ARCADE: &str = "12121212-3434-4565-8787-909090909090";

/// A PNG of one pixel.
const PNG: [u8; 70] = [
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0x64, 0x60, 0xf8, 0x5f,
    0x0f, 0x00, 0x02, 0x87, 0x01, 0x80, 0xeb, 0x47, 0xba, 0x92, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
    0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

/// Folders a test's watcher was asked to watch. One it refuses has no
/// events, as a network share may not.
#[derive(Debug, Default)]
struct Fake {
    watched: Vec<PathBuf>,
    refuse: bool,
    /// Every watch asked for, kept or not.
    asked: usize,
}

#[derive(Debug, Clone, Default)]
struct Shared(Arc<Mutex<Fake>>);

impl Backend for Shared {
    fn watch(&mut self, path: &Path) -> Result<(), String> {
        let mut fake = self.0.lock().unwrap();
        fake.asked += 1;
        if fake.refuse {
            return Err("no events for this folder".to_owned());
        }
        fake.watched.push(path.to_owned());
        Ok(())
    }

    fn unwatch(&mut self, path: &Path) {
        self.0
            .lock()
            .unwrap()
            .watched
            .retain(|watched| watched != path);
    }
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/audio")
        .join(name)
}

/// A hub with a music root, scanned, and a watcher whose events the test
/// hands in.
struct Rig {
    core: Core,
    clock: Arc<SetClock>,
    scratch: Scratch,
    session: Arc<Session>,
    watch: Watch,
    fake: Shared,
    source: Id,
    keys: u8,
    /// Each file put gets a time of its own, so a scan never takes a
    /// replaced file for the one it replaced.
    mtimes: u64,
}

impl Rig {
    fn new() -> Self {
        Self::with(false)
    }

    fn with(refuse: bool) -> Self {
        let clock = Arc::new(SetClock::at(T0));
        let scratch = Scratch::new();
        let dir = DataDir::prepare(scratch.path().join("data")).unwrap();
        let store = ServerStore::open_in_memory(clock.clone(), Arc::new(OsEntropy)).unwrap();
        let (identity, issued) = store.ensure_identity("desk").unwrap();
        let (_, verifier) = crate::credential::ensure(&store, &dir.credential(), issued).unwrap();
        let core = Core::new(
            Stores::new(store, None),
            &identity,
            &dir,
            verifier,
            clock.clone(),
            Arc::new(OsEntropy),
            Bounds::default(),
        )
        .unwrap();
        let run = WatchRun {
            injected: true,
            ..WatchRun::on()
        };
        core.set_watch(run.clone());
        let session = core.open_session(SessionKind::Full).unwrap();
        let fake = Shared::default();
        fake.0.lock().unwrap().refuse = refuse;
        let mut rig = Self {
            watch: Watch::new(Box::new(fake.clone()), &run),
            core,
            clock,
            scratch,
            session,
            fake,
            source: Id::from_bytes([0; 16]),
            keys: 0,
            mtimes: 0,
        };
        let hello = json!({
            "name": "test", "kind": "cli", "version": "1", "contractVersion": CONTRACT_VERSION
        });
        rig.call("server.hello", hello);
        let key = rig.key();
        rig.call("library.create", key);
        rig.put("Tin Weather/01.flac", "a.flac");
        rig.put("Tin Weather/02.flac", "g1.flac");
        let mut add = rig.key();
        add["name"] = json!("music");
        add["path"] = json!(rig.music().to_str().unwrap());
        rig.source =
            serde_json::from_value(rig.call("sources.add", add)["source"]["sourceId"].clone())
                .unwrap();
        let mut scan = rig.key();
        scan["sourceId"] = json!(rig.source);
        rig.call("scan.start", scan);
        rig.run_jobs();
        // The hub starts: its root was scanned a moment ago.
        rig.look();
        rig
    }

    fn call(&self, method: &str, params: Value) -> Value {
        let request = Frame::Request(Request {
            id: RequestId::Number(1),
            method: method.to_owned(),
            params: Some(params),
        });
        let Some(Frame::Response(response)) = self.core.dispatch(&self.session, request) else {
            panic!("a request gets a response");
        };
        response
            .outcome
            .unwrap_or_else(|error| panic!("{method}: {error:?}"))
    }

    fn key(&mut self) -> Value {
        self.keys += 1;
        json!({ "idempotencyKey": Id::from_bytes([self.keys; 16]).to_string() })
    }

    fn music(&self) -> PathBuf {
        self.scratch.path().join("music")
    }

    /// Puts a copy of a fixture under the music root.
    fn put(&mut self, path: &str, fixture_name: &str) {
        let to = self.music().join(path);
        std::fs::create_dir_all(to.parent().unwrap()).unwrap();
        std::fs::write(&to, std::fs::read(fixture(fixture_name)).unwrap()).unwrap();
        self.mtimes += 1;
        let file = std::fs::File::options().write(true).open(&to).unwrap();
        file.set_modified(UNIX_EPOCH + Duration::from_secs(1_700_000_000 + self.mtimes))
            .unwrap();
    }

    fn at(&self, now_ms: i64) {
        self.clock.set(now_ms);
    }

    fn now(&self) -> i64 {
        ritornel_engine::Clock::now_ms(&*self.clock)
    }

    /// The system says these paths changed, now.
    fn changed(&mut self, paths: &[PathBuf]) {
        let now = self.now();
        self.watch.signal(Signal::Changed(paths.to_vec()), now);
    }

    /// A look by the watching thread: the folders, then what is due.
    fn look(&mut self) {
        let now = self.now();
        self.watch
            .refresh(self.core.watched_folders().unwrap(), now);
        tick(&self.core, &mut self.watch, now);
        // What the thread shows the status.
        self.core.watching().unwrap().show(None, self.watch.shown());
    }

    fn jobs(&self) -> Vec<ritornel_engine::Job> {
        let mut jobs = self
            .core
            .with_stores(|stores| stores.server().jobs(100).unwrap())
            .unwrap();
        jobs.reverse();
        jobs
    }

    /// Runs every queued job to its end, as the worker would.
    fn run_jobs(&self) {
        let boot = Id::from_bytes([0xb0; 16]);
        self.core
            .with_library(|server, library| {
                while let Some(job) = server.claim_job(boot).unwrap() {
                    let row = job.id;
                    let (state, failure) = match ritornel_engine::run_scan(
                        job,
                        server,
                        library,
                        ScanSettings::default(),
                    ) {
                        Ok(_) => (JobState::Succeeded, None),
                        Err(failed) => (JobState::Failed, Some(failed.to_string())),
                    };
                    server.finish_job(row, state, failure).unwrap();
                }
            })
            .unwrap();
    }

    /// What the last job's scan found.
    fn last_scan(&self) -> ScanSummary {
        scan_summary(self.jobs().last().unwrap()).unwrap()
    }

    fn stats(&self) -> Value {
        self.call("library.stats", json!({}))
    }

    /// The tracks of the files under `path`, by title, with the track each
    /// is.
    fn tracks(&self, path: &Path) -> Vec<(String, String)> {
        let found = self.call("sources.tracks", json!({ "path": path.to_str().unwrap() }));
        found["tracks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|track| {
                (
                    track["title"].as_str().unwrap().to_owned(),
                    track["appearanceId"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    }

    fn status(&self) -> Value {
        self.call("server.status", json!({}))["watch"].clone()
    }
}

#[test]
fn a_new_file_is_picked_up_once_the_folder_is_quiet() {
    let mut rig = Rig::new();
    assert_eq!(rig.jobs().len(), 1);
    assert_eq!(
        rig.fake.0.lock().unwrap().watched,
        [rig.music()],
        "the root is watched"
    );

    rig.at(T0 + 10 * SECOND);
    rig.put("Tin Weather/03.flac", "g2.flac");
    rig.changed(&[rig.music().join("Tin Weather/03.flac")]);
    // Still settling: nothing yet.
    rig.at(T0 + 13 * SECOND);
    rig.look();
    assert_eq!(rig.jobs().len(), 1);
    assert_eq!(rig.status()["folders"][0]["pendingSince"], T0 + 10 * SECOND);

    rig.at(T0 + 14 * SECOND);
    rig.look();
    let jobs = rig.jobs();
    assert_eq!(jobs.len(), 2);
    assert!(
        jobs[1].params_json.contains(r#""trigger":"change""#),
        "{}",
        jobs[1].params_json
    );
    rig.run_jobs();
    let found = rig.last_scan();
    assert_eq!((found.new, found.unchanged), (1, 2));
    assert_eq!(rig.stats()["files"], 3);

    let status = rig.status();
    assert_eq!(status["on"], true);
    let music = &status["folders"][0];
    assert_eq!(
        (&music["name"], &music["kind"], &music["state"]),
        (&json!("music"), &json!("music"), &json!("watching"))
    );
    assert_eq!(music.get("pendingSince"), None);
    assert_eq!(music["lastAuto"]["trigger"], "change");
    assert_eq!(music["lastAuto"]["state"], "succeeded");
}

#[test]
fn a_retag_moves_and_deletions_are_taken_in_by_the_rescan_rules() {
    let mut rig = Rig::new();
    let album = rig.music().join("Tin Weather");
    let first = rig.tracks(&album.join("01.flac"));
    assert_eq!(first.len(), 1);

    // Retagged: the same audio under other tags, at the same path.
    rig.at(T0 + 10 * SECOND);
    rig.put("Tin Weather/01.flac", "a.retagged.flac");
    rig.changed(&[album.join("01.flac")]);
    rig.at(T0 + 20 * SECOND);
    rig.look();
    rig.run_jobs();
    let found = rig.last_scan();
    assert_eq!((found.changed, found.new, found.retired), (1, 0, 0));
    let retagged = rig.tracks(&album.join("01.flac"));
    assert_ne!(retagged[0].0, first[0].0, "the new tags show");
    assert_eq!(rig.stats()["files"], 2);

    // Moved within the root: the same item, so the same track.
    let moved_from = rig.tracks(&album.join("02.flac"));
    std::fs::create_dir_all(rig.music().join("Elsewhere")).unwrap();
    std::fs::rename(album.join("02.flac"), rig.music().join("Elsewhere/02.flac")).unwrap();
    rig.changed(&[album.join("02.flac"), rig.music().join("Elsewhere/02.flac")]);
    rig.at(T0 + 30 * SECOND);
    rig.look();
    rig.run_jobs();
    let found = rig.last_scan();
    assert_eq!((found.moved, found.new, found.missing), (1, 0, 0));
    assert_eq!(
        rig.tracks(&rig.music().join("Elsewhere/02.flac")),
        moved_from
    );

    // Deleted: missing, with its rows kept.
    std::fs::remove_file(album.join("01.flac")).unwrap();
    rig.changed(&[album.join("01.flac")]);
    rig.at(T0 + 40 * SECOND);
    rig.look();
    rig.run_jobs();
    assert_eq!(rig.last_scan().missing, 1);
    let stats = rig.stats();
    assert_eq!(
        (&stats["files"], &stats["filesMissing"]),
        (&json!(2), &json!(1))
    );
}

#[test]
fn a_burst_of_changes_is_one_scan_and_churn_waits_no_longer_than_a_minute() {
    let mut rig = Rig::new();
    // A copy of an album, a file a second: one scan, after the last.
    for n in 0..10 {
        rig.at(T0 + (10 + n) * SECOND);
        rig.put(&format!("Copied/{n:02}.flac"), "g2.flac");
        rig.changed(&[rig.music().join(format!("Copied/{n:02}.flac"))]);
        rig.look();
    }
    assert_eq!(rig.jobs().len(), 1);
    rig.at(T0 + 23 * SECOND);
    rig.look();
    assert_eq!(rig.jobs().len(), 2);
    rig.run_jobs();
    assert_eq!(rig.last_scan().new, 10);

    // A folder that never settles is scanned once a minute has passed.
    let start = T0 + 100 * SECOND;
    for n in 0..=60 {
        rig.at(start + n * SECOND);
        rig.changed(&[rig.music().join("Copied/00.flac")]);
        rig.look();
    }
    assert_eq!(rig.jobs().len(), 3);
    assert_eq!(rig.jobs()[2].created_at_ms, start + 60 * SECOND);
}

#[test]
fn hidden_files_and_other_folders_are_not_changes() {
    let mut rig = Rig::new();
    // A data directory kept in a root is written all the time.
    rig.watch.skip(&rig.music().join("ritornel-data"));
    rig.at(T0 + 10 * SECOND);
    rig.changed(&[
        rig.music().join("ritornel-data/server.db-wal"),
        rig.music().join(".DS_Store"),
        rig.music().join("Tin Weather/.01.flac.part"),
        rig.music().join(".Trash/old.flac"),
        rig.scratch.path().join("elsewhere/new.flac"),
    ]);
    rig.at(T0 + 20 * SECOND);
    rig.look();
    assert_eq!(rig.jobs().len(), 1);
}

#[test]
fn a_manual_scan_queued_or_running_since_the_changes_is_joined() {
    let mut rig = Rig::new();
    let boot = Id::from_bytes([0xb1; 16]);
    let scan = |rig: &mut Rig| {
        let mut params = rig.key();
        params["sourceId"] = json!(rig.source);
        rig.call("scan.start", params);
    };

    // Queued by hand: it will see the changes.
    rig.at(T0 + 10 * SECOND);
    rig.changed(&[rig.music().join("Tin Weather/01.flac")]);
    scan(&mut rig);
    rig.at(T0 + 20 * SECOND);
    rig.look();
    assert_eq!(rig.jobs().len(), 2);
    rig.run_jobs();

    // Running, and started after the changes: it sees them too.
    rig.at(T0 + 30 * SECOND);
    rig.changed(&[rig.music().join("Tin Weather/01.flac")]);
    scan(&mut rig);
    rig.at(T0 + 31 * SECOND);
    rig.core
        .with_stores(|stores| stores.server().claim_job(boot).unwrap())
        .unwrap();
    rig.at(T0 + 40 * SECOND);
    rig.look();
    assert_eq!(rig.jobs().len(), 3);
    rig.core
        .with_stores(|stores| {
            let running = stores
                .server()
                .active_job("scan", "music")
                .unwrap()
                .unwrap();
            stores
                .server()
                .finish_job(running.id, JobState::Succeeded, None)
                .unwrap();
        })
        .unwrap();

    // Running since before the changes: they wait for it to end, and then
    // have a scan of their own.
    rig.at(T0 + 50 * SECOND);
    scan(&mut rig);
    rig.core
        .with_stores(|stores| stores.server().claim_job(boot).unwrap())
        .unwrap();
    rig.at(T0 + 51 * SECOND);
    rig.changed(&[rig.music().join("Tin Weather/01.flac")]);
    rig.at(T0 + 60 * SECOND);
    rig.look();
    assert_eq!(rig.jobs().len(), 4);
    rig.look();
    assert_eq!(rig.jobs().len(), 4, "never two scans of a root at once");
    rig.core
        .with_stores(|stores| {
            let running = stores
                .server()
                .active_job("scan", "music")
                .unwrap()
                .unwrap();
            stores
                .server()
                .finish_job(running.id, JobState::Succeeded, None)
                .unwrap();
        })
        .unwrap();
    rig.look();
    let jobs = rig.jobs();
    assert_eq!(jobs.len(), 5);
    assert_eq!(jobs[4].state, JobState::Queued);
}

#[test]
fn a_job_that_panics_ends_as_failed_and_says_why() {
    let mut rig = Rig::new();
    let mut start = rig.key();
    start["sourceId"] = json!(rig.source);
    rig.call("scan.start", start);
    let boot = Id::from_bytes([0xb0; 16]);
    let job = rig
        .core
        .with_stores(|stores| stores.server().claim_job(boot).unwrap().unwrap())
        .unwrap();
    crate::jobs::caught(&rig.core, job, |_| panic!("a file the reader chokes on"));
    let ended = rig.jobs().pop().unwrap();
    assert_eq!(ended.state, JobState::Failed);
    assert_eq!(
        ended.error.as_deref(),
        Some("the job stopped on an error it did not expect: a file the reader chokes on")
    );
}

#[test]
fn a_root_that_goes_offline_is_never_taken_to_be_empty() {
    let mut rig = Rig::new();
    let away = rig.scratch.path().join("unmounted");

    // The disk is unmounted: the folder is gone, and so is every file in it.
    rig.at(T0 + 10 * SECOND);
    std::fs::rename(rig.music(), &away).unwrap();
    rig.changed(&[rig.music()]);
    rig.at(T0 + 20 * SECOND);
    rig.look();
    assert_eq!(rig.jobs().len(), 1, "no scan of a folder that is not there");
    assert_eq!(rig.status()["folders"][0]["state"], "offline");
    assert!(rig.fake.0.lock().unwrap().watched.is_empty());
    // The timer passes it over too.
    rig.at(T0 + 7 * HOUR);
    rig.look();
    assert_eq!(rig.jobs().len(), 1);
    assert_eq!(rig.stats()["filesMissing"], 0);

    // Back: it is watched again and rescanned, and nothing was lost.
    std::fs::rename(&away, rig.music()).unwrap();
    rig.look();
    assert_eq!(rig.status()["folders"][0]["state"], "watching");
    rig.at(T0 + 7 * HOUR + 10 * SECOND);
    rig.look();
    rig.run_jobs();
    let jobs = rig.jobs();
    assert_eq!(jobs.len(), 2);
    assert!(jobs[1].params_json.contains(r#""trigger":"returned""#));
    assert_eq!(rig.last_scan().unchanged, 2);
    assert_eq!(rig.stats()["filesMissing"], 0);

    // A mount point left with nothing but a hidden file is offline as
    // well: the scan fails and marks nothing missing.
    for file in ["Tin Weather/01.flac", "Tin Weather/02.flac"] {
        std::fs::remove_file(rig.music().join(file)).unwrap();
    }
    std::fs::write(rig.music().join(".DS_Store"), b"").unwrap();
    rig.changed(&[rig.music().join("Tin Weather/01.flac")]);
    rig.at(T0 + 7 * HOUR + 20 * SECOND);
    rig.look();
    rig.run_jobs();
    let jobs = rig.jobs();
    assert_eq!(jobs[2].state, JobState::Failed);
    assert_eq!(rig.stats()["filesMissing"], 0);
}

#[test]
fn a_disk_mounted_again_unseen_is_watched_afresh_and_rescanned() {
    let mut rig = Rig::new();
    assert_eq!(rig.fake.0.lock().unwrap().asked, 1);
    // Unmounted and mounted again between two looks: the same path, and
    // another folder, which the old watch hears nothing of.
    let away = rig.scratch.path().join("unmounted");
    std::fs::rename(rig.music(), &away).unwrap();
    std::fs::create_dir(rig.music()).unwrap();
    std::fs::rename(away.join("Tin Weather"), rig.music().join("Tin Weather")).unwrap();
    rig.at(T0 + 10 * SECOND);
    rig.look();
    assert_eq!(rig.fake.0.lock().unwrap().asked, 2);
    assert_eq!(rig.fake.0.lock().unwrap().watched, [rig.music()]);
    rig.at(T0 + 20 * SECOND);
    rig.look();
    rig.run_jobs();
    let jobs = rig.jobs();
    assert_eq!(jobs.len(), 2);
    assert!(jobs[1].params_json.contains(r#""trigger":"returned""#));
    assert_eq!(rig.last_scan().unchanged, 2);

    // Lost events: watched afresh, and rescanned.
    rig.at(T0 + 30 * SECOND);
    let now = rig.now();
    rig.watch
        .signal(Signal::Lost(Some(rig.music()), "overflow".to_owned()), now);
    rig.look();
    assert_eq!(rig.fake.0.lock().unwrap().asked, 3);
    rig.at(T0 + 40 * SECOND);
    rig.look();
    let jobs = rig.jobs();
    assert_eq!(jobs.len(), 3);
    assert!(jobs[2].params_json.contains(r#""trigger":"lost""#));
}

#[test]
fn a_change_in_a_folder_inside_another_is_a_change_to_both() {
    let mut rig = Rig::new();
    let pictures = rig.music().join("Artist pictures");
    std::fs::create_dir_all(&pictures).unwrap();
    let folders = vec![
        Folder {
            name: "music".to_owned(),
            path: rig.music(),
            source: Some(rig.source),
        },
        Folder {
            name: "pictures".to_owned(),
            path: pictures.clone(),
            source: None,
        },
    ];
    rig.watch.refresh(folders, rig.now());
    rig.at(T0 + 10 * SECOND);
    rig.changed(&[pictures.join("someone/portrait.jpg")]);
    let pending: Vec<Option<i64>> = rig
        .watch
        .shown()
        .iter()
        .map(|shown| shown.pending_since)
        .collect();
    assert_eq!(pending, [Some(T0 + 10 * SECOND), Some(T0 + 10 * SECOND)]);
}

#[test]
fn the_timer_rescans_a_folder_with_no_events_and_the_start_one_left_too_long() {
    let mut rig = Rig::with(true);
    let music = &rig.status()["folders"][0];
    assert_eq!(music["state"], "polling");
    assert_eq!(music["detail"], "no events for this folder");

    // Not yet: the last scan ended under six hours ago.
    rig.at(T0 + 5 * HOUR);
    rig.look();
    assert_eq!(rig.jobs().len(), 1);
    rig.put("Tin Weather/03.flac", "g2.flac");
    rig.at(T0 + 6 * HOUR);
    rig.look();
    rig.run_jobs();
    let jobs = rig.jobs();
    assert_eq!(jobs.len(), 2);
    assert!(jobs[1].params_json.contains(r#""trigger":"timer""#));
    assert_eq!(rig.last_scan().new, 1);

    // The hub starts again two hours later: what changed meanwhile is
    // looked for at once.
    rig.at(T0 + 8 * HOUR);
    rig.watch = Watch::new(Box::new(rig.fake.clone()), &WatchRun::on());
    rig.look();
    let jobs = rig.jobs();
    assert_eq!(jobs.len(), 3);
    assert!(jobs[2].params_json.contains(r#""trigger":"start""#));
}

#[test]
fn a_server_that_is_not_the_hub_watches_nothing() {
    let clock = Arc::new(SetClock::at(T0));
    let scratch = Scratch::new();
    let dir = DataDir::prepare(scratch.path().join("data")).unwrap();
    let store = ServerStore::open_in_memory(clock.clone(), Arc::new(OsEntropy)).unwrap();
    let (identity, issued) = store.ensure_identity("desk").unwrap();
    let (_, verifier) = crate::credential::ensure(&store, &dir.credential(), issued).unwrap();
    let core = Core::new(
        Stores::new(store, None),
        &identity,
        &dir,
        verifier,
        clock,
        Arc::new(OsEntropy),
        Bounds::default(),
    )
    .unwrap();
    core.set_watch(WatchRun {
        injected: true,
        ..WatchRun::on()
    });
    assert_eq!(
        core.watch_off().as_deref(),
        Some("this server is not the library's hub")
    );
    assert_eq!(core.watched_folders(), None);
    let status = core
        .with_stores(|stores| core.watch_status(stores))
        .unwrap();
    assert!(!status.on);
    assert!(status.folders.is_empty());
}

#[test]
fn watching_turned_off_stops_and_a_run_that_does_not_watch_says_so() {
    let rig = Rig::new();
    assert_eq!(rig.core.watch_off(), None);
    rig.core.set_watching(false).unwrap();
    assert_eq!(
        rig.core.watch_off().as_deref(),
        Some("watching is turned off")
    );
    assert_eq!(rig.status()["on"], false);
    rig.core.set_watching(true).unwrap();
    assert_eq!(rig.core.watch_off(), None);
}

#[test]
fn a_picture_added_to_a_picture_folder_is_imported() {
    let mut rig = Rig::new();
    let pictures = rig.scratch.path().join("pictures");
    std::fs::create_dir_all(&pictures).unwrap();
    std::fs::write(
        pictures.join("index.json"),
        r#"{"format": 1, "artists": []}"#,
    )
    .unwrap();
    let mut import = rig.key();
    import["name"] = json!("pictures");
    import["path"] = json!(pictures.to_str().unwrap());
    rig.call("artists.importPictures", import);
    rig.look();
    let folders = rig.status()["folders"].clone();
    assert_eq!(folders[1]["name"], "pictures");
    assert_eq!(folders[1]["kind"], WatchedKind::Pictures.as_str());
    let revision = rig.stats()["libraryRevision"].clone();

    let dir = pictures.join(MOSS_ARCADE);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("portrait.png"), PNG).unwrap();
    let manifest = json!({
        "format": 1,
        "files": [{ "file": "portrait.png", "kind": "portrait", "source": "owner" }],
        "chosen": { "portrait": { "file": "portrait.png", "by": "owner" } }
    });
    std::fs::write(dir.join("manifest.json"), manifest.to_string()).unwrap();
    rig.at(T0 + 10 * SECOND);
    rig.changed(&[dir.join("portrait.png"), dir.join("manifest.json")]);
    rig.at(T0 + 20 * SECOND);
    rig.look();

    // One write, the import, and no scan.
    assert_eq!(rig.jobs().len(), 1);
    assert_eq!(
        rig.stats()["libraryRevision"],
        json!(revision.as_u64().unwrap() + 1)
    );
    let portrait = rig
        .core
        .with_library(|_, library| {
            let view = library
                .list_open(ListKind::Artists, &Query::default())
                .unwrap();
            library
                .list_rows(ListKind::Artists, &view.ids)
                .unwrap()
                .into_iter()
                .any(|row| matches!(row, Row::Artist(artist) if artist.portrait))
        })
        .unwrap();
    assert!(portrait);
    let shown = &rig.status()["folders"][1]["lastAuto"];
    assert_eq!(
        (&shown["trigger"], &shown["state"]),
        (&json!(AutoTrigger::Change.as_str()), &json!("succeeded"))
    );
    assert_eq!(shown.get("jobId"), None);

    // Looking again with nothing changed writes nothing.
    rig.changed(&[dir.join("manifest.json")]);
    rig.at(T0 + 30 * SECOND);
    rig.look();
    assert_eq!(
        rig.stats()["libraryRevision"],
        json!(revision.as_u64().unwrap() + 1)
    );
}

#[test]
fn folders_are_watched_as_they_come_and_forgotten_as_they_go() {
    let mut rig = Rig::new();
    let other = rig.scratch.path().join("other");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::copy(fixture("a.mp3"), other.join("a.mp3")).unwrap();
    let mut add = rig.key();
    add["name"] = json!("other");
    add["path"] = json!(other.to_str().unwrap());
    rig.call("sources.add", add);
    rig.look();
    assert_eq!(
        rig.fake.0.lock().unwrap().watched,
        [rig.music(), other.clone()]
    );
    // Never scanned: the timer scans it, a moment after it was added.
    assert_eq!(rig.jobs().len(), 1);
    rig.at(T0 + 4 * SECOND);
    rig.look();
    rig.run_jobs();
    assert_eq!(rig.jobs().len(), 2);
    assert_eq!(rig.last_scan().new, 1);

    rig.watch.refresh(
        vec![Folder {
            name: "music".to_owned(),
            path: rig.music(),
            source: Some(rig.source),
        }],
        rig.now(),
    );
    assert_eq!(rig.fake.0.lock().unwrap().watched, [rig.music()]);
    assert_eq!(rig.watch.shown().len(), 1);
    assert_eq!(rig.watch.shown()[0].state, WatchState::Watching);
}

/// A running hub with a player on the null output, whose watcher's events
/// the test hands in: a rescan while a track plays leaves it be.
mod playing {
    use std::path::Path;
    use std::sync::Arc;
    use std::time::Duration;

    use ritornel_client::Client;
    use ritornel_client::contract::methods::{
        HistoryRecent, PlaybackGet, PlaybackPause, PlaybackPlay, PlaybackResume, QueueWindow,
        SourcesTracks,
    };
    use ritornel_client::contract::{
        CONTRACT_VERSION, ClientKind, Event, HelloParams, HistoryRecentParams, Id, NoParams,
        PlaySource, PlaybackCommandParams, PlaybackPlayParams, PlaybackStatus, QueueWindowParams,
        SessionKind, SourcesTracksParams, Topic,
    };
    use ritornel_engine::playback::Output;
    use ritornel_engine::{OsEntropy, SystemClock};

    use super::{Signal, WatchRun, fixture};
    use crate::testing::Scratch;
    use crate::{Bounds, HeldDir, Listen, PlaybackRun, Run, Server};

    const WAIT: Duration = Duration::from_secs(60);

    fn key() -> Id {
        ritornel_client::idempotency_key().unwrap()
    }

    async fn ended(client: &mut Client, job: Id) {
        loop {
            if let Event::Jobs(event) = client.next_event().await.unwrap()
                && event.job.job_id == job
                && event.job.state.ended()
            {
                return;
            }
        }
    }

    #[tokio::test]
    async fn a_file_replaced_while_it_plays_plays_on_as_it_was_through_the_rescan() {
        tokio::time::timeout(WAIT, async {
            let scratch = Scratch::new();
            let run = Run {
                playback: PlaybackRun {
                    output: Some(Output::Null { speed: 1 }),
                    ..PlaybackRun::default()
                },
                watch: WatchRun {
                    injected: true,
                    quiet: Duration::from_millis(50),
                    ..WatchRun::on()
                },
                ..Run::default()
            };
            let server = Server::start_run(
                HeldDir::take(scratch.path().join("data")).unwrap(),
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
            let folder = scratch.path().join("music");
            std::fs::create_dir_all(folder.join("Tin Weather")).unwrap();
            std::fs::copy(fixture("b.flac"), folder.join("Tin Weather/01.flac")).unwrap();
            std::fs::copy(fixture("a.retagged.mp3"), folder.join("Tin Weather/02.mp3")).unwrap();
            let folder = folder.canonicalize().unwrap();
            client.create_library(key()).await.unwrap();
            let source = client
                .add_source(key(), "music", folder.to_str().unwrap())
                .await
                .unwrap()
                .source;
            client.subscribe(&[Topic::Jobs]).await.unwrap();
            let job = client
                .start_scan(key(), source.source_id)
                .await
                .unwrap()
                .job;
            ended(&mut client, job.job_id).await;
            let mut tracks = Vec::new();
            for file in ["Tin Weather/01.flac", "Tin Weather/02.mp3"] {
                let found = client
                    .call::<SourcesTracks>(&SourcesTracksParams {
                        path: folder.join(file).to_str().unwrap().to_owned(),
                    })
                    .await
                    .unwrap()
                    .tracks;
                tracks.extend(found);
            }
            assert_eq!(tracks.len(), 2);

            // The first track plays, and is paused, so that it is still
            // playing when the rescan has ended.
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
            client
                .call::<PlaybackPause>(&PlaybackCommandParams {
                    idempotency_key: key(),
                })
                .await
                .unwrap();
            let before = client.call::<PlaybackGet>(&NoParams {}).await.unwrap();
            assert_eq!(before.status, PlaybackStatus::Paused);
            assert_eq!(before.index, Some(0));

            // Another song is saved over it, as a tagger would put it there,
            // and the watcher hears of it.
            let playing = folder.join("Tin Weather/01.flac");
            let incoming = folder.join("Tin Weather/.01.flac.part");
            std::fs::copy(fixture("a.retagged.flac"), &incoming).unwrap();
            std::fs::rename(&incoming, &playing).unwrap();
            let watching = server.core().watching().unwrap();
            watching.send(Signal::Changed(vec![playing.clone()]));
            let rescan = loop {
                if let Event::Jobs(event) = client.next_event().await.unwrap()
                    && event.job.job_id != job.job_id
                    && event.job.state.ended()
                {
                    break event.job;
                }
            };
            let found = rescan.scan.unwrap();
            assert_eq!((found.retired, found.new), (1, 0), "{found:?}");

            // The player is where it was, on the song it was playing.
            let after = client.call::<PlaybackGet>(&NoParams {}).await.unwrap();
            assert_eq!(after.status, PlaybackStatus::Paused);
            assert_eq!(
                (after.index, after.entry_id),
                (before.index, before.entry_id)
            );
            assert_eq!(
                after.track.as_ref().map(|track| track.appearance_id),
                Some(tracks[0].appearance_id)
            );

            // It plays on to the end, as it was, and is heard as it was.
            client
                .call::<PlaybackResume>(&PlaybackCommandParams {
                    idempotency_key: key(),
                })
                .await
                .unwrap();
            loop {
                let state = client.call::<PlaybackGet>(&NoParams {}).await.unwrap();
                if state.status == PlaybackStatus::Stopped {
                    break;
                }
                tokio::task::yield_now().await;
            }
            let window = client
                .call::<QueueWindow>(&QueueWindowParams {
                    offset: 0,
                    limit: None,
                    order: None,
                })
                .await
                .unwrap();
            assert!(
                window
                    .entries
                    .iter()
                    .all(|entry| entry.failed_reason.is_none()),
                "{window:?}"
            );
            let recent = loop {
                let recent = client
                    .call::<HistoryRecent>(&HistoryRecentParams {
                        limit: None,
                        before: None,
                    })
                    .await
                    .unwrap();
                if recent.undelivered == 0 {
                    break recent;
                }
                tokio::task::yield_now().await;
            };
            let heard: Vec<Option<Id>> = recent
                .plays
                .iter()
                .map(|play| play.track.appearance_id)
                .collect();
            assert!(heard.contains(&Some(tracks[0].appearance_id)), "{heard:?}");
            assert!(Path::new(&playing).exists());
            server.stop().await.unwrap();
        })
        .await
        .unwrap();
    }
}
