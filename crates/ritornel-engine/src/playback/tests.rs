//! The player with a fake renderer, both stores in memory and clocks the
//! test moves: the queue, the session, what every listen records, the
//! crash, and delivery to history. The catalog is a few invented tracks
//! written straight into the library. See docs/internals/playback.md and
//! docs/internals/history.md.

use crate::playback::Catalog;
use std::sync::Arc;

use ritornel_domain::history::ContextKind;
use rusqlite::OptionalExtension;

use super::player::{Direction, PlayTracks, Player, PlayerSetup, Request, Status};
use super::renderer::fake::Fake;
use super::store::Staged;
use super::{SendStamps, deliver_listens};
use crate::testing::{CountingEntropy, FixedClock};
use crate::timekeeping::fixed::FixedTimekeeper;
use crate::{
    Clock, Command, Fingerprint, Id, LibraryStore, ServerIdentity, ServerStore, WriteRequest,
};

/// 2026-11-15 21:00:00 UTC: 16:00 in New York.
const T0: i64 = 1_794_776_400_000;
const ROOT: &str = "/invented/music";

/// The boot of the machine the fixed timekeeper starts in.
const MACHINE_BOOT: Id = Id::from_bytes([0xba; 16]);

fn id(byte: u8) -> Id {
    Id::from_bytes([byte; 16])
}

/// A track of the invented album: its title, its length and its file.
pub(crate) struct Track {
    pub(crate) title: &'static str,
    pub(crate) length_ms: i64,
    pub(crate) file: &'static str,
}

pub(crate) const ALBUM: [Track; 4] = [
    Track {
        title: "Paper Kites",
        length_ms: 240_000,
        file: "Moss Arcade/Tin Weather/01.flac",
    },
    Track {
        title: "Lanternfish",
        length_ms: 300_000,
        file: "Moss Arcade/Tin Weather/02.flac",
    },
    Track {
        title: "Interlude",
        length_ms: 25_000,
        file: "Moss Arcade/Tin Weather/03.flac",
    },
    Track {
        title: "Salt Road",
        length_ms: 200_000,
        file: "Moss Arcade/Tin Weather/04.mp3",
    },
];

/// Writes an album straight into the library: a root on `server`, a folder
/// source, one recording, appearance, file, revision, asset and binding a
/// track. Returns the appearances and the source items.
pub(crate) fn catalog(library: &LibraryStore, server: Id, tracks: &[Track]) -> (Vec<Id>, Vec<i64>) {
    let files: Vec<(String, String, i64)> = tracks
        .iter()
        .map(|track| {
            (
                track.title.to_owned(),
                track.file.to_owned(),
                track.length_ms,
            )
        })
        .collect();
    let request = WriteRequest {
        method: "test.catalog",
        command: None,
        client: None,
    };
    let written = library
        .write(request, move |write| {
            let (connection, rev) = (write.connection(), write.rev());
            let new = || write.new_id().map(|id| id.as_bytes().to_vec());
            connection.execute(
                "INSERT INTO library_root (public_id, server_id, name, kind, created_at_ms, rev)
                 VALUES (?1, (SELECT id FROM server WHERE public_id = ?2), 'music', 'master', 0, ?3)",
                (new()?, server.as_bytes(), rev),
            )?;
            let root = connection.last_insert_rowid();
            connection.execute(
                "INSERT INTO source_instance (public_id, kind, name, root_id, created_at_ms, rev)
                 VALUES (?1, 'folder', 'music', ?2, 0, ?3)",
                (new()?, root, rev),
            )?;
            let source = connection.last_insert_rowid();
            connection.execute(
                "INSERT INTO artist_credit (display_text, rev) VALUES ('Moss Arcade', ?1)",
                [rev],
            )?;
            let credit = connection.last_insert_rowid();
            connection.execute("INSERT INTO release_group (public_id, rev) VALUES (?1, ?2)", (new()?, rev))?;
            let group = connection.last_insert_rowid();
            let release: i64 = connection.query_row(
                "SELECT coalesce(max(id), 0) + 1 FROM release",
                [],
                |row| row.get(0),
            )?;
            connection.execute(
                "INSERT INTO release (id, public_id, release_group_id, canonical_id, display_title, rev)
                 VALUES (?1, ?2, ?3, ?1, 'Tin Weather', ?4)",
                (release, new()?, group, rev),
            )?;
            let mut made = Vec::new();
            for (number, (title, file, length)) in files.iter().enumerate() {
                let number = i64::try_from(number).unwrap() + 1;
                let recording: i64 = connection.query_row(
                    "SELECT coalesce(max(id), 0) + 1 FROM recording",
                    [],
                    |row| row.get(0),
                )?;
                connection.execute(
                    "INSERT INTO recording (id, public_id, canonical_id, display_title, display_artist_credit_id, rev)
                     VALUES (?1, ?2, ?1, ?3, ?4, ?5)",
                    (recording, new()?, title, credit, rev),
                )?;
                let appearance: i64 = connection.query_row(
                    "SELECT coalesce(max(id), 0) + 1 FROM track_appearance",
                    [],
                    |row| row.get(0),
                )?;
                let appearance_pid = new()?;
                connection.execute(
                    "INSERT INTO track_appearance
                       (id, public_id, recording_id, release_id, canonical_id, display_title,
                        display_artist_credit_id, display_disc_number, display_track_number,
                        display_duration_ms, rev)
                     VALUES (?1, ?2, ?3, ?4, ?1, ?5, ?6, 1, ?7, ?8, ?9)",
                    (appearance, &appearance_pid, recording, release, title, credit, number, length, rev),
                )?;
                connection.execute(
                    "INSERT INTO audio_asset
                       (public_id, hash_scheme, audio_hash, codec, sample_rate_hz, channel_layout,
                        bit_depth, lossless, duration_ms, rev)
                     VALUES (?1, 1, ?2, 'flac', 44100, 'stereo', 16, 1, ?3, ?4)",
                    (new()?, new()?, length, rev),
                )?;
                let asset = connection.last_insert_rowid();
                connection.execute(
                    "INSERT INTO source_item
                       (public_id, source_instance_id, item_kind, provider_key, size_bytes, mtime_ms,
                        first_seen_at_ms, rev)
                     VALUES (?1, ?2, 'file', ?3, 1000, 0, 0, ?4)",
                    (new()?, source, file, rev),
                )?;
                let item = connection.last_insert_rowid();
                connection.execute(
                    "INSERT INTO source_item_revision
                       (source_item_id, revision_no, payload_hash, payload_json, audio_asset_id,
                        observed_at_ms, rev)
                     VALUES (?1, 1, ?2, '{}', ?3, 0, ?4)",
                    (item, new()?, asset, rev),
                )?;
                let revision = connection.last_insert_rowid();
                connection.execute(
                    "UPDATE source_item SET current_revision_id = ?2, current_recording_id = ?3 WHERE id = ?1",
                    (item, revision, recording),
                )?;
                connection.execute(
                    "INSERT INTO identity_decision
                       (action, source_item_id, method, resolver_version, normalizer_version,
                        evidence_json, actor_kind, decided_at_ms, rev)
                     VALUES ('bind', ?1, 'new', 1, 1, '{}', 'resolver', 0, ?2)",
                    (item, rev),
                )?;
                let decision = connection.last_insert_rowid();
                connection.execute(
                    "INSERT INTO assignment
                       (source_item_id, recording_id, appearance_id, decision_id, effective_from_ms, rev)
                     VALUES (?1, ?2, ?3, ?4, 0, ?5)",
                    (item, recording, appearance, decision, rev),
                )?;
                made.push((Id::from_bytes(<[u8; 16]>::try_from(appearance_pid).unwrap()), item));
            }
            Ok(serde_json::to_string(&made.iter().map(|(a, i)| (a.to_string(), *i)).collect::<Vec<_>>())?)
        })
        .unwrap();
    let made: Vec<(String, i64)> = serde_json::from_str(&written.result_json).unwrap();
    made.into_iter()
        .map(|(appearance, item)| (appearance.parse::<Id>().unwrap(), item))
        .unzip()
}

/// Everything a player test needs: the stores, the renderer, the clocks.
struct Rig {
    /// The playing server's wall clock.
    clock: Arc<FixedClock>,
    /// The hub's clock: the library's.
    hub_clock: Arc<FixedClock>,
    keeper: Arc<FixedTimekeeper>,
    entropy: Arc<CountingEntropy>,
    server: ServerStore,
    library: LibraryStore,
    identity: ServerIdentity,
    fake: Fake,
    player: Player,
    /// This run of the server.
    run: Id,
    tracks: Vec<Id>,
    items: Vec<i64>,
    next_command: u8,
    /// How long the output plays on during each call to it: see [`Rig::busy`].
    busy: u64,
    /// Each track the player said began to be heard, and its length.
    began: Vec<(Id, i64)>,
}

fn setup(
    identity: &ServerIdentity,
    run: Id,
    clock: &Arc<FixedClock>,
    keeper: &Arc<FixedTimekeeper>,
    entropy: &Arc<CountingEntropy>,
) -> PlayerSetup {
    PlayerSetup {
        server_id: identity.server_id,
        run,
        clock: clock.clone(),
        timekeeper: keeper.clone(),
        entropy: entropy.clone(),
        checkpoint_every_ms: super::CHECKPOINT_EVERY_MS,
        fail_at_position_ms: None,
        fail_saves_after_ms: None,
    }
}

impl Rig {
    fn new() -> Self {
        let clock = Arc::new(FixedClock::at(T0));
        let hub_clock = Arc::new(FixedClock::at(T0));
        let keeper = Arc::new(FixedTimekeeper::new(1_000_000, Some("America/New_York")));
        let entropy = Arc::new(CountingEntropy::default());
        let server = ServerStore::open_in_memory(clock.clone(), entropy.clone()).unwrap();
        let (identity, _) = server.ensure_identity("desk").unwrap();
        let (library, _) = LibraryStore::create_in_memory(
            &identity,
            WriteRequest {
                method: "library.create",
                command: None,
                client: None,
            },
            |_| String::new(),
            hub_clock.clone(),
            entropy.clone(),
        )
        .unwrap();
        server.claim_library(library.library_id()).unwrap();
        server.add_root("music", ROOT).unwrap();
        let (tracks, items) = catalog(&library, identity.server_id, &ALBUM);
        let fake = Fake::default();
        fake.clocked(keeper.clone());
        for track in &ALBUM {
            fake.track(
                format!("{ROOT}/{}", track.file),
                u64::try_from(track.length_ms).unwrap(),
            );
        }
        let run = id(0xb0);
        let (player, crashed) = Player::restore(
            setup(&identity, run, &clock, &keeper, &entropy),
            fake.renderer(),
            &server,
            Some(&library),
        )
        .unwrap();
        assert!(crashed.is_empty());
        Self {
            clock,
            hub_clock,
            keeper,
            entropy,
            server,
            library,
            identity,
            fake,
            player,
            run,
            tracks,
            items,
            next_command: 0,
            busy: 0,
            began: Vec::new(),
        }
    }

    /// Keeps what the player said began.
    fn note(&mut self, events: &[super::PlayerEvent]) {
        for event in events {
            if let super::PlayerEvent::Began {
                appearance,
                duration_ms,
            } = event
            {
                self.began.push((*appearance, *duration_ms));
            }
        }
    }

    /// Ticks the player and keeps what it said began.
    fn tick(&mut self) {
        let events = self.player.tick(&self.server, Some(&self.library)).unwrap();
        self.note(&events);
    }

    fn request(&mut self, method: &'static str) -> Request {
        self.next_command += 1;
        Request {
            command: Command {
                id: Id::from_bytes([self.next_command; 16]),
                fingerprint: Fingerprint::of(method, &[self.next_command]),
            },
            method,
            answer: Box::new(|summary| format!("{}", summary.queue_revision)),
        }
    }

    /// Lets `ms` pass, a second at a time, ticking the player after each.
    fn pass(&mut self, ms: i64) {
        let mut left = ms;
        while left > 0 {
            let step = left.min(1000);
            self.fake.advance(u64::try_from(step).unwrap());
            self.clock.set(self.clock.now_ms() + step);
            self.hub_clock.set(self.hub_clock.now_ms() + step);
            self.keeper.advance(step);
            self.tick();
            left -= step;
        }
    }

    /// Lets `ms` pass with the output stopped: a pause.
    fn wait(&mut self, ms: i64) {
        self.clock.set(self.clock.now_ms() + ms);
        self.hub_clock.set(self.hub_clock.now_ms() + ms);
        self.keeper.advance(ms);
        self.tick();
    }

    fn play(&mut self, start: usize) {
        self.play_as(start, (ContextKind::Album, Some(id(0xa1))));
    }

    fn play_as(&mut self, start: usize, context: (ContextKind, Option<Id>)) {
        let request = self.request("playback.play");
        let what = PlayTracks {
            tracks: self.tracks.clone(),
            start: Some(start),
            context,
            actor: self.server.local_session().unwrap(),
            shuffle: None,
            known: false,
        };
        let done = self
            .player
            .play(request, what, &self.server, Some(&self.library))
            .unwrap();
        self.note(&done.events);
        // The first frames are heard on the next tick.
        self.tick();
    }

