//! What Lean back says about the song that is playing and its record, one
//! thing at a time ("History tab and Lean back" in
//! docs/internals/desktop.md).
//!
//! It counts as a page does (history_pages.rs): the plays that count and
//! are not excluded, of songs the library has bound them to. The song's are
//! its recording's, wherever it was heard; the record's are those heard
//! from one of its versions, read by the album page's own query. What waits
//! on listening sessions is left out: "played start to finish" and "you
//! often play it with".

use std::collections::{BTreeMap, HashMap, HashSet};

use jiff::Unit;
use jiff::civil::Date;
use ritornel_contract::{Fact, FactKind, TrackHistory};
use rusqlite::{Connection, OptionalExtension, params};

use crate::history_pages::{
    ALBUM_PLAYS, MONTHS, Said, Today, YEAR_MS, counted, grouped, listener_key, month_of, peak,
    year_of,
};
use crate::{Error, Id, LibraryStore};

/// The parts of a day, six hours each from midnight.
const PARTS: [&str; 4] = ["late-night", "morning", "afternoon", "evening"];
/// Fewer plays than this, and a record has no year of its own.
const YEAR_PLAYS: u64 = 20;
/// Fewer plays with an hour than this, and a record has no time of day.
const PART_PLAYS: u64 = 30;
/// Fewer plays than this, and a record's song played most goes unsaid.
const MOST_PLAYS: u64 = 10;

/// The song that is playing, as far as its plays go.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Song {
    /// Its plays, wherever it was heard.
    pub(crate) plays: u64,
    /// The play of it with the latest instant: that instant, and its local
    /// date, YYYYMMDD, where it has one.
    pub(crate) last: Option<(i64, Option<i64>)>,
    /// Whether the listener gave it five stars.
    pub(crate) five_stars: bool,
}

/// One play of the record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Heard {
    pub(crate) at_ms: Option<i64>,
    /// Its local date, YYYYMMDD.
    pub(crate) date: Option<i64>,
    /// Its song, by its recording's key.
    pub(crate) song: i64,
    /// Its local hour.
    pub(crate) hour: Option<i64>,
    /// Its zone's offset from UTC then, in minutes.
    pub(crate) offset: Option<i64>,
}

impl Heard {
    /// The minute of the local day it began in.
    fn minute(self) -> Option<i64> {
        Some((self.at_ms?.div_euclid(60_000) + self.offset?).rem_euclid(1_440))
    }
}

/// The record that is playing, as far as its plays go.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Record {
    /// Its plays, heard from any of its versions.
    pub(crate) plays: Vec<Heard>,
    /// How many songs the version playing has.
    pub(crate) size: u64,
    /// Its songs' titles on it, as the version playing names them, by
    /// their recordings' keys.
    pub(crate) titles: HashMap<i64, String>,
    /// The song on it played most, wherever heard: its title and its plays.
    pub(crate) top: Option<(String, u64)>,
}

/// What Lean back's facts are made of, before they are said.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Gathered {
    pub(crate) song: Song,
    /// `None` for a song on no record.
    pub(crate) record: Option<Record>,
    /// The listener's plays in each part of the day: what a record's hours
    /// are held against.
    pub(crate) parts: [u64; 4],
}

fn fact(kind: FactKind, head: Said, more: Option<String>) -> Fact {
    Fact {
        kind,
        head: head.0,
        more,
    }
}

fn civil(date: i64) -> Option<Date> {
    Date::new(
        i16::try_from(date / 10_000).ok()?,
        i8::try_from(date / 100 % 100).ok()?,
        i8::try_from(date % 100).ok()?,
    )
    .ok()
}

/// "10 October 2007", from a local date.
fn long(date: i64) -> String {
    let name = usize::try_from(date / 100 % 100 - 1)
        .ok()
        .and_then(|index| MONTHS.get(index))
        .copied()
        .unwrap_or("");
    format!("{} {name} {}", date % 100, date / 10_000)
}

