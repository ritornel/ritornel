//! Spotify's streams in the library: what applying a run of them would do,
//! read without writing; applying it, which stores the streams that are
//! new and records the run; and undoing it. See "Spotify streams" in
//! docs/internals/history.md.

use std::collections::{BTreeMap, HashMap};

use ritornel_contract::{DeviceKind, ImportReport, Platform, PlayedVia, SpotifyStream};
use ritornel_domain::stream_report::{DryRunInput, dry_run, keyed};
use ritornel_domain::streams::{Pins, Scrobble, Stream, kept, repeat_order};
use rusqlite::{Connection, OptionalExtension};
use sha2::{Digest, Sha256};

use crate::history_store::{echo_of, zone_periods};
use crate::library_store::Write;
use crate::{Error, Id, LibraryStore, WriteRequest};

/// What a dry run reads from the library, as of one commit.
struct Read {
    scrobbles: Vec<Scrobble>,
    excluded: u64,
    echoes: u64,
    plays_by_year: BTreeMap<i32, (u64, u64)>,
    listener: i64,
    zone_periods: Vec<ritornel_domain::history::ZonePeriod>,
    stored: HashMap<Vec<u8>, SpotifyStream>,
}

/// The streams stored already, by key, as they were stored.
fn stored_streams(connection: &Connection) -> Result<HashMap<Vec<u8>, SpotifyStream>, Error> {
    let mut statement = connection.prepare(&format!(
        "SELECT {STORED_STREAM} FROM history_observation o {STORED_STREAM_JOINS}"
    ))?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, Vec<u8>>(0)?, stored_stream(row, 1)?))
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// A stored stream's columns, after its source key: what [`stored_stream`]
/// reads, from the observation `o` and [`STORED_STREAM_JOINS`].
pub(crate) const STORED_STREAM: &str = "o.source_key, si.provider_key, i.title, i.artist, i.album,
    s.ended_at_utc_ms, s.ms_played, s.reason_start, s.reason_end, s.shuffle, s.skipped,
    s.offline, s.offline_timestamp, s.incognito";

/// What [`STORED_STREAM`] reads from, joined to the observation `o`.
pub(crate) const STORED_STREAM_JOINS: &str = "JOIN observation_import i ON i.observation_id = o.id
     JOIN observation_import_stream s ON s.observation_id = o.id
     JOIN source_item si ON si.id = o.source_item_id";

/// A stored stream's record from its typed columns, from column `at` on: as
/// it was kept, with nothing of what it was played on. Its stored JSON is
/// the record's archive, and is never read back.
pub(crate) fn stored_stream(row: &rusqlite::Row<'_>, at: usize) -> rusqlite::Result<SpotifyStream> {
    Ok(SpotifyStream {
        track_uri: row.get(at)?,
        title: row.get(at + 1)?,
        artist: row.get(at + 2)?,
        album: row.get(at + 3)?,
        ended_at: row.get(at + 4)?,
        ms_played: row.get(at + 5)?,
        reason_start: row.get(at + 6)?,
        reason_end: row.get(at + 7)?,
        shuffle: row.get(at + 8)?,
        skipped: row.get(at + 9)?,
        offline: row.get(at + 10)?,
        offline_timestamp: row.get(at + 11)?,
        incognito: row.get(at + 12)?,
        platform: None,
        device_kind: None,
        via: None,
    })
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
            observation,
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
        let read = self.read_snapshot(read)?;
        let streams = keyed(records, &read.stored);
        Ok(report(&read, &streams, made_at))
    }
}

/// What a dry run reads, as `connection` sees the library.
fn read(connection: &Connection) -> Result<Read, Error> {
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
        stored: stored_streams(connection)?,
    })
}

/// The dry run's report of these streams, keyed against what was read.
fn report(read: &Read, streams: &[Stream], made_at: i64) -> ImportReport {
    dry_run(&DryRunInput {
        streams,
        scrobbles: &read.scrobbles,
        excluded: read.excluded,
        echoes: read.echoes,
        plays_by_year: &read.plays_by_year,
        listener: read.listener,
        zone_periods: &read.zone_periods,
        pins: &Pins::default(),
        stored: &read.stored,
        made_at,
    })
}

/// The name of the library's Spotify endpoint and source instance. Neither
/// holds an account's name.
const SPOTIFY: &str = "Spotify";

/// The item kind of what a stream was of: a track, by its URI.
const TRACK: &str = "spotify_track";

/// What a write that applies a run is called.
pub const APPLY_METHOD: &str = "imports.apply";

/// What a write that undoes a run is called.
pub const UNDO_METHOD: &str = "imports.undo";