    fn pause(&mut self) {
        let request = self.request("playback.pause");
        let done = self.player.pause(request, &self.server).unwrap();
        self.note(&done.events);
    }

    fn resume(&mut self) {
        let request = self.request("playback.resume");
        let done = self
            .player
            .resume(request, &self.server, Some(&self.library))
            .unwrap();
        self.note(&done.events);
        self.tick();
    }

    fn entry(&self) -> Id {
        self.player.state().current.unwrap().entry_id
    }

    fn seek(&mut self, position_ms: i64) {
        let request = self.request("playback.seek");
        let entry = self.entry();
        let done = self
            .player
            .seek(
                request,
                entry,
                position_ms,
                &self.server,
                Some(&self.library),
            )
            .unwrap();
        self.note(&done.events);
    }

    fn skip(&mut self, direction: Direction) {
        let request = self.request("playback.skip");
        let entry = self.entry();
        let done = self
            .player
            .skip(request, entry, direction, &self.server, Some(&self.library))
            .unwrap();
        self.note(&done.events);
        self.tick();
    }

    /// The listens staged now, oldest first.
    fn staged(&self) -> Vec<Staged> {
        let ids: Vec<[u8; 16]> = self
            .server
            .read(|connection| {
                let mut statement =
                    connection.prepare("SELECT listen_id FROM listen_staging ORDER BY id")?;
                let rows = statement.query_map([], |row| row.get(0))?;
                Ok(rows.collect::<Result<_, _>>()?)
            })
            .unwrap();
        ids.into_iter()
            .map(|listen| {
                self.server
                    .staged_listen(Id::from_bytes(listen))
                    .unwrap()
                    .unwrap()
            })
            .collect()
    }

    fn session_position(&self) -> i64 {
        self.server.playback_session().unwrap().position_ms
    }

    /// The queue in queue order: each entry, its track and why it failed.
    fn queue(&self) -> Vec<(Id, Id, Option<String>)> {
        self.player
            .window(super::Order::Queue, 0, usize::MAX)
            .entries
    }

    fn stamps(&self) -> SendStamps {
        SendStamps {
            sent_at_ms: self.clock.now_ms(),
            sent_mono_ms: crate::timekeeping::Timekeeper::mono_ms(&*self.keeper),
            sent_boot: crate::timekeeping::clock_boot(&*self.keeper, self.run),
        }
    }

    /// Delivers every staged listen that has ended to the library.
    fn deliver(&self) -> super::Delivered {
        let stamps = self.stamps();
        deliver_listens(
            &self.server,
            &self.library,
            &*self.clock,
            &|| stamps,
            &|| Ok(()),
        )
        .unwrap()
    }

    fn plays(&self) -> Vec<crate::PlayRow> {
        let listener = self.library.default_listener().unwrap().unwrap();
        let mut plays = self.library.recent_plays(listener, 100, None).unwrap();
        plays.reverse();
        plays
    }

    /// Makes every write of `server.db` that `when` names fail until
    /// [`heal`](Self::heal), as a full disk would: a trigger on the
    /// writer's own connection.
    fn break_writes(&self, when: &str) {
        let sql = format!(
            "CREATE TEMP TRIGGER injected {when} BEGIN SELECT RAISE(ABORT, 'injected failure'); END;"
        );
        self.server
            .transact(move |transaction, _, _| Ok(transaction.execute_batch(&sql)?))
            .unwrap();
    }

    fn heal(&self) {
        self.server
            .transact(|transaction, _, _| {
                Ok(transaction.execute_batch("DROP TRIGGER temp.injected")?)
            })
            .unwrap();
    }

    /// The server restarts: a new run, from what `server.db` holds, on the
    /// same boot of the machine unless the test reboots the timekeeper. A
    /// crash is a restart with nothing closed first.
    fn restart(&mut self, run: u8) -> Vec<Id> {
        self.run = id(run);
        self.fake = Fake::default();
        self.fake.clocked(self.keeper.clone());
        for track in &ALBUM {
            self.fake.track(
                format!("{ROOT}/{}", track.file),
                u64::try_from(track.length_ms).unwrap(),
            );
        }
        let (player, crashed) = Player::restore(
            setup(
                &self.identity,
                self.run,
                &self.clock,
                &self.keeper,
                &self.entropy,
            ),
            self.fake.renderer(),
            &self.server,
            Some(&self.library),
        )
        .unwrap();
        self.player = player;
        crashed
    }
}

#[test]
fn a_listen_of_40_percent_is_stored_and_does_not_count_and_one_of_60_percent_counts() {
    let mut rig = Rig::new();
    rig.play(0);
    // 40% of the first track, a 4:00 one, then next.
    rig.pass(96_000);
    rig.skip(Direction::Next);
    // 60% of the second, a 5:00 one, then next.
    rig.pass(180_000);
    rig.skip(Direction::Next);
    rig.pause();
    let delivered = rig.deliver();
    assert_eq!(delivered.delivered.len(), 2);
    let plays = rig.plays();
    assert_eq!(plays.len(), 2);
    assert_eq!(
        (
            plays[0].ms_played,
            plays[0].counts,
            plays[0].outcome.as_str()
        ),
        (Some(96_000), false, "skipped")
    );
    assert_eq!(
        (
            plays[1].ms_played,
            plays[1].counts,
            plays[1].outcome.as_str()
        ),
        (Some(180_000), true, "listened")
    );
    assert_eq!(plays[0].title.as_deref(), Some("Paper Kites"));
    assert_eq!(plays[0].artist.as_deref(), Some("Moss Arcade"));
    assert_eq!(plays[0].album.as_deref(), Some("Tin Weather"));
    // Delivered, the rows in server.db are gone.
    assert!(rig.staged().iter().all(|staged| staged.ended.is_none()));
    assert_eq!(rig.server.undelivered_listens().unwrap(), 0);
}

#[test]
fn a_track_of_30_seconds_or_less_played_whole_does_not_count_and_the_album_plays_on() {
    let mut rig = Rig::new();
    rig.play(2);
    rig.pass(25_000);
    // The interlude ended and the next track began on its own.
    rig.pass(1_000);
    let staged = rig.staged();
    assert_eq!(staged.len(), 2);
    let ended = staged[0].ended.as_ref().unwrap();
    assert_eq!(ended.end_reason.as_str(), "finished");
    assert_eq!(staged[0].heard.ms_played, 25_000);
    assert_eq!(staged[1].started.start_reason.as_str(), "auto_advance");
    rig.deliver();
    let plays = rig.plays();
    assert_eq!(
        (plays[0].counts, plays[0].outcome.as_str()),
        (false, "partial")
    );
}

#[test]
fn the_recording_example_pause_seek_and_next_with_its_checkpoints_and_its_crash() {
    // Recording a 4:00 track queued from an album, with checkpoints every 5 s,
    // a pause, a seek, and a skip. Times below are minutes after 21:00:00.
    let mut rig = Rig::new();
    rig.play(0);
    // Written once its first frame is heard, dated then, with what was
    // heard by the time the player took it in.
    rig.pass(1_000);
    let [first] = rig.staged().try_into().unwrap();
    assert_eq!(first.heard.ms_played, 1_000);
    assert_eq!(first.started.started_at_ms, T0);
    assert_eq!(first.started.context_kind.as_str(), "album");
    assert_eq!(first.started.context_ref, Some(id(0xa1)));
    assert_eq!(first.started.zone.as_deref(), Some("America/New_York"));
    // The machine's boot, which its monotonic clock counts from.
    assert_eq!(first.started.boot, MACHINE_BOOT);
    assert_eq!(first.started.played_on_server, rig.identity.server_id);
    assert_eq!(first.started.start_reason.as_str(), "user_pick");
    // Checkpoints every five seconds from the first frame.
    rig.pass(11_000);
    assert_eq!(rig.staged()[0].heard.ms_played, 10_000);
    // 21:00:40 pause.
    rig.pass(28_000);
    rig.pause();
    let paused = &rig.staged()[0];
    assert_eq!(
        (paused.heard.ms_played, paused.heard.pause_count),
        (40_000, 1)
    );
    // Five paused minutes add nothing.
    rig.wait(300_000);
    rig.resume();
    assert_eq!(rig.staged()[0].heard.ms_played, 40_000);
    // 21:06:00: twenty seconds later, seek to 3:00.
    rig.pass(20_000);
    rig.seek(180_000);
    let sought = &rig.staged()[0];
    assert_eq!(
        (sought.heard.ms_played, sought.heard.seek_count),
        (60_000, 1)
    );
    // Had the server died at 21:06:11, the row would hold the checkpoint of
    // 21:06:10: 70,000.
    rig.pass(11_000);
    assert_eq!(rig.staged()[0].heard.ms_played, 70_000);
    assert_eq!(rig.session_position(), 190_000);
    // 21:06:12 next.
    rig.pass(1_000);
    rig.skip(Direction::Next);
    let done = &rig.staged()[0];
    let ended = done.ended.as_ref().unwrap();
    assert_eq!(done.heard.ms_played, 72_000);
    assert_eq!(done.heard.max_position_ms, 192_000);
    assert_eq!(ended.end_reason.as_str(), "next");
    assert_eq!(ended.end_position_ms, 192_000);
    assert_eq!(done.heard.ranges_json, "[[0,60000],[180000,192000]]");
}

#[test]
fn a_crash_at_2_40_with_a_checkpoint_at_2_35_is_closed_as_a_crash_and_resumed_as_one_play() {
    // A 5:00 track crashes at 2:40 after a 2:35 checkpoint and resumes as one
    // play.
    let mut rig = Rig::new();
    rig.play(1);
    rig.pass(159_000);
    let started_at = rig.staged()[0].started.started_at_ms;
    assert_eq!(started_at, T0);
    assert_eq!(rig.staged()[0].heard.ms_played, 155_000);
    // The server dies at 2:40, a second later, before its next
    // checkpoint. Nothing is closed.
    rig.fake.advance(1_000);
    let crashed_at = rig.clock.now_ms() + 1_000;
    rig.clock.set(crashed_at + 30_000);
    rig.hub_clock.set(crashed_at + 30_000);
    // The machine's clock ran on through the restart.
    rig.keeper.advance(31_000);
    let crashed = rig.restart(0xb1);
    assert_eq!(crashed.len(), 1);
    // The queue and the position are back, and nothing plays.
    let state = rig.player.state();
    assert_eq!(state.status, Status::Paused);
    assert_eq!(state.current.as_ref().unwrap().index, 1);
    assert_eq!(state.position_ms, 155_000);
    assert!(!rig.fake.playing());
    // Listen 1 is closed as a crash with what its last checkpoint saved:
    // five seconds at most are lost.
    let closed = &rig.staged()[0];
    let ended = closed.ended.as_ref().unwrap();
    assert_eq!(ended.end_reason.as_str(), "crash");
    assert_eq!(closed.heard.ms_played, 155_000);
    assert_eq!(ended.end_position_ms, 155_000);
    // It ended, as far as anything says, at its last checkpoint.
    assert_eq!(ended.ended_at_ms, T0 + 155_000);
    // The next run delivers it.
    assert_eq!(rig.deliver().delivered, [closed.started.listen_id]);

    // The track resumes and finishes: listen 2 is 145 seconds and
    // continues listen 1, even though listen 1 has been delivered.
    rig.resume();
    rig.pass(1_000);
    let resumed = rig.staged()[0].clone();
    assert_eq!(resumed.started.start_reason.as_str(), "resume");
    assert_eq!(resumed.started.continues, Some(closed.started.listen_id));
    assert_eq!(resumed.started.start_position_ms, 155_000);
    assert_eq!(rig.fake.loads().last().unwrap().1, 155_000);
    rig.pass(144_000);
    rig.deliver();
    let plays = rig.plays();
    let play = plays
        .iter()
        .find(|play| play.listens.len() == 2)
        .expect("one play with both listens");
    assert_eq!(play.ms_played, Some(300_000));
    assert!(play.counts);
    assert_eq!(play.at_ms, Some(started_at));
    assert_eq!(play.listens[0].end_reason, "crash");
    assert_eq!(play.listens[1].start_reason, "resume");
    // The next track began on its own: a play of its own, not this one's.
    assert!(
        plays
            .iter()
            .all(|other| other.play_id == play.play_id || other.listens.len() == 1)
    );
}

#[test]
fn a_clean_shutdown_closes_the_listen_as_shutdown_and_a_resume_continues_it() {
    let mut rig = Rig::new();
    rig.play(0);
    rig.pass(30_000);
    rig.player.shutdown(&rig.server).unwrap();
    let [closed] = rig.staged().try_into().unwrap();
    assert_eq!(
        closed.ended.as_ref().unwrap().end_reason.as_str(),
        "shutdown"
    );
    assert_eq!(closed.heard.ms_played, 30_000);
    assert!(rig.restart(0xb1).is_empty());
    assert_eq!(rig.player.state().position_ms, 30_000);
    rig.resume();
    rig.pass(1_000);
    let resumed = &rig.staged()[1];
    assert_eq!(resumed.started.continues, Some(closed.started.listen_id));
}