/// The part of the day most of a record's plays fall in, when it holds
/// 45% of them and there are enough to say: the part, its plays, and all
/// the plays with an hour.
fn leaning(plays: &[Heard]) -> Option<(usize, u64, u64)> {
    let mut parts = [0_u64; 4];
    for heard in plays {
        if let Some(part) = heard
            .hour
            .and_then(|hour| usize::try_from(hour / 6).ok())
            .and_then(|part| parts.get_mut(part))
        {
            *part += 1;
        }
    }
    let timed: u64 = parts.iter().sum();
    let (part, most) =
        parts.iter().enumerate().fold(
            (0, 0),
            |best, (part, &n)| {
                if n > best.1 { (part, n) } else { best }
            },
        );
    (timed >= PART_PLAYS && 100 * most >= 45 * timed).then_some((part, most, timed))
}

/// How long before `today` a local date was: "today", "yesterday", "12 days
/// ago", "3 months ago", "a year ago", "19 years ago".
fn ago(date: i64, today: i64) -> Option<String> {
    let (from, to) = (civil(date)?, civil(today)?);
    let years = from.until((Unit::Year, to)).ok()?.get_years();
    if years >= 1 {
        return Some(if years == 1 {
            "a year ago".to_owned()
        } else {
            format!("{years} years ago")
        });
    }
    let days = from.until(to).ok()?.get_days();
    Some(match days {
        ..=0 => "today".to_owned(),
        1 => "yesterday".to_owned(),
        2..45 => format!("{days} days ago"),
        _ => match from.until((Unit::Month, to)).ok()?.get_months() {
            ..=1 => "a month ago".to_owned(),
            months => format!("{months} months ago"),
        },
    })
}

