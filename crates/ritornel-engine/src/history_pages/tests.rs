use ritornel_contract::{AlbumRef, Phrase};

use super::{Facts, Today, sentence};
use crate::Id;
use crate::invent::testing::{NOW, library_drawing};
use crate::invent::{PlaysPlan, invent_plays};

const DAY: i64 = 86_400_000;
/// 15 November 2026, near enough to the invented libraries' now.
const TODAY: Today = Today {
    now_ms: NOW,
    year: 2026,
    month: 11,
    date: 20261115,
};

/// A play `days` before [`TODAY`], on the local date `date`.
fn play(facts: &mut Facts, days: i64, date: i64) {
    facts.add(Some(NOW - days * DAY), Some(date), None, TODAY);
}

/// The sentence as text, with what is set apart in asterisks.
fn said(phrases: &[Phrase]) -> String {
    phrases
        .iter()
        .map(|phrase| {
            if phrase.strong {
                format!("*{}*", phrase.text)
            } else {
                phrase.text.clone()
            }
        })
        .collect()
}

#[test]
fn nothing_is_said_of_nothing_and_little_of_plays_with_no_day() {
    assert!(sentence(&Facts::default(), Some(NOW), TODAY).is_empty());
    let mut facts = Facts::default();
    for _ in 0..3 {
        facts.add(None, None, None, TODAY);
    }
    assert_eq!(said(&sentence(&facts, None, TODAY)), "Played *3 times*.");
}

#[test]
fn a_sentence_says_since_when_and_then_the_first_thing_that_is_true() {
    // Played this past year: how much.
    let mut facts = Facts::default();
    play(&mut facts, 3_000, 20180610);
    play(&mut facts, 40, 20261006);
    play(&mut facts, 10, 20261105);
    assert_eq!(
        said(&sentence(&facts, Some(NOW - 6_000 * DAY), TODAY)),
        "With you since *June 2018*. *2 plays* in the last year."
    );

    // Found in the history's first weeks.
    assert_eq!(
        said(&sentence(&facts, Some(NOW - 3_010 * DAY), TODAY)),
        "With you since your first weeks, *June 2018*. *2 plays* in the last year."
    );

    // Not played for two years: since when.
    let mut gone = Facts::default();
    play(&mut gone, 4_000, 20151201);
    play(&mut gone, 900, 20240530);
    assert_eq!(
        said(&sentence(&gone, Some(NOW - 6_000 * DAY), TODAY)),
        "With you since *December 2015*. Not played since *May 2024*."
    );

    // Played mostly in one year that is over, and in more than one.
    let mut mostly = Facts::default();
    for _ in 0..8 {
        play(&mut mostly, 2_000, 20210504);
    }
    for _ in 0..4 {
        play(&mut mostly, 400, 20251011);
    }
    assert_eq!(
        said(&sentence(&mostly, Some(NOW - 6_000 * DAY), TODAY)),
        "With you since *May 2021*, and most of all in *2021*."
    );

    // Neither: when last.
    let mut quiet = Facts::default();
    play(&mut quiet, 1_000, 20240219);
    play(&mut quiet, 500, 20250703);
    assert_eq!(
        said(&sentence(&quiet, Some(NOW - 6_000 * DAY), TODAY)),
        "With you since *February 2024*. Last played in *July 2025*."
    );
}

#[test]
fn a_year_is_most_of_all_only_when_it_is_past_and_holds_two_in_five() {
    // Most of it this year: not said, as the year is not over.
    let mut now = Facts::default();
    for _ in 0..12 {
        play(&mut now, 30, 20261016);
    }
    play(&mut now, 800, 20240808);
    assert!(
        said(&sentence(&now, Some(NOW - 6_000 * DAY), TODAY))
            .ends_with("*12 plays* in the last year.")
    );

    // Fewer than ten plays in all: too few to have a year of them.
    let mut few = Facts::default();
    for _ in 0..6 {
        play(&mut few, 2_000, 20210504);
    }
    play(&mut few, 300, 20260119);
    assert!(said(&sentence(&few, Some(NOW - 6_000 * DAY), TODAY)).ends_with("in the last year."));

    // Spread over five years: none holds two in five.
    let mut spread = Facts::default();
    for (year, days) in [
        (2020, 2_200),
        (2021, 1_800),
        (2022, 1_400),
        (2023, 1_000),
        (2024, 700),
    ] {
        for _ in 0..3 {
            play(&mut spread, days, year * 10_000 + 501);
        }
    }
    play(&mut spread, 200, 20260429);
    assert!(
        said(&sentence(&spread, Some(NOW - 6_000 * DAY), TODAY))
            .ends_with("*1 play* in the last year.")
    );
}

#[test]
fn a_large_count_is_grouped_in_thousands() {
    let mut facts = Facts::default();
    play(&mut facts, 4_000, 20150801);
    for _ in 0..1_234 {
        play(&mut facts, 100, 20260807);
    }
    assert!(said(&sentence(&facts, Some(NOW - 6_000 * DAY), TODAY)).contains("*1,234 plays*"));
}

