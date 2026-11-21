//! Plays another service reported, against the invented album of the
//! player's tests: the ingest of a batch and its cursor, matching, the
//! report, the loves, and dates by the listener's zone.

use std::sync::Arc;

use ritornel_source::{Batch, Identifiers, ImportRecord, Love, Span, TrackText};

use super::*;
use crate::playback::tests::{ALBUM, catalog};
use crate::testing::{CountingEntropy, FixedClock};
use crate::{Observations, ServerStore, ZonePeriodRow};

/// 2026-11-15 21:00:00 UTC.
const T0: i64 = 1_794_776_400_000;
const HOUR: i64 = 3_600_000;

struct Rig {
    library: LibraryStore,
    account: Id,
    appearances: Vec<Id>,
}

fn rig() -> Rig {
    let clock = Arc::new(FixedClock::at(T0));
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
        clock,
        entropy,
    )
    .unwrap();
    let (appearances, _) = catalog(&library, server.server_id, &ALBUM);
    let account = library.ensure_service_account("lastfm", "someone").unwrap();
    Rig {
        library,
        account,
        appearances,
    }
}

/// A scrobble at `at_ms` of `title`, by `artist`, on the invented album.
fn record(at_ms: i64, artist: &str, title: &str, ordinal: u8) -> ImportRecord {
    let mut key = at_ms.to_be_bytes().to_vec();
    key.extend_from_slice(title.as_bytes());
    key.push(ordinal);
    ImportRecord {
        source_key: key,
        played_at_ms: at_ms,
        track: TrackText {
            artist: artist.to_owned(),
            album: Some("Tin Weather".to_owned()),
            title: title.to_owned(),
        },
        ids: Identifiers::default(),
        payload_json: format!("{{\"name\":\"{title}\",\"ordinal\":{ordinal}}}"),
    }
}

fn batch(records: Vec<ImportRecord>, from_ms: i64, to_ms: i64) -> Batch {
    Batch {
        records,
        span: Span { from_ms, to_ms },
        cursor: format!("{{\"readBefore\":{}}}", to_ms / 1000),
    }
}

impl Rig {
    fn ingest(&self, batch: &Batch) -> ritornel_source::Stored {
        match self
            .library
            .ingest(Observations::Imports {
                account: self.account,
                batch,
            })
            .unwrap()
        {
            crate::IngestOutcome::Imports(stored) => stored,
            crate::IngestOutcome::Listen(_) => unreachable!(),
        }
    }

    fn revision(&self) -> i64 {
        self.library.meta().unwrap().revision
    }

    fn one(&self, sql: &str) -> String {
        let sql = sql.to_owned();
        self.library
            .read(move |connection| {
                Ok(connection.query_row(&sql, [], |row| {
                    Ok(match row.get_ref(0)? {
                        rusqlite::types::ValueRef::Null => "-".to_owned(),
                        rusqlite::types::ValueRef::Integer(n) => n.to_string(),
                        rusqlite::types::ValueRef::Text(t) => {
                            String::from_utf8_lossy(t).into_owned()
                        }
                        other => format!("{other:?}"),
                    })
                })?)
            })
            .unwrap()
    }
}

fn first_batch() -> Batch {
    batch(
        vec![
            record(T0 - 3 * HOUR, "Moss Arcade", "Paper Kites", 0),
            record(T0 - 3 * HOUR, "Moss Arcade", "Paper Kites", 1),
            record(T0 - 2 * HOUR, "moss arcade", "LANTERNFISH", 0),
            record(T0 - HOUR, "Somebody Else", "Not Here", 0),
            record(T0 - HOUR + 1000, "Moss Arcade", "Salt Road (Live)", 0),
        ],
        T0 - 4 * HOUR,
        T0,
    )
}

