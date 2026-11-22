use ritornel_contract::{AlbumRef, Phrase};

use super::{Facts, Today, sentence};
use crate::Id;
use crate::invent::testing::{NOW, library_drawing, tamper};
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
            assert_eq!(Some(peak.year), record.peak_year);
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

#[test]
fn the_first_and_last_months_are_those_of_the_first_and_last_plays_dated_or_not() {
    let since = Some(NOW - 6_000 * DAY);
    // A dated play in 2023, and one five days ago whose source gave no
    // zone, so it has an instant and no local date.
    let mut recent = Facts::default();
    play(&mut recent, 1_250, 20230619);
    recent.add(Some(NOW - 5 * DAY), None, None, TODAY);
    assert_eq!(
        said(&sentence(&recent, since, TODAY)),
        "With you since *June 2023*. *1 play* in the last year."
    );

    // Over two years ago, after the last dated play: the month of its
    // instant in UTC.
    let mut gone = Facts::default();
    play(&mut gone, 3_000, 20180610);
    gone.add(Some(NOW - 1_000 * DAY), None, None, TODAY);
    assert_eq!(
        said(&sentence(&gone, since, TODAY)),
        "With you since *June 2018*. Not played since *February 2024*."
    );

    // Most of all in one year, unless the undated plays might outdo it.
    let mut mostly = Facts::default();
    for _ in 0..8 {
        play(&mut mostly, 2_000, 20210504);
    }
    for _ in 0..4 {
        play(&mut mostly, 400, 20251011);
    }
    assert!(said(&sentence(&mostly, since, TODAY)).ends_with("most of all in *2021*."));
    for _ in 0..8 {
        mostly.add(Some(NOW - 3_000 * DAY), None, None, TODAY);
    }
    assert!(!said(&sentence(&mostly, since, TODAY)).contains("most of all"));

    // Between one and two years ago: so it is here.
    let mut quiet = Facts::default();
    play(&mut quiet, 1_000, 20240219);
    quiet.add(Some(NOW - 500 * DAY), None, None, TODAY);
    assert_eq!(
        said(&sentence(&quiet, since, TODAY)),
        "With you since *February 2024*. Last played in *July 2025*."
    );

    // An undated play years before the first dated one: its month, not
    // the dated one's.
    let mut before = Facts::default();
    play(&mut before, 900, 20240530);
    before.add(Some(NOW - 2_000 * DAY), None, None, TODAY);
    assert_eq!(
        said(&sentence(&before, since, TODAY)),
        "With you since *May 2021*. Not played since *May 2024*."
    );

    // No play with a local date at all.
    let mut none = Facts::default();
    none.add(Some(NOW - 500 * DAY), None, None, TODAY);
    none.add(Some(NOW - 2_000 * DAY), None, None, TODAY);
    assert_eq!(
        said(&sentence(&none, since, TODAY)),
        "With you since *May 2021*. Last played in *July 2025*."
    );
}

#[test]
fn a_months_day_with_no_local_date_is_its_day_in_utc() {
    // 23:30 UTC on 31 January 2024.
    let at = 1_706_743_800_000;
    assert_eq!(super::month_of((at, None)), "January 2024");
    // Heard where it was already February, the local date says so.
    assert_eq!(super::month_of((at, Some(20240201))), "February 2024");
}

#[test]
fn of_plays_at_the_same_instant_one_with_a_local_date_is_first_and_last() {
    let at = 1_706_743_800_000;
    for dated_first in [true, false] {
        let mut facts = Facts::default();
        let mut add = |date| facts.add(Some(at), date, None, TODAY);
        if dated_first {
            add(Some(20240201));
            add(None);
        } else {
            add(None);
            add(Some(20240201));
        }
        let played = (at, Some(20240201));
        assert_eq!((facts.first, facts.last), (Some(played), Some(played)));
    }
}

#[test]
fn a_records_plays_count_its_undated_ones_and_only_dated_ones_have_a_year() {
    let mut facts = Facts::default();
    for _ in 0..3 {
        facts.add(Some(NOW - 5 * DAY), None, Some(7), TODAY);
    }
    facts.add(Some(NOW - 900 * DAY), Some(20240530), Some(7), TODAY);
    facts.add(None, None, Some(8), TODAY);
    let seven = &facts.records[&7];
    assert_eq!(seven.plays, 4);
    assert_eq!(seven.years, [(2024, 1)].into());
    let eight = &facts.records[&8];
    assert_eq!((eight.plays, eight.years.len()), (1, 0));
}

