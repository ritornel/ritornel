//! The export of the record: what was heard, said and corrected, in files
//! a program that knows nothing of Ritornel can read. See "Export" in
//! docs/design/history.md.
//!
//! ```text
//! manifest.json        what this is: its format and version, the library, and
//!                      the endpoints, listeners and sources the lines name
//! observations.jsonl   every observation, oldest first: a listen, an import
//!                      record or a correction, with the record as it was received
//! ratings.jsonl        every rating and love as it was said, a clearing included
//! plays.csv            the plays derived from them, as a plain table
//! ```
//!
//! An observation is named by its endpoint and its source key, never by
//! the ID of a play, which a rebuild can change (decision 4). The
//! observations and the ratings are the record; the plays are derived
//! from it, by the rule the manifest names, and are there to be used at
//! once. What the library calls a track now (its title, artist and album)
//! is given beside each line for a reader's convenience, and is not part
//! of the record.
//!
//! Every file is read in one read transaction on a connection of its own,
//! so the export is of one moment and holds up neither the writer nor the
//! readers.

use std::io::{BufWriter, Write};
use std::path::Path;

use rusqlite::{Connection, OpenFlags, Row};
use serde_json::{Value, json};

use crate::{Error, LibraryStore};

/// The format's name, in the manifest.
pub const EXPORT_FORMAT: &str = "ritornel-record";

/// The format's version. A change a reader of version 1 would misread is
/// version 2.
pub const EXPORT_VERSION: u32 = 1;

/// How many of each were written.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Exported {
    pub observations: u64,
    pub ratings: u64,
    pub plays: u64,
}