/// The artist with the most plays bound to their songs.
fn favourite(store: &crate::LibraryStore) -> Id {
    store
        .read(|connection| {
            Ok(connection.query_row(
                "SELECT a.public_id FROM play_event pe
                 JOIN recording r ON r.id = pe.recording_id
                 JOIN artist_credit_name acn ON acn.artist_credit_id = r.display_artist_credit_id
                 JOIN artist a ON a.id = acn.artist_id
                 WHERE pe.counts = 1
                 GROUP BY a.id ORDER BY count(*) DESC LIMIT 1",
                [],
                |row| Ok(Id::from_bytes(row.get(0)?)),
            )?)
        })
        .unwrap()
}

#[test]
fn a_page_counts_the_bound_plays_of_its_artist_and_its_album_as_counting_by_hand_does() {
    let invented = library_drawing(40);
    let store = &invented.store;
    let plan = PlaysPlan {
        plays: 4_000,
        seed: 3,
        now_ms: NOW,
        years: 12,
        zone: "Europe/London".to_owned(),
    };
    let stored = invent_plays(store, &plan, |_, _| {}).unwrap();
    assert!(stored > 3_900, "about as many as asked for: {stored}");
    // The same plan again stores nothing.
    assert_eq!(invent_plays(store, &plan, |_, _| {}).unwrap(), 0);
    let listener = store.default_listener().unwrap().unwrap();

    // About a fifth are of strangers, and reach no page.
    let (all, bound): (i64, i64) = store
        .read(|connection| {
            Ok(connection.query_row(
                "SELECT count(*), count(recording_id) FROM play_event WHERE counts = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?)
        })
        .unwrap();
    assert!(
        (0.6..0.9).contains(&(bound as f64 / all as f64)),
        "{bound} of {all} bound"
    );

    let artist = favourite(store);
    let history = store
        .artist_history(listener, artist, TODAY)
        .unwrap()
        .unwrap();
    let (by_hand, first, last): (i64, i64, i64) = store
        .read(move |connection| {
            Ok(connection.query_row(
                "SELECT count(*), min(pe.local_date), max(pe.local_date) FROM play_event pe
                 JOIN recording r ON r.id = pe.recording_id
                 JOIN artist_credit_name acn ON acn.artist_credit_id = r.display_artist_credit_id
                 WHERE acn.artist_id = (SELECT id FROM artist WHERE public_id = ?1)
                   AND pe.counts = 1 AND pe.excluded = 0",
                [artist.as_bytes()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?)
        })
        .unwrap();
    assert_eq!(history.plays, u64::try_from(by_hand).unwrap());
    assert_eq!(
        history.years.iter().map(|year| year.plays).sum::<u64>(),
        history.plays,
        "every invented play has a date"
    );
    assert_eq!(
        history
            .first_played
            .as_deref()
            .map(|day| day.replace('-', "")),
        Some(first.to_string())
    );
    assert_eq!(
        history
            .last_played
            .as_deref()
            .map(|day| day.replace('-', "")),
        Some(last.to_string())
    );
    assert!(
        history
            .years
            .windows(2)
            .all(|pair| pair[0].year < pair[1].year)
    );
    let span = history.span.unwrap();
    assert_eq!((span.from, span.to), (2014, 2026));
    assert!(said(&history.sentence).starts_with("With you since "));

    // The records under the picture: most played first, each an Albums
    // tile, with every play of them counted on its own page.
    assert!(!history.records.is_empty() && history.records.len() <= 6);
    assert!(
        history
            .records
            .windows(2)
            .all(|pair| pair[0].plays >= pair[1].plays)
    );
    for record in &history.records {
        let album = store
            .album_history(listener, record.tile.album, TODAY)
            .unwrap()
            .unwrap();
        assert!(album.plays >= record.plays, "{album:?} {record:?}");
        assert!(album.records.is_empty());
        assert_eq!(album.span, history.span);
        let peak = album
            .years
            .iter()
            .max_by_key(|year| (year.plays, -year.year))
            .unwrap();
        if album.plays == record.plays {
            assert_eq!(peak.year, record.peak_year);
        }
        // Asked for by one of its releases, an album is its whole record.
        let group = store.album_page(record.tile.album).unwrap().unwrap();
        let by_release = store
            .album_history(
                listener,
                AlbumRef::Release {
                    id: group.release_id,
                },
                TODAY,
            )
            .unwrap()
            .unwrap();
        assert_eq!(by_release, album);
    }

    // What the library does not have, it says so.
    let nobody = Id::from_bytes([0x99; 16]);
    assert_eq!(store.artist_history(listener, nobody, TODAY).unwrap(), None);
    assert_eq!(
        store
            .album_history(listener, AlbumRef::ReleaseGroup { id: nobody }, TODAY)
            .unwrap(),
        None
    );
}