/// An invented library with `plays` plays over the three years before
/// [`NOW`], its listener, and the artist played most.
fn played(plays: usize) -> (crate::invent::testing::Library, Id, Id) {
    let invented = library_drawing(40);
    let plan = PlaysPlan {
        plays,
        seed: 3,
        now_ms: NOW,
        years: 3,
        zone: "Europe/London".to_owned(),
    };
    invent_plays(&invented.store, &plan, |_, _| {}).unwrap();
    let listener = invented.store.default_listener().unwrap().unwrap();
    let artist = favourite(&invented.store);
    (invented, listener, artist)
}

/// An ID as an SQL blob literal.
fn blob(id: Id) -> String {
    let hex: String = id
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("x'{hex}'")
}

/// The counted plays of the artist whose public ID is `artist`, as SQL.
fn plays_of(artist: Id) -> String {
    let artist = blob(artist);
    format!(
        "SELECT pe.id FROM play_event pe
         JOIN recording r ON r.id = pe.recording_id
         JOIN artist_credit_name acn ON acn.artist_credit_id = r.display_artist_credit_id
         WHERE acn.artist_id = (SELECT id FROM artist WHERE public_id = {artist})
           AND pe.counts = 1 AND pe.excluded = 0"
    )
}

#[test]
fn the_span_holds_every_local_year_played_across_the_date_line() {
    let (invented, listener, artist) = played(600);
    let store = &invented.store;
    let of_artist = plays_of(artist);
    // 00:30 on New Year's Day 2020 in Kiritimati (UTC+14) is 10:30 UTC on
    // New Year's Eve, before 23:00 on New Year's Eve in Honolulu (UTC−10),
    // which is 09:00 UTC on New Year's Day: the first play in time is in
    // 2020, and the next, by local date, in 2019. And 00:30 on New Year's
    // Day 2027 in Kiritimati is still 2026 where the server is, and comes
    // before 01:00 on New Year's Eve in Honolulu. A play stamped a year
    // ahead, by a clock that was wrong, is not heard yet.
    tamper(
        store,
        &format!(
            "UPDATE play_event SET event_at_utc_ms = 1577788200000, local_date = 20200101
             WHERE id = ({of_artist} ORDER BY pe.id LIMIT 1);
             UPDATE play_event SET event_at_utc_ms = 1577869200000, local_date = 20191231
             WHERE id = ({of_artist} ORDER BY pe.id LIMIT 1 OFFSET 1);
             UPDATE play_event SET event_at_utc_ms = 1798713000000, local_date = 20270101
             WHERE id = ({of_artist} ORDER BY pe.id DESC LIMIT 1);
             UPDATE play_event SET event_at_utc_ms = 1798714800000, local_date = 20261231
             WHERE id = ({of_artist} ORDER BY pe.id DESC LIMIT 1 OFFSET 2);
             UPDATE play_event SET event_at_utc_ms = 1830254400000, local_date = 20280101
             WHERE id = ({of_artist} ORDER BY pe.id DESC LIMIT 1 OFFSET 1);"
        ),
    );
    // Noon UTC on New Year's Eve 2026.
    let today = Today {
        now_ms: 1_798_718_400_000,
        year: 2026,
        month: 12,
        date: 20261231,
    };
    let history = store
        .artist_history(listener, artist, today)
        .unwrap()
        .unwrap();
    let years: Vec<i32> = history.years.iter().map(|year| year.year).collect();
    assert_eq!((years.first(), years.last()), (Some(&2019), Some(&2028)));
    let span = history.span.unwrap();
    assert_eq!((span.from, span.to), (2019, 2027));
}