/// The facts about a song and its record, in the order Lean back shows
/// them: how often the song was played, when the record was first heard,
/// its year, its time of day, this day in an earlier year, its song played
/// most, and how long it had gone unplayed. Each is included only where it
/// meets the relevance thresholds.
pub(crate) fn facts(gathered: &Gathered, today: Today) -> Vec<Fact> {
    let mut out = Vec::new();
    let song = &gathered.song;
    if song.plays > 1 {
        let stars = if song.five_stars {
            "One of your five-star songs. "
        } else {
            ""
        };
        let last = song.last.map(|last| {
            if last.1 == Some(today.date) {
                "The last time was earlier today.".to_owned()
            } else {
                format!("The last time was in {}.", month_of(last))
            }
        });
        let more = last
            .map(|last| format!("{stars}{last}"))
            .or_else(|| song.five_stars.then(|| stars.trim_end().to_owned()));
        out.push(fact(
            FactKind::SongPlays,
            Said::default()
                .plain("You have played this song ")
                .strong(&counted(song.plays, "time", "times"))
                .plain("."),
            more,
        ));
    }
    let Some(record) = gathered
        .record
        .as_ref()
        .filter(|record| !record.plays.is_empty())
    else {
        return out;
    };
    let plays = u64::try_from(record.plays.len()).unwrap_or(u64::MAX);
    // A play's instant, unless it is after now, by a clock that was wrong:
    // such a play is taken as one with no instant, so it is never said as
    // past.
    let instant = |heard: &Heard| heard.at_ms.filter(|&at| at <= today.now_ms);
    // The plays with a day, each with its instant and its date.
    let dated: Vec<(i64, i64, &Heard)> = record
        .plays
        .iter()
        .filter_map(|heard| Some((instant(heard)?, heard.date?, heard)))
        .collect();

    // A play with no local date before it may be from a day earlier still,
    // so nothing is said.
    if let Some(&(first_at, first, _)) = dated.iter().min_by_key(|(at, ..)| *at)
        && !record.plays.iter().any(|heard| {
            heard.date.is_none() && instant(heard).is_some_and(|earlier| earlier < first_at)
        })
        && let Some(ago) = ago(first, today.date)
    {
        let since = if plays > 1 {
            format!(" {} since.", counted(plays, "play", "plays"))
        } else {
            String::new()
        };
        out.push(fact(
            FactKind::FirstHeard,
            Said::default()
                .plain("You first heard this record ")
                .strong(&ago)
                .plain("."),
            Some(format!("{}.{since}", long(first))),
        ));
    }

    // A year that is over and held three plays in ten.
    let mut years: BTreeMap<i32, u64> = BTreeMap::new();
    for &(_, date, _) in &dated {
        *years.entry(year_of(date)).or_default() += 1;
    }
    // Never a year the undated plays might have outdone.
    let undated = plays - u64::try_from(dated.len()).unwrap_or(plays);
    if let Some((year, most)) = peak(&years)
        && plays >= YEAR_PLAYS
        && 10 * most >= 3 * plays
        && year < today.year
        && most > undated
    {
        let more = if most == plays {
            format!("All {} of its plays came then.", grouped(plays))
        } else {
            format!(
                "{} of its {} plays came then.",
                grouped(most),
                grouped(plays)
            )
        };
        out.push(fact(
            FactKind::ItsYear,
            Said::default()
                .strong(&year.to_string())
                .plain(" was its year."),
            Some(more),
        ));
    }

    // A part of the day that holds most of its plays, and half as many
    // again as the listener's plays at that time hold of theirs.
    let all: u64 = gathered.parts.iter().sum();
    if let Some((part, most, timed)) = leaning(&record.plays)
        && all > 0
        && 2 * most * all >= 3 * gathered.parts.get(part).copied().unwrap_or(0) * timed
        && let Some(name) = PARTS.get(part)
    {
        let article = if name.starts_with(['a', 'e']) {
            "an"
        } else {
            "a"
        };
        out.push(fact(
            FactKind::TimeOfDay,
            Said::default()
                .plain(&format!("Mostly {article} "))
                .strong(name)
                .plain(" record."),
            Some(format!(
                "{}% of its plays.",
                (100 * most + timed / 2) / timed
            )),
        ));
    }

    // This day of the year, the last year before this one it was played on.
    let day = today.date % 10_000;
    let year = |date: i64| date / 10_000;
    if let Some(date) = dated
        .iter()
        .map(|&(_, date, _)| date)
        .filter(|&date| date % 10_000 == day && year(date) < year(today.date))
        .max()
    {
        let that: Vec<&Heard> = dated
            .iter()
            .filter(|&&(_, on, _)| on == date)
            .map(|&(.., heard)| heard)
            .collect();
        let n = that.len();
        let songs: HashSet<i64> = that.iter().map(|heard| heard.song).collect();
        let distinct = u64::try_from(songs.len()).unwrap_or(0);
        let alone = songs
            .iter()
            .next()
            .and_then(|song| record.titles.get(song))
            .filter(|_| distinct == 1);
        let what = if record.size >= 4 && 4 * distinct >= 3 * record.size {
            "The whole album".to_owned()
        } else if let Some(title) = alone {
            if n > 1 {
                format!("“{title}” {n} times")
            } else {
                format!("“{title}”")
            }
        } else {
            counted(distinct, "of its songs", "of its songs")
        };
        let when = that
            .iter()
            .filter_map(|heard| heard.minute())
            .min()
            .map_or_else(String::new, |minute| {
                let word = if n == 1 { "at" } else { "from" };
                format!(", {word} {:02}:{:02}", minute / 60, minute % 60)
            });
        out.push(fact(
            FactKind::OnThisDay,
            Said::default()
                .plain("You played this ")
                .strong(&format!("on this day in {}", year(date)))
                .plain("."),
            Some(format!("{what}{when}.")),
        ));
    }

    if let Some((title, most)) = &record.top
        && *most >= MOST_PLAYS
    {
        out.push(fact(
            FactKind::PlayedMost,
            Said::default()
                .strong(&format!("“{title}”"))
                .plain(" is the one you play most."),
            Some(format!("{}.", counted(*most, "time", "times"))),
        ));
    }

    // Unplayed for over a year before today, whatever was played today. A
    // play with no local date may have been today, and is counted as before
    // it: if it is the last, it was lately, and nothing is said. Of plays
    // at the same instant, one with a local date is taken.
    if let Some(last) = record
        .plays
        .iter()
        .filter(|heard| heard.date.is_none_or(|date| date < today.date))
        .filter_map(|heard| Some((instant(heard)?, heard.date)))
        .max()
        && last.0 < today.now_ms - YEAR_MS
    {
        out.push(fact(
            FactKind::NotSince,
            Said::default()
                .plain("Before today, you had not played it ")
                .strong(&format!("since {}", month_of(last)))
                .plain("."),
            None,
        ));
    }
    out
}

