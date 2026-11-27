//! Views over an invented library: membership, order, sections, rows,
//! search, locating and rows that have gone.

use ritornel_contract::{
    FILTER_VERSION, Filter, GroupBy, ListKind, Predicate, Query, Row, Sort, SortKey,
};

use super::fold;
use crate::invent::testing::{Library, NOW, library, library_drawing};
use crate::invent::{PlaysPlan, invent_plays};
use crate::{Find, View, WriteRequest};

fn query(sort: SortKey) -> Query {
    Query {
        sort: Some(vec![Sort { key: sort }]),
        ..Query::default()
    }
}

fn open(library: &Library, kind: ListKind, query: &Query) -> View {
    library.store.list_open(kind, query).unwrap()
}

fn rows(library: &Library, resolved: &View) -> Vec<Row> {
    library
        .store
        .list_rows(resolved.kind, &resolved.ids)
        .unwrap()
}

fn titles(rows: &[Row]) -> Vec<String> {
    rows.iter()
        .map(|row| match row {
            Row::Artist(artist) => artist.name.clone().unwrap_or_default(),
            Row::Album(album) => album.title.clone().unwrap_or_default(),
            Row::Song(song) => song.title.clone().unwrap_or_default(),
            Row::Gone(_) => "<gone>".to_owned(),
        })
        .collect()
}

/// Whether the sections begin where the order changes letter, in order.
fn sections_are_letters(resolved: &View, keys: &[String]) {
    let letters: Vec<String> = resolved.sections.iter().map(|s| s.label.clone()).collect();
    let mut expected: Vec<String> = Vec::new();
    for key in keys {
        let letter = fold::letter(key).to_string();
        if expected.last() != Some(&letter) {
            expected.push(letter);
        }
    }
    assert_eq!(letters, expected);
    // A to Z, then #.
    let mut sorted = letters.clone();
    sorted.sort_by_key(|letter| (letter == "#", letter.clone()));
    assert_eq!(letters, sorted);
}

#[test]
fn artists_are_those_with_a_record_filed_a_to_z_with_hash_last() {
    let library = library(120);
    let resolved = open(&library, ListKind::Artists, &Query::default());
    assert_eq!(resolved.sort, SortKey::Name);
    let rows = rows(&library, &resolved);
    // Every invented artist with a record, and Various Artists if there is
    // a compilation.
    assert!(rows.len() >= library.invented.artists, "{}", rows.len());
    assert!(rows.len() <= library.invented.artists + 1);
    let keys: Vec<String> = rows
        .iter()
        .map(|row| match row {
            Row::Artist(artist) => fold::sort_text(
                artist.name.as_deref().unwrap_or_default(),
                artist.sort_name.as_deref(),
            ),
            other => panic!("{other:?}"),
        })
        .collect();
    let mut sorted = keys.clone();
    sorted.sort_by(|a, b| super::open::by_name_for_tests(a, b));
    assert_eq!(keys, sorted);
    sections_are_letters(&resolved, &keys);
    for row in &rows {
        let Row::Artist(artist) = row else { panic!() };
        assert!(artist.album_count >= 1, "{artist:?}");
        assert!(artist.track_count >= artist.album_count, "{artist:?}");
    }
    // Most artists are drawn with a cover; one whose records have none is
    // drawn without.
    let with_art = rows
        .iter()
        .filter(|row| matches!(row, Row::Artist(artist) if artist.art_id.is_some()))
        .count();
    assert!(
        with_art * 10 > rows.len() * 8,
        "{with_art} of {}",
        rows.len()
    );
}

#[test]
fn albums_are_one_tile_a_record_in_each_order_with_sections() {
    let library = library(150);
    for sort in [
        SortKey::Artist,
        SortKey::Title,
        SortKey::Year,
        SortKey::Added,
    ] {
        let resolved = open(&library, ListKind::Albums, &query(sort));
        assert_eq!(resolved.ids.len(), library.invented.groups, "{sort:?}");
        assert!(!resolved.sections.is_empty(), "{sort:?}");
        // The sections start at 0 and go up.
        assert_eq!(resolved.sections.first().map(|s| s.index), Some(0));
        assert!(
            resolved
                .sections
                .windows(2)
                .all(|pair| pair[0].index < pair[1].index)
        );
    }
    let by_year = open(&library, ListKind::Albums, &query(SortKey::Year));
    let rows = rows(&library, &by_year);
    let years: Vec<i64> = rows
        .iter()
        .map(|row| match row {
            Row::Album(album) => album.year.unwrap(),
            other => panic!("{other:?}"),
        })
        .collect();
    assert!(
        years.windows(2).all(|pair| pair[0] >= pair[1]),
        "newest first"
    );
    let labels: Vec<&str> = by_year.sections.iter().map(|s| s.label.as_str()).collect();
    assert_eq!(labels.first().copied(), Some(years[0].to_string().as_str()));
    // A year typed finds where it starts; nothing typed finds nothing.
    let locate = |typed: &str| library.store.list_locate_prefix(&by_year, typed).unwrap();
    assert_eq!(locate(&years[0].to_string()), Some(0));
    assert_eq!(locate("   "), None);

    // Some records have several versions, shown as one tile that counts
    // them, and some have no cover.
    let by_artist = open(&library, ListKind::Albums, &Query::default());
    let tiles =
        super::LibraryStore::list_rows(&library.store, ListKind::Albums, &by_artist.ids).unwrap();
    assert!(
        tiles
            .iter()
            .any(|row| matches!(row, Row::Album(a) if a.version_count > 1))
    );
    assert!(
        tiles
            .iter()
            .any(|row| matches!(row, Row::Album(a) if a.art_id.is_none()))
    );
    for row in &tiles {
        let Row::Album(album) = row else { panic!() };
        assert!(album.track_count >= 1);
        assert!(album.duration_ms > 0);
        assert!(album.quality.is_some());
        assert!(
            album
                .artist_credit
                .as_ref()
                .is_some_and(|c| !c.artist_ids.is_empty())
        );
    }
    // Added: the newest are this week.
    let added = open(&library, ListKind::Albums, &query(SortKey::Added));
    assert!(
        added
            .sections
            .iter()
            .any(|s| s.label.chars().all(|c| c.is_ascii_digit()))
    );
}

#[test]
fn a_query_a_list_does_not_take_is_refused() {
    let library = library(5);
    let refused = library
        .store
        .list_open(ListKind::Artists, &query(SortKey::Year));
    assert!(
        matches!(refused, Err(super::ListError::Invalid(_))),
        "{refused:?}"
    );
    let two = Query {
        sort: Some(vec![
            Sort {
                key: SortKey::Title,
            },
            Sort { key: SortKey::Year },
        ]),
        ..Query::default()
    };
    let refused = library.store.list_open(ListKind::Albums, &two);
    assert!(
        matches!(refused, Err(super::ListError::Invalid(_))),
        "{refused:?}"
    );
    let later = Query {
        filter: Some(Filter {
            version: FILTER_VERSION + 1,
            predicate: Predicate::ByArtist {
                artist_id: crate::Id::from_bytes([1; 16]),
            },
        }),
        ..Query::default()
    };
    let refused = library.store.list_open(ListKind::Albums, &later);
    assert!(
        matches!(refused, Err(super::ListError::Unsupported(_))),
        "{refused:?}"
    );
}

