//! Who and what plays were heard as, on invented records and scrobbles:
//! an artist with no files, an album of the library's artist that it does
//! not have, a song credited to two artists, two namesakes, and a record
//! bought later.

use std::sync::Arc;

use ritornel_contract::{HeardAlbum, HeardArtist, HeardListParams, HeardOrder};
use ritornel_source::{Batch, Identifiers, ImportRecord, Span, TrackText};

use crate::testing::{CountingEntropy, FixedClock};
use crate::{
    Id, IngestOutcome, LibraryStore, Observations, ServerStore, Today, WriteRequest, ZonePeriodRow,
};

/// 2026-11-15 21:00:00 UTC.
const T0: i64 = 1_794_776_400_000;
const DAY: i64 = 86_400_000;

struct Rig {
    library: LibraryStore,
    account: Id,
    listener: Id,
}

fn rig() -> Rig {
    let clock = Arc::new(FixedClock::at(T0));
    let entropy = Arc::new(CountingEntropy::default());
    let servers = ServerStore::open_in_memory(clock.clone(), entropy.clone()).unwrap();
    let (server, _) = servers.ensure_identity("desk").unwrap();
    let (library, _) = LibraryStore::create_in_memory(
        &server,
        request("library.create"),
        |_| String::new(),
        clock,
        entropy,
    )
    .unwrap();
    let account = library.ensure_service_account("lastfm", "someone").unwrap();
    let listener = library
        .read(|connection| {
            Ok(
                connection.query_row("SELECT public_id FROM listener LIMIT 1", [], |row| {
                    Ok(Id::from_bytes(row.get(0)?))
                })?,
            )
        })
        .unwrap();
    // Where the listener was, so that a scrobble has a local date.
    library
        .set_zone_periods(
            request("history.setZones"),
            listener,
            vec![ZonePeriodRow {
                zone: "Europe/London".to_owned(),
                from_ms: None,
                to_ms: None,
            }],
            |_| String::new(),
        )
        .unwrap();
    Rig {
        library,
        account,
        listener,
    }
}

fn request(method: &'static str) -> WriteRequest {
    WriteRequest {
        method,
        command: None,
        client: None,
    }
}

/// A scrobble of `title` by `artist` from `album`, `days` before now.
fn scrobble(days: i64, artist: &str, album: &str, title: &str) -> ImportRecord {
    let at_ms = T0 - days * DAY;
    let mut key = at_ms.to_be_bytes().to_vec();
    key.extend_from_slice(title.as_bytes());
    ImportRecord {
        source_key: key,
        played_at_ms: at_ms,
        track: TrackText {
            artist: artist.to_owned(),
            album: Some(album.to_owned()),
            title: title.to_owned(),
        },
        ids: Identifiers::default(),
        payload_json: format!("{{\"name\":\"{title}\"}}"),
    }
}

impl Rig {
    fn ingest(&self, records: Vec<ImportRecord>) {
        let batch = Batch {
            records,
            span: Span {
                from_ms: T0 - 400 * DAY,
                to_ms: T0,
            },
            cursor: format!("{{\"readBefore\":{}}}", T0 / 1000),
        };
        match self
            .library
            .ingest(Observations::Imports {
                account: self.account,
                batch: &batch,
                full: false,
            })
            .unwrap()
        {
            IngestOutcome::Imports(_) => {}
            IngestOutcome::Listen(_) => unreachable!(),
        }
        self.library.match_library().unwrap();
    }