#[test]
fn a_batch_is_stored_with_its_cursor_and_a_second_reading_writes_nothing() {
    let rig = rig();
    let first = first_batch();
    let before = rig.revision();
    let stored = rig.ingest(&first);
    assert_eq!((stored.new, stored.gone), (5, 0));
    assert_eq!(rig.revision(), before + 1, "one batch, one transaction");
    assert_eq!(
        rig.library.history_cursor(rig.account).unwrap().as_deref(),
        Some(first.cursor.as_str())
    );
    assert_eq!(
        rig.one("SELECT count(*) FROM history_observation WHERE kind = 'import'"),
        "5"
    );
    // Two identical plays in one second are two plays.
    assert_eq!(
        rig.one("SELECT count(*) FROM play_event WHERE count_basis = 'source_verdict'"),
        "5"
    );
    // Four distinct tracks, each a text-only item; identical text is one.
    assert_eq!(
        rig.one("SELECT count(*) FROM source_item WHERE item_kind = 'scrobble_subject'"),
        "4"
    );

    let again = rig.revision();
    let stored = rig.ingest(&first);
    assert_eq!((stored.new, stored.gone), (0, 0));
    assert_eq!(rig.revision(), again, "nothing new, nothing written");

    // A reading that no longer has one says so, and removes nothing.
    let mut fewer = first_batch();
    fewer.records.remove(3);
    let stored = rig.ingest(&fewer);
    assert_eq!((stored.new, stored.gone), (0, 1));
    assert_eq!(rig.revision(), again);
}

#[test]
fn a_scrobble_binds_only_on_a_confident_match_and_the_report_lists_the_doubtful() {
    let rig = rig();
    rig.ingest(&first_batch());
    // Paper Kites twice and Lanternfish bind, whatever the case; the live
    // version of Salt Road is near and not the same; the stranger is absent.
    assert_eq!(
        rig.one("SELECT count(*) FROM play_event WHERE recording_id IS NOT NULL"),
        "3"
    );
    assert_eq!(
        rig.one(
            "SELECT group_concat(method) FROM identity_decision WHERE method = 'scrobble_text'"
        ),
        "scrobble_text,scrobble_text"
    );
    let report = rig.library.import_report(rig.account, 10).unwrap();
    assert_eq!(
        (report.imported, report.plays, report.matched_plays),
        (5, 5, 3)
    );
    assert_eq!((report.tracks, report.matched_tracks), (4, 2));
    assert_eq!((report.doubtful_tracks, report.doubtful_plays), (1, 1));
    assert_eq!((report.absent_tracks, report.absent_plays), (1, 1));
    assert_eq!(report.doubtful[0].title, "Salt Road (Live)");
    assert_eq!(report.doubtful[0].doubt, "near");
    assert_eq!(
        report.doubtful[0].candidates,
        vec![(Some("Salt Road".to_owned()), Some("Moss Arcade".to_owned()))]
    );
    assert_eq!(report.first_ms, Some(T0 - 3 * HOUR));
    // Nothing more binds when matched again: the library has not changed.
    let before = rig.revision();
    assert_eq!(rig.library.match_scrobbles(rig.account).unwrap(), 0);
    assert_eq!(rig.revision(), before);
}

#[test]
fn an_imported_play_is_dated_in_the_zone_the_listener_was_in_then() {
    let rig = rig();
    // 2009-06-30 23:30 UTC: 1 July in Helsinki, 30 June in New York.
    let at = 1_246_404_600_000;
    rig.ingest(&batch(
        vec![record(at, "Moss Arcade", "Paper Kites", 0)],
        at,
        at + 1000,
    ));
    let date = || rig.one("SELECT local_date FROM play_event");
    assert_eq!(date(), "-", "no zone known, no local date");
    let listener = rig.library.default_listener().unwrap().unwrap();
    let set = |periods: Vec<ZonePeriodRow>| {
        rig.library
            .set_zone_periods(
                WriteRequest {
                    method: "history.setZones",
                    command: None,
                    client: None,
                },
                listener,
                periods,
                |changed| changed.to_string(),
            )
            .unwrap()
    };
    let home = ZonePeriodRow {
        zone: "America/New_York".to_owned(),
        from_ms: None,
        to_ms: None,
    };
    assert_eq!(set(vec![home.clone()]), "1");
    assert_eq!(date(), "20090630");
    let trip = ZonePeriodRow {
        zone: "Europe/Helsinki".to_owned(),
        from_ms: crate::start_of_day_ms(20_090_601, "Europe/Helsinki"),
        to_ms: crate::start_of_day_ms(20_090_801, "Europe/Helsinki"),
    };
    assert_eq!(set(vec![home, trip.clone()]), "1");
    assert_eq!(date(), "20090701");
    assert_eq!(rig.library.zone_periods_of(listener).unwrap().len(), 2);
    let derived = rig.library.derived_history().unwrap();
    rig.library
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
    assert_eq!(rig.library.derived_history().unwrap(), derived);
    let refused = rig.library.set_zone_periods(
        WriteRequest {
            method: "history.setZones",
            command: None,
            client: None,
        },
        listener,
        vec![ZonePeriodRow {
            zone: "Mars/Olympus".to_owned(),
            ..trip
        }],
        |_| String::new(),
    );
    assert!(matches!(refused, Err(Error::Invalid(_))), "{refused:?}");
}

