//! The listener's whole history as the History tab draws it: the plays of
//! each month, in layers by the year each artist was first heard ("History
//! tab and Lean back" in docs/internals/desktop.md).
//!
//! It counts every dated play that counts and is not excluded, bound or
//! not, as History is the record of all of it. A play is its artist's as
//! the library credits the song, or, unbound, as its source named them; an
//! artist is known by their name whatever its case. A day of more than 300
//! plays is treated as a device catching up and left out.
//!
//! It reads every counted play once, so it is the heaviest read of
//! history: about a fifth of a second at 250,000 plays (`history_size.rs`).
//! The plays are counted here by day and by credit, as numbers, rather
//! than grouped by name in SQL, which sorted every play by its text and
//! took several times as long. The tab is opened by hand and the page
//! holds what it read, so no rollup is kept for it.

use std::collections::{BTreeMap, HashMap};

use ritornel_contract::{HistoryStream, StreamLayer};
use rusqlite::{Connection, OptionalExtension};

use crate::{Error, Id, LibraryStore, Today};

/// More plays than this in one day is a device catching up.
const SYNC_DAY: u64 = 300;
/// How many artists a layer names for each month.
const NAMED: usize = 3;

/// Every counted, dated play of the listener `?1`, with the credit of its
/// song when it is bound, or of the track that was queued, and otherwise
/// the artist its source named.
const PLAYS: &str = "
SELECT pe.local_date, coalesce(r.display_artist_credit_id, qta.display_artist_credit_id), imp.artist
FROM play_event pe
LEFT JOIN recording r ON r.id = pe.recording_id
LEFT JOIN play_event_observation peo
  ON pe.recording_id IS NULL AND peo.play_event_id = pe.id AND peo.role = 'primary'
LEFT JOIN observation_import imp ON imp.observation_id = peo.observation_id
LEFT JOIN observation_listen l ON l.observation_id = peo.observation_id
LEFT JOIN track_appearance qta ON qta.id = l.queued_appearance_id
WHERE pe.listener_id = ?1 AND pe.counts = 1 AND pe.excluded = 0 AND pe.local_date IS NOT NULL";

/// Who a play is credited to, before names are compared.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Credit {
    Library(i64),
    Named(String),
    Nobody,
}

/// One layer as it is gathered: its plays in each month, and in each month
/// its artists with their plays.
type Layering = (Vec<u64>, Vec<Vec<(u64, usize)>>);

/// A month as a number that counts up by one a month.
fn month_of(date: i64) -> i64 {
    (date / 10_000) * 12 + (date / 100 % 100 - 1)
}

/// The plays of each day by each credit, counted from the scan.
fn by_day(connection: &Connection, listener: i64) -> Result<HashMap<(i64, Credit), u64>, Error> {
    let mut statement = connection.prepare(PLAYS)?;
    let mut rows = statement.query([listener])?;
    let mut counted: HashMap<(i64, Credit), u64> = HashMap::new();
    while let Some(row) = rows.next()? {
        let day: i64 = row.get(0)?;
        let credit = match (
            row.get::<_, Option<i64>>(1)?,
            row.get::<_, Option<String>>(2)?,
        ) {
            (Some(credit), _) => Credit::Library(credit),
            (None, Some(name)) => Credit::Named(name),
            (None, None) => Credit::Nobody,
        };
        *counted.entry((day, credit)).or_default() += 1;
    }
    Ok(counted)
}

/// The name of each credit of the library that `credits` holds.
fn credit_names(
    connection: &Connection,
    credits: impl Iterator<Item = i64>,
) -> Result<HashMap<i64, String>, Error> {
    let mut statement =
        connection.prepare_cached("SELECT display_text FROM artist_credit WHERE id = ?1")?;
    let mut names = HashMap::new();
    for credit in credits {
        if names.contains_key(&credit) {
            continue;
        }
        let name: Option<String> = statement.query_row([credit], |row| row.get(0)).optional()?;
        names.insert(credit, name.unwrap_or_default());
    }
    Ok(names)
}