#[test]
fn three_passes_by_hand_are_three_listens_and_three_plays() {
    let mut rig = Rig::new();
    let only = rig.tracks[0];
    rig.tracks = vec![only];
    rig.play(0);
    rig.pass(240_000);
    rig.pass(1_000);
    assert_eq!(rig.player.state().status, Status::Stopped);
    for _ in 0..2 {
        rig.resume();
        rig.pass(240_000);
        rig.pass(1_000);
    }
    let staged = rig.staged();
    assert_eq!(staged.len(), 3);
    assert!(
        staged
            .iter()
            .all(|listen| listen.ended.as_ref().unwrap().end_reason.as_str() == "finished")
    );
    rig.deliver();
    let plays = rig.plays();
    assert_eq!(plays.len(), 3);
    assert!(plays.iter().all(|play| play.counts));
}

#[test]
fn next_and_previous_end_a_listen_and_previous_restarts_after_three_seconds() {
    let mut rig = Rig::new();
    rig.play(1);
    rig.pass(10_000);
    // Ten seconds in: previous starts the track again, inside the listen.
    rig.skip(Direction::Previous);
    assert_eq!(rig.player.state().current.unwrap().index, 1);
    rig.pass(2_000);
    // Two seconds in: previous goes back an entry.
    rig.skip(Direction::Previous);
    assert_eq!(rig.player.state().current.unwrap().index, 0);
    rig.pass(5_000);
    rig.skip(Direction::Next);
    rig.pause();
    let staged = rig.staged();
    let reasons: Vec<&str> = staged
        .iter()
        .filter_map(|listen| listen.ended.as_ref())
        .map(|ended| ended.end_reason.as_str())
        .collect();
    assert_eq!(reasons, ["previous", "next"]);
    // The restart was a seek back inside the first listen.
    assert_eq!(staged[0].heard.seek_count, 1);
    assert_eq!(staged[0].heard.ms_played, 12_000);
    assert_eq!(staged[0].heard.ranges_json, "[[0,10000]]");
    // A skip that names an entry the player has left changes nothing.
    let request = rig.request("playback.skip");
    let gone = staged[0].started.appearance;
    let before = rig.player.state();
    rig.player
        .skip(
            request,
            gone,
            Direction::Next,
            &rig.server,
            Some(&rig.library),
        )
        .unwrap();
    assert_eq!(rig.player.state().current, before.current);
}

#[test]
fn a_command_sent_again_is_answered_from_its_receipt_and_does_nothing() {
    let mut rig = Rig::new();
    rig.play(0);
    rig.pass(5_000);
    let request = rig.request("playback.skip");
    let (command, entry) = (request.command, rig.entry());
    let first = rig
        .player
        .skip(
            request,
            entry,
            Direction::Next,
            &rig.server,
            Some(&rig.library),
        )
        .unwrap();
    assert!(!first.replayed);
    let index = rig.player.state().current.unwrap().index;
    let again = Request {
        command,
        method: "playback.skip",
        answer: Box::new(|_| "not this".to_owned()),
    };
    let second = rig
        .player
        .skip(
            again,
            entry,
            Direction::Next,
            &rig.server,
            Some(&rig.library),
        )
        .unwrap();
    assert!(second.replayed);
    assert_eq!(second.answer, first.answer);
    assert_eq!(rig.player.state().current.unwrap().index, index);
}

#[test]
fn an_entry_none_of_whose_copies_opens_is_marked_failed_and_three_in_a_row_stop_the_player() {
    let mut rig = Rig::new();
    for track in &ALBUM[..3] {
        rig.fake
            .broken(format!("{ROOT}/{}", track.file), "unsupported_format");
    }
    rig.play(0);
    let queue = rig.queue();
    let failed: Vec<Option<String>> = queue.iter().map(|(_, _, failed)| failed.clone()).collect();
    assert_eq!(
        failed,
        [
            Some("unsupported_format".to_owned()),
            Some("unsupported_format".to_owned()),
            Some("unsupported_format".to_owned()),
            None
        ]
    );
    assert_eq!(rig.player.state().status, Status::Stopped);
    // No listen is written for a track that was never heard.
    assert!(rig.staged().is_empty());

    let mut rig = Rig::new();
    rig.fake
        .broken(format!("{ROOT}/{}", ALBUM[0].file), "unsupported_format");
    rig.play(0);
    assert_eq!(rig.player.state().status, Status::Playing);
    assert_eq!(rig.player.state().current.unwrap().index, 1);
}

#[test]
fn a_sender_whose_wall_clock_is_an_hour_fast_still_dates_the_play_by_its_age() {
    let mut rig = Rig::new();
    rig.clock.set(T0 + 3_600_000);
    rig.play(0);
    rig.pass(240_000);
    rig.pause();
    let staged = rig.staged();
    assert_eq!(staged[0].started.started_at_ms, T0 + 3_600_000);
    rig.deliver();
    let play = &rig.plays()[0];
    // The hub's clock, less the listen's age: T0.
    assert_eq!(play.at_ms, Some(T0));
    assert_eq!(play.clock_basis, "hub_age");
    assert_eq!(play.local_hour, Some(16));
    assert_eq!(play.zone.as_deref(), Some("America/New_York"));
}

#[test]
fn a_listen_delivered_twice_is_written_once() {
    let mut rig = Rig::new();
    rig.play(0);
    rig.pass(200_000);
    rig.pause();
    rig.skip(Direction::Next);
    // The hub takes it, and the server dies before it removes its rows.
    let staged = rig.staged()[0].clone();
    let stamps = rig.stamps();
    let cut = deliver_listens(&rig.server, &rig.library, &*rig.clock, &|| stamps, &|| {
        Err(crate::Error::FailPoint)
    });
    assert!(matches!(cut, Err(crate::Error::FailPoint)), "{cut:?}");
    assert!(
        rig.server
            .staged_listen(staged.started.listen_id)
            .unwrap()
            .is_some()
    );
    let revision = rig.library.meta().unwrap().revision;
    // The next run sends it again, stamped afresh.
    rig.clock.set(rig.clock.now_ms() + 60_000);
    let again = rig.deliver();
    assert_eq!((again.delivered.len(), again.duplicates), (1, 1));
    assert_eq!(rig.library.meta().unwrap().revision, revision);
    assert_eq!(rig.plays().len(), 1);
    let observations: i64 = rig
        .library
        .read(|connection| {
            Ok(
                connection.query_row("SELECT count(*) FROM history_observation", [], |row| {
                    row.get(0)
                })?,
            )
        })
        .unwrap();
    assert_eq!(observations, 1);
}

#[test]
fn the_zone_is_captured_at_the_start_of_each_listen() {
    let mut rig = Rig::new();
    rig.keeper.set_zone(Some("Europe/Lisbon"));
    rig.play(0);
    rig.pass(10_000);
    // The zone is the one the server is in when a listen begins.
    rig.keeper.set_zone(None);
    rig.skip(Direction::Next);
    rig.pass(1_000);
    let staged = rig.staged();
    assert_eq!(staged[0].started.zone.as_deref(), Some("Europe/Lisbon"));
    assert_eq!(staged[1].started.zone, None);
    let request = rig.request("playback.stop");
    rig.player.stop(request, &rig.server).unwrap();
    rig.deliver();
    let plays = rig.plays();
    assert_eq!(plays[0].zone.as_deref(), Some("Europe/Lisbon"));
    // 21:00 UTC is 21:00 in Lisbon in November.
    assert_eq!(plays[0].local_hour, Some(21));
    assert_eq!(plays[1].zone, None);
    assert_eq!(plays[1].local_date, None);
    let zone: Option<String> = rig
        .library
        .read(|connection| {
            Ok(connection
                .query_row(
                    "SELECT zone_basis FROM history_observation ORDER BY id LIMIT 1",
                    [],
                    |row| row.get(0),
                )
                .optional()?)
        })
        .unwrap();
    assert_eq!(zone.as_deref(), Some("captured"));
}

#[test]
fn stop_ends_the_listen_and_play_again_starts_the_entry_from_its_beginning() {
    let mut rig = Rig::new();
    rig.play(0);
    rig.pass(20_000);
    let request = rig.request("playback.stop");
    rig.player.stop(request, &rig.server).unwrap();
    assert_eq!(rig.player.state().status, Status::Stopped);
    assert_eq!(rig.player.state().position_ms, 0);
    rig.resume();
    rig.pass(1_000);
    let staged = rig.staged();
    assert_eq!(
        staged[0].ended.as_ref().unwrap().end_reason.as_str(),
        "stopped"
    );
    assert_eq!(staged[1].started.start_reason.as_str(), "user_pick");
    assert_eq!(staged[1].started.start_position_ms, 0);
    assert_eq!(staged[1].started.continues, None);
}

#[test]
fn replacing_the_queue_ends_the_open_listen_as_replaced() {
    let mut rig = Rig::new();
    rig.play(0);
    rig.pass(20_000);
    rig.play(3);
    let staged = rig.staged();
    assert_eq!(
        staged[0].ended.as_ref().unwrap().end_reason.as_str(),
        "replaced"
    );
    assert_eq!(rig.player.state().current.unwrap().index, 3);
    assert_eq!(rig.player.state().queue_revision, 2);
    let _ = rig.items.len();
}

/// Kills the server two minutes and forty seconds into the five-minute
/// track, between checkpoints, with the player's wall clock an hour fast,
/// and starts the next run 30 seconds later. Returns the crashed listen's
/// play once the next run has delivered it.
fn crash_and_deliver(rig: &mut Rig, before_restart: impl FnOnce(&Rig)) -> crate::PlayRow {
    rig.clock.set(T0 + 3_600_000);
    rig.play(1);
    rig.pass(159_000);
    rig.fake.advance(1_000);
    for clock in [&rig.clock, &rig.hub_clock] {
        clock.set(clock.now_ms() + 31_000);
    }
    rig.keeper.advance(31_000);
    before_restart(rig);
    assert_eq!(rig.restart(0xb1).len(), 1);
    assert_eq!(rig.deliver().delivered.len(), 1);
    let [play] = rig.plays().try_into().unwrap();
    assert_eq!(play.listens[0].end_reason, "crash");
    play
}

#[test]
fn a_listen_closed_by_a_crash_and_sent_by_the_next_run_on_the_same_boot_is_dated_by_its_age() {
    let mut rig = Rig::new();
    let play = crash_and_deliver(&mut rig, |_| {});
    // The hub's clock less the listen's age, whatever the sender's wall
    // clock said: T0, 16:00 in New York.
    assert_eq!(play.clock_basis, "hub_age");
    assert_eq!(play.at_ms, Some(T0));
    assert_eq!(play.local_hour, Some(16));
}

#[test]
fn across_a_reboot_of_the_machine_the_crashed_listen_falls_back_to_the_wall_clock_and_skew() {
    let mut rig = Rig::new();
    let play = crash_and_deliver(&mut rig, |rig| {
        rig.keeper.reboot(Some(Id::from_bytes([0xbb; 16])), 9_000);
    });
    assert_eq!(play.clock_basis, "server_wall");
    // The wall clock was an hour fast at the send too, and the skew
    // measured then is taken off.
    assert_eq!(play.at_ms, Some(T0));
}

#[test]
fn where_the_machines_boot_cannot_be_read_the_run_stands_in_and_a_restart_breaks_the_age() {
    let mut rig = Rig::new();
    rig.keeper.reboot(None, 1_000_000);
    let play = crash_and_deliver(&mut rig, |rig| {
        // The crashed listen was stamped with the run it was heard in.
        let [staged] = rig.staged().try_into().unwrap();
        assert_eq!(staged.started.boot, id(0xb0));
    });
    assert_eq!(play.clock_basis, "server_wall");
    assert_eq!(play.at_ms, Some(T0));
    // In one run the run is as good as the machine's boot.
    rig.play(0);
    rig.pass(240_000);
    rig.pause();
    rig.skip(Direction::Next);
    rig.deliver();
    let newest = &rig.plays()[1];
    assert_eq!(newest.clock_basis, "hub_age");
}

#[test]
fn a_file_replaced_since_the_scan_is_not_played_and_its_entry_says_why() {
    let mut rig = Rig::new();
    let first = format!("{ROOT}/{}", ALBUM[0].file);
    rig.fake.replace(
        &first,
        crate::playback::renderer::FileFacts {
            size_bytes: 1234,
            mtime_ms: 99,
        },
    );
    rig.play(0);
    // The first entry is marked, nothing was staged for it, and the second
    // plays.
    let queue = rig.queue();
    assert_eq!(queue[0].2.as_deref(), Some("file_changed"));
    assert_eq!(rig.player.state().current.unwrap().index, 1);
    rig.pass(10_000);
    let staged = rig.staged();
    assert_eq!(staged.len(), 1);
    assert_eq!(staged[0].started.appearance, rig.tracks[1]);
}

#[test]
fn a_file_written_to_while_it_plays_ends_its_listen_at_the_next_seek_and_the_player_moves_on() {
    let mut rig = Rig::new();
    rig.play(0);
    rig.pass(20_000);
    rig.fake.rewrite_loaded();
    rig.seek(100_000);
    // What it played is kept, under the file that played it, and closed as
    // an error; the entry says why and the next one plays.
    let staged = rig.staged();
    let ended = staged[0].ended.clone().unwrap();
    assert_eq!(
        (
            ended.end_reason,
            ended.end_position_ms,
            staged[0].heard.ms_played
        ),
        (ritornel_domain::history::EndReason::Error, 20_000, 20_000)
    );
    assert_eq!(rig.queue()[0].2.as_deref(), Some("file_changed"));
    assert_eq!(rig.player.state().current.unwrap().index, 1);
    assert_eq!(rig.player.state().status, Status::Playing);
}

