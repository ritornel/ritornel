//! History at the size of a long Last.fm import: about 241,000 plays
//! against a library of about 14,000 tracks, all invented. It
//! measures what a wall and a page ask of history with no rollup table,
//! the counts of every song and one artist's plays by year (history.md,
//! "Queries M1 must answer"), and checks a small history gives the same
//! answers as counting by hand.
//!
//! The measurement is slow to set up, so it is ignored by default. Run it
//! with
//!
//! ```text
//! cargo test --release -p ritornel-engine imported_size -- --ignored --nocapture
//! ```

// What it measured is printed for whoever runs it.
#![allow(clippy::print_stderr)]

use std::time::Instant;

use ritornel_source::{Batch, Identifiers, ImportRecord, Span, TrackText};

use std::sync::Arc;

use crate::invent::testing::NOW;
use crate::invent::{Invented, Plan};
use crate::testing::{FixedClock, Scratch};
use crate::{
    Id, IngestOutcome, LibraryStore, Observations, OsEntropy, ServerStore, Today, WriteRequest,
    ZonePeriodRow,
};

/// An invented library of `albums` records, its IDs from the system's
/// random numbers: a history this size draws more of them than counted
/// ones can tell apart.
struct Library {
    store: LibraryStore,
    invented: Invented,
    _scratch: Scratch,
}

fn library(albums: usize) -> Library {
    let scratch = Scratch::new();
    let clock = Arc::new(FixedClock::at(NOW));
    let entropy = Arc::new(OsEntropy);
    let servers = ServerStore::open_in_memory(clock.clone(), entropy.clone()).unwrap();
    let (identity, _) = servers.ensure_identity("desk").unwrap();
    let request = WriteRequest {
        method: "library.create",
        command: None,
        client: None,
    };
    let (store, _) =
        LibraryStore::create_in_memory(&identity, request, |_| String::new(), clock, entropy)
            .unwrap();
    let plan = Plan {
        albums,
        seed: 11,
        root: scratch.path().join("root"),
        now_ms: NOW,
        cover_sides: (16, 16),
    };
    let invented = crate::invent::seed(&store, &servers, &plan, |_, _| {}).unwrap();
    Library {
        store,
        invented,
        _scratch: scratch,
    }
}