/// A run of streams as the library holds it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredRun {
    /// The run, as it was staged.
    pub run: Id,
    /// The streams its archive held.
    pub streams: i64,
    /// Of those, the ones it stored: the rest were stored by an earlier run.
    pub new_streams: i64,
    /// Whether it is in force.
    pub in_force: bool,
    /// When it was applied.
    pub applied_at_ms: i64,
    /// When it was last undone, if it has been.
    pub undone_at_ms: Option<i64>,
    /// What read the archive.
    pub reader: String,
}

/// The rows of the library's Spotify endpoint and source instance, made
/// when the first run is applied.
fn spotify_rows(write: &Write<'_>) -> Result<(i64, i64), Error> {
    let connection = write.connection();
    let endpoint: Option<i64> = connection
        .query_row(
            "SELECT id FROM history_endpoint WHERE kind = 'import' AND name = ?1",
            [SPOTIFY],
            |row| row.get(0),
        )
        .optional()?;
    let endpoint = match endpoint {
        Some(endpoint) => endpoint,
        None => {
            connection.execute(
                "INSERT INTO history_endpoint (public_id, kind, name, rev)
                 VALUES (?1, 'import', ?2, ?3)",
                (write.new_id()?.as_bytes(), SPOTIFY, write.rev()),
            )?;
            connection.last_insert_rowid()
        }
    };
    let instance: Option<i64> = connection
        .query_row(
            "SELECT id FROM source_instance WHERE kind = 'spotify'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let instance = match instance {
        Some(instance) => instance,
        None => {
            connection.execute(
                "INSERT INTO source_instance (public_id, kind, name, created_at_ms, rev)
                 VALUES (?1, 'spotify', ?2, ?3, ?4)",
                (
                    write.new_id()?.as_bytes(),
                    SPOTIFY,
                    write.now_ms(),
                    write.rev(),
                ),
            )?;
            connection.last_insert_rowid()
        }
    };
    Ok((endpoint, instance))
}

/// The SHA-256 of an archive's records as they are kept, in an order of
/// their own and not by their keys: the same archive has the same hash.
fn records_hash(streams: &[Stream]) -> Vec<u8> {
    // The archive's own records as they are kept, in an order of their
    // own, so that the same archive has the same hash whatever is stored.
    let mut order: Vec<SpotifyStream> = streams.iter().map(|stream| kept(&stream.record)).collect();
    order.sort_by(|a, b| {
        (a.ended_at, a.ms_played, &a.track_uri, repeat_order(a)).cmp(&(
            b.ended_at,
            b.ms_played,
            &b.track_uri,
            repeat_order(b),
        ))
    });
    let mut hash = Sha256::new();
    for record in order {
        let written = serde_json::to_vec(&record).unwrap_or_default();
        hash.update((written.len() as u64).to_be_bytes());
        hash.update(written);
    }
    hash.finalize().to_vec()
}

fn stored_run(connection: &Connection, run: Id) -> Result<Option<StoredRun>, Error> {
    connection
        .query_row(
            "SELECT r.streams, r.new_streams, r.reader,
                    (SELECT min(at_ms) FROM import_run_event WHERE run_id = r.id AND state = 'applied'),
                    (SELECT state FROM import_run_event WHERE run_id = r.id ORDER BY id DESC LIMIT 1),
                    (SELECT max(at_ms) FROM import_run_event WHERE run_id = r.id AND state = 'undone')
               FROM import_run r WHERE r.public_id = ?1",
            [run.as_bytes()],
            |row| {
                Ok(StoredRun {
                    run,
                    streams: row.get(0)?,
                    new_streams: row.get(1)?,
                    reader: row.get(2)?,
                    applied_at_ms: row.get::<_, Option<i64>>(3)?.unwrap_or(0),
                    in_force: row.get::<_, Option<String>>(4)?.as_deref() == Some("applied"),
                    undone_at_ms: row.get(5)?,
                })
            },
        )
        .optional()
        .map_err(Error::from)
}