impl LibraryStore {
    /// Writes the record into the folder `into`, which must exist and be
    /// empty, as of the last commit before this began. `now_ms` is the time
    /// the manifest gives.
    pub fn export_record(&self, into: &Path, now_ms: i64) -> Result<Exported, Error> {
        let connection = self.location().open(OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        connection.execute_batch("BEGIN")?;
        let exported = Exported {
            observations: observations(&connection, &into.join("observations.jsonl"))?,
            ratings: ratings(&connection, &into.join("ratings.jsonl"))?,
            plays: plays(&connection, &into.join("plays.csv"))?,
        };
        let manifest = manifest(&connection, self.library_id(), now_ms, exported)?;
        connection.execute_batch("COMMIT")?;
        let mut file = create(&into.join("manifest.json"))?;
        serde_json::to_writer_pretty(&mut file, &manifest)?;
        finish(file)?;
        Ok(exported)
    }
}

fn create(path: &Path) -> Result<BufWriter<std::fs::File>, Error> {
    Ok(BufWriter::new(
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(Error::Io)?,
    ))
}

fn finish(file: BufWriter<std::fs::File>) -> Result<(), Error> {
    file.into_inner()
        .map_err(|error| Error::Io(error.into_error()))?
        .sync_all()
        .map_err(Error::Io)
}

fn line(file: &mut BufWriter<std::fs::File>, value: &Value) -> Result<(), Error> {
    serde_json::to_writer(&mut *file, value)?;
    file.write_all(b"\n").map_err(Error::Io)
}

/// Sets a field of a JSON object.
fn put(value: &mut Value, field: &str, to: Value) {
    if let Some(object) = value.as_object_mut() {
        object.insert(field.to_owned(), to);
    }
}

/// A public ID as its UUID, or a key of any other length as hex.
fn key(bytes: Option<Vec<u8>>) -> Value {
    match bytes {
        Some(bytes) => match <[u8; 16]>::try_from(bytes.as_slice()) {
            Ok(id) => Value::String(crate::Id::from_bytes(id).to_string()),
            Err(_) => Value::String(hex(&bytes)),
        },
        None => Value::Null,
    }
}

/// Bytes as lower-case hex, always: a source key is shown so whatever its
/// length.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Unix milliseconds as UTC, to the millisecond.
fn utc(ms: Option<i64>) -> Value {
    ms.and_then(|ms| jiff::Timestamp::from_millisecond(ms).ok())
        .map_or(Value::Null, |at| Value::String(at.to_string()))
}

/// `YYYYMMDD` as `YYYY-MM-DD`.
fn date(day: Option<i64>) -> Value {
    day.map_or(Value::Null, |day| {
        Value::String(format!(
            "{:04}-{:02}-{:02}",
            day / 10_000,
            day / 100 % 100,
            day % 100
        ))
    })
}

fn manifest(
    connection: &Connection,
    library_id: crate::Id,
    now_ms: i64,
    exported: Exported,
) -> Result<Value, Error> {
    let list = |sql: &str, row: fn(&Row<'_>) -> rusqlite::Result<Value>| -> Result<Value, Error> {
        let mut statement = connection.prepare(sql)?;
        let rows = statement.query_map([], row)?;
        Ok(Value::Array(rows.collect::<Result<_, _>>()?))
    };
    // The rules the plays were derived by: more than one before a
    // rebuild has caught up.
    let versions = |column: &str| -> Result<Vec<i64>, Error> {
        let mut statement = connection.prepare(&format!(
            "SELECT DISTINCT {column} FROM play_event ORDER BY {column}"
        ))?;
        let rows = statement.query_map([], |row| row.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    };
    Ok(json!({
        "format": EXPORT_FORMAT,
        "version": EXPORT_VERSION,
        "exportedAt": utc(Some(now_ms)),
        "libraryId": library_id.to_string(),
        "files": {
            "observations.jsonl": { "lines": exported.observations },
            "ratings.jsonl": { "lines": exported.ratings },
            "plays.csv": {
                "rows": exported.plays,
                "playRuleVersions": versions("play_rule_version")?,
                "grouperVersions": versions("grouper_version")?,
            },
        },
        "servers": list(
            "SELECT public_id, name FROM server ORDER BY id",
            |row| Ok(json!({ "id": key(row.get(0)?), "name": row.get::<_, String>(1)? })),
        )?,
        "endpoints": list(
            "SELECT public_id, kind, name FROM history_endpoint ORDER BY id",
            |row| Ok(json!({ "id": key(row.get(0)?), "kind": row.get::<_, String>(1)?, "name": row.get::<_, String>(2)? })),
        )?,
        "listeners": list(
            "SELECT public_id, name FROM listener ORDER BY id",
            |row| Ok(json!({ "id": key(row.get(0)?), "name": row.get::<_, String>(1)? })),
        )?,
        "sources": list(
            "SELECT public_id, kind, name FROM source_instance ORDER BY id",
            |row| Ok(json!({ "id": key(row.get(0)?), "kind": row.get::<_, String>(1)?, "name": row.get::<_, String>(2)? })),
        )?,
        // Where each listener was, as they said: what a play with no zone
        // of its own is dated in.
        "zonePeriods": list(
            "SELECT l.public_id, z.zone_name, z.valid_from_ms, z.valid_to_ms
             FROM listener_zone_period z JOIN listener l ON l.id = z.listener_id
             ORDER BY z.listener_id, z.valid_from_ms IS NOT NULL, z.valid_from_ms, z.id",
            |row| Ok(json!({
                "listener": key(row.get(0)?),
                "zone": row.get::<_, String>(1)?,
                "from": utc(row.get(2)?),
                "until": utc(row.get(3)?),
            })),
        )?,
    }))
}

/// A source item: a file by its path under its source's root, or what a
/// service's play or love was of, by the text the service gave, which is
/// what its key holds.
fn item_of(id: Option<Vec<u8>>, kind: String, key_text: String, source: Option<Vec<u8>>) -> Value {
    let subject = (kind == "scrobble_subject")
        .then(|| serde_json::from_str::<(String, Option<String>, String)>(&key_text).ok())
        .flatten();
    let mut value = json!({
        "id": key(id),
        "kind": kind,
        "key": key_text,
        "source": key(source),
    });
    if let Some((artist, album, title)) = subject {
        put(
            &mut value,
            "subject",
            json!({ "artist": artist, "album": album, "title": title }),
        );
    }
    value
}

/// The record as it was received, which is JSON; text that is not is
/// given as text.
fn record(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.to_owned()))
}

