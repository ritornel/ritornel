//! What a page says of the listener's history with an artist or an album:
//! one sentence for its banner, the plays by year for its picture and, for
//! an artist, the records played most ("Album and artist pages" in
//! docs/internals/desktop.md).
//!
//! It counts the plays that count and are not excluded. The library's
//! artist's are the plays of their songs, by each song's credit, and the
//! library's album's those heard from one of its versions: a play bound to
//! no song reaches neither ("Queries" in docs/internals/history.md), and a
//! play bound to a song but to no record counts for its artist and for no
//! album. These are read from `play_event` through its index on counted
//! plays. An artist or an album heard by its name alone has the plays
//! linked to its name ("History by artist and album"), read from the index
//! on those links. There is no rollup table.

use std::collections::{BTreeMap, HashMap};

use ritornel_contract::{AlbumRef, HistoryOf, Phrase, RecordPlayed, Row, YearPlays, YearSpan};
use rusqlite::{Connection, OptionalExtension, params};

use crate::history_heard::{library_artists, only, record_named, records_of};
use crate::{Error, Id, LibraryStore};
use ritornel_domain::normalize::artist_key;

const DAY_MS: i64 = 86_400_000;
/// "The last year": the 365 days before now.
pub(crate) const YEAR_MS: i64 = 365 * DAY_MS;
/// Not played for this long (two years), a page says so first.
const LONG_AGO_MS: i64 = 2 * YEAR_MS + DAY_MS / 2;
/// Found this soon after the history begins, an artist was there from your
/// first weeks.
const FIRST_WEEKS_MS: i64 = 30 * DAY_MS;
/// How many of an artist's records stand under their picture.
const RECORDS: usize = 6;
/// How far apart two zones' clocks can be: from UTC−12 to UTC+14. The
/// first local date of a history is among its plays within this long of
/// the first in time, and its last among those within this long of the
/// last.
const ZONES_APART_MS: i64 = 26 * 3_600_000;

pub(crate) const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// When a page is read: the instant, and the year it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Today {
    /// Now.
    pub now_ms: i64,
    /// The year it is, where the server is.
    pub year: i32,
    /// The month it is there, from 1.
    pub month: u32,
    /// The date it is there, as YYYYMMDD.
    pub date: i64,
}

/// What a page's history is made of, before it is said.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Facts {
    /// The plays, dated or not.
    pub(crate) plays: u64,
    /// The first play in time, dated or not: its instant and its local
    /// date, YYYYMMDD, where it has one.
    pub(crate) first: Option<(i64, Option<i64>)>,
    /// The last.
    pub(crate) last: Option<(i64, Option<i64>)>,
    /// The plays in the last year.
    pub(crate) last_year: u64,
    /// The dated plays by year, those dated after now left out.
    pub(crate) years: BTreeMap<i32, u64>,
    /// The plays of each record they were heard from, keyed by the record's
    /// release group.
    pub(crate) records: HashMap<i64, RecordFacts>,
}

/// The plays of one record.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct RecordFacts {
    /// Its plays, dated or not.
    pub(crate) plays: u64,
    /// Its dated plays by year.
    pub(crate) years: BTreeMap<i32, u64>,
}

impl Facts {
    /// Counts one play: its instant, its local date and the record it was
    /// heard from, each where known.
    fn add(&mut self, at_ms: Option<i64>, date: Option<i64>, record: Option<i64>, today: Today) {
        self.plays += 1;
        if let Some(record) = record {
            self.records.entry(record).or_default().plays += 1;
        }
        // A play dated after now, by a clock that was wrong, is counted as
        // one with no instant: in no year, and neither the first nor the
        // last, which would be said as past.
        let Some(at) = at_ms.filter(|&at| at <= today.now_ms) else {
            return;
        };
        if at > today.now_ms - YEAR_MS {
            self.last_year += 1;
        }
        // Of plays at the same instant, one with a local date is taken.
        if self
            .first
            .is_none_or(|(first, day)| (at, date.is_none(), date) < (first, day.is_none(), day))
        {
            self.first = Some((at, date));
        }
        if self.last.is_none_or(|last| (at, date) > last) {
            self.last = Some((at, date));
        }
        let Some(date) = date else {
            return;
        };
        let year = year_of(date);
        *self.years.entry(year).or_default() += 1;
        if let Some(record) = record {
            *self
                .records
                .entry(record)
                .or_default()
                .years
                .entry(year)
                .or_default() += 1;
        }
    }
}

pub(crate) fn year_of(date: i64) -> i32 {
    i32::try_from(date / 10_000).unwrap_or(0)
}