#[test]
fn songs_hide_the_other_versions_of_what_a_collapsed_record_shows() {
    let library = library(150);
    let resolved = open(&library, ListKind::Songs, &Query::default());
    assert!(resolved.ids.len() < library.invented.tracks);
    let rows = rows(&library, &resolved);
    let mut appearances = std::collections::HashSet::new();
    let mut recordings_by_album: std::collections::HashMap<String, usize> = Default::default();
    for row in &rows {
        let Row::Song(song) = row else {
            panic!("{row:?}")
        };
        assert!(appearances.insert(song.appearance_id));
        assert!(!song.copies.is_empty());
        *recordings_by_album
            .entry(song.album_title.clone().unwrap_or_default())
            .or_default() += 1;
    }
    let by_album = open(&library, ListKind::Songs, &query(SortKey::Album));
    assert_eq!(by_album.ids.len(), resolved.ids.len());
}

#[test]
fn text_matches_ignoring_case_and_accents_and_short_text_matches_a_word_start() {
    let library = library(150);
    let all = open(&library, ListKind::Songs, &Query::default());
    let all_rows = rows(&library, &all);
    // A word some title has, typed in capitals.
    let Row::Song(first) = &all_rows[0] else {
        panic!()
    };
    let title = first.title.clone().unwrap();
    let word = title
        .split(' ')
        .find(|word| word.chars().count() >= 3)
        .unwrap();
    let text = Query {
        text: Some(word.to_uppercase()),
        ..Query::default()
    };
    let found = open(&library, ListKind::Songs, &text);
    assert!(found.ids.contains(&all.ids[0]));
    let found_rows = rows(&library, &found);
    let needle = fold::needle(word);
    for row in &found_rows {
        let Row::Song(song) = row else { panic!() };
        let haystack = format!(
            "{} {} {}",
            song.title.as_deref().unwrap_or_default(),
            song.artist_credit.as_ref().map_or("", |c| c.text.as_str()),
            song.album_title.as_deref().unwrap_or_default()
        );
        assert!(fold::matches(&haystack, &needle), "{haystack} for {needle}");
    }

    // Two characters: a word that starts with them.
    let short = Query {
        text: Some(word.chars().take(2).collect::<String>().to_lowercase()),
        ..Query::default()
    };
    let found = open(&library, ListKind::Songs, &short);
    assert!(found.ids.contains(&all.ids[0]));

    // Albums by text, through the title or the artist.
    let albums = open(&library, ListKind::Albums, &Query::default());
    let album_rows = rows(&library, &albums);
    let Row::Album(album) = &album_rows[0] else {
        panic!()
    };
    let artist = album.artist_credit.clone().unwrap().text;
    let by_artist_text = Query {
        text: Some(artist.clone()),
        ..Query::default()
    };
    let found = open(&library, ListKind::Albums, &by_artist_text);
    assert!(found.ids.contains(&albums.ids[0]), "{artist}");
}

#[test]
fn accents_match_both_ways() {
    let library = library(5);
    library
        .store
        .write(
            WriteRequest {
                method: "test.write",
                command: None,
                client: None,
            },
            |write| {
                write.connection().execute(
                    "UPDATE search_text SET title = 'Sigur Rós Café' WHERE rowid = (SELECT min(rowid) FROM search_text)",
                    [],
                )?;
                write.connection().execute(
                    "UPDATE track_appearance SET display_title = 'Sigur Rós Café' WHERE id = (SELECT min(id) FROM track_appearance)",
                    [],
                )?;
                Ok(String::new())
            },
        )
        .unwrap();
    for typed in ["ROS CAFE", "rós", "cafe", "Ca", "s"] {
        let found = open(
            &library,
            ListKind::Songs,
            &Query {
                text: Some(typed.to_owned()),
                ..Query::default()
            },
        );
        let rows = rows(&library, &found);
        assert!(
            titles(&rows).iter().any(|title| title == "Sigur Rós Café"),
            "{typed}: {:?}",
            titles(&rows)
        );
    }
}

#[test]
fn a_filter_by_artist_gives_their_records_and_songs() {
    let library = library(150);
    let artists = open(&library, ListKind::Artists, &Query::default());
    let artist_rows = rows(&library, &artists);
    let busiest = artist_rows
        .iter()
        .filter_map(|row| match row {
            Row::Artist(artist) => Some(artist),
            _ => None,
        })
        .max_by_key(|artist| artist.album_count)
        .unwrap();
    let filter = Query {
        filter: Some(Filter {
            version: FILTER_VERSION,
            predicate: Predicate::ByArtist {
                artist_id: busiest.artist_id,
            },
        }),
        ..query(SortKey::Year)
    };
    let albums = open(&library, ListKind::Albums, &filter);
    assert_eq!(albums.ids.len() as u64, busiest.album_count);
    for row in rows(&library, &albums) {
        let Row::Album(album) = row else { panic!() };
        assert!(
            album
                .artist_credit
                .unwrap()
                .artist_ids
                .contains(&busiest.artist_id)
        );
    }
    // Their songs are the whole list's songs of theirs, in the same order,
    // with the same versions hidden, though only their records are read.
    for sort in [SortKey::Title, SortKey::Album] {
        let theirs = open(
            &library,
            ListKind::Songs,
            &Query {
                sort: Some(vec![Sort { key: sort }]),
                ..filter.clone()
            },
        );
        assert!(!theirs.ids.is_empty());
        let all = open(&library, ListKind::Songs, &query(sort));
        let credited: Vec<i64> = all
            .ids
            .iter()
            .zip(rows(&library, &all))
            .filter(|(_, row)| match row {
                Row::Song(song) => song
                    .artist_credit
                    .as_ref()
                    .is_some_and(|credit| credit.artist_ids.contains(&busiest.artist_id)),
                _ => false,
            })
            .map(|(id, _)| *id)
            .collect();
        assert_eq!(theirs.ids, credited);
    }
}

#[test]
fn a_row_found_by_id_or_by_the_start_of_its_name() {
    let library = library(150);
    let artists = open(&library, ListKind::Artists, &Query::default());
    let artist_rows = rows(&library, &artists);
    let Row::Artist(tenth) = &artist_rows[10] else {
        panic!()
    };
    let key = library
        .store
        .list_key(ListKind::Artists, &tenth.artist_id.to_string())
        .unwrap()
        .unwrap();
    assert_eq!(artists.ids.iter().position(|id| *id == key), Some(10));
    assert_eq!(
        library
            .store
            .list_key(ListKind::Artists, "nonsense")
            .unwrap(),
        None
    );

    let name = tenth.name.clone().unwrap();
    let typed: String = fold::sort_text(&name, tenth.sort_name.as_deref())
        .chars()
        .take(4)
        .collect();
    let found = library
        .store
        .list_locate_prefix(&artists, &typed)
        .unwrap()
        .unwrap();
    assert!(found <= 10);
    let Row::Artist(at) = &artist_rows[found as usize] else {
        panic!()
    };
    assert!(
        fold::sort_text(at.name.as_deref().unwrap(), at.sort_name.as_deref()).starts_with(&typed),
        "{typed}"
    );
    // Past the last letter is the first name filed under #, if there is
    // one, and otherwise no row.
    let hash = artists
        .sections
        .iter()
        .find(|section| section.label == "#")
        .map(|section| section.index);
    assert_eq!(
        library
            .store
            .list_locate_prefix(&artists, "zzzzzz")
            .unwrap(),
        hash
    );

    // Albums by their ref.
    let albums = open(&library, ListKind::Albums, &query(SortKey::Title));
    let album_rows = rows(&library, &albums);
    let Row::Album(fifth) = &album_rows[5] else {
        panic!()
    };
    let key = library
        .store
        .list_key(ListKind::Albums, &fifth.album.id().to_string())
        .unwrap()
        .unwrap();
    assert_eq!(albums.ids.iter().position(|id| *id == key), Some(5));
}