    /// Writes a record with a file for each song, its credit and each
    /// song's naming `artists`, each a new artist of the library; and then,
    /// as a resolve does, moves what the change to the catalog moves.
    fn record(&self, artists: &[&str], title: &str, songs: &[&str]) {
        let artists: Vec<String> = artists.iter().map(|name| (*name).to_owned()).collect();
        let (title, songs) = (
            title.to_owned(),
            songs
                .iter()
                .map(|song| (*song).to_owned())
                .collect::<Vec<_>>(),
        );
        self.library
            .write(request("test.record"), move |write| {
                let (c, rev) = (write.connection(), write.rev());
                let new = || write.new_id().map(|id| id.as_bytes().to_vec());
                let next = |table: &str| -> rusqlite::Result<i64> {
                    c.query_row(
                        &format!("SELECT coalesce(max(id), 0) + 1 FROM {table}"),
                        [],
                        |row| row.get(0),
                    )
                };
                let source: Option<i64> = rusqlite::OptionalExtension::optional(c.query_row(
                    "SELECT id FROM source_instance WHERE kind = 'folder'",
                    [],
                    |row| row.get(0),
                ))?;
                let source = match source {
                    Some(source) => source,
                    None => {
                        c.execute(
                            "INSERT INTO library_root (public_id, server_id, name, kind,
                                                       created_at_ms, rev)
                             VALUES (?1, (SELECT id FROM server LIMIT 1), 'music', 'master', 0, ?2)",
                            (new()?, rev),
                        )?;
                        let root = c.last_insert_rowid();
                        c.execute(
                            "INSERT INTO source_instance (public_id, kind, name, root_id,
                                                          created_at_ms, rev)
                             VALUES (?1, 'folder', 'music', ?2, 0, ?3)",
                            (new()?, root, rev),
                        )?;
                        c.last_insert_rowid()
                    }
                };
                c.execute(
                    "INSERT INTO artist_credit (display_text, rev) VALUES (?1, ?2)",
                    (artists.join(" & "), rev),
                )?;
                let credit = c.last_insert_rowid();
                for (position, name) in artists.iter().enumerate() {
                    let artist = next("artist")?;
                    c.execute(
                        "INSERT INTO artist (id, public_id, canonical_id, display_name, rev)
                         VALUES (?1, ?2, ?1, ?3, ?4)",
                        (artist, new()?, name, rev),
                    )?;
                    c.execute(
                        "INSERT INTO artist_credit_name (artist_credit_id, position, artist_id,
                                                         credited_name, join_phrase, rev)
                         VALUES (?1, ?2, ?3, ?4, ' & ', ?5)",
                        (credit, i64::try_from(position).unwrap_or(0), artist, name, rev),
                    )?;
                }
                c.execute(
                    "INSERT INTO release_group (public_id, rev) VALUES (?1, ?2)",
                    (new()?, rev),
                )?;
                let group = c.last_insert_rowid();
                let release = next("release")?;
                c.execute(
                    "INSERT INTO release (id, public_id, release_group_id, canonical_id,
                                          display_title, display_artist_credit_id, rev)
                     VALUES (?1, ?2, ?3, ?1, ?4, ?5, ?6)",
                    (release, new()?, group, &title, credit, rev),
                )?;
                c.execute(
                    "UPDATE release_group SET display_release_id = ?2 WHERE id = ?1",
                    (group, release),
                )?;
                for (number, song) in songs.iter().enumerate() {
                    let recording = next("recording")?;
                    c.execute(
                        "INSERT INTO recording (id, public_id, canonical_id, display_title,
                                                display_artist_credit_id, rev)
                         VALUES (?1, ?2, ?1, ?3, ?4, ?5)",
                        (recording, new()?, song, credit, rev),
                    )?;
                    let appearance = next("track_appearance")?;
                    c.execute(
                        "INSERT INTO track_appearance
                           (id, public_id, recording_id, release_id, canonical_id, display_title,
                            display_artist_credit_id, display_disc_number, display_track_number,
                            display_duration_ms, rev)
                         VALUES (?1, ?2, ?3, ?4, ?1, ?5, ?6, 1, ?7, 200000, ?8)",
                        (
                            appearance,
                            new()?,
                            recording,
                            release,
                            song,
                            credit,
                            i64::try_from(number).unwrap_or(0) + 1,
                            rev,
                        ),
                    )?;
                    c.execute(
                        "INSERT INTO source_item (public_id, source_instance_id, item_kind,
                                                  provider_key, size_bytes, mtime_ms,
                                                  first_seen_at_ms, current_recording_id, rev)
                         VALUES (?1, ?2, 'file', ?3, 1000, 0, 0, ?4, ?5)",
                        (new()?, source, format!("{title}/{song}.flac"), recording, rev),
                    )?;
                    let item = c.last_insert_rowid();
                    c.execute(
                        "INSERT INTO identity_decision
                           (action, source_item_id, method, resolver_version, normalizer_version,
                            evidence_json, actor_kind, decided_at_ms, rev)
                         VALUES ('bind', ?1, 'new', 1, 1, '{}', 'resolver', 0, ?2)",
                        (item, rev),
                    )?;
                    c.execute(
                        "INSERT INTO assignment (source_item_id, recording_id, appearance_id,
                                                 decision_id, effective_from_ms, rev)
                         VALUES (?1, ?2, ?3, ?4, 0, ?5)",
                        (item, recording, appearance, c.last_insert_rowid(), rev),
                    )?;
                }
                crate::history_store::catalog_changed(write)?;
                Ok(String::new())
            })
            .unwrap();
        self.library.match_library().unwrap();
    }

    fn artists(&self) -> Vec<HeardArtist> {
        self.library
            .heard_artists(self.listener, HeardListParams::default())
            .unwrap()
            .artists
    }

    fn albums(&self) -> Vec<HeardAlbum> {
        self.library
            .heard_albums(self.listener, HeardListParams::default())
            .unwrap()
            .albums
    }

    /// Every link, as the play's ID and what it links to, in order: what a
    /// rebuild must leave as it was.
    fn links(&self) -> Vec<String> {
        self.library
            .read(|connection| {
                let mut statement = connection.prepare(
                    "SELECT hex(pe.public_id) || ' ' || coalesce(ha.artist_id, ha.name_key)
                            || ' ' || coalesce(pa.spelling, '') || ' ' || pa.counted
                     FROM play_event_artist pa
                     JOIN play_event pe ON pe.id = pa.play_event_id
                     JOIN heard_artist ha ON ha.id = pa.heard_artist_id
                     UNION ALL
                     SELECT hex(pe.public_id) || ' album ' || coalesce(h.release_group_id,
                            (SELECT coalesce(ha.artist_id, ha.name_key) FROM heard_artist ha
                             WHERE ha.id = h.heard_artist_id) || '/' || h.name_key)
                            || ' ' || coalesce(pa.spelling, '')
                     FROM play_event_album pa
                     JOIN play_event pe ON pe.id = pa.play_event_id
                     JOIN heard_album h ON h.id = pa.heard_album_id
                     ORDER BY 1",
                )?;
                let rows = statement.query_map([], |row| row.get(0))?;
                Ok(rows.collect::<Result<_, _>>()?)
            })
            .unwrap()
    }

    fn rebuild(&self) {
        self.library
            .rebuild_history(request("history.rebuild"), false, |_| String::new())
            .unwrap();
    }

    fn today(&self) -> Today {
        Today {
            now_ms: T0,
            year: 2026,
            month: 11,
            date: 20_261_115,
        }
    }
}