fn observations(connection: &Connection, path: &Path) -> Result<u64, Error> {
    let mut file = create(path)?;
    let mut statement = connection.prepare(
        "SELECT o.id, e.public_id, o.source_key, o.kind, l.public_id,
                o.captured_at_utc_ms, o.received_at_utc_ms, o.sent_at_utc_ms, o.clock_skew_ms,
                o.zone_name, o.zone_basis, o.utc_offset_minutes, o.local_date, o.local_hour,
                o.payload_json,
                si.public_id, si.item_kind, si.provider_key, src.public_id,
                rec.public_id, rec.display_title, credit.display_text,
                o.sent_mono_ms, o.sent_boot_id
         FROM history_observation o
         JOIN history_endpoint e ON e.id = o.endpoint_id
         JOIN listener l ON l.id = o.listener_id
         LEFT JOIN source_item si ON si.id = o.source_item_id
         LEFT JOIN source_instance src ON src.id = si.source_instance_id
         LEFT JOIN recording bound ON bound.id = si.current_recording_id
         LEFT JOIN recording rec ON rec.id = bound.canonical_id
         LEFT JOIN artist_credit credit ON credit.id = rec.display_artist_credit_id
         ORDER BY o.id",
    )?;
    let mut rows = statement.query([])?;
    let mut written = 0;
    while let Some(row) = rows.next()? {
        let id: i64 = row.get(0)?;
        let kind: String = row.get(3)?;
        let mut value = json!({
            "endpoint": key(row.get(1)?),
            "sourceKey": hex(&row.get::<_, Vec<u8>>(2)?),
            "kind": kind,
            "listener": key(row.get(4)?),
            "capturedAt": utc(row.get(5)?),
            "receivedAt": utc(row.get(6)?),
            "sentAt": utc(row.get(7)?),
            // What a listen's age is read from: the sender's monotonic clock
            // at the send, and the boot it counts from.
            "sentMonoMs": row.get::<_, Option<i64>>(22)?,
            "sentBoot": key(row.get(23)?),
            "clockSkewMs": row.get::<_, Option<i64>>(8)?,
            "zone": {
                "name": row.get::<_, Option<String>>(9)?,
                "basis": row.get::<_, String>(10)?,
                "utcOffsetMinutes": row.get::<_, Option<i64>>(11)?,
            },
            "localDate": date(row.get(12)?),
            "localHour": row.get::<_, Option<i64>>(13)?,
            "record": record(&row.get::<_, String>(14)?),
        });
        let item: Option<Vec<u8>> = row.get(15)?;
        if item.is_some() {
            put(
                &mut value,
                "item",
                item_of(item, row.get(16)?, row.get(17)?, row.get(18)?),
            );
            put(
                &mut value,
                "now",
                json!({
                    "recording": key(row.get(19)?),
                    "title": row.get::<_, Option<String>>(20)?,
                    "artist": row.get::<_, Option<String>>(21)?,
                }),
            );
        }
        match kind.as_str() {
            "listen" => put(&mut value, "listen", listen(connection, id)?),
            "import" => put(&mut value, "import", import(connection, id)?),
            "correction" => put(&mut value, "correction", correction(connection, id)?),
            _ => {}
        }
        line(&mut file, &value)?;
        written += 1;
    }
    finish(file)?;
    Ok(written)
}

