//! History in `library.db`: ingest, derivation in the same transaction,
//! and the rebuild, which must give exactly what incremental ingest gave,
//! on every fixture and after the rule changes. See decision 22 and
//! "Rebuild" in docs/design/history.md.
//!
//! The fixtures are listens made up for the purpose, sent as the player
//! would send them, against the invented album of the player's tests.

use std::sync::Arc;

use ritornel_domain::history::PlayRule;

use super::*;
use crate::playback::tests::{ALBUM, catalog};
use crate::testing::{CountingEntropy, FixedClock};
use crate::{ServerIdentity, ServerStore};

/// 2026-11-15 21:00:00 UTC.
const T0: i64 = 1_794_776_400_000;
const MINUTE: i64 = 60_000;
const BOOT: Id = Id::from_bytes([0xb0; 16]);

/// The hub's clock, as the test sets it. The library's writer can be made
/// to read it later than everyone else, as if a write had waited that long
/// for the writer.
#[derive(Debug)]
struct HubClock {
    now: FixedClock,
    writer_lag_ms: std::sync::atomic::AtomicI64,
}

impl HubClock {
    fn set(&self, now_ms: i64) {
        self.now.set(now_ms);
    }
}

impl crate::Clock for HubClock {
    fn now_ms(&self) -> i64 {
        let lag = if std::thread::current().name() == Some("library.db writer") {
            self.writer_lag_ms.load(std::sync::atomic::Ordering::SeqCst)
        } else {
            0
        };
        self.now.now_ms() + lag
    }
}

/// A library with the invented album, and what a listen names of it.
struct Library {
    library: LibraryStore,
    clock: Arc<HubClock>,
    server: ServerIdentity,
    listener: Id,
    /// Each track's appearance, source item and asset.
    tracks: Vec<(Id, Id, Id)>,
    /// A file with no binding: its source item and asset.
    loose: (Id, Id),
}

fn library() -> Library {
    let clock = Arc::new(HubClock {
        now: FixedClock::at(T0),
        writer_lag_ms: std::sync::atomic::AtomicI64::new(0),
    });
    let entropy = Arc::new(CountingEntropy::default());
    let servers = ServerStore::open_in_memory(clock.clone(), entropy.clone()).unwrap();
    let (server, _) = servers.ensure_identity("desk").unwrap();
    let (library, _) = LibraryStore::create_in_memory(
        &server,
        WriteRequest {
            method: "library.create",
            command: None,
            client: None,
        },
        |_| String::new(),
        clock.clone(),
        entropy,
    )
    .unwrap();
    let (appearances, items) = catalog(&library, server.server_id, &ALBUM);
    let ids = |item: i64| -> (Id, Id) {
        library
            .read(|connection| {
                Ok(connection.query_row(
                    "SELECT si.public_id, aa.public_id FROM source_item si
                     JOIN source_item_revision sr ON sr.id = si.current_revision_id
                     JOIN audio_asset aa ON aa.id = sr.audio_asset_id WHERE si.id = ?1",
                    [item],
                    |row| Ok((Id::from_bytes(row.get(0)?), Id::from_bytes(row.get(1)?))),
                )?)
            })
            .unwrap()
    };
    let tracks = appearances
        .iter()
        .zip(&items)
        .map(|(appearance, item)| {
            let (source_item, asset) = ids(*item);
            (*appearance, source_item, asset)
        })
        .collect();
    // A file the resolver has not bound: its binding is taken away.
    let loose_item = *items.last().unwrap();
    library
        .write(
            WriteRequest {
                method: "test.unbind",
                command: None,
                client: None,
            },
            move |write| {
                write.connection().execute_batch(&format!(
                    "DROP TRIGGER assignment_no_delete;
                     DELETE FROM assignment WHERE source_item_id = {loose_item};"
                ))?;
                Ok(String::new())
            },
        )
        .unwrap();
    let loose = ids(loose_item);
    let listener = library.default_listener().unwrap().unwrap();
    Library {
        library,
        clock,
        server,
        listener,
        tracks,
        loose,
    }
}

impl Library {
    /// A listen of track `track` (0 to 2), sent in the boot it was heard
    /// in, a minute after it ended.
    fn listen(&self, key: u8, track: usize, start: i64, ms_played: i64) -> ListenRecord {
        let (appearance, source_item, asset) = self.tracks[track];
        ListenRecord {
            v: LISTEN_RECORD_VERSION,
            listen_id: Id::from_bytes([key; 16]),
            listener: self.listener,
            actor_session: None,
            played_on_server: self.server.server_id,
            appearance,
            asset,
            copy_kind: "master".to_owned(),
            source_item,
            source_revision: 1,
            started_at_ms: start,
            zone: Some("America/New_York".to_owned()),
            started_mono_ms: start - T0 + 1_000_000,
            boot: BOOT,
            start_reason: "user_pick".to_owned(),
            continues: None,
            start_position_ms: 0,
            track_duration_ms: ALBUM[track].length_ms,
            ms_played,
            heard_ranges: vec![[0, ms_played]],
            heard_ranges_approximate: false,
            max_position_ms: ms_played,
            seek_count: 0,
            pause_count: 0,
            end_position_ms: ms_played,
            end_reason: "finished".to_owned(),
            ended_at_ms: start + ms_played,
            ended_mono_ms: Some(start - T0 + 1_000_000 + ms_played),
            context_kind: "album".to_owned(),
            context_ref: None,
            shuffle: false,
        }
    }

    fn send(&self, record: &ListenRecord, rule: PlayRule) -> Ingested {
        let sent_at = record.ended_at_ms + MINUTE;
        self.clock.set(sent_at);
        self.library
            .ingest_listen_by(
                record,
                SendStamps {
                    sent_at_ms: sent_at,
                    sent_mono_ms: record.started_mono_ms + (sent_at - record.started_at_ms),
                    sent_boot: BOOT,
                },
                rule,
            )
            .unwrap()
    }

    fn rebuild(&self, rule: PlayRule, dry_run: bool) -> Rebuilt {
        let mut found = None;
        let answer = self
            .library
            .rebuild_history_by(
                WriteRequest {
                    method: "history.rebuild",
                    command: None,
                    client: None,
                },
                dry_run,
                rule,
                |rebuilt| format!("{} {} {}", rebuilt.before, rebuilt.after, rebuilt.changed),
            )
            .unwrap();
        let numbers: Vec<i64> = answer.split(' ').map(|n| n.parse().unwrap()).collect();
        found.get_or_insert(Rebuilt {
            before: numbers[0],
            after: numbers[1],
            changed: numbers[2],
            revision: None,
        });
        found.unwrap()
    }