fn plays_of<'a>(artists: &'a [HeardArtist], name: &str) -> Vec<(&'a str, u64, bool)> {
    artists
        .iter()
        .filter(|artist| artist.name == name)
        .map(|artist| {
            (
                artist.key.as_str(),
                artist.heard.plays,
                artist.artist_id.is_some(),
            )
        })
        .collect()
}

#[test]
fn an_artist_with_no_files_is_heard_by_their_name_spelled_as_played_most() {
    let rig = rig();
    rig.ingest(vec![
        scrobble(3, "Odile Castellan", "Harbour", "Night Ferry"),
        scrobble(2, "odile castellan", "Harbour", "Paper Lanterns"),
        scrobble(1, "Odile Castellan", "Harbour", "Low Tide"),
    ]);
    let artists = rig.artists();
    assert_eq!(
        plays_of(&artists, "Odile Castellan"),
        [("odile castellan", 3, false)]
    );
    let odile = &artists[0];
    assert_eq!(odile.heard.first_played.as_deref(), Some("2026-11-12"));
    assert_eq!(odile.heard.last_played.as_deref(), Some("2026-11-14"));
    let albums = rig.albums();
    assert_eq!(albums.len(), 1);
    assert_eq!(
        (
            albums[0].name.as_str(),
            albums[0].artist_name.as_str(),
            albums[0].heard.plays,
            albums[0].album_id
        ),
        ("Harbour", "Odile Castellan", 3, None)
    );
    // Their page, by their key, counts the same plays.
    let page = rig
        .library
        .heard_artist_history(rig.listener, "odile castellan".to_owned(), rig.today())
        .unwrap()
        .unwrap();
    assert_eq!(page.plays, 3);
    let album = rig
        .library
        .heard_album_history(
            rig.listener,
            "odile castellan".to_owned(),
            "harbour".to_owned(),
            rig.today(),
        )
        .unwrap()
        .unwrap();
    assert_eq!(album.plays, 3);
    assert!(
        rig.library
            .heard_artist_history(rig.listener, "nobody".to_owned(), rig.today())
            .unwrap()
            .is_none()
    );
}