#[test]
fn a_close_that_cannot_be_saved_is_kept_whole_and_the_next_save_closes_it_as_it_ended() {
    let mut rig = Rig::new();
    rig.play(0);
    rig.pass(20_000);
    let stopped_at = rig.clock.now_ms();
    rig.break_writes("BEFORE UPDATE OF end_reason ON listen_staging");
    // The stop is carried out; only the save of its listen waits.
    let request = rig.request("playback.stop");
    rig.player.stop(request, &rig.server).unwrap();
    assert!(rig.staged()[0].ended.is_none());
    rig.heal();
    rig.wait(3_000);

    // Closed as it ended, when it ended, and on its way to the hub.
    let [first] = rig.staged().try_into().unwrap();
    let ended = first.ended.unwrap();
    assert_eq!(
        (ended.end_reason, ended.ended_at_ms, first.heard.ms_played),
        (
            ritornel_domain::history::EndReason::Stopped,
            stopped_at,
            20_000
        )
    );
    assert_eq!(rig.server.undelivered_listens().unwrap(), 1);

    // Another track, a shutdown and a restart: the resume continues that
    // track's listen, and nothing was left open to close as a crash.
    rig.play(1);
    rig.pass(10_000);
    rig.player.shutdown(&rig.server).unwrap();
    let second = rig.staged()[1].started.listen_id;
    assert!(rig.restart(0xb1).is_empty());
    assert_eq!(
        rig.server.playback_session().unwrap().resume_listen,
        Some(second)
    );
}

/// A second of time on every clock, and a tick. Whether its save failed.
fn second(rig: &mut Rig) -> bool {
    rig.fake.advance(1000);
    rig.clock.set(rig.clock.now_ms() + 1000);
    rig.hub_clock.set(rig.hub_clock.now_ms() + 1000);
    rig.keeper.advance(1000);
    rig.player.tick(&rig.server, Some(&rig.library)).unwrap();
    rig.player.save_failing().is_some()
}

#[test]
fn frames_heard_while_the_listen_cannot_be_written_are_kept_with_its_start() {
    let mut rig = Rig::new();
    rig.break_writes("BEFORE INSERT ON listen_staging");
    rig.play(0);
    // A second is heard and the listen cannot be written; then it can.
    assert!(second(&mut rig));
    rig.heal();
    assert!(!second(&mut rig));
    let request = rig.request("playback.stop");
    rig.player.stop(request, &rig.server).unwrap();
    // Two seconds were heard, from when the track began.
    let [listen] = rig.staged().try_into().unwrap();
    assert_eq!(
        (listen.started.started_at_ms, listen.heard.ms_played),
        (T0, 2_000)
    );
    assert_eq!(listen.heard.ranges_json, "[[0,2000]]");
}

#[test]
fn a_track_left_while_its_listen_cannot_be_written_keeps_what_it_played() {
    let mut rig = Rig::new();
    rig.break_writes("BEFORE INSERT ON listen_staging");
    rig.play(0);
    assert!(second(&mut rig));
    // Stopped before the listen could be written: what was heard is kept
    // for the next save.
    let request = rig.request("playback.stop");
    rig.player.stop(request, &rig.server).unwrap();
    assert!(rig.staged().is_empty());
    rig.heal();
    rig.wait(50);
    let [listen] = rig.staged().try_into().unwrap();
    assert_eq!(
        (listen.started.started_at_ms, listen.heard.ms_played),
        (T0, 1_000)
    );
    assert!(listen.ended.is_some());
}

#[test]
fn a_crash_after_a_seek_back_closes_the_listen_where_its_last_checkpoint_was() {
    let mut rig = Rig::new();
    rig.play(0);
    rig.pass(100_000);
    rig.player.shutdown(&rig.server).unwrap();
    rig.restart(0xb1);
    // Resumed at 1:40, a second heard, back to the start, ten seconds
    // heard, and a crash.
    rig.resume();
    rig.pass(1_000);
    rig.seek(0);
    rig.pass(10_000);
    rig.clock.set(rig.clock.now_ms() + 2_000);
    rig.restart(0xb2);
    let resumed = rig.staged().pop().unwrap();
    assert_eq!(resumed.started.start_position_ms, 100_000);
    let ended = resumed.ended.unwrap();
    assert_eq!(
        (ended.end_reason, ended.end_position_ms),
        (ritornel_domain::history::EndReason::Crash, 10_000)
    );
}