#[test]
fn a_row_that_has_gone_since_the_view_was_opened_is_gone_at_its_index() {
    let library = library(40);
    let albums = open(&library, ListKind::Albums, &Query::default());
    let artists = open(&library, ListKind::Artists, &Query::default());
    let songs = open(&library, ListKind::Songs, &Query::default());
    // Merge away the first record's releases, the first artist and the
    // first song, as a merge would.
    let (group, artist, song) = (albums.ids[0], artists.ids[0], songs.ids[0]);
    library
        .store
        .write(
            WriteRequest {
                method: "test.write",
                command: None,
                client: None,
            },
            move |write| {
                let connection = write.connection();
                connection.execute_batch("PRAGMA defer_foreign_keys = ON")?;
                let other: i64 = connection.query_row(
                    "SELECT min(id) FROM release WHERE release_group_id <> ?1",
                    [group],
                    |row| row.get(0),
                )?;
                connection.execute(
                    "UPDATE release SET redirect_to_id = ?1, canonical_id = ?1 WHERE release_group_id = ?2",
                    (other, group),
                )?;
                let other: i64 = connection.query_row(
                    "SELECT min(id) FROM artist WHERE id <> ?1",
                    [artist],
                    |row| row.get(0),
                )?;
                connection.execute(
                    "UPDATE artist SET redirect_to_id = ?1, canonical_id = ?1 WHERE id = ?2",
                    (other, artist),
                )?;
                let other: i64 = connection.query_row(
                    "SELECT min(id) FROM track_appearance WHERE id <> ?1",
                    [song],
                    |row| row.get(0),
                )?;
                connection.execute(
                    "UPDATE track_appearance SET redirect_to_id = ?1, canonical_id = ?1 WHERE id = ?2",
                    (other, song),
                )?;
                Ok(String::new())
            },
        )
        .unwrap();
    for (resolved, kind) in [
        (&albums, ListKind::Albums),
        (&artists, ListKind::Artists),
        (&songs, ListKind::Songs),
    ] {
        let rows = library.store.list_rows(kind, &resolved.ids[..3]).unwrap();
        assert!(matches!(rows[0], Row::Gone(_)), "{kind:?}: {:?}", rows[0]);
        assert!(!matches!(rows[1], Row::Gone(_)), "{kind:?}");
    }
    // A view opened now does not have them.
    let again = open(&library, ListKind::Albums, &Query::default());
    assert_eq!(again.ids.len(), albums.ids.len() - 1);
    assert!(again.revision > albums.revision);
}

#[test]
fn a_view_of_many_rows_is_one_integer_a_row() {
    let library = library(60);
    let songs = open(&library, ListKind::Songs, &Query::default());
    let per_row = songs.bytes() as f64 / songs.ids.len() as f64;
    assert!(per_row < 12.0, "{per_row}");
}

/// What the scanner writes is what the walls read: the fixtures scanned
/// into a library, then listed.
#[test]
fn a_scanned_root_is_listed_as_the_scan_made_it() {
    let mut rig = crate::scan_tests::Rig::new();
    // One song in two files of one record, and the same audio retagged as
    // a loose song by someone else.
    rig.put("Moss Arcade/Tin Weather/01.flac", "a.flac");
    rig.put("Moss Arcade/Tin Weather/02.mp3", "a.mp3");
    rig.put("Elsewhere/03.flac", "a.retagged.flac");
    rig.scan();
    let list = |kind: ListKind, text: Option<&str>| {
        let query = Query {
            text: text.map(str::to_owned),
            ..Query::default()
        };
        let view = rig.library.list_open(kind, &query).unwrap();
        let rows = rig.library.list_rows(kind, &view.ids).unwrap();
        (view, rows)
    };

    // The artist with a record, and not the one with only a loose song.
    let (_, artists) = list(ListKind::Artists, None);
    let [Row::Artist(artist)] = &artists[..] else {
        panic!("{artists:?}");
    };
    assert_eq!(artist.name.as_deref(), Some("Moss Arcade"));
    assert_eq!((artist.album_count, artist.track_count), (1, 2));

    // The record, with its first year, its cover and both its files.
    let (albums_view, albums) = list(ListKind::Albums, None);
    let [Row::Album(album)] = &albums[..] else {
        panic!("{albums:?}");
    };
    assert_eq!(album.title.as_deref(), Some("Tin Weather"));
    assert_eq!(
        album
            .artist_credit
            .as_ref()
            .map(|credit| credit.text.as_str()),
        Some("Moss Arcade")
    );
    assert_eq!((album.year, album.track_count), (Some(2004), 2));
    assert!(album.art_id.is_some());
    assert_eq!(artist.art_id, album.art_id);
    let quality = album.quality.as_ref().unwrap();
    assert_eq!(
        (quality.best.codec.as_str(), quality.varies),
        ("flac", true)
    );
    assert_eq!(albums_view.sections.len(), 1);

    // Every song, filed by title, and found by what was typed.
    let (songs_view, songs) = list(ListKind::Songs, None);
    assert_eq!(
        titles(&songs),
        ["Another Name Entirely", "Paper Kites", "Paper Kites"]
    );
    let letters: Vec<_> = songs_view
        .sections
        .iter()
        .map(|s| s.label.as_str())
        .collect();
    assert_eq!(letters, ["A", "P"]);
    assert_eq!(titles(&list(ListKind::Songs, Some("KITES")).1).len(), 2);
    assert_eq!(
        titles(&list(ListKind::Songs, Some("an")).1),
        ["Another Name Entirely"]
    );
}

#[test]
fn an_artists_records_show_no_plays_and_their_other_songs_do() {
    // The tape's rule (`View::shows_plays`): a change of plays alone tells
    // only the views whose rows show plays. An artist's discography is of
    // records, which show none; their songs on no record are songs, which
    // do. Neither filters or sorts on plays, so neither reorders for one.
    let library = library(40);
    let artists = open(&library, ListKind::Artists, &Query::default());
    let Some(Row::Artist(artist)) = rows(&library, &artists).into_iter().next() else {
        panic!("an artist")
    };
    let records = Query {
        filter: Some(Filter {
            version: FILTER_VERSION,
            predicate: Predicate::WithArtist {
                artist_id: artist.artist_id,
            },
        }),
        ..query(SortKey::Discography)
    };
    let records = open(&library, ListKind::Albums, &records);
    assert!(!records.ids.is_empty());
    assert!(!records.shows_plays());
    let loose = Query {
        filter: Some(Filter {
            version: FILTER_VERSION,
            predicate: Predicate::All {
                of: vec![
                    Predicate::ByArtist {
                        artist_id: artist.artist_id,
                    },
                    Predicate::NoRecord,
                ],
            },
        }),
        ..Query::default()
    };
    assert!(open(&library, ListKind::Songs, &loose).shows_plays());
}

/// A record whose files name no artist is on the Artists wall under
/// Unknown Artist, whose page lists it.
#[test]
fn a_record_whose_files_name_no_artist_is_unknown_artists() {
    let mut rig = crate::scan_tests::Rig::new();
    rig.put("Moss Arcade/Tin Weather/02.flac", "a.flac");
    rig.put("Field Notes/01.flac", "b.no-artist.flac");
    rig.scan();
    let open = |kind: ListKind, query: &Query| {
        let view = rig.library.list_open(kind, query).unwrap();
        rig.library.list_rows(kind, &view.ids).unwrap()
    };
    let artists = open(ListKind::Artists, &Query::default());
    assert_eq!(titles(&artists), ["Moss Arcade", "Unknown Artist"]);
    let Row::Artist(unknown) = &artists[1] else {
        panic!("{artists:?}")
    };
    assert_eq!((unknown.album_count, unknown.track_count), (1, 1));
    let theirs = Query {
        filter: Some(Filter {
            version: FILTER_VERSION,
            predicate: Predicate::WithArtist {
                artist_id: unknown.artist_id,
            },
        }),
        ..query(SortKey::Discography)
    };
    assert_eq!(titles(&open(ListKind::Albums, &theirs)), ["Field Notes"]);
}

