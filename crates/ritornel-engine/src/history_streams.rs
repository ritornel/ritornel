//! Spotify's streams as derivation reads them: those in force, each
//! track's length as they give it, and, for any import, the streams and
//! scrobbles the stream matcher could join it with, which derivation
//! widens to so that an incremental derive equals a rebuild. See "Spotify
//! streams" in docs/internals/history.md.

use std::collections::{BTreeSet, HashMap};

use ritornel_contract::{LONGEST_STREAM_MS, SpotifyStream};
use ritornel_domain::history::StreamImport;
use ritornel_domain::streams::{
    PARTNER_WITHIN_MS, Stream, Tiers, WITHIN_MS, events, lengths as track_lengths, name_key,
};
use rusqlite::{Connection, OptionalExtension};

use crate::Error;

/// The runs in force: those whose newest ledger entry is `applied`.
macro_rules! runs_in_force {
    () => {
        "SELECT r.id FROM import_run r
         WHERE (SELECT e.state FROM import_run_event e WHERE e.run_id = r.id
                ORDER BY e.id DESC LIMIT 1) = 'applied'"
    };
}

/// The streams in force, as a query: those a run in force held.
pub(crate) const IN_FORCE: &str = concat!(
    "SELECT DISTINCT h.observation_id FROM import_run_stream h WHERE h.run_id IN (",
    runs_in_force!(),
    ")"
);

/// Whether the observation `o` is a stream in force, as a condition that
/// looks it up by its index rather than listing every stream in force.
const HELD: &str = concat!(
    "EXISTS (SELECT 1 FROM import_run_stream h
             WHERE h.observation_id = o.id AND h.run_id IN (",
    runs_in_force!(),
    "))"
);

fn stream_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<(i64, i64, i64, i64, Vec<u8>, String)> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
    ))
}

fn stream_of(
    (observation, endpoint, listener, source_item, key, payload): (
        i64,
        i64,
        i64,
        i64,
        Vec<u8>,
        String,
    ),
) -> Result<StreamImport, Error> {
    Ok(StreamImport {
        observation,
        endpoint,
        listener,
        source_item,
        key,
        record: serde_json::from_str(&payload)?,
    })
}

/// The streams in force with these observation IDs, or every one.
pub(crate) fn streams(
    connection: &Connection,
    only: Option<&BTreeSet<i64>>,
) -> Result<Vec<StreamImport>, Error> {
    const COLUMNS: &str = "o.id, o.endpoint_id, o.listener_id, o.source_item_id, o.source_key,
                           o.payload_json";
    let Some(only) = only else {
        let mut statement = connection.prepare(&format!(
            "SELECT {COLUMNS} FROM ({IN_FORCE}) f
             JOIN history_observation o ON o.id = f.observation_id ORDER BY o.id"
        ))?;
        let rows = statement.query_map([], stream_row)?;
        return rows.map(|row| stream_of(row?)).collect();
    };
    let mut one = connection.prepare_cached(&format!(
        "SELECT {COLUMNS} FROM history_observation o
         WHERE o.id = ?1 AND {HELD}"
    ))?;
    let mut found = Vec::new();
    for observation in only {
        if let Some(row) = one.query_row([observation], stream_row).optional()? {
            found.push(stream_of(row)?);
        }
    }
    Ok(found)
}

/// Each track's length as its streams in force give it, by track URI: of
/// these tracks, or of every one.
pub(crate) fn lengths(
    connection: &Connection,
    tracks: Option<&BTreeSet<String>>,
) -> Result<HashMap<String, i64>, Error> {
    let all = match tracks {
        None => streams(connection, None)?,
        Some(tracks) => {
            let mut of_track = connection.prepare_cached(&format!(
                "SELECT o.id, o.endpoint_id, o.listener_id, o.source_item_id, o.source_key,
                        o.payload_json
                 FROM history_observation o JOIN source_item si ON si.id = o.source_item_id
                 WHERE si.item_kind = 'spotify_track' AND si.provider_key = ?1
                   AND {HELD}"
            ))?;
            let mut found = Vec::new();
            for track in tracks {
                for row in of_track.query_map([track], stream_row)? {
                    found.push(stream_of(row?)?);
                }
            }
            found
        }
    };
    let keyed: Vec<Stream> = all
        .into_iter()
        .map(|stream| Stream {
            key: stream.key,
            record: stream.record,
        })
        .collect();
    Ok(track_lengths(&keyed, &events(&keyed)))
}