#[test]
fn whether_the_heard_ranges_are_approximate_is_a_fact_kept_through_staging_and_delivery() {
    use super::listen::{Listen, MAX_RANGES};
    use super::store::{Ended, Saved};
    let cap = i64::try_from(MAX_RANGES).unwrap();
    // One range past the cap, then all of them heard again as one; and
    // exactly as many ranges as the cap, none joined.
    for (ranges, heard_whole, approximate) in [(cap + 1, true, true), (cap, false, false)] {
        let mut rig = Rig::new();
        rig.play(0);
        rig.pass(1_000);
        let started = rig.staged()[0].started.clone();
        let mut listen = Listen::new(started.clone(), super::renderer::fake::RATE);
        for at in 0..ranges {
            listen.seek(at * 30);
            listen.heard(10);
        }
        if heard_whole {
            listen.seek(0);
            listen.heard(200_000);
        }
        let ended = Ended {
            end_position_ms: listen.position_ms(0),
            end_reason: ritornel_domain::history::EndReason::Stopped,
            ended_at_ms: rig.clock.now_ms(),
            ended_mono_ms: Some(2_000_000),
        };
        let saved = Saved {
            started,
            heard: listen.snapshot(0),
            position_ms: listen.position_ms(0),
            ended: Some(ended),
        };
        rig.server
            .save_listens(vec![saved], (None, 0, None), 2_000_000)
            .unwrap();
        assert_eq!(rig.deliver().delivered.len(), 1);
        let (flag, count): (bool, i64) = rig
            .library
            .read(|connection| {
                Ok(connection.query_row(
                    "SELECT heard_ranges_approximate, json_array_length(heard_ranges_json)
                     FROM observation_listen",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )?)
            })
            .unwrap();
        let expected_count = if heard_whole { 1 } else { cap };
        assert_eq!(
            (flag, count),
            (approximate, expected_count),
            "{ranges} ranges"
        );
    }
}

#[test]
fn an_output_that_goes_away_ends_the_listen_as_output_lost_and_play_continues_it() {
    let mut rig = Rig::new();
    rig.play(0);
    rig.pass(30_000);
    rig.fake.lose_output();
    rig.wait(1_000);
    // The listen ended where the output went, and the player waits, paused
    // at that place.
    let [lost] = rig.staged().try_into().unwrap();
    let ended = lost.ended.clone().unwrap();
    assert_eq!(
        (
            ended.end_reason,
            ended.end_position_ms,
            lost.heard.ms_played
        ),
        (
            ritornel_domain::history::EndReason::OutputLost,
            30_000,
            30_000
        )
    );
    // The output is let go, and with it its rate: the place is saved where
    // the track was heard to, not at its end.
    let state = rig.player.state();
    assert_eq!((state.status, state.position_ms), (Status::Paused, 30_000));
    assert_eq!(rig.session_position(), 30_000);
    // Play opens the output again, from there, as a listen that continues
    // the lost one: one play.
    rig.resume();
    assert_eq!(rig.fake.loads().last().unwrap().1, 30_000);
    rig.pass(120_000);
    let staged = rig.staged();
    assert_eq!(staged[1].started.continues, Some(lost.started.listen_id));
    rig.skip(Direction::Next);
    rig.deliver();
    let plays = rig.plays();
    assert_eq!(plays[0].ms_played, Some(150_000));
    assert_eq!(plays[0].listens.len(), 2);
}

fn file(track: usize) -> std::path::PathBuf {
    format!("{ROOT}/{}", ALBUM[track].file).into()
}

#[test]
fn an_album_plays_gaplessly_and_each_track_is_a_listen_of_its_own_with_nothing_between() {
    let mut rig = Rig::new();
    rig.fake.gapless();
    rig.play(0);
    // The second track is opened as the first starts.
    assert_eq!(rig.fake.queued(), Some(file(1)));
    // The first ends half way through a step: the output goes straight on
    // into the second, which was never loaded on its own.
    rig.pass(239_500);
    rig.pass(1_000);
    assert_eq!(rig.fake.loads(), [(file(0), 0)]);
    let state = rig.player.state();
    assert_eq!(state.current.as_ref().unwrap().index, 1);
    assert_eq!((state.status, state.position_ms), (Status::Playing, 500));
    let [first, second] = rig.staged().try_into().unwrap();
    let ended = first.ended.as_ref().unwrap();
    assert_eq!(
        (ended.end_reason.as_str(), first.heard.ms_played),
        ("finished", 240_000)
    );
    // The second began the instant the first ended.
    assert_eq!(ended.ended_at_ms, T0 + 240_000);
    assert_eq!(second.started.started_at_ms, T0 + 240_000);
    assert_eq!(second.started.start_reason.as_str(), "auto_advance");
    assert_eq!(second.started.start_position_ms, 0);
    // And so on to the end of the album, which stops the player.
    rig.pass(300_000 + 25_000 + 200_000);
    let staged = rig.staged();
    assert_eq!(staged.len(), 4);
    assert_eq!(
        staged
            .iter()
            .map(|staged| staged.heard.ms_played)
            .collect::<Vec<_>>(),
        [240_000, 300_000, 25_000, 200_000]
    );
    assert_eq!(rig.fake.loads().len(), 1);
    assert_eq!(rig.player.state().status, Status::Stopped);
}

#[test]
fn a_track_is_said_to_begin_when_it_is_first_heard_joined_to_and_resumed_but_not_on_a_seek() {
    let mut rig = Rig::new();
    rig.fake.gapless();
    rig.play(0);
    let first = (rig.tracks[0], ALBUM[0].length_ms);
    let second = (rig.tracks[1], ALBUM[1].length_ms);
    // Not when it is asked for, but when it is first heard.
    assert_eq!(rig.began, []);
    rig.pass(10_000);
    assert_eq!(rig.began, [first]);
    // A pause says nothing; a resume says it again.
    rig.pause();
    rig.wait(5_000);
    assert_eq!(rig.began, [first]);
    rig.resume();
    assert_eq!(rig.began, [first, first]);
    // A seek within the track is the same listen.
    rig.seek(100_000);
    rig.pass(1_000);
    assert_eq!(rig.began, [first, first]);
    // The output runs straight on into the next track.
    rig.pass(140_000);
    assert_eq!(rig.player.state().current.unwrap().index, 1);
    assert_eq!(rig.began, [first, first, second]);
    // A skip begins the one after, once it is heard.
    rig.skip(Direction::Next);
    rig.pass(1_000);
    assert_eq!(rig.began.len(), 4);
    assert_eq!(rig.began[3].0, rig.tracks[2]);
}

#[test]
fn a_jump_asked_for_within_the_entry_is_counted_and_playing_on_is_not() {
    let mut rig = Rig::new();
    rig.play(0);
    rig.pass(10_000);
    assert_eq!(rig.player.state().seeks, 0);
    // However small the jump.
    rig.seek(10_500);
    assert_eq!(rig.player.state().seeks, 1);
    // A seek that names an entry the player has left does nothing.
    let request = rig.request("playback.seek");
    let gone = id(0xee);
    rig.player
        .seek(request, gone, 1_000, &rig.server, Some(&rig.library))
        .unwrap();
    assert_eq!(rig.player.state().seeks, 1);
    // A previous that starts the track again is a jump; a skip is not.
    rig.pass(5_000);
    rig.skip(Direction::Previous);
    assert_eq!(rig.player.state().seeks, 2);
    rig.skip(Direction::Next);
    assert_eq!(rig.player.state().seeks, 2);
}

#[test]
fn an_album_plays_at_its_album_gain_and_a_mix_at_each_tracks_own() {
    let mut rig = Rig::new();
    rig.fake.gapless();
    rig.play(2);
    rig.pass(5_000);
    let album = gain_names(&rig.fake.gains());
    assert_eq!(album, [(file(2), "album"), (file(3), "album")]);
    // The same tracks shuffled into a mix, or queued by hand.
    for context in [ContextKind::Query, ContextKind::Manual] {
        let mut rig = Rig::new();
        rig.fake.gapless();
        rig.play_as(2, (context, None));
        rig.pass(5_000);
        let mix = gain_names(&rig.fake.gains());
        assert_eq!(mix, [(file(2), "track"), (file(3), "track")]);
    }
}

fn gain_names(
    gains: &[(std::path::PathBuf, super::renderer::ReplayGain)],
) -> Vec<(std::path::PathBuf, &'static str)> {
    use super::renderer::ReplayGain;
    gains
        .iter()
        .map(|(path, gain)| {
            let gain = match gain {
                ReplayGain::Off => "off",
                ReplayGain::Track => "track",
                ReplayGain::Album => "album",
            };
            (path.clone(), gain)
        })
        .collect()
}

#[test]
fn a_short_track_has_the_next_prepared_as_it_starts_and_not_a_tick_later() {
    let mut rig = Rig::new();
    rig.fake.gapless();
    let request = rig.request("playback.play");
    let what = PlayTracks {
        tracks: rig.tracks.clone(),
        start: Some(2),
        context: (ContextKind::Album, None),
        actor: None,
        shuffle: None,
        known: false,
    };
    rig.player
        .play(request, what, &rig.server, Some(&rig.library))
        .unwrap();
    assert_eq!(rig.fake.queued(), Some(file(3)));
}

#[test]
fn the_last_track_has_nothing_queued_after_it_and_ends_the_queue() {
    let mut rig = Rig::new();
    rig.fake.gapless();
    rig.play(3);
    assert_eq!(rig.fake.queued(), None);
    rig.pass(200_000);
    rig.pass(1_000);
    assert_eq!(rig.player.state().status, Status::Stopped);
    assert_eq!(rig.staged().len(), 1);
}

#[test]
fn a_seek_lets_the_prepared_track_go_and_it_is_prepared_again_at_once() {
    let mut rig = Rig::new();
    rig.fake.gapless();
    rig.play(0);
    rig.pass(215_000);
    assert_eq!(rig.fake.queued(), Some(file(1)));
    rig.seek(10_000);
    assert_eq!(rig.fake.queued(), Some(file(1)));
    let opened = |rig: &Rig| {
        let gains = rig.fake.gains();
        gains.iter().filter(|(path, _)| *path == file(1)).count()
    };
    assert_eq!(opened(&rig), 2);
    // A skip lets it go too, loads the next entry, and prepares the one
    // after it.
    rig.skip(Direction::Next);
    assert_eq!(rig.fake.loads().last(), Some(&(file(1), 0)));
    assert_eq!(rig.fake.queued(), Some(file(2)));
}

#[test]
fn a_track_the_output_cannot_join_plays_after_the_one_before_as_it_always_did() {
    // The fake joins nothing unless asked, as an output that cannot.
    let mut rig = Rig::new();
    rig.play(0);
    rig.pass(240_000);
    rig.pass(1_000);
    assert_eq!(rig.fake.loads(), [(file(0), 0), (file(1), 0)]);
    assert_eq!(rig.staged().len(), 2);
}

#[test]
fn the_volume_is_set_kept_and_given_to_the_output_again_after_a_restart() {
    let mut rig = Rig::new();
    let request = rig.request("playback.set");
    rig.player
        .set_volume(request, Some(0.4), None, &rig.server)
        .unwrap();
    assert_eq!(rig.fake.volume(), Some(0.4));
    let state = rig.player.state();
    assert_eq!((state.volume, state.muted), (0.4, false));
    // Muted, the output is given nothing, and the volume is kept.
    let request = rig.request("playback.set");
    rig.player
        .set_volume(request, None, Some(true), &rig.server)
        .unwrap();
    assert_eq!(rig.fake.volume(), Some(0.0));
    assert_eq!(rig.player.state().volume, 0.4);
    // Out of range is refused, and changes nothing.
    let request = rig.request("playback.set");
    assert!(matches!(
        rig.player.set_volume(request, Some(1.5), None, &rig.server),
        Err(crate::Error::Invalid(_))
    ));
    // A restart gives the output the volume it had.
    let fake = Fake::default();
    let (player, _) = Player::restore(
        setup(
            &rig.identity,
            rig.run,
            &rig.clock,
            &rig.keeper,
            &rig.entropy,
        ),
        fake.renderer(),
        &rig.server,
        Some(&rig.library),
    )
    .unwrap();
    let state = player.state();
    assert_eq!((state.volume, state.muted), (0.4, true));
    assert_eq!(fake.volume(), Some(0.0));
}

#[test]
fn a_join_taken_late_saves_what_the_next_track_has_played_so_a_crash_loses_no_more_than_five_seconds()
 {
    let mut rig = Rig::new();
    rig.fake.gapless();
    rig.play(0);
    rig.pass(239_500);
    // The control thread stalls but the output keeps playing, crossing the
    // boundary and handing out eight seconds of the next track.
    rig.fake.advance(8_500);
    rig.clock.set(rig.clock.now_ms() + 8_500);
    rig.keeper.advance(8_500);
    rig.player.tick(&rig.server, Some(&rig.library)).unwrap();
    assert_eq!(rig.player.state().position_ms, 8_000);
    let [first, next] = rig.staged().try_into().unwrap();
    assert_eq!(first.ended.unwrap().ended_at_ms, T0 + 240_000);
    assert_eq!(next.started.started_at_ms, T0 + 240_000);
    rig.restart(0xb1);
    let [_, next] = rig.staged().try_into().unwrap();
    assert_eq!(next.ended.unwrap().end_reason.as_str(), "crash");
    assert!(
        8_000 - next.heard.ms_played <= 5_000,
        "heard 8000 ms but crash preserved only {} ms",
        next.heard.ms_played
    );
}

#[test]
fn pauses_and_seeks_after_a_join_checkpoint_what_was_heard() {
    let mut rig = Rig::new();
    rig.fake.gapless();
    rig.play(0);
    rig.pass(240_500);
    rig.pass(2_000);
    rig.pause();
    let [_, next] = rig.staged().try_into().unwrap();
    assert_eq!(next.heard.ms_played, 2_500);
    rig.wait(60_000);
    rig.resume();
    rig.pass(2_000);
    rig.seek(100_000);
    let [_, next] = rig.staged().try_into().unwrap();
    assert_eq!(next.heard.ms_played, 4_500);
    assert_eq!(next.heard.seek_count, 1);
    rig.pass(4_900);
    rig.restart(0xb1);
    let [_, next] = rig.staged().try_into().unwrap();
    assert!(9_400 - next.heard.ms_played <= 5_000);
}

#[test]
fn a_pause_and_resume_before_the_join_is_taken_lose_no_more_than_five_seconds_to_a_crash() {
    let mut rig = Rig::new();
    rig.fake.gapless();
    rig.play(0);
    rig.pass(239_500);
    rig.fake.advance(1_000);
    rig.clock.set(rig.clock.now_ms() + 1_000);
    rig.keeper.advance(1_000);
    rig.pause();
    rig.resume();
    rig.pass(4_600);
    assert_eq!(rig.player.state().position_ms, 5_100);
    rig.restart(0xb1);
    let [_, next] = rig.staged().try_into().unwrap();
    assert!(
        5_100 - next.heard.ms_played <= 5_000,
        "heard 5100 ms around pause/resume but crash preserved only {} ms",
        next.heard.ms_played
    );
}

#[test]
fn a_pause_at_the_very_end_of_a_track_starts_the_next_listen_when_it_is_heard() {
    let mut rig = Rig::new();
    rig.fake.gapless();
    rig.play(0);
    rig.pass(239_000);
    // The first ends exactly as this step does, and nothing of the next
    // is heard before the pause.
    rig.fake.advance(1_000);
    rig.clock.set(rig.clock.now_ms() + 1_000);
    rig.keeper.advance(1_000);
    rig.pause();
    rig.wait(2_000);
    rig.resume();
    rig.pass(1_000);
    let [first, next] = rig.staged().try_into().unwrap();
    assert_eq!(first.ended.unwrap().ended_at_ms, T0 + 240_000);
    assert_eq!(next.started.started_at_ms, T0 + 242_000);
    assert_eq!(rig.player.state().position_ms, 1_000);
}

// ---------------------------------------------------------------- the queue's orders and edits

impl Rig {
    fn set(&mut self, shuffle: Option<bool>, repeat: Option<super::Repeat>) {
        let request = self.request("playback.set");
        let settings = super::Settings {
            shuffle,
            repeat,
            ..super::Settings::default()
        };
        self.player.set(request, settings, &self.server).unwrap();
    }

    /// The entries in play order.
    fn play_order(&self) -> Vec<Id> {
        self.player
            .window(super::Order::Play, 0, usize::MAX)
            .entries
            .into_iter()
            .map(|(entry, ..)| entry)
            .collect()
    }

    fn add(
        &mut self,
        track: usize,
        place: super::Place,
        expected: Option<i64>,
    ) -> Result<super::Done, crate::Error> {
        let request = self.request("queue.add");
        let what = super::AddTracks {
            tracks: vec![self.tracks[track]],
            place,
            context: (ContextKind::Manual, None),
            actor: None,
            play_if_empty: false,
        };
        self.player
            .add(request, expected, what, &self.server, Some(&self.library))
    }

    fn appearance(&self) -> Id {
        self.player
            .state()
            .current
            .unwrap()
            .track
            .unwrap()
            .appearance
    }
}

#[test]
fn shuffle_is_a_stored_order_that_a_restart_keeps_and_previous_walks_back_through() {
    let mut rig = Rig::new();
    rig.play(1);
    let playing = rig.entry();
    rig.set(Some(true), None);
    let order = rig.play_order();
    assert_eq!(order.len(), 4);
    assert_eq!(order[0], playing, "the entry on plays first");
    assert!(rig.player.state().shuffle);
    // It plays on in that order.
    rig.pass(300_000);
    assert_eq!(rig.entry(), order[1]);
    rig.pass(2_000);
    // A restart keeps the order, paused.
    rig.player.shutdown(&rig.server).unwrap();
    rig.restart(0xb1);
    assert_eq!(rig.play_order(), order);
    let state = rig.player.state();
    assert!(state.shuffle);
    assert_eq!((state.status, rig.entry()), (Status::Paused, order[1]));
    // Previous goes back through what was heard.
    rig.resume();
    rig.skip(Direction::Previous);
    assert_eq!(rig.entry(), order[0]);
    // Each listen says whether shuffle chose it: the first was chosen by
    // hand before shuffle was on, and every one after by the order.
    let shuffled: Vec<bool> = rig.staged().iter().map(|l| l.started.shuffle).collect();
    assert!(!shuffled[0]);
    assert!(
        shuffled[1..].iter().all(|shuffled| *shuffled),
        "{shuffled:?}"
    );
    // Off goes back to queue order at the entry on.
    rig.set(Some(false), None);
    let state = rig.player.state();
    assert!(!state.shuffle);
    assert_eq!(rig.entry(), playing);
    assert_eq!(state.current.unwrap().index, 1);
}

#[test]
fn repeat_one_plays_the_entry_again_as_a_new_listen_and_next_still_moves_on() {
    let mut rig = Rig::new();
    rig.play(2);
    rig.set(None, Some(super::Repeat::One));
    assert_eq!(rig.player.state().repeat, super::Repeat::One);
    rig.pass(26_000);
    assert_eq!(rig.player.state().current.unwrap().index, 2);
    let staged = rig.staged();
    assert_eq!(staged.len(), 2);
    assert_eq!(
        staged[0].ended.as_ref().unwrap().end_reason.as_str(),
        "finished"
    );
    assert_eq!(staged[1].started.start_reason.as_str(), "repeat");
    rig.skip(Direction::Next);
    assert_eq!(rig.player.state().current.unwrap().index, 3);
}

#[test]
fn repeat_all_starts_again_at_the_end_and_with_shuffle_in_a_new_order() {
    let mut rig = Rig::new();
    rig.play(3);
    rig.set(None, Some(super::Repeat::All));
    rig.pass(201_000);
    let state = rig.player.state();
    assert_eq!(
        (state.status, state.current.unwrap().index),
        (Status::Playing, 0)
    );
    // Previous from the first goes round to the last.
    rig.skip(Direction::Previous);
    assert_eq!(rig.player.state().current.unwrap().index, 3);

    rig.set(Some(true), None);
    let before = rig.player.state().queue_revision;
    let last = *rig.play_order().last().unwrap();
    for _ in 0..3 {
        rig.skip(Direction::Next);
    }
    assert_eq!(rig.entry(), last);
    rig.skip(Direction::Next);
    let state = rig.player.state();
    assert_eq!(state.status, Status::Playing);
    assert_eq!(state.queue_revision, before + 1, "a new order was written");
    let order = rig.play_order();
    assert_eq!(rig.entry(), order[0]);
    assert_ne!(
        order[0], last,
        "what just ended does not open the new order"
    );
}

#[test]
fn a_song_added_to_play_next_plays_next_in_queue_order_and_shuffled() {
    let mut rig = Rig::new();
    rig.play(0);
    rig.add(3, super::Place::Next, None).unwrap();
    let up = rig.player.window(super::Order::UpNext, 0, 10);
    assert_eq!(up.total, 4);
    rig.pass(241_000);
    assert_eq!(rig.appearance(), rig.tracks[3]);

    rig.set(Some(true), None);
    rig.add(2, super::Place::Next, None).unwrap();
    rig.skip(Direction::Next);
    assert_eq!(rig.appearance(), rig.tracks[2]);
    // At the end while shuffled, it is among what is still to play.
    let played = rig.entry();
    let added = rig.add(1, super::Place::End, None).unwrap();
    assert!(!added.replayed);
    let order = rig.play_order();
    let at = order.iter().position(|entry| *entry == played).unwrap();
    let up: Vec<Id> = rig
        .player
        .window(super::Order::UpNext, 0, 10)
        .entries
        .into_iter()
        .map(|(entry, ..)| entry)
        .collect();
    assert_eq!(up, order[at + 1..]);
    assert_eq!(rig.queue().len(), 7);
}

#[test]
fn a_stale_queue_edit_is_refused_with_a_conflict_and_changes_nothing() {
    let mut rig = Rig::new();
    rig.play(0);
    let seen = rig.player.state().queue_revision;
    rig.add(3, super::Place::End, Some(seen)).unwrap();
    let now = rig.player.state().queue_revision;
    assert_eq!(now, seen + 1);
    let queue = rig.queue();

    let stale = rig.add(2, super::Place::Next, Some(seen));
    assert!(matches!(stale, Err(crate::Error::StaleQueue { current }) if current == now));
    let request = rig.request("queue.remove");
    let entries = [queue[1].0];
    let stale = rig.player.remove(
        request,
        Some(seen),
        &entries,
        &rig.server,
        Some(&rig.library),
    );
    assert!(matches!(stale, Err(crate::Error::StaleQueue { .. })));
    let request = rig.request("queue.move");
    let stale = rig.player.move_entries(
        request,
        Some(seen),
        &entries,
        super::Place::Next,
        &rig.server,
    );
    assert!(matches!(stale, Err(crate::Error::StaleQueue { .. })));
    let request = rig.request("queue.clear");
    let stale = rig.player.clear(request, Some(seen), &rig.server);
    assert!(matches!(stale, Err(crate::Error::StaleQueue { .. })));
    // Nothing changed, in memory or in the file.
    assert_eq!(rig.queue(), queue);
    assert_eq!(rig.player.state().queue_revision, now);
    assert_eq!(rig.server.playback_session().unwrap().queue_revision, now);
    assert_eq!(rig.server.queue().unwrap().len(), queue.len());
    // An edit on the revision there is goes through.
    let request = rig.request("queue.remove");
    rig.player
        .remove(
            request,
            Some(now),
            &entries,
            &rig.server,
            Some(&rig.library),
        )
        .unwrap();
    assert_eq!(rig.queue().len(), 4);
}

#[test]
fn taking_out_the_playing_entry_ends_its_listen_as_removed_and_plays_the_next() {
    let mut rig = Rig::new();
    rig.play(0);
    rig.pass(10_000);
    let request = rig.request("queue.remove");
    let playing = [rig.entry()];
    rig.player
        .remove(request, None, &playing, &rig.server, Some(&rig.library))
        .unwrap();
    rig.player.tick(&rig.server, Some(&rig.library)).unwrap();
    let state = rig.player.state();
    assert_eq!(state.status, Status::Playing);
    assert_eq!(state.queue_length, 3);
    assert_eq!(rig.appearance(), rig.tracks[1]);
    let staged = rig.staged();
    assert_eq!(
        staged[0].ended.as_ref().unwrap().end_reason.as_str(),
        "removed"
    );
    // The place saved is the new entry's.
    let session = rig.server.playback_session().unwrap();
    let rows = rig.server.queue().unwrap();
    assert_eq!(
        session.current,
        rows.iter()
            .find(|entry| entry.entry_id == rig.entry())
            .map(|entry| entry.row)
    );
}

#[test]
fn moved_entries_play_in_their_new_place_and_jump_plays_one_now() {
    let mut rig = Rig::new();
    rig.play(0);
    let queue = rig.queue();
    // The last to straight after the one playing.
    let request = rig.request("queue.move");
    rig.player
        .move_entries(
            request,
            None,
            &[queue[3].0],
            super::Place::Next,
            &rig.server,
        )
        .unwrap();
    rig.pass(5_000);
    rig.skip(Direction::Next);
    assert_eq!(rig.entry(), queue[3].0);
    rig.pass(5_000);
    // After itself is refused.
    let request = rig.request("queue.move");
    let refused = rig.player.move_entries(
        request,
        None,
        &[queue[1].0],
        super::Place::After(queue[1].0),
        &rig.server,
    );
    assert!(matches!(refused, Err(crate::Error::Invalid(_))));
    // Jump plays the one named now, and the listen it left ends as jump.
    let request = rig.request("playback.jump");
    rig.player
        .jump(request, queue[2].0, &rig.server, Some(&rig.library))
        .unwrap();
    rig.player.tick(&rig.server, Some(&rig.library)).unwrap();
    assert_eq!(rig.entry(), queue[2].0);
    let reasons: Vec<&str> = rig
        .staged()
        .iter()
        .filter_map(|listen| listen.ended.as_ref())
        .map(|ended| ended.end_reason.as_str())
        .collect();
    assert_eq!(reasons, ["next", "jump"]);
}

#[test]
fn clearing_the_queue_stops_the_player_and_adding_to_none_waits_on_the_first() {
    let mut rig = Rig::new();
    rig.play(0);
    rig.pass(5_000);
    let request = rig.request("queue.clear");
    rig.player.clear(request, None, &rig.server).unwrap();
    let state = rig.player.state();
    assert_eq!((state.status, state.queue_length), (Status::Stopped, 0));
    assert!(state.current.is_none());
    assert_eq!(rig.server.queue().unwrap().len(), 0);
    rig.add(2, super::Place::Next, None).unwrap();
    let state = rig.player.state();
    assert_eq!(state.status, Status::Stopped);
    assert_eq!(rig.appearance(), rig.tracks[2]);
    rig.resume();
    assert_eq!(rig.player.state().status, Status::Playing);
}

#[test]
fn a_replayed_edit_is_answered_from_its_receipt_and_adds_nothing_twice() {
    let mut rig = Rig::new();
    rig.play(0);
    let request = rig.request("queue.add");
    let command = request.command;
    let what = super::AddTracks {
        tracks: vec![rig.tracks[2]],
        place: super::Place::Next,
        context: (ContextKind::Manual, None),
        actor: None,
        play_if_empty: false,
    };
    let first = rig
        .player
        .add(request, None, what.clone(), &rig.server, Some(&rig.library))
        .unwrap();
    let again = Request {
        command,
        method: "queue.add",
        answer: Box::new(|_| String::from("never")),
    };
    let second = rig
        .player
        .add(again, None, what, &rig.server, Some(&rig.library))
        .unwrap();
    assert!(second.replayed);
    assert_eq!(second.answer, first.answer);
    assert_eq!(rig.queue().len(), 5);
}

#[test]
fn the_track_prepared_for_a_join_is_the_one_shuffle_and_repeat_say_follows_and_an_edit_lets_it_go()
{
    let mut rig = Rig::new();
    rig.fake.gapless();
    rig.play(0);
    rig.set(Some(true), None);
    let order = rig.play_order();
    let second = rig
        .queue()
        .iter()
        .position(|entry| entry.0 == order[1])
        .unwrap();
    // Thirty seconds from the end, what is prepared is what plays next in
    // the shuffled order.
    rig.pass(215_000);
    assert_eq!(rig.fake.queued(), Some(file(second)));
    // Adding a song to play next changes what follows: the one prepared
    // goes, and the added one is prepared in its place.
    rig.add(2, super::Place::Next, None).unwrap();
    assert_eq!(rig.fake.queued(), None);
    rig.pass(1_000);
    assert_eq!(rig.fake.queued(), Some(file(2)));
    // Repeat one: the same track follows, and joins as a repeat.
    rig.set(None, Some(super::Repeat::One));
    assert_eq!(rig.fake.queued(), None);
    rig.pass(1_000);
    assert_eq!(rig.fake.queued(), Some(file(0)));
    rig.pass(30_000);
    let state = rig.player.state();
    assert_eq!(state.current.unwrap().index, 0);
    let staged = rig.staged();
    assert_eq!(staged.len(), 2);
    assert_eq!(staged[1].started.start_reason.as_str(), "repeat");
}

impl Rig {
    /// Plays these tracks of the album, in this order, from the first.
    fn play_tracks(&mut self, tracks: &[usize]) {
        let request = self.request("playback.play");
        let what = PlayTracks {
            tracks: tracks.iter().map(|track| self.tracks[*track]).collect(),
            start: Some(0),
            context: (ContextKind::Album, Some(id(0xa1))),
            actor: None,
            shuffle: None,
            known: false,
        };
        self.player
            .play(request, what, &self.server, Some(&self.library))
            .unwrap();
        self.player.tick(&self.server, Some(&self.library)).unwrap();
    }
}

#[test]
fn with_repeat_all_an_entry_that_fails_never_comes_round_to_itself() {
    let mut rig = Rig::new();
    rig.play_tracks(&[0]);
    rig.set(None, Some(super::Repeat::All));
    rig.pass(20_000);
    rig.fake.rewrite_loaded();
    rig.seek(100_000);
    rig.pass(2_000);
    // Its only entry failed: the player stops rather than open it again.
    assert_eq!(rig.player.state().status, Status::Stopped);
    assert_eq!(rig.fake.loads(), [(file(0), 0)]);
    let [failed] = rig.staged().try_into().unwrap();
    assert_eq!(
        failed.ended.unwrap().end_reason,
        ritornel_domain::history::EndReason::Error
    );

    // With another entry, the failed last one goes round to it.
    let mut rig = Rig::new();
    rig.play_tracks(&[0, 1]);
    rig.set(None, Some(super::Repeat::All));
    rig.skip(Direction::Next);
    rig.pass(20_000);
    rig.fake.rewrite_loaded();
    rig.seek(100_000);
    rig.pass(1_000);
    let state = rig.player.state();
    assert_eq!(
        (state.status, state.current.unwrap().index),
        (Status::Playing, 0)
    );
}

#[test]
fn adding_to_an_empty_queue_selects_the_first_entry_in_the_same_write_as_its_receipt() {
    let mut rig = Rig::new();
    let command = Command {
        id: id(0x77),
        fingerprint: Fingerprint::of("queue.add", &[0x77]),
    };
    let request = || Request {
        command,
        method: "queue.add",
        answer: Box::new(|summary| summary.queue_revision.to_string()),
    };
    let what = super::AddTracks {
        tracks: vec![rig.tracks[2]],
        place: super::Place::Next,
        context: (ContextKind::Manual, None),
        actor: None,
        play_if_empty: false,
    };
    // Choosing the entry fails, as a crash there would leave it.
    rig.break_writes(
        "BEFORE UPDATE OF current_entry_id ON playback_session \
         WHEN NEW.current_entry_id IS NOT NULL",
    );
    let failed = rig.player.add(
        request(),
        None,
        what.clone(),
        &rig.server,
        Some(&rig.library),
    );
    assert!(failed.is_err());
    rig.heal();
    // Sent again, the add leaves the player waiting on its entry, and the
    // file says so too.
    rig.player
        .add(request(), None, what, &rig.server, Some(&rig.library))
        .unwrap();
    assert_eq!(rig.appearance(), rig.tracks[2]);
    let session = rig.server.playback_session().unwrap();
    let rows = rig.server.queue().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(session.current, Some(rows[0].row));
}

#[test]
fn a_new_order_at_the_wrap_is_saved_with_the_entry_that_opens_it() {
    let mut rig = Rig::new();
    rig.play(0);
    rig.set(Some(true), Some(super::Repeat::All));
    for _ in 0..3 {
        rig.skip(Direction::Next);
    }
    let before = rig.play_order();
    assert_eq!(rig.entry(), before[3]);
    // Choosing the entry that opens the new order fails, as a crash just
    // after the order was written would leave it.
    rig.break_writes(
        "BEFORE UPDATE OF current_entry_id ON playback_session \
         WHEN NEW.current_entry_id IS NOT OLD.current_entry_id",
    );
    for _ in 0..400 {
        rig.fake.advance(1_000);
        rig.clock.set(rig.clock.now_ms() + 1_000);
        rig.hub_clock.set(rig.hub_clock.now_ms() + 1_000);
        rig.keeper.advance(1_000);
        let _ = rig.player.tick(&rig.server, Some(&rig.library));
    }
    rig.heal();
    rig.restart(0xb3);
    // Either the queue is as it was before the wrap, on its last entry, or
    // the new order and the entry that opens it were saved together.
    let after = rig.play_order();
    let on = rig.entry();
    assert!(
        (after == before && on == before[3]) || (after[0] == on && rig.session_position() == 0),
        "restored on {on} in {after:?}, where the order was {before:?}"
    );
}

#[test]
fn entries_moved_to_the_end_go_last_in_queue_order_and_in_play_order() {
    let mut rig = Rig::new();
    rig.play(0);
    rig.set(Some(true), None);
    let order = rig.play_order();
    let request = rig.request("queue.move");
    rig.player
        .move_entries(request, None, &[order[1]], super::Place::End, &rig.server)
        .unwrap();
    assert_eq!(rig.play_order().last(), Some(&order[1]));
    assert_eq!(rig.queue().last().map(|entry| entry.0), Some(order[1]));
    let up = rig.player.window(super::Order::UpNext, 0, 10);
    assert_eq!(up.entries.last().map(|entry| entry.0), Some(order[1]));
}

#[test]
fn an_edit_after_a_join_has_happened_keeps_the_join_and_the_next_listen_starts_at_its_boundary() {
    let mut rig = Rig::new();
    rig.fake.gapless();
    rig.play(0);
    rig.pass(239_000);
    assert_eq!(rig.fake.queued(), Some(file(1)));
    // The first track ends into the second, which plays 100 ms before the
    // player next looks.
    rig.fake.advance(1_100);
    rig.clock.set(rig.clock.now_ms() + 1_100);
    rig.hub_clock.set(rig.hub_clock.now_ms() + 1_100);
    rig.keeper.advance(1_100);
    // Meanwhile a song is added at the end: too late to undo the join.
    rig.add(3, super::Place::End, None).unwrap();
    rig.pass(1_000);
    // The second plays on from the join: it was not opened again.
    assert_eq!(rig.fake.loads(), [(file(0), 0)]);
    let state = rig.player.state();
    assert_eq!(state.current.unwrap().index, 1);
    assert_eq!(state.position_ms, 1_100);
    // One listen each, the second from the boundary.
    let [first, second] = rig.staged().try_into().unwrap();
    assert_eq!(
        first.ended.as_ref().unwrap().end_reason.as_str(),
        "finished"
    );
    assert_eq!(first.heard.ms_played, 240_000);
    assert_eq!(second.started.start_position_ms, 0);
    assert_eq!(second.started.started_at_ms, T0 + 240_000);
    assert_eq!(second.ended, None);
}

#[test]
fn with_repeat_all_a_last_entry_that_cannot_open_goes_round_to_the_first() {
    let mut rig = Rig::new();
    rig.fake.broken(file(1), "unreadable");
    rig.set(None, Some(super::Repeat::All));
    rig.play_tracks(&[0, 1]);
    rig.pass(241_000);
    // The second could not open: the first plays again, and the second
    // says why.
    let state = rig.player.state();
    assert_eq!(
        (state.status, state.current.unwrap().index),
        (Status::Playing, 0)
    );
    assert_eq!(rig.queue()[1].2.as_deref(), Some("unreadable"));

    // A queue of one that cannot open does not come round to itself.
    let mut rig = Rig::new();
    rig.fake.broken(file(1), "unreadable");
    rig.set(None, Some(super::Repeat::All));
    rig.play_tracks(&[1]);
    assert_eq!(rig.player.state().status, Status::Stopped);
}

#[test]
fn a_repeat_already_joined_keeps_its_reason_when_repeat_is_turned_off_before_the_tick() {
    let mut rig = Rig::new();
    rig.fake.gapless();
    rig.play(0);
    rig.set(None, Some(super::Repeat::One));
    rig.pass(239_000);
    // The track ends into itself, and is heard 100 ms before repeat is
    // turned off and the player next looks.
    rig.fake.advance(1_100);
    rig.clock.set(rig.clock.now_ms() + 1_100);
    rig.hub_clock.set(rig.hub_clock.now_ms() + 1_100);
    rig.keeper.advance(1_100);
    rig.set(None, Some(super::Repeat::Off));
    rig.pass(1_000);
    let staged = rig.staged();
    assert_eq!(staged.len(), 2);
    assert_eq!(staged[1].started.start_reason.as_str(), "repeat");
    assert_eq!(rig.player.state().current.unwrap().index, 0);
}

// ---------------------------------------------------------------- schedules held to the fake's own timeline

/// One step of a schedule.
#[derive(Debug, Clone, Copy)]
enum Step {
    /// The output plays on and the player does not look: a command may
    /// come in the meantime.
    Play(i64),
    /// As `Play`, to this many milliseconds before the end of the track in
    /// hand: a command next comes as it ends.
    Near(i64),
    /// Time passes with the player ticking, a second at a time.
    Tick(i64),
    Pause,
    Resume,
    Seek(i64),
    Skip(Direction),
    /// A song of the album added to play next, or last.
    Add(usize, bool),
    /// The entry this many places on in play order taken out, or jumped to.
    Remove(usize),
    Jump(usize),
    Shuffle(bool),
    Repeat(super::Repeat),
    Stop,
}

/// What a schedule did that its listens are held to: when shuffle was
/// turned on or off, by how many spans had been given; the entries it
/// took out, with when each removal returned; and when each command
/// ran, by the monotonic clock.
#[derive(Debug, Default)]
struct Ran {
    shuffles: Vec<(usize, bool)>,
    removed: Vec<(Id, i64)>,
    commands: Vec<(i64, i64)>,
}

impl Ran {
    fn and(mut self, then: Self) -> Self {
        self.shuffles.extend(then.shuffles.into_iter().skip(1));
        self.removed.extend(then.removed);
        self.commands.extend(then.commands);
        self
    }
}

impl Rig {
    /// While the player is busy with each call to the renderer, the output
    /// plays on for `ms`, and the clocks move on with it, as a callback
    /// runs during a command.
    fn busy(&mut self, ms: u64) {
        self.busy = ms;
        let (clock, hub, keeper) = (
            self.clock.clone(),
            self.hub_clock.clone(),
            self.keeper.clone(),
        );
        self.fake.meanwhile(ms, move |ms| {
            clock.set(clock.now_ms() + ms);
            hub.set(hub.now_ms() + ms);
            keeper.advance(ms);
        });
    }

    /// Runs `steps`, and after every command holds the player to the fake:
    /// the entry it is on is the one whose track the output plays. With the
    /// output busy during commands, the player may have been left behind
    /// by the time one returns, and only its listens are held.
    fn run(&mut self, steps: &[Step]) -> Ran {
        let mut ran = Ran {
            shuffles: vec![(0, self.player.state().shuffle)],
            ..Ran::default()
        };
        let mono = |rig: &Self| crate::timekeeping::Timekeeper::mono_ms(&*rig.keeper);
        for step in steps {
            let from = mono(self);
            let on = self.player.state().current.map(|current| current.entry_id);
            let left = self.fake.left().map(|left| i64::try_from(left).unwrap());
            match *step {
                Step::Play(ms) | Step::Near(ms) => {
                    let ms = match step {
                        Step::Near(_) => left.map_or(0, |left| (left - ms).max(0)),
                        _ => ms,
                    };
                    self.fake.advance(u64::try_from(ms).unwrap());
                    for clock in [&self.clock, &self.hub_clock] {
                        clock.set(clock.now_ms() + ms);
                    }
                    self.keeper.advance(ms);
                    continue;
                }
                Step::Tick(ms) => {
                    self.pass(ms);
                    continue;
                }
                Step::Pause => self.pause(),
                Step::Resume => {
                    let request = self.request("playback.resume");
                    self.player
                        .resume(request, &self.server, Some(&self.library))
                        .unwrap();
                }
                Step::Seek(ms) if on.is_some() => self.seek(ms),
                Step::Skip(direction) => {
                    let (Some(entry), request) = (on, self.request("playback.skip")) else {
                        continue;
                    };
                    let (server, library): (_, Option<&dyn Catalog>) =
                        (&self.server, Some(&self.library));
                    self.player
                        .skip(request, entry, direction, server, library)
                        .unwrap();
                }
                Step::Add(track, next) => {
                    let place = if next {
                        super::Place::Next
                    } else {
                        super::Place::End
                    };
                    self.add(track, place, None).unwrap();
                }
                Step::Remove(ahead) | Step::Jump(ahead) => {
                    let order = self.play_order();
                    let slot = self.player.window(super::Order::Play, 0, 0).current;
                    let Some(entry) = order.get((slot.unwrap_or(0) + ahead) % order.len().max(1))
                    else {
                        continue;
                    };
                    let request = self.request("queue.edit");
                    let (server, library): (_, Option<&dyn Catalog>) =
                        (&self.server, Some(&self.library));
                    if matches!(step, Step::Jump(_)) {
                        self.player.jump(request, *entry, server, library).unwrap();
                    } else {
                        self.player
                            .remove(request, None, &[*entry], server, library)
                            .unwrap();
                        ran.removed.push((*entry, mono(self)));
                    }
                }
                Step::Shuffle(on) => {
                    self.set(Some(on), None);
                    let decided = self.fake.timeline().1.len();
                    ran.shuffles.push((decided, on));
                }
                Step::Repeat(repeat) => self.set(None, Some(repeat)),
                Step::Stop => {
                    let request = self.request("playback.stop");
                    self.player.stop(request, &self.server).unwrap();
                }
                Step::Seek(_) => continue,
            }
            ran.commands.push((from, mono(self)));
            let state = self.player.state();
            let Some(playing) = self.fake.in_hand() else {
                continue;
            };
            if self.busy > 0 || self.player.moving_on() || state.status == Status::Stopped {
                continue;
            }
            let track = state
                .current
                .as_ref()
                .and_then(|current| current.track.as_ref());
            let index =
                track.and_then(|track| self.tracks.iter().position(|t| *t == track.appearance));
            assert_eq!(
                index.map(file),
                Some(playing),
                "after {step:?} the player is on another track than the output: {state:?}"
            );
        }
        ran
    }

    /// Holds every listen to what the fake says it played: no frame lost
    /// or counted twice, each listen dated by its own first frame and its
    /// last, of the track that span played, recording shuffle as it was
    /// when its span was decided, and ended as removed only if its entry
    /// was, or if all of it was heard while one command ran. A removed
    /// entry never begins after its removal has returned.
    fn listens_hold(&self, ran: &Ran) {
        let (said, given) = self.fake.timeline();
        let staged = self.staged();
        let heard: u64 = said
            .iter()
            .map(|event| match event {
                super::Event::Ended { frames, .. } => *frames,
                super::Event::Began { .. } => 0,
            })
            .sum();
        let kept: i64 = staged.iter().map(|listen| listen.heard.ms_played).sum();
        assert_eq!(
            kept,
            i64::try_from(heard).unwrap(),
            "frames heard and kept differ"
        );
        let mono = |at: &u64| i64::try_from(at / 1_000_000).unwrap();
        let mut before = i64::MIN;
        for listen in &staged {
            let ended = listen.ended.as_ref().expect("a listen was left open");
            let (start, end) = (listen.started.started_mono_ms, ended.ended_mono_ms.unwrap());
            assert!(before <= start && start <= end, "{listen:?} after {before}");
            before = end;
            let span = said.iter().find_map(|event| match event {
                super::Event::Began { span, at } if mono(at) == start => Some(*span),
                _ => None,
            });
            let span = span.unwrap_or_else(|| panic!("no span began as {listen:?} did"));
            assert!(
                said.iter().any(|event| matches!(event,
                    super::Event::Ended { at, .. } if mono(at) == end)),
                "no span ended as {listen:?} did"
            );
            let removed = ran
                .removed
                .iter()
                .find(|(entry, _)| Some(*entry) == listen.started.entry);
            if let Some((_, at)) = removed {
                assert!(start <= *at, "{listen:?} began after its removal at {at}");
            } else if ended.end_reason.as_str() == "removed" {
                assert!(
                    ran.commands
                        .iter()
                        .any(|(from, to)| *from <= start && end <= *to),
                    "{listen:?} ended as removed, its entry in the queue"
                );
            }
            // Its span, and the one it was decided with: a seek's track's.
            let index = |span: u64| given.iter().position(|given| given.span == span).unwrap();
            let mut decided = index(span);
            let track = self
                .tracks
                .iter()
                .position(|t| *t == listen.started.appearance);
            assert_eq!(
                track.map(file).as_ref(),
                Some(&given[decided].path),
                "{listen:?}"
            );
            while let Some(of) = given[decided].seek_of {
                decided = index(of);
            }
            let (_, shuffled) = ran
                .shuffles
                .iter()
                .rev()
                .find(|(spans, _)| *spans <= decided)
                .unwrap();
            assert_eq!(
                listen.started.shuffle, *shuffled,
                "{listen:?} decided as span {decided}"
            );
        }
    }
}

/// Plays the album gaplessly to a second before the end of its first
/// track, and on into the second for 100 ms with nothing looking.
fn just_after_a_join() -> (Rig, Vec<Step>) {
    let mut rig = Rig::new();
    rig.fake.gapless();
    rig.play(0);
    (rig, vec![Step::Tick(239_000), Step::Play(1_100)])
}

#[test]
fn shuffle_turned_on_just_after_a_join_reorders_around_the_track_playing_and_not_its_listen() {
    let (mut rig, mut steps) = just_after_a_join();
    steps.extend([Step::Shuffle(true), Step::Tick(1_000)]);
    let ran = rig.run(&steps);
    // The new order opens with the entry the output is playing.
    assert_eq!(rig.play_order()[0], rig.queue()[1].0);
    // Its listen was decided with shuffle off.
    let [first, second] = rig.staged().try_into().unwrap();
    assert!(!first.started.shuffle && !second.started.shuffle);
    rig.run(&[Step::Stop, Step::Tick(1)]);
    rig.listens_hold(&ran);
}

#[test]
fn a_song_added_to_play_next_just_after_a_join_plays_after_the_track_playing() {
    let (mut rig, mut steps) = just_after_a_join();
    steps.extend([Step::Add(3, true), Step::Tick(300_000)]);
    let ran = rig.run(&steps);
    // The second track played out, and the song added is what followed.
    assert_eq!(rig.appearance(), rig.tracks[3]);
    let played: Vec<Id> = rig.staged().iter().map(|l| l.started.appearance).collect();
    assert_eq!(played, [rig.tracks[0], rig.tracks[1], rig.tracks[3]]);
    rig.run(&[Step::Stop, Step::Tick(1)]);
    rig.listens_hold(&ran);
}

#[test]
fn a_join_made_while_an_edit_lets_the_next_go_keeps_the_track_it_ran_into() {
    let mut rig = Rig::new();
    rig.fake.gapless();
    rig.play(0);
    rig.run(&[Step::Tick(239_000), Step::Play(998)]);
    // The output runs into the second track while the edit is written,
    // after the player last looked.
    rig.busy(2);
    let ran = rig.run(&[Step::Add(3, false), Step::Tick(1_000)]);
    // The second plays on from the join: never let go, never loaded again.
    assert_eq!(rig.fake.loads(), [(file(0), 0)]);
    let [first, second] = rig.staged().try_into().unwrap();
    assert_eq!(first.ended.unwrap().end_reason.as_str(), "finished");
    assert_eq!(second.ended, None);
    assert_eq!(rig.player.state().current.unwrap().index, 1);
    rig.run(&[Step::Stop, Step::Tick(1)]);
    rig.listens_hold(&ran);
}

#[test]
fn a_song_taken_out_of_up_next_in_the_last_half_second_is_never_heard() {
    let mut rig = Rig::new();
    rig.fake.gapless();
    rig.play(0);
    // 400 ms from the end of the first: its decoder has the second.
    let ran = rig.run(&[Step::Tick(239_000), Step::Play(600), Step::Remove(1)]);
    let ran = ran.and(rig.run(&[Step::Tick(2_000), Step::Stop, Step::Tick(1)]));
    // The first ended where it would have, the second was never heard, and
    // the third started as any track does.
    let (said, given) = rig.fake.timeline();
    let second: Vec<u64> = given
        .iter()
        .filter(|given| given.path == file(1))
        .map(|given| given.span)
        .collect();
    assert!(!second.is_empty());
    let began = |span: &u64| {
        said.iter()
            .any(|event| matches!(event, super::Event::Began { span: s, .. } if s == span))
    };
    assert!(!second.iter().any(began), "{said:?}");
    assert_eq!(rig.fake.loads(), [(file(0), 0), (file(2), 0)]);
    let [first, third] = rig.staged().try_into().unwrap();
    let ended = first.ended.as_ref().unwrap();
    assert_eq!(
        (ended.end_reason.as_str(), first.heard.ms_played),
        ("finished", 240_000)
    );
    assert_eq!(ended.ended_at_ms, T0 + 240_000);
    assert_eq!(third.started.appearance, rig.tracks[2]);
    assert_eq!(third.started.start_reason.as_str(), "auto_advance");
    rig.listens_hold(&ran);
}

#[test]
fn no_schedule_of_play_commands_and_ticks_parts_the_player_from_the_output_or_a_listen_from_it() {
    use crate::testing::Draws;
    for seed in 1..=150 {
        let mut draws = Draws::seeded(seed);
        let mut rig = Rig::new();
        rig.fake.gapless();
        // A few seconds each: joins in time are the rule, as in use.
        for track in 0..ALBUM.len() {
            rig.fake.track(file(track), 2_000 + draws.below(6_000));
        }
        let cut = |draws: &mut Draws| usize::try_from(draws.below(4)).unwrap();
        // Now and then a file that cannot be read to its end.
        if draws.below(3) == 0 {
            rig.fake
                .cut(file(cut(&mut draws)), 300 + draws.below(4_000));
        }
        // In half the schedules the output plays on while a command is
        // busy, so that joins and ends fall inside commands.
        if draws.below(2) == 0 {
            rig.busy(draws.below(3) + 1);
        }
        let steps: Vec<Step> = (0..60)
            .map(|_| match draws.below(29) {
                0..=7 => Step::Play(i64::try_from(draws.below(1_500)).unwrap() + 1),
                8..=10 => Step::Tick(i64::try_from(draws.below(1_200)).unwrap() + 1),
                11 => Step::Pause,
                12 | 13 => Step::Resume,
                14 => Step::Seek(i64::try_from(draws.below(8_000)).unwrap()),
                15 => Step::Skip(Direction::Next),
                16 => Step::Skip(Direction::Previous),
                17 => Step::Add(cut(&mut draws), draws.below(2) == 0),
                18 => Step::Remove(cut(&mut draws)),
                19 => Step::Jump(cut(&mut draws)),
                20 => Step::Shuffle(draws.below(2) == 0),
                21 => Step::Repeat(match draws.below(3) {
                    0 => super::Repeat::Off,
                    1 => super::Repeat::All,
                    _ => super::Repeat::One,
                }),
                22 => Step::Stop,
                23..=25 => Step::Near(i64::try_from(draws.below(4)).unwrap()),
                // The song after the one playing, taken out of Up next,
                // often just before the one playing ends.
                26 | 27 => Step::Remove(1),
                _ => Step::Tick(50),
            })
            .collect();
        rig.play(usize::try_from(draws.below(4)).unwrap());
        let ran = rig.run(&steps).and(rig.run(&[Step::Stop, Step::Tick(1)]));
        assert_eq!(rig.player.open_listen(), None, "seed {seed}");
        let held = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            rig.listens_hold(&ran);
        }));
        assert!(held.is_ok(), "seed {seed}: {steps:?}");
    }
}