/// A library that catalog version 2 resolved showed such a record as
/// Various Artists'. The next scan of its unchanged files resolves it again,
/// keeps every row it had, and shows it as Unknown Artist's; the scan after
/// that changes nothing.
#[test]
fn a_record_shown_as_various_artists_before_keeps_its_rows_as_unknown_artists() {
    let mut rig = crate::scan_tests::Rig::new();
    rig.put("Moss Arcade/Tin Weather/02.flac", "a.flac");
    rig.put("Field Notes/01.flac", "b.no-artist.flac");
    rig.scan();
    let rows = "SELECT r.id, r.release_group_id, ta.id, ta.recording_id
                FROM release r JOIN track_appearance ta ON ta.release_id = r.id
                WHERE r.display_title = 'Field Notes'";
    let before = rig.one(rows);
    let shown = "SELECT ac.display_text FROM release r
                 JOIN artist_credit ac ON ac.id = r.display_artist_credit_id
                 WHERE r.display_title = 'Field Notes'";
    assert_eq!(rig.one(shown), "Unknown Artist");

    // As version 2 left it: the record credited to Various Artists, whose
    // artist the claims still name, and the library marked as resolved by
    // that version.
    let request = WriteRequest {
        method: "test.catalog",
        command: None,
        client: None,
    };
    rig.library
        .write(request, |write| {
            let connection = write.connection();
            let various: i64 = connection.query_row(
                "SELECT id FROM artist WHERE display_name = 'Various Artists'",
                [],
                |row| row.get(0),
            )?;
            connection.execute(
                "INSERT INTO artist_credit (display_text, rev) VALUES ('Various Artists', ?1)",
                [write.rev()],
            )?;
            let credit = connection.last_insert_rowid();
            connection.execute(
                "INSERT INTO artist_credit_name
                   (artist_credit_id, position, artist_id, credited_name, rev)
                 VALUES (?1, 0, ?2, 'Various Artists', ?3)",
                (credit, various, write.rev()),
            )?;
            connection.execute(
                "UPDATE release SET display_artist_credit_id = ?1
                 WHERE display_title = 'Field Notes'",
                [credit],
            )?;
            let versions: String = connection.query_row(
                "SELECT value_json FROM library_setting WHERE key = 'identity.versions'",
                [],
                |row| row.get(0),
            )?;
            let old = versions.replace(
                &format!("\"catalog\":{}", ritornel_domain::CATALOG_VERSION),
                "\"catalog\":2",
            );
            assert_ne!(old, versions);
            connection.execute(
                "UPDATE library_setting SET value_json = ?1 WHERE key = 'identity.versions'",
                [old],
            )?;
            Ok(String::new())
        })
        .unwrap();
    assert_eq!(rig.one(shown), "Various Artists");

    rig.scan();
    assert_eq!(rig.one(rows), before);
    assert_eq!(rig.one(shown), "Unknown Artist");
    let settled = rig.dump();
    rig.scan();
    assert_eq!(rig.dump(), settled);
}

/// When every file of a record is retagged to join another that the
/// library has, the record they left keeps its rows, for whatever named
/// them, and is on no wall, in no search and not in what the scrobble
/// matcher reads.
#[test]
fn a_record_whose_files_joined_another_is_on_no_wall() {
    let mut rig = crate::scan_tests::Rig::new();
    rig.put("Moss Arcade/Tin Weather/05.flac", "b.flac");
    rig.put("Moss Arcade/Tin Weather/07.flac", "b.compilation.flac");
    rig.scan();
    let open = |rig: &crate::scan_tests::Rig, kind: ListKind, query: &Query| {
        let view = rig.library.list_open(kind, query).unwrap();
        let rows = rig.library.list_rows(kind, &view.ids).unwrap();
        (view, rows)
    };
    let mut before = titles(&open(&rig, ListKind::Albums, &Query::default()).1);
    before.sort();
    assert_eq!(before, ["Night Drives", "Tin Weather"]);
    let artists = titles(&open(&rig, ListKind::Artists, &Query::default()).1);
    assert_eq!(artists, ["Moss Arcade", "Various Artists"]);

    // The compilation's only file is retagged as the artist's own record's
    // copy of the song, so the two releases are one.
    rig.put("Moss Arcade/Tin Weather/07.flac", "b.flac");
    rig.scan();
    // The release no file reaches, and its song.
    let reached =
        "EXISTS (SELECT 1 FROM assignment a JOIN source_item si ON si.id = a.source_item_id
                    WHERE a.appearance_id = ta.id AND a.superseded_at_ms IS NULL
                      AND si.item_kind = 'file' AND si.retired_at_ms IS NULL)";
    let ghost = rig.one(&format!(
        "SELECT r.id, rg.public_id, ta.id FROM release r
         JOIN release_group rg ON rg.id = r.release_group_id
         JOIN track_appearance ta ON ta.release_id = r.id
         WHERE r.redirect_to_id IS NULL AND ta.redirect_to_id IS NULL AND NOT {reached}"
    ));
    let [release, group, song] = ghost.split('|').collect::<Vec<_>>()[..] else {
        panic!("{ghost}")
    };
    let (release, song): (i64, i64) = (release.parse().unwrap(), song.parse().unwrap());

    let (albums, rows) = open(&rig, ListKind::Albums, &Query::default());
    assert_eq!(titles(&rows), ["Tin Weather"]);
    assert!(!albums.ids.contains(&-release), "{:?}", albums.ids);
    let (_, artists) = open(&rig, ListKind::Artists, &Query::default());
    assert_eq!(titles(&artists), ["Moss Arcade"]);
    let Row::Artist(artist) = &artists[0] else {
        panic!("{artists:?}")
    };
    assert_eq!((artist.album_count, artist.track_count), (1, 1));
    // The artist's page: their records, and those they are on a song of.
    let theirs = Query {
        filter: Some(Filter {
            version: FILTER_VERSION,
            predicate: Predicate::WithArtist {
                artist_id: artist.artist_id,
            },
        }),
        ..query(SortKey::Discography)
    };
    assert_eq!(
        titles(&open(&rig, ListKind::Albums, &theirs).1),
        ["Tin Weather"]
    );
    let (songs, rows) = open(&rig, ListKind::Songs, &Query::default());
    assert!(!songs.ids.contains(&song), "{:?}", songs.ids);
    assert_eq!(titles(&rows), ["Paper Kites"]);
    for typed in ["night", "paper"] {
        let found = rig.library.search(typed, 10).unwrap();
        assert!(
            found
                .albums
                .iter()
                .all(|album| album.title.as_deref() == Some("Tin Weather")),
            "{typed}: {:?}",
            found.albums
        );
        assert!(found.songs.len() <= 1, "{typed}: {:?}", found.songs);
    }
    let group: Vec<u8> = (0..group.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&group[at..at + 2], 16).unwrap())
        .collect();
    let page = rig
        .library
        .album_page(ritornel_contract::AlbumRef::ReleaseGroup {
            id: ritornel_contract::Id::from_bytes(group.try_into().unwrap()),
        })
        .unwrap();
    // The record's own group has no other version: it has no page.
    assert!(page.is_none(), "{page:#?}");
    // Nor do portraits come for its artist.
    let pictured = rig
        .library
        .read(crate::artist_pictures::artist_names)
        .unwrap();
    assert_eq!(pictured, [Some("Moss Arcade".to_owned())]);

    // Nor is it played: not from a view opened before, nor by its ID, nor
    // as a record.
    assert_eq!(rig.library.song_ids(&[song]).unwrap(), [None]);
    let id = rig.one(&format!(
        "SELECT public_id FROM track_appearance WHERE id = {song}"
    ));
    let id: Vec<u8> = (0..id.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&id[at..at + 2], 16).unwrap())
        .collect();
    let id = ritornel_contract::Id::from_bytes(id.try_into().unwrap());
    assert_eq!(rig.library.unknown_appearance(&[id]).unwrap(), Some(id));
    let public = rig.one(&format!(
        "SELECT public_id FROM release WHERE id = {release}"
    ));
    let public: Vec<u8> = (0..public.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&public[at..at + 2], 16).unwrap())
        .collect();
    let public = ritornel_contract::Id::from_bytes(public.try_into().unwrap());
    let tracks = rig.library.release_tracks(public).unwrap();
    assert!(tracks.as_ref().is_some_and(Vec::is_empty), "{tracks:?}");
    let page = rig
        .library
        .album_page(ritornel_contract::AlbumRef::Release { id: public })
        .unwrap();
    assert!(page.is_none(), "{page:#?}");

    // A group set to show the release, beside one a file reaches, shows
    // and is found by the one a file reaches.
    rig.library
        .write(
            WriteRequest {
                method: "test.group",
                command: None,
                client: None,
            },
            move |write| {
                let connection = write.connection();
                connection.execute(
                    "UPDATE release SET release_group_id =
                       (SELECT release_group_id FROM release WHERE id <> ?1 AND display_title = 'Tin Weather')
                     WHERE id = ?1",
                    [release],
                )?;
                connection.execute(
                    "UPDATE release_group SET display_release_id = ?1
                     WHERE id = (SELECT release_group_id FROM release WHERE id = ?1)",
                    [release],
                )?;
                Ok(String::new())
            },
        )
        .unwrap();
    let (albums, rows) = open(&rig, ListKind::Albums, &query(SortKey::Title));
    assert_eq!(titles(&rows), ["Tin Weather"]);
    assert_eq!(
        rig.library.list_locate_prefix(&albums, "tin").unwrap(),
        Some(0)
    );
    let Row::Album(tile) = &rows[0] else {
        panic!("{rows:?}")
    };
    let page = rig.library.album_page(tile.album).unwrap().unwrap();
    assert!(page.versions.is_empty(), "{:#?}", page.versions);
    assert_eq!(page.tracks.len(), 1);

    // A file that is missing still reaches its record; one retired, when
    // another song took its path, does not.
    let set = |sql: &'static str| {
        rig.library
            .write(
                WriteRequest {
                    method: "test.items",
                    command: None,
                    client: None,
                },
                move |write| {
                    write.connection().execute(sql, [])?;
                    Ok(String::new())
                },
            )
            .unwrap();
    };
    set("UPDATE source_item SET missing_since_ms = 1 WHERE item_kind = 'file'");
    assert_eq!(
        titles(&open(&rig, ListKind::Albums, &Query::default()).1),
        ["Tin Weather"]
    );
    set("UPDATE source_item SET retired_at_ms = 1 WHERE item_kind = 'file'");
    assert!(open(&rig, ListKind::Albums, &Query::default()).1.is_empty());
    assert!(open(&rig, ListKind::Songs, &Query::default()).1.is_empty());
    set(
        "UPDATE source_item SET retired_at_ms = NULL, missing_since_ms = NULL
         WHERE item_kind = 'file'",
    );

    // The matcher reads only what a file reaches.
    let recordings = rig
        .library
        .read(crate::import_store::catalog_recordings)
        .unwrap();
    assert!(!recordings.is_empty());
    for recording in &recordings {
        assert!(!recording.appearances.is_empty(), "{recording:?}");
        for appearance in &recording.appearances {
            assert_ne!(appearance.appearance, song, "{recording:?}");
        }
    }
}

