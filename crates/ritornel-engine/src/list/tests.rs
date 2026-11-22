//! Views over an invented library: membership, order, sections, rows,
//! search, locating and rows that have gone.

use ritornel_contract::{FILTER_VERSION, Filter, ListKind, Predicate, Query, Row, Sort, SortKey};

use super::fold;
use crate::invent::testing::{Library, library};
use crate::{View, WriteRequest};

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
