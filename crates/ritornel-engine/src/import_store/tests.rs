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
    assert_eq!(rig.library.match_scrobbles(rig.account).unwrap(), 1);
    assert_eq!(rig.one(bound), "1 960");
    assert_eq!(rig.bound(), ["Paper Kites -> Paper Kites"]);
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
}