#[test]
fn a_scanned_song_is_found_with_either_spelling_of_a_folded_letter() {
    let mut rig = crate::scan_tests::Rig::new();
    rig.put("Quæł Søl.wav", "a.wav");
    rig.scan();
    for typed in ["Quæł", "quael", "søl", "sol"] {
        let view = rig
            .library
            .list_open(
                ListKind::Songs,
                &Query {
                    text: Some(typed.into()),
                    ..Query::default()
                },
            )
            .unwrap();
        assert_eq!(view.ids.len(), 1, "{typed}");
        let songs = rig.library.search(typed, 10).unwrap().songs;
        assert_eq!(songs.len(), 1, "{typed}");
        assert_eq!(songs[0].song.title.as_deref(), Some("Quæł Søl"));
    }
}

#[test]
fn indexed_search_files_a_song_by_its_display_title() {
    let lib = library(5);
    lib.store.write(WriteRequest { method: "test.write", command: None, client: None }, |write| {
        let db = write.connection();
        let mut statement = db.prepare("SELECT min(id) FROM track_appearance GROUP BY recording_id ORDER BY recording_id LIMIT 2")?;
        let ids = statement.query_map([], |row| row.get::<_, i64>(0))?.collect::<Result<Vec<_>, _>>()?;
        for (key, title) in ids.into_iter().zip(["Å Harbor Frost", "The Harbor Dusk"]) {
            db.execute("UPDATE track_appearance SET display_title = ?1 WHERE id = ?2", (title, key))?;
            db.execute("UPDATE search_text SET title = ?1 WHERE rowid = ?2", (fold::fold(title), key))?;
        }
        Ok(String::new())
    }).unwrap();
    let songs = lib.store.search("harbor", 10).unwrap().songs;
    assert_eq!(
        songs
            .iter()
            .filter_map(|hit| hit.song.title.as_deref())
            .collect::<Vec<_>>(),
        ["Å Harbor Frost", "The Harbor Dusk"]
    );
}

#[test]
fn a_nul_in_typed_text_is_ignored_by_both_song_searches() {
    let mut rig = crate::scan_tests::Rig::new();
    rig.put("Tin Field.wav", "a.wav");
    rig.scan();
    let typed = "Ti\0n";
    let view = rig
        .library
        .list_open(
            ListKind::Songs,
            &Query {
                text: Some(typed.into()),
                ..Query::default()
            },
        )
        .unwrap();
    assert_eq!(view.ids.len(), 1);
    assert_eq!(rig.library.search(typed, 10).unwrap().songs.len(), 1);
}

#[test]
fn loose_songs_after_the_last_album_do_not_hide_it_from_type_to_find() {
    let mut rig = crate::scan_tests::Rig::new();
    rig.put("A/1.flac", "a.flac");
    rig.put("Loose/Tin Field.wav", "a.wav");
    rig.scan();
    let mut view = rig
        .library
        .list_open(ListKind::Songs, &query(SortKey::Album))
        .unwrap();
    assert_eq!(view.ids.len(), 2);
    assert_eq!(
        rig.library.list_locate_prefix(&view, "Moss").unwrap(),
        Some(0)
    );
    assert_eq!(rig.library.list_locate_prefix(&view, "Zinc").unwrap(), None);
    // A view also retains the ID of a row that has since been deleted.
    view.ids.insert(1, i64::MAX);
    assert_eq!(
        rig.library.list_locate_prefix(&view, "Moss").unwrap(),
        Some(0)
    );
}

// ------------------------------------------------- groupings, filters, plays

/// A query in `sort`, grouped as `group` says.
fn grouped(sort: SortKey, group: GroupBy) -> Query {
    Query {
        group_by: Some(group),
        ..query(sort)
    }
}

/// A query in `sort` of the rows `predicate` holds for.
fn filtered(sort: SortKey, predicate: Predicate) -> Query {
    Query {
        filter: Some(Filter {
            version: FILTER_VERSION,
            predicate,
        }),
        ..query(sort)
    }
}