#[test]
fn the_loved_tracks_follow_the_list_and_show_on_the_track() {
    let rig = rig();
    let loves = vec![
        Love {
            artist: "Moss Arcade".to_owned(),
            title: "Lanternfish".to_owned(),
            loved_at_ms: T0 - 100 * HOUR,
        },
        Love {
            artist: "Somebody Else".to_owned(),
            title: "Not Here".to_owned(),
            loved_at_ms: T0 - 50 * HOUR,
        },
    ];
    let stored = rig.library.replace_loves(rig.account, &loves).unwrap();
    assert_eq!((stored.loved, stored.new, stored.cleared), (2, 2, 0));
    let lanternfish = rig.appearances[1];
    let shown = rig.library.track_detail(lanternfish).unwrap().unwrap();
    assert_eq!(shown.title.as_deref(), Some("Lanternfish"));
    assert_eq!(
        shown.loves,
        vec![LoveRow {
            by: "lastfm:someone".to_owned(),
            at_ms: T0 - 100 * HOUR
        }]
    );

    let before = rig.revision();
    let stored = rig.library.replace_loves(rig.account, &loves).unwrap();
    assert_eq!((stored.new, stored.cleared), (0, 0));
    assert_eq!(rig.revision(), before, "the same list writes nothing");

    let stored = rig.library.replace_loves(rig.account, &loves[1..]).unwrap();
    assert_eq!((stored.new, stored.cleared), (0, 1));
    let shown = rig.library.track_detail(lanternfish).unwrap().unwrap();
    assert!(shown.loves.is_empty());
    // The rows are kept: a love and the row that took it away.
    assert_eq!(
        rig.one("SELECT count(*) FROM listener_rating WHERE kind = 'love'"),
        "3"
    );
    let report = rig.library.import_report(rig.account, 10).unwrap();
    assert_eq!((report.loved, report.loved_matched), (1, 0));
}

impl Rig {
    /// Binds the subject of this text to the appearance of `title`, as
    /// another matcher or the owner did: a decision by `actor` and an
    /// assignment, superseding the one in force.
    fn force(&self, text: (&str, &str, &str), title: &str, actor: &'static str) {
        let (artist, album, scrobbled) = text;
        let key = provider_key(artist, Some(album), scrobbled);
        let title = title.to_owned();
        self.library
            .write(
                WriteRequest {
                    method: "test.force",
                    command: None,
                    client: None,
                },
                move |write| {
                    let connection = write.connection();
                    let item: i64 = connection.query_row(
                        "SELECT id FROM source_item WHERE provider_key = ?1",
                        [&key],
                        |row| row.get(0),
                    )?;
                    let (recording, appearance): (i64, i64) = connection.query_row(
                        "SELECT recording_id, id FROM track_appearance WHERE display_title = ?1",
                        [&title],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )?;
                    connection.execute(
                        "INSERT INTO identity_decision
                           (action, source_item_id, method, resolver_version, normalizer_version,
                            evidence_json, actor_kind, decided_at_ms, rev)
                         VALUES ('bind', ?1, ?2, 1, 1, '{}', ?3, ?4, ?5)",
                        (
                            item,
                            if actor == "resolver" {
                                "scrobble_text"
                            } else {
                                "user"
                            },
                            actor,
                            write.now_ms(),
                            write.rev(),
                        ),
                    )?;
                    let decision = connection.last_insert_rowid();
                    connection.execute(
                        "UPDATE assignment SET superseded_at_ms = ?2, rev = ?3
                         WHERE source_item_id = ?1 AND superseded_at_ms IS NULL",
                        (item, write.now_ms(), write.rev()),
                    )?;
                    connection.execute(
                        "INSERT INTO assignment
                           (source_item_id, recording_id, appearance_id, decision_id,
                            effective_from_ms, rev)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                        (
                            item,
                            recording,
                            appearance,
                            decision,
                            write.now_ms(),
                            write.rev(),
                        ),
                    )?;
                    connection.execute(
                        "UPDATE source_item SET current_recording_id = ?2 WHERE id = ?1",
                        (item, recording),
                    )?;
                    crate::history_store::rebind(write)?;
                    Ok(String::new())
                },
            )
            .unwrap();
    }