fn listen(connection: &Connection, observation: i64) -> Result<Value, Error> {
    let mut statement = connection.prepare_cached(
        "SELECT s.public_id, ol.actor_session_pid, ol.copy_kind,
                ol.started_at_utc_ms, ol.ended_at_utc_ms, ol.start_reason, ol.end_reason,
                ol.continues_source_key, ol.start_position_ms, ol.end_position_ms,
                ol.max_position_ms, ol.track_duration_ms, ol.ms_played, ol.heard_ranges_json,
                ol.heard_ranges_approximate, ol.seek_count, ol.pause_count,
                ol.context_kind, ol.context_ref, ol.shuffle,
                coalesce(rel.display_title, art.display_name),
                ta.display_title, queued_release.display_title
         FROM observation_listen ol
         JOIN server s ON s.id = ol.played_on_server_id
         LEFT JOIN release rel ON ol.context_kind = 'album' AND rel.public_id = ol.context_ref
         LEFT JOIN artist art ON ol.context_kind = 'artist' AND art.public_id = ol.context_ref
         LEFT JOIN track_appearance ta ON ta.id = ol.queued_appearance_id
         LEFT JOIN release queued_release ON queued_release.id = ta.release_id
         WHERE ol.observation_id = ?1",
    )?;
    Ok(statement.query_row([observation], |row| {
        Ok(json!({
            "playedOn": key(row.get(0)?),
            "session": key(row.get(1)?),
            "copy": row.get::<_, String>(2)?,
            "startedAt": utc(row.get(3)?),
            "endedAt": utc(row.get(4)?),
            "startReason": row.get::<_, String>(5)?,
            "endReason": row.get::<_, String>(6)?,
            "continues": row.get::<_, Option<Vec<u8>>>(7)?.map(|bytes| hex(&bytes)),
            "startPositionMs": row.get::<_, i64>(8)?,
            "endPositionMs": row.get::<_, i64>(9)?,
            "maxPositionMs": row.get::<_, i64>(10)?,
            "trackDurationMs": row.get::<_, i64>(11)?,
            "msPlayed": row.get::<_, i64>(12)?,
            "heardRanges": record(&row.get::<_, String>(13)?),
            "heardRangesApproximate": row.get::<_, bool>(14)?,
            "seekCount": row.get::<_, i64>(15)?,
            "pauseCount": row.get::<_, i64>(16)?,
            "playedFrom": {
                "kind": row.get::<_, String>(17)?,
                "id": key(row.get(18)?),
                "title": row.get::<_, Option<String>>(20)?,
            },
            "shuffle": row.get::<_, bool>(19)?,
            "queued": {
                "title": row.get::<_, Option<String>>(21)?,
                "album": row.get::<_, Option<String>>(22)?,
            },
        }))
    })?)
}

fn import(connection: &Connection, observation: i64) -> Result<Value, Error> {
    let mut statement = connection.prepare_cached(
        "SELECT played_at_utc_ms, artist, album, title, artist_mbid, album_mbid, track_mbid
         FROM observation_import WHERE observation_id = ?1",
    )?;
    Ok(statement.query_row([observation], |row| {
        Ok(json!({
            "playedAt": utc(row.get(0)?),
            "artist": row.get::<_, String>(1)?,
            "album": row.get::<_, Option<String>>(2)?,
            "title": row.get::<_, String>(3)?,
            "artistMbid": row.get::<_, Option<String>>(4)?,
            "albumMbid": row.get::<_, Option<String>>(5)?,
            "trackMbid": row.get::<_, Option<String>>(6)?,
        }))
    })?)
}