/// The song an appearance is of, and its record: the recording's key, and
/// the release it is on and that release's group, where it is on one.
const APPEARANCE: &str = "
SELECT r.canonical_id, rel.id, rel.release_group_id
FROM track_appearance ta
JOIN track_appearance cta ON cta.id = ta.canonical_id
JOIN recording r ON r.id = cta.recording_id
LEFT JOIN release heard ON heard.id = cta.release_id
LEFT JOIN release rel ON rel.id = heard.canonical_id
WHERE ta.public_id = ?1";

/// A song's plays, wherever heard, and the last of them: the play with the
/// latest instant not after now (`?3`), which is not always the one with
/// the latest local date when plays were heard in zones far apart, and one
/// with a local date of those at the same instant. Its instant and its
/// local date. A play dated after now, by a clock that was wrong, is
/// counted and is not the last.
const SONG_PLAYS: &str = "
SELECT count(*), last.event_at_utc_ms, last.local_date
FROM play_event
LEFT JOIN (SELECT event_at_utc_ms, local_date FROM play_event
           WHERE listener_id = ?1 AND recording_id = ?2 AND counts = 1 AND excluded = 0
             AND event_at_utc_ms <= ?3
           ORDER BY event_at_utc_ms DESC, local_date IS NULL, id DESC LIMIT 1) last
  ON true
WHERE listener_id = ?1 AND recording_id = ?2 AND counts = 1 AND excluded = 0";

/// The songs of a record's versions, each with its title on the record and
/// its plays wherever heard, the version playing first. `?2` is the
/// record's release group and `?3` the release playing. A song on several
/// versions comes once for each. The last column says whether a file reaches the song there: only one
/// that does is the record's song played most.
const RECORD_SONGS: &str = "
SELECT r.canonical_id, coalesce(ta.display_title, cr.display_title),
       (SELECT count(*) FROM play_event pe
        WHERE pe.listener_id = ?1 AND pe.recording_id = r.canonical_id
          AND pe.counts = 1 AND pe.excluded = 0),
       ta.redirect_to_id IS NULL AND rel.redirect_to_id IS NULL AND {reached}
FROM release rel
JOIN track_appearance ta ON ta.release_id = rel.id
JOIN recording r ON r.id = ta.recording_id
JOIN recording cr ON cr.id = r.canonical_id
WHERE rel.release_group_id = ?2
ORDER BY rel.canonical_id = ?3 DESC, ta.display_disc_number, ta.display_track_number";

/// The listener's plays in each part of the day, by local hour.
const PARTS_OF_DAY: &str = "
SELECT local_hour / 6, count(*) FROM play_event
WHERE listener_id = ?1 AND counts = 1 AND excluded = 0 AND local_hour IS NOT NULL
GROUP BY local_hour / 6";

/// A count as SQLite gives it.
fn count(n: i64) -> u64 {
    u64::try_from(n).unwrap_or(0)
}