/// Stores a run's streams that are not stored yet, and records the run, the
/// streams it held and that it is in force, in one write. A run applied
/// before is not applied again. Derivation passes streams over, so no play
/// changes.
fn apply_in(
    write: &Write<'_>,
    run: Id,
    streams: &[Stream],
    reader: &str,
    session: Option<Id>,
) -> Result<StoredRun, Error> {
    let connection = write.connection();
    if let Some(stored) = stored_run(connection, run)? {
        return Ok(stored);
    }
    let (endpoint, instance) = spotify_rows(write)?;
    let listener: i64 = connection
        .query_row("SELECT id FROM listener ORDER BY id LIMIT 1", [], |row| {
            row.get(0)
        })
        .optional()?
        .ok_or(Error::Corrupt("a library with no listener"))?;
    let server: i64 = connection.query_row(
        "SELECT hub_server_id FROM library_meta WHERE id = 1",
        [],
        |row| row.get(0),
    )?;
    let mut held_by_key = connection.prepare_cached(
        "SELECT id FROM history_observation WHERE endpoint_id = ?1 AND source_key = ?2",
    )?;
    let mut item_by_uri = connection.prepare_cached(
        "SELECT id FROM source_item WHERE source_instance_id = ?1 AND item_kind = ?2
           AND provider_key = ?3 AND retired_at_ms IS NULL",
    )?;
    let mut items: HashMap<&str, i64> = HashMap::new();
    let mut observations = Vec::with_capacity(streams.len());
    let mut new_streams = 0_i64;
    for stream in streams {
        if let Some(held) = held_by_key
            .query_row((endpoint, &stream.key), |row| row.get::<_, i64>(0))
            .optional()?
        {
            observations.push(held);
            continue;
        }
        let r = &stream.record;
        let item = match items.get(r.track_uri.as_str()) {
            Some(item) => *item,
            None => {
                let found: Option<i64> = item_by_uri
                    .query_row((instance, TRACK, &r.track_uri), |row| row.get(0))
                    .optional()?;
                let item = match found {
                    Some(item) => item,
                    None => {
                        connection
                            .prepare_cached(
                                "INSERT INTO source_item
                                   (public_id, source_instance_id, item_kind, provider_key,
                                    first_seen_at_ms, rev)
                                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                            )?
                            .execute((
                                write.new_id()?.as_bytes(),
                                instance,
                                TRACK,
                                &r.track_uri,
                                write.now_ms(),
                                write.rev(),
                            ))?;
                        connection.last_insert_rowid()
                    }
                };
                items.insert(r.track_uri.as_str(), item);
                item
            }
        };
        // The record is written from its type, never from text it was
        // handed: the kept fields, and no other. What it was played on is
        // kept apart, where a later reader may say it again.
        let payload = serde_json::to_string(&kept(r))?;
        connection
            .prepare_cached(
                "INSERT INTO history_observation
                   (endpoint_id, source_key, kind, source_item_id, listener_id, captured_at_utc_ms,
                    received_at_utc_ms, zone_basis, payload_json, rev)
                 VALUES (?1, ?2, 'import', ?3, ?4, ?5, ?6, 'none', ?7, ?8)",
            )?
            .execute(rusqlite::params![
                endpoint,
                stream.key,
                item,
                listener,
                r.ended_at,
                write.now_ms(),
                payload,
                write.rev(),
            ])?;
        let observation = connection.last_insert_rowid();
        connection
            .prepare_cached(
                "INSERT INTO observation_import
                   (observation_id, played_at_utc_ms, artist, album, title, rev)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?
            .execute(rusqlite::params![
                observation,
                r.ended_at.saturating_sub(r.ms_played),
                r.artist,
                r.album,
                r.title,
                write.rev(),
            ])?;
        connection
            .prepare_cached(
                "INSERT INTO observation_import_stream
                   (observation_id, ended_at_utc_ms, ms_played, reason_start, reason_end, shuffle,
                    skipped, offline, offline_timestamp, incognito, rev)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            )?
            .execute(rusqlite::params![
                observation,
                r.ended_at,
                r.ms_played,
                r.reason_start,
                r.reason_end,
                r.shuffle,
                r.skipped,
                r.offline,
                r.offline_timestamp,
                r.incognito,
                write.rev(),
            ])?;
        observations.push(observation);
        new_streams += 1;
    }
    connection.execute(
        "INSERT INTO import_run
           (public_id, endpoint_id, applied_by_server_id, session_pid, reader, records_hash,
            streams, new_streams, created_at_ms, rev)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        rusqlite::params![
            run.as_bytes(),
            endpoint,
            server,
            session.as_ref().map(Id::as_bytes),
            reader,
            records_hash(streams),
            streams.len() as i64,
            new_streams,
            write.now_ms(),
            write.rev(),
        ],
    )?;
    let row = connection.last_insert_rowid();
    let mut held = connection.prepare_cached(
        "INSERT INTO import_run_stream (run_id, observation_id, rev) VALUES (?1, ?2, ?3)",
    )?;
    // What each stream was played on is said again by this run's reader,
    // for a stream stored before as for a new one.
    let mut device = connection.prepare_cached(
        "INSERT INTO stream_device (observation_id, platform, device_kind, via, run_id, rev)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT (observation_id) DO UPDATE
           SET platform = excluded.platform, device_kind = excluded.device_kind,
               via = excluded.via, run_id = excluded.run_id, rev = excluded.rev",
    )?;
    for (observation, stream) in observations.iter().zip(streams) {
        held.execute((row, observation, write.rev()))?;
        device.execute(rusqlite::params![
            observation,
            stream.record.platform.map(Platform::as_str),
            stream.record.device_kind.map(DeviceKind::as_str),
            stream.record.via.map(PlayedVia::as_str),
            row,
            write.rev(),
        ])?;
    }
    connection.execute(
        "INSERT INTO import_run_event (run_id, state, at_ms, rev) VALUES (?1, 'applied', ?2, ?3)",
        (row, write.now_ms(), write.rev()),
    )?;
    stored_run(connection, run)?.ok_or(Error::Corrupt("a run that was just applied"))
}

impl LibraryStore {
    /// A run as the library holds it, or `None` if it was never applied.
    pub fn stored_run(&self, run: Id) -> Result<Option<StoredRun>, Error> {
        self.read(move |connection| stored_run(connection, run))
    }

    /// Applies a run of streams: stores those not stored yet and records
    /// the run, in one write, and returns it with the report of what it
    /// did, worked out in the same write from what it saw. A run applied
    /// before writes nothing, and returns what it is with no report. That is looked at before the write is begun, as
    /// one writer of runs, the hub's job worker, is enough to make sure of;
    /// a second writer racing it would find the run inside the write, and
    /// take a revision that changes nothing.
    pub fn apply_streams(
        &self,
        request: WriteRequest,
        run: Id,
        records: Vec<SpotifyStream>,
        reader: String,
        session: Option<Id>,
    ) -> Result<(StoredRun, Option<ImportReport>), Error> {
        if let Some(stored) = self.stored_run(run)? {
            return Ok((stored, None));
        }
        // The report goes beside the write's result, which a command sent
        // again is answered with, and not in it.
        let (sent, report_of) = std::sync::mpsc::channel();
        let written = self.write(request, move |write| {
            // Keyed against what is stored as this write sees it, so that a
            // row a run stored before keeps its key.
            let read = read(write.connection())?;
            let streams = keyed(records, &read.stored);
            let _ = sent.send(report(&read, &streams, write.now_ms()));
            // What the report read is not held while the run is stored.
            drop(read);
            let stored = apply_in(write, run, &streams, &reader, session)?;
            Ok(serde_json::to_string(&stored)?)
        })?;
        Ok((
            serde_json::from_str(&written.result_json)?,
            report_of.try_recv().ok(),
        ))
    }

    /// Takes a run out of force: a row of its ledger, in one write. Its
    /// streams stay stored, and those another run in force holds stay in
    /// force. A run already out of force writes nothing.
    pub fn undo_streams(&self, request: WriteRequest, run: Id) -> Result<StoredRun, Error> {
        let stored = self
            .stored_run(run)?
            .ok_or(Error::NotFound("no run of that ID was applied"))?;
        if !stored.in_force {
            return Ok(stored);
        }
        let written = self.write(request, move |write| {
            let connection = write.connection();
            let row: i64 = connection.query_row(
                "SELECT id FROM import_run WHERE public_id = ?1",
                [run.as_bytes()],
                |row| row.get(0),
            )?;
            let state: Option<String> = connection
                .query_row(
                    "SELECT state FROM import_run_event WHERE run_id = ?1 ORDER BY id DESC LIMIT 1",
                    [row],
                    |row| row.get(0),
                )
                .optional()?;
            if state.as_deref() == Some("applied") {
                connection.execute(
                    "INSERT INTO import_run_event (run_id, state, at_ms, rev)
                     VALUES (?1, 'undone', ?2, ?3)",
                    (row, write.now_ms(), write.rev()),
                )?;
            }
            let stored = stored_run(connection, run)?.ok_or(Error::Corrupt("a run undone"))?;
            Ok(serde_json::to_string(&stored)?)
        })?;
        Ok(serde_json::from_str(&written.result_json)?)
    }

    /// Every run the library holds, newest first.
    pub fn stored_runs(&self) -> Result<Vec<StoredRun>, Error> {
        self.read(|connection| {
            let runs: Vec<[u8; 16]> = connection
                .prepare("SELECT public_id FROM import_run ORDER BY id DESC")?
                .query_map([], |row| row.get(0))?
                .collect::<Result<_, _>>()?;
            runs.into_iter()
                .filter_map(|run| stored_run(connection, Id::from_bytes(run)).transpose())
                .collect()
        })
    }
}