fn correction(connection: &Connection, observation: i64) -> Result<Value, Error> {
    let mut statement = connection.prepare_cached(
        "SELECT c.verb, l.public_id, e.public_id, c.target_from_utc_ms, c.target_to_utc_ms,
                c.clock_offset_ms, c.note, c.actor_session_pid,
                cr.correction_id IS NOT NULL, rec.public_id, now.public_id, ta.public_id,
                si.public_id,
                CASE WHEN r_ta.canonical_id = rec.canonical_id THEN ta_now.public_id ELSE NULL END
         FROM observation_correction c
         LEFT JOIN listener l ON l.id = c.set_listener_id
         LEFT JOIN history_endpoint e ON e.id = c.target_endpoint_id
         LEFT JOIN correction_recording cr ON cr.correction_id = c.observation_id
         LEFT JOIN recording rec ON rec.id = cr.recording_id
         LEFT JOIN recording now ON now.id = rec.canonical_id
         LEFT JOIN track_appearance ta ON ta.id = cr.appearance_id
         LEFT JOIN track_appearance ta_now ON ta_now.id = ta.canonical_id
         LEFT JOIN recording r_ta ON r_ta.id = ta_now.recording_id
         LEFT JOIN source_item si ON si.id = cr.source_item_id
         WHERE c.observation_id = ?1",
    )?;
    let mut value = statement.query_row([observation], |row| {
        let (endpoint, from, to): (Option<Vec<u8>>, Option<i64>, Option<i64>) =
            (row.get(2)?, row.get(3)?, row.get(4)?);
        let has_recording: bool = row.get(8)?;
        let mut object = json!({
            "verb": row.get::<_, String>(0)?,
            "setListener": key(row.get(1)?),
            "window": endpoint.map(|endpoint| json!({
                "endpoint": key(Some(endpoint)),
                "from": utc(from),
                "to": utc(to),
            })),
            "clockOffsetMs": row.get::<_, Option<i64>>(5)?,
            "note": row.get::<_, Option<String>>(6)?,
            "session": key(row.get(7)?),
        });
        if has_recording {
            put(
                &mut object,
                "setRecording",
                json!({
                    "recording": key(row.get(9)?),
                    "appearance": key(row.get(11)?),
                    "sourceItem": key(row.get(12)?),
                    "now": {
                        "recording": key(row.get(10)?),
                        "appearance": key(row.get(13)?),
                    },
                }),
            );
        }
        Ok(object)
    })?;
    let mut targets = connection.prepare_cached(
        "SELECT e.public_id, o.source_key FROM correction_target t
         JOIN history_observation o ON o.id = t.target_observation_id
         JOIN history_endpoint e ON e.id = o.endpoint_id
         WHERE t.correction_id = ?1 ORDER BY t.id",
    )?;
    let named = targets.query_map([observation], |row| {
        Ok(json!({
            "endpoint": key(row.get(0)?),
            "sourceKey": hex(&row.get::<_, Vec<u8>>(1)?),
        }))
    })?;
    put(
        &mut value,
        "targets",
        Value::Array(named.collect::<Result<_, _>>()?),
    );
    Ok(value)
}

fn ratings(connection: &Connection, path: &Path) -> Result<u64, Error> {
    let mut file = create(path)?;
    let mut statement = connection.prepare(
        "SELECT l.public_id, r.kind, r.value, r.cleared, r.rated_at_ms,
                rec.public_id, rec.display_title, credit.display_text,
                si.public_id, si.item_kind, si.provider_key, src.public_id,
                e.public_id, r.actor_kind, r.actor_session_pid,
                now.public_id, now.display_title, now_credit.display_text
         FROM listener_rating r
         JOIN listener l ON l.id = r.listener_id
         LEFT JOIN recording rec ON rec.id = r.recording_id
         LEFT JOIN artist_credit credit ON credit.id = rec.display_artist_credit_id
         LEFT JOIN source_item si ON si.id = r.source_item_id
         LEFT JOIN source_instance src ON src.id = si.source_instance_id
         LEFT JOIN history_endpoint e ON e.id = r.endpoint_id
         -- What it is of now: the recording it named, or its item's, as
         -- merges have left it.
         LEFT JOIN recording bound ON bound.id = coalesce(r.recording_id, si.current_recording_id)
         LEFT JOIN recording now ON now.id = bound.canonical_id
         LEFT JOIN artist_credit now_credit ON now_credit.id = now.display_artist_credit_id
         ORDER BY r.id",
    )?;
    let mut rows = statement.query([])?;
    let mut written = 0;
    while let Some(row) = rows.next()? {
        let recording: Option<Vec<u8>> = row.get(5)?;
        let item: Option<Vec<u8>> = row.get(8)?;
        let value = json!({
            "listener": key(row.get(0)?),
            "kind": row.get::<_, String>(1)?,
            "value": row.get::<_, Option<i64>>(2)?,
            "cleared": row.get::<_, bool>(3)?,
            "at": utc(row.get(4)?),
            "recording": recording.is_some().then(|| -> rusqlite::Result<Value> {
                Ok(json!({
                    "id": key(recording.clone()),
                    "title": row.get::<_, Option<String>>(6)?,
                    "artist": row.get::<_, Option<String>>(7)?,
                }))
            }).transpose()?,
            "item": item.is_some().then(|| -> rusqlite::Result<Value> {
                Ok(item_of(item.clone(), row.get(9)?, row.get(10)?, row.get(11)?))
            }).transpose()?,
            "endpoint": key(row.get(12)?),
            "by": row.get::<_, Option<String>>(13)?,
            "session": key(row.get(14)?),
            "now": {
                "recording": key(row.get(15)?),
                "title": row.get::<_, Option<String>>(16)?,
                "artist": row.get::<_, Option<String>>(17)?,
            },
        });
        line(&mut file, &value)?;
        written += 1;
    }
    finish(file)?;
    Ok(written)
}