fn stream(connection: &Connection, listener: Id, today: Today) -> Result<HistoryStream, Error> {
    let revision: i64 =
        connection.query_row("SELECT rev FROM library_meta WHERE id = 1", [], |row| {
            row.get(0)
        })?;
    let library_revision = u64::try_from(revision).unwrap_or(0);
    let listener: Option<i64> = connection
        .query_row(
            "SELECT id FROM listener WHERE public_id = ?1",
            [listener.as_bytes()],
            |row| row.get(0),
        )
        .optional()?;
    let listener = listener.ok_or(Error::NotFound("no such listener"))?;
    let counted = by_day(connection, listener)?;
    let mut per_day: HashMap<i64, u64> = HashMap::new();
    for ((day, _), plays) in &counted {
        *per_day.entry(*day).or_default() += plays;
    }
    let days_left_out = per_day.values().filter(|&&plays| plays > SYNC_DAY).count() as u64;
    let credits = credit_names(
        connection,
        counted.keys().filter_map(|(_, credit)| match credit {
            Credit::Library(credit) => Some(*credit),
            _ => None,
        }),
    )?;

    // Each artist by their name folded, spelled as they were first seen.
    let mut keys: HashMap<String, usize> = HashMap::new();
    let mut names: Vec<String> = Vec::new();
    let mut played: HashMap<(usize, i64), u64> = HashMap::new();
    let mut first: Vec<i64> = Vec::new();
    let mut kept: Vec<((i64, Credit), u64)> = counted
        .into_iter()
        .filter(|((day, _), _)| per_day.get(day).is_none_or(|&plays| plays <= SYNC_DAY))
        .collect();
    // In order, so that who is seen first, and so how a name is spelled,
    // is the same at every reading.
    kept.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    for ((day, credit), plays) in kept {
        let month = month_of(day);
        let name = match credit {
            Credit::Library(credit) => credits.get(&credit).cloned().unwrap_or_default(),
            Credit::Named(name) => name,
            Credit::Nobody => String::new(),
        };
        let artist = *keys.entry(name.to_lowercase()).or_insert_with(|| {
            names.push(name);
            first.push(month);
            names.len() - 1
        });
        if let Some(found) = first.get_mut(artist) {
            *found = (*found).min(month);
        }
        *played.entry((artist, month)).or_default() += plays;
    }
    let Some(&start) = first.iter().min() else {
        return Ok(HistoryStream {
            from: None,
            months: 0,
            total: Vec::new(),
            layers: Vec::new(),
            artists: Vec::new(),
            days_left_out,
            library_revision,
        });
    };
    let end = (i64::from(today.year) * 12 + i64::from(today.month) - 1).max(start);
    let months = usize::try_from(end - start + 1).unwrap_or(0);
    let at = |month: i64| {
        usize::try_from(month - start)
            .ok()
            .filter(|&at| at < months)
    };

    // The layers, by the year each artist was first heard, and in each
    // month the artists of a layer played most.
    let mut total = vec![0; months];
    let mut layers: BTreeMap<i64, Layering> = BTreeMap::new();
    for (&(artist, month), &plays) in &played {
        let (Some(at), Some(&found)) = (at(month), first.get(artist)) else {
            continue;
        };
        let (sums, most) = layers
            .entry(found.div_euclid(12))
            .or_insert_with(|| (vec![0; months], vec![Vec::new(); months]));
        if let (Some(sum), Some(month_total), Some(most)) =
            (sums.get_mut(at), total.get_mut(at), most.get_mut(at))
        {
            *sum += plays;
            *month_total += plays;
            most.push((plays, artist));
        }
    }
    // Only the artists a layer names are sent, each once.
    let mut sent: HashMap<usize, u32> = HashMap::new();
    let mut artists: Vec<String> = Vec::new();
    let layers = layers
        .into_iter()
        .map(|(year, (plays, most))| StreamLayer {
            year: i32::try_from(year).unwrap_or(0),
            plays,
            most: most
                .into_iter()
                .map(|mut month| {
                    month.sort_unstable_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
                    month
                        .into_iter()
                        .take(NAMED)
                        .map(|(_, artist)| {
                            *sent.entry(artist).or_insert_with(|| {
                                artists.push(names.get(artist).cloned().unwrap_or_default());
                                u32::try_from(artists.len() - 1).unwrap_or(0)
                            })
                        })
                        .collect()
                })
                .collect(),
        })
        .collect();
    Ok(HistoryStream {
        from: Some(format!(
            "{:04}-{:02}",
            start.div_euclid(12),
            start.rem_euclid(12) + 1
        )),
        months: u32::try_from(months).unwrap_or(0),
        total,
        layers,
        artists,
        days_left_out,
        library_revision,
    })
}

