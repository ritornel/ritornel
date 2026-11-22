use ritornel_contract::{AlbumRef, Fact, FactKind};

use super::{Gathered, Heard, Record, Song, ago, facts};
use crate::history_pages::Today;
use crate::invent::testing::{NOW, library_drawing, tamper};
use crate::invent::{PlaysPlan, invent_plays};
use crate::{Id, LibraryStore};

const DAY: i64 = 86_400_000;
/// Sunday 15 November 2026, at noon in London, near enough to the invented
/// libraries' now.
const TODAY: Today = Today {
    now_ms: NOW,
    year: 2026,
    month: 11,
    date: 20261115,
};

/// A fact as text, with what is set apart in asterisks, and the line under it.
fn said(fact: &Fact) -> (String, String) {
    let head = fact
        .head
        .iter()
        .map(|phrase| {
            if phrase.strong {
                format!("*{}*", phrase.text)
            } else {
                phrase.text.clone()
            }
        })
        .collect();
    (head, fact.more.clone().unwrap_or_default())
}

fn kinds(facts: &[Fact]) -> Vec<FactKind> {
    facts.iter().map(|fact| fact.kind).collect()
}

fn of(facts: &[Fact], kind: FactKind) -> Option<(String, String)> {
    facts.iter().find(|fact| fact.kind == kind).map(said)
}

/// A play of song `song`, `days` before now, on the local date `date`, at
/// `hour`:`minute` in London's winter.
fn heard(days: i64, date: i64, song: i64, hour: i64, minute: i64) -> Heard {
    Heard {
        at_ms: Some(NOW - days * DAY),
        date: Some(date),
        song,
        hour: Some(hour),
        offset: Some(0),
    }
    .at(hour, minute)
}

impl Heard {
    /// The same play, moved to `hour`:`minute` of its day.
    fn at(mut self, hour: i64, minute: i64) -> Self {
        let at = self.at_ms.unwrap_or(NOW);
        self.at_ms = Some(at - at.rem_euclid(DAY) + (hour * 60 + minute) * 60_000);
        self
    }
}

fn record(plays: Vec<Heard>) -> Record {
    Record {
        plays,
        size: 10,
        titles: [(1, "Paper Weather".to_owned()), (2, "Low Tide".to_owned())].into(),
        top: None,
    }
}

#[test]
fn a_song_with_no_history_has_no_facts() {
    assert!(facts(&Gathered::default(), TODAY).is_empty());
    // Played once: nothing yet worth saying of the song.
    let once = Gathered {
        song: Song {
            plays: 1,
            last: Some(20240101),
            five_stars: false,
        },
        ..Gathered::default()
    };
    assert!(facts(&once, TODAY).is_empty());
}

#[test]
fn the_song_says_how_often_and_when_last() {
    let mut gathered = Gathered {
        song: Song {
            plays: 1_234,
            last: Some(20250310),
            five_stars: true,
        },
        ..Gathered::default()
    };
    assert_eq!(
        of(&facts(&gathered, TODAY), FactKind::SongPlays).unwrap(),
        (
            "You have played this song *1,234 times*.".to_owned(),
            "One of your five-star songs. The last time was in March 2025.".to_owned()
        )
    );
    gathered.song.last = Some(TODAY.date);
    gathered.song.five_stars = false;
    assert_eq!(
        of(&facts(&gathered, TODAY), FactKind::SongPlays).unwrap().1,
        "The last time was earlier today."
    );
}

#[test]
fn how_long_ago_is_said_as_a_person_would() {
    let today = TODAY.date;
    assert_eq!(ago(today, today).unwrap(), "today");
    assert_eq!(ago(20261114, today).unwrap(), "yesterday");
    assert_eq!(ago(20261003, today).unwrap(), "43 days ago");
    assert_eq!(ago(20260920, today).unwrap(), "a month ago");
    assert_eq!(ago(20260514, today).unwrap(), "6 months ago");
    assert_eq!(ago(20251115, today).unwrap(), "a year ago");
    assert_eq!(ago(20071116, today).unwrap(), "18 years ago");
    assert_eq!(ago(20071115, today).unwrap(), "19 years ago");
}