fn album(row: &Row) -> &ritornel_contract::AlbumTile {
    match row {
        Row::Album(album) => album,
        other => panic!("{other:?}"),
    }
}

fn song(row: &Row) -> &ritornel_contract::SongRow {
    match row {
        Row::Song(song) => song,
        other => panic!("{other:?}"),
    }
}

/// The rows of each section of a view, with the section's label: the
/// sections must begin at the first row, rise, and stay within the view.
fn by_section(resolved: &View, rows: &[Row]) -> Vec<(String, Vec<Row>)> {
    assert_eq!(
        resolved.sections.first().map(|section| section.index),
        Some(0)
    );
    let ends = resolved
        .sections
        .iter()
        .skip(1)
        .map(|section| section.index)
        .chain([rows.len() as u64]);
    resolved
        .sections
        .iter()
        .zip(ends)
        .map(|(section, end)| {
            assert!(section.index < end, "{section:?} ends at {end}");
            (
                section.label.clone(),
                rows[section.index as usize..end as usize].to_vec(),
            )
        })
        .collect()
}

fn decade_of(year: Option<i64>) -> String {
    year.map_or_else(
        || "Unknown".to_owned(),
        |year| format!("{}s", year / 10 * 10),
    )
}

fn sorted(mut ids: Vec<i64>) -> Vec<i64> {
    ids.sort_unstable();
    ids
}

/// A library with a history: some of its songs played, most of them more
/// than once, and some never.
fn played(albums: usize) -> Library {
    let library = library_drawing(albums);
    let plan = PlaysPlan {
        plays: 600,
        seed: 3,
        now_ms: NOW,
        years: 4,
        zone: "Europe/London".to_owned(),
    };
    invent_plays(&library.store, &plan, |_, _| {}).unwrap();
    library
}

#[test]
fn a_grouping_named_orders_by_group_then_by_the_sort_with_exact_sections() {
    let library = library(150);
    let natural = open(&library, ListKind::Albums, &query(SortKey::Title));

    // By decade, newest first, and by title within each.
    let decades = open(
        &library,
        ListKind::Albums,
        &grouped(SortKey::Title, GroupBy::Decade),
    );
    assert_eq!(sorted(decades.ids.clone()), sorted(natural.ids.clone()));
    let rows_of = rows(&library, &decades);
    let sections = by_section(&decades, &rows_of);
    assert!(sections.len() > 2, "{:?}", decades.sections);
    let labels: Vec<&str> = sections.iter().map(|(label, _)| label.as_str()).collect();
    let mut newest_first = labels.clone();
    newest_first.sort_by(|a, b| b.cmp(a));
    assert_eq!(labels, newest_first);
    for (label, rows) in &sections {
        let keys: Vec<String> = rows
            .iter()
            .map(|row| {
                assert_eq!(&decade_of(album(row).year), label);
                fold::sort_text(album(row).title.as_deref().unwrap_or_default(), None)
            })
            .collect();
        let mut in_order = keys.clone();
        in_order.sort_by(|a, b| super::open::by_name_for_tests(a, b));
        assert_eq!(keys, in_order, "{label}");
    }

    // By letter in an order that is not by name: the letter is the
    // artist's, A to Z, and each letter starts the years again.
    let letters = open(
        &library,
        ListKind::Albums,
        &grouped(SortKey::Year, GroupBy::Letter),
    );
    let rows_of = rows(&library, &letters);
    let sections = by_section(&letters, &rows_of);
    let labels: Vec<String> = sections.iter().map(|(label, _)| label.clone()).collect();
    let mut a_to_z = labels.clone();
    a_to_z.sort_by_key(|letter| (letter == "#", letter.clone()));
    assert_eq!(labels, a_to_z);
    assert!(labels.len() > 5, "{labels:?}");
    for (label, rows) in &sections {
        let years: Vec<i64> = rows.iter().filter_map(|row| album(row).year).collect();
        assert!(years.windows(2).all(|pair| pair[0] >= pair[1]), "{label}");
    }
    // They are the letters an order by artist has, each as long.
    let by_artist = open(&library, ListKind::Albums, &query(SortKey::Artist));
    assert_eq!(letters.sections, by_artist.sections);
    let (first, second) = (letters.sections[0].index, letters.sections[1].index);
    assert_eq!(
        sorted(letters.ids[first as usize..second as usize].to_vec()),
        sorted(by_artist.ids[first as usize..second as usize].to_vec())
    );
    // Letters named for an order by name are the sections it has anyway.
    let named = open(
        &library,
        ListKind::Albums,
        &grouped(SortKey::Title, GroupBy::Letter),
    );
    assert_eq!(named.ids, natural.ids);
    assert_eq!(named.sections, natural.sections);
}

#[test]
fn a_grouping_of_none_keeps_the_order_and_has_no_sections() {
    let library = library(80);
    for (kind, sort) in [
        (ListKind::Artists, SortKey::Name),
        (ListKind::Albums, SortKey::Year),
        (ListKind::Albums, SortKey::Added),
        (ListKind::Songs, SortKey::Album),
    ] {
        let natural = open(&library, kind, &query(sort));
        assert!(!natural.sections.is_empty(), "{kind:?} {sort:?}");
        let flat = open(&library, kind, &grouped(sort, GroupBy::None));
        assert_eq!(flat.ids, natural.ids, "{kind:?} {sort:?}");
        assert!(flat.sections.is_empty(), "{kind:?} {sort:?}");
    }
}

#[test]
fn records_group_by_year_shelf_quality_and_when_they_came_in() {
    let library = library(150);
    let total = library.invented.groups;

    let years = open(
        &library,
        ListKind::Albums,
        &grouped(SortKey::Artist, GroupBy::Year),
    );
    let rows_of = rows(&library, &years);
    for (label, rows) in by_section(&years, &rows_of) {
        for row in &rows {
            assert_eq!(album(row).year.unwrap().to_string(), label);
        }
    }

    // Every shelf but the one an artist's page alone has, in the order a
    // discography files them, each saying which it is.
    let shelves = open(
        &library,
        ListKind::Albums,
        &grouped(SortKey::Artist, GroupBy::Shelf),
    );
    assert_eq!(shelves.ids.len(), total);
    let filed: Vec<_> = shelves.sections.iter().map(|s| s.shelf.unwrap()).collect();
    assert!(filed.windows(2).all(|pair| pair[0] < pair[1]), "{filed:?}");
    assert!(filed.len() > 1, "{filed:?}");
    assert!(!filed.contains(&ritornel_contract::Shelf::AppearsOn));
    for section in &shelves.sections {
        assert_eq!(section.label, section.shelf.unwrap().label());
    }

    // Quality is the tile's own best copy, best first.
    let quality = open(
        &library,
        ListKind::Albums,
        &grouped(SortKey::Artist, GroupBy::Quality),
    );
    let rows_of = rows(&library, &quality);
    let sections = by_section(&quality, &rows_of);
    let order = ["Hi-Res Lossless", "Lossless", "Lossy", "Unknown"];
    let places: Vec<usize> = sections
        .iter()
        .map(|(label, _)| order.iter().position(|known| known == label).unwrap())
        .collect();
    assert!(
        places.windows(2).all(|pair| pair[0] < pair[1]),
        "{places:?}"
    );
    assert!(sections.len() >= 3, "{places:?}");
    for (label, rows) in &sections {
        for row in rows {
            let best = &album(row).quality.as_ref().unwrap().best;
            let said = match (best.lossless, best.bit_depth, best.sample_rate_hz) {
                (true, Some(bits), rate) if bits > 16 || rate > 48_000 => "Hi-Res Lossless",
                (true, ..) => "Lossless",
                (false, ..) => "Lossy",
            };
            assert_eq!(said, label, "{best:?}");
        }
    }

    // When they came in: the sections an order by it has, whatever the
    // order within them.
    let natural = open(&library, ListKind::Albums, &query(SortKey::Added));
    let added = open(
        &library,
        ListKind::Albums,
        &grouped(SortKey::Title, GroupBy::Added),
    );
    assert_eq!(added.sections, natural.sections);
    assert_ne!(added.ids, natural.ids);
    let (first, second) = (added.sections[0].index, added.sections[1].index);
    assert_eq!(
        sorted(added.ids[first as usize..second as usize].to_vec()),
        sorted(natural.ids[first as usize..second as usize].to_vec())
    );
}