    /// The title each counted play is bound to, by what was scrobbled.
    fn bound(&self) -> Vec<String> {
        let sql = "SELECT o.title || ' -> ' || coalesce(ta.display_title, '-')
                   FROM play_event pe
                   JOIN play_event_observation peo ON peo.play_event_id = pe.id AND peo.role = 'primary'
                   JOIN observation_import o ON o.observation_id = peo.observation_id
                   LEFT JOIN track_appearance ta ON ta.id = pe.appearance_id
                   ORDER BY o.title";
        self.library
            .read(move |connection| {
                let mut statement = connection.prepare(sql)?;
                let rows = statement.query_map([], |row| row.get(0))?;
                Ok(rows.collect::<Result<Vec<String>, _>>()?)
            })
            .unwrap()
    }
}

#[test]
fn a_bound_track_is_judged_again_at_each_sync() {
    let rig = rig();
    let mut acoustic = record(T0 - 2 * HOUR, "Moss Arcade", "Paper Kites", 0);
    acoustic.track.album = Some("Acoustic Sessions".to_owned());
    rig.ingest(&batch(
        vec![acoustic, record(T0 - HOUR, "Moss Arcade", "Salt Road", 0)],
        T0 - 4 * HOUR,
        T0,
    ));
    // The album fits none of the one recording's appearances: held back.
    assert_eq!(rig.bound(), ["Paper Kites -> -", "Salt Road -> Salt Road"]);
    let report = rig.library.import_report(rig.account, 10).unwrap();
    assert_eq!(report.doubtful[0].doubt, "album");

    // Bound as matcher 1 bound them: the acoustic play to the album track,
    // and Salt Road to the wrong song.
    rig.force(
        ("Moss Arcade", "Acoustic Sessions", "Paper Kites"),
        "Paper Kites",
        "resolver",
    );
    rig.force(
        ("Moss Arcade", "Tin Weather", "Salt Road"),
        "Lanternfish",
        "resolver",
    );
    assert_eq!(
        rig.bound(),
        ["Paper Kites -> Paper Kites", "Salt Road -> Lanternfish"]
    );
    // Judged again, the one is released and the other moved.
    assert_eq!(rig.library.match_scrobbles(rig.account).unwrap(), 2);
    assert_eq!(rig.bound(), ["Paper Kites -> -", "Salt Road -> Salt Road"]);
    assert_eq!(
        rig.one(
            "SELECT count(*) FROM source_item WHERE item_kind = 'scrobble_subject'
                 AND current_recording_id IS NULL"
        ),
        "1"
    );
    // A second judgement changes nothing and writes nothing.
    let before = rig.revision();
    assert_eq!(rig.library.match_scrobbles(rig.account).unwrap(), 0);
    assert_eq!(rig.revision(), before);

    // The owner's own binding is never judged again.
    rig.force(
        ("Moss Arcade", "Acoustic Sessions", "Paper Kites"),
        "Paper Kites",
        "person",
    );
    assert_eq!(rig.library.match_scrobbles(rig.account).unwrap(), 0);
    assert_eq!(
        rig.bound(),
        ["Paper Kites -> Paper Kites", "Salt Road -> Salt Road"]
    );
}