#[test]
fn a_first_or_last_play_with_no_local_date_has_no_day_but_keeps_its_month() {
    let (invented, listener, artist) = played(600);
    let store = &invented.store;
    let before = store
        .artist_history(listener, artist, TODAY)
        .unwrap()
        .unwrap();
    assert!(before.first_played.is_some() && before.last_played.is_some());
    tamper(
        store,
        &format!(
            "UPDATE play_event SET local_date = NULL
             WHERE id = ({} ORDER BY pe.event_at_utc_ms DESC LIMIT 1)",
            plays_of(artist)
        ),
    );
    let after = store
        .artist_history(listener, artist, TODAY)
        .unwrap()
        .unwrap();
    assert_eq!(
        (&after.first_played, &after.last_played, &after.sentence),
        (&before.first_played, &None, &before.sentence)
    );
    tamper(
        store,
        &format!(
            "UPDATE play_event SET local_date = NULL
             WHERE id = ({} ORDER BY pe.event_at_utc_ms LIMIT 1)",
            plays_of(artist)
        ),
    );
    let after = store
        .artist_history(listener, artist, TODAY)
        .unwrap()
        .unwrap();
    assert_eq!(
        (&after.first_played, &after.last_played, &after.sentence),
        (&None, &None, &before.sentence)
    );

    // With no local date anywhere, the chart has no years, but where the
    // listener's history begins is still known: with the artist.
    tamper(
        store,
        &format!(
            "UPDATE play_event SET event_at_utc_ms =
               (SELECT min(event_at_utc_ms) FROM play_event) - {DAY}
             WHERE id = ({} ORDER BY pe.event_at_utc_ms LIMIT 1)",
            plays_of(artist)
        ),
    );
    let before = store
        .artist_history(listener, artist, TODAY)
        .unwrap()
        .unwrap();
    assert!(said(&before.sentence).contains("your first weeks"));
    tamper(store, "UPDATE play_event SET local_date = NULL");
    let after = store
        .artist_history(listener, artist, TODAY)
        .unwrap()
        .unwrap();
    // What it says after since when needs years, which there are not now.
    assert_eq!(
        (after.span, after.years.len(), &after.sentence[..2]),
        (None, 0, &before.sentence[..2])
    );
}

#[test]
fn played_most_skips_a_record_the_albums_list_no_longer_has() {
    let (invented, listener, artist) = played(4_000);
    let store = &invented.store;
    let artist_blob = blob(artist);
    // Credit the artist with a played song from each of eight records.
    tamper(
        store,
        &format!(
            "UPDATE recording SET display_artist_credit_id = (
               SELECT acn.artist_credit_id FROM artist_credit_name acn
               JOIN artist a ON a.id = acn.artist_id WHERE a.public_id = {artist_blob} LIMIT 1)
             WHERE id IN (
               SELECT min(pe.recording_id) FROM play_event pe
               JOIN track_appearance ta ON ta.id = pe.appearance_id
               JOIN release rel ON rel.id = ta.release_id
               WHERE pe.counts = 1
               GROUP BY rel.release_group_id LIMIT 8)"
        ),
    );
    let before = store
        .artist_history(listener, artist, TODAY)
        .unwrap()
        .unwrap();
    assert_eq!(before.records.len(), 6);
    let albums: Vec<AlbumRef> = before
        .records
        .iter()
        .map(|record| record.tile.album)
        .collect();
    // Every file of the six played most is gone from the library.
    let groups: Vec<String> = albums
        .iter()
        .map(|album| match album {
            AlbumRef::ReleaseGroup { id } => blob(*id),
            AlbumRef::Release { .. } => panic!("a tile is of a record"),
        })
        .collect();
    tamper(
        store,
        &format!(
            "UPDATE source_item SET retired_at_ms = 1 WHERE id IN (
               SELECT a.source_item_id FROM assignment a
               JOIN track_appearance ta ON ta.id = a.appearance_id
               JOIN release rel ON rel.id = ta.release_id
               JOIN release_group rg ON rg.id = rel.release_group_id
               WHERE rg.public_id IN ({}))",
            groups.join(", ")
        ),
    );
    let after = store
        .artist_history(listener, artist, TODAY)
        .unwrap()
        .unwrap();
    // The records after them take their places.
    assert!(!after.records.is_empty());
    assert!(
        after
            .records
            .iter()
            .all(|record| !albums.contains(&record.tile.album) && record.plays > 0)
    );
}