#[test]
fn an_output_that_cannot_be_opened_pauses_in_place_and_blames_no_song() {
    let mut rig = Rig::new();
    rig.play(1);
    rig.pass(30_000);
    rig.fake.lose_output();
    rig.wait(1_000);
    let lost_at = rig.clock.now_ms();
    rig.fake.refuse_output(true);
    // Play tries the output, again and again: the player stays on the
    // song, where it was, and no song is marked.
    for _ in 0..4 {
        rig.resume();
        rig.wait(60_000);
    }
    let state = rig.player.state();
    assert_eq!(
        (
            state.status,
            state.current.unwrap().index,
            state.position_ms
        ),
        (Status::Paused, 1, 30_000)
    );
    assert_eq!(rig.session_position(), 30_000);
    assert!(rig.queue().iter().all(|(_, _, failed)| failed.is_none()));
    // It says why, since the output first went.
    let failing = state.output_failing.unwrap();
    assert_eq!(failing.since_ms, lost_at);
    assert_eq!(failing.detail, "no output device is there");
    // Once it opens, the song carries on where the output went, as the
    // listen it cut off.
    rig.fake.refuse_output(false);
    rig.resume();
    assert_eq!(rig.fake.loads().last().unwrap(), &(file(1), 30_000));
    rig.pass(5_000);
    let staged = rig.staged();
    let lost = staged
        .iter()
        .find(|listen| listen.ended.is_some())
        .unwrap()
        .started
        .listen_id;
    assert_eq!(staged.last().unwrap().started.continues, Some(lost));
    assert_eq!(rig.player.state().output_failing, None);
    rig.fake.refuse_output(true);
    // A queue played anew waits on its first song, from its start.
    rig.play(2);
    let state = rig.player.state();
    assert_eq!(
        (
            state.status,
            state.current.unwrap().index,
            state.position_ms
        ),
        (Status::Paused, 2, 0)
    );
    assert!(rig.queue().iter().all(|(_, _, failed)| failed.is_none()));
    // Once it opens, the song plays from where it waited.
    rig.fake.refuse_output(false);
    rig.resume();
    let state = rig.player.state();
    assert_eq!(
        (state.status, state.current.unwrap().index),
        (Status::Playing, 2)
    );
    assert_eq!(state.output_failing, None);
    assert_eq!(rig.fake.loads().last().unwrap().0, file(2));
}