#[test]
fn a_record_says_when_it_was_found_its_year_and_its_song() {
    // 30 plays: 24 of them in 2019, at night, the rest this year.
    let mut plays = Vec::new();
    for at in 0..24 {
        plays.push(heard(2_500 - at, 20190910 + at % 3, 1 + at % 2, 1, 15));
    }
    for at in 0..6 {
        plays.push(heard(200 - at, 20260429, 1, 2, 30));
    }
    let mut record = record(plays);
    record.top = Some(("Paper Weather".to_owned(), 41));
    let gathered = Gathered {
        record: Some(record),
        // The listener mostly listens by day.
        parts: [10, 300, 400, 290],
        ..Gathered::default()
    };
    let said = facts(&gathered, TODAY);
    assert_eq!(
        kinds(&said),
        [
            FactKind::FirstHeard,
            FactKind::ItsYear,
            FactKind::TimeOfDay,
            FactKind::PlayedMost
        ]
    );
    assert_eq!(
        of(&said, FactKind::FirstHeard).unwrap(),
        (
            "You first heard this record *7 years ago*.".to_owned(),
            "10 September 2019. 30 plays since.".to_owned()
        )
    );
    assert_eq!(
        of(&said, FactKind::ItsYear).unwrap(),
        (
            "*2019* was its year.".to_owned(),
            "24 of its 30 plays came then.".to_owned()
        )
    );
    assert_eq!(
        of(&said, FactKind::TimeOfDay).unwrap(),
        (
            "Mostly a *late-night* record.".to_owned(),
            "100% of its plays.".to_owned()
        )
    );
    assert_eq!(
        of(&said, FactKind::PlayedMost).unwrap(),
        (
            "*“Paper Weather”* is the one you play most.".to_owned(),
            "41 times.".to_owned()
        )
    );
}

#[test]
fn a_time_of_day_is_said_only_where_it_stands_out() {
    // Every play in the evening, from a listener who plays mostly then.
    let plays = (0..40)
        .map(|at| heard(300 + at, 20260101, 1, 20, 0))
        .collect();
    let mut gathered = Gathered {
        record: Some(record(plays)),
        parts: [10, 10, 10, 970],
        ..Gathered::default()
    };
    assert_eq!(of(&facts(&gathered, TODAY), FactKind::TimeOfDay), None);
    gathered.parts = [100, 300, 300, 300];
    assert_eq!(
        of(&facts(&gathered, TODAY), FactKind::TimeOfDay).unwrap().0,
        "Mostly an *evening* record."
    );
    // Too few plays to say.
    gathered.record = Some(record(
        (0..29)
            .map(|at| heard(300 + at, 20260101, 1, 20, 0))
            .collect(),
    ));
    assert_eq!(of(&facts(&gathered, TODAY), FactKind::TimeOfDay), None);
}

#[test]
fn a_year_is_its_year_only_when_it_is_over_and_holds_three_plays_in_ten() {
    // All 25 this year: not yet its year.
    let now: Vec<Heard> = (0..25)
        .map(|at| heard(10 + at, 20261020, 1, 12, 0))
        .collect();
    let gathered = Gathered {
        record: Some(record(now)),
        ..Gathered::default()
    };
    assert_eq!(of(&facts(&gathered, TODAY), FactKind::ItsYear), None);
    // All 25 in 2021.
    let then: Vec<Heard> = (0..25)
        .map(|at| heard(1_800 + at, 20211020, 1, 12, 0))
        .collect();
    let gathered = Gathered {
        record: Some(record(then)),
        ..Gathered::default()
    };
    assert_eq!(
        of(&facts(&gathered, TODAY), FactKind::ItsYear).unwrap().1,
        "All 25 of its plays came then."
    );
    // Five years of four plays: none holds enough, and too few in all.
    let spread: Vec<Heard> = (0..20)
        .map(|at| {
            heard(
                400 * (at % 5) + 100,
                (2021 - at % 5) * 10_000 + 601,
                1,
                12,
                0,
            )
        })
        .collect();
    let gathered = Gathered {
        record: Some(record(spread)),
        ..Gathered::default()
    };
    assert_eq!(of(&facts(&gathered, TODAY), FactKind::ItsYear), None);
}