/// The year with the most plays, the first of those that tie, and its plays.
pub(crate) fn peak(years: &BTreeMap<i32, u64>) -> Option<(i32, u64)> {
    years.iter().fold(
        None,
        |best: Option<(i32, u64)>, (&year, &plays)| match best {
            Some((_, most)) if most >= plays => best,
            _ => Some((year, plays)),
        },
    )
}

/// "June 2009", from a local date.
fn month(date: i64) -> String {
    let name = usize::try_from(date / 100 % 100 - 1)
        .ok()
        .and_then(|index| MONTHS.get(index))
        .copied()
        .unwrap_or("");
    format!("{name} {}", date / 10_000)
}

/// The month a play was heard in, as [`month`] says it: that of its local
/// date, or where it has none, that of its instant in UTC. That is a day
/// out at most, and only at a month's edge.
pub(crate) fn month_of((at_ms, date): (i64, Option<i64>)) -> String {
    month(date.unwrap_or_else(|| {
        let day = jiff::Timestamp::from_millisecond(at_ms)
            .unwrap_or(jiff::Timestamp::MAX)
            .to_zoned(jiff::tz::TimeZone::UTC)
            .date();
        i64::from(day.year()) * 10_000 + i64::from(day.month()) * 100 + i64::from(day.day())
    }))
}