    fn count(&self, sql: &'static str) -> i64 {
        self.library
            .read(|connection| Ok(connection.query_row(sql, [], |row| row.get(0))?))
            .unwrap()
    }
}

/// The fixtures: each is the listens a run of the player could send.
fn fixtures(at: &Library) -> Vec<(&'static str, Vec<ListenRecord>)> {
    let mut forty = at.listen(1, 0, T0, 96_000);
    forty.end_reason = "next".to_owned();
    let sixty = at.listen(2, 1, T0 + 5 * MINUTE, 180_000);

    let mut crashed = at.listen(3, 1, T0 + 10 * MINUTE, 155_000);
    crashed.end_reason = "crash".to_owned();
    let mut resumed = at.listen(4, 1, T0 + 14 * MINUTE, 145_000);
    resumed.continues = Some(crashed.listen_id);
    resumed.start_reason = "resume".to_owned();
    resumed.start_position_ms = 155_000;
    resumed.boot = Id::from_bytes([0xb1; 16]);
    resumed.started_mono_ms = 50;

    let repeats: Vec<ListenRecord> = (0..3)
        .map(|pass| {
            at.listen(
                5 + pass,
                0,
                T0 + (20 + 5 * i64::from(pass)) * MINUTE,
                240_000,
            )
        })
        .collect();

    let short = at.listen(8, 2, T0 + 40 * MINUTE, 25_000);

    let mut loose = at.listen(9, 0, T0 + 45 * MINUTE, 200_000);
    loose.source_item = at.loose.0;
    loose.asset = at.loose.1;

    let mut fast = at.listen(10, 0, T0 + 50 * MINUTE + 60 * MINUTE, 200_000);
    fast.started_mono_ms = 2_000_000;

    let mut sets = vec![
        ("40 and 60 percent", vec![forty, sixty]),
        (
            "a crash and its resume",
            vec![crashed.clone(), resumed.clone()],
        ),
        ("the resume first", vec![resumed, crashed]),
        ("three repeats", repeats),
        ("a short track", vec![short]),
        ("a file with no binding", vec![loose]),
        ("a clock an hour fast", vec![fast]),
    ];
    let everything: Vec<ListenRecord> = sets
        .iter()
        .filter(|(name, _)| *name != "the resume first")
        .flat_map(|(_, listens)| listens.clone())
        .collect();
    sets.push(("everything", everything));
    sets
}

#[test]
fn a_full_rebuild_equals_what_ingest_derived_on_every_fixture() {
    let names: Vec<&str> = fixtures(&library()).iter().map(|(name, _)| *name).collect();
    for name in names {
        let at = library();
        let listens = fixtures(&at)
            .into_iter()
            .find(|(fixture, _)| *fixture == name)
            .unwrap()
            .1;
        for listen in &listens {
            assert!(
                matches!(at.send(listen, PlayRule::V1), Ingested::Stored(_)),
                "{name}"
            );
        }
        let incremental = at.library.derived_history().unwrap();
        assert!(!incremental.is_empty(), "{name}");
        // A dry run finds nothing to change, and writes nothing.
        let revision = at.library.meta().unwrap().revision;
        let dry = at.rebuild(PlayRule::V1, true);
        assert_eq!(
            (dry.changed, dry.before, dry.after),
            (0, incremental.len() as i64, incremental.len() as i64),
            "{name}"
        );
        assert_eq!(at.library.meta().unwrap().revision, revision, "{name}");
        let rebuilt = at.rebuild(PlayRule::V1, false);
        assert_eq!(rebuilt.changed, 0, "{name}");
        assert_eq!(at.library.derived_history().unwrap(), incremental, "{name}");
        assert!(!at.library.history_needs_rebuild().unwrap());
    }
}

#[test]
fn after_the_rule_is_bumped_a_rebuild_equals_ingest_under_the_new_rule() {
    // A stricter rule, for the test: three quarters of the track.
    let v2 = PlayRule {
        version: 2,
        share_numerator: 3,
        share_denominator: 4,
        ..PlayRule::V1
    };
    let everything = |at: &Library| fixtures(at).pop().unwrap().1;

    let bumped = library();
    for listen in everything(&bumped) {
        bumped.send(&listen, PlayRule::V1);
    }
    let before = bumped.library.derived_history().unwrap();
    let dry = bumped.rebuild(v2, true);
    assert!(dry.changed > 0);
    let rebuilt = bumped.rebuild(v2, false);
    assert_eq!(rebuilt.changed, before.len() as i64);
    // The 60% listen counted under rule 1 and does not under rule 2.
    assert_eq!(
        bumped.count("SELECT count(*) FROM play_event WHERE play_rule_version = 2"),
        before.len() as i64
    );
    assert!(bumped.library.history_needs_rebuild().unwrap());

    let fresh = library();
    for listen in everything(&fresh) {
        fresh.send(&listen, v2);
    }
    assert_eq!(
        bumped.library.derived_history().unwrap(),
        fresh.library.derived_history().unwrap()
    );
}

#[test]
fn a_listen_of_40_percent_is_stored_and_does_not_count_and_one_of_60_percent_counts() {
    let at = library();
    let (_, listens) = fixtures(&at).into_iter().next().unwrap();
    for listen in &listens {
        at.send(listen, PlayRule::V1);
    }
    let plays = at.library.recent_plays(at.listener, 10, None).unwrap();
    let judged: Vec<(i64, bool, &str)> = plays
        .iter()
        .map(|play| (play.ms_played.unwrap(), play.counts, play.outcome.as_str()))
        .collect();
    assert_eq!(
        judged,
        [(180_000, true, "listened"), (96_000, false, "skipped")]
    );
    assert_eq!(at.count("SELECT count(*) FROM history_observation"), 2);
    assert_eq!(at.count("SELECT count(*) FROM observation_listen"), 2);
}