/// An import as the neighbourhood reads it: a stream in force, or a
/// scrobble of an account, by its time and names.
enum Near {
    Stream(SpotifyStream),
    Scrobble { at_ms: i64, names: (String, String) },
}

fn near(connection: &Connection, observation: i64) -> Result<Option<Near>, Error> {
    if let Some(stream) = streams(connection, Some(&BTreeSet::from([observation])))?.pop() {
        return Ok(Some(Near::Stream(stream.record)));
    }
    let scrobble: Option<(i64, String, String)> = connection
        .prepare_cached(
            "SELECT i.played_at_utc_ms, i.artist, i.title FROM observation_import i
             JOIN history_observation o ON o.id = i.observation_id
             JOIN service_account a ON a.endpoint_id = o.endpoint_id
             WHERE i.observation_id = ?1",
        )?
        .query_row([observation], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .optional()?;
    Ok(scrobble.map(|(at_ms, artist, title)| Near::Scrobble {
        at_ms,
        names: (name_key(&artist), name_key(&title)),
    }))
}

/// Whether a scrobble of these names at `at_ms` could be joined to this
/// stream by a tier in force, or is a record of its names close enough
/// to decide whether tier C may join something else.
fn could_join(stream: &SpotifyStream, at_ms: i64, names: &(String, String), tiers: Tiers) -> bool {
    let own = (name_key(&stream.artist), name_key(&stream.title));
    let start = stream.ended_at.saturating_sub(stream.ms_played);
    let apart = (i128::from(at_ms) - i128::from(start)).abs();
    if own == *names {
        if apart <= i128::from(WITHIN_MS) {
            return true;
        }
        if tiers.during && at_ms >= start && at_ms <= stream.ended_at.saturating_add(WITHIN_MS) {
            return true;
        }
        if tiers.renamed && apart <= i128::from(PARTNER_WITHIN_MS) {
            return true;
        }
        return false;
    }
    tiers.renamed && apart <= i128::from(WITHIN_MS) && (own.0 == names.0 || own.1 == names.1)
}

/// How far from an import its partners can be, by the tiers in force.
fn reach(tiers: Tiers) -> i64 {
    let mut reach = WITHIN_MS;
    if tiers.during {
        reach = reach.max(LONGEST_STREAM_MS.saturating_add(WITHIN_MS));
    }
    if tiers.renamed {
        reach = reach.max(PARTNER_WITHIN_MS);
    }
    reach
}

/// The streams and scrobbles an import could be joined with by the tiers
/// in force, and a stream's repeats: what deriving it again must derive
/// with it, so that the matcher sees all it would see in a rebuild.
pub(crate) fn neighbours(connection: &Connection, observation: i64) -> Result<Vec<i64>, Error> {
    let tiers = Tiers::CURRENT;
    let Some(this) = near(connection, observation)? else {
        return Ok(Vec::new());
    };
    let reach = reach(tiers);
    let (from, to) = match &this {
        Near::Stream(record) => {
            let start = record.ended_at.saturating_sub(record.ms_played);
            (
                start.saturating_sub(reach),
                record.ended_at.saturating_add(reach),
            )
        }
        Near::Scrobble { at_ms, .. } => (at_ms.saturating_sub(reach), at_ms.saturating_add(reach)),
    };
    // Every import whose stored time, a stream's worked-out start or a
    // scrobble's stamp, falls in the window.
    let around: Vec<i64> = connection
        .prepare_cached(
            "SELECT observation_id FROM observation_import
             WHERE played_at_utc_ms BETWEEN ?1 AND ?2 AND observation_id <> ?3",
        )?
        .query_map((from, to, observation), |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    let mut found = Vec::new();
    for other in around {
        let Some(that) = near(connection, other)? else {
            continue;
        };
        let joins = match (&this, &that) {
            (Near::Stream(record), Near::Scrobble { at_ms, names })
            | (Near::Scrobble { at_ms, names }, Near::Stream(record)) => {
                could_join(record, *at_ms, names, tiers)
            }
            // A stream's repeats are one event with it.
            (Near::Stream(a), Near::Stream(b)) => {
                (a.ended_at, a.ms_played, &a.track_uri) == (b.ended_at, b.ms_played, &b.track_uri)
            }
            (Near::Scrobble { .. }, Near::Scrobble { .. }) => false,
        };
        if joins {
            found.push(other);
        }
    }
    Ok(found)
}