impl LibraryStore {
    /// `listener`'s whole history as the History tab draws it, up to the
    /// month of `today`.
    pub fn history_stream(&self, listener: Id, today: Today) -> Result<HistoryStream, Error> {
        self.read_snapshot(move |connection| stream(connection, listener, today))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::invent::testing::{NOW, library_drawing};
    use crate::invent::{PlaysPlan, invent_plays};

    #[test]
    fn the_stream_holds_every_counted_play_in_layers_by_the_year_each_artist_was_found() {
        let invented = library_drawing(30);
        let store = &invented.store;
        let listener = store.default_listener().unwrap().unwrap();
        let today = Today {
            now_ms: NOW,
            year: 2026,
            month: 11,
            date: 20261115,
        };
        let empty = store.history_stream(listener, today).unwrap();
        assert_eq!((empty.from, empty.months), (None, 0));

        let plan = PlaysPlan {
            plays: 3_000,
            seed: 5,
            now_ms: NOW,
            years: 6,
            zone: "Europe/London".to_owned(),
        };
        invent_plays(store, &plan, |_, _| {}).unwrap();
        let stream = store.history_stream(listener, today).unwrap();
        let counted: i64 = store
            .read(|connection| {
                Ok(connection.query_row(
                    "SELECT count(*) FROM play_event WHERE counts = 1 AND local_date IS NOT NULL",
                    [],
                    |row| row.get(0),
                )?)
            })
            .unwrap();
        // Every play is in one month and one layer, and no day is so full
        // here as to be left out.
        assert_eq!(stream.days_left_out, 0);
        assert_eq!(
            stream.total.iter().sum::<u64>(),
            u64::try_from(counted).unwrap()
        );
        assert_eq!(stream.total.len(), stream.months as usize);
        for month in 0..stream.months as usize {
            let layered: u64 = stream.layers.iter().map(|layer| layer.plays[month]).sum();
            assert_eq!(layered, stream.total[month], "month {month}");
        }
        // It ends this month and begins with the first play, six years back.
        assert_eq!(stream.from.as_deref().map(|from| &from[..4]), Some("2020"));
        assert!(
            stream
                .layers
                .windows(2)
                .all(|pair| pair[0].year < pair[1].year)
        );
        // A layer has no plays before its year, and names who made it up.
        let start: i32 = stream.from.as_deref().unwrap()[..4].parse().unwrap();
        let start_month: usize = stream.from.as_deref().unwrap()[5..].parse().unwrap();
        for layer in &stream.layers {
            let before = (layer.year - start) as usize * 12;
            let before = before.saturating_sub(start_month - 1);
            assert!(
                layer.plays[..before].iter().all(|&plays| plays == 0),
                "{}",
                layer.year
            );
            for (plays, most) in layer.plays.iter().zip(&layer.most) {
                assert_eq!(*plays == 0, most.is_empty());
                assert!(most.len() <= 3);
                assert!(
                    most.iter()
                        .all(|&artist| (artist as usize) < stream.artists.len())
                );
            }
        }
    }
}