#[test]
fn a_listen_sent_again_writes_once_and_one_with_another_payload_is_kept_as_a_conflict() {
    let at = library();
    let listen = at.listen(1, 0, T0, 200_000);
    assert!(matches!(
        at.send(&listen, PlayRule::V1),
        Ingested::Stored(_)
    ));
    let revision = at.library.meta().unwrap().revision;
    // Stamped afresh, the same listen is the same listen.
    assert_eq!(at.send(&listen, PlayRule::V1), Ingested::Duplicate);
    assert_eq!(at.library.meta().unwrap().revision, revision);
    let mut other = listen.clone();
    other.ms_played = 1;
    assert_eq!(at.send(&other, PlayRule::V1), Ingested::Conflict);
    assert_eq!(at.count("SELECT count(*) FROM history_observation"), 1);
    assert_eq!(at.count("SELECT count(*) FROM history_ingest_conflict"), 1);
    assert_eq!(at.count("SELECT count(*) FROM play_event"), 1);
}

#[test]
fn a_listen_whose_source_item_has_no_assignment_gives_an_unbound_play_that_binds_later() {
    let at = library();
    let mut loose = at.listen(1, 0, T0, 200_000);
    loose.source_item = at.loose.0;
    loose.asset = at.loose.1;
    at.send(&loose, PlayRule::V1);
    let [play] = at
        .library
        .recent_plays(at.listener, 10, None)
        .unwrap()
        .try_into()
        .unwrap();
    assert_eq!((play.recording, play.appearance), (None, None));
    assert!(play.counts);
    // It shows the track that was queued.
    assert_eq!(play.title.as_deref(), Some("Paper Kites"));
    // Once the item is bound, the play follows it.
    let (appearance, item) = (at.tracks[3].0, at.loose.0);
    at.library
        .write(
            WriteRequest {
                method: "test.bind",
                command: None,
                client: None,
            },
            move |write| {
                write.connection().execute(
                    "INSERT INTO assignment (source_item_id, recording_id, appearance_id, decision_id, effective_from_ms, rev)
                     SELECT si.id, ta.recording_id, ta.id, (SELECT max(id) FROM identity_decision), 0, ?3
                     FROM source_item si, track_appearance ta WHERE si.public_id = ?1 AND ta.public_id = ?2",
                    (item.as_bytes(), appearance.as_bytes(), write.rev()),
                )?;
                rebind(write)?;
                Ok(String::new())
            },
        )
        .unwrap();
    let [play] = at
        .library
        .recent_plays(at.listener, 10, None)
        .unwrap()
        .try_into()
        .unwrap();
    assert_eq!(play.appearance, Some(appearance));
    assert_eq!(play.title.as_deref(), Some("Salt Road"));
    // And a rebuild agrees.
    let incremental = at.library.derived_history().unwrap();
    assert_eq!(at.rebuild(PlayRule::V1, false).changed, 0);
    assert_eq!(at.library.derived_history().unwrap(), incremental);
}

#[test]
fn an_excluded_play_is_kept_and_marked_and_a_rebuild_keeps_it_excluded() {
    let at = library();
    let listen = at.listen(1, 0, T0, 200_000);
    at.send(&listen, PlayRule::V1);
    // An exclude correction, written as the correction call will write it.
    let (server, key) = (at.server.server_id, listen.listen_id);
    at.library
        .write(
            WriteRequest {
                method: "test.exclude",
                command: None,
                client: None,
            },
            move |write| {
                let connection = write.connection();
                connection.execute(
                    "INSERT INTO history_observation
                       (endpoint_id, source_key, kind, listener_id, captured_at_utc_ms,
                        received_at_utc_ms, zone_basis, payload_json, rev)
                     VALUES ((SELECT id FROM history_endpoint WHERE kind = 'manual'), x'0c', 'correction',
                             1, 0, 0, 'none', '{}', ?1)",
                    [write.rev()],
                )?;
                let correction = connection.last_insert_rowid();
                connection.execute(
                    "INSERT INTO observation_correction (observation_id, verb, rev) VALUES (?1, 'exclude', ?2)",
                    (correction, write.rev()),
                )?;
                connection.execute(
                    "INSERT INTO correction_target (correction_id, target_observation_id, rev)
                     SELECT ?1, o.id, ?3 FROM history_observation o
                     JOIN history_endpoint e ON e.id = o.endpoint_id
                     JOIN server s ON s.id = e.server_id
                     WHERE s.public_id = ?2 AND o.source_key = ?4",
                    (correction, server.as_bytes(), write.rev(), key.as_bytes()),
                )?;
                Ok(String::new())
            },
        )
        .unwrap();
    // A correction triggers a derive; here the rebuild is that derive.
    assert_eq!(at.rebuild(PlayRule::V1, false).changed, 1);
    let [play] = at
        .library
        .recent_plays(at.listener, 10, None)
        .unwrap()
        .try_into()
        .unwrap();
    assert!(play.excluded && play.counts);
    assert_eq!(at.count("SELECT count(*) FROM play_effective"), 0);
    assert_eq!(at.rebuild(PlayRule::V1, false).changed, 0);
}

#[test]
fn on_this_day_reads_the_plays_that_count_on_that_month_and_day_of_any_year() {
    let at = library();
    let last_year = at.listen(1, 0, T0 - 365 * 24 * 60 * MINUTE, 200_000);
    let today = at.listen(2, 1, T0, 200_000);
    let mut skipped = at.listen(3, 0, T0 + MINUTE * 10, 10_000);
    skipped.end_reason = "next".to_owned();
    let tomorrow = at.listen(4, 0, T0 + 24 * 60 * MINUTE, 200_000);
    for listen in [&last_year, &today, &skipped, &tomorrow] {
        at.send(listen, PlayRule::V1);
    }
    let plays = at.library.plays_on_this_day(at.listener, 1115).unwrap();
    let dates: Vec<Option<i64>> = plays.iter().map(|play| play.local_date).collect();
    assert_eq!(dates, [Some(20_261_115), Some(20_251_115)]);
    assert_eq!(plays[0].listens.len(), 1);
    assert_eq!(plays[0].listens[0].listen_id, today.listen_id);
}

#[test]
fn a_listen_that_names_what_the_library_does_not_hold_is_refused_and_nothing_is_written() {
    let at = library();
    let mut wrong = at.listen(1, 0, T0, 200_000);
    wrong.source_revision = 7;
    let refused = at.library.ingest_listen(
        &wrong,
        SendStamps {
            sent_at_ms: T0,
            sent_mono_ms: 1,
            sent_boot: BOOT,
        },
    );
    assert!(matches!(refused, Err(Error::BadListen(_))), "{refused:?}");
    wrong.source_revision = 1;
    wrong.end_reason = "fell asleep".to_owned();
    let refused = at.library.ingest_listen(
        &wrong,
        SendStamps {
            sent_at_ms: T0,
            sent_mono_ms: 1,
            sent_boot: BOOT,
        },
    );
    assert!(matches!(refused, Err(Error::BadListen(_))), "{refused:?}");
    assert_eq!(at.count("SELECT count(*) FROM history_observation"), 0);
}