#[test]
fn this_day_in_an_earlier_year_says_what_was_played_and_from_when() {
    // This day in 2019 and in 2023: the later is said.
    let mut plays = vec![
        heard(2_557, 20191115, 1, 9, 0),
        heard(1_096, 20231115, 1, 21, 40),
        heard(1_096, 20231115, 1, 21, 44),
        heard(1_096, 20231115, 1, 21, 49),
    ];
    let gathered = Gathered {
        record: Some(record(plays.clone())),
        ..Gathered::default()
    };
    assert_eq!(
        of(&facts(&gathered, TODAY), FactKind::OnThisDay).unwrap(),
        (
            "You played this *on this day in 2023*.".to_owned(),
            "“Paper Weather” 3 times, from 21:40.".to_owned()
        )
    );
    // One song once.
    let once = Gathered {
        record: Some(record(vec![heard(2_557, 20191115, 2, 9, 5)])),
        ..Gathered::default()
    };
    assert_eq!(
        of(&facts(&once, TODAY), FactKind::OnThisDay).unwrap().1,
        "“Low Tide”, at 09:05."
    );
    // Two songs.
    plays.push(heard(1_096, 20231115, 2, 21, 52));
    let two = Gathered {
        record: Some(record(plays.clone())),
        ..Gathered::default()
    };
    assert_eq!(
        of(&facts(&two, TODAY), FactKind::OnThisDay).unwrap().1,
        "2 of its songs, from 21:40."
    );
    // Three of four songs is the whole album.
    plays.push(heard(1_096, 20231115, 3, 21, 58));
    let mut whole = record(plays);
    whole.size = 4;
    let whole = Gathered {
        record: Some(whole),
        ..Gathered::default()
    };
    assert_eq!(
        of(&facts(&whole, TODAY), FactKind::OnThisDay).unwrap().1,
        "The whole album, from 21:40."
    );
    // Today is not an earlier year.
    let today = Gathered {
        record: Some(record(vec![heard(0, TODAY.date, 1, 9, 0)])),
        ..Gathered::default()
    };
    assert_eq!(of(&facts(&today, TODAY), FactKind::OnThisDay), None);
}

#[test]
fn a_record_unplayed_for_a_year_before_today_says_since_when() {
    let gathered = Gathered {
        record: Some(record(vec![
            heard(900, 20240530, 1, 12, 0),
            // Earlier today: not what it had been unplayed before.
            heard(0, TODAY.date, 2, 9, 0),
        ])),
        ..Gathered::default()
    };
    assert_eq!(
        of(&facts(&gathered, TODAY), FactKind::NotSince).unwrap(),
        (
            "Before today, you had not played it *since May 2024*.".to_owned(),
            String::new()
        )
    );
    let lately = Gathered {
        record: Some(record(vec![heard(200, 20260429, 1, 12, 0)])),
        ..Gathered::default()
    };
    assert_eq!(of(&facts(&lately, TODAY), FactKind::NotSince), None);

    // Played since with no local date, so maybe before today: nothing is
    // said.
    let undated = Gathered {
        record: Some(record(vec![
            heard(900, 20240530, 1, 12, 0),
            Heard {
                at_ms: Some(NOW - 30 * DAY),
                date: None,
                song: 1,
                hour: None,
                offset: None,
            },
        ])),
        ..Gathered::default()
    };
    assert_eq!(of(&facts(&undated, TODAY), FactKind::NotSince), None);
}

/// The record played most, and an appearance on it that was played.
fn favourite(store: &LibraryStore) -> (Id, Id) {
    store
        .read(|connection| {
            Ok(connection.query_row(
                "SELECT rg.public_id, ta.public_id FROM play_event pe
                 JOIN track_appearance ta ON ta.id = pe.appearance_id
                 JOIN release rel ON rel.id = ta.release_id
                 JOIN release_group rg ON rg.id = rel.release_group_id
                 WHERE pe.counts = 1
                 GROUP BY rg.id ORDER BY count(*) DESC LIMIT 1",
                [],
                |row| Ok((Id::from_bytes(row.get(0)?), Id::from_bytes(row.get(1)?))),
            )?)
        })
        .unwrap()
}