#[test]
fn songs_group_by_decade_and_quality() {
    let library = library(40);
    let natural = open(&library, ListKind::Songs, &Query::default());
    let decades = open(
        &library,
        ListKind::Songs,
        &grouped(SortKey::Title, GroupBy::Decade),
    );
    assert_eq!(sorted(decades.ids.clone()), sorted(natural.ids.clone()));
    let rows_of = rows(&library, &decades);
    for (label, rows) in by_section(&decades, &rows_of) {
        for row in &rows {
            assert_eq!(decade_of(song(row).year), label);
        }
    }
    let quality = open(
        &library,
        ListKind::Songs,
        &grouped(SortKey::Album, GroupBy::Quality),
    );
    let rows_of = rows(&library, &quality);
    for (label, rows) in by_section(&quality, &rows_of) {
        for row in &rows {
            let lossless = song(row).copies.iter().any(|copy| copy.format.lossless);
            assert_eq!(lossless, label != "Lossy", "{label}");
        }
    }
}

#[test]
fn a_grouping_or_a_bound_a_list_does_not_take_is_refused() {
    let library = library(5);
    let refused = |kind, query: &Query| {
        let refused = library.store.list_open(kind, query);
        assert!(
            matches!(refused, Err(super::ListError::Invalid(_))),
            "{refused:?}"
        );
    };
    refused(ListKind::Artists, &grouped(SortKey::Name, GroupBy::Year));
    refused(ListKind::Songs, &grouped(SortKey::Title, GroupBy::Shelf));
    refused(ListKind::Albums, &query(SortKey::Rating));
    refused(
        ListKind::Songs,
        &filtered(SortKey::Title, Predicate::MinTracks { count: 3 }),
    );
    refused(
        ListKind::Albums,
        &filtered(SortKey::Title, Predicate::MinRating { rating: 80 }),
    );
    refused(
        ListKind::Songs,
        &filtered(SortKey::Title, Predicate::MinRating { rating: 101 }),
    );
    refused(
        ListKind::Songs,
        &filtered(
            SortKey::Title,
            Predicate::All {
                of: vec![
                    Predicate::Lossless { lossless: true },
                    Predicate::Lossless { lossless: false },
                ],
            },
        ),
    );
}

#[test]
fn records_are_filtered_by_tracks_years_and_whether_they_are_lossless() {
    let library = library(150);
    let all = open(&library, ListKind::Albums, &Query::default());
    let every = rows(&library, &all);
    let ids_where = |keep: &dyn Fn(&ritornel_contract::AlbumTile) -> bool| -> Vec<i64> {
        all.ids
            .iter()
            .zip(&every)
            .filter(|(_, row)| keep(album(row)))
            .map(|(id, _)| *id)
            .collect()
    };
    let open_where = |predicate| {
        open(
            &library,
            ListKind::Albums,
            &filtered(SortKey::Artist, predicate),
        )
    };

    let full = open_where(Predicate::MinTracks { count: 3 });
    assert_eq!(full.ids, ids_where(&|album| album.track_count >= 3));
    assert!(!full.ids.is_empty() && full.ids.len() < all.ids.len());

    let nineties = open_where(Predicate::YearRange {
        from: Some(1990),
        to: Some(1999),
    });
    assert_eq!(
        nineties.ids,
        ids_where(&|album| album.year.is_some_and(|year| (1990..=1999).contains(&year)))
    );
    assert!(!nineties.ids.is_empty());
    let since = open_where(Predicate::YearRange {
        from: Some(2000),
        to: None,
    });
    assert_eq!(
        since.ids,
        ids_where(&|album| album.year.is_some_and(|year| year >= 2000))
    );

    let lossless = open_where(Predicate::Lossless { lossless: true });
    let lossy = open_where(Predicate::Lossless { lossless: false });
    let is_lossless = |album: &ritornel_contract::AlbumTile| {
        album
            .quality
            .as_ref()
            .is_some_and(|quality| quality.best.lossless)
    };
    assert_eq!(lossless.ids, ids_where(&is_lossless));
    assert_eq!(lossy.ids, ids_where(&|album| !is_lossless(album)));
    assert!(!lossless.ids.is_empty() && !lossy.ids.is_empty());

    // Every condition of several holds, and two bounds of a kind are both
    // kept.
    let both = open_where(Predicate::All {
        of: vec![
            Predicate::MinTracks { count: 2 },
            Predicate::MinTracks { count: 3 },
            Predicate::YearRange {
                from: Some(1980),
                to: None,
            },
            Predicate::YearRange {
                from: Some(1990),
                to: Some(1999),
            },
            Predicate::Lossless { lossless: true },
        ],
    });
    assert_eq!(
        both.ids,
        ids_where(&|album| album.track_count >= 3
            && album.year.is_some_and(|year| (1990..=1999).contains(&year))
            && is_lossless(album))
    );
    assert!(!both.ids.is_empty());
    assert!(!both.by_plays && !both.by_ratings);
}

#[test]
fn songs_are_filtered_and_ordered_by_plays_and_by_when_they_were_last_played() {
    let library = played(60);
    let all = open(&library, ListKind::Songs, &Query::default());
    assert!(!all.by_plays);

    let by_plays = open(&library, ListKind::Songs, &query(SortKey::Plays));
    assert!(by_plays.by_plays);
    assert_eq!(sorted(by_plays.ids.clone()), sorted(all.ids.clone()));
    assert!(by_plays.sections.is_empty());
    let rows_of = rows(&library, &by_plays);
    let counts: Vec<u64> = rows_of.iter().map(|row| song(row).play_count).collect();
    assert!(counts.windows(2).all(|pair| pair[0] >= pair[1]));
    assert!(counts[0] > 1 && counts.last() == Some(&0), "{counts:?}");

    let never = filtered(
        SortKey::Title,
        Predicate::PlayCount {
            min: None,
            max: Some(0),
        },
    );
    let unplayed = open(&library, ListKind::Songs, &never);
    let heard = filtered(
        SortKey::Title,
        Predicate::PlayCount {
            min: Some(1),
            max: None,
        },
    );
    let heard = open(&library, ListKind::Songs, &heard);
    assert!(unplayed.by_plays && heard.by_plays);
    assert!(
        rows(&library, &unplayed)
            .iter()
            .all(|row| song(row).play_count == 0)
    );
    assert!(
        rows(&library, &heard)
            .iter()
            .all(|row| song(row).play_count >= 1)
    );
    assert_eq!(unplayed.ids.len() + heard.ids.len(), all.ids.len());
    assert!(!unplayed.ids.is_empty() && !heard.ids.is_empty());
    let twice = Predicate::PlayCount {
        min: Some(2),
        max: Some(3),
    };
    let twice = open(&library, ListKind::Songs, &filtered(SortKey::Plays, twice));
    assert!(
        rows(&library, &twice)
            .iter()
            .all(|row| (2..=3).contains(&song(row).play_count))
    );

    // Last played: newest first, with a heading for each stretch of time
    // and what was never played under its own, last.
    let recent = open(&library, ListKind::Songs, &query(SortKey::LastPlayed));
    let rows_of = rows(&library, &recent);
    let last: Vec<Option<i64>> = rows_of.iter().map(|row| song(row).last_played_at).collect();
    let dated = last.iter().take_while(|at| at.is_some()).count();
    assert_eq!(dated, heard.ids.len());
    assert!(last[..dated].windows(2).all(|pair| pair[0] >= pair[1]));
    assert!(last[dated..].iter().all(Option::is_none));
    let never = recent.sections.last().unwrap();
    assert_eq!(
        (never.label.as_str(), never.index),
        ("Never played", dated as u64)
    );
    assert!(recent.sections.len() > 2, "{:?}", recent.sections);
    // Its sections are found by their headings.
    assert_eq!(recent.find, Find::Sections { names: false });
    assert_eq!(
        library.store.list_locate_prefix(&recent, "never").unwrap(),
        Some(dated as u64)
    );
}

