//! History in `library.db`: ingest, derivation in the same transaction,
//! and the rebuild, which must give exactly what incremental ingest gave,
//! on every fixture and after the rule changes. See "Incremental updates and
//! rebuilds" in docs/internals/history.md.
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
fn an_old_derived_zone_is_rebuilt_from_the_original_observation() {
    let at = library();
    let mut listen = at.listen(1, 0, T0, 60_000);
    listen.zone = Some("Invented/Harbour".into());
    at.send(&listen, PlayRule::CURRENT);
    at.library.write(WriteRequest { method: "test.previous_derivation", command: None, client: None }, |write| {
        write.connection().execute("UPDATE play_event SET grouper_version = 1, zone_name = 'Invented/Harbour', zone_basis = 'captured'", [])?;
        Ok(String::new())
    }).unwrap();
    assert!(at.library.history_needs_rebuild().unwrap());
    at.rebuild(PlayRule::CURRENT, false);
    assert_eq!(
        at.count("SELECT count(*) FROM play_event WHERE zone_basis = 'none' AND zone_name IS NULL"),
        1
    );
    assert!(!at.library.history_needs_rebuild().unwrap());
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

/// Listens a build that kept no monotonic clock at a checkpoint or an end
/// staged, as a database from then still holds them: a crash it left open,
/// with no clock reading at its last checkpoint, its resume in the next
/// boot and a listen of its own, both closed and waiting for the hub with
/// no end clock. The next run closes the open one as a crash, and all three
/// are delivered. They reach the library with no end clock, as every listen
/// it took from such a build has, and make the plays they made then.
#[test]
fn listens_staged_with_no_clock_readings_are_closed_delivered_and_make_the_plays_they_did() {
    let at = library();
    let servers =
        ServerStore::open_in_memory(at.clock.clone(), Arc::new(CountingEntropy::default()))
            .unwrap();
    let (crash, resume, whole) = (
        Id::from_bytes([0xe3; 16]),
        Id::from_bytes([0xe4; 16]),
        Id::from_bytes([0xe5; 16]),
    );
    let (second, first) = (at.tracks[1], at.tracks[0]);
    let (listener, server) = (at.listener, at.server.server_id);
    let (boot, next_boot) = (BOOT, Id::from_bytes([0xb1; 16]));
    struct Old {
        listen: Id,
        track: (Id, Id, Id),
        length: i64,
        boot: Id,
        continues: Option<Id>,
        start: i64,
        mono: i64,
        from: i64,
        heard: i64,
        /// None for the one left open.
        end_reason: Option<&'static str>,
    }
    let listens = [
        Old {
            listen: crash,
            track: second,
            length: 300_000,
            boot,
            continues: None,
            start: T0,
            mono: 1_600_000,
            from: 0,
            heard: 155_000,
            end_reason: None,
        },
        Old {
            listen: resume,
            track: second,
            length: 300_000,
            boot: next_boot,
            continues: Some(crash),
            start: T0 + 240_000,
            mono: 50,
            from: 155_000,
            heard: 145_000,
            end_reason: Some("finished"),
        },
        Old {
            listen: whole,
            track: first,
            length: 240_000,
            boot,
            continues: None,
            start: T0 + 600_000,
            mono: 2_200_000,
            from: 0,
            heard: 240_000,
            end_reason: Some("finished"),
        },
    ];
    servers
        .transact(move |transaction, _, _| {
            for old in listens {
                let (appearance, item, asset) = old.track;
                let (to, last) = (old.from + old.heard, old.start + old.heard);
                // Closed: its last checkpoint its end. Open: its last
                // checkpoint where it was when the run died.
                let ended = old.end_reason.map(|_| (to, last));
                transaction.execute(
                    "INSERT INTO listen_staging (
                       listen_id, queue_entry_pid, listener_pid, played_on_server_pid,
                       appearance_pid, asset_pid, copy_kind, source_item_pid,
                       source_revision_no, started_at_utc_ms, zone_name, started_mono_ms,
                       boot_id, start_reason, continues_listen_id, start_position_ms,
                       track_duration_ms, ms_played, heard_ranges_json, max_position_ms,
                       context_kind, shuffle, checkpoint_position_ms, checkpoint_at_ms,
                       checkpoint_mono_ms, end_position_ms, end_reason, ended_at_utc_ms,
                       ended_mono_ms)
                     VALUES (?1, NULL, ?2, ?3, ?4, ?5, 'master', ?6, 1, ?7, 'America/New_York',
                       ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, 'album', 0, ?16, ?17, NULL,
                       ?18, ?19, ?20, NULL)",
                    rusqlite::params![
                        old.listen.as_bytes(),
                        listener.as_bytes(),
                        server.as_bytes(),
                        appearance.as_bytes(),
                        asset.as_bytes(),
                        item.as_bytes(),
                        old.start,
                        old.mono,
                        old.boot.as_bytes(),
                        if old.continues.is_some() {
                            "resume"
                        } else {
                            "user_pick"
                        },
                        old.continues.map(|id| id.as_bytes().to_vec()),
                        old.from,
                        old.length,
                        old.heard,
                        format!("[[{},{to}]]", old.from),
                        to,
                        last,
                        ended.map(|(position, _)| position),
                        old.end_reason,
                        ended.map(|(_, at)| at),
                    ],
                )?;
                if ended.is_some() {
                    transaction.execute(
                        "INSERT INTO outbox (kind, dedupe_key, next_attempt_at_ms, created_at_ms)
                         VALUES ('history.ingest', ?1, ?2, ?2)",
                        (old.listen.as_bytes(), last),
                    )?;
                }
            }
            Ok(())
        })
        .unwrap();

    // The next run closes the open one as a crash, where and when its last
    // checkpoint was, with no clock reading to give.
    at.clock.set(T0 + 800_000);
    assert_eq!(servers.close_crashed_listens().unwrap(), [crash]);
    let closed = servers.staged_listen(crash).unwrap().unwrap();
    let ended = closed.ended.unwrap();
    assert_eq!(
        (
            ended.end_reason,
            ended.end_position_ms,
            ended.ended_at_ms,
            ended.ended_mono_ms
        ),
        (
            ritornel_domain::history::EndReason::Crash,
            155_000,
            T0 + 155_000,
            None
        )
    );
    assert_eq!(
        (closed.heard.ms_played, closed.checkpoint_mono_ms),
        (155_000, None)
    );
    assert_eq!(servers.undelivered_listens().unwrap(), 3);

    let sent_at = T0 + 900_000;
    at.clock.set(sent_at);
    let delivered = crate::playback::deliver_listens(
        &servers,
        &at.library,
        &*at.clock,
        &|| SendStamps {
            sent_at_ms: sent_at,
            sent_mono_ms: 50 + (sent_at - (T0 + 240_000)),
            sent_boot: next_boot,
        },
        &|| Ok(()),
    )
    .unwrap();
    assert_eq!(
        (delivered.delivered.len(), delivered.failed),
        (3, Vec::new())
    );
    assert_eq!(servers.undelivered_listens().unwrap(), 0);

    // Stored as such a build's listens were: no end clock, nothing
    // approximate, and records that say neither.
    let stored: (i64, i64, i64) = at
        .library
        .read(|connection| {
            Ok(connection.query_row(
                "SELECT (SELECT count(*) FROM observation_listen
                         WHERE ended_mono_ms IS NULL AND heard_ranges_approximate = 0),
                        (SELECT count(*) FROM history_observation
                         WHERE payload_json LIKE '%endedMonoMs%'
                            OR payload_json LIKE '%heardRangesApproximate%'),
                        (SELECT count(*) FROM play_event)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?)
        })
        .unwrap();
    assert_eq!(stored, (3, 0, 2));

    // The crash and its resume, in two boots with no end clock between
    // them, are one play, joined by their dates; the other is its own.
    let plays = at.library.recent_plays(at.listener, 10, None).unwrap();
    let shape: Vec<(Option<i64>, usize)> = plays
        .iter()
        .map(|play| (play.ms_played, play.listens.len()))
        .collect();
    assert_eq!(shape, [(Some(240_000), 1), (Some(300_000), 2)]);

    // And a rebuild derives exactly what ingest did.
    let incremental = at.library.derived_history().unwrap();
    at.library
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
    assert_eq!(at.library.derived_history().unwrap(), incremental);
}