#[test]
fn lean_back_counts_a_record_as_its_page_does() {
    let invented = library_drawing(40);
    let store = &invented.store;
    let listener = store.default_listener().unwrap().unwrap();
    // Before any plays, a song has nothing to say.
    let any: Id = store
        .read(|connection| {
            Ok(connection.query_row(
                "SELECT public_id FROM track_appearance LIMIT 1",
                [],
                |row| Ok(Id::from_bytes(row.get(0)?)),
            )?)
        })
        .unwrap();
    let quiet = store.track_history(listener, any, TODAY).unwrap().unwrap();
    assert!(quiet.facts.is_empty());

    let plan = PlaysPlan {
        plays: 4_000,
        seed: 3,
        now_ms: NOW,
        years: 12,
        zone: "Europe/London".to_owned(),
    };
    invent_plays(store, &plan, |_, _| {}).unwrap();
    let (group, appearance) = favourite(store);
    let history = store
        .track_history(listener, appearance, TODAY)
        .unwrap()
        .unwrap();
    let page = store
        .album_history(listener, AlbumRef::ReleaseGroup { id: group }, TODAY)
        .unwrap()
        .unwrap();
    // When it was first heard, and how often since, are its page's.
    let (_, more) = of(&history.facts, FactKind::FirstHeard).unwrap();
    let first: Vec<u32> = page
        .first_played
        .as_deref()
        .unwrap()
        .split('-')
        .map(|part| part.parse().unwrap())
        .collect();
    assert!(more.starts_with(&format!("{} ", first[2])), "{more}");
    assert!(more.contains(&format!(" {}. ", first[0])), "{more}");
    let plays = page.plays.to_string();
    let plays = if plays.len() > 3 {
        format!(
            "{},{}",
            &plays[..plays.len() - 3],
            &plays[plays.len() - 3..]
        )
    } else {
        plays
    };
    assert!(more.ends_with(&format!(" {plays} plays since.")), "{more}");
    // Its song played most says how often.
    let (_, most) = of(&history.facts, FactKind::PlayedMost).unwrap();
    assert!(most.ends_with(" times."), "{most}");

    let nobody = Id::from_bytes([0x99; 16]);
    assert_eq!(store.track_history(listener, nobody, TODAY).unwrap(), None);
}

#[test]
fn a_songs_last_time_is_the_day_of_its_latest_play() {
    let invented = library_drawing(40);
    let store = &invented.store;
    let listener = store.default_listener().unwrap().unwrap();
    let plan = PlaysPlan {
        plays: 4_000,
        seed: 3,
        now_ms: NOW,
        years: 12,
        zone: "Europe/London".to_owned(),
    };
    invent_plays(store, &plan, |_, _| {}).unwrap();
    let (_, appearance) = favourite(store);
    let hex: String = appearance
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let nth_play = |offset: u32| {
        format!(
            "(SELECT pe.id FROM play_event pe
              JOIN track_appearance ta ON ta.public_id = x'{hex}'
              JOIN track_appearance cta ON cta.id = ta.canonical_id
              JOIN recording r ON r.id = cta.recording_id
              WHERE pe.recording_id = r.canonical_id AND pe.counts = 1 AND pe.excluded = 0
              ORDER BY pe.id LIMIT 1 OFFSET {offset})"
        )
    };
    let last_time = || {
        let history = store
            .track_history(listener, appearance, TODAY)
            .unwrap()
            .unwrap();
        of(&history.facts, FactKind::SongPlays).unwrap().1
    };
    // Its two latest plays: at 00:30 on 16 November in Kiritimati (UTC+14),
    // and an hour and a half later at 01:59 on the 15th in Honolulu
    // (UTC−10). The later is today's, though the earlier has the later date.
    tamper(
        store,
        &format!(
            "UPDATE play_event SET event_at_utc_ms = {}, local_date = 20261116 WHERE id = {};
             UPDATE play_event SET event_at_utc_ms = {}, local_date = 20261115 WHERE id = {};",
            NOW - 90 * 60_000,
            nth_play(0),
            NOW - 60_000,
            nth_play(1),
        ),
    );
    assert_eq!(last_time(), "The last time was earlier today.");
    // With no local date, the day of the latest play is not known.
    tamper(
        store,
        &format!(
            "UPDATE play_event SET local_date = NULL WHERE id = {}",
            nth_play(1)
        ),
    );
    assert_eq!(last_time(), "");
}
