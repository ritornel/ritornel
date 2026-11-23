//! Spotify's streams in the library: what applying a run of them would do,
//! read without writing. See "Spotify streams" in docs/internals/history.md.

use std::collections::BTreeMap;

use ritornel_contract::{ImportReport, SpotifyStream};
use ritornel_domain::stream_report::{DryRunInput, dry_run, keyed};
use ritornel_domain::streams::{Pins, Scrobble};
use rusqlite::Connection;

use crate::history_store::{echo_of, zone_periods};
use crate::{Error, LibraryStore};

/// What a dry run reads from the library, as of one commit.
struct Read {
    scrobbles: Vec<Scrobble>,
    excluded: u64,
    echoes: u64,
    plays_by_year: BTreeMap<i32, (u64, u64)>,
    listener: i64,
    zone_periods: Vec<ritornel_domain::history::ZonePeriod>,
}

/// Every scrobble of every account, sorted into those a stream may take,
/// those set aside and those that are this library's own listens come back.
/// An import is an echo when it matches a delivery of a listen, as
/// derivation finds it, so that a second copy of an echo, which stands as a
/// play of its own, is an echo too; and it is set aside when a correction
/// that stands excludes it.
fn scrobbles(connection: &Connection) -> Result<(Vec<Scrobble>, u64, u64), Error> {
    // Set aside as derivation has it: by the corrections that stand, and
    // never by what a play of it shows, which another record can decide.
    let set_aside =
        ritornel_domain::history::excluded(&crate::history_store::corrections(connection)?);
    let mut statement = connection.prepare(
        "SELECT o.id, o.source_key, i.played_at_utc_ms, i.artist, i.title, o.endpoint_id
           FROM observation_import i
           JOIN history_observation o ON o.id = i.observation_id
           JOIN service_account a ON a.endpoint_id = o.endpoint_id",
    )?;
    let mut rows = statement.query([])?;
    let (mut offered, mut excluded, mut echoes) = (Vec::new(), 0, 0);
    while let Some(row) = rows.next()? {
        let observation: i64 = row.get(0)?;
        let (at_ms, artist, title): (i64, String, String) = (row.get(2)?, row.get(3)?, row.get(4)?);
        if echo_of(connection, row.get(5)?, at_ms, &artist, &title)?.is_some() {
            echoes += 1;
            continue;
        }
        if set_aside.contains(&observation) {
            excluded += 1;
            continue;
        }
        // Keyed as derivation keys it, so that a tie goes the same way.
        offered.push(Scrobble {
            key: row.get(1)?,
            at_ms,
            artist,
            title,
        });
    }
    Ok((offered, excluded, echoes))
}

/// The plays now, and of those the ones that count, by the year of their
/// start in UTC. A play set aside is not one.
fn plays_by_year(connection: &Connection) -> Result<BTreeMap<i32, (u64, u64)>, Error> {
    let mut statement = connection.prepare(
        "SELECT CAST(strftime('%Y', event_at_utc_ms / 1000, 'unixepoch') AS INTEGER),
                count(*), sum(counts)
           FROM play_event
          WHERE excluded = 0 AND event_at_utc_ms IS NOT NULL
          GROUP BY 1",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, Option<i32>>(0)?.unwrap_or(0),
            (row.get::<_, i64>(1)?, row.get::<_, Option<i64>>(2)?),
        ))
    })?;
    let mut by_year = BTreeMap::new();
    for row in rows {
        let (year, (plays, counted)) = row?;
        let unsigned = |n: i64| u64::try_from(n).unwrap_or(0);
        by_year.insert(year, (unsigned(plays), unsigned(counted.unwrap_or(0))));
    }
    Ok(by_year)
}

impl LibraryStore {
    /// What applying these streams would do: which scrobble each joins, how
    /// many count, and what changes by year. It reads the library as of one
    /// commit and writes nothing.
    pub fn stream_dry_run(&self, records: Vec<SpotifyStream>) -> Result<ImportReport, Error> {
        let made_at = self.clock().now_ms();
        let read = self.read_snapshot(|connection| {
            let (scrobbles, excluded, echoes) = scrobbles(connection)?;
            let listener = connection.query_row(
                "SELECT coalesce((SELECT id FROM listener ORDER BY id LIMIT 1), 0)",
                [],
                |row| row.get(0),
            )?;
            Ok(Read {
                scrobbles,
                excluded,
                echoes,
                plays_by_year: plays_by_year(connection)?,
                listener,
                zone_periods: zone_periods(connection)?,
            })
        })?;
        let streams = keyed(records);
        Ok(dry_run(&DryRunInput {
            streams: &streams,
            scrobbles: &read.scrobbles,
            excluded: read.excluded,
            echoes: read.echoes,
            plays_by_year: &read.plays_by_year,
            listener: read.listener,
            zone_periods: &read.zone_periods,
            pins: &Pins::default(),
            made_at,
        }))
    }
}