#[test]
fn an_album_the_library_lacks_is_heard_under_the_librarys_artist() {
    let rig = rig();
    rig.record(
        &["Calla Brandt"],
        "Glasshouse",
        &["Low Tide", "Paper Lanterns"],
    );
    rig.ingest(vec![
        // A song of the record the library has, which binds.
        scrobble(4, "Calla Brandt", "Glasshouse", "Low Tide"),
        // A song of a record it does not have, heard by its names.
        scrobble(3, "Calla Brandt", "Night Ferry", "Harbour Lights"),
        scrobble(2, "The Calla Brandt", "Night Ferry", "Harbour Lights"),
    ]);
    let artists = rig.artists();
    assert_eq!(
        plays_of(&artists, "Calla Brandt"),
        [("calla brandt", 3, true)]
    );
    let mut albums: Vec<(String, u64, bool)> = rig
        .albums()
        .into_iter()
        .map(|album| (album.name, album.heard.plays, album.album_id.is_some()))
        .collect();
    albums.sort();
    assert_eq!(
        albums,
        [
            ("Glasshouse".to_owned(), 1, true),
            ("Night Ferry".to_owned(), 2, false)
        ]
    );
    // The album heard by its name has its page under its artist's key, and
    // the artist's key answers with the library's artist.
    let page = rig
        .library
        .heard_album_history(
            rig.listener,
            "calla brandt".to_owned(),
            "night ferry".to_owned(),
            rig.today(),
        )
        .unwrap()
        .unwrap();
    assert_eq!(page.plays, 2);
    let artist = rig
        .library
        .heard_artist_history(rig.listener, "calla brandt".to_owned(), rig.today())
        .unwrap()
        .unwrap();
    // The library's page counts the plays bound to its songs.
    assert_eq!(artist.plays, 1);
}

#[test]
fn a_song_credited_to_two_artists_counts_for_each() {
    let rig = rig();
    rig.record(
        &["Calla Brandt", "Odile Castellan"],
        "Two Rooms",
        &["Between Walls"],
    );
    rig.ingest(vec![scrobble(
        1,
        "Calla Brandt & Odile Castellan",
        "Two Rooms",
        "Between Walls",
    )]);
    let artists = rig.artists();
    assert_eq!(
        plays_of(&artists, "Calla Brandt"),
        [("calla brandt", 1, true)]
    );
    assert_eq!(
        plays_of(&artists, "Odile Castellan"),
        [("odile castellan", 1, true)]
    );
    // The album the list gives is found again by its keys.
    let albums = rig.albums();
    assert_eq!(albums.len(), 1);
    let page = rig
        .library
        .heard_album_history(
            rig.listener,
            albums[0].artist_key.clone(),
            albums[0].key.clone(),
            rig.today(),
        )
        .unwrap()
        .unwrap();
    assert_eq!(page.plays, 1);
}

#[test]
fn a_bound_play_follows_the_credit_of_the_appearance_it_was_heard_from() {
    let rig = rig();
    rig.record(&["Calla Brandt"], "Glasshouse", &["Low Tide"]);
    rig.ingest(vec![scrobble(1, "Calla Brandt", "Glasshouse", "Low Tide")]);
    assert_eq!(plays_of(&rig.artists(), "Odile Castellan"), []);
    // The edition's credit names a second artist, which the catalog's
    // change carries to the play without moving its binding.
    rig.library
        .write(request("test.credit"), |write| {
            let (c, rev) = (write.connection(), write.rev());
            let artist: i64 =
                c.query_row("SELECT coalesce(max(id), 0) + 1 FROM artist", [], |row| {
                    row.get(0)
                })?;
            c.execute(
                "INSERT INTO artist (id, public_id, canonical_id, display_name, rev)
                 VALUES (?1, ?2, ?1, 'Odile Castellan', ?3)",
                (artist, write.new_id()?.as_bytes().to_vec(), rev),
            )?;
            c.execute(
                "INSERT INTO artist_credit (display_text, rev)
                 VALUES ('Calla Brandt & Odile Castellan', ?1)",
                [rev],
            )?;
            let credit = c.last_insert_rowid();
            c.execute(
                "INSERT INTO artist_credit_name (artist_credit_id, position, artist_id,
                                                 credited_name, rev)
                 SELECT ?1, 0, id, display_name, ?2 FROM artist WHERE display_name = 'Calla Brandt'
                 UNION ALL SELECT ?1, 1, ?3, 'Odile Castellan', ?2",
                (credit, rev, artist),
            )?;
            c.execute(
                "UPDATE track_appearance SET display_artist_credit_id = ?1",
                [credit],
            )?;
            crate::history_store::catalog_changed(write)?;
            Ok(String::new())
        })
        .unwrap();
    let artists = rig.artists();
    assert_eq!(
        plays_of(&artists, "Calla Brandt"),
        [("calla brandt", 1, true)]
    );
    assert_eq!(
        plays_of(&artists, "Odile Castellan"),
        [("odile castellan", 1, true)]
    );
    // Her key, as the list gives it, finds her page, though she has no
    // record of her own.
    assert!(
        rig.library
            .heard_artist_history(rig.listener, "odile castellan".to_owned(), rig.today())
            .unwrap()
            .is_some()
    );
    let incremental = rig.links();
    rig.rebuild();
    assert_eq!(rig.links(), incremental);
}