fn read_record(
    connection: &Connection,
    listener: i64,
    release: i64,
    group: i64,
) -> Result<Record, Error> {
    let mut record = Record::default();
    let mut statement = connection.prepare_cached(ALBUM_PLAYS)?;
    let mut rows = statement.query(params![listener, group])?;
    while let Some(row) = rows.next()? {
        record.plays.push(Heard {
            at_ms: row.get(0)?,
            date: row.get(1)?,
            song: row.get(3)?,
            hour: row.get(4)?,
            offset: row.get(5)?,
        });
    }
    if record.plays.is_empty() {
        return Ok(record);
    }
    // Its songs a file reaches, as its page lists them.
    let size: i64 = connection
        .prepare_cached(&format!(
            "SELECT count(*) FROM track_appearance ta
             WHERE ta.release_id = ?1 AND ta.redirect_to_id IS NULL AND {}",
            crate::list::REACHED_TA
        ))?
        .query_row([release], |row| row.get(0))?;
    record.size = count(size);
    let mut statement =
        connection.prepare_cached(&RECORD_SONGS.replace("{reached}", crate::list::REACHED_TA))?;
    let mut rows = statement.query(params![listener, group, release])?;
    while let Some(row) = rows.next()? {
        let (song, title, plays, reached): (i64, Option<String>, i64, bool) =
            (row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?);
        // A song is named as the version playing names it, and counted once.
        if record.titles.contains_key(&song) {
            continue;
        }
        let plays = count(plays);
        let title = title.unwrap_or_else(|| "Untitled".to_owned());
        if reached && record.top.as_ref().is_none_or(|(_, most)| plays > *most) {
            record.top = Some((title.clone(), plays));
        }
        record.titles.insert(song, title);
    }
    Ok(record)
}

fn read_track(
    connection: &Connection,
    listener: Id,
    appearance: Id,
    today: Today,
) -> Result<Option<TrackHistory>, Error> {
    let revision: i64 =
        connection.query_row("SELECT rev FROM library_meta WHERE id = 1", [], |row| {
            row.get(0)
        })?;
    let listener = listener_key(connection, listener)?;
    let found: Option<(i64, Option<i64>, Option<i64>)> = connection
        .prepare_cached(APPEARANCE)?
        .query_row([appearance.as_bytes()], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .optional()?;
    let Some((recording, release, group)) = found else {
        return Ok(None);
    };
    let (plays, last_at, last_date): (i64, Option<i64>, Option<i64>) = connection
        .prepare_cached(SONG_PLAYS)?
        .query_row(params![listener, recording, today.now_ms], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
    let five_stars = crate::ratings::said(connection, &[recording])?
        .get(&recording)
        .and_then(|said| said.rating)
        == Some(100);
    let record = match (release, group) {
        (Some(release), Some(group)) => Some(read_record(connection, listener, release, group)?),
        _ => None,
    };
    // The listener's hours are every play they have, so they are read only
    // for a record whose own hours lean one way.
    let mut parts = [0_u64; 4];
    if record
        .as_ref()
        .is_some_and(|record| leaning(&record.plays).is_some())
    {
        let mut statement = connection.prepare_cached(PARTS_OF_DAY)?;
        let mut rows = statement.query([listener])?;
        while let Some(row) = rows.next()? {
            let (part, plays): (i64, i64) = (row.get(0)?, row.get(1)?);
            if let Some(slot) = usize::try_from(part)
                .ok()
                .and_then(|part| parts.get_mut(part))
            {
                *slot = count(plays);
            }
        }
    }
    let gathered = Gathered {
        song: Song {
            plays: count(plays),
            last: last_at.map(|at| (at, last_date)),
            five_stars,
        },
        record,
        parts,
    };
    Ok(Some(TrackHistory {
        facts: facts(&gathered, today),
        library_revision: u64::try_from(revision).unwrap_or(0),
    }))
}

impl LibraryStore {
    /// What Lean back says about the song an appearance is of and its
    /// record, for `listener`, read `today`. `None` if the library has no
    /// such appearance.
    pub fn track_history(
        &self,
        listener: Id,
        appearance: Id,
        today: Today,
    ) -> Result<Option<TrackHistory>, Error> {
        self.read_snapshot(move |connection| read_track(connection, listener, appearance, today))
    }
}

#[cfg(test)]
mod tests;