/// The columns of `plays.csv`.
const PLAY_COLUMNS: [&str; 18] = [
    "play",
    "listener",
    "played_at_utc",
    "local_date",
    "zone",
    "artist",
    "title",
    "album",
    "ms_played",
    "duration_ms",
    "outcome",
    "counts",
    "excluded",
    "played_from",
    "source",
    "endpoint",
    "source_key",
    "corroborated_by",
];

fn plays(connection: &Connection, path: &Path) -> Result<u64, Error> {
    let mut file = create(path)?;
    csv_row(
        &mut file,
        PLAY_COLUMNS.iter().map(|column| (*column).to_owned()),
    )?;
    let mut statement = connection.prepare(
        "SELECT p.public_id, l.public_id, p.event_at_utc_ms, p.local_date, p.zone_name,
                coalesce(credit.display_text, imported.artist),
                coalesce(ta.display_title, queued.display_title, rec.display_title, imported.title),
                coalesce(rel.display_title, queued_release.display_title, imported.album),
                p.ms_played, p.track_duration_ms, p.outcome, p.counts, p.excluded,
                p.context_kind, e.kind, e.public_id, o.source_key,
                (SELECT group_concat(hex(ce.public_id) || ':' || hex(co.source_key), ' ')
                 FROM play_event_observation c
                 JOIN history_observation co ON co.id = c.observation_id
                 JOIN history_endpoint ce ON ce.id = co.endpoint_id
                 WHERE c.play_event_id = p.id AND c.role = 'corroborating')
         FROM play_event p
         JOIN listener l ON l.id = p.listener_id
         -- Every play has a primary observation, and one that somehow had
         -- none is still a row of the table, with its source left empty.
         LEFT JOIN play_event_observation primary_one
           ON primary_one.play_event_id = p.id AND primary_one.role = 'primary'
         LEFT JOIN history_observation o ON o.id = primary_one.observation_id
         LEFT JOIN history_endpoint e ON e.id = o.endpoint_id
         LEFT JOIN observation_import imported ON imported.observation_id = o.id
         -- The track as the library shows it: the play's own appearance,
         -- or for one not bound, the appearance its listen queued, before
         -- the recording's own title and credit.
         LEFT JOIN observation_listen heard ON heard.observation_id = o.id
         LEFT JOIN track_appearance queued
           ON p.recording_id IS NULL AND queued.id = heard.queued_appearance_id
         LEFT JOIN release queued_release ON queued_release.id = queued.release_id
         LEFT JOIN recording rec ON rec.id = p.recording_id
         LEFT JOIN track_appearance ta ON ta.id = p.appearance_id
         LEFT JOIN release rel ON rel.id = ta.release_id
         LEFT JOIN artist_credit credit ON credit.id = coalesce(
           ta.display_artist_credit_id, queued.display_artist_credit_id,
           rec.display_artist_credit_id)
         ORDER BY p.event_at_utc_ms IS NULL, p.event_at_utc_ms, p.id",
    )?;
    let mut rows = statement.query([])?;
    let mut written = 0;
    while let Some(row) = rows.next()? {
        let text = |value: Value| match value {
            Value::String(text) => text,
            Value::Null => String::new(),
            other => other.to_string(),
        };
        let number = |value: Option<i64>| value.map_or_else(String::new, |n| n.to_string());
        let flag = |value: bool| if value { "1" } else { "0" }.to_owned();
        let cells = [
            text(key(row.get(0)?)),
            text(key(row.get(1)?)),
            text(utc(row.get(2)?)),
            text(date(row.get(3)?)),
            row.get::<_, Option<String>>(4)?.unwrap_or_default(),
            row.get::<_, Option<String>>(5)?.unwrap_or_default(),
            row.get::<_, Option<String>>(6)?.unwrap_or_default(),
            row.get::<_, Option<String>>(7)?.unwrap_or_default(),
            number(row.get(8)?),
            number(row.get(9)?),
            row.get::<_, String>(10)?,
            flag(row.get(11)?),
            flag(row.get(12)?),
            row.get::<_, Option<String>>(13)?.unwrap_or_default(),
            row.get::<_, Option<String>>(14)?.unwrap_or_default(),
            text(key(row.get(15)?)),
            row.get::<_, Option<Vec<u8>>>(16)?
                .map(|bytes| hex(&bytes))
                .unwrap_or_default(),
            row.get::<_, Option<String>>(17)?
                .map(|also| named_observations(&also))
                .unwrap_or_default(),
        ];
        csv_row(&mut file, cells.into_iter())?;
        written += 1;
    }
    finish(file)?;
    Ok(written)
}

