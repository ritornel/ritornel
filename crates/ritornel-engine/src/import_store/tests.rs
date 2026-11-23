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
        self.read(batch, false)
    }

    /// Takes in a batch as a read that is full or not does.
    fn read(&self, batch: &Batch, full: bool) -> ritornel_source::Stored {
        match self
            .library
            .ingest(Observations::Imports {
                account: self.account,
                batch,
                full,
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
fn a_full_read_sets_aside_a_scrobble_an_earlier_read_stored_twice() {
    let rig = rig();
    // An earlier read whose pages shifted between requests: Paper Kites
    // twice in its second, Lanternfish not at all.
    let shifted = batch(
        vec![
            record(T0 - 3 * HOUR, "Moss Arcade", "Paper Kites", 0),
            record(T0 - 3 * HOUR, "Moss Arcade", "Paper Kites", 1),
            record(T0 - HOUR, "Somebody Else", "Not Here", 0),
        ],
        T0 - 4 * HOUR,
        T0,
    );
    rig.ingest(&shifted);
    // What the source holds: one Paper Kites, and Lanternfish in the same
    // second. Not Here is gone from it altogether.
    let held = batch(
        vec![
            record(T0 - 3 * HOUR, "Moss Arcade", "Paper Kites", 0),
            record(T0 - 3 * HOUR, "moss arcade", "LANTERNFISH", 0),
        ],
        T0 - 4 * HOUR,
        T0,
    );
    let counted = "SELECT count(*) FROM play_event WHERE excluded = 0";
    let corrections = format!(
        "SELECT count(*) FROM observation_correction WHERE verb = 'exclude' AND note = '{}'",
        super::DUPLICATE_NOTE
    );

    // A read that is not full stores what is new and sets nothing aside.
    let stored = rig.ingest(&held);
    assert_eq!((stored.new, stored.gone, stored.set_aside), (1, 2, 0));
    assert_eq!(rig.one(counted), "4");

    // A full read sets the second Paper Kites aside, and nothing else: Not
    // Here is gone, and no record of the same track and time is left.
    let stored = rig.read(&held, true);
    assert_eq!((stored.new, stored.gone, stored.set_aside), (0, 2, 1));
    assert_eq!(rig.one(counted), "3");
    assert_eq!(rig.one(&corrections), "1");
    assert_eq!(
        rig.one("SELECT count(*) FROM history_observation WHERE kind = 'import'"),
        "4",
        "nothing is removed"
    );
    assert_eq!(
        rig.one(
            "SELECT o.payload_json FROM correction_target t
             JOIN history_observation o ON o.id = t.target_observation_id"
        ),
        r#"{"name":"Paper Kites","ordinal":1}"#
    );

    // Again: already set aside, so nothing is written.
    let revision = rig.revision();
    assert_eq!(rig.read(&held, true).set_aside, 0);
    assert_eq!(rig.revision(), revision);

    // Revoked by the owner, it counts again, and a full read leaves it be.
    let correction = rig.one(
        "SELECT hex(o.source_key) FROM history_observation o
         JOIN observation_correction c ON c.observation_id = o.id",
    );
    let correction: Id = format!(
        "{}-{}-{}-{}-{}",
        &correction[0..8],
        &correction[8..12],
        &correction[12..16],
        &correction[16..20],
        &correction[20..32]
    )
    .to_lowercase()
    .parse()
    .unwrap();
    let revoke = |correction| {
        rig.library
            .revoke_correction(
                WriteRequest {
                    method: "history.revoke",
                    command: None,
                    client: None,
                },
                correction,
                None,
            )
            .unwrap()
            .1
    };
    let revocation = revoke(correction);
    assert_eq!(rig.one(counted), "4");
    assert_eq!(rig.read(&held, true).set_aside, 0);
    assert_eq!(rig.one(counted), "4");

    // The revocation revoked in turn: set aside again, and lifted when the
    // source lists it again.
    revoke(revocation);
    assert_eq!(rig.one(counted), "3");
    let mut listed = held.clone();
    listed
        .records
        .push(record(T0 - 3 * HOUR, "Moss Arcade", "Paper Kites", 1));
    rig.read(&listed, true);
    assert_eq!(rig.one(counted), "4");
}

#[test]
fn a_full_read_keeps_a_repeat_deleted_at_the_source_and_lifts_one_listed_again() {
    let rig = rig();
    let counted = "SELECT count(*) FROM play_event WHERE excluded = 0";
    // One read stored Paper Kites twice and Lanternfish in one second, as
    // the source listed them. Then one Paper Kites was deleted there: the
    // second holds fewer now, which is not the shape of a shifted read.
    let at = T0 - 3 * HOUR;
    rig.ingest(&batch(
        vec![
            record(at, "Moss Arcade", "Paper Kites", 0),
            record(at, "Moss Arcade", "Paper Kites", 1),
            record(at, "moss arcade", "LANTERNFISH", 0),
        ],
        T0 - 4 * HOUR,
        T0,
    ));
    let deleted = batch(
        vec![
            record(at, "Moss Arcade", "Paper Kites", 0),
            record(at, "moss arcade", "LANTERNFISH", 0),
        ],
        T0 - 4 * HOUR,
        T0,
    );
    assert_eq!(rig.read(&deleted, true).set_aside, 0);
    assert_eq!(rig.one(counted), "3");

    // In another second, a shifted read: Salt Road twice, Not Here missed.
    // A full read sets one aside. Later the source lists a second Salt
    // Road under the key set aside, as a scrobble added there would be:
    // the next full read lifts it.
    let later = T0 - HOUR;
    rig.ingest(&batch(
        vec![
            record(later, "Moss Arcade", "Salt Road (Live)", 0),
            record(later, "Moss Arcade", "Salt Road (Live)", 1),
        ],
        later,
        later + 1000,
    ));
    let shifted_back = batch(
        vec![
            record(later, "Moss Arcade", "Salt Road (Live)", 0),
            record(later, "Somebody Else", "Not Here", 0),
        ],
        later,
        later + 1000,
    );
    let stored = rig.read(&shifted_back, true);
    assert_eq!((stored.new, stored.set_aside), (1, 1));
    assert_eq!(rig.one(counted), "5");
    let added = batch(
        vec![
            record(later, "Moss Arcade", "Salt Road (Live)", 0),
            record(later, "Moss Arcade", "Salt Road (Live)", 1),
            record(later, "Somebody Else", "Not Here", 0),
        ],
        later,
        later + 1000,
    );
    assert_eq!(rig.read(&added, true).set_aside, 0);
    assert_eq!(rig.one(counted), "6");
    assert_eq!(
        rig.one("SELECT count(*) FROM observation_correction WHERE verb = 'revoke'"),
        "1"
    );
    // And it stays lifted.
    let revision = rig.revision();
    assert_eq!(rig.read(&added, true).set_aside, 0);
    assert_eq!(rig.revision(), revision);
}

#[test]
fn a_full_read_never_leaves_fewer_counted_than_the_source_lists() {
    let rig = rig();
    let at = T0 - 3 * HOUR;
    rig.ingest(&batch(
        vec![
            record(at, "Moss Arcade", "Paper Kites", 0),
            record(at, "Moss Arcade", "Paper Kites", 1),
        ],
        T0 - 4 * HOUR,
        T0,
    ));
    // The first copy is already left out, by a correction of the owner's.
    let first = rig.one("SELECT min(id) FROM history_observation WHERE kind = 'import'");
    let first: i64 = first.parse().unwrap();
    rig.library
        .write(
            WriteRequest {
                method: "test.exclude",
                command: None,
                client: None,
            },
            move |write| {
                crate::history_store::exclude(
                    write,
                    &[first],
                    "the owner's",
                    None,
                    ritornel_domain::history::PlayRule::CURRENT,
                )?;
                Ok(String::new())
            },
        )
        .unwrap();
    // A shifted read's shape, but the copy the source does not list is the
    // one that still counts: it stays.
    let held = batch(
        vec![
            record(at, "Moss Arcade", "Paper Kites", 0),
            record(at, "moss arcade", "LANTERNFISH", 0),
        ],
        T0 - 4 * HOUR,
        T0,
    );
    assert_eq!(rig.read(&held, true).set_aside, 0);
    assert_eq!(
        rig.one("SELECT count(*) FROM play_event WHERE excluded = 0"),
        "2"
    );
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

#[test]
fn a_recording_the_owner_excluded_is_never_bound_even_through_a_merge() {
    let rig = rig();
    rig.ingest(&batch(
        vec![record(T0 - HOUR, "Moss Arcade", "Paper Kites", 0)],
        T0 - 2 * HOUR,
        T0,
    ));
    assert_eq!(rig.bound(), ["Paper Kites -> Paper Kites"]);
    // The owner said the track is not a recording that has since been
    // merged into the one it is bound to.
    let key = provider_key("Moss Arcade", Some("Tin Weather"), "Paper Kites");
    rig.library
        .write(
            WriteRequest {
                method: "test.exclude",
                command: None,
                client: None,
            },
            move |write| {
                let connection = write.connection();
                let kept: i64 = connection.query_row(
                    "SELECT recording_id FROM track_appearance WHERE display_title = 'Paper Kites'",
                    [],
                    |row| row.get(0),
                )?;
                connection.execute(
                    "INSERT INTO recording (id, public_id, redirect_to_id, canonical_id, display_title, rev)
                     VALUES (900, ?1, ?2, ?2, 'Paper Kites', ?3)",
                    (write.new_id()?.as_bytes(), kept, write.rev()),
                )?;
                connection.execute(
                    "INSERT INTO identity_exclusion (source_item_id, recording_id, actor_kind, created_at_ms, rev)
                     SELECT id, 900, 'person', ?2, ?3 FROM source_item WHERE provider_key = ?1",
                    (&key, write.now_ms(), write.rev()),
                )?;
                Ok(String::new())
            },
        )
        .unwrap();
    assert_eq!(rig.library.match_scrobbles(rig.account).unwrap(), 1);
    assert_eq!(rig.bound(), ["Paper Kites -> -"]);
    // The report counts it, as absent: the library has nothing else it
    // could be.
    let report = rig.library.import_report(rig.account, 10).unwrap();
    assert_eq!((report.matched_tracks, report.absent_tracks), (0, 1));
    // And judged again, nothing is written.
    let before = rig.revision();
    assert_eq!(rig.library.match_scrobbles(rig.account).unwrap(), 0);
    assert_eq!(rig.revision(), before);
}

impl Rig {
    /// Judges every subject again in the write itself, past the read that
    /// picks which differ, and says how many bindings changed.
    fn rematch(&self) -> String {
        let account = self.account;
        self.library
            .write(
                WriteRequest {
                    method: "test.match",
                    command: None,
                    client: None,
                },
                move |write| {
                    let connection = write.connection();
                    let rows = rows_of(connection, account)?;
                    let subjects: Vec<Subject> = judged_subjects(connection, rows.instance)?
                        .into_iter()
                        .map(|(subject, _)| subject)
                        .collect();
                    let catalog = super::catalog(connection)?;
                    Ok(match_items(write, &catalog, &subjects)?.to_string())
                },
            )
            .unwrap()
            .result_json
    }

    /// Runs `sql` in a write, with `?1` the revision.
    fn exec(&self, sql: &'static str) {
        self.library
            .write(
                WriteRequest {
                    method: "test.exec",
                    command: None,
                    client: None,
                },
                move |write| {
                    let connection = write.connection();
                    let mut rest = sql;
                    while let Some(end) = rest.find(';') {
                        let (one, after) = rest.split_at(end);
                        rest = &after[1..];
                        if !one.trim().is_empty() {
                            connection.execute(one, [write.rev()])?;
                        }
                    }
                    Ok(String::new())
                },
            )
            .unwrap();
    }
}

#[test]
fn a_binding_through_a_merge_is_kept_so_that_undoing_the_merge_is_exact() {
    let rig = rig();
    let mut plain = record(T0 - HOUR, "Moss Arcade", "Paper Kites", 0);
    plain.track.album = None;
    rig.ingest(&batch(vec![plain], T0 - 2 * HOUR, T0));
    // A second file of the song, once a recording of its own and since
    // merged into the first: the song is on two appearances, so a play
    // with no album binds to the recording and to neither.
    rig.exec(
        "INSERT INTO recording (id, public_id, redirect_to_id, canonical_id, display_title, rev)
           SELECT 900, randomblob(16), recording_id, recording_id, 'Paper Kites', ?1
           FROM track_appearance WHERE display_title = 'Paper Kites';
         INSERT INTO track_appearance
           (id, public_id, recording_id, release_id, canonical_id, display_title, rev)
           SELECT 900, randomblob(16), 900, release_id, 900, 'Paper Kites', ?1
           FROM track_appearance WHERE display_title = 'Paper Kites' AND id <> 900;
         INSERT INTO source_item
           (id, public_id, source_instance_id, item_kind, provider_key, size_bytes, mtime_ms,
            first_seen_at_ms, rev)
           SELECT 900, randomblob(16), source_instance_id, 'file', 'Moss Arcade/copy.flac', 1, 0, 0, ?1
           FROM source_item WHERE item_kind = 'file' LIMIT 1;
         INSERT INTO identity_decision
           (id, action, source_item_id, method, resolver_version, normalizer_version,
            evidence_json, actor_kind, decided_at_ms, rev)
           VALUES (900, 'bind', 900, 'new', 1, 1, '{}', 'resolver', 0, ?1);
         INSERT INTO assignment
           (source_item_id, recording_id, appearance_id, decision_id, effective_from_ms, rev)
           VALUES (900, 900, 900, 900, 0, ?1);",
    );
    // The play was bound to the merged recording before the merge.
    rig.exec(
        "INSERT INTO identity_decision
           (id, action, source_item_id, method, resolver_version, normalizer_version,
            evidence_json, actor_kind, decided_at_ms, rev)
           SELECT 901, 'bind', id, 'scrobble_text', 2, 2, '{}', 'resolver', 0, ?1
           FROM source_item WHERE item_kind = 'scrobble_subject';
         UPDATE assignment SET superseded_at_ms = 1, rev = ?1
           WHERE superseded_at_ms IS NULL AND source_item_id IN
             (SELECT id FROM source_item WHERE item_kind = 'scrobble_subject');
         INSERT INTO assignment
           (source_item_id, recording_id, appearance_id, decision_id, effective_from_ms, rev)
           SELECT source_item_id, 900, NULL, 901, 1, ?1 FROM identity_decision WHERE id = 901;",
    );
    let bound = "SELECT a.recording_id || ' ' || coalesce(a.appearance_id, '-') FROM assignment a
                 JOIN source_item si ON si.id = a.source_item_id
                 WHERE si.item_kind = 'scrobble_subject' AND a.superseded_at_ms IS NULL";
    assert_eq!(rig.one(bound), "900 -");
    // Judged again, it is where the merge sends it: nothing is written, and
    // the binding still names the recording a merge's undo gives back.
    let before = rig.revision();
    assert_eq!(rig.library.match_scrobbles(rig.account).unwrap(), 0);
    assert_eq!(rig.revision(), before);
    assert_eq!(rig.one(bound), "900 -");
    // The write, given it, agrees.
    assert_eq!(rig.rematch(), "0");
    assert_eq!(rig.one(bound), "900 -");

    // So too a binding to an appearance merged into another.
    rig.exec(
        "UPDATE track_appearance SET redirect_to_id = 1, canonical_id = 1, rev = ?1 WHERE id = 900;
         UPDATE assignment SET superseded_at_ms = 2, rev = ?1 WHERE decision_id = 901;
         INSERT INTO assignment
           (source_item_id, recording_id, appearance_id, decision_id, effective_from_ms, rev)
           SELECT source_item_id, 900, 900, 901, 2, ?1 FROM identity_decision WHERE id = 901;",
    );
    assert_eq!(
        rig.one(
            "SELECT id FROM track_appearance WHERE display_title = 'Paper Kites' AND id <> 900"
        ),
        "1"
    );
    let before = rig.revision();
    assert_eq!(rig.library.match_scrobbles(rig.account).unwrap(), 0);
    assert_eq!(rig.revision(), before);
    assert_eq!(rig.one(bound), "900 900");
    assert_eq!(rig.rematch(), "0");
    assert_eq!(rig.one(bound), "900 900");

    // But an appearance merged into one of another recording carries the
    // play to a recording it was never counted for: it is bound again.
    rig.exec(
        "INSERT INTO recording (id, public_id, canonical_id, display_title, rev)
           VALUES (950, randomblob(16), 950, 'Paper Kites', ?1);
         INSERT INTO track_appearance
           (id, public_id, recording_id, release_id, redirect_to_id, canonical_id, display_title, rev)
           SELECT 950, randomblob(16), 950, release_id, 1, 1, 'Paper Kites', ?1
           FROM track_appearance WHERE id = 1;
         UPDATE assignment SET superseded_at_ms = 3, rev = ?1
           WHERE decision_id = 901 AND superseded_at_ms IS NULL;
         INSERT INTO assignment
           (source_item_id, recording_id, appearance_id, decision_id, effective_from_ms, rev)
           SELECT source_item_id, 950, 950, 901, 3, ?1 FROM identity_decision WHERE id = 901;",
    );
    assert_eq!(rig.library.match_scrobbles(rig.account).unwrap(), 1);
    assert_eq!(rig.one(bound), "1 1");
    // And its play counts for the recording it is bound to now.
    assert_eq!(rig.one(PLAYED), "1 1");
}

/// The recording and song the play counts for.
const PLAYED: &str = "SELECT recording_id || ' ' || coalesce(appearance_id, '-') FROM play_event";

#[test]
fn a_song_whose_own_recording_was_merged_keeps_its_binding() {
    let rig = rig();
    // The song is also on another record, as a recording once of its own
    // and merged into the first by way of a third.
    rig.exec(
        "INSERT INTO recording (id, public_id, redirect_to_id, canonical_id, display_title, rev)
           VALUES (971, randomblob(16), 1, 1, 'Paper Kites', ?1);
         INSERT INTO recording (id, public_id, redirect_to_id, canonical_id, display_title, rev)
           VALUES (970, randomblob(16), 971, 1, 'Paper Kites', ?1);
         INSERT INTO release (id, public_id, release_group_id, canonical_id, display_title, rev)
           SELECT 970, randomblob(16), release_group_id, 970, 'Night Drives', ?1
           FROM release WHERE id = (SELECT release_id FROM track_appearance WHERE id = 1);
         INSERT INTO track_appearance
           (id, public_id, recording_id, release_id, canonical_id, display_title, rev)
           VALUES (970, randomblob(16), 970, 970, 970, 'Paper Kites', ?1);
         INSERT INTO source_item
           (id, public_id, source_instance_id, item_kind, provider_key, size_bytes, mtime_ms,
            first_seen_at_ms, rev)
           SELECT 970, randomblob(16), source_instance_id, 'file', 'Night Drives/07.flac', 1, 0, 0, ?1
           FROM source_item WHERE item_kind = 'file' LIMIT 1;
         INSERT INTO identity_decision
           (id, action, source_item_id, method, resolver_version, normalizer_version,
            evidence_json, actor_kind, decided_at_ms, rev)
           VALUES (970, 'bind', 970, 'new', 1, 1, '{}', 'resolver', 0, ?1);
         INSERT INTO assignment
           (source_item_id, recording_id, appearance_id, decision_id, effective_from_ms, rev)
           VALUES (970, 970, 970, 970, 0, ?1);",
    );
    let mut there = record(T0 - HOUR, "Moss Arcade", "Paper Kites", 0);
    there.track.album = Some("Night Drives".to_owned());
    rig.ingest(&batch(vec![there], T0 - 2 * HOUR, T0));
    // Bound to that song, whose own recording is the one it was.
    let bound = "SELECT a.recording_id || ' ' || coalesce(a.appearance_id, '-') FROM assignment a
                 JOIN source_item si ON si.id = a.source_item_id
                 WHERE si.item_kind = 'scrobble_subject' AND a.superseded_at_ms IS NULL";
    assert_eq!(rig.one(bound), "970 970");
    assert_eq!(rig.one(PLAYED), "1 970");
    // Judged again, by both passes, it is kept as it is.
    let before = rig.revision();
    assert_eq!(rig.library.match_scrobbles(rig.account).unwrap(), 0);
    assert_eq!(rig.revision(), before);
    assert_eq!(rig.rematch(), "0");
    assert_eq!(rig.one(bound), "970 970");
}

#[test]
fn a_play_bound_to_a_song_no_file_reaches_is_bound_again() {
    let rig = rig();
    rig.ingest(&batch(
        vec![record(T0 - HOUR, "Moss Arcade", "Paper Kites", 0)],
        T0 - 2 * HOUR,
        T0,
    ));
    let bound = "SELECT a.recording_id || ' ' || coalesce(a.appearance_id, '-') FROM assignment a
                 JOIN source_item si ON si.id = a.source_item_id
                 WHERE si.item_kind = 'scrobble_subject' AND a.superseded_at_ms IS NULL";
    assert_eq!(rig.one(bound), "1 1");
    // The song's file is retagged onto another copy of the record, as a
    // rescan would bind it: the song it left keeps its row, and no file
    // reaches it.
    rig.exec(
        "INSERT INTO release (id, public_id, release_group_id, canonical_id, display_title, rev)
           SELECT 960, randomblob(16), release_group_id, 960, 'Tin Weather', ?1
           FROM release WHERE id = (SELECT release_id FROM track_appearance WHERE id = 1);
         INSERT INTO track_appearance
           (id, public_id, recording_id, release_id, canonical_id, display_title, rev)
           VALUES (960, randomblob(16), 1, 960, 960, 'Paper Kites', ?1);
         INSERT INTO identity_decision
           (id, action, source_item_id, method, resolver_version, normalizer_version,
            evidence_json, actor_kind, decided_at_ms, rev)
           SELECT 960, 'bind', source_item_id, 'tags_duration', 2, 2, '{}', 'resolver', 1, ?1
           FROM assignment WHERE appearance_id = 1 AND superseded_at_ms IS NULL
             AND source_item_id IN (SELECT id FROM source_item WHERE item_kind = 'file');
         UPDATE assignment SET superseded_at_ms = 1, rev = ?1
           WHERE appearance_id = 1 AND superseded_at_ms IS NULL
             AND source_item_id IN (SELECT id FROM source_item WHERE item_kind = 'file');
         INSERT INTO assignment
           (source_item_id, recording_id, appearance_id, decision_id, effective_from_ms, rev)
           SELECT source_item_id, 1, 960, 960, 1, ?1 FROM identity_decision WHERE id = 960;",
    );
    // At the next judgement the play follows the song to the copy a file
    // reaches, and stays on its recording.
    assert_eq!(rig.one(PLAYED), "1 1");
    assert_eq!(rig.library.match_scrobbles(rig.account).unwrap(), 1);
    assert_eq!(rig.one(bound), "1 960");
    assert_eq!(rig.one(PLAYED), "1 960");
    assert_eq!(rig.library.match_scrobbles(rig.account).unwrap(), 0);
}

#[test]
fn the_matcher_never_writes_over_the_owners_binding() {
    let rig = rig();
    let mut acoustic = record(T0 - HOUR, "Moss Arcade", "Paper Kites", 0);
    acoustic.track.album = Some("Acoustic Sessions".to_owned());
    rig.ingest(&batch(vec![acoustic], T0 - 2 * HOUR, T0));
    // The owner binds it between the matcher's reading and its writing:
    // the write judges only what is not the owner's.
    rig.force(
        ("Moss Arcade", "Acoustic Sessions", "Paper Kites"),
        "Lanternfish",
        "person",
    );
    let item: Subject = (
        rig.one("SELECT id FROM source_item WHERE item_kind = 'scrobble_subject'")
            .parse()
            .unwrap(),
        "Moss Arcade".to_owned(),
        Some("Acoustic Sessions".to_owned()),
        "Paper Kites".to_owned(),
    );
    let changed = rig
        .library
        .write(
            WriteRequest {
                method: "test.match",
                command: None,
                client: None,
            },
            move |write| {
                let catalog = super::catalog(write.connection())?;
                Ok(match_items(write, &catalog, &[item])?.to_string())
            },
        )
        .unwrap();
    assert_eq!(changed.result_json, "0");
    assert_eq!(rig.bound(), ["Paper Kites -> Lanternfish"]);
    assert_eq!(
        rig.one("SELECT count(*) FROM identity_decision WHERE action = 'unbind'"),
        "0"
    );
}

#[test]
fn releasing_a_scrobble_binding_records_an_unbind_decision() {
    let rig = rig();
    rig.ingest(&batch(
        vec![record(T0 - HOUR, "Moss Arcade", "Paper Kites", 0)],
        T0 - 2 * HOUR,
        T0,
    ));
    assert_eq!(rig.bound(), ["Paper Kites -> Paper Kites"]);
    assert_eq!(
        rig.one("SELECT count(*) FROM identity_decision WHERE action = 'unbind'"),
        "0"
    );

    // Add a second distinct recording with the same artist and title so the
    // scrobble text becomes doubtful and its binding is released.
    rig.exec(
        "INSERT INTO recording
           (id, public_id, canonical_id, display_title, display_artist_credit_id, rev)
         SELECT 990, randomblob(16), 990, display_title, display_artist_credit_id, ?1
         FROM recording WHERE id = 1;
         INSERT INTO track_appearance
           (id, public_id, recording_id, release_id, canonical_id, display_title,
            display_artist_credit_id, rev)
         SELECT 990, randomblob(16), 990, release_id, 990, display_title,
                display_artist_credit_id, ?1
         FROM track_appearance WHERE id = 1;
         INSERT INTO source_item
           (id, public_id, source_instance_id, provider_key, item_kind, current_recording_id,
            first_seen_at_ms, rev)
         SELECT 990, randomblob(16), source_instance_id, 'extra/paper-kites-2.flac', 'file', 990, 1, ?1
         FROM source_item WHERE item_kind = 'file' LIMIT 1;
         INSERT INTO identity_decision
           (id, action, source_item_id, method, resolver_version, normalizer_version,
            evidence_json, actor_kind, decided_at_ms, rev)
         VALUES (990, 'bind', 990, 'tags_duration', 2, 2, '{}', 'resolver', 1, ?1);
         INSERT INTO assignment
           (source_item_id, recording_id, appearance_id, decision_id, effective_from_ms, rev)
         VALUES (990, 990, 990, 990, 1, ?1);",
    );

    assert_eq!(rig.library.match_scrobbles(rig.account).unwrap(), 1);
    assert_eq!(rig.bound(), ["Paper Kites -> -"]);
    assert_eq!(rig.one(PLAYED), "-");

    let (method, score, actor_kind, evidence_json): (String, Option<f64>, String, String) = rig
        .library
        .read(|c| {
            Ok(c.query_row(
                "SELECT method, score, actor_kind, evidence_json
                 FROM identity_decision
                 WHERE action = 'unbind'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )?)
        })
        .unwrap();
    assert_eq!(method, "scrobble_text");
    assert_eq!(score, None);
    assert_eq!(actor_kind, "resolver");
    let evidence: serde_json::Value = serde_json::from_str(&evidence_json).unwrap();
    assert_eq!(evidence["verdict"], "doubtful");
    assert_eq!(evidence["doubt"], "several");
    assert_eq!(evidence["candidates"], 2);
    assert_eq!(evidence["artist"], "Moss Arcade");
    assert_eq!(evidence["title"], "Paper Kites");

    // Re-running the matcher writes nothing more and adds no duplicate unbind decision.
    assert_eq!(rig.library.match_scrobbles(rig.account).unwrap(), 0);
    assert_eq!(
        rig.one("SELECT count(*) FROM identity_decision WHERE action = 'unbind'"),
        "1"
    );

    // Also test an `absent` verdict unbind (recording excluded -> doubt is null, candidates is 0).
    let rig2 = self::rig();
    rig2.ingest(&batch(
        vec![record(T0 - HOUR, "Moss Arcade", "Paper Kites", 0)],
        T0 - 2 * HOUR,
        T0,
    ));
    assert_eq!(rig2.bound(), ["Paper Kites -> Paper Kites"]);
    rig2.exec(
        "INSERT INTO identity_exclusion (source_item_id, recording_id, actor_kind, created_at_ms, rev)
         SELECT id, 1, 'person', 1, ?1 FROM source_item WHERE item_kind = 'scrobble_subject';",
    );
    assert_eq!(rig2.library.match_scrobbles(rig2.account).unwrap(), 1);
    assert_eq!(rig2.bound(), ["Paper Kites -> -"]);
    assert_eq!(rig2.one(PLAYED), "-");
    let absent_json: String =
        rig2.one("SELECT evidence_json FROM identity_decision WHERE action = 'unbind'");
    let absent: serde_json::Value = serde_json::from_str(&absent_json).unwrap();
    assert_eq!(absent["verdict"], "absent");
    assert!(absent["doubt"].is_null());
    assert_eq!(absent["candidates"], 0);
}

/// An invented stream of `title` on the invented album, which started at
/// `start_ms` and played `played_ms`.
pub(crate) fn stream(
    start_ms: i64,
    played_ms: i64,
    title: &str,
) -> ritornel_contract::SpotifyStream {
    ritornel_contract::SpotifyStream {
        ended_at: start_ms + played_ms,
        ms_played: played_ms,
        track_uri: format!("spotify:track:{}", title.replace(' ', "")),
        title: title.to_owned(),
        artist: "Moss Arcade".to_owned(),
        album: Some("Tin Weather".to_owned()),
        reason_start: Some("trackdone".to_owned()),
        reason_end: Some("trackdone".to_owned()),
        shuffle: Some(false),
        skipped: Some(false),
        offline: Some(false),
        offline_timestamp: None,
        incognito: Some(false),
        platform: None,
        device_kind: None,
        via: None,
    }
}

#[test]
fn a_dry_run_offers_no_stream_a_scrobble_set_aside_and_writes_nothing() {
    let rig = rig();
    // An earlier read whose pages shifted stored Paper Kites twice in its
    // second, and Lanternfish not at all; a full read sets the second
    // Paper Kites aside.
    let shifted = batch(
        vec![
            record(T0 - 3 * HOUR, "Moss Arcade", "Paper Kites", 0),
            record(T0 - 3 * HOUR, "Moss Arcade", "Paper Kites", 1),
        ],
        T0 - 4 * HOUR,
        T0,
    );
    rig.ingest(&shifted);
    let held = batch(
        vec![
            record(T0 - 3 * HOUR, "Moss Arcade", "Paper Kites", 0),
            record(T0 - 3 * HOUR, "Moss Arcade", "Lanternfish", 0),
        ],
        T0 - 4 * HOUR,
        T0,
    );
    assert_eq!(rig.read(&held, true).set_aside, 1);
    let revision = rig.revision();

    // Two streams of it start within seconds of both scrobbles: offered,
    // the one set aside would join the second.
    let streams = vec![
        stream(T0 - 3 * HOUR + 2000, 200_000, "Paper Kites"),
        stream(T0 - 3 * HOUR - 5000, 210_000, "Paper Kites"),
    ];
    let report = rig.library.stream_dry_run(streams).unwrap();
    assert_eq!(rig.revision(), revision, "a dry run writes nothing");
    assert_eq!(
        (
            report.scrobbles,
            report.scrobbles_excluded,
            report.scrobbles_echoes
        ),
        (2, 1, 0)
    );
    assert_eq!(report.events, 2);
    let start = &report.tiers[0];
    assert_eq!((start.joined, start.in_force), (1, true));
    let year = report.years.iter().find(|year| year.streams > 0).unwrap();
    // Lanternfish has no stream and stays a scrobble alone.
    assert_eq!((year.joined_start, year.scrobbles_alone), (1, 1));
    // The other stream is alone: its track ran to its end, so its length
    // is known, and all of it was played.
    assert_eq!(year.alone_counted, 1);
    assert_eq!(year.plays_after, year.plays_before + 1);
    assert_eq!(year.counted_after, year.counted_before + 1);
}

/// The streams in force: those that a run whose newest ledger entry is
/// `applied` held.
fn in_force(rig: &Rig) -> String {
    rig.one(
        "SELECT count(DISTINCT h.observation_id) FROM import_run_stream h
         WHERE (SELECT state FROM import_run_event WHERE run_id = h.run_id
                ORDER BY id DESC LIMIT 1) = 'applied'",
    )
}

fn apply(rig: &Rig, run: u8, streams: Vec<ritornel_contract::SpotifyStream>) -> crate::StoredRun {
    rig.library
        .apply_streams(
            WriteRequest {
                method: crate::APPLY_METHOD,
                command: None,
                client: None,
            },
            Id::from_bytes([run; 16]),
            streams,
            "ritornelctl 0.1.0".to_owned(),
            None,
        )
        .unwrap()
}

fn undo(rig: &Rig, run: u8) -> crate::StoredRun {
    rig.library
        .undo_streams(
            WriteRequest {
                method: crate::UNDO_METHOD,
                command: None,
                client: None,
            },
            Id::from_bytes([run; 16]),
        )
        .unwrap()
}

#[test]
fn a_run_is_stored_once_in_one_write_and_undone_by_a_row_of_its_ledger() {
    let rig = rig();
    rig.ingest(&first_batch());
    let plays = rig.library.derived_history().unwrap();
    let archive = vec![
        stream(T0 - 3 * HOUR + 2000, 200_000, "Paper Kites"),
        stream(T0 - 2 * HOUR, 180_000, "Lanternfish"),
        stream(T0 - 2 * HOUR, 180_000, "Lanternfish"),
    ];
    let observations = "SELECT count(*) FROM observation_import_stream";

    // One revision stores the three, and changes no play.
    let revision = rig.revision();
    let first = apply(&rig, 1, archive.clone());
    assert_eq!(rig.revision(), revision + 1);
    assert_eq!(
        (first.streams, first.new_streams, first.in_force),
        (3, 3, true)
    );
    assert_eq!(rig.one(observations), "3");
    assert_eq!(rig.library.derived_history().unwrap(), plays);
    let rebuilt = rig
        .library
        .rebuild_history(
            WriteRequest {
                method: "history.rebuild",
                command: None,
                client: None,
            },
            true,
            |rebuilt| rebuilt.changed.to_string(),
        )
        .unwrap();
    assert_eq!(rebuilt, "0", "a rebuild finds nothing to change");

    // The same run again writes nothing; the same archive in a new run
    // stores no stream, and records the run.
    assert_eq!(apply(&rig, 1, archive.clone()), first);
    assert_eq!(rig.revision(), revision + 1);
    let again = apply(&rig, 2, archive.clone());
    assert_eq!((again.streams, again.new_streams), (3, 0));
    assert_eq!(rig.one(observations), "3");
    // A later archive stores only what this one lacked.
    let mut later = archive.clone();
    later.push(stream(T0 - HOUR, 200_000, "Salt Road"));
    let third = apply(&rig, 3, later);
    assert_eq!((third.streams, third.new_streams), (4, 1));
    assert_eq!(rig.one(observations), "4");
    assert_eq!(in_force(&rig), "4");

    // Undoing the first of the runs leaves in force what the others held.
    let revision = rig.revision();
    let undone = undo(&rig, 1);
    assert!(!undone.in_force);
    assert_eq!(rig.revision(), revision + 1);
    assert_eq!(in_force(&rig), "4");
    // Undone again, nothing is written.
    assert_eq!(undo(&rig, 1), undone);
    assert_eq!(rig.revision(), revision + 1);
    undo(&rig, 2);
    undo(&rig, 3);
    assert_eq!(in_force(&rig), "0");
    assert_eq!(rig.one(observations), "4", "nothing is removed");
    assert_eq!(rig.library.derived_history().unwrap(), plays);
    assert_eq!(
        rig.library
            .stored_runs()
            .unwrap()
            .iter()
            .map(|run| run.run)
            .collect::<Vec<_>>(),
        [3, 2, 1].map(|run| Id::from_bytes([run; 16]))
    );

    // The export carries each stream's own fields, the runs with their
    // ledgers, and what each held.
    let scratch = crate::testing::Scratch::new();
    let exported = rig.library.export_record(scratch.path(), T0).unwrap();
    assert_eq!((exported.runs, exported.run_streams), (3, 10));
    let observations = std::fs::read_to_string(scratch.path().join("observations.jsonl")).unwrap();
    let streams: Vec<serde_json::Value> = observations
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .filter(|line| line["import"]["stream"].is_object())
        .collect();
    assert_eq!(streams.len(), 4);
    assert_eq!(streams[0]["import"]["stream"]["platform"], "mac");
    let runs = std::fs::read_to_string(scratch.path().join("runs.jsonl")).unwrap();
    let first_run: serde_json::Value = serde_json::from_str(runs.lines().next().unwrap()).unwrap();
    assert_eq!(first_run["ledger"][0]["state"], "applied");
    assert_eq!(first_run["ledger"][1]["state"], "undone");
    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(scratch.path().join("manifest.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(manifest["version"], 3);
}