#[test]
fn an_add_to_an_empty_queue_can_play_it_and_one_to_a_queue_never_replaces_it() {
    let mut rig = Rig::new();
    let add = |rig: &mut Rig, track: usize, play_if_empty: bool| {
        let request = rig.request("queue.add");
        let what = super::AddTracks {
            tracks: vec![rig.tracks[track]],
            place: super::Place::Next,
            context: (ContextKind::Manual, None),
            actor: None,
            play_if_empty,
        };
        let done = rig
            .player
            .add(request, None, what, &rig.server, Some(&rig.library))
            .unwrap();
        rig.note(&done.events);
    };
    // Left out, the player waits on what was added, as before.
    add(&mut rig, 0, false);
    assert_eq!(rig.player.state().status, Status::Stopped);
    let clear = rig.request("queue.clear");
    rig.player.clear(clear, None, &rig.server).unwrap();
    // Asked, an empty queue plays.
    add(&mut rig, 1, true);
    rig.tick();
    let state = rig.player.state();
    assert_eq!((state.status, state.queue_length), (Status::Playing, 1));
    assert_eq!(rig.fake.in_hand(), Some(file(1)));
    assert!(rig.fake.playing());
    // A second add asking the same, made before its window heard of the
    // first, goes in after it and changes nothing else.
    add(&mut rig, 2, true);
    rig.pass(1_000);
    let state = rig.player.state();
    assert_eq!(
        (
            state.status,
            state.current.unwrap().index,
            state.queue_length
        ),
        (Status::Playing, 0, 2)
    );
    assert_eq!(rig.fake.in_hand(), Some(file(1)));
    assert_eq!(
        rig.queue().iter().map(|entry| entry.1).collect::<Vec<_>>(),
        [rig.tracks[1], rig.tracks[2]]
    );
}