#[test]
fn what_the_sender_knew_of_its_own_date_is_kept_on_the_observation() {
    let at = library();
    let mut fast = at.listen(1, 0, T0 + 4 * 60 * MINUTE, 200_000);
    fast.started_mono_ms = 77;
    at.send(&fast, PlayRule::V1);
    let row: (String, Option<String>, i64, i64, i64, i64) = at
        .library
        .read(|connection| {
            Ok(connection.query_row(
                "SELECT zone_basis, zone_name, utc_offset_minutes, local_date, local_hour,
                        clock_skew_ms
                 FROM history_observation",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )?)
        })
        .unwrap();
    // 01:00 UTC on the 16th is 20:00 on the 15th in New York.
    assert_eq!(
        row,
        (
            "captured".to_owned(),
            Some("America/New_York".to_owned()),
            -300,
            20_261_115,
            20,
            0
        )
    );
}

/// A file scanned, listened to, then retagged and scanned again: the
/// resolver gives it a new appearance, and the play moves with it in the
/// resolver's own transaction, as a rebuild would put it.
#[test]
fn a_play_follows_its_file_when_a_rescan_binds_the_file_elsewhere() {
    let mut rig = crate::scan_tests::Rig::new();
    rig.put("Moss Arcade/Tin Weather/02 Paper Kites.flac", "a.flac");
    rig.scan();
    let library = &rig.library;
    let (source_item, asset, appearance, hub): (Id, Id, Id, Id) = library
        .read(|connection| {
            Ok(connection.query_row(
                "SELECT si.public_id, aa.public_id, ta.public_id,
                        (SELECT s.public_id FROM server s JOIN library_meta m ON m.hub_server_id = s.id)
                 FROM source_item si
                 JOIN source_item_revision sr ON sr.id = si.current_revision_id
                 JOIN audio_asset aa ON aa.id = sr.audio_asset_id
                 JOIN assignment a ON a.source_item_id = si.id AND a.superseded_at_ms IS NULL
                 JOIN track_appearance ta ON ta.id = a.appearance_id",
                [],
                |row| {
                    Ok((
                        Id::from_bytes(row.get(0)?),
                        Id::from_bytes(row.get(1)?),
                        Id::from_bytes(row.get(2)?),
                        Id::from_bytes(row.get(3)?),
                    ))
                },
            )?)
        })
        .unwrap();
    let listener = library.default_listener().unwrap().unwrap();
    let record = ListenRecord {
        v: LISTEN_RECORD_VERSION,
        listen_id: Id::from_bytes([0x71; 16]),
        listener,
        actor_session: None,
        played_on_server: hub,
        appearance,
        asset,
        copy_kind: "master".to_owned(),
        source_item,
        source_revision: 1,
        started_at_ms: T0,
        zone: Some("America/New_York".to_owned()),
        started_mono_ms: 1_000,
        boot: BOOT,
        start_reason: "user_pick".to_owned(),
        continues: None,
        start_position_ms: 0,
        track_duration_ms: 500,
        ms_played: 500,
        heard_ranges: vec![[0, 500]],
        heard_ranges_approximate: false,
        max_position_ms: 500,
        seek_count: 0,
        pause_count: 0,
        end_position_ms: 500,
        end_reason: "finished".to_owned(),
        ended_at_ms: T0 + 500,
        ended_mono_ms: Some(1_500),
        context_kind: "album".to_owned(),
        context_ref: None,
        shuffle: false,
    };
    let stamps = SendStamps {
        sent_at_ms: T0 + 1_000,
        sent_mono_ms: 2_000,
        sent_boot: BOOT,
    };
    assert!(matches!(
        rig.library.ingest_listen(&record, stamps).unwrap(),
        Ingested::Stored(_)
    ));
    let shown = |rig: &crate::scan_tests::Rig| {
        let [play] = rig
            .library
            .recent_plays(listener, 10, None)
            .unwrap()
            .try_into()
            .unwrap();
        (play.appearance, play.title)
    };
    assert_eq!(
        shown(&rig),
        (Some(appearance), Some("Paper Kites".to_owned()))
    );

    rig.put(
        "Moss Arcade/Tin Weather/02 Paper Kites.flac",
        "a.retagged.flac",
    );
    rig.scan();
    let (moved_to, title) = shown(&rig);
    assert_ne!(moved_to, Some(appearance));
    assert!(moved_to.is_some());
    assert_eq!(title.as_deref(), Some("Another Name Entirely"));
    // What the resolver's transaction left is what a rebuild derives.
    let incremental = rig.library.derived_history().unwrap();
    rig.library
        .rebuild_history(
            WriteRequest {
                method: "history.rebuild",
                command: None,
                client: None,
            },
            false,
            |rebuilt| rebuilt.changed.to_string(),
        )
        .map(|changed| assert_eq!(changed, "0"))
        .unwrap();
    assert_eq!(rig.library.derived_history().unwrap(), incremental);
}

/// A `library.db` as the second migration left it, with nine listens of the
/// invented album and the eight plays derived from them.
const SAVED_LIBRARY_0002: &str = include_str!("../../saved/library/0002.sql");

fn schema_of(connection: &Connection) -> Vec<String> {
    connection
        .prepare("SELECT sql FROM sqlite_schema WHERE sql IS NOT NULL ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[test]
fn the_saved_library_with_listens_is_one_the_first_two_migrations_made() {
    let saved = Connection::open_in_memory().unwrap();
    saved.execute_batch(SAVED_LIBRARY_0002).unwrap();
    let mut made = Connection::open_in_memory().unwrap();
    crate::migrate::run(
        &mut made,
        crate::migrate::LIBRARY.get(..2).unwrap(),
        &FixedClock::at(T0),
    )
    .unwrap();
    assert_eq!(schema_of(&saved), schema_of(&made));
}

#[test]
fn a_library_saved_with_listens_is_taken_forward_with_its_listens_and_plays() {
    let scratch = crate::testing::Scratch::new();
    let path = scratch.path().join("library.db");
    let saved = Connection::open(&path).unwrap();
    saved.execute_batch(SAVED_LIBRARY_0002).unwrap();
    let count = |connection: &Connection, table: &str| -> i64 {
        connection
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap()
    };
    let tables = [
        "history_observation",
        "observation_listen",
        "play_event",
        "play_event_observation",
    ];
    let before: Vec<i64> = tables.iter().map(|table| count(&saved, table)).collect();
    assert_eq!(before, [9, 9, 8, 9]);
    let derived_before = derived(&saved).unwrap();
    let listens_before: Vec<String> = saved
        .prepare(
            "SELECT observation_id || '|' || ms_played || '|' || end_reason || '|' ||
                    heard_ranges_json || '|' || ifnull(hex(continues_source_key), '')
             FROM observation_listen ORDER BY observation_id",
        )
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    drop(saved);

    // The server that made it, made again the same way, a month on.
    let clock = Arc::new(FixedClock::at(T0));
    let entropy = Arc::new(CountingEntropy::default());
    let servers = ServerStore::open_in_memory(clock.clone(), entropy.clone()).unwrap();
    let (server, _) = servers.ensure_identity("desk").unwrap();
    clock.set(T0 + 30 * 24 * 60 * MINUTE);
    let library = LibraryStore::open(&path, server.server_id, clock.clone(), entropy).unwrap();
    assert_eq!(
        (library.migrations_applied(), library.schema_version()),
        (3, 5)
    );

    library
        .read(|connection| {
            // Every row is there, as it was.
            let after: Vec<i64> = tables
                .iter()
                .map(|table| count(connection, table))
                .collect();
            assert_eq!(after, before);
            assert_eq!(derived(connection)?, derived_before);
            let listens_after: Vec<String> = connection
                .prepare(
                    "SELECT observation_id || '|' || ms_played || '|' || end_reason || '|' ||
                            heard_ranges_json || '|' || ifnull(hex(continues_source_key), '')
                     FROM observation_listen ORDER BY observation_id",
                )?
                .query_map([], |row| row.get(0))?
                .collect::<Result<_, _>>()?;
            assert_eq!(listens_after, listens_before);
            // No listen then kept its end clock, and none was approximate,
            // as each stored record says.
            let (no_end_clock, approximate): (i64, i64) = connection.query_row(
                "SELECT count(*) FILTER (WHERE ended_mono_ms IS NULL),
                        count(*) FILTER (WHERE heard_ranges_approximate = 1)
                 FROM observation_listen",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            assert_eq!((no_end_clock, approximate), (9, 0));
            // Nothing points nowhere, and the file is whole.
            let dangling: i64 = connection.query_row(
                "SELECT count(*) FROM pragma_foreign_key_check",
                [],
                |row| row.get(0),
            )?;
            let whole: String =
                connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
            assert_eq!((dangling, whole.as_str()), (0, "ok"));
            // A listen is still immutable.
            assert!(
                connection
                    .execute("UPDATE observation_listen SET ms_played = 1", [])
                    .is_err()
            );
            assert!(
                connection
                    .execute("DELETE FROM observation_listen", [])
                    .is_err()
            );
            // The listen a resume continues is found by the new index.
            let plan: Vec<String> = connection
                .prepare(
                    "EXPLAIN QUERY PLAN SELECT observation_id FROM observation_listen
                     WHERE continues_source_key = x'00'",
                )?
                .query_map([], |row| row.get(3))?
                .collect::<Result<_, _>>()?;
            assert!(
                plan.iter()
                    .any(|step| step.contains("observation_listen_continues")),
                "{plan:?}"
            );
            // The tables are the ones a new library is made with.
            let fresh = Connection::open_in_memory()?;
            let mut fresh = fresh;
            crate::migrate::run(&mut fresh, crate::migrate::LIBRARY, &FixedClock::at(T0))?;
            assert_eq!(schema_of(connection), schema_of(&fresh));
            Ok(())
        })
        .unwrap();

    // The plays carried across are the plays the listens derive.
    let rebuilt = library
        .rebuild_history(
            WriteRequest {
                method: "history.rebuild",
                command: None,
                client: None,
            },
            true,
            |rebuilt| format!("{}|{}|{}", rebuilt.before, rebuilt.after, rebuilt.changed),
        )
        .unwrap();
    assert_eq!(rebuilt, "8|8|0");

    // A listen begun from an artist's page that lost its output is taken.
    let stored: String = library
        .read(|connection| {
            Ok(connection.query_row(
                "SELECT payload_json FROM history_observation ORDER BY id LIMIT 1",
                [],
                |row| row.get(0),
            )?)
        })
        .unwrap();
    let mut record: ListenRecord = serde_json::from_str(&stored).unwrap();
    record.listen_id = Id::from_bytes([0xe1; 16]);
    record.continues = None;
    record.context_kind = "artist".to_owned();
    record.end_reason = "output_lost".to_owned();
    record.ended_mono_ms = Some(record.started_mono_ms + record.ms_played);
    let stamps = SendStamps {
        sent_at_ms: T0 + 30 * 24 * 60 * MINUTE,
        sent_mono_ms: record.started_mono_ms + 60 * MINUTE,
        sent_boot: record.boot,
    };
    assert!(matches!(
        library.ingest_listen(&record, stamps).unwrap(),
        Ingested::Stored(_)
    ));
}

#[test]
fn a_wait_for_the_writer_is_added_neither_to_a_plays_date_nor_to_the_senders_skew() {
    let at = library();
    let record = at.listen(1, 0, T0, 200_000);
    // The ingest waits a minute for the library's writer.
    at.clock
        .writer_lag_ms
        .store(MINUTE, std::sync::atomic::Ordering::SeqCst);
    assert!(matches!(
        at.send(&record, PlayRule::CURRENT),
        Ingested::Stored(_)
    ));
    let sent_at = record.ended_at_ms + MINUTE;
    let (received, skew): (i64, i64) = at
        .library
        .read(|connection| {
            Ok(connection.query_row(
                "SELECT received_at_utc_ms, clock_skew_ms FROM history_observation",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?)
        })
        .unwrap();
    assert_eq!((received, skew), (sent_at, 0));
    let plays = at.library.recent_plays(at.listener, 10, None).unwrap();
    assert_eq!(
        (plays[0].at_ms, plays[0].clock_basis.as_str()),
        (Some(T0), "hub_age")
    );
}

#[test]
fn recent_history_pages_through_plays_that_share_an_instant() {
    let at = library();
    // Two plays that began in the same millisecond, and one before them.
    at.send(&at.listen(1, 0, T0, 200_000), PlayRule::CURRENT);
    at.send(&at.listen(2, 1, T0, 200_000), PlayRule::CURRENT);
    at.send(
        &at.listen(3, 2, T0 - 10 * MINUTE, 20_000),
        PlayRule::CURRENT,
    );
    let all = at.library.recent_plays(at.listener, 10, None).unwrap();
    assert_eq!(all.len(), 3);
    // A page at a time, each from where the last one ended.
    let mut paged = Vec::new();
    let mut before = None;
    loop {
        let page = at.library.recent_plays(at.listener, 1, before).unwrap();
        let Some(last) = page.last() else {
            break;
        };
        before = Some(last.cursor());
        paged.extend(page);
        assert!(paged.len() <= 3, "a page came round again");
    }
    let ids = |plays: &[crate::PlayRow]| plays.iter().map(|play| play.play_id).collect::<Vec<_>>();
    assert_eq!(ids(&paged), ids(&all));
}

/// A `library.db` as the third migration left it: the library of
/// [`SAVED_LIBRARY_0002`] taken forward one migration.
const SAVED_LIBRARY_0003: &str = include_str!("../../saved/library/0003.sql");

#[test]
fn the_saved_library_at_the_third_migration_is_one_the_first_three_made() {
    let saved = Connection::open_in_memory().unwrap();
    saved.execute_batch(SAVED_LIBRARY_0003).unwrap();
    let mut made = Connection::open_in_memory().unwrap();
    crate::migrate::run(
        &mut made,
        crate::migrate::LIBRARY.get(..3).unwrap(),
        &FixedClock::at(T0),
    )
    .unwrap();
    assert_eq!(schema_of(&saved), schema_of(&made));
}

#[test]
fn a_library_saved_at_the_third_migration_keeps_its_decisions_and_plays_through_the_fourth() {
    let scratch = crate::testing::Scratch::new();
    let path = scratch.path().join("library.db");
    let saved = Connection::open(&path).unwrap();
    saved.execute_batch(SAVED_LIBRARY_0003).unwrap();
    let rows = |connection: &Connection, sql: &str| -> Vec<String> {
        connection
            .prepare(sql)
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    const DECISIONS: &str =
        "SELECT id || '|' || action || '|' || ifnull(source_item_id, '') || '|' ||
                method || '|' || evidence_json || '|' || rev FROM identity_decision ORDER BY id";
    const ASSIGNED: &str = "SELECT id || '|' || decision_id FROM assignment ORDER BY id";
    let decisions = rows(&saved, DECISIONS);
    let assigned = rows(&saved, ASSIGNED);
    assert_eq!((decisions.len(), assigned.len()), (4, 4));
    let derived_before = derived(&saved).unwrap();
    drop(saved);

    let clock = Arc::new(FixedClock::at(T0));
    let entropy = Arc::new(CountingEntropy::default());
    let servers = ServerStore::open_in_memory(clock.clone(), entropy.clone()).unwrap();
    let (server, _) = servers.ensure_identity("desk").unwrap();
    let library = LibraryStore::open(&path, server.server_id, clock.clone(), entropy).unwrap();
    assert_eq!(
        (library.migrations_applied(), library.schema_version()),
        (2, 5)
    );
    library
        .read(|connection| {
            assert_eq!(rows(connection, DECISIONS), decisions);
            assert_eq!(rows(connection, ASSIGNED), assigned);
            assert_eq!(derived(connection)?, derived_before);
            // The rebuilt table keeps its triggers, and what points at it
            // points at it.
            let dangling: i64 = connection.query_row(
                "SELECT count(*) FROM pragma_foreign_key_check",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(dangling, 0);
            let refused = connection.execute("DELETE FROM identity_decision", []);
            assert!(refused.is_err());
            Ok(())
        })
        .unwrap();
}

/// A `library.db` as the fourth migration left it: the library of
/// [`SAVED_LIBRARY_0003`] taken forward one migration.
const SAVED_LIBRARY_0004: &str = include_str!("../../saved/library/0004.sql");

#[test]
fn the_saved_library_at_the_fourth_migration_is_one_the_first_four_made() {
    let saved = Connection::open_in_memory().unwrap();
    saved.execute_batch(SAVED_LIBRARY_0004).unwrap();
    let mut made = Connection::open_in_memory().unwrap();
    crate::migrate::run(
        &mut made,
        crate::migrate::LIBRARY.get(..4).unwrap(),
        &FixedClock::at(T0),
    )
    .unwrap();
    assert_eq!(schema_of(&saved), schema_of(&made));
}

#[test]
fn a_library_saved_at_the_fourth_migration_keeps_what_it_holds_through_the_fifth() {
    let scratch = crate::testing::Scratch::new();
    let path = scratch.path().join("library.db");
    let saved = Connection::open(&path).unwrap();
    saved.execute_batch(SAVED_LIBRARY_0004).unwrap();
    let derived_before = derived(&saved).unwrap();
    drop(saved);

    let clock = Arc::new(FixedClock::at(T0));
    let entropy = Arc::new(CountingEntropy::default());
    let servers = ServerStore::open_in_memory(clock.clone(), entropy.clone()).unwrap();
    let (server, _) = servers.ensure_identity("desk").unwrap();
    let library = LibraryStore::open(&path, server.server_id, clock, entropy).unwrap();
    assert_eq!(
        (library.migrations_applied(), library.schema_version()),
        (1, 5)
    );
    library
        .read(|connection| {
            assert_eq!(derived(connection)?, derived_before);
            let pictures: i64 = connection.query_row(
                "SELECT (SELECT count(*) FROM picture_folder) + (SELECT count(*) FROM artist_picture)",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(pictures, 0);
            Ok(())
        })
        .unwrap();
    let albums = library
        .list_open(ritornel_contract::ListKind::Albums, &Default::default())
        .unwrap();
    assert_eq!(albums.ids.len(), 1);
}

/// A Last.fm account on the rig's library, and an import of one scrobble of
/// it.
fn scrobble_of(at: &Library, account: Id, at_s: i64, artist: &str, title: &str) {
    let mut key = b"echo".to_vec();
    key.extend_from_slice(&at_s.to_be_bytes());
    let batch = ritornel_source::Batch {
        records: vec![ritornel_source::ImportRecord {
            source_key: key,
            played_at_ms: at_s * 1000,
            track: ritornel_source::TrackText {
                artist: artist.to_owned(),
                album: Some("Tin Weather".to_owned()),
                title: title.to_owned(),
            },
            ids: ritornel_source::Identifiers::default(),
            payload_json: "{}".to_owned(),
        }],
        span: ritornel_source::Span {
            from_ms: at_s * 1000,
            to_ms: at_s * 1000 + 1000,
        },
        cursor: "{}".to_owned(),
    };
    at.library
        .ingest(Observations::Imports {
            account,
            batch: &batch,
        })
        .unwrap();
}

#[test]
fn a_counted_listen_is_recorded_before_it_is_sent_once_and_its_echo_joins_its_play() {
    use crate::Settled;

    let at = library();
    let account = at
        .library
        .ensure_service_account("lastfm", "someone")
        .unwrap();
    let start = T0 + 10 * MINUTE;
    // A 60% play counts; a 40% skip does not; a play from before scrobbling
    // was turned on is not sent.
    let counted = at.listen(1, 0, start, 144_000);
    let skipped = at.listen(2, 1, start + 5 * MINUTE, 120_000);
    let before = at.listen(3, 0, start - 5 * MINUTE, 200_000);
    for listen in [&counted, &skipped, &before] {
        at.send(listen, PlayRule::V1);
    }
    let from = start - MINUTE;
    assert_eq!(at.library.plan_deliveries(account, from).unwrap(), 1);
    let revision = at.library.meta().unwrap().revision;
    assert_eq!(at.library.plan_deliveries(account, from).unwrap(), 0);
    assert_eq!(at.library.meta().unwrap().revision, revision);

    let due = at.library.take_due_deliveries(account, 0, 50).unwrap();
    assert_eq!(due.len(), 1);
    let sent = &due[0];
    assert_eq!(
        (
            sent.artist.as_str(),
            sent.title.as_str(),
            sent.album.as_deref()
        ),
        ("Moss Arcade", "Paper Kites", Some("Tin Weather"))
    );
    assert_eq!(sent.timestamp_s, start / 1000);
    assert_eq!(sent.duration_ms, Some(240_000));
    // Taken once, it is not taken again.
    assert!(
        at.library
            .take_due_deliveries(account, 0, 50)
            .unwrap()
            .is_empty()
    );
    at.library
        .settle_deliveries(vec![(
            sent.delivery,
            Settled::Accepted {
                artist: "Moss Arcade".to_owned(),
                title: "Paper Kites".to_owned(),
            },
        )])
        .unwrap();
    assert_eq!(
        at.library.delivery_counts(account).unwrap(),
        [("accepted".to_owned(), 1)]
    );

    // The echo comes back through the import, as Last.fm spells it.
    let plays_before = at.count("SELECT count(*) FROM play_event");
    scrobble_of(&at, account, start / 1000, "moss arcade", "Paper Kites");
    assert_eq!(at.count("SELECT count(*) FROM play_event"), plays_before);
    assert_eq!(
        at.count(
            "SELECT count(*) FROM play_event_observation WHERE role = 'corroborating'
             AND observation_id IN (SELECT id FROM history_observation WHERE kind = 'import')"
        ),
        1
    );
    // A play with the account's own observation is never sent there again.
    assert_eq!(at.library.plan_deliveries(account, from).unwrap(), 0);
    // A scrobble of the same second naming another track is not the echo.
    scrobble_of(&at, account, start / 1000 + 1, "Moss Arcade", "Lanternfish");
    assert_eq!(
        at.count("SELECT count(*) FROM play_event"),
        plays_before + 1
    );

    let incremental = at.library.derived_history().unwrap();
    at.rebuild(PlayRule::V1, false);
    assert_eq!(at.library.derived_history().unwrap(), incremental);
}

#[test]
fn a_delivery_a_run_left_sending_is_unknown_and_never_sent_again() {
    use crate::Settled;

    let at = library();
    let account = at
        .library
        .ensure_service_account("lastfm", "someone")
        .unwrap();
    let start = T0 + 10 * MINUTE;
    at.send(&at.listen(1, 0, start, 200_000), PlayRule::V1);
    at.send(&at.listen(2, 1, start + 5 * MINUTE, 300_000), PlayRule::V1);
    assert_eq!(at.library.plan_deliveries(account, 0).unwrap(), 2);
    let due = at.library.take_due_deliveries(account, 0, 1).unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(at.library.abandon_sending().unwrap(), 1);
    // The other is sent, refused for now, and due again later.
    let later = at.library.take_due_deliveries(account, 0, 50).unwrap();
    assert_eq!(later.len(), 1);
    at.library
        .settle_deliveries(vec![(
            later[0].delivery,
            Settled::Later {
                detail: "offline".to_owned(),
                at_ms: start + 60 * MINUTE,
            },
        )])
        .unwrap();
    assert!(
        at.library
            .take_due_deliveries(account, 0, 50)
            .unwrap()
            .is_empty()
    );
    at.clock.set(start + 61 * MINUTE);
    assert_eq!(
        at.library
            .take_due_deliveries(account, 0, 50)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        at.library.delivery_counts(account).unwrap(),
        [("sending".to_owned(), 1), ("unknown".to_owned(), 1)]
    );
    // A play older than the service takes is not eligible.
    at.clock.set(start + crate::SEND_WITHIN_MS + 30 * MINUTE);
    at.send(&at.listen(3, 2, start + 30 * MINUTE, 25_000), PlayRule::V1);
    assert_eq!(at.library.plan_deliveries(account, 0).unwrap(), 0);
}

/// Excludes the play of the listen with this key, as the correction call
/// will, and derives again.
fn exclude_listen(at: &Library, key: Id) {
    let server = at.server.server_id;
    at.library
        .write(
            WriteRequest {
                method: "test.exclude",
                command: None,
                client: None,
            },
            move |write| {
                let connection = write.connection();
                connection.execute(
                    "INSERT INTO history_observation
                       (endpoint_id, source_key, kind, listener_id, captured_at_utc_ms,
                        received_at_utc_ms, zone_basis, payload_json, rev)
                     VALUES ((SELECT id FROM history_endpoint WHERE kind = 'manual'), ?2,
                             'correction', 1, 0, 0, 'none', '{}', ?1)",
                    (write.rev(), key.as_bytes()),
                )?;
                let correction = connection.last_insert_rowid();
                connection.execute(
                    "INSERT INTO observation_correction (observation_id, verb, rev)
                     VALUES (?1, 'exclude', ?2)",
                    (correction, write.rev()),
                )?;
                connection.execute(
                    "INSERT INTO correction_target (correction_id, target_observation_id, rev)
                     SELECT ?1, o.id, ?3 FROM history_observation o
                     JOIN history_endpoint e ON e.id = o.endpoint_id
                     JOIN server s ON s.id = e.server_id
                     WHERE s.public_id = ?2 AND o.source_key = ?4",
                    (correction, server.as_bytes(), write.rev(), key.as_bytes()),
                )?;
                Ok(String::new())
            },
        )
        .unwrap();
    at.rebuild(PlayRule::V1, false);
}

#[test]
fn a_queued_play_that_is_no_longer_eligible_is_withdrawn_and_never_sent() {
    let at = library();
    let account = at
        .library
        .ensure_service_account("lastfm", "someone")
        .unwrap();
    let start = T0 + 10 * MINUTE;
    let excluded = at.listen(1, 0, start, 200_000);
    let aged = at.listen(2, 1, start + 5 * MINUTE, 300_000);
    for listen in [&excluded, &aged] {
        at.send(listen, PlayRule::V1);
    }
    assert_eq!(at.library.plan_deliveries(account, 0).unwrap(), 2);
    // Excluded after it was queued: it is not sent.
    exclude_listen(&at, excluded.listen_id);
    let due = at.library.take_due_deliveries(account, 0, 50).unwrap();
    assert_eq!(
        due.iter().map(|due| due.title.as_str()).collect::<Vec<_>>(),
        ["Lanternfish"]
    );
    assert_eq!(
        at.count("SELECT count(*) FROM history_delivery WHERE state = 'withdrawn'"),
        1
    );
    // Too old by the time it is due: not sent.
    at.library
        .settle_deliveries(vec![(
            due[0].delivery,
            crate::Settled::Later {
                detail: "offline".to_owned(),
                at_ms: start + 30 * 24 * 60 * MINUTE,
            },
        )])
        .unwrap();
    at.clock.set(start + 30 * 24 * 60 * MINUTE);
    assert!(
        at.library
            .take_due_deliveries(account, 0, 50)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        at.count("SELECT count(*) FROM history_delivery WHERE state = 'withdrawn'"),
        2
    );
    assert_eq!(
        at.count("SELECT count(*) FROM history_delivery WHERE state = 'sending'"),
        0
    );
}

#[test]
fn a_queued_play_from_before_scrobbling_was_turned_on_again_is_withdrawn() {
    let at = library();
    let account = at
        .library
        .ensure_service_account("lastfm", "someone")
        .unwrap();
    let start = T0 + 10 * MINUTE;
    at.send(&at.listen(1, 0, start, 200_000), PlayRule::V1);
    assert_eq!(at.library.plan_deliveries(account, 0).unwrap(), 1);
    // Scrobbling turned off and on again after the play: from then on.
    let cutoff = start + 20 * MINUTE;
    assert!(
        at.library
            .take_due_deliveries(account, cutoff, 50)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        at.count("SELECT count(*) FROM history_delivery WHERE state = 'withdrawn'"),
        1
    );
}

#[test]
fn an_echo_imported_while_its_scrobble_was_sending_joins_its_play_once_the_answer_is_recorded() {
    use crate::Settled;

    let at = library();
    let account = at
        .library
        .ensure_service_account("lastfm", "someone")
        .unwrap();
    let start = T0 + 10 * MINUTE;
    at.send(&at.listen(1, 0, start, 200_000), PlayRule::V1);
    at.library.plan_deliveries(account, 0).unwrap();
    let [sent] = at
        .library
        .take_due_deliveries(account, 0, 50)
        .unwrap()
        .try_into()
        .unwrap();
    // Last.fm takes it under a corrected title, and a sync running at the
    // same time brings that echo back before the answer is recorded.
    scrobble_of(
        &at,
        account,
        start / 1000,
        "Moss Arcade",
        "Paper Kites (Remastered)",
    );
    assert_eq!(at.count("SELECT count(*) FROM play_event"), 2);
    at.library
        .settle_deliveries(vec![(
            sent.delivery,
            Settled::Accepted {
                artist: "Moss Arcade".to_owned(),
                title: "Paper Kites (Remastered)".to_owned(),
            },
        )])
        .unwrap();
    // The answer names the echo: it joins the play, and counts once.
    assert_eq!(at.count("SELECT count(*) FROM play_event"), 1);
    let incremental = at.library.derived_history().unwrap();
    at.rebuild(PlayRule::V1, false);
    assert_eq!(at.library.derived_history().unwrap(), incremental);
}

#[test]
fn a_play_another_scrobbler_already_sent_to_the_account_is_not_sent_again() {
    let at = library();
    let account = at
        .library
        .ensure_service_account("lastfm", "someone")
        .unwrap();
    let start = T0 + 10 * MINUTE;
    let first = at.listen(1, 0, start, 200_000);
    let second = at.listen(2, 1, start + 10 * MINUTE, 300_000);
    at.send(&first, PlayRule::V1);
    // Another player scrobbled the first play itself, a second later.
    scrobble_of(&at, account, start / 1000 + 1, "moss arcade", "PAPER KITES");
    // Before it is queued: it is not.
    assert_eq!(at.library.plan_deliveries(account, 0).unwrap(), 0);
    // After it is queued: it is not sent.
    at.send(&second, PlayRule::V1);
    assert_eq!(at.library.plan_deliveries(account, 0).unwrap(), 1);
    scrobble_of(
        &at,
        account,
        (start + 10 * MINUTE) / 1000,
        "Moss Arcade",
        "Lanternfish",
    );
    assert!(
        at.library
            .take_due_deliveries(account, 0, 50)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        at.count(
            "SELECT count(*) FROM history_delivery WHERE state = 'withdrawn'
             AND detail LIKE '%already%'"
        ),
        2
    );
    // A play of another track in the same second is not that play.
    let third = at.listen(3, 2, start + 30 * MINUTE, 25_000);
    let fourth = at.listen(4, 0, start + 40 * MINUTE, 200_000);
    at.send(&third, PlayRule::V1);
    at.send(&fourth, PlayRule::V1);
    scrobble_of(
        &at,
        account,
        (start + 40 * MINUTE) / 1000,
        "Moss Arcade",
        "Salt Road",
    );
    assert_eq!(at.library.plan_deliveries(account, 0).unwrap(), 1);
}
