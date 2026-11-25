//! Drive the hub's real link-event handler and sender against a loopback Last.fm.
//! The worker is stepped explicitly so a pause, disconnect or account change can
//! fall exactly between a start and the end of its settling delay.

use std::sync::Arc;
use std::time::{Duration, Instant};

use ritornel_contract::{CONTRACT_VERSION, Id, SessionKind};
use ritornel_engine::{JobState, ScanSettings, run_scan};
use ritornel_lastfm::fake::FakeLastfm;
use serde_json::{Value, json};

use super::{Core, Scratch, Session, SetClock, T0, call, hello, key, server_at};
use crate::lastfm::show_settled;

struct Hub {
    core: Core,
    dir: Scratch,
    cli: Arc<Session>,
    fake: FakeLastfm,
    songs: Vec<Value>,
}

impl Hub {
    fn new() -> Self {
        let fake = FakeLastfm::start("someone", vec![]);
        let (core, dir) = server_at(&Arc::new(SetClock::at(T0)), fake.base_url());
        let cli = hello(&core, "cli");
        call(&core, &cli, "library.create", key(1)).unwrap();
        let folder = dir.path().join("music");
        std::fs::create_dir(&folder).unwrap();
        for name in ["b.flac", "a.retagged.flac"] {
            let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/audio")
                .join(name);
            std::fs::copy(source, folder.join(name)).unwrap();
        }
        let added = call(
            &core,
            &cli,
            "sources.add",
            json!({"idempotencyKey":key(2)["idempotencyKey"], "name":"Music", "path":folder}),
        )
        .unwrap();
        call(&core, &cli, "scan.start", json!({"idempotencyKey":key(3)["idempotencyKey"],"sourceId":added["source"]["sourceId"]})).unwrap();
        core.with_library(|server, library| {
            let job = server.claim_job(Id::from_bytes([4; 16])).unwrap().unwrap();
            let row = job.id;
            run_scan(job, server, library, ScanSettings::default()).unwrap();
            server.finish_job(row, JobState::Succeeded, None).unwrap();
        })
        .unwrap();
        let list = call(
            &core,
            &cli,
            "list.open",
            json!({"kind":"songs","window":{"offset":0,"limit":2}}),
        )
        .unwrap();
        let mut songs = list["rows"].as_array().unwrap().clone();
        songs.sort_by_key(|row| row["title"] != "Paper Kites");
        assert_eq!(songs.len(), 2, "{list}");
        Self {
            core,
            dir,
            cli,
            fake,
            songs,
        }
    }

    fn sign_in(&self) {
        self.fake.approve_after(0);
        let start = call(&self.core, &self.cli, "lastfm.signInStart", json!({})).unwrap();
        let mut finish = key(5);
        finish["signIn"] = start["signIn"].clone();
        let done = call(&self.core, &self.cli, "lastfm.signInFinish", finish).unwrap();
        assert_eq!(done["account"], "someone");
    }

    fn switch(&self, byte: u8, on: bool) {
        let mut params = key(byte);
        params["on"] = json!(on);
        call(&self.core, &self.cli, "lastfm.setScrobbling", params).unwrap();
    }

    fn peer(&self, byte: u8, own: bool) -> Arc<Session> {
        let peer = self
            .core
            .open(
                &mut crate::dispatch::lock(&self.core.sessions),
                SessionKind::Full,
                Some(Id::from_bytes([byte; 16])),
            )
            .unwrap();
        let hello = json!({
            "kind": "server", "name": "Player", "version": "1", "contractVersion": CONTRACT_VERSION,
            "presence": { "hasPlayer": true, "sendsNowPlaying": own, "player": {"status": "paused"} }
        });
        call(&self.core, &peer, "server.hello", hello).unwrap();
        peer
    }

    fn began(&self, peer: &Session, song: usize) {
        let report = json!({
            "topic": "began", "appearanceId": self.songs[song]["appearanceId"], "durationMs": 43_100
        });
        call(&self.core, peer, "link.event", report).unwrap();
    }

    fn state(&self, peer: &Session, status: &str, song: Option<usize>) {
        let track = song.map(|index| json!({"appearanceId": self.songs[index]["appearanceId"]}));
        let report = json!({"topic": "playback", "state": {
            "status": status, "revision": 1, "positionMs": 0, "volume": 1, "muted": false,
            "track": track, "shuffle": false, "repeat": "off", "queueRevision": 1, "queueLength": 1,
            "output": {"name": "Null", "mode": "shared", "sampleRateHz": 44100, "channels": 2}
        }});
        call(&self.core, peer, "link.event", report).unwrap();
    }

    fn settle(&self) {
        let settled = Instant::now() + Duration::from_secs(2);
        assert!(show_settled(&self.core, self.core.lastfm().unwrap(), || settled).is_none());
    }