#[test]
fn a_listen_begun_from_an_artists_page_that_lost_its_output_is_taken() {
    let at = library();
    let mut record = at.listen(1, 0, T0, 200_000);
    record.context_kind = "artist".to_owned();
    record.end_reason = "output_lost".to_owned();
    assert!(matches!(
        at.send(&record, PlayRule::CURRENT),
        Ingested::Stored(_)
    ));
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
    // The row says who sent it: this server, the hub.
    assert_eq!(
        at.count(
            "SELECT count(*) FROM history_delivery d JOIN library_meta m ON m.hub_server_id = d.sent_by_server_id"
        ),
        1
    );
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

/// What a program that knows nothing of Ritornel would do with an export:
/// read each line as JSON, and the table as CSV, with nothing of this
/// crate's but the folder's name.
mod reading_an_export {
    use serde_json::Value;

    pub(super) fn lines(path: &std::path::Path) -> Vec<Value> {
        std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    /// RFC 4180, as far as a quoted cell with a comma, a quote or a line
    /// break.
    pub(super) fn table(path: &std::path::Path) -> Vec<std::collections::HashMap<String, String>> {
        let text = std::fs::read_to_string(path).unwrap();
        let mut rows = Vec::new();
        let (mut row, mut cell, mut quoted, mut chars) =
            (Vec::new(), String::new(), false, text.chars().peekable());
        while let Some(c) = chars.next() {
            match (quoted, c) {
                (true, '"') if chars.peek() == Some(&'"') => {
                    chars.next();
                    cell.push('"');
                }
                (true, '"') => quoted = false,
                (true, c) => cell.push(c),
                (false, '"') => quoted = true,
                (false, ',') => row.push(std::mem::take(&mut cell)),
                (false, '\r') => {}
                (false, '\n') => {
                    row.push(std::mem::take(&mut cell));
                    rows.push(std::mem::take(&mut row));
                }
                (false, c) => cell.push(c),
            }
        }
        let mut rows = rows.into_iter();
        let header = rows.next().unwrap();
        rows.map(|row| header.iter().cloned().zip(row).collect())
            .collect()
    }
}

#[test]
fn an_export_gives_a_skipped_listen_with_what_it_was_played_from_an_imported_play_and_a_cleared_rating()
 {
    let at = library();
    let account = at
        .library
        .ensure_service_account("lastfm", "someone")
        .unwrap();
    let (release, release_title): ([u8; 16], String) = at
        .library
        .read(|connection| {
            Ok(connection.query_row(
                "SELECT public_id, display_title FROM release ORDER BY id LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?)
        })
        .unwrap();

    // A listen skipped twenty seconds into the second track, begun from
    // the album's page, and one that finished.
    let start = T0 + 10 * MINUTE;
    let mut skipped = at.listen(7, 1, start, 20_000);
    skipped.end_reason = "next".to_owned();
    skipped.context_ref = Some(Id::from_bytes(release));
    at.send(&skipped, PlayRule::V1);
    let finished = at.listen(8, 0, start + 5 * MINUTE, ALBUM[0].length_ms);
    at.send(&finished, PlayRule::V1);
    // One taken out of history again.
    exclude_listen(&at, Id::from_bytes([8; 16]));
    // A scrobble from years before, imported.
    scrobble_of(
        &at,
        account,
        1_500_000_000,
        "Ada Example, \"Quoted\"",
        "Tin Roof",
    );
    // Four stars, then none.
    let recording: [u8; 16] = at
        .library
        .read(|connection| {
            Ok(connection.query_row(
                "SELECT rec.public_id FROM source_item si
                 JOIN recording rec ON rec.id = si.current_recording_id
                 WHERE si.public_id = ?1",
                [at.tracks[0].1.as_bytes()],
                |row| row.get(0),
            )?)
        })
        .unwrap();
    for (key, value) in [(0x51, Some(80)), (0x52, None)] {
        at.library
            .set_rating(
                WriteRequest {
                    method: "ratings.set",
                    command: Some(crate::Command {
                        id: Id::from_bytes([key; 16]),
                        fingerprint: crate::Fingerprint::of("ratings.set", &[key]),
                    }),
                    client: None,
                },
                Id::from_bytes(recording),
                value,
                |_, _| String::new(),
            )
            .unwrap();
    }

    // Where the listener said they were, and a recording rated and then
    // merged into another.
    let second: [u8; 16] = at
        .library
        .read(|connection| {
            Ok(connection.query_row(
                "SELECT rec.public_id FROM source_item si
                 JOIN recording rec ON rec.id = si.current_recording_id
                 WHERE si.public_id = ?1",
                [at.tracks[1].1.as_bytes()],
                |row| row.get(0),
            )?)
        })
        .unwrap();
    at.library
        .write(
            WriteRequest {
                method: "test.zone_and_merge",
                command: None,
                client: None,
            },
            move |write| {
                let connection = write.connection();
                connection.execute(
                    "INSERT INTO listener_zone_period (listener_id, zone_name, valid_from_ms,
                                                       valid_to_ms, rev)
                     VALUES (1, 'Europe/London', 1483228800000, 1704067200000, ?1)",
                    [write.rev()],
                )?;
                connection.execute(
                    "UPDATE recording SET
                       redirect_to_id = (SELECT id FROM recording WHERE public_id = ?2),
                       canonical_id = (SELECT id FROM recording WHERE public_id = ?2)
                     WHERE public_id = ?1",
                    (recording.as_slice(), second.as_slice()),
                )?;
                Ok(String::new())
            },
        )
        .unwrap();

    let scratch = crate::testing::Scratch::new();
    let exported = at
        .library
        .export_record(scratch.path(), T0 + 99 * MINUTE)
        .unwrap();
    assert_eq!(
        (exported.observations, exported.ratings),
        (4, 2),
        "two listens, an import and a correction"
    );

    // From here on, only what is in the folder.
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(scratch.path().join("manifest.json")).unwrap())
            .unwrap();
    assert_eq!(
        (&manifest["format"], &manifest["version"]),
        (&serde_json::json!("ritornel-record"), &serde_json::json!(2))
    );
    assert_eq!(
        manifest["zonePeriods"],
        serde_json::json!([{
            "listener": at.listener.to_string(),
            "zone": "Europe/London",
            "from": "2017-01-01T00:00:00Z",
            "until": "2024-01-01T00:00:00Z",
        }])
    );
    assert_eq!(
        manifest["files"]["plays.csv"]["playRuleVersions"],
        serde_json::json!([1])
    );
    let observations = reading_an_export::lines(&scratch.path().join("observations.jsonl"));
    let endpoint_kind = |id: &serde_json::Value| {
        manifest["endpoints"]
            .as_array()
            .unwrap()
            .iter()
            .find(|endpoint| endpoint["id"] == *id)
            .unwrap()["kind"]
            .clone()
    };

    // The skipped listen, with what it was played from.
    let listen = observations
        .iter()
        .find(|line| line["kind"] == "listen" && line["listen"]["endReason"] == "next")
        .unwrap();
    assert_eq!(endpoint_kind(&listen["endpoint"]), "player");
    // The server it played on is named.
    assert!(
        manifest["servers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|server| server["id"] == listen["listen"]["playedOn"]),
        "{manifest}"
    );
    assert_eq!(listen["listen"]["msPlayed"], 20_000);
    assert_eq!(listen["listen"]["playedFrom"]["kind"], "album");
    assert_eq!(
        listen["listen"]["playedFrom"]["id"],
        Id::from_bytes(release).to_string()
    );
    assert_eq!(
        listen["listen"]["playedFrom"]["title"],
        release_title.as_str()
    );
    assert_eq!(listen["sourceKey"], "07".repeat(16));
    assert_eq!(listen["record"]["endReason"], "next");
    // What its time is worked out from, beside the record.
    assert_eq!(listen["sentBoot"], BOOT.to_string());
    assert!(listen["sentMonoMs"].is_i64(), "{listen}");

    // The imported play, as the service gave it.
    let import = observations
        .iter()
        .find(|line| line["kind"] == "import")
        .unwrap();
    assert_eq!(endpoint_kind(&import["endpoint"]), "service");
    assert_eq!(import["import"]["artist"], "Ada Example, \"Quoted\"");
    assert_eq!(import["import"]["title"], "Tin Roof");
    assert_eq!(import["import"]["playedAt"], "2017-07-14T02:40:00Z");
    assert_eq!(import["item"]["subject"]["title"], "Tin Roof");

    // The correction names what it excludes by endpoint and source key.
    let correction = observations
        .iter()
        .find(|line| line["kind"] == "correction")
        .unwrap();
    assert_eq!(correction["correction"]["verb"], "exclude");
    assert_eq!(
        correction["correction"]["targets"][0]["sourceKey"],
        "08".repeat(16)
    );

    // The rating, then its clearing.
    let ratings = reading_an_export::lines(&scratch.path().join("ratings.jsonl"));
    let of_it: Vec<_> = ratings
        .iter()
        .filter(|line| line["recording"]["id"] == Id::from_bytes(recording).to_string())
        .collect();
    assert_eq!(of_it.len(), 2);
    // Said of the recording since merged away, and what that is now.
    for line in &of_it {
        assert_eq!(line["now"]["recording"], Id::from_bytes(second).to_string());
    }
    assert_eq!(
        (&of_it[0]["value"], &of_it[0]["cleared"]),
        (&80.into(), &false.into())
    );
    assert_eq!(
        (&of_it[1]["value"], &of_it[1]["cleared"]),
        (&serde_json::Value::Null, &true.into())
    );

    // The plays as a table: the import's, the skip's and the excluded one.
    let plays = reading_an_export::table(&scratch.path().join("plays.csv"));
    assert_eq!(plays.len() as u64, exported.plays);
    let imported = plays
        .iter()
        .find(|play| play["source"] == "service")
        .unwrap();
    assert_eq!(imported["artist"], "Ada Example, \"Quoted\"");
    assert_eq!(imported["played_at_utc"], "2017-07-14T02:40:00Z");
    let skip = plays
        .iter()
        .find(|play| play["source_key"] == "07".repeat(16))
        .unwrap();
    assert_eq!(
        (
            skip["outcome"].as_str(),
            skip["counts"].as_str(),
            skip["played_from"].as_str()
        ),
        ("skipped", "0", "album")
    );
    let excluded = plays
        .iter()
        .find(|play| play["source_key"] == "08".repeat(16))
        .unwrap();
    assert_eq!(excluded["excluded"], "1");

    // A play that has lost its primary observation is still a row of the
    // table, with no source.
    at.library
        .write(
            WriteRequest {
                method: "test.orphan",
                command: None,
                client: None,
            },
            |write| {
                write.connection().execute(
                    "DELETE FROM play_event_observation WHERE role = 'primary'",
                    [],
                )?;
                Ok(String::new())
            },
        )
        .unwrap();
    let again = crate::testing::Scratch::new();
    let exported_again = at.library.export_record(again.path(), T0).unwrap();
    assert_eq!(exported_again.plays, exported.plays);
    let plays = reading_an_export::table(&again.path().join("plays.csv"));
    assert!(plays.iter().all(|play| play["source"].is_empty()));
}

#[test]
fn scrobbles_that_ended_unknown_or_withdrawn_are_counted_from_when_they_ended_until_set_aside() {
    let at = library();
    let account = at
        .library
        .ensure_service_account("lastfm", "someone")
        .unwrap();
    let start = T0 + 10 * MINUTE;
    let sent = at.listen(1, 0, start, 200_000);
    let excluded = at.listen(2, 1, start + 5 * MINUTE, 300_000);
    for listen in [&sent, &excluded] {
        at.send(listen, PlayRule::V1);
    }
    assert_eq!(
        at.library
            .lost_scrobbles(i64::MIN, Some("someone"))
            .unwrap(),
        crate::LostScrobbles::default()
    );
    assert_eq!(at.library.plan_deliveries(account, 0).unwrap(), 2);
    // One is being sent when the run ends; the other is excluded first.
    at.clock.set(start + 20 * MINUTE);
    let due = at.library.take_due_deliveries(account, 0, 1).unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(at.library.abandon_sending().unwrap(), 1);
    let after_unknown = at.library.meta().unwrap().revision;
    at.clock.set(start + 30 * MINUTE);
    exclude_listen(&at, excluded.listen_id);
    assert!(
        at.library
            .take_due_deliveries(account, 0, 50)
            .unwrap()
            .is_empty()
    );

    let lost = at
        .library
        .lost_scrobbles(i64::MIN, Some("someone"))
        .unwrap();
    let unknown = lost.unknown.unwrap();
    assert_eq!((unknown.count, unknown.since_ms), (1, start + 20 * MINUTE));
    assert_eq!(
        unknown.detail.as_deref(),
        Some("the server stopped while it was being sent")
    );
    let withdrawn = lost.withdrawn.unwrap();
    assert_eq!(withdrawn.count, 1);
    assert_eq!(withdrawn.detail.as_deref(), Some("its play is excluded"));

    // The first's echo comes back with the next import: it arrived after
    // all, and is not lost.
    let echoed = at
        .library
        .lost_scrobbles(i64::MIN, Some("someone"))
        .unwrap();
    assert!(echoed.unknown.is_some());
    scrobble_of(
        &at,
        account,
        due[0].timestamp_s,
        &due[0].artist,
        &due[0].title,
    );
    let echoed = at
        .library
        .lost_scrobbles(i64::MIN, Some("someone"))
        .unwrap();
    assert_eq!(echoed.unknown, None);
    assert_eq!(echoed.withdrawn.unwrap().count, 1);

    // Set aside by the revision after the first: only the second is
    // left, whatever the clock says.
    at.clock.set(T0);
    let lost = at
        .library
        .lost_scrobbles(after_unknown, Some("someone"))
        .unwrap();
    assert_eq!(lost.withdrawn.unwrap().count, 1);
    // And by the revision now, nothing.
    assert_eq!(
        at.library
            .lost_scrobbles(at.library.meta().unwrap().revision, Some("someone"))
            .unwrap(),
        crate::LostScrobbles::default()
    );
}

#[test]
fn scrobbles_refused_with_their_request_count_until_the_account_takes_one_again() {
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
    // The key is refused: the request and the play in it.
    let first = at.library.take_due_deliveries(account, 0, 1).unwrap();
    at.library
        .settle_deliveries(vec![(
            first[0].delivery,
            Settled::Refused("Invalid API key".to_owned()),
        )])
        .unwrap();
    let refused = at
        .library
        .lost_scrobbles(i64::MIN, Some("someone"))
        .unwrap()
        .refused
        .unwrap();
    assert_eq!(
        (refused.count, refused.detail.as_deref()),
        (1, Some("Invalid API key"))
    );
    // Dismissed: set aside.
    assert_eq!(
        at.library
            .lost_scrobbles(at.library.meta().unwrap().revision, Some("someone"))
            .unwrap()
            .refused,
        None
    );
    // Not the signed-in account's, or none signed in: nothing to put right.
    for signed_in in [Some("someone else"), None] {
        assert_eq!(
            at.library
                .lost_scrobbles(i64::MIN, signed_in)
                .unwrap()
                .refused,
            None
        );
    }
    // Put right: the next is taken, and the refusal is over.
    let second = at.library.take_due_deliveries(account, 0, 1).unwrap();
    at.library
        .settle_deliveries(vec![(
            second[0].delivery,
            Settled::Accepted {
                artist: second[0].artist.clone(),
                title: second[0].title.clone(),
            },
        )])
        .unwrap();
    assert_eq!(
        at.library
            .lost_scrobbles(i64::MIN, Some("someone"))
            .unwrap()
            .refused,
        None
    );
}

fn recording_of_track(at: &Library, index: usize) -> Id {
    let item = at.tracks[index].1;
    at.library
        .read(move |connection| {
            Ok(connection.query_row(
                "SELECT r.public_id FROM source_item si
                 JOIN recording r ON r.id = si.current_recording_id
                 WHERE si.public_id = ?1",
                [item.as_bytes()],
                |row| Ok(Id::from_bytes(row.get(0)?)),
            )?)
        })
        .unwrap()
}

fn import_keys_oldest_first(at: &Library, endpoint: Id) -> Vec<Vec<u8>> {
    at.library
        .read(move |connection| {
            let mut stmt = connection.prepare(
                "SELECT o.source_key FROM history_observation o
                 JOIN history_endpoint e ON e.id = o.endpoint_id
                 JOIN observation_import i ON i.observation_id = o.id
                 WHERE e.public_id = ?1
                 ORDER BY i.played_at_utc_ms, o.id",
            )?;
            let rows = stmt.query_map([endpoint.as_bytes()], |row| row.get::<_, Vec<u8>>(0))?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .unwrap()
}

#[test]
fn of_two_plays_sharing_one_scrobbled_text_one_moves_to_another_recording_and_survives_rebuild() {
    let at = library();
    let account = at
        .library
        .ensure_service_account("lastfm", "someone")
        .unwrap();
    let endpoint = at
        .library
        .service_account(account)
        .unwrap()
        .unwrap()
        .endpoint;
    let rec_a = recording_of_track(&at, 0);
    let rec_b = recording_of_track(&at, 1);
    let rec_c = recording_of_track(&at, 2);
    let app_b = at.tracks[1].0;

    // Two imported plays share one scrobbled text, matching track 0 (rec_a).
    scrobble_of(&at, account, 1_500_000_000, "Moss Arcade", ALBUM[0].title);
    scrobble_of(&at, account, 1_500_003_600, "Moss Arcade", ALBUM[0].title);
    let keys = import_keys_oldest_first(&at, endpoint);
    assert_eq!(keys.len(), 2);

    let before = at.library.recent_plays(at.listener, 10, None).unwrap();
    assert_eq!(before.len(), 2);
    assert!(before.iter().all(|play| play.recording == Some(rec_a)));
    assert_eq!(at.rebuild(PlayRule::V1, true).changed, 0);

    // Move ONLY the first observation to rec_b (with app_b).
    let (_, corr_id) = at
        .library
        .set_observation_recording(
            WriteRequest {
                method: "history.setRecording",
                command: None,
                client: None,
            },
            CorrectionTargets::Observations(vec![(endpoint, keys[0].clone())]),
            Some(rec_b),
            Some(app_b),
            Some("live cut".to_owned()),
        )
        .unwrap();

    // Incremental derive moved the older play and left the newer one on rec_a.
    let after = at.library.recent_plays(at.listener, 10, None).unwrap();
    assert_eq!(
        after
            .iter()
            .map(|play| (play.recording, play.appearance))
            .collect::<Vec<_>>(),
        [
            (Some(rec_a), Some(at.tracks[0].0)),
            (Some(rec_b), Some(app_b))
        ]
    );

    // history rebuild --dry-run agrees with incremental derive, and a full
    // rebuild keeps the first play on rec_b.
    assert_eq!(at.rebuild(PlayRule::V1, true).changed, 0);
    assert_eq!(at.rebuild(PlayRule::V1, false).changed, 0);

    // Running rebind (as after a rescan) leaves the overridden play alone.
    at.library
        .write(
            WriteRequest {
                method: "test.rebind",
                command: None,
                client: None,
            },
            |write| {
                rebind(write)?;
                Ok(String::new())
            },
        )
        .unwrap();
    assert_eq!(at.rebuild(PlayRule::V1, true).changed, 0);

    // Merging rec_b into rec_c moves the corrected play through the redirect
    // while keeping rec_b stored on correction_recording, so undoing the merge
    // returns it to rec_b.
    at.library
        .write(
            WriteRequest {
                method: "test.merge",
                command: None,
                client: None,
            },
            move |write| {
                let conn = write.connection();
                conn.execute(
                    "UPDATE recording SET
                       redirect_to_id = (SELECT id FROM recording WHERE public_id = ?2),
                       canonical_id = (SELECT id FROM recording WHERE public_id = ?2),
                       rev = ?3
                     WHERE public_id = ?1",
                    (rec_b.as_bytes(), rec_c.as_bytes(), write.rev()),
                )?;
                rebind(write)?;
                Ok(String::new())
            },
        )
        .unwrap();
    let merged = at.library.recent_plays(at.listener, 10, None).unwrap();
    assert_eq!(merged[1].recording, Some(rec_c));
    assert_eq!(at.rebuild(PlayRule::V1, true).changed, 0);

    // Export records the set_recording correction with its named recording and
    // current canonical recording.
    let scratch = crate::testing::Scratch::new();
    at.library
        .export_record(scratch.path(), T0 + 10 * MINUTE)
        .unwrap();
    let exported_obs = reading_an_export::lines(&scratch.path().join("observations.jsonl"));
    let exported_corr = exported_obs
        .iter()
        .find(|line| line["kind"] == "correction")
        .unwrap();
    assert_eq!(exported_corr["correction"]["verb"], "set_recording");
    assert_eq!(
        exported_corr["correction"]["setRecording"]["recording"],
        rec_b.to_string()
    );
    assert_eq!(
        exported_corr["correction"]["setRecording"]["now"]["recording"],
        rec_c.to_string()
    );

    // Undo the merge: play returns to rec_b.
    at.library
        .write(
            WriteRequest {
                method: "test.unmerge",
                command: None,
                client: None,
            },
            move |write| {
                let conn = write.connection();
                conn.execute(
                    "UPDATE recording SET redirect_to_id = NULL, canonical_id = id, rev = ?2
                     WHERE public_id = ?1",
                    (rec_b.as_bytes(), write.rev()),
                )?;
                rebind(write)?;
                Ok(String::new())
            },
        )
        .unwrap();
    let unmerged = at.library.recent_plays(at.listener, 10, None).unwrap();
    assert_eq!(unmerged[1].recording, Some(rec_b));
    assert_eq!(at.rebuild(PlayRule::V1, true).changed, 0);

    // Revoking the correction returns the first play to rec_a immediately and
    // after a rebuild.
    at.library
        .revoke_correction(
            WriteRequest {
                method: "history.revoke",
                command: None,
                client: None,
            },
            corr_id,
            Some("actually studio".to_owned()),
        )
        .unwrap();
    let revoked = at.library.recent_plays(at.listener, 10, None).unwrap();
    assert!(revoked.iter().all(|play| play.recording == Some(rec_a)));
    assert_eq!(at.rebuild(PlayRule::V1, true).changed, 0);
}

#[test]
fn a_recording_correction_by_date_range_or_to_unbound_survives_rebuild() {
    let at = library();
    let account = at
        .library
        .ensure_service_account("lastfm", "someone")
        .unwrap();
    let endpoint = at
        .library
        .service_account(account)
        .unwrap()
        .unwrap()
        .endpoint;
    let rec_a = recording_of_track(&at, 0);
    let rec_b = recording_of_track(&at, 1);
    let rec_c = recording_of_track(&at, 2);

    // Three plays of track 0 at t=1000s, t=2000s, t=3000s, plus one play of
    // track 1 inside the middle window at t=2500s.
    scrobble_of(&at, account, 1_500_001_000, "Moss Arcade", ALBUM[0].title);
    scrobble_of(&at, account, 1_500_002_000, "Moss Arcade", ALBUM[0].title);
    scrobble_of(&at, account, 1_500_002_500, "Moss Arcade", ALBUM[1].title);
    scrobble_of(&at, account, 1_500_003_000, "Moss Arcade", ALBUM[0].title);

    let subject_a: Id = at
        .library
        .read(|connection| {
            Ok(connection.query_row(
                "SELECT public_id FROM source_item
                 WHERE item_kind = 'scrobble_subject' AND provider_key LIKE '%\"' || ?1 || '\"%'",
                [ALBUM[0].title],
                |row| Ok(Id::from_bytes(row.get(0)?)),
            )?)
        })
        .unwrap();

    // Move plays of subject_a in [1_500_001_500_000, 1_500_002_800_000) to rec_c.
    at.library
        .set_observation_recording(
            WriteRequest {
                method: "history.setRecording",
                command: None,
                client: None,
            },
            CorrectionTargets::Window {
                endpoint,
                from_ms: Some(1_500_001_500_000),
                to_ms: Some(1_500_002_800_000),
                source_item: Some(subject_a),
            },
            Some(rec_c),
            None,
            None,
        )
        .unwrap();

    // Unbind the newest play (at t=3000s) explicitly.
    let keys = import_keys_oldest_first(&at, endpoint);
    at.library
        .set_observation_recording(
            WriteRequest {
                method: "history.setRecording",
                command: None,
                client: None,
            },
            CorrectionTargets::Observations(vec![(endpoint, keys[3].clone())]),
            None,
            None,
            Some("not owned".to_owned()),
        )
        .unwrap();

    let plays = at.library.recent_plays(at.listener, 10, None).unwrap();
    // Newest first: t=3000 (unbound), t=2500 (track 1 untouched: rec_b),
    // t=2000 (window override: rec_c), t=1000 (track 0 untouched: rec_a).
    assert_eq!(
        plays.iter().map(|play| play.recording).collect::<Vec<_>>(),
        [None, Some(rec_b), Some(rec_c), Some(rec_a)]
    );
    assert_eq!(at.rebuild(PlayRule::V1, true).changed, 0);
    assert_eq!(at.rebuild(PlayRule::V1, false).changed, 0);
}

#[test]
fn invalid_recording_correction_inputs_are_refused() {
    let at = library();
    let account = at
        .library
        .ensure_service_account("lastfm", "someone")
        .unwrap();
    let endpoint = at
        .library
        .service_account(account)
        .unwrap()
        .unwrap()
        .endpoint;
    scrobble_of(&at, account, 1_500_000_000, "Moss Arcade", ALBUM[0].title);
    let keys = import_keys_oldest_first(&at, endpoint);
    let rec_a = recording_of_track(&at, 0);
    let app_b = at.tracks[1].0;
    let req = || WriteRequest {
        method: "history.setRecording",
        command: None,
        client: None,
    };

    // Empty observation list.
    assert!(matches!(
        at.library.set_observation_recording(
            req(),
            CorrectionTargets::Observations(Vec::new()),
            Some(rec_a),
            None,
            None,
        ),
        Err(Error::Invalid(_))
    ));
    // Appearance without recording.
    assert!(matches!(
        at.library.set_observation_recording(
            req(),
            CorrectionTargets::Observations(vec![(endpoint, keys[0].clone())]),
            None,
            Some(app_b),
            None,
        ),
        Err(Error::Invalid(_))
    ));
    // Appearance belonging to another recording.
    assert!(matches!(
        at.library.set_observation_recording(
            req(),
            CorrectionTargets::Observations(vec![(endpoint, keys[0].clone())]),
            Some(rec_a),
            Some(app_b),
            None,
        ),
        Err(Error::Invalid(_))
    ));
    // Window ending before it begins.
    assert!(matches!(
        at.library.set_observation_recording(
            req(),
            CorrectionTargets::Window {
                endpoint,
                from_ms: Some(2000),
                to_ms: Some(1000),
                source_item: None,
            },
            Some(rec_a),
            None,
            None,
        ),
        Err(Error::Invalid(_))
    ));
    // Out-of-range window timestamps that cannot be exported as ISO-8601.
    assert!(matches!(
        at.library.set_observation_recording(
            req(),
            CorrectionTargets::Window {
                endpoint,
                from_ms: Some(i64::MAX),
                to_ms: None,
                source_item: None,
            },
            Some(rec_a),
            None,
            None,
        ),
        Err(Error::Invalid(_))
    ));
    assert!(matches!(
        at.library.set_observation_recording(
            req(),
            CorrectionTargets::Window {
                endpoint,
                from_ms: None,
                to_ms: Some(i64::MIN),
                source_item: None,
            },
            Some(rec_a),
            None,
            None,
        ),
        Err(Error::Invalid(_))
    ));
}

#[test]
fn recording_only_correction_on_a_listen_shows_and_exports_the_new_recording() {
    let at = library();
    let listen = at.listen(0x31, 0, T0 - 5 * MINUTE, 180_000);
    at.send(&listen, PlayRule::V1);

    // Find the listen's observation endpoint and key.
    let (endpoint, source_key): (Id, Vec<u8>) = at
        .library
        .read(|connection| {
            Ok(connection.query_row(
                "SELECT e.public_id, o.source_key FROM history_observation o
                 JOIN history_endpoint e ON e.id = o.endpoint_id
                 WHERE o.kind = 'listen'",
                [],
                |row| Ok((Id::from_bytes(row.get(0)?), row.get(1)?)),
            )?)
        })
        .unwrap();

    let rec_b = recording_of_track(&at, 1);
    at.library
        .set_observation_recording(
            WriteRequest {
                method: "history.setRecording",
                command: None,
                client: None,
            },
            CorrectionTargets::Observations(vec![(endpoint, source_key)]),
            Some(rec_b),
            None,
            None,
        )
        .unwrap();

    let plays = at.library.recent_plays(at.listener, 10, None).unwrap();
    assert_eq!(plays.len(), 1);
    assert_eq!(plays[0].recording, Some(rec_b));
    assert_eq!(plays[0].title.as_deref(), Some(ALBUM[1].title));
    assert_eq!(plays[0].album, None);

    let out = crate::testing::Scratch::new();
    at.library.export_record(out.path(), T0).unwrap();
    let csv = std::fs::read_to_string(out.path().join("plays.csv")).unwrap();
    assert!(csv.contains(ALBUM[1].title), "plays.csv was:\n{csv}");
    assert!(!csv.contains(ALBUM[0].title), "plays.csv was:\n{csv}");
}

#[test]
fn recording_correction_accepts_merged_recording_appearance_and_exports_now_appearance() {
    let at = library();
    let account = at
        .library
        .ensure_service_account("lastfm", "someone")
        .unwrap();
    let endpoint = at
        .library
        .service_account(account)
        .unwrap()
        .unwrap()
        .endpoint;
    scrobble_of(&at, account, 1_500_000_000, "Moss Arcade", ALBUM[0].title);
    let keys = import_keys_oldest_first(&at, endpoint);

    // Redirect recording 1 into recording 2; track 1's appearance still has
    // recording_id = rec_1 while its recording's canonical_id is rec_2.
    let rec_1 = recording_of_track(&at, 1);
    let rec_2 = recording_of_track(&at, 2);
    let app_1 = at.tracks[1].0;
    let app_2 = at.tracks[2].0;
    at.library
        .write(
            WriteRequest {
                method: "test.merge",
                command: None,
                client: None,
            },
            move |write| {
                write.connection().execute(
                    "UPDATE recording
                     SET redirect_to_id = (SELECT id FROM recording WHERE public_id = ?2),
                         canonical_id = (SELECT id FROM recording WHERE public_id = ?2)
                     WHERE public_id = ?1",
                    (rec_1.as_bytes().as_slice(), rec_2.as_bytes().as_slice()),
                )?;
                Ok(String::new())
            },
        )
        .unwrap();

    // Naming canonical rec_2 with app_1 (whose row belongs to merged rec_1)
    // succeeds, and exporting includes now.appearance.
    at.library
        .set_observation_recording(
            WriteRequest {
                method: "history.setRecording",
                command: None,
                client: None,
            },
            CorrectionTargets::Observations(vec![(endpoint, keys[0].clone())]),
            Some(rec_2),
            Some(app_1),
            None,
        )
        .unwrap();

    let plays = at.library.recent_plays(at.listener, 10, None).unwrap();
    assert_eq!(plays[0].recording, Some(rec_2));

    // Also redirect app_1 -> app_2 and check export's now.appearance follows it.
    at.library
        .write(
            WriteRequest {
                method: "test.mergeApp",
                command: None,
                client: None,
            },
            move |write| {
                write.connection().execute(
                    "UPDATE track_appearance
                     SET redirect_to_id = (SELECT id FROM track_appearance WHERE public_id = ?2),
                         canonical_id = (SELECT id FROM track_appearance WHERE public_id = ?2)
                     WHERE public_id = ?1",
                    (app_1.as_bytes().as_slice(), app_2.as_bytes().as_slice()),
                )?;
                Ok(String::new())
            },
        )
        .unwrap();

    let out = crate::testing::Scratch::new();
    at.library.export_record(out.path(), T0).unwrap();
    let jsonl = std::fs::read_to_string(out.path().join("observations.jsonl")).unwrap();
    let corr_line = jsonl
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .find(|row| row["kind"] == "correction")
        .unwrap();
    assert_eq!(
        corr_line["correction"]["setRecording"]["now"]["recording"],
        serde_json::Value::String(rec_2.to_string())
    );
    assert_eq!(
        corr_line["correction"]["setRecording"]["now"]["appearance"],
        serde_json::Value::String(app_2.to_string())
    );
}

#[test]
fn pending_delivery_is_withdrawn_when_its_listen_is_corrected_or_unbound() {
    let at = library();
    let account = at
        .library
        .ensure_service_account("lastfm", "someone")
        .unwrap();
    let start = T0 - 5 * MINUTE;
    let listen1 = at.listen(0x41, 0, start, 180_000);
    let listen2 = at.listen(0x42, 0, start + 10_000, 180_000);
    at.send(&listen1, PlayRule::V1);
    at.send(&listen2, PlayRule::V1);
    let queued = at.library.plan_deliveries(account, start - MINUTE).unwrap();
    assert_eq!(queued, 2);

    let listen_obs: Vec<(Id, Vec<u8>)> = at
        .library
        .read(|connection| {
            let mut stmt = connection.prepare(
                "SELECT e.public_id, o.source_key FROM history_observation o
                 JOIN history_endpoint e ON e.id = o.endpoint_id
                 WHERE o.kind = 'listen'
                 ORDER BY o.id",
            )?;
            let rows = stmt.query_map([], |row| Ok((Id::from_bytes(row.get(0)?), row.get(1)?)))?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .unwrap();

    // Unbind listen1, and move listen2 to track 1's recording.
    let rec_b = recording_of_track(&at, 1);
    at.library
        .set_observation_recording(
            WriteRequest {
                method: "history.setRecording",
                command: None,
                client: None,
            },
            CorrectionTargets::Observations(vec![listen_obs[0].clone()]),
            None,
            None,
            None,
        )
        .unwrap();
    at.library
        .set_observation_recording(
            WriteRequest {
                method: "history.setRecording",
                command: None,
                client: None,
            },
            CorrectionTargets::Observations(vec![listen_obs[1].clone()]),
            Some(rec_b),
            None,
            None,
        )
        .unwrap();

    // Neither stale delivery may be sent.
    let due = at
        .library
        .take_due_deliveries(account, start - MINUTE, 10)
        .unwrap();
    assert!(due.is_empty());
    let states: Vec<String> = at
        .library
        .read(|connection| {
            let mut stmt = connection.prepare("SELECT state FROM history_delivery ORDER BY id")?;
            let rows = stmt.query_map([], |row| row.get(0))?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .unwrap();
    assert_eq!(states, ["withdrawn", "withdrawn"]);
}

#[test]
fn moving_an_echo_between_two_listens_keeps_both_plays_after_incremental_rederive() {
    let at = library();
    let account = at
        .library
        .ensure_service_account("lastfm", "someone")
        .unwrap();
    // Two listens starting in the same second, for track 0 (listen A, earlier
    // observation_id, and listen B, later observation_id).
    let start_s = (T0 - 5 * MINUTE) / 1000;
    let start_ms = start_s * 1000;
    let listen_a = at.listen(0x51, 0, start_ms, 180_000);
    let listen_b = at.listen(0x52, 0, start_ms + 200, 180_000);
    at.send(&listen_a, PlayRule::V1);
    at.send(&listen_b, PlayRule::V1);

    let (obs_a, obs_b): (i64, i64) = at
        .library
        .read(|connection| {
            let a: i64 = connection.query_row(
                "SELECT o.id FROM history_observation o
                 JOIN observation_listen l ON l.observation_id = o.id
                 WHERE l.started_at_utc_ms = ?1",
                [start_ms],
                |row| row.get(0),
            )?;
            let b: i64 = connection.query_row(
                "SELECT o.id FROM history_observation o
                 JOIN observation_listen l ON l.observation_id = o.id
                 WHERE l.started_at_utc_ms = ?1",
                [start_ms + 200],
                |row| row.get(0),
            )?;
            Ok((a, b))
        })
        .unwrap();
    let endpoint_row: i64 = at
        .library
        .read(|connection| {
            Ok(connection.query_row(
                "SELECT endpoint_id FROM service_account WHERE public_id = ?1",
                [account.as_bytes().as_slice()],
                |row| row.get(0),
            )?)
        })
        .unwrap();

    // First, an accepted delivery on listen B matches the imported scrobble so
    // the import corroborates listen B's play.
    at.library
        .write(
            WriteRequest {
                method: "test.acceptedB",
                command: None,
                client: None,
            },
            move |write| {
                write.connection().execute(
                    "INSERT INTO history_delivery
                       (observation_id, endpoint_id, state, sent_timestamp_s, artist, title,
                        accepted_artist, accepted_title, created_at_ms, updated_at_ms, rev)
                     VALUES (?1, ?2, 'accepted', ?3, 'Moss Arcade', ?4, 'Moss Arcade', ?4, ?5, ?5, ?6)",
                    rusqlite::params![
                        obs_b,
                        endpoint_row,
                        start_s,
                        ALBUM[0].title,
                        write.now_ms(),
                        write.rev(),
                    ],
                )?;
                Ok(String::new())
            },
        )
        .unwrap();
    scrobble_of(&at, account, start_s, "Moss Arcade", ALBUM[0].title);
    assert_eq!(
        at.library
            .recent_plays(at.listener, 10, None)
            .unwrap()
            .len(),
        2
    );

    // Now an accepted delivery for listen A (earlier observation_id) settles at
    // the same second, moving the echo's preferred owner from listen B to A.
    at.library
        .write(
            WriteRequest {
                method: "test.acceptedA",
                command: None,
                client: None,
            },
            move |write| {
                write.connection().execute(
                    "INSERT INTO history_delivery
                       (observation_id, endpoint_id, state, sent_timestamp_s, artist, title,
                        accepted_artist, accepted_title, created_at_ms, updated_at_ms, rev)
                     VALUES (?1, ?2, 'accepted', ?3, 'Moss Arcade', ?4, 'Moss Arcade', ?4, ?5, ?5, ?6)",
                    rusqlite::params![
                        obs_a,
                        endpoint_row,
                        start_s,
                        ALBUM[0].title,
                        write.now_ms(),
                        write.rev(),
                    ],
                )?;
                let delivery_id = write.connection().last_insert_rowid();
                rederive_delivery(write, delivery_id)?;
                Ok(String::new())
            },
        )
        .unwrap();

    // Both listens must still have plays, matching a full rebuild.
    assert_eq!(
        at.library
            .recent_plays(at.listener, 10, None)
            .unwrap()
            .len(),
        2
    );
    assert_eq!(at.rebuild(PlayRule::V1, true).changed, 0);
}

#[test]
fn migrating_a_saved_schema_1_library_preserves_rows_and_enables_recording_corrections() {
    let scratch = crate::testing::Scratch::new();
    let path = scratch.path().join("library.db");
    {
        let mut connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute_batch("PRAGMA foreign_keys = ON;")
            .unwrap();
        crate::migrate::run(
            &mut connection,
            &crate::migrate::LIBRARY[..2],
            &FixedClock::at(T0),
        )
        .unwrap();
        connection
            .execute_batch(include_str!("../../saved/library/0002.sql"))
            .unwrap();
    }
    let clock = Arc::new(FixedClock::at(T0 + 60 * MINUTE));
    let entropy = Arc::new(CountingEntropy::default());
    let store = LibraryStore::open(&path, Id::from_bytes([0x01; 16]), clock, entropy).unwrap();

    // Schema migrated cleanly, pre-existing exclude correction and rating kept.
    let (version, excludes, ratings): (i64, i64, i64) = store
        .read(|connection| {
            Ok((
                connection.query_row("SELECT max(version) FROM schema_migration", [], |row| {
                    row.get(0)
                })?,
                connection.query_row(
                    "SELECT count(*) FROM observation_correction WHERE verb = 'exclude'",
                    [],
                    |row| row.get(0),
                )?,
                connection
                    .query_row("SELECT count(*) FROM listener_rating", [], |row| row.get(0))?,
            ))
        })
        .unwrap();
    assert_eq!(version, crate::migrate::LIBRARY.len() as i64);
    assert_eq!((excludes, ratings), (1, 1));

    // Derive the migrated plays, then move observation 1 to recording 2 and
    // check rebuild agrees.
    store
        .rebuild_history(
            WriteRequest {
                method: "history.rebuild",
                command: None,
                client: None,
            },
            false,
            |_| String::new(),
        )
        .unwrap();
    let endpoint = Id::from_bytes([0xb3; 16]);
    let rec2 = Id::from_bytes([0x42; 16]);
    store
        .set_observation_recording(
            WriteRequest {
                method: "history.setRecording",
                command: None,
                client: None,
            },
            CorrectionTargets::Observations(vec![(endpoint, vec![0xd1; 8])]),
            Some(rec2),
            None,
            None,
        )
        .unwrap();
    let listener = Id::from_bytes([0xa1; 16]);
    let plays = store.recent_plays(listener, 10, None).unwrap();
    assert_eq!(plays.len(), 2);
    // Observation 2 (newer) is excluded and on recording 1; observation 1 is
    // moved to recording 2.
    assert!(plays[0].excluded);
    assert_eq!(plays[1].recording, Some(rec2));
    let dry: i64 = store
        .rebuild_history(
            WriteRequest {
                method: "history.rebuild",
                command: None,
                client: None,
            },
            true,
            |rebuilt| rebuilt.changed.to_string(),
        )
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(dry, 0);
}

#[test]
fn a_predecessor_arriving_late_does_not_schedule_its_play_again() {
    use crate::Settled;
    let at = library();
    let account = at
        .library
        .ensure_service_account("lastfm", "someone")
        .unwrap();
    let start = T0 + 10 * MINUTE;
    let mut crashed = at.listen(3, 1, start, 155_000);
    crashed.end_reason = "crash".to_owned();
    let mut resumed = at.listen(4, 1, start + 4 * MINUTE, 200_000);
    resumed.continues = Some(crashed.listen_id);
    resumed.start_position_ms = 155_000;
    // The resumed listen arrives first, counts and is sent.
    at.send(&resumed, PlayRule::V1);
    assert_eq!(
        at.library.plan_deliveries(account, start - MINUTE).unwrap(),
        1
    );
    for due in at.library.take_due_deliveries(account, 0, 50).unwrap() {
        at.library
            .settle_deliveries(vec![(
                due.delivery,
                Settled::Accepted {
                    artist: due.artist.clone(),
                    title: due.title.clone(),
                },
            )])
            .unwrap();
    }
    // Its predecessor arrives and leads the play from then on: the play was
    // sent, and is not sent again.
    at.send(&crashed, PlayRule::V1);
    assert_eq!(
        at.library.plan_deliveries(account, start - MINUTE).unwrap(),
        0
    );
}

#[test]
fn a_play_waiting_to_be_sent_when_its_predecessor_arrives_is_sent_once_from_it() {
    let at = library();
    let account = at
        .library
        .ensure_service_account("lastfm", "someone")
        .unwrap();
    let start = T0 + 10 * MINUTE;
    let mut crashed = at.listen(3, 1, start, 155_000);
    crashed.end_reason = "crash".to_owned();
    let mut resumed = at.listen(4, 1, start + 4 * MINUTE, 200_000);
    resumed.continues = Some(crashed.listen_id);
    resumed.start_position_ms = 155_000;
    at.send(&resumed, PlayRule::V1);
    assert_eq!(
        at.library.plan_deliveries(account, start - MINUTE).unwrap(),
        1
    );
    // Not yet sent when the predecessor arrives: the waiting one is
    // withdrawn at its turn, and the play goes once, at its true start.
    at.send(&crashed, PlayRule::V1);
    assert_eq!(
        at.library.plan_deliveries(account, start - MINUTE).unwrap(),
        1
    );
    let due = at.library.take_due_deliveries(account, 0, 50).unwrap();
    assert_eq!(
        due.iter().map(|d| d.timestamp_s).collect::<Vec<_>>(),
        [start / 1000]
    );
    assert_eq!(
        at.count("SELECT count(*) FROM history_delivery WHERE state = 'withdrawn'"),
        1
    );
}

#[test]
fn an_echo_of_one_play_does_not_withdraw_another_play_of_the_same_track() {
    use crate::Settled;
    let at = library();
    let account = at
        .library
        .ensure_service_account("lastfm", "someone")
        .unwrap();
    let start = (T0 + 10 * MINUTE) / 1000 * 1000;
    // Two plays of one track a second apart, as two players could make.
    let first = at.listen(1, 0, start, 200_000);
    let second = at.listen(2, 0, start + 1_000, 200_000);
    at.send(&first, PlayRule::V1);
    at.send(&second, PlayRule::V1);
    assert_eq!(
        at.count("SELECT count(*) FROM play_event WHERE counts = 1"),
        2
    );
    assert_eq!(
        at.library.plan_deliveries(account, start - MINUTE).unwrap(),
        2
    );
    // The first is sent and its echo comes back.
    let due = at.library.take_due_deliveries(account, 0, 1).unwrap();
    assert_eq!(due[0].timestamp_s, start / 1000);
    at.library
        .settle_deliveries(vec![(
            due[0].delivery,
            Settled::Accepted {
                artist: due[0].artist.clone(),
                title: due[0].title.clone(),
            },
        )])
        .unwrap();
    scrobble_of(&at, account, start / 1000, "Moss Arcade", "Paper Kites");
    // The echo is the first play's: the second is still sent.
    let due = at.library.take_due_deliveries(account, 0, 50).unwrap();
    assert_eq!(
        due.iter().map(|d| d.timestamp_s).collect::<Vec<_>>(),
        [start / 1000 + 1]
    );
}

#[test]
fn after_a_restore_the_import_finds_the_plays_the_service_took_and_only_those() {
    let at = library();
    let account = at
        .library
        .ensure_service_account("lastfm", "someone")
        .unwrap();
    let start = (T0 + 10 * MINUTE) / 1000 * 1000;
    // Two plays of one track a second apart, and one of another track, all
    // waiting to be sent when the backup was taken.
    at.send(&at.listen(1, 0, start, 200_000), PlayRule::V1);
    at.send(&at.listen(2, 0, start + 1_000, 200_000), PlayRule::V1);
    at.send(&at.listen(3, 1, start + 10 * MINUTE, 250_000), PlayRule::V1);
    assert_eq!(
        at.library.plan_deliveries(account, start - MINUTE).unwrap(),
        3
    );
    at.library
        .write(
            WriteRequest {
                method: "test.restore",
                command: None,
                client: None,
            },
            move |write| {
                crate::delivery_store::hold_sending(write.connection(), start)?;
                Ok(String::new())
            },
        )
        .unwrap();
    // The server the backup was taken from sent the first before it went;
    // the import brings it back.
    scrobble_of(&at, account, start / 1000, "Moss Arcade", "Paper Kites");
    let counted = || at.count("SELECT count(*) FROM play_event WHERE counts = 1");
    assert_eq!(
        counted(),
        4,
        "until it is recognised, the echo is a play of its own"
    );
    assert!(at.library.release_hold(account).unwrap());
    // It is the first play's echo now, and the first is not sent again.
    assert_eq!(counted(), 3);
    assert_eq!(
        at.library.delivery_counts(account).unwrap(),
        [("accepted".to_owned(), 1), ("pending".to_owned(), 2)]
    );
    // The two the service did not take are sent, the near twin included.
    let mut sent = Vec::new();
    for _ in 0..2 {
        sent.extend(
            at.library
                .take_due_deliveries(account, 0, 50)
                .unwrap()
                .into_iter()
                .map(|due| due.timestamp_s),
        );
    }
    assert_eq!(sent, [start / 1000 + 1, (start + 10 * MINUTE) / 1000]);
    let incremental = at.library.derived_history().unwrap();
    at.rebuild(PlayRule::V1, false);
    assert_eq!(at.library.derived_history().unwrap(), incremental);
}

#[test]
fn a_play_joined_by_a_late_middle_listen_is_not_sent_again_from_its_first() {
    use crate::Settled;
    let at = library();
    let account = at
        .library
        .ensure_service_account("lastfm", "someone")
        .unwrap();
    let start = T0 + 10 * MINUTE;
    // One hearing in three parts, each after a crash, the first and the
    // last long enough to count alone: the last arrives first and is sent,
    // then the first, and the middle one last.
    let mut first = at.listen(3, 1, start, 160_000);
    first.end_reason = "crash".to_owned();
    let mut middle = at.listen(4, 1, start + 4 * MINUTE, 20_000);
    middle.end_reason = "crash".to_owned();
    middle.continues = Some(first.listen_id);
    middle.start_position_ms = 160_000;
    let mut last = at.listen(5, 1, start + 6 * MINUTE, 160_000);
    last.continues = Some(middle.listen_id);
    last.start_position_ms = 140_000;
    at.send(&last, PlayRule::V1);
    assert_eq!(
        at.library.plan_deliveries(account, start - MINUTE).unwrap(),
        1
    );
    for due in at.library.take_due_deliveries(account, 0, 50).unwrap() {
        at.library
            .settle_deliveries(vec![(
                due.delivery,
                Settled::Accepted {
                    artist: due.artist.clone(),
                    title: due.title.clone(),
                },
            )])
            .unwrap();
    }
    at.send(&first, PlayRule::V1);
    // Not yet one play with the last: the first part is planned alone.
    assert_eq!(
        at.library.plan_deliveries(account, start - MINUTE).unwrap(),
        1
    );
    at.send(&middle, PlayRule::V1);
    // Whatever was planned for the first part before the three were one
    // play is not sent: the play went from its last part.
    let due = at.library.take_due_deliveries(account, 0, 50).unwrap();
    assert!(due.is_empty(), "{due:?}");
}

#[test]
fn an_outside_reader_joins_an_echo_under_corrected_names_to_its_play_from_the_export_alone() {
    use crate::Settled;
    use serde_json::Value;
    let at = library();
    let account = at
        .library
        .ensure_service_account("lastfm", "someone")
        .unwrap();
    let start = (T0 + 10 * MINUTE) / 1000 * 1000;
    at.send(&at.listen(1, 0, start, 200_000), PlayRule::V1);
    assert_eq!(
        at.library.plan_deliveries(account, start - MINUTE).unwrap(),
        1
    );
    let due = at.library.take_due_deliveries(account, 0, 50).unwrap();
    // Last.fm took it under another title, and the import brings it back so.
    at.library
        .settle_deliveries(vec![(
            due[0].delivery,
            Settled::Accepted {
                artist: "Moss Arcade".to_owned(),
                title: "Paper Kites (Live)".to_owned(),
            },
        )])
        .unwrap();
    scrobble_of(
        &at,
        account,
        start / 1000,
        "Moss Arcade",
        "Paper Kites (Live)",
    );
    // The same scrobble again at Last.fm, as a repeat there makes: a play
    // of its own, as only one import echoes a listen.
    let repeat = ritornel_source::Batch {
        records: vec![ritornel_source::ImportRecord {
            source_key: b"repeat".to_vec(),
            played_at_ms: start,
            track: ritornel_source::TrackText {
                artist: "Moss Arcade".to_owned(),
                album: Some("Tin Weather".to_owned()),
                title: "Paper Kites (Live)".to_owned(),
            },
            ids: ritornel_source::Identifiers::default(),
            payload_json: "{}".to_owned(),
        }],
        span: ritornel_source::Span {
            from_ms: start,
            to_ms: start + 1000,
        },
        cursor: "{}".to_owned(),
    };
    at.library
        .ingest(Observations::Imports {
            account,
            batch: &repeat,
        })
        .unwrap();

    let scratch = crate::testing::Scratch::new();
    let exported = at
        .library
        .export_record(scratch.path(), T0 + 99 * MINUTE)
        .unwrap();
    assert_eq!(exported.deliveries, 1);

    // From here on, only what is in the folder, read as a program that
    // knows nothing of Ritornel would: an import is the echo of the
    // delivery to its own endpoint that may have arrived, sent at its
    // second, naming its track as sent or as the service took it.
    let observations = reading_an_export::lines(&scratch.path().join("observations.jsonl"));
    let deliveries = reading_an_export::lines(&scratch.path().join("deliveries.jsonl"));
    // The imports in the order of the file: of two that echo one listen,
    // the first joins its play.
    let imports: Vec<&Value> = observations
        .iter()
        .filter(|line| line["kind"] == "import")
        .collect();
    assert_eq!(imports.len(), 2);
    let import = imports[0];
    let names = |artist: &Value, title: &Value| {
        (
            artist.as_str().unwrap_or_default().to_lowercase(),
            title.as_str().unwrap_or_default().to_lowercase(),
        )
    };
    let theirs = names(&import["import"]["artist"], &import["import"]["title"]);
    let echoed: Vec<&Value> = deliveries
        .iter()
        .filter(|delivery| {
            delivery["destination"] == import["endpoint"]
                && ["sending", "accepted", "unknown"].contains(&delivery["state"].as_str().unwrap())
                && delivery["sentTimestamp"] == import["import"]["playedAt"]
                && (names(&delivery["sent"]["artist"], &delivery["sent"]["title"]) == theirs
                    || names(
                        &delivery["accepted"]["artist"],
                        &delivery["accepted"]["title"],
                    ) == theirs)
        })
        .collect();
    assert_eq!(echoed.len(), 1);
    // Only the names Last.fm took say so: the names sent do not.
    assert_ne!(
        names(&echoed[0]["sent"]["artist"], &echoed[0]["sent"]["title"]),
        theirs
    );
    let listen = &echoed[0]["observation"];
    assert!(observations.iter().any(|line| line["kind"] == "listen"
        && line["endpoint"] == listen["endpoint"]
        && line["sourceKey"] == listen["sourceKey"]));

    // What the reader derived is what the table says: the listen's play,
    // with the first import beside it, and the repeat as a play of its own.
    let plays = reading_an_export::table(&scratch.path().join("plays.csv"));
    assert_eq!(plays.len(), 2, "{plays:?}");
    let of = |source_key: &Value| {
        plays
            .iter()
            .find(|play| play["source_key"] == source_key.as_str().unwrap())
            .unwrap()
    };
    assert_eq!(
        of(&listen["sourceKey"])["corroborated_by"],
        format!(
            "{}:{}",
            import["endpoint"].as_str().unwrap(),
            import["sourceKey"].as_str().unwrap()
        )
    );
    assert_eq!(of(&imports[1]["sourceKey"])["corroborated_by"], "");
}
