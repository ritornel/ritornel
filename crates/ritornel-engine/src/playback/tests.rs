//! The player with a fake renderer, both stores in memory and clocks the
//! test moves: the queue, the session, what every listen records, the
//! crash, and delivery to history. The catalog is a few invented tracks
//! written straight into the library. See docs/design/playback.md and
//! docs/design/history.md.

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
        }
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
            self.player.tick(&self.server, Some(&self.library)).unwrap();
            left -= step;
        }
    }

    /// Lets `ms` pass with the output stopped: a pause.
    fn wait(&mut self, ms: i64) {
        self.clock.set(self.clock.now_ms() + ms);
        self.hub_clock.set(self.hub_clock.now_ms() + ms);
        self.keeper.advance(ms);
        self.player.tick(&self.server, Some(&self.library)).unwrap();
    }

    fn play(&mut self, start: usize) {
        let request = self.request("playback.play");
        let what = PlayTracks {
            tracks: self.tracks.clone(),
            start,
            context: (ContextKind::Album, Some(id(0xa1))),
            actor: self.server.local_session().unwrap(),
        };
        self.player
            .play(request, what, &self.server, Some(&self.library))
            .unwrap();
        // The first frames are heard on the next tick.
        self.player.tick(&self.server, Some(&self.library)).unwrap();
    }

    fn pause(&mut self) {
        let request = self.request("playback.pause");
        self.player.pause(request, &self.server).unwrap();
    }

    fn resume(&mut self) {
        let request = self.request("playback.resume");
        self.player
            .resume(request, &self.server, Some(&self.library))
            .unwrap();
        self.player.tick(&self.server, Some(&self.library)).unwrap();
    }

    fn entry(&self) -> Id {
        self.player.state().current.unwrap().entry_id
    }

    fn seek(&mut self, position_ms: i64) {
        let request = self.request("playback.seek");
        let entry = self.entry();
        self.player
            .seek(
                request,
                entry,
                position_ms,
                &self.server,
                Some(&self.library),
            )
            .unwrap();
    }

    fn skip(&mut self, direction: Direction) {
        let request = self.request("playback.skip");
        let entry = self.entry();
        self.player
            .skip(request, entry, direction, &self.server, Some(&self.library))
            .unwrap();
        self.player.tick(&self.server, Some(&self.library)).unwrap();
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
        (96_000, false, "skipped")
    );
    assert_eq!(
        (
            plays[1].ms_played,
            plays[1].counts,
            plays[1].outcome.as_str()
        ),
        (180_000, true, "listened")
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
    // playback.md, "Recording a listen": a 4:00 track queued from an
    // album. The table's times are minutes after 21:00:00.
    let mut rig = Rig::new();
    rig.play(0);
    // Written when the first frame is heard, with nothing played yet.
    rig.pass(1_000);
    let [first] = rig.staged().try_into().unwrap();
    assert_eq!(first.heard.ms_played, 0);
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
    // history.md, "What a listen contains": a 5:00 track.
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
    assert_eq!(play.ms_played, 300_000);
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
    let queue = rig.player.queue();
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
    let queue = rig.player.queue();
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
    assert_eq!(rig.player.queue()[0].2.as_deref(), Some("file_changed"));
    assert_eq!(rig.player.state().current.unwrap().index, 1);
    assert_eq!(rig.player.state().status, Status::Playing);
}

#[test]
fn a_close_that_fails_keeps_the_listen_whole_and_the_retry_closes_it_as_it_ended() {
    let mut rig = Rig::new();
    rig.play(0);
    rig.pass(20_000);
    let stopped_at = rig.clock.now_ms();
    rig.break_writes("BEFORE UPDATE OF end_reason ON listen_staging");
    let request = rig.request("playback.stop");
    assert!(rig.player.stop(request, &rig.server).is_err());
    rig.heal();
    rig.wait(3_000);
    let request = rig.request("playback.stop");
    rig.player.stop(request, &rig.server).unwrap();

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

/// A second of time on every clock, and a tick, whose error is returned.
fn second(rig: &mut Rig) -> Result<Vec<super::PlayerEvent>, crate::Error> {
    rig.fake.advance(1000);
    rig.clock.set(rig.clock.now_ms() + 1000);
    rig.hub_clock.set(rig.hub_clock.now_ms() + 1000);
    rig.keeper.advance(1000);
    rig.player.tick(&rig.server, Some(&rig.library))
}

#[test]
fn frames_heard_while_the_listen_cannot_be_written_are_kept_with_its_start() {
    let mut rig = Rig::new();
    rig.break_writes("BEFORE INSERT ON listen_staging");
    rig.play(0);
    // A second is heard and the listen cannot be written; then it can.
    assert!(second(&mut rig).is_err());
    rig.heal();
    second(&mut rig).unwrap();
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
    assert!(second(&mut rig).is_err());
    // Stopped before the write could be made: the stop fails, and what was
    // heard is still there for the retry.
    let request = rig.request("playback.stop");
    assert!(rig.player.stop(request, &rig.server).is_err());
    rig.heal();
    let request = rig.request("playback.stop");
    rig.player.stop(request, &rig.server).unwrap();
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
    use super::listen::{MAX_RANGES, OpenListen};
    use super::store::{Ended, Mark};
    let cap = i64::try_from(MAX_RANGES).unwrap();
    // One range past the cap, then all of them heard again as one; and
    // exactly as many ranges as the cap, none joined.
    for (ranges, heard_whole, approximate) in [(cap + 1, true, true), (cap, false, false)] {
        let mut rig = Rig::new();
        rig.play(0);
        rig.pass(1_000);
        let started = rig.staged()[0].started.clone();
        let mut listen = OpenListen::new(started.clone(), super::renderer::fake::RATE);
        for at in 0..ranges {
            listen.seek(at * 30);
            listen.heard(10, at * 30 + 10);
        }
        if heard_whole {
            listen.seek(0);
            listen.heard(200_000, 200_000);
        }
        let mark = Mark {
            position_ms: listen.position_ms(),
            mono_ms: 2_000_000,
        };
        rig.server
            .checkpoint_listen(started.listen_id, listen.snapshot(), mark)
            .unwrap();
        let ended = Ended {
            end_position_ms: listen.position_ms(),
            end_reason: ritornel_domain::history::EndReason::Stopped,
            ended_at_ms: rig.clock.now_ms(),
            ended_mono_ms: Some(2_000_000),
        };
        rig.server
            .close_listen(started.listen_id, listen.snapshot(), ended, (None, 0, None))
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
    let state = rig.player.state();
    assert_eq!((state.status, state.position_ms), (Status::Paused, 30_000));
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
    assert_eq!(plays[0].ms_played, 150_000);
    assert_eq!(plays[0].listens.len(), 2);
}