#[test]
fn of_two_namesakes_neither_takes_a_play_by_name() {
    let rig = rig();
    rig.record(&["Moss Arcade"], "Tin Weather", &["Paper Kites"]);
    rig.record(&["Moss Arcade"], "Second Tide", &["Salt Road"]);
    // A song neither has: the name finds two artists, and stays a name.
    rig.ingest(vec![scrobble(1, "Moss Arcade", "Elsewhere", "Not Theirs")]);
    let artists = rig.artists();
    assert_eq!(
        plays_of(&artists, "Moss Arcade"),
        [("moss arcade", 1, false)]
    );
    // Its key answers with the name, not either artist.
    let page = rig
        .library
        .heard_artist_history(rig.listener, "moss arcade".to_owned(), rig.today())
        .unwrap()
        .unwrap();
    assert_eq!((page.plays, page.records.len()), (1, 0));
}

#[test]
fn a_record_bought_later_takes_its_plays_and_its_row_stays_one() {
    let rig = rig();
    rig.ingest(vec![
        scrobble(5, "Calla Brandt", "Glasshouse", "Low Tide"),
        scrobble(4, "Calla Brandt", "Glasshouse", "Paper Lanterns"),
        // A bonus track the edition bought lacks.
        scrobble(3, "Calla Brandt", "Glasshouse", "Hidden Room"),
    ]);
    let before = rig.albums();
    assert_eq!(before.len(), 1);
    assert_eq!((before[0].heard.plays, before[0].album_id), (3, None));
    let incremental = rig.links();
    rig.rebuild();
    assert_eq!(rig.links(), incremental, "a rebuild links as ingest did");

    rig.record(
        &["Calla Brandt"],
        "Glasshouse",
        &["Low Tide", "Paper Lanterns"],
    );
    let after = rig.albums();
    assert_eq!(after.len(), 1, "{after:?}");
    assert_eq!(after[0].heard.plays, 3);
    assert!(after[0].album_id.is_some());
    assert_eq!(after[0].key, before[0].key);
    let artists = rig.artists();
    assert_eq!(
        plays_of(&artists, "Calla Brandt"),
        [("calla brandt", 3, true)]
    );
    // Two of the three are bound to its songs; the bonus track is found by
    // its names.
    let bound = rig
        .library
        .read(|connection| {
            Ok(connection.query_row(
                "SELECT count(*) FROM play_event WHERE recording_id IS NOT NULL",
                [],
                |row| row.get::<_, i64>(0),
            )?)
        })
        .unwrap();
    assert_eq!(bound, 2);
    let incremental = rig.links();
    rig.rebuild();
    assert_eq!(
        rig.links(),
        incremental,
        "a rebuild links as the change did"
    );
}

#[test]
fn a_list_is_ordered_as_asked_and_paged() {
    let rig = rig();
    rig.ingest(vec![
        scrobble(30, "Odile Castellan", "Harbour", "Night Ferry"),
        scrobble(20, "Calla Brandt", "Glasshouse", "Low Tide"),
        scrobble(19, "Calla Brandt", "Glasshouse", "Paper Lanterns"),
        scrobble(10, "Søren Dunmore", "Lowlands", "Fen"),
    ]);
    let names = |order: HeardOrder, offset: u32| -> Vec<String> {
        rig.library
            .heard_artists(
                rig.listener,
                HeardListParams {
                    order: Some(order),
                    limit: Some(2),
                    offset: Some(offset),
                },
            )
            .unwrap()
            .artists
            .into_iter()
            .map(|artist| artist.name)
            .collect()
    };
    assert_eq!(
        names(HeardOrder::Plays, 0),
        ["Calla Brandt", "Odile Castellan"]
    );
    assert_eq!(names(HeardOrder::Plays, 2), ["Søren Dunmore"]);
    assert_eq!(
        names(HeardOrder::First, 0),
        ["Odile Castellan", "Calla Brandt"]
    );
    assert_eq!(
        names(HeardOrder::Last, 0),
        ["Søren Dunmore", "Calla Brandt"]
    );
    assert_eq!(
        rig.library
            .heard_artists(rig.listener, HeardListParams::default())
            .unwrap()
            .total,
        3
    );
}