/// The tracks of the invented library: artist credit, album and title.
fn tracks(library: &LibraryStore) -> Vec<(String, String, String)> {
    library
        .read(|connection| {
            let mut statement = connection.prepare(
                "SELECT ac.display_text, rel.display_title, ta.display_title
                 FROM track_appearance ta
                 JOIN artist_credit ac ON ac.id = ta.display_artist_credit_id
                 JOIN release rel ON rel.id = ta.release_id
                 ORDER BY ta.id",
            )?;
            let rows =
                statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .unwrap()
}

/// A play of track `pick` of `tracks` at `at_s`, or of a stranger's track
/// when `pick` is past the end.
fn record(tracks: &[(String, String, String)], pick: usize, at_s: i64, n: usize) -> ImportRecord {
    let track = match tracks.get(pick) {
        Some((artist, album, title)) => TrackText {
            artist: artist.clone(),
            album: Some(album.clone()),
            title: title.clone(),
        },
        None => TrackText {
            artist: format!("Stranger {}", pick % 2_000),
            album: Some(format!("Elsewhere {}", pick % 5_000)),
            title: format!("Unowned {}", pick % 30_000),
        },
    };
    let mut key = b"size".to_vec();
    key.extend_from_slice(&at_s.to_be_bytes());
    key.extend_from_slice(&(n as u64).to_be_bytes());
    ImportRecord {
        source_key: key,
        played_at_ms: at_s * 1000,
        track,
        ids: Identifiers::default(),
        payload_json: format!("{{\"n\":{n}}}"),
    }
}

/// A random number generator that needs no crate: the same seed, the same
/// history.
struct Mix(u64);

impl Mix {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
}

/// Imports `plays` invented plays, from 2008 to the library's present, a
/// fifth of them of music the library does not have, the rest leaning on
/// a few favourites as a real history does.
fn import(library: &LibraryStore, account: Id, plays: usize) -> std::time::Duration {
    let tracks = tracks(library);
    let mut mix = Mix(7);
    let start_s = 1_199_145_600; // 2008-01-01
    let end_s = NOW / 1000 - 86_400;
    let step = (end_s - start_s) / i64::try_from(plays).unwrap();
    let began = Instant::now();
    let mut batch = Vec::new();
    for n in 0..plays {
        let roll = mix.next();
        let pick = if roll.is_multiple_of(5) {
            tracks.len() + usize::try_from(mix.next() % 1_000_000).unwrap()
        } else {
            // Squared, so the low end of the library is played most.
            let x = (mix.next() % 10_000) as f64 / 10_000.0;
            (x * x * tracks.len() as f64) as usize
        };
        let at = start_s + i64::try_from(n).unwrap() * step;
        batch.push(record(&tracks, pick, at, n));
        if batch.len() == 5_000 || n + 1 == plays {
            let from = batch.first().unwrap().played_at_ms;
            let to = batch.last().unwrap().played_at_ms + 1000;
            let records = std::mem::take(&mut batch);
            let stored = library
                .ingest(Observations::Imports {
                    account,
                    batch: &Batch {
                        records,
                        span: Span {
                            from_ms: from,
                            to_ms: to,
                        },
                        cursor: format!("{{\"readBefore\":{}}}", to / 1000),
                    },
                })
                .unwrap();
            assert!(matches!(stored, IngestOutcome::Imports(_)));
        }
    }
    began.elapsed()
}

fn zone(library: &LibraryStore, listener: Id) {
    library
        .set_zone_periods(
            WriteRequest {
                method: "history.setZones",
                command: None,
                client: None,
            },
            listener,
            vec![ZonePeriodRow {
                zone: "America/New_York".to_owned(),
                from_ms: None,
                to_ms: None,
            }],
            |_| String::new(),
        )
        .unwrap();
}

/// The artist with the most counted plays.
fn favourite(library: &LibraryStore) -> Id {
    library
        .read(|connection| {
            Ok(connection.query_row(
                "SELECT a.public_id FROM play_event pe
                 JOIN recording r ON r.id = pe.recording_id
                 JOIN artist_credit_name acn ON acn.artist_credit_id = r.display_artist_credit_id
                 JOIN artist a ON a.id = acn.artist_id
                 GROUP BY a.id ORDER BY count(*) DESC LIMIT 1",
                [],
                |row| Ok(Id::from_bytes(row.get(0)?)),
            )?)
        })
        .unwrap()
}

/// The slowest of `times` runs of `work`, after one to warm the cache.
fn slowest(times: usize, mut work: impl FnMut()) -> std::time::Duration {
    work();
    (0..times)
        .map(|_| {
            let began = Instant::now();
            work();
            began.elapsed()
        })
        .max()
        .unwrap_or_default()
}

#[test]
fn the_counts_and_an_artists_years_agree_with_counting_by_hand() {
    let invented = library(30);
    let store = &invented.store;
    let account = store.ensure_service_account("lastfm", "someone").unwrap();
    let listener = store.default_listener().unwrap().unwrap();
    zone(store, listener);
    import(store, account, 3_000);
    let counts = store.play_counts(listener).unwrap();
    let total: i64 = counts.iter().map(|(_, plays, _)| plays).sum();
    let by_hand: i64 = store
        .read(|connection| {
            Ok(connection.query_row(
                "SELECT count(*) FROM play_event WHERE recording_id IS NOT NULL AND counts = 1",
                [],
                |row| row.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(total, by_hand);
    assert!(
        total > 2_000,
        "four fifths of the plays are of the library: {total}"
    );
    let artist = favourite(store);
    let years = store.artist_plays_by_year(listener, artist).unwrap();
    assert!(years.windows(2).all(|pair| pair[0].0 < pair[1].0));
    assert!(years.first().unwrap().0 >= 2007 && years.last().unwrap().0 <= 2026);
    let of_artist: i64 = store
        .read(move |connection| {
            Ok(connection.query_row(
                "SELECT count(*) FROM play_event pe
                 JOIN recording r ON r.id = pe.recording_id
                 JOIN artist_credit_name acn ON acn.artist_credit_id = r.display_artist_credit_id
                 WHERE acn.artist_id = (SELECT id FROM artist WHERE public_id = ?1)
                   AND pe.counts = 1",
                [artist.as_bytes()],
                |row| row.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(years.iter().map(|(_, plays)| plays).sum::<i64>(), of_artist);
}

#[test]
#[ignore = "slow: about 241,000 plays; run it by name with --ignored"]
fn imported_size() {
    const PLAYS: usize = 241_000;
    let began = Instant::now();
    let invented = library(1_200);
    let store = &invented.store;
    eprintln!(
        "invented {} tracks of {} artists in {:?}",
        invented.invented.tracks,
        invented.invented.artists,
        began.elapsed()
    );
    let account = store.ensure_service_account("lastfm", "someone").unwrap();
    let listener = store.default_listener().unwrap().unwrap();
    zone(store, listener);
    let took = import(store, account, PLAYS);
    eprintln!("imported {PLAYS} plays through the ingest call in {took:?}");

    let counts = store.play_counts(listener).unwrap();
    let songs = slowest(5, || {
        assert_eq!(store.play_counts(listener).unwrap().len(), counts.len());
    });
    let artist = favourite(store);
    let years = slowest(5, || {
        assert!(
            !store
                .artist_plays_by_year(listener, artist)
                .unwrap()
                .is_empty()
        );
    });
    // What an artist's page and an album's read: the sentence, the years
    // and, for the artist, the records played most.
    let today = Today {
        now_ms: NOW,
        year: 2026,
        month: 11,
        date: 20261115,
    };
    let page = store
        .artist_history(listener, artist, today)
        .unwrap()
        .unwrap();
    let artist_page = slowest(5, || {
        store
            .artist_history(listener, artist, today)
            .unwrap()
            .unwrap();
    });
    let album = page.records.first().unwrap().tile.album;
    let album_page = slowest(5, || {
        store
            .album_history(listener, album, today)
            .unwrap()
            .unwrap();
    });
    // What Lean back reads for a song of the record played most: its facts,
    // with every play's hour of the listener's to hold the record's against.
    let song = store.album_page(album).unwrap().unwrap().tracks[0].appearance_id;
    let lean_back = slowest(5, || {
        store.track_history(listener, song, today).unwrap().unwrap();
    });
    // What the History tab reads: every counted play, by day and artist.
    let stream = slowest(3, || {
        assert!(store.history_stream(listener, today).unwrap().months > 200);
    });
    let report = slowest(1, || {
        store.import_report(account, 50).unwrap();
    });
    let began = Instant::now();
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
    let rebuild = began.elapsed();
    let matched = store.import_report(account, 0).unwrap();
    eprintln!(
        "counts of every song ({} songs): {songs:?}\n\
         one artist's plays by year ({} years): {years:?}\n\
         an artist's page ({} plays): {artist_page:?}\n\
         an album's page: {album_page:?}\n\
         Lean back's facts: {lean_back:?}\n\
         the History tab's stream: {stream:?}\n\
         the import report: {report:?}\n\
         a full rebuild: {rebuild:?}\n\
         plays {}, matched {}",
        counts.len(),
        store.artist_plays_by_year(listener, artist).unwrap().len(),
        page.plays,
        matched.plays,
        matched.matched_plays,
    );
}