/// A count as it is read, in thousands: "1,234".
pub(crate) fn grouped(count: u64) -> String {
    let digits = count.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

/// A count and its noun, as it is read: "1 play", "1,234 plays".
pub(crate) fn counted(count: u64, one: &str, many: &str) -> String {
    format!("{} {}", grouped(count), if count == 1 { one } else { many })
}

/// A sentence being written: runs of words, each plain or set apart.
#[derive(Default)]
pub(crate) struct Said(pub(crate) Vec<Phrase>);

impl Said {
    fn say(mut self, text: &str, strong: bool) -> Self {
        match self.0.last_mut() {
            Some(last) if last.strong == strong => last.text.push_str(text),
            _ => self.0.push(Phrase {
                text: text.to_owned(),
                strong,
            }),
        }
        self
    }

    pub(crate) fn plain(self, text: &str) -> Self {
        self.say(text, false)
    }

    pub(crate) fn strong(self, text: &str) -> Self {
        self.say(text, true)
    }
}

/// The one thing worth saying of a listener's history with an artist or
/// an album: since when, and then the first of these that is true. Not
/// played for two years, since when; played mostly in one year that is
/// over, that year; played in the last year, how much; otherwise when last.
/// `began_ms` is when the listener's history begins. Nothing at all with no
/// plays.
///
/// The first and last months are those of the first and last plays in
/// time, whether they have a local date or not ([`month_of`]). A play
/// whose instant is after now is counted as one with no instant, so it is
/// never said as past.
///
/// What waits on sessions is left out: "played start to finish".
pub(crate) fn sentence(facts: &Facts, began_ms: Option<i64>, today: Today) -> Vec<Phrase> {
    if facts.plays == 0 {
        return Vec::new();
    }
    let (Some(first), Some(last)) = (facts.first, facts.last) else {
        // Plays with no instant, or only after now: how many is all there
        // is to say.
        return Said::default()
            .plain("Played ")
            .strong(&counted(facts.plays, "time", "times"))
            .plain(".")
            .0;
    };
    let mut said = Said::default().plain("With you since ");
    if began_ms.is_some_and(|began| first.0 - began < FIRST_WEEKS_MS) {
        said = said.plain("your first weeks, ");
    }
    said = said.strong(&month_of(first));
    if last.0 < today.now_ms - LONG_AGO_MS {
        return said
            .plain(". Not played since ")
            .strong(&month_of(last))
            .plain(".")
            .0;
    }
    let dated: u64 = facts.years.values().sum();
    // Never a year the undated plays might have outdone.
    let mostly = peak(&facts.years).filter(|&(year, most)| {
        5 * most >= 2 * dated
            && dated >= 10
            && facts.years.len() > 1
            && year < today.year
            && most > facts.plays - dated
    });
    if let Some((year, _)) = mostly {
        return said
            .plain(", and most of all in ")
            .strong(&year.to_string())
            .plain(".")
            .0;
    }
    if facts.last_year > 0 {
        return said
            .plain(". ")
            .strong(&counted(facts.last_year, "play", "plays"))
            .plain(" in the last year.")
            .0;
    }
    said.plain(". Last played in ")
        .strong(&month_of(last))
        .plain(".")
        .0
}

/// A local date as `YYYY-MM-DD`.
pub(crate) fn date(yyyymmdd: i64) -> String {
    format!(
        "{:04}-{:02}-{:02}",
        yyyymmdd / 10_000,
        yyyymmdd / 100 % 100,
        yyyymmdd % 100
    )
}

/// The listener's key.
pub(crate) fn listener_key(connection: &Connection, listener: Id) -> Result<i64, Error> {
    let key: Option<i64> = connection
        .query_row(
            "SELECT id FROM listener WHERE public_id = ?1",
            [listener.as_bytes()],
            |row| row.get(0),
        )
        .optional()?;
    key.ok_or(Error::NotFound("no such listener"))
}

/// Reads the plays `sql` selects (each its instant, its local date and the
/// release group it was heard from) for the listener `?1`, with `subject`
/// as `?2`, into what a page says, with the listener's whole history beside
/// them. It is run in one read transaction with the lookup of `subject`.
fn history_of(
    connection: &Connection,
    listener: Id,
    subject: i64,
    sql: &str,
    today: Today,
    records: bool,
) -> Result<HistoryOf, Error> {
    let revision: i64 =
        connection.query_row("SELECT rev FROM library_meta WHERE id = 1", [], |row| {
            row.get(0)
        })?;
    let listener = listener_key(connection, listener)?;
    let mut facts = Facts::default();
    let mut statement = connection.prepare_cached(sql)?;
    let mut rows = statement.query(params![listener, subject])?;
    while let Some(row) = rows.next()? {
        facts.add(row.get(0)?, row.get(1)?, row.get(2)?, today);
    }
    // Where the listener's history begins, whatever is bound: through the
    // index of plays by time, the oldest first.
    let began: Option<i64> = connection
        .prepare_cached(
            "SELECT event_at_utc_ms FROM play_event
             WHERE listener_id = ?1 AND event_at_utc_ms IS NOT NULL
               AND counts = 1 AND excluded = 0
             ORDER BY event_at_utc_ms LIMIT 1",
        )?
        .query_row([listener], |row| row.get(0))
        .optional()?;
    let span = span(connection, listener, today)?;
    let records = if records {
        played_most(connection, &facts)?
    } else {
        Vec::new()
    };
    Ok(HistoryOf {
        plays: facts.plays,
        first_played: facts.first.and_then(|(_, day)| day).map(date),
        last_played: facts.last.and_then(|(_, day)| day).map(date),
        last_year: facts.last_year,
        years: facts
            .years
            .iter()
            .map(|(&year, &plays)| YearPlays { year, plays })
            .collect(),
        span,
        records,
        sentence: sentence(&facts, began, today),
        library_revision: u64::try_from(revision).unwrap_or(0),
    })
}

/// The years the listener's whole history runs over, if any play has a
/// local date: from the local year of their first dated play to this year,
/// or to the next, where a play already heard in a zone ahead of the
/// server's has reached it. A play dated after now, by a clock that was
/// wrong, neither begins nor stretches it. The first dated play in time is
/// not always the first by local date: one heard just after midnight on
/// New Year's Day in a zone far ahead can come before one heard late on
/// New Year's Eve in a zone far behind. So each end is the earliest or latest
/// local date among the plays near it in time.
fn span(connection: &Connection, listener: i64, today: Today) -> Result<Option<YearSpan>, Error> {
    let (first, last): (Option<i64>, Option<i64>) = connection
        .prepare_cached(
            "SELECT
               (SELECT min(pe.local_date) FROM play_event pe,
                  (SELECT event_at_utc_ms AS at FROM play_event
                   WHERE listener_id = ?1 AND counts = 1 AND excluded = 0
                     AND local_date IS NOT NULL AND event_at_utc_ms <= ?3
                   ORDER BY event_at_utc_ms LIMIT 1) began
                WHERE pe.listener_id = ?1 AND pe.counts = 1 AND pe.excluded = 0
                  AND pe.local_date IS NOT NULL
                  AND pe.event_at_utc_ms BETWEEN began.at AND min(began.at + ?2, ?3)),
               (SELECT max(pe.local_date) FROM play_event pe,
                  (SELECT event_at_utc_ms AS at FROM play_event
                   WHERE listener_id = ?1 AND counts = 1 AND excluded = 0
                     AND local_date IS NOT NULL AND event_at_utc_ms <= ?3
                   ORDER BY event_at_utc_ms DESC LIMIT 1) latest
                WHERE pe.listener_id = ?1 AND pe.counts = 1 AND pe.excluded = 0
                  AND pe.local_date IS NOT NULL
                  AND pe.event_at_utc_ms BETWEEN latest.at - ?2 AND latest.at)",
        )?
        .query_row(params![listener, ZONES_APART_MS, today.now_ms], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;
    Ok(first.map(|first| YearSpan {
        from: year_of(first).min(today.year),
        to: last.map_or(today.year, year_of).max(today.year),
    }))
}

/// The records played most, most first, each with its plays and the year
/// it was most played, as Albums tiles. A record the Albums list no longer
/// has is left out, and the next one takes its place.
fn played_most(connection: &Connection, facts: &Facts) -> Result<Vec<RecordPlayed>, Error> {
    let mut ranked: Vec<(i64, &RecordFacts)> = facts
        .records
        .iter()
        .map(|(&group, record)| (group, record))
        .collect();
    ranked.sort_by(|a, b| b.1.plays.cmp(&a.1.plays).then(a.0.cmp(&b.0)));
    // Read six at a time: almost always the first six are all there.
    let mut records = Vec::with_capacity(RECORDS);
    for chunk in ranked.chunks(RECORDS) {
        let keys: Vec<i64> = chunk.iter().map(|(group, _)| *group).collect();
        let tiles = crate::list::album_tiles(connection, &keys)?;
        for ((_, record), row) in chunk.iter().zip(tiles) {
            if let Row::Album(tile) = row {
                records.push(RecordPlayed {
                    tile,
                    plays: record.plays,
                    peak_year: peak(&record.years).map(|(year, _)| year),
                });
            }
            if records.len() == RECORDS {
                return Ok(records);
            }
        }
    }
    Ok(records)
}

/// An artist's plays: of every recording whose credit names them, or the
/// artist they were merged into. `?2` is the artist's canonical key.
const ARTIST_PLAYS: &str = "
SELECT pe.event_at_utc_ms, pe.local_date, rel.release_group_id
FROM play_event pe
LEFT JOIN track_appearance ta ON ta.id = pe.appearance_id
LEFT JOIN release heard ON heard.id = ta.release_id
LEFT JOIN release rel ON rel.id = heard.canonical_id
WHERE pe.listener_id = ?1 AND pe.counts = 1 AND pe.excluded = 0
  AND pe.recording_id IN (
    SELECT r.id FROM artist a
    JOIN artist_credit_name acn ON acn.artist_id = a.id
    JOIN recording r ON r.display_artist_credit_id = acn.artist_credit_id
    WHERE a.canonical_id = ?2 AND r.redirect_to_id IS NULL)";

/// An album's plays: of a song heard from one of its versions. `?2` is its
/// release group's key. The recordings narrow the read to the index on
/// counted plays; the appearances say which of those were of this record.
/// After what a page reads come what Lean back reads as well: each play's
/// song, local hour and offset from UTC.
pub(crate) const ALBUM_PLAYS: &str = "
WITH heard AS (
  SELECT ta.id, ta.recording_id FROM release rel
  JOIN track_appearance ta ON ta.release_id = rel.id
  WHERE rel.release_group_id = ?2)
SELECT pe.event_at_utc_ms, pe.local_date, NULL, pe.recording_id, pe.local_hour,
       pe.utc_offset_minutes
FROM play_event pe
WHERE pe.listener_id = ?1 AND pe.counts = 1 AND pe.excluded = 0
  AND pe.recording_id IN (
    SELECT r.canonical_id FROM heard JOIN recording r ON r.id = heard.recording_id)
  AND pe.appearance_id IN (SELECT id FROM heard)";

/// The plays of an artist heard by name: those linked to their row. `?2`
/// is the row.
const NAME_PLAYS: &str = "
SELECT pe.event_at_utc_ms, pe.local_date, NULL
FROM play_event_artist pa
CROSS JOIN play_event pe ON pe.id = pa.play_event_id
WHERE pa.heard_artist_id = ?2 AND pa.counted = 1 AND pa.listener_id = ?1";

/// The plays of an album heard by name: those linked to its row. `?2` is
/// the row.
const NAMED_ALBUM_PLAYS: &str = "
SELECT pe.event_at_utc_ms, pe.local_date, NULL
FROM play_event_album pa
CROSS JOIN play_event pe ON pe.id = pa.play_event_id
WHERE pa.heard_album_id = ?2 AND pa.counted = 1 AND pa.listener_id = ?1";

impl LibraryStore {
    /// What an artist's page says of `listener`'s history with them, read
    /// `today`. `None` if the library has no such artist.
    pub fn artist_history(
        &self,
        listener: Id,
        artist: Id,
        today: Today,
    ) -> Result<Option<HistoryOf>, Error> {
        self.read_snapshot(move |connection| {
            let key: Option<i64> = connection
                .query_row(
                    "SELECT canonical_id FROM artist WHERE public_id = ?1",
                    [artist.as_bytes()],
                    |row| row.get(0),
                )
                .optional()?;
            key.map(|key| history_of(connection, listener, key, ARTIST_PLAYS, today, true))
                .transpose()
        })
    }

    /// What an album's page says of `listener`'s history with it, across
    /// every version of its record, read `today`. A release is taken as its
    /// record. `None` if the library has no such album.
    pub fn album_history(
        &self,
        listener: Id,
        album: AlbumRef,
        today: Today,
    ) -> Result<Option<HistoryOf>, Error> {
        self.read_snapshot(move |connection| {
            let group: Option<i64> = match album {
                AlbumRef::ReleaseGroup { id } => connection
                    .query_row(
                        "SELECT id FROM release_group WHERE public_id = ?1",
                        [id.as_bytes()],
                        |row| row.get(0),
                    )
                    .optional()?,
                AlbumRef::Release { id } => connection
                    .query_row(
                        "SELECT c.release_group_id FROM release r
                         JOIN release c ON c.id = r.canonical_id WHERE r.public_id = ?1",
                        [id.as_bytes()],
                        |row| row.get(0),
                    )
                    .optional()?,
            };
            group
                .map(|group| history_of(connection, listener, group, ALBUM_PLAYS, today, false))
                .transpose()
        })
    }
}

impl LibraryStore {
    /// What a page says of `listener`'s history with an artist heard, by
    /// their name's key, read `today`: the library's artist, when exactly
    /// one with a record has that name, and otherwise the name's plays.
    /// `None` if no play was heard under it.
    pub fn heard_artist_history(
        &self,
        listener: Id,
        key: String,
        today: Today,
    ) -> Result<Option<HistoryOf>, Error> {
        self.read_snapshot(move |connection| {
            if let Some(artist) = only(library_artists(connection)?.get(&key)) {
                return history_of(connection, listener, artist, ARTIST_PLAYS, today, true)
                    .map(Some);
            }
            let row: Option<i64> = connection
                .query_row(
                    "SELECT id FROM heard_artist WHERE name_key = ?1",
                    [&key],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(row) = row {
                return Ok(
                    history_of(connection, listener, row, NAME_PLAYS, today, false)
                        .map(Some)?
                        .filter(|of| of.plays > 0),
                );
            }
            // The library's artist a song's credit names who has no record
            // of their own, as History lists them.
            let mut credited = connection.prepare(
                "SELECT DISTINCT ca.id, ca.display_name FROM heard_artist h
                 JOIN artist a ON a.id = h.artist_id
                 JOIN artist ca ON ca.id = a.canonical_id
                 WHERE ca.display_name IS NOT NULL",
            )?;
            let named: Vec<i64> = credited
                .query_map([], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                })?
                .filter_map(|row| match row {
                    Ok((artist, name)) => (artist_key(&name) == key).then_some(Ok(artist)),
                    Err(error) => Some(Err(error)),
                })
                .collect::<Result<_, _>>()?;
            only(Some(&named))
                .map(|artist| history_of(connection, listener, artist, ARTIST_PLAYS, today, true))
                .transpose()
        })
    }

    /// What a page says of `listener`'s history with an album heard, by its
    /// artist's key and its own, read `today`: the library's record, when
    /// its artist is exactly one of the library's and exactly one of theirs
    /// has that title, and otherwise the name's plays. `None` if no play
    /// was heard under it.
    pub fn heard_album_history(
        &self,
        listener: Id,
        artist_key: String,
        key: String,
        today: Today,
    ) -> Result<Option<HistoryOf>, Error> {
        self.read_snapshot(move |connection| {
            let artist = only(library_artists(connection)?.get(&artist_key));
            if let Some(artist) = artist
                && let Some(group) = record_named(Some(&records_of(connection, artist)?), &key)
            {
                return history_of(connection, listener, group, ALBUM_PLAYS, today, false)
                    .map(Some);
            }
            let row: Option<i64> = connection
                .query_row(
                    "SELECT h.id FROM heard_album h
                     JOIN heard_artist ha ON ha.id = h.heard_artist_id
                     WHERE ha.name_key = ?1 AND h.name_key = ?2",
                    [&artist_key, &key],
                    |row| row.get(0),
                )
                .optional()?;
            Ok(row
                .map(|row| history_of(connection, listener, row, NAMED_ALBUM_PLAYS, today, false))
                .transpose()?
                .filter(|of| of.plays > 0))
        })
    }
}

#[cfg(test)]
mod tests;