/// The observations a play's `corroborated_by` names, as SQLite lists them,
/// `<endpoint hex>:<source key hex>` apart by spaces, each as
/// `<endpoint ID>:<source key>`, as the lines of observations.jsonl name
/// them.
fn named_observations(listed: &str) -> String {
    listed
        .split(' ')
        .filter_map(|one| {
            let (endpoint, source_key) = one.split_once(':')?;
            let bytes: Vec<u8> = (0..endpoint.len())
                .step_by(2)
                .filter_map(|at| u8::from_str_radix(endpoint.get(at..at + 2)?, 16).ok())
                .collect();
            let endpoint = match key(Some(bytes)) {
                Value::String(text) => text,
                _ => return None,
            };
            Some(format!("{endpoint}:{}", source_key.to_lowercase()))
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// One row of RFC 4180 CSV: a cell with a comma, a quote or a line break
/// is quoted, with its quotes doubled.
fn csv_row(
    file: &mut BufWriter<std::fs::File>,
    cells: impl Iterator<Item = String>,
) -> Result<(), Error> {
    let mut row = String::new();
    for (n, cell) in cells.enumerate() {
        if n > 0 {
            row.push(',');
        }
        if cell.contains([',', '"', '\n', '\r']) {
            row.push('"');
            row.push_str(&cell.replace('"', "\"\""));
            row.push('"');
        } else {
            row.push_str(&cell);
        }
    }
    row.push_str("\r\n");
    file.write_all(row.as_bytes()).map_err(Error::Io)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corroborating_observations_are_named_as_the_lines_name_them() {
        assert_eq!(
            named_observations(&format!(
                "{}:6563686F01 {}:AB",
                "11".repeat(16),
                "22".repeat(16)
            )),
            "11111111-1111-1111-1111-111111111111:6563686f01 \
             22222222-2222-2222-2222-222222222222:ab"
        );
    }
}
