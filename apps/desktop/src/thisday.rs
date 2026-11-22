//! "On this day", under History's picture ("History tab and Lean back" in
//! docs/internals/desktop.md): who you spent this day with in years gone
//! by. A few lines and no more; a day that was a device catching up says
//! nothing. Its tests come with it.

use ritornel_contract::{OnThisDay, Phrase, Play};

use crate::words::plural;

/// More plays than this in one day is a device catching up, as History's
/// picture has it.
const SYNC_DAY: usize = 300;
/// How many years are remembered at most.
const AT_MOST: usize = 3;

const NUMBERS: [&str; 21] = [
    "no",
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "ten",
    "eleven",
    "twelve",
    "thirteen",
    "fourteen",
    "fifteen",
    "sixteen",
    "seventeen",
    "eighteen",
    "nineteen",
    "twenty",
];

/// One year's day, remembered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Remembered {
    pub year: i32,
    /// How many years ago.
    pub ago: i32,
    pub artist: String,
    /// Morning, afternoon, evening or night: when you started on them.
    pub part: &'static str,
    /// How many of that day's plays were theirs.
    pub plays: u64,
    /// The record most of them were from, when the plays name one.
    pub record: Option<String>,
}

fn part_of(hour: u32) -> &'static str {
    match hour {
        0..5 => "night",
        5..12 => "morning",
        12..18 => "afternoon",
        _ => "evening",
    }
}

/// The one value of `values` there is most of, the first of those that tie.
fn most<'a>(values: impl Iterator<Item = &'a str>) -> Option<(&'a str, u64)> {
    let mut counts: Vec<(&str, u64)> = Vec::new();
    for value in values {
        match counts.iter_mut().find(|(seen, _)| *seen == value) {
            Some((_, count)) => *count += 1,
            None => counts.push((value, 1)),
        }
    }
    let mut best: Option<(&str, u64)> = None;
    for (value, count) in counts {
        if best.is_none_or(|(_, most)| count > most) {
            best = Some((value, count));
        }
    }
    best
}

/// The years whose day was given to one artist, at most three, nearest
/// first: on a day of that year, the artist with five plays or more and
/// three in ten of the day's. A round number of years ago is remembered
/// before the rest.
pub(crate) fn remember(day: &OnThisDay, this_year: i32) -> Vec<Remembered> {
    let mut by_year: Vec<(i32, Vec<&Play>)> = Vec::new();
    for play in &day.plays {
        let Some(year) = play
            .local_date
            .as_deref()
            .and_then(|date| date.get(..4))
            .and_then(|year| year.parse::<i32>().ok())
        else {
            continue;
        };
        if year >= this_year {
            continue;
        }
        match by_year.iter_mut().find(|(seen, _)| *seen == year) {
            Some((_, plays)) => plays.push(play),
            None => by_year.push((year, vec![play])),
        }
    }
    let mut found: Vec<(Remembered, u64)> = Vec::new();
    for (year, plays) in by_year {
        if plays.len() > SYNC_DAY {
            continue;
        }
        let artists = plays
            .iter()
            .filter_map(|play| play.track.artist.as_deref())
            .filter(|artist| !artist.is_empty());
        let Some((artist, count)) = most(artists) else {
            continue;
        };
        if count < 5 || (count as f64) / (plays.len() as f64) < 0.3 {
            continue;
        }
        let theirs: Vec<&Play> = plays
            .iter()
            .copied()
            .filter(|play| play.track.artist.as_deref() == Some(artist))
            .collect();
        let Some(first) = theirs.iter().copied().reduce(|earliest, play| {
            if play.at.unwrap_or(0) < earliest.at.unwrap_or(0) {
                play
            } else {
                earliest
            }
        }) else {
            continue;
        };
        let record = most(
            theirs
                .iter()
                .filter_map(|play| play.track.album.as_deref())
                .filter(|album| !album.is_empty()),
        );
        let ago = this_year - year;
        found.push((
            Remembered {
                year,
                ago,
                artist: artist.to_owned(),
                part: part_of(first.local_hour.unwrap_or(12)),
                plays: count,
                record: record.map(|(record, _)| record.to_owned()),
            },
            count + if ago % 5 == 0 { 40 } else { 0 },
        ));
    }
    found.sort_by_key(|(_, score)| std::cmp::Reverse(*score));
    found.truncate(AT_MOST);
    let mut remembered: Vec<Remembered> = found.into_iter().map(|(one, _)| one).collect();
    remembered.sort_by_key(|one| one.ago);
    remembered
}