#[test]
fn records_and_artists_are_ordered_and_filtered_by_the_plays_of_their_songs() {
    let library = played(60);
    for kind in [ListKind::Albums, ListKind::Artists] {
        let all = open(&library, kind, &Query::default());
        let by_plays = open(&library, kind, &query(SortKey::Plays));
        assert_eq!(sorted(by_plays.ids.clone()), sorted(all.ids.clone()));
        let unplayed = filtered(
            all.sort,
            Predicate::PlayCount {
                min: None,
                max: Some(0),
            },
        );
        let unplayed = open(&library, kind, &unplayed);
        assert!(!unplayed.ids.is_empty() && unplayed.ids.len() < all.ids.len());
        // What was never played comes last by plays, in the kind's own
        // order, and last by when it was played.
        let tail = by_plays.ids.len() - unplayed.ids.len();
        assert_eq!(by_plays.ids[tail..], unplayed.ids, "{kind:?}");
        let recent = open(&library, kind, &query(SortKey::LastPlayed));
        assert_eq!(recent.ids[tail..], unplayed.ids, "{kind:?}");
        assert_eq!(
            recent.sections.last().map(|section| section.index),
            Some(tail as u64)
        );
    }
    // A record's plays are those of the songs on it: the most played
    // record holds a song that has been played.
    let by_plays = open(&library, ListKind::Albums, &query(SortKey::Plays));
    let Some(Row::Album(most)) = rows(&library, &by_plays).into_iter().next() else {
        panic!("a record")
    };
    let songs = open(&library, ListKind::Songs, &query(SortKey::Plays));
    let most_played = rows(&library, &songs);
    assert!(
        most_played
            .iter()
            .take_while(|row| song(row).play_count > 0)
            .any(|row| song(row).album_title == most.title),
        "{most:?}"
    );
}

#[test]
fn songs_are_filtered_and_ordered_by_their_stars() {
    let library = library(20);
    let all = open(&library, ListKind::Songs, &Query::default());
    let every = rows(&library, &all);
    for (key, (stars, row)) in [100, 80, 60].into_iter().zip(&every).enumerate() {
        let key = u8::try_from(key).unwrap();
        let request = WriteRequest {
            method: "ratings.set",
            command: Some(crate::Command {
                id: crate::Id::from_bytes([key; 16]),
                fingerprint: crate::Fingerprint::of("ratings.set", &[key]),
            }),
            client: None,
        };
        library
            .store
            .set_rating(request, song(row).recording_id, Some(stars), |_, _| {
                String::new()
            })
            .unwrap();
    }
    let rated = |song: &ritornel_contract::SongRow| song.rating;
    let by_rating = open(&library, ListKind::Songs, &query(SortKey::Rating));
    assert!(by_rating.by_ratings && !by_rating.by_plays);
    assert_eq!(by_rating.ids.len(), all.ids.len());
    let stars: Vec<Option<u32>> = rows(&library, &by_rating)
        .iter()
        .map(|row| rated(song(row)))
        .collect();
    let given = stars.iter().take_while(|stars| stars.is_some()).count();
    assert!(given >= 3, "{stars:?}");
    assert!(stars[..given].windows(2).all(|pair| pair[0] >= pair[1]));
    assert_eq!(stars[0], Some(100));
    assert!(stars[given..].iter().all(Option::is_none));

    let four = filtered(SortKey::Title, Predicate::MinRating { rating: 80 });
    let four = open(&library, ListKind::Songs, &four);
    assert!(four.by_ratings);
    let kept = rows(&library, &four);
    assert!(
        kept.len() >= 2 && kept.len() < given.max(3) + 1,
        "{}",
        kept.len()
    );
    assert!(kept.iter().all(|row| rated(song(row)) >= Some(80)));
    assert!(!all.by_ratings);
}

#[test]
fn a_prefix_typed_is_found_by_name_in_each_group_or_by_a_groups_label() {
    let library = library(150);
    let locate = |view: &View, typed: &str| library.store.list_locate_prefix(view, typed).unwrap();
    let key = |row: &Row| fold::sort_text(album(row).title.as_deref().unwrap_or_default(), None);

    // Flat, an order by name is found in as it is with its letters.
    let natural = open(&library, ListKind::Albums, &query(SortKey::Title));
    let flat = open(
        &library,
        ListKind::Albums,
        &grouped(SortKey::Title, GroupBy::None),
    );
    assert_eq!((natural.find, flat.find), (Find::Name, Find::Name));
    let rows_of = rows(&library, &natural);
    let typed: String = key(&rows_of[40]).chars().take(4).collect();
    assert_eq!(locate(&flat, &typed), locate(&natural, &typed));
    assert!(locate(&flat, &typed).is_some());

    // Grouped, each group starts the order again: the row found is the
    // first, in the view's order, whose title begins with what was typed,
    // though titles before it in the alphabet come after it.
    let decades = open(
        &library,
        ListKind::Albums,
        &grouped(SortKey::Title, GroupBy::Decade),
    );
    assert_eq!(decades.find, Find::Sections { names: true });
    let rows_of = rows(&library, &decades);
    let second = decades.sections[1].index as usize;
    for at in [second + 2, rows_of.len() - 1] {
        let typed: String = key(&rows_of[at]).chars().take(5).collect();
        let first = rows_of
            .iter()
            .position(|row| key(row).starts_with(&typed))
            .unwrap();
        assert_eq!(locate(&decades, &typed), Some(first as u64), "{typed}");
    }
    // A name no row begins with is not the row after where it would be,
    // which is only some other name in the first group. A label is found.
    assert_eq!(locate(&decades, "zzzzzz"), None);
    let last = decades.sections.last().unwrap();
    assert_eq!(locate(&decades, &last.label), Some(last.index));
    assert_eq!(locate(&decades, &last.label[..3]), Some(last.index));
    assert_eq!(locate(&decades, "  "), None);

    // Where the order within a group is not by name, only labels are
    // found: a letter typed is its section.
    let letters = open(
        &library,
        ListKind::Albums,
        &grouped(SortKey::Year, GroupBy::Letter),
    );
    assert_eq!(letters.find, Find::Sections { names: false });
    let third = &letters.sections[2];
    assert_eq!(
        locate(&letters, &third.label.to_lowercase()),
        Some(third.index)
    );
    let added = open(&library, ListKind::Albums, &query(SortKey::Added));
    assert_eq!(
        locate(&added, &added.sections[1].label),
        Some(added.sections[1].index)
    );

    // And a flat order that is not by name is found in by nothing.
    let flat = open(
        &library,
        ListKind::Albums,
        &grouped(SortKey::Year, GroupBy::None),
    );
    assert_eq!(flat.find, Find::Nothing);
    assert_eq!(locate(&flat, &typed), None);
    assert_eq!(locate(&flat, "19"), None);
}