    fn titles(&self) -> Vec<String> {
        self.fake
            .now_playing()
            .into_iter()
            .map(|p| p["track"].clone())
            .collect()
    }
}

#[test]
fn the_hub_sends_in_start_order_and_cancels_only_the_affected_player() {
    let h = Hub::new();
    h.sign_in();
    h.switch(6, true);
    let a = h.peer(10, false);
    let b = h.peer(11, false);
    let first = h.songs[0]["title"].as_str().unwrap();
    let second = h.songs[1]["title"].as_str().unwrap();
    h.began(&a, 0);
    h.began(&b, 1);
    h.settle();
    assert_eq!(h.titles(), [first, second]);
    h.settle();
    assert_eq!(h.titles(), [first, second]);
    let sent = &h.fake.now_playing()[0];
    assert_eq!(sent["artist"], "Moss Arcade");
    assert_eq!(sent["album"], "Tin Weather");
    assert_eq!(sent["duration"], "43");

    h.began(&a, 0);
    h.began(&b, 1);
    h.state(&a, "paused", Some(0));
    h.settle();
    assert_eq!(h.titles(), [first, second, second]);
    h.began(&b, 1);
    h.began(&a, 0);
    h.state(&b, "stopped", None);
    h.settle();
    assert_eq!(h.titles(), [first, second, second, first]);

    h.began(&b, 1);
    h.began(&a, 0);
    let own = h.peer(10, true);
    h.began(&own, 0);
    h.settle();
    assert_eq!(h.titles(), [first, second, second, first, second]);

    let back = h.peer(10, false);
    // A snapshot after report loss can cancel an obsolete start. It never creates a send.
    h.began(&back, 1);
    h.state(&back, "playing", Some(0));
    h.settle();
    assert_eq!(h.titles(), [first, second, second, first, second]);
    h.began(&back, 0);
    h.began(&b, 1);
    h.state(&back, "playing", Some(0));
    h.core.close_session(&a);
    h.core.close_session(&own);
    h.core.close_session(&b);
    h.settle();
    assert_eq!(h.titles(), [first, second, second, first, second, first]);
}

#[test]
fn a_satellite_start_requires_an_enabled_session_and_a_restored_library_must_import_first() {
    let h = Hub::new();
    let desk = h.peer(10, false);
    h.began(&desk, 0);
    h.settle();
    h.sign_in();
    h.began(&desk, 0);
    h.settle();
    h.switch(6, true);
    h.began(&desk, 0);
    h.switch(7, false);
    h.switch(8, true);
    h.settle();
    assert!(h.titles().is_empty());
    assert!(
        ritornel_engine::backup::hold_sending(&h.dir.path().join("data/library.db"), T0).unwrap()
    );
    let account = h
        .core
        .with_library(|server, library| {
            let account = server
                .service_session(super::SERVICE)
                .unwrap()
                .unwrap()
                .account;
            assert!(library.opened_restored(Some(account)).unwrap().is_some());
            account
        })
        .unwrap();
    h.began(&desk, 0);
    h.settle();
    h.core
        .with_library(|_, library| assert!(library.release_hold(account).unwrap()))
        .unwrap();
    h.settle();
    assert!(h.titles().is_empty());
    h.began(&desk, 1);
    h.settle();
    assert_eq!(h.titles(), [h.songs[1]["title"].as_str().unwrap()]);
    assert!(h.fake.scrobbled().is_empty());
}

#[test]
fn a_start_waits_through_a_busy_session_but_buffering_sign_out_and_refusal_suppress_it() {
    use ritornel_lastfm::fake::SESSION_KEY;
    let h = Hub::new();
    h.sign_in();
    h.switch(6, true);
    let desk = h.peer(10, false);
    let lastfm = h.core.lastfm().unwrap();
    h.began(&desk, 0);
    let change = lastfm.hold();
    let settled = Instant::now() + Duration::from_secs(2);
    assert!(show_settled(&h.core, lastfm, || settled).is_some());
    assert!(h.titles().is_empty());
    drop(change);
    h.settle();
    assert_eq!(h.titles().len(), 1);
    h.began(&desk, 0);
    h.state(&desk, "buffering", Some(0));
    h.settle();
    h.began(&desk, 0);
    call(&h.core, &h.cli, "lastfm.signOut", key(12)).unwrap();
    h.settle();
    h.sign_in();
    h.switch(13, true);
    h.began(&desk, 1);
    h.core
        .with_stores(|stores| {
            stores
                .server()
                .refuse_service_session("lastfm", SESSION_KEY)
                .unwrap()
        })
        .unwrap();
    h.settle();
    assert_eq!(h.titles().len(), 1);
}