/// "Twelve years ago today you spent the evening with Artist."
pub(crate) fn say_remembered(remembered: &Remembered) -> Vec<Phrase> {
    let count = usize::try_from(remembered.ago)
        .ok()
        .and_then(|ago| NUMBERS.get(ago))
        .map_or_else(|| remembered.ago.to_string(), |word| (*word).to_owned());
    let mut chars = count.chars();
    let capital: String = chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default();
    let years = if remembered.ago == 1 { "year" } else { "years" };
    let phrase = |text: String, strong: bool| Phrase { text, strong };
    vec![
        phrase(format!("{capital} {years} ago today"), true),
        phrase(format!(" you spent the {} with ", remembered.part), false),
        phrase(remembered.artist.clone(), true),
        phrase(
            if remembered.artist.ends_with('.') {
                ""
            } else {
                "."
            }
            .to_owned(),
            false,
        ),
    ]
}

/// "9 plays, mostly" and the record, which is set apart where it is shown.
pub(crate) fn plays_of(remembered: &Remembered) -> String {
    match remembered.record {
        None => format!("{}.", plural(remembered.plays, "play")),
        Some(_) => format!("{}, mostly", plural(remembered.plays, "play")),
    }
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;
    use ritornel_contract::{Id, PlayOutcome, PlayedTrack};

    fn play(year: i32, artist: &str, hour: u32, album: &str) -> Play {
        Play {
            play_id: Id::new_v7(0, [0; 10]),
            // November 16th of that year, near enough: the order is what counts.
            at: Some(i64::from(year - 1970) * 31_556_952_000 + i64::from(hour) * 3_600_000),
            local_date: Some(format!("{year}-11-16")),
            local_hour: Some(hour),
            zone: None,
            utc_offset_minutes: None,
            clock_basis: "source_utc".to_owned(),
            track: PlayedTrack {
                appearance_id: None,
                recording_id: None,
                title: Some("Song".to_owned()),
                artist: Some(artist.to_owned()),
                album: Some(album.to_owned()),
            },
            ms_played: None,
            duration_ms: None,
            outcome: PlayOutcome::Listened,
            counts: true,
            excluded: false,
            listens: Vec::new(),
        }
    }

    fn text(phrases: &[Phrase]) -> String {
        phrases.iter().map(|phrase| phrase.text.as_str()).collect()
    }

    fn times(n: usize, year: i32, artist: &str, hour: u32, album: &str) -> Vec<Play> {
        (0..n).map(|_| play(year, artist, hour, album)).collect()
    }

    #[test]
    fn remembers_the_years_a_day_was_given_to_one_artist_a_round_number_of_years_first() {
        let mut plays = times(6, 2014, "Ada", 21, "Night");
        plays.push(play(2014, "Bo", 9, "Night"));
        plays.extend(times(5, 2016, "Cy", 10, "Day"));
        plays.extend(times(9, 2021, "Dee", 15, "Night"));
        // Too spread out to be anyone's day.
        for who in ["A", "B", "C", "D", "E", "F"] {
            plays.push(play(2019, who, 12, "Night"));
            plays.push(play(2019, who, 13, "Night"));
        }
        // This year's day is not over.
        plays.extend(times(8, 2026, "Eve", 8, "Night"));
        let day = OnThisDay {
            day: "11-16".to_owned(),
            plays,
        };
        let found = remember(&day, 2026);
        let who: Vec<(i32, &str)> = found
            .iter()
            .map(|one| (one.year, one.artist.as_str()))
            .collect();
        assert_eq!(who, [(2021, "Dee"), (2016, "Cy"), (2014, "Ada")]);
        assert_eq!(
            text(&say_remembered(&found[0])),
            "Five years ago today you spent the afternoon with Dee."
        );
        assert_eq!(
            text(&say_remembered(&found[2])),
            "Twelve years ago today you spent the evening with Ada."
        );
        assert_eq!(found[1].record.as_deref(), Some("Day"));
        assert_eq!(plays_of(&found[1]), "5 plays, mostly");
    }

    #[test]
    fn forgets_a_day_that_was_a_device_catching_up() {
        let day = OnThisDay {
            day: "11-16".to_owned(),
            plays: times(301, 2020, "Ada", 11, "Night"),
        };
        assert_eq!(remember(&day, 2026), []);
    }
}