#[test]
fn a_tick_says_what_it_did_while_saves_fail() {
    let mut rig = Rig::new();
    rig.player = Player::restore(
        PlayerSetup {
            fail_saves_after_ms: Some(10_000),
            ..setup(
                &rig.identity,
                rig.run,
                &rig.clock,
                &rig.keeper,
                &rig.entropy,
            )
        },
        rig.fake.renderer(),
        &rig.server,
        Some(&rig.library),
    )
    .unwrap()
    .0;
    rig.play(2);
    // The 25 seconds of the interlude, and on into the next song.
    let mut moved = None;
    for _ in 0..30 {
        rig.fake.advance(1_000);
        rig.clock.set(rig.clock.now_ms() + 1_000);
        rig.keeper.advance(1_000);
        let events = rig.player.tick(&rig.server, Some(&rig.library)).unwrap();
        let index = rig.player.state().current.unwrap().index;
        if index == 3 && moved.is_none() {
            moved = Some((
                events.contains(&super::PlayerEvent::State),
                rig.player.save_failing().is_some(),
            ));
        }
    }
    // The tick that moved on said so, though its save failed.
    assert_eq!(moved, Some((true, true)));
}

#[test]
fn a_seek_with_nothing_open_stays_inside_the_song() {
    let mut rig = Rig::new();
    rig.play(0);
    let stop = rig.request("playback.stop");
    rig.player.stop(stop, &rig.server).unwrap();
    let (seek, entry) = (rig.request("playback.seek"), rig.entry());
    rig.player
        .seek(seek, entry, 600_000, &rig.server, Some(&rig.library))
        .unwrap();
    assert_eq!(rig.player.state().position_ms, ALBUM[0].length_ms);
    assert_eq!(rig.session_position(), ALBUM[0].length_ms);
}

#[test]
fn a_song_none_of_which_is_heard_is_marked_at_once_in_the_live_queue() {
    let mut rig = Rig::new();
    rig.fake.cut(file(0), 0);
    rig.play(0);
    rig.fake.advance(10);
    let events = rig.player.tick(&rig.server, Some(&rig.library)).unwrap();
    assert!(
        events
            .iter()
            .any(|event| matches!(event, super::PlayerEvent::Queue(_)))
    );
    assert_eq!(rig.queue()[0].2.as_deref(), Some("unreadable"));
}

#[test]
fn an_output_that_stops_answering_after_running_on_into_the_next_song_waits_in_that_song() {
    let mut rig = Rig::new();
    rig.fake.gapless();
    rig.play(0);
    rig.pass(239_000);
    // The seek runs the last second of the first song and a tenth of the
    // next before the output stops answering.
    rig.fake.stall_at_seek(1_100);
    rig.seek(10_000);
    let state = rig.player.state();
    assert_eq!(
        (
            state.status,
            state.current.unwrap().index,
            state.position_ms
        ),
        (Status::Paused, 1, 100)
    );
    let lost = rig
        .staged()
        .into_iter()
        .find(|listen| {
            listen.ended.as_ref().is_some_and(|ended| {
                ended.end_reason == ritornel_domain::history::EndReason::OutputLost
            })
        })
        .unwrap();
    assert_eq!(lost.started.appearance, rig.tracks[1]);
    let session = rig.server.playback_session().unwrap();
    assert_eq!(
        (session.resume_listen, session.position_ms),
        (Some(lost.started.listen_id), 100)
    );
}

#[test]
fn a_song_the_output_could_not_open_is_where_a_restart_finds_the_player() {
    let mut rig = Rig::new();
    rig.fake.broken(file(0), "missing");
    rig.fake.refuse_output(true);
    rig.play(0);
    // The first is marked and passed over; the second waits on the output.
    let state = rig.player.state();
    assert_eq!(
        (state.status, state.current.unwrap().index),
        (Status::Paused, 1)
    );
    assert!(rig.restart(0xb1).is_empty());
    assert_eq!(rig.player.state().current.unwrap().index, 1);
}
