//! History in `library.db`: the one ingest path for a listen, derivation of
//! plays in the same transaction, the full rebuild, and history queries. See
//! "Observations and derivation", "Incremental updates and rebuilds", and
//! "Queries" in docs/internals/history.md.
//!
//! An observation is written once and never changed. Plays are derived
//! from observations by the pure functions in `ritornel_domain::history`,
//! and are dropped and derived again whenever what they rest on changes:
//! for a new listen, the listens it is chained to; for a new binding, the
//! plays of the items it moved; for a new rule, everything.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use ritornel_domain::history::{
    self as derive, Binding, ContextKind, Correction, EndReason, Import, Inputs, Listen, Play,
    PlayRule, Received, RecordingOverride, StartReason, Verb, Window, ZonePeriod,
};
use rusqlite::{Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::library_store::Write;
use crate::{Error, Id, LibraryStore, WriteRequest};
use ritornel_domain::scrobble::same_track;

/// The version of a listen record's shape.
pub const LISTEN_RECORD_VERSION: u32 = 1;

/// What a write that takes in a listen is called, as its [`WriteRequest`]
/// names it: what the library's commit hook is told.
pub const INGEST_METHOD: &str = "history.ingest";

/// One finished listen, as the playing server sends it to the hub: every
/// field `listen_staging` holds, with public IDs. It is stored verbatim as
/// the observation's payload. See "Recording a listen" in
/// docs/internals/playback.md.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListenRecord {
    /// [`LISTEN_RECORD_VERSION`].
    pub v: u32,
    /// The listen's ID, a UUIDv7: its source key at the playing server's
    /// endpoint.
    pub listen_id: Id,
    /// The listener set on the player.
    pub listener: Id,
    /// The client session that queued the track.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_session: Option<Id>,
    /// The server whose player made the sound.
    pub played_on_server: Id,
    /// The track appearance that was queued: a hint for display.
    pub appearance: Id,
    /// The asset that played.
    pub asset: Id,
    /// The kind of copy that played.
    pub copy_kind: String,
    /// The source item of the master behind the copy.
    pub source_item: Id,
    /// Which of its revisions was heard.
    pub source_revision: i64,
    /// The playing server's wall clock at the first frame heard.
    pub started_at_ms: i64,
    /// The playing server's zone then, by its IANA name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zone: Option<String>,
    /// The playing server's monotonic clock then.
    pub started_mono_ms: i64,
    /// The playing server's boot then.
    pub boot: Id,
    /// Why it began.
    pub start_reason: String,
    /// The previous listen after a restart or a move between players.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continues: Option<Id>,
    /// Where in the track it began.
    pub start_position_ms: i64,
    /// The track's length.
    pub track_duration_ms: i64,
    /// Audio actually rendered.
    pub ms_played: i64,
    /// The ranges of the track that were heard, merged.
    pub heard_ranges: Vec<[i64; 2]>,
    /// Whether neighbouring ranges were joined to keep them under the cap.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub heard_ranges_approximate: bool,
    /// The furthest point reached.
    pub max_position_ms: i64,
    /// Seeks made.
    pub seek_count: i64,
    /// Pauses made.
    pub pause_count: i64,
    /// Where in the track it ended.
    pub end_position_ms: i64,
    /// Why it ended.
    pub end_reason: String,
    /// The playing server's wall clock at the end.
    pub ended_at_ms: i64,
    /// Its monotonic clock then, in the same boot as at the start. Left out
    /// only by a listen staged before the clock was kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_mono_ms: Option<i64>,
    /// The pause for a handoff whose release had not arrived at shutdown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff_mono_ms: Option<i64>,
    /// Why the track was queued.
    pub context_kind: String,
    /// What it was queued from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_ref: Option<Id>,
    /// Whether it was played in a shuffled order.
    pub shuffle: bool,
}

/// What the sender stamps each send with, retries included. See "Clocks and
/// monotonic age dating" in docs/internals/history.md.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SendStamps {
    /// The sender's wall clock.
    pub sent_at_ms: i64,
    /// The sender's monotonic clock.
    pub sent_mono_ms: i64,
    /// The sender's boot.
    pub sent_boot: Id,
}

/// What the one ingest call takes: observations of one kind, from one
/// endpoint. See "Observations and derivation" in docs/internals/history.md.
#[derive(Debug, Clone, Copy)]
pub enum Observations<'a> {
    /// One listen, as the playing server sends it, with the stamps of this
    /// send.
    Listen {
        /// The listen.
        record: &'a ListenRecord,
        /// The send's stamps.
        stamps: SendStamps,
    },
    /// Import records a history source read for an account, with the
    /// cursor that says how far it has read.
    Imports {
        /// The account, a `service_account`.
        account: Id,
        /// What the source read.
        batch: &'a ritornel_source::Batch,
        /// Whether it is of a full read, which sets aside what an earlier
        /// read stored twice ("Last.fm" in docs/internals/history.md).
        full: bool,
    },
}

/// What the one ingest call did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestOutcome {
    /// What became of a listen.
    Listen(Ingested),
    /// What became of a batch of import records.
    Imports(ritornel_source::Stored),
}

/// What became of a listen sent to the hub.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ingested {
    /// It is stored, at this library revision.
    Stored(i64),
    /// It was stored before, as it is: nothing was written.
    Duplicate,
    /// It was stored before with another payload. The first is kept and
    /// this one is recorded as a conflict.
    Conflict,
}

/// What a rebuild found and did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rebuilt {
    /// Plays before.
    pub before: i64,
    /// Plays after.
    pub after: i64,
    /// Plays that are new, gone or different.
    pub changed: i64,
    /// The library revision it was committed at, when it wrote.
    pub revision: Option<i64>,
}

/// One play, as the history reads give it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayRow {
    /// The play's ID.
    pub play_id: Id,
    /// When it began.
    pub at_ms: Option<i64>,
    /// Its local date, as YYYYMMDD.
    pub local_date: Option<i64>,
    /// Its local hour.
    pub local_hour: Option<i64>,
    /// The zone that date is in.
    pub zone: Option<String>,
    /// The zone's offset from UTC then, in minutes.
    pub offset_minutes: Option<i64>,
    /// Which clock the time rests on.
    pub clock_basis: String,
    /// The appearance, when the play is bound to one.
    pub appearance: Option<Id>,
    /// The recording, when the play is bound.
    pub recording: Option<Id>,
    /// What it shows: title, artist and album, from the appearance it is
    /// bound to or, for a play that is not bound, from the appearance its
    /// first listen queued, which history keeps as a hint for display.
    pub title: Option<String>,
    /// See `title`.
    pub artist: Option<String>,
    /// See `title`.
    pub album: Option<String>,
    /// Audio rendered, where it is known: not for a play another service
    /// reported.
    pub ms_played: Option<i64>,
    /// The track's length.
    pub track_duration_ms: Option<i64>,
    /// What became of it.
    pub outcome: String,
    /// Whether it counts.
    pub counts: bool,
    /// Whether it is excluded.
    pub excluded: bool,
    /// The listens behind it, first first.
    pub listens: Vec<ListenRow>,
}

/// One stretch of time a listener spent in one zone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZonePeriodRow {
    /// The zone, by its IANA name.
    pub zone: String,
    /// From when, as Unix milliseconds; `None` is open.
    pub from_ms: Option<i64>,
    /// Until when, not included; `None` is open.
    pub to_ms: Option<i64>,
}

/// Where a page of recent plays ended: its last play's instant, when it
/// has one, and its ID, which no two plays share. The next page holds the
/// plays after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlayCursor {
    /// The play's instant.
    pub at_ms: Option<i64>,
    /// The play's ID.
    pub play_id: Id,
}

impl PlayRow {
    /// The cursor a page that ends with this play hands on.
    pub fn cursor(&self) -> PlayCursor {
        PlayCursor {
            at_ms: self.at_ms,
            play_id: self.play_id,
        }
    }
}

/// One listen behind a play.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListenRow {
    /// The listen's ID.
    pub listen_id: Id,
    /// When it began and ended, by the playing server's clock.
    pub started_at_ms: i64,
    /// See `started_at_ms`.
    pub ended_at_ms: i64,
    /// Why it began.
    pub start_reason: String,
    /// Why it ended.
    pub end_reason: String,
    /// Where in the track it began and ended.
    pub start_position_ms: i64,
    /// See `start_position_ms`.
    pub end_position_ms: i64,
    /// Audio rendered.
    pub ms_played: i64,
    /// The listen it continues.
    pub continues: Option<Id>,
    /// The playing server's zone at the start.
    pub zone: Option<String>,
}

/// A play event's ID: 16 bytes computed from its first observation's
/// endpoint and source key, so a rebuild gives it again. See "Observations
/// and derivation" in docs/internals/history.md.
fn play_id(endpoint: &[u8; 16], source_key: &[u8]) -> [u8; 16] {
    let mut hash = Sha256::new();
    hash.update(b"ritornel-play-event");
    hash.update(endpoint);
    hash.update(source_key);
    let digest = hash.finalize();
    let mut bytes = [0; 16];
    for (byte, from) in bytes.iter_mut().zip(digest.iter()) {
        *byte = *from;
    }
    bytes
}

const LISTEN_COLUMNS: &str = "o.id, o.endpoint_id, o.source_key, o.listener_id, o.source_item_id,
    o.received_at_utc_ms, o.sent_mono_ms, o.sent_boot_id, o.clock_skew_ms, o.zone_name,
    l.started_at_utc_ms, l.ended_at_utc_ms, l.started_mono_ms, l.boot_id,
    l.continues_source_key, l.ms_played, l.track_duration_ms, l.end_reason, l.context_kind,
    l.context_ref, l.shuffle, l.ended_mono_ms,
    CASE WHEN l.end_reason='shutdown' THEN json_extract(o.payload_json,'$.handoffMonoMs') END";

fn listen(row: &Row<'_>) -> rusqlite::Result<Listen> {
    let text = |column: usize| -> rusqlite::Result<String> { row.get(column) };
    Ok(Listen {
        observation: row.get(0)?,
        endpoint: row.get(1)?,
        source_key: row.get(2)?,
        listener: row.get(3)?,
        source_item: row.get(4)?,
        received: Received {
            received_at_ms: row.get(5)?,
            sent_mono_ms: row.get(6)?,
            sent_boot: row.get(7)?,
            clock_skew_ms: row.get(8)?,
        },
        zone: row.get(9)?,
        started_at_ms: row.get(10)?,
        ended_at_ms: row.get(11)?,
        started_mono_ms: row.get(12)?,
        ended_mono_ms: row.get(21)?,
        handoff_mono_ms: row.get(22)?,
        boot: row.get(13)?,
        continues: row.get(14)?,
        ms_played: row.get(15)?,
        track_duration_ms: row.get(16)?,
        end_reason: EndReason::parse(&text(17)?).ok_or(rusqlite::Error::InvalidQuery)?,
        context_kind: ContextKind::parse(&text(18)?).ok_or(rusqlite::Error::InvalidQuery)?,
        context_ref: row.get(19)?,
        shuffle: row.get(20)?,
    })
}

/// The listens with these observation IDs, or every listen.
fn listens(connection: &Connection, only: Option<&BTreeSet<i64>>) -> Result<Vec<Listen>, Error> {
    let Some(only) = only else {
        let mut statement = connection.prepare_cached(&format!(
            "SELECT {LISTEN_COLUMNS} FROM history_observation o
             JOIN observation_listen l ON l.observation_id = o.id
             WHERE o.kind = 'listen' ORDER BY o.id"
        ))?;
        let all = statement.query_map([], listen)?;
        return Ok(all.collect::<Result<_, _>>()?);
    };
    let mut one = connection.prepare_cached(&format!(
        "SELECT {LISTEN_COLUMNS} FROM history_observation o
         JOIN observation_listen l ON l.observation_id = o.id
         WHERE o.id = ?1 AND o.kind = 'listen'"
    ))?;
    let mut found = Vec::with_capacity(only.len());
    for observation in only {
        if let Some(listen) = one.query_row([observation], listen).optional()? {
            found.push(listen);
        }
    }
    Ok(found)
}

/// Which observations a `set_recording` correction targets: explicit
/// observations named by `(endpoint, source_key)`, or a half-open UTC time
/// window on one endpoint, optionally narrowed to one source item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CorrectionTargets {
    /// Specific observations, named by `(endpoint public ID, source key)`.
    Observations(Vec<(Id, Vec<u8>)>),
    /// Observations on `endpoint` whose source-reported time falls in
    /// `[from_ms, to_ms)`, optionally narrowed to `source_item`.
    Window {
        /// The endpoint whose observations the window covers.
        endpoint: Id,
        /// Earliest reported instant, inclusive. `None` is open.
        from_ms: Option<i64>,
        /// Latest reported instant, exclusive. `None` is open.
        to_ms: Option<i64>,
        /// Narrow to observations of this source item, when set.
        source_item: Option<Id>,
    },
}

/// The current binding of each of these source items that has one, or of
/// every source item, the recording and appearance read through their
/// redirects. An appearance that leads to another recording's is no
/// appearance of this one, as for a correction.
fn bindings(
    connection: &Connection,
    only: Option<&BTreeSet<i64>>,
) -> Result<HashMap<i64, Binding>, Error> {
    const BINDING: &str = "SELECT a.source_item_id, r.canonical_id,
                CASE WHEN r_ta.canonical_id = r.canonical_id THEN ta.canonical_id END
         FROM assignment a JOIN recording r ON r.id = a.recording_id
         LEFT JOIN track_appearance ta ON ta.id = a.appearance_id
         LEFT JOIN track_appearance ta_canon ON ta_canon.id = ta.canonical_id
         LEFT JOIN recording r_ta ON r_ta.id = ta_canon.recording_id
         WHERE a.superseded_at_ms IS NULL";
    let binding = |row: &Row<'_>| -> rusqlite::Result<(i64, Binding)> {
        Ok((
            row.get(0)?,
            Binding {
                recording: row.get(1)?,
                appearance: row.get(2)?,
            },
        ))
    };
    let Some(only) = only else {
        let mut statement = connection.prepare_cached(BINDING)?;
        let rows = statement.query_map([], binding)?;
        return Ok(rows.collect::<Result<_, _>>()?);
    };
    let mut one = connection.prepare_cached(&format!("{BINDING} AND a.source_item_id = ?1"))?;
    let mut found = HashMap::new();
    for item in only {
        if let Some((item, bound)) = one.query_row([item], binding).optional()? {
            found.insert(item, bound);
        }
    }
    Ok(found)
}

/// Every correction, with its targets, window and recording override.
pub(crate) fn corrections(connection: &Connection) -> Result<Vec<Correction>, Error> {
    let mut statement = connection.prepare_cached(
        "SELECT c.observation_id, c.verb, c.target_endpoint_id, c.target_from_utc_ms,
                c.target_to_utc_ms, cr.correction_id IS NOT NULL, r.canonical_id,
                CASE WHEN r_ta.canonical_id = r.canonical_id THEN ta_canon.id ELSE NULL END,
                cr.source_item_id, t.target_observation_id
         FROM observation_correction c
         LEFT JOIN correction_recording cr ON cr.correction_id = c.observation_id
         LEFT JOIN recording r ON r.id = cr.recording_id
         LEFT JOIN track_appearance ta ON ta.id = cr.appearance_id
         LEFT JOIN track_appearance ta_canon ON ta_canon.id = ta.canonical_id
         LEFT JOIN recording r_ta ON r_ta.id = ta_canon.recording_id
         LEFT JOIN correction_target t ON t.correction_id = c.observation_id
         ORDER BY c.observation_id, t.target_observation_id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<i64>>(2)?,
            row.get::<_, Option<i64>>(3)?,
            row.get::<_, Option<i64>>(4)?,
            row.get::<_, bool>(5)?,
            row.get::<_, Option<i64>>(6)?,
            row.get::<_, Option<i64>>(7)?,
            row.get::<_, Option<i64>>(8)?,
            row.get::<_, Option<i64>>(9)?,
        ))
    })?;
    let mut by_id: BTreeMap<i64, Correction> = BTreeMap::new();
    for row in rows {
        let (
            observation,
            verb,
            endpoint,
            from_ms,
            to_ms,
            has_recording,
            recording,
            appearance,
            source_item,
            target,
        ) = row?;
        let verb = Verb::parse(&verb).ok_or(Error::Corrupt("a correction's verb"))?;
        let window = endpoint.map(|endpoint| Window {
            endpoint,
            from_ms,
            to_ms,
            source_item,
        });
        let recording = has_recording.then(|| RecordingOverride {
            binding: recording.map(|recording| Binding {
                recording,
                appearance,
            }),
        });
        let correction = by_id.entry(observation).or_insert_with(|| Correction {
            observation,
            verb,
            targets: Vec::new(),
            window,
            recording,
        });
        correction.targets.extend(target);
    }
    Ok(by_id.into_values().collect())
}

/// Every listener's zone periods.
pub(crate) fn zone_periods(connection: &Connection) -> Result<Vec<ZonePeriod>, Error> {
    let mut statement = connection.prepare_cached(
        "SELECT listener_id, zone_name, valid_from_ms, valid_to_ms FROM listener_zone_period",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(ZonePeriod {
            listener: row.get(0)?,
            zone: row.get(1)?,
            from_ms: row.get(2)?,
            to_ms: row.get(3)?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// The states of a delivery that can have reached its destination, and so
/// can come back from it as an echo.
const MAY_HAVE_ARRIVED: &str = "('sending', 'accepted', 'unknown')";

/// Whether an import naming `(artist, title)` matches a delivery's track,
/// either as sent or as the service took it.
fn matches_delivery(
    artist: &str,
    title: &str,
    sent_artist: &str,
    sent_title: &str,
    took_artist: Option<&str>,
    took_title: Option<&str>,
) -> bool {
    let took = match (took_artist, took_title) {
        (Some(a), Some(t)) => same_track(artist, title, a, t),
        _ => false,
    };
    took || same_track(artist, title, sent_artist, sent_title)
}

/// Every listen whose play an import may be the echo of: deliveries to the
/// import's own endpoint, which may have arrived, sent with the import's
/// time to the second and naming the same track, as sent or as the service
/// took it, ordered by listen observation.
pub(crate) fn echo_candidates(
    connection: &Connection,
    endpoint: i64,
    played_at_ms: i64,
    artist: &str,
    title: &str,
) -> Result<Vec<i64>, Error> {
    if played_at_ms % 1000 != 0 {
        return Ok(Vec::new());
    }
    let mut sent = connection.prepare_cached(&format!(
        "SELECT observation_id, artist, title, accepted_artist, accepted_title
         FROM history_delivery
         WHERE endpoint_id = ?1 AND sent_timestamp_s = ?2 AND state IN {MAY_HAVE_ARRIVED}
         ORDER BY observation_id"
    ))?;
    let rows = sent.query_map((endpoint, played_at_ms / 1000), |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, Option<String>>(4)?,
        ))
    })?;
    let mut matched = Vec::new();
    for row in rows {
        let (listen, sent_artist, sent_title, took_artist, took_title) = row?;
        if matches_delivery(
            artist,
            title,
            &sent_artist,
            &sent_title,
            took_artist.as_deref(),
            took_title.as_deref(),
        ) {
            matched.push(listen);
        }
    }
    Ok(matched)
}

/// The imports that may echo any of this listen's deliveries.
fn echoes_of(connection: &Connection, listen: i64) -> Result<Vec<i64>, Error> {
    type DeliveryRow = (i64, i64, String, String, Option<String>, Option<String>);
    let mut deliveries = connection.prepare_cached(&format!(
        "SELECT endpoint_id, sent_timestamp_s, artist, title, accepted_artist, accepted_title
         FROM history_delivery
         WHERE observation_id = ?1 AND state IN {MAY_HAVE_ARRIVED}"
    ))?;
    let sent: Vec<DeliveryRow> = deliveries
        .query_map([listen], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        })?
        .collect::<Result<_, _>>()?;
    let mut found = Vec::new();
    let mut at = connection.prepare_cached(
        "SELECT o.id, i.artist, i.title FROM observation_import i
         JOIN history_observation o ON o.id = i.observation_id
         WHERE i.played_at_utc_ms = ?2 AND o.endpoint_id = ?1",
    )?;
    for (endpoint, seconds, sent_artist, sent_title, took_artist, took_title) in sent {
        let Some(at_ms) = seconds.checked_mul(1000) else {
            continue;
        };
        let candidates: Vec<(i64, String, String)> = at
            .query_map((endpoint, at_ms), |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?
            .collect::<Result<_, _>>()?;
        for (import, artist, title) in candidates {
            if matches_delivery(
                &artist,
                &title,
                &sent_artist,
                &sent_title,
                took_artist.as_deref(),
                took_title.as_deref(),
            ) {
                found.push(import);
            }
        }
    }
    Ok(found)
}

const IMPORT_COLUMNS: &str = "o.id, o.endpoint_id, o.listener_id, o.source_item_id,
    i.played_at_utc_ms, i.artist, i.title, o.source_key";

/// Leaves Spotify's streams out of the scrobbles: derivation reads the
/// streams in force on their own (`history_streams`).
const NOT_A_STREAM: &str =
    "NOT EXISTS (SELECT 1 FROM observation_import_stream s WHERE s.observation_id = o.id)";

/// Observations currently excluded by an active `shifted_read` correction
/// (phantom duplicates from an earlier shifted paged read). These are never
/// real echoes of a local listen and must not pair with a delivered listen.
fn shifted_read_excluded(connection: &Connection) -> Result<BTreeSet<i64>, Error> {
    let mut statement = connection.prepare_cached(
        "SELECT c.observation_id
         FROM observation_correction c
         JOIN history_observation o ON o.id = c.observation_id
         WHERE c.verb = 'exclude' AND json_extract(o.payload_json, '$.reason') = 'shifted_read'",
    )?;
    let shifted_ids: BTreeSet<i64> = statement
        .query_map([], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    if shifted_ids.is_empty() {
        return Ok(BTreeSet::new());
    }
    let relevant: Vec<Correction> = corrections(connection)?
        .into_iter()
        .filter(|correction| {
            correction.verb == Verb::Revoke || shifted_ids.contains(&correction.observation)
        })
        .collect();
    Ok(derive::excluded(&relevant))
}

/// The imports with these observation IDs, or every import, each with the
/// listens it may be the echo of.
fn imports(connection: &Connection, only: Option<&BTreeSet<i64>>) -> Result<Vec<Import>, Error> {
    type Found = (i64, i64, i64, i64, i64, String, String, Vec<u8>);
    let row = |row: &Row<'_>| -> rusqlite::Result<Found> {
        Ok((
            row.get(0)?,
            row.get(1)?,
            row.get(2)?,
            row.get(3)?,
            row.get(4)?,
            row.get(5)?,
            row.get(6)?,
            row.get(7)?,
        ))
    };
    let found: Vec<Found> = match only {
        None => {
            let mut statement = connection.prepare_cached(&format!(
                "SELECT {IMPORT_COLUMNS} FROM history_observation o
                 JOIN observation_import i ON i.observation_id = o.id
                 WHERE {NOT_A_STREAM} ORDER BY o.id"
            ))?;
            statement.query_map([], row)?.collect::<Result<_, _>>()?
        }
        Some(only) => {
            let mut one = connection.prepare_cached(&format!(
                "SELECT {IMPORT_COLUMNS} FROM history_observation o
                 JOIN observation_import i ON i.observation_id = o.id
                 WHERE o.id = ?1 AND {NOT_A_STREAM}"
            ))?;
            let mut found = Vec::with_capacity(only.len());
            for observation in only {
                if let Some(import) = one.query_row([observation], row).optional()? {
                    found.push(import);
                }
            }
            found
        }
    };
    let shifted_excluded = if found.is_empty() {
        BTreeSet::new()
    } else {
        shifted_read_excluded(connection)?
    };
    found
        .into_iter()
        .map(
            |(observation, endpoint, listener, source_item, played_at_ms, artist, title, key)| {
                let echo_of = if shifted_excluded.contains(&observation) {
                    Vec::new()
                } else {
                    echo_candidates(connection, endpoint, played_at_ms, &artist, &title)?
                };
                Ok(Import {
                    observation,
                    endpoint,
                    listener,
                    source_item,
                    played_at_ms,
                    echo_of,
                    key,
                    artist,
                    title,
                })
            },
        )
        .collect()
}

/// The observations a play of this one can depend on, one step away: a
/// listen's, those joined to it by a `continues` link either way and the
/// echoes of its deliveries; an import's, the listen it is the echo of and
/// the streams and scrobbles the stream matcher could join it with; and any
/// observation a correction joins to it. [`rederive`] follows them as far
/// as they go.
fn neighbours(connection: &Connection, at: i64) -> Result<Vec<i64>, Error> {
    let (what, endpoint, played_at, artist, title): (
        String,
        i64,
        Option<i64>,
        Option<String>,
        Option<String>,
    ) = connection
        .prepare_cached(
            "SELECT o.kind, o.endpoint_id, i.played_at_utc_ms, i.artist, i.title
             FROM history_observation o
             LEFT JOIN observation_import i ON i.observation_id = o.id WHERE o.id = ?1",
        )?
        .query_row([at], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })?;
    let mut near = Vec::new();
    if what == "import" {
        if let (Some(played_at), Some(artist), Some(title)) = (played_at, artist, title) {
            near.extend(echo_candidates(
                connection, endpoint, played_at, &artist, &title,
            )?);
        }
        near.extend(crate::history_streams::neighbours(connection, at)?);
    } else {
        for sql in [
            "SELECT p.id FROM history_observation o
             JOIN observation_listen l ON l.observation_id = o.id
             JOIN history_endpoint ep ON ep.kind = 'player'
             JOIN history_observation p ON p.endpoint_id = ep.id
                                       AND p.kind = 'listen'
                                       AND (p.endpoint_id = o.endpoint_id OR p.listener_id = o.listener_id)
                                       AND p.source_key = l.continues_source_key
             WHERE o.id = ?1",
            "SELECT n.observation_id FROM history_observation o
             JOIN history_observation m ON m.endpoint_id = o.endpoint_id OR m.listener_id = o.listener_id
             JOIN observation_listen n ON n.observation_id = m.id
             WHERE o.id = ?1 AND n.continues_source_key = o.source_key",
        ] {
            let joined: Vec<i64> = connection
                .prepare_cached(sql)?
                .query_map([at], |row| row.get(0))?
                .collect::<Result<_, _>>()?;
            near.extend(joined);
        }
        near.extend(echoes_of(connection, at)?);
    }
    let pinned: Vec<i64> = connection
        .prepare_cached(
            "SELECT other.target_observation_id FROM correction_target mine
             JOIN observation_correction c ON c.observation_id = mine.correction_id
                                          AND c.verb = 'join'
             JOIN correction_target other ON other.correction_id = mine.correction_id
             WHERE mine.target_observation_id = ?1",
        )?
        .query_map([at], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    near.extend(pinned);
    Ok(near)
}

/// Drops the plays these observations support, and every link to them,
/// which `heard` tidies when it finishes.
fn drop_plays_of(
    write: &Write<'_>,
    observations: &BTreeSet<i64>,
    heard: &mut crate::history_heard::Linker<'_>,
) -> Result<(), Error> {
    let connection = write.connection();
    let mut plays = BTreeSet::new();
    let mut find = connection.prepare_cached(
        "SELECT play_event_id FROM play_event_observation WHERE observation_id = ?1",
    )?;
    for observation in observations {
        let found: Vec<i64> = find
            .query_map([observation], |row| row.get(0))?
            .collect::<Result<_, _>>()?;
        plays.extend(found);
    }
    heard.unlink(&plays)?;
    for play in plays {
        connection
            .prepare_cached("DELETE FROM play_event_observation WHERE play_event_id = ?1")?
            .execute([play])?;
        connection
            .prepare_cached("DELETE FROM play_event WHERE id = ?1")?
            .execute([play])?;
    }
    Ok(())
}

/// The endpoint's public ID and the source key of an observation, which a
/// play's ID is computed from.
fn keys(connection: &Connection, observation: i64) -> Result<([u8; 16], Vec<u8>), Error> {
    Ok(connection
        .prepare_cached(
            "SELECT e.public_id, o.source_key FROM history_observation o
             JOIN history_endpoint e ON e.id = o.endpoint_id WHERE o.id = ?1",
        )?
        .query_row([observation], |row| Ok((row.get(0)?, row.get(1)?)))?)
}

/// Writes derived plays.
fn insert_plays(write: &Write<'_>, plays: &[Play]) -> Result<(), Error> {
    let mut heard = crate::history_heard::Linker::new(write.connection(), write.rev());
    link_plays(write, plays, &mut heard)?;
    heard.finish()
}

/// Writes derived plays and links them, with `heard`, which the caller
/// finishes.
fn link_plays(
    write: &Write<'_>,
    plays: &[Play],
    heard: &mut crate::history_heard::Linker<'_>,
) -> Result<(), Error> {
    let connection = write.connection();
    for play in plays {
        let Some(&(head, _)) = play.observations.first() else {
            continue;
        };
        let (endpoint, key) = keys(connection, head)?;
        let time = &play.time;
        let local = time.local;
        connection
            .prepare_cached(
                "INSERT INTO play_event
                   (public_id, listener_id, recording_id, appearance_id, source_item_id,
                    event_at_utc_ms, time_anchor, clock_basis, time_precision_ms, zone_name,
                    zone_basis, utc_offset_minutes, local_date, month_day, local_hour, local_dow,
                    calendar_basis, evidence_class, ms_played, track_duration_ms, outcome, counts,
                    count_basis, play_rule_version, grouper_version, excluded, context_kind,
                    context_ref, shuffle, rev)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                         ?17, 'observed', ?18, ?19, ?20, ?21, ?29, ?22, ?23, ?24, ?25,
                         ?26, ?27, ?28)",
            )?
            .execute(rusqlite::params![
                play_id(&endpoint, &key),
                play.listener,
                play.recording,
                play.appearance,
                play.source_item,
                time.event_at_ms,
                time.anchor.as_str(),
                time.clock_basis.as_str(),
                time.precision_ms,
                time.zone,
                time.zone_basis.as_str(),
                local.map(|local| local.offset_minutes),
                local.map(|local| local.date),
                local.map(|local| local.month_day),
                local.map(|local| local.hour),
                local.map(|local| local.weekday),
                time.calendar_basis.as_str(),
                play.ms_played,
                play.track_duration_ms,
                play.outcome.as_str(),
                play.counts,
                play.play_rule_version,
                play.grouper_version,
                play.excluded,
                play.context_kind.map(ContextKind::as_str),
                play.context_ref,
                play.shuffle,
                write.rev(),
                play.count_basis.as_str(),
            ])?;
        let event = connection.last_insert_rowid();
        heard.link(&crate::history_heard::ToLink {
            event,
            listener: play.listener,
            recording: play.recording,
            appearance: play.appearance,
            primary: head,
            counted: play.counts && !play.excluded,
            local_date: local.map(|local| local.date),
            at_ms: time.event_at_ms,
        })?;
        let joined: HashMap<i64, derive::Joined> = play.joined.iter().copied().collect();
        for (observation, role) in &play.observations {
            connection
                .prepare_cached(
                    "INSERT INTO play_event_observation
                       (play_event_id, observation_id, role, joined_by, rev)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                )?
                .execute((
                    event,
                    observation,
                    role.as_str(),
                    joined.get(observation).map(|joined| joined.as_str()),
                    write.rev(),
                ))?;
        }
    }
    Ok(())
}

/// Derives again what a change to a delivery can move: whether an import
/// is the echo of its listen depends on the delivery's state and on the
/// names the service took it under. With no import of that second on that
/// account there is nothing to move.
pub(crate) fn rederive_delivery(write: &Write<'_>, delivery: i64) -> Result<(), Error> {
    rederive_delivery_by(write, delivery, PlayRule::CURRENT)
}

/// As [`rederive_delivery`], under `rule`.
pub(crate) fn rederive_delivery_by(
    write: &Write<'_>,
    delivery: i64,
    rule: PlayRule,
) -> Result<(), Error> {
    let connection = write.connection();
    let (listen, endpoint, seconds): (i64, i64, i64) = connection
        .prepare_cached(
            "SELECT observation_id, endpoint_id, sent_timestamp_s FROM history_delivery
             WHERE id = ?1",
        )?
        .query_row([delivery], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
    let imports: Vec<i64> = connection
        .prepare_cached(
            "SELECT o.id FROM observation_import i
             JOIN history_observation o ON o.id = i.observation_id
             WHERE i.played_at_utc_ms = ?2 AND o.endpoint_id = ?1",
        )?
        .query_map((endpoint, seconds * 1000), |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    if imports.is_empty() {
        return Ok(());
    }
    let mut affected: BTreeSet<i64> = imports.into_iter().collect();
    affected.insert(listen);
    rederive(write, &affected, rule)
}

/// Derives again the plays of these observations, and of everything chained
/// to them or sharing an existing play with them.
pub(crate) fn rederive(
    write: &Write<'_>,
    observations: &BTreeSet<i64>,
    rule: PlayRule,
) -> Result<(), Error> {
    let connection = write.connection();
    let mut around = BTreeSet::new();
    let mut todo: Vec<i64> = observations.iter().copied().collect();
    // Widen to every observation currently sharing a derived play with any
    // affected observation so moving an echo or breaking a chain can never
    // drop a sibling observation's play.
    let mut siblings = connection.prepare_cached(
        "SELECT other.observation_id
         FROM play_event_observation mine
         JOIN play_event_observation other ON other.play_event_id = mine.play_event_id
         WHERE mine.observation_id = ?1",
    )?;
    while let Some(observation) = todo.pop() {
        if !around.insert(observation) {
            continue;
        }
        for near in neighbours(connection, observation)? {
            if !around.contains(&near) {
                todo.push(near);
            }
        }
        for sibling in siblings.query_map([observation], |row| row.get::<_, i64>(0))? {
            let sibling = sibling?;
            if !around.contains(&sibling) {
                todo.push(sibling);
            }
        }
    }
    // One linker drops the plays' links and makes the new ones, so that a
    // row the old plays and the new share is never dropped and made again.
    let mut heard = crate::history_heard::Linker::new(connection, write.rev());
    drop_plays_of(write, &around, &mut heard)?;
    let listens = listens(connection, Some(&around))?;
    let imports = imports(connection, Some(&around))?;
    let streams = crate::history_streams::streams(connection, Some(&around))?;
    let items: BTreeSet<i64> = listens
        .iter()
        .map(|listen| listen.source_item)
        .chain(imports.iter().map(|import| import.source_item))
        .chain(streams.iter().map(|stream| stream.source_item))
        .collect();
    let tracks: BTreeSet<i64> = streams.iter().map(|stream| stream.source_item).collect();
    let plays = derive::derive(&Inputs {
        listens: &listens,
        imports: &imports,
        bindings: &bindings(connection, Some(&items))?,
        corrections: &corrections(connection)?,
        zone_periods: &zone_periods(connection)?,
        rule,
        lengths: &crate::history_streams::lengths(connection, &tracks)?,
        streams: &streams,
    });
    link_plays(write, &plays, &mut heard)?;
    heard.finish()
}

/// Every play, derived from scratch, as the rows a rebuild would write.
pub(crate) fn derive_all(connection: &Connection, rule: PlayRule) -> Result<Vec<Play>, Error> {
    let listens = listens(connection, None)?;
    let imports = imports(connection, None)?;
    let streams = crate::history_streams::streams(connection, None)?;
    Ok(derive::derive(&Inputs {
        listens: &listens,
        imports: &imports,
        bindings: &bindings(connection, None)?,
        corrections: &corrections(connection)?,
        zone_periods: &zone_periods(connection)?,
        rule,
        lengths: &crate::history_streams::lengths_of(&streams),
        streams: &streams,
    }))
}

/// Drops every play and derives each again, in the write: what applying or
/// undoing a run of streams does, since a stream in force or out of it can
/// move a track's length, and so any play of it.
pub(crate) fn derive_every_play(write: &Write<'_>) -> Result<(), Error> {
    derive_every_play_by(write, PlayRule::CURRENT)
}

/// Drops every play and derives each again by `rule`, in the write.
fn derive_every_play_by(write: &Write<'_>, rule: PlayRule) -> Result<(), Error> {
    let connection = write.connection();
    crate::history_heard::clear(connection)?;
    connection.execute("DELETE FROM play_event_observation", [])?;
    connection.execute("DELETE FROM play_event", [])?;
    insert_plays(write, &derive_all(connection, rule)?)
}

/// The derived tables, one line per play with its observations, in the
/// order of the plays' IDs: what a rebuild is compared by. Row keys and
/// `rev` are left out, since a rebuild writes them anew.
fn derived(connection: &Connection) -> Result<Vec<String>, Error> {
    let mut statement = connection.prepare(
        "SELECT hex(pe.public_id), pe.listener_id, pe.recording_id, pe.appearance_id,
                pe.source_item_id, pe.event_at_utc_ms, pe.time_anchor, pe.clock_basis,
                pe.time_precision_ms, pe.zone_name, pe.zone_basis, pe.utc_offset_minutes,
                pe.local_date, pe.month_day, pe.local_hour, pe.local_dow, pe.calendar_basis,
                pe.not_before_ms, pe.not_after_ms, pe.evidence_class, pe.ms_played,
                pe.track_duration_ms, pe.outcome, pe.counts, pe.count_basis,
                pe.play_rule_version, pe.grouper_version, pe.excluded, pe.context_kind,
                hex(pe.context_ref), pe.shuffle,
                (SELECT group_concat(observation_id || ':' || role || ':'
                                     || coalesce(joined_by, ''), ',')
                 FROM (SELECT observation_id, role, joined_by FROM play_event_observation
                       WHERE play_event_id = pe.id ORDER BY observation_id))
         FROM play_event pe ORDER BY pe.public_id",
    )?;
    let columns = statement.column_count();
    let rows = statement.query_map([], |row| {
        (0..columns)
            .map(|column| {
                Ok(match row.get_ref(column)? {
                    rusqlite::types::ValueRef::Null => "-".to_owned(),
                    rusqlite::types::ValueRef::Integer(number) => number.to_string(),
                    rusqlite::types::ValueRef::Real(number) => number.to_string(),
                    rusqlite::types::ValueRef::Text(text) => {
                        String::from_utf8_lossy(text).into_owned()
                    }
                    rusqlite::types::ValueRef::Blob(bytes) => {
                        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
                    }
                })
            })
            .collect::<rusqlite::Result<Vec<String>>>()
            .map(|columns| columns.join("|"))
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Builds a SQL `WHERE` predicate and parameter list for a window target on
/// one observation table, omitting unconstrained clauses so SQLite can use
/// indexes on `(endpoint_id, ...)`, `source_item_id`, and timestamps.
fn window_where(window: Window, time_column: &str) -> (String, Vec<rusqlite::types::Value>) {
    use rusqlite::types::Value;
    let mut clauses = vec!["o.endpoint_id = ?".to_owned()];
    let mut params = vec![Value::Integer(window.endpoint)];
    if let Some(item) = window.source_item {
        clauses.push("o.source_item_id = ?".to_owned());
        params.push(Value::Integer(item));
    }
    if let Some(from) = window.from_ms {
        clauses.push(format!("{time_column} >= ?"));
        params.push(Value::Integer(from));
    }
    if let Some(to) = window.to_ms {
        clauses.push(format!("{time_column} < ?"));
        params.push(Value::Integer(to));
    }
    (clauses.join(" AND "), params)
}

/// The observations matched by a window target on one endpoint.
fn window_observations(connection: &Connection, window: Window) -> Result<Vec<i64>, Error> {
    let mut found = Vec::new();
    let (import_where, import_params) = window_where(window, "i.played_at_utc_ms");
    let mut imports = connection.prepare(&format!(
        "SELECT o.id FROM history_observation o
         JOIN observation_import i ON i.observation_id = o.id
         WHERE {import_where}"
    ))?;
    for id in imports.query_map(rusqlite::params_from_iter(import_params), |row| {
        row.get::<_, i64>(0)
    })? {
        found.push(id?);
    }
    let (listen_where, listen_params) = window_where(window, "l.started_at_utc_ms");
    let mut listens = connection.prepare(&format!(
        "SELECT o.id FROM history_observation o
         JOIN observation_listen l ON l.observation_id = o.id
         WHERE {listen_where}"
    ))?;
    for id in listens.query_map(rusqlite::params_from_iter(listen_params), |row| {
        row.get::<_, i64>(0)
    })? {
        found.push(id?);
    }
    Ok(found)
}

/// Sets aside each of `targets`, observations of the account's history,
/// with an `exclude` correction of its own on the manual endpoint, which
/// says `note` and whose payload names its `reason` where it has one, and
/// derives their plays again by `rule`. Nothing is removed, and a
/// correction is revoked as any other is.
pub(crate) fn exclude(
    write: &Write<'_>,
    targets: &[i64],
    note: &str,
    reason: Option<&str>,
    rule: PlayRule,
) -> Result<(), Error> {
    if targets.is_empty() {
        return Ok(());
    }
    let connection = write.connection();
    let manual: i64 = connection
        .query_row(
            "SELECT id FROM history_endpoint WHERE kind = 'manual'",
            [],
            |row| row.get(0),
        )
        .optional()?
        .ok_or(Error::Corrupt("a library with no manual endpoint"))?;
    let payload =
        serde_json::json!({ "verb": "exclude", "reason": reason, "note": note }).to_string();
    for target in targets {
        let listener: i64 = connection.query_row(
            "SELECT listener_id FROM history_observation WHERE id = ?1",
            [target],
            |row| row.get(0),
        )?;
        connection.execute(
            "INSERT INTO history_observation
               (endpoint_id, source_key, kind, listener_id, captured_at_utc_ms,
                received_at_utc_ms, zone_basis, payload_json, rev)
             VALUES (?1, ?2, 'correction', ?3, ?4, ?4, 'none', ?5, ?6)",
            rusqlite::params![
                manual,
                write.new_id()?.as_bytes(),
                listener,
                write.now_ms(),
                payload,
                write.rev(),
            ],
        )?;
        let correction = connection.last_insert_rowid();
        connection.execute(
            "INSERT INTO observation_correction (observation_id, verb, note, rev)
             VALUES (?1, 'exclude', ?2, ?3)",
            rusqlite::params![correction, note, write.rev()],
        )?;
        connection.execute(
            "INSERT INTO correction_target (correction_id, target_observation_id, rev)
             VALUES (?1, ?2, ?3)",
            (correction, target, write.rev()),
        )?;
    }
    rederive(write, &targets.iter().copied().collect(), rule)
}

/// Revokes the correction `correction`, an observation's row, with a
/// `revoke` correction on the manual endpoint that says `note`, and derives
/// every play it touched again by `rule`.
pub(crate) fn revoke(
    write: &Write<'_>,
    correction: i64,
    note: &str,
    rule: PlayRule,
) -> Result<(), Error> {
    revoke_by(write, correction, Some(note), None, rule).map(|_| ())
}

/// [`revoke`], by the client session `session` where there is one. Returns
/// the revocation's key.
fn revoke_by(
    write: &Write<'_>,
    correction: i64,
    note: Option<&str>,
    session: Option<Id>,
    rule: PlayRule,
) -> Result<Id, Error> {
    let connection = write.connection();
    let (manual, listener, target): (i64, i64, [u8; 16]) = connection.query_row(
        "SELECT endpoint_id, listener_id, source_key FROM history_observation WHERE id = ?1",
        [correction],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    let key = write.new_id()?;
    let payload = serde_json::json!({
        "verb": "revoke",
        "target": Id::from_bytes(target).to_string(),
        "note": note,
    })
    .to_string();
    connection.execute(
        "INSERT INTO history_observation
           (endpoint_id, source_key, kind, listener_id, captured_at_utc_ms,
            received_at_utc_ms, zone_basis, payload_json, rev)
         VALUES (?1, ?2, 'correction', ?3, ?4, ?4, 'none', ?5, ?6)",
        rusqlite::params![
            manual,
            key.as_bytes(),
            listener,
            write.now_ms(),
            payload,
            write.rev(),
        ],
    )?;
    let revocation = connection.last_insert_rowid();
    connection.execute(
        "INSERT INTO observation_correction
           (observation_id, verb, note, actor_session_pid, rev)
         VALUES (?1, 'revoke', ?2, ?3, ?4)",
        rusqlite::params![
            revocation,
            note,
            session.as_ref().map(Id::as_bytes),
            write.rev(),
        ],
    )?;
    connection.execute(
        "INSERT INTO correction_target (correction_id, target_observation_id, rev)
         VALUES (?1, ?2, ?3)",
        (revocation, correction, write.rev()),
    )?;
    let affected = correction_affected_observations(connection, correction)?;
    if !affected.is_empty() {
        rederive(write, &affected, rule)?;
    }
    Ok(key)
}

/// The non-correction observations affected by a correction, following
/// `revoke` targets down to the corrections they undo.
fn correction_affected_observations(
    connection: &Connection,
    correction: i64,
) -> Result<BTreeSet<i64>, Error> {
    let all = corrections(connection)?;
    let by_id: HashMap<i64, &Correction> =
        all.iter().map(|entry| (entry.observation, entry)).collect();
    let mut seen_corrections = BTreeSet::new();
    let mut todo = vec![correction];
    let mut affected = BTreeSet::new();
    while let Some(id) = todo.pop() {
        if !seen_corrections.insert(id) {
            continue;
        }
        let Some(entry) = by_id.get(&id) else {
            continue;
        };
        if entry.verb == Verb::Revoke {
            todo.extend(entry.targets.iter().copied());
            continue;
        }
        affected.extend(entry.targets.iter().copied());
        if let Some(window) = entry.window {
            affected.extend(window_observations(connection, window)?);
        }
    }
    Ok(affected)
}

/// The plays whose binding is not what derivation would give them now: a
/// scan, a merge or an observation correction has moved what they point to
/// since they were derived.
fn stale_bindings(connection: &Connection) -> Result<BTreeSet<i64>, Error> {
    let has_overrides: bool = connection.query_row(
        "SELECT EXISTS (SELECT 1 FROM correction_recording)",
        [],
        |row| row.get(0),
    )?;
    let active: Vec<(i64, Option<i64>, Option<i64>)> = if has_overrides {
        let all = corrections(connection)?;
        derive::active_recording_overrides(&all)
            .into_iter()
            .filter_map(|entry| {
                let chosen = entry.recording?;
                Some((
                    entry.observation,
                    chosen.binding.map(|binding| binding.recording),
                    chosen.binding.and_then(|binding| binding.appearance),
                ))
            })
            .collect()
    } else {
        Vec::new()
    };
    if active.is_empty() {
        let mut statement = connection.prepare(
            "SELECT peo.observation_id FROM play_event pe
             JOIN play_event_observation peo ON peo.play_event_id = pe.id
             LEFT JOIN assignment a ON a.source_item_id = pe.source_item_id
                                   AND a.superseded_at_ms IS NULL
             LEFT JOIN recording r ON r.id = a.recording_id
             LEFT JOIN track_appearance ta ON ta.id = a.appearance_id
             LEFT JOIN track_appearance ta_canon ON ta_canon.id = ta.canonical_id
             LEFT JOIN recording r_ta ON r_ta.id = ta_canon.recording_id
             WHERE pe.recording_id IS NOT r.canonical_id
                OR pe.appearance_id IS NOT
                   (CASE WHEN r_ta.canonical_id = r.canonical_id THEN ta.canonical_id END)",
        )?;
        let rows = statement.query_map([], |row| row.get(0))?;
        return Ok(rows.collect::<Result<_, _>>()?);
    }

    // With active `set_recording` corrections, compute the winning override
    // per play set-wise in SQL and compare each play against its winner (or
    // against its source item's assignment when no override matches it).
    let encoded = serde_json::to_string(&active)?;
    let mut statement = connection.prepare(
        "WITH active(corr_id, want_rec, want_app) AS (
             SELECT CAST(json_extract(value, '$[0]') AS INTEGER),
                    CAST(json_extract(value, '$[1]') AS INTEGER),
                    CAST(json_extract(value, '$[2]') AS INTEGER)
             FROM json_each(?1)
         ),
         active_win AS (
             SELECT a.corr_id, a.want_rec, a.want_app,
                    oc.target_endpoint_id AS endpoint_id,
                    cr.source_item_id,
                    oc.target_from_utc_ms AS from_ms,
                    oc.target_to_utc_ms AS to_ms
             FROM active a
             JOIN observation_correction oc ON oc.observation_id = a.corr_id
             JOIN correction_recording cr ON cr.correction_id = a.corr_id
             WHERE oc.target_endpoint_id IS NOT NULL
         ),
         matched(play_event_id, corr_id, want_rec, want_app) AS (
             SELECT peo.play_event_id, a.corr_id, a.want_rec, a.want_app
             FROM active a
             JOIN correction_target ct ON ct.correction_id = a.corr_id
             JOIN play_event_observation peo ON peo.observation_id = ct.target_observation_id
             UNION ALL
             SELECT peo.play_event_id, w.corr_id, w.want_rec, w.want_app
             FROM active_win w
             JOIN history_observation o
               ON o.endpoint_id = w.endpoint_id
              AND (w.source_item_id IS NULL OR o.source_item_id = w.source_item_id)
             JOIN observation_import i
               ON i.observation_id = o.id
              AND (w.from_ms IS NULL OR i.played_at_utc_ms >= w.from_ms)
              AND (w.to_ms IS NULL OR i.played_at_utc_ms < w.to_ms)
             JOIN play_event_observation peo ON peo.observation_id = o.id
             UNION ALL
             SELECT peo.play_event_id, w.corr_id, w.want_rec, w.want_app
             FROM active_win w
             JOIN history_observation o
               ON o.endpoint_id = w.endpoint_id
              AND (w.source_item_id IS NULL OR o.source_item_id = w.source_item_id)
             JOIN observation_listen l
               ON l.observation_id = o.id
              AND (w.from_ms IS NULL OR l.started_at_utc_ms >= w.from_ms)
              AND (w.to_ms IS NULL OR l.started_at_utc_ms < w.to_ms)
             JOIN play_event_observation peo ON peo.observation_id = o.id
         ),
         winner AS (
             SELECT play_event_id, want_rec, want_app,
                    ROW_NUMBER() OVER (PARTITION BY play_event_id ORDER BY corr_id DESC) AS rn
             FROM matched
         )
         SELECT peo.observation_id
         FROM play_event pe
         LEFT JOIN winner w ON w.play_event_id = pe.id AND w.rn = 1
         LEFT JOIN assignment a ON a.source_item_id = pe.source_item_id
                               AND a.superseded_at_ms IS NULL
         LEFT JOIN recording r ON r.id = a.recording_id
         LEFT JOIN track_appearance ta ON ta.id = a.appearance_id
         LEFT JOIN track_appearance ta_canon ON ta_canon.id = ta.canonical_id
         LEFT JOIN recording r_ta ON r_ta.id = ta_canon.recording_id
         JOIN play_event_observation peo ON peo.play_event_id = pe.id
         WHERE (w.play_event_id IS NOT NULL
                AND (pe.recording_id IS NOT w.want_rec OR pe.appearance_id IS NOT w.want_app))
            OR (w.play_event_id IS NULL
                AND (pe.recording_id IS NOT r.canonical_id
                     OR pe.appearance_id IS NOT
                        (CASE WHEN r_ta.canonical_id = r.canonical_id THEN ta.canonical_id END)))",
    )?;
    let rows = statement.query_map([encoded], |row| row.get(0))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// What a change to the catalog moves, in its write: plays follow their
/// source items' new bindings, and what each play was heard as follows the
/// catalog.
pub(crate) fn catalog_changed(write: &Write<'_>) -> Result<(), Error> {
    rebind(write)?;
    crate::history_heard::catalog_changed(write.connection(), write.rev())
}

/// Derives again the plays whose binding has moved, in the transaction that
/// moves bindings: the resolver's, through [`catalog_changed`], and the
/// matcher's.
pub(crate) fn rebind(write: &Write<'_>) -> Result<(), Error> {
    let stale = stale_bindings(write.connection())?;
    if stale.is_empty() {
        return Ok(());
    }
    rederive(write, &stale, PlayRule::CURRENT)
}

/// Stores one listen, in the transaction of `write`, and derives its play.
/// `received_at_ms` is when the hub took it in, before it waited for the
/// writer.
fn ingest(
    write: &Write<'_>,
    record: &ListenRecord,
    stamps: SendStamps,
    received_at_ms: i64,
    payload: &str,
    rule: PlayRule,
) -> Result<Ingested, Error> {
    let connection = write.connection();
    let endpoint: i64 = connection
        .query_row(
            "SELECT e.id FROM history_endpoint e JOIN server s ON s.id = e.server_id
             WHERE e.kind = 'player' AND s.public_id = ?1",
            [record.played_on_server.as_bytes()],
            |row| row.get(0),
        )
        .optional()?
        .ok_or(Error::BadListen(
            "the server that played it has no player endpoint here",
        ))?;
    let existing: Option<(i64, String)> = connection
        .query_row(
            "SELECT id, payload_json FROM history_observation WHERE endpoint_id = ?1 AND source_key = ?2",
            (endpoint, record.listen_id.as_bytes()),
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((first, stored)) = existing {
        if stored == payload {
            return Ok(Ingested::Duplicate);
        }
        connection.execute(
            "INSERT INTO history_ingest_conflict
               (endpoint_id, source_key, existing_observation_id, payload_json,
                received_at_utc_ms, rev)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            (
                endpoint,
                record.listen_id.as_bytes(),
                first,
                payload,
                received_at_ms,
                write.rev(),
            ),
        )?;
        return Ok(Ingested::Conflict);
    }
    let one = |sql: &str, id: &Id| -> Result<Option<i64>, Error> {
        Ok(connection
            .query_row(sql, [id.as_bytes()], |row| row.get(0))
            .optional()?)
    };
    let source_item = one(
        "SELECT id FROM source_item WHERE public_id = ?1",
        &record.source_item,
    )?
    .ok_or(Error::BadListen("its source item is not in this library"))?;
    let revision_is_its: bool = connection.query_row(
        "SELECT EXISTS (SELECT 1 FROM source_item_revision WHERE source_item_id = ?1 AND revision_no = ?2)",
        (source_item, record.source_revision),
        |row| row.get(0),
    )?;
    if !revision_is_its {
        return Err(Error::BadListen(
            "its revision is not one of its source item's",
        ));
    }
    let listener = one(
        "SELECT id FROM listener WHERE public_id = ?1",
        &record.listener,
    )?
    .ok_or(Error::BadListen("its listener is not in this library"))?;
    let server = one(
        "SELECT id FROM server WHERE public_id = ?1",
        &record.played_on_server,
    )?
    .ok_or(Error::BadListen(
        "the server that played it is not in this library",
    ))?;
    let appearance = one(
        "SELECT id FROM track_appearance WHERE public_id = ?1",
        &record.appearance,
    )?;
    let asset = one(
        "SELECT id FROM audio_asset WHERE public_id = ?1",
        &record.asset,
    )?;
    if StartReason::parse(&record.start_reason).is_none()
        || EndReason::parse(&record.end_reason).is_none()
        || ContextKind::parse(&record.context_kind).is_none()
    {
        return Err(Error::BadListen(
            "a reason or a context it names is not one there is",
        ));
    }
    // What the sender knew of its own wall clock: its zone, the offset,
    // and the local date and hour at the start. Kept as evidence; the
    // play's own date comes from the corrected instant.
    let local = record
        .zone
        .as_deref()
        .and_then(|zone| derive::local_time(record.started_at_ms, zone));
    connection.execute(
        "INSERT INTO history_observation
           (endpoint_id, source_key, kind, source_item_id, listener_id, captured_at_utc_ms,
            received_at_utc_ms, sent_at_utc_ms, sent_mono_ms, sent_boot_id, clock_skew_ms,
            zone_name, zone_basis, utc_offset_minutes, local_date, local_hour, payload_json, rev)
         VALUES (?1, ?2, 'listen', ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
        rusqlite::params![
            endpoint,
            record.listen_id.as_bytes(),
            source_item,
            listener,
            record.ended_at_ms,
            received_at_ms,
            stamps.sent_at_ms,
            stamps.sent_mono_ms,
            stamps.sent_boot.as_bytes(),
            stamps.sent_at_ms - received_at_ms,
            record.zone,
            if record.zone.is_some() { "captured" } else { "none" },
            local.map(|local| local.offset_minutes),
            local.map(|local| local.date),
            local.map(|local| local.hour),
            payload,
            write.rev(),
        ],
    )?;
    let observation = connection.last_insert_rowid();
    let ranges = serde_json::to_string(&record.heard_ranges)?;
    connection.execute(
        "INSERT INTO observation_listen
           (observation_id, played_on_server_id, actor_session_pid, source_revision_no,
            queued_appearance_id, audio_asset_id, copy_kind, started_at_utc_ms, ended_at_utc_ms,
            started_mono_ms, boot_id, start_reason, continues_source_key, start_position_ms,
            end_position_ms, max_position_ms, track_duration_ms, ms_played, heard_ranges_json,
            seek_count, pause_count, end_reason, context_kind, context_ref, shuffle, rev,
            ended_mono_ms, heard_ranges_approximate)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17,
                 ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28)",
        rusqlite::params![
            observation,
            server,
            record.actor_session.as_ref().map(Id::as_bytes),
            record.source_revision,
            appearance,
            asset,
            record.copy_kind,
            record.started_at_ms,
            record.ended_at_ms,
            record.started_mono_ms,
            record.boot.as_bytes(),
            record.start_reason,
            record.continues.as_ref().map(Id::as_bytes),
            record.start_position_ms,
            record.end_position_ms,
            record.max_position_ms,
            record.track_duration_ms,
            record.ms_played,
            ranges,
            record.seek_count,
            record.pause_count,
            record.end_reason,
            record.context_kind,
            record.context_ref.as_ref().map(Id::as_bytes),
            record.shuffle,
            write.rev(),
            record.ended_mono_ms,
            record.heard_ranges_approximate,
        ],
    )?;
    rederive(write, &BTreeSet::from([observation]), rule)?;
    crate::delivery_store::reconcile_late_listen(write, observation, rule)?;
    Ok(Ingested::Stored(write.rev()))
}

fn play_rows(
    connection: &Connection,
    filter: &str,
    params: &[&dyn rusqlite::ToSql],
) -> Result<Vec<PlayRow>, Error> {
    // A play that is not bound shows the appearance its first listen
    // queued, and one another service reported shows what it named.
    let mut statement = connection.prepare(&format!(
        "SELECT pe.id, pe.public_id, pe.event_at_utc_ms, pe.local_date, pe.local_hour,
                pe.zone_name, pe.utc_offset_minutes, pe.clock_basis, ta.public_id, r.public_id,
                coalesce(shown.display_title, r.display_title, imp.title),
                coalesce(ac.display_text, imp.artist),
                coalesce(rel.display_title, imp.album), pe.ms_played, pe.track_duration_ms,
                pe.outcome, pe.counts, pe.excluded
         FROM play_event pe
         LEFT JOIN observation_import imp ON imp.observation_id = (
           SELECT peo.observation_id FROM play_event_observation peo
           WHERE peo.play_event_id = pe.id AND peo.role = 'primary')
         LEFT JOIN track_appearance ta ON ta.id = pe.appearance_id
         LEFT JOIN recording r ON r.id = pe.recording_id
         LEFT JOIN track_appearance shown ON shown.id = coalesce(
           pe.appearance_id,
           CASE WHEN pe.recording_id IS NULL THEN
             (SELECT l.queued_appearance_id FROM play_event_observation peo
              JOIN observation_listen l ON l.observation_id = peo.observation_id
              WHERE peo.play_event_id = pe.id AND peo.role = 'primary')
           END)
         LEFT JOIN artist_credit ac
           ON ac.id = coalesce(shown.display_artist_credit_id, r.display_artist_credit_id)
         LEFT JOIN release rel ON rel.id = shown.release_id
         {filter}"
    ))?;
    let found: Vec<(i64, PlayRow)> = statement
        .query_map(params, |row| {
            let id = |column: usize| -> rusqlite::Result<Option<Id>> {
                Ok(row.get::<_, Option<[u8; 16]>>(column)?.map(Id::from_bytes))
            };
            Ok((
                row.get(0)?,
                PlayRow {
                    play_id: Id::from_bytes(row.get(1)?),
                    at_ms: row.get(2)?,
                    local_date: row.get(3)?,
                    local_hour: row.get(4)?,
                    zone: row.get(5)?,
                    offset_minutes: row.get(6)?,
                    clock_basis: row.get(7)?,
                    appearance: id(8)?,
                    recording: id(9)?,
                    title: row.get(10)?,
                    artist: row.get(11)?,
                    album: row.get(12)?,
                    ms_played: row.get(13)?,
                    track_duration_ms: row.get(14)?,
                    outcome: row.get(15)?,
                    counts: row.get(16)?,
                    excluded: row.get(17)?,
                    listens: Vec::new(),
                },
            ))
        })?
        .collect::<Result<_, _>>()?;
    let mut behind = connection.prepare_cached(
        "SELECT o.source_key, l.started_at_utc_ms, l.ended_at_utc_ms, l.start_reason,
                l.end_reason, l.start_position_ms, l.end_position_ms, l.ms_played,
                l.continues_source_key, o.zone_name
         FROM play_event_observation peo
         JOIN history_observation o ON o.id = peo.observation_id
         JOIN observation_listen l ON l.observation_id = o.id
         WHERE peo.play_event_id = ?1
         ORDER BY peo.role = 'primary' DESC, l.started_at_utc_ms, o.id",
    )?;
    let mut plays = Vec::with_capacity(found.len());
    for (event, mut play) in found {
        play.listens = behind
            .query_map([event], |row| {
                let key = |column: usize| -> rusqlite::Result<Option<Id>> {
                    Ok(row
                        .get::<_, Option<Vec<u8>>>(column)?
                        .and_then(|bytes| <[u8; 16]>::try_from(bytes).ok())
                        .map(Id::from_bytes))
                };
                Ok(ListenRow {
                    listen_id: key(0)?.unwrap_or(Id::from_bytes([0; 16])),
                    started_at_ms: row.get(1)?,
                    ended_at_ms: row.get(2)?,
                    start_reason: row.get(3)?,
                    end_reason: row.get(4)?,
                    start_position_ms: row.get(5)?,
                    end_position_ms: row.get(6)?,
                    ms_played: row.get(7)?,
                    continues: key(8)?,
                    zone: row.get(9)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        plays.push(play);
    }
    Ok(plays)
}

impl LibraryStore {
    /// The one ingest path, for every observation that comes from outside
    /// the library: a listen from a player, or import records from a
    /// history source. Each is stored keyed on its endpoint and its source
    /// key, with its detail, and its play is derived, in one transaction;
    /// a batch of imports moves its cursor in the same transaction. What is
    /// sent again as it was writes nothing. See "Observations and derivation"
    /// in docs/internals/history.md.
    pub fn ingest(&self, observations: Observations<'_>) -> Result<IngestOutcome, Error> {
        self.ingest_with(observations, None)
    }

    /// As [`ingest`](Self::ingest), checking `cancel` inside `BEGIN IMMEDIATE`
    /// before any mutation.
    pub fn ingest_guarded(
        &self,
        observations: Observations<'_>,
        cancel: crate::JobCancel,
    ) -> Result<IngestOutcome, Error> {
        self.ingest_with(observations, Some(cancel))
    }

    fn ingest_with(
        &self,
        observations: Observations<'_>,
        cancel: Option<crate::JobCancel>,
    ) -> Result<IngestOutcome, Error> {
        match observations {
            Observations::Listen { record, stamps } => Ok(IngestOutcome::Listen(
                self.ingest_listen_with(record, stamps, PlayRule::CURRENT, cancel)?,
            )),
            Observations::Imports {
                account,
                batch,
                full,
            } => Ok(IngestOutcome::Imports(self.ingest_import_batch(
                account,
                batch,
                full,
                PlayRule::CURRENT,
                cancel,
            )?)),
        }
    }

    /// Takes in one listen through [`ingest`](Self::ingest): stores it as
    /// an observation keyed on the playing server's endpoint and the
    /// listen's ID, and derives its play, in one transaction. A listen sent
    /// again as it was writes nothing.
    pub fn ingest_listen(
        &self,
        record: &ListenRecord,
        stamps: SendStamps,
    ) -> Result<Ingested, Error> {
        match self.ingest(Observations::Listen { record, stamps })? {
            IngestOutcome::Listen(ingested) => Ok(ingested),
            IngestOutcome::Imports(_) => Err(Error::Corrupt("a listen ingested as imports")),
        }
    }

    /// As [`ingest_listen`](Self::ingest_listen), judged by `rule`.
    #[cfg(test)]
    pub(crate) fn ingest_listen_by(
        &self,
        record: &ListenRecord,
        stamps: SendStamps,
        rule: PlayRule,
    ) -> Result<Ingested, Error> {
        self.ingest_listen_with(record, stamps, rule, None)
    }

    fn ingest_listen_with(
        &self,
        record: &ListenRecord,
        stamps: SendStamps,
        rule: PlayRule,
        cancel: Option<crate::JobCancel>,
    ) -> Result<Ingested, Error> {
        if let Some(cancel) = &cancel {
            cancel.check()?;
        }
        // Received now, as the send's stamps were taken: the age and the
        // skew are measured from here, and the wait for the writer is no
        // part of either.
        let received_at_ms = self.clock_now_ms();
        let payload = serde_json::to_string(record)?;
        // Sent again as it was: answered from a read, so that no library
        // revision is taken for nothing.
        let (server, key, check) = (record.played_on_server, record.listen_id, payload.clone());
        let stored: Option<String> = self.read(|connection| {
            Ok(connection
                .query_row(
                    "SELECT o.payload_json FROM history_observation o
                     JOIN history_endpoint e ON e.id = o.endpoint_id
                     JOIN server s ON s.id = e.server_id
                     WHERE e.kind = 'player' AND s.public_id = ?1 AND o.source_key = ?2",
                    (server.as_bytes(), key.as_bytes()),
                    |row| row.get(0),
                )
                .optional()?)
        })?;
        if stored.as_deref() == Some(check.as_str()) {
            return Ok(Ingested::Duplicate);
        }
        let record = record.clone();
        let written = self.write_guarded(
            WriteRequest {
                method: INGEST_METHOD,
                command: None,
                client: None,
            },
            cancel,
            move |write| {
                let ingested = ingest(write, &record, stamps, received_at_ms, &payload, rule)?;
                Ok(match ingested {
                    Ingested::Stored(_) => "stored",
                    Ingested::Duplicate => "duplicate",
                    Ingested::Conflict => "conflict",
                }
                .to_owned())
            },
        )?;
        Ok(match written.result_json.as_str() {
            "stored" => Ingested::Stored(written.revision),
            "conflict" => Ingested::Conflict,
            _ => Ingested::Duplicate,
        })
    }

    /// Drops every play and derives them all again from the observations,
    /// in one transaction, under the rule in force, and returns what
    /// `receipt` makes of what it did. With `dry_run` nothing is kept, and
    /// the answer says what would change. See "Incremental updates and
    /// rebuilds" in docs/internals/history.md.
    pub fn rebuild_history(
        &self,
        request: WriteRequest,
        dry_run: bool,
        receipt: impl FnOnce(Rebuilt) -> String + Send + 'static,
    ) -> Result<String, Error> {
        self.rebuild_history_by(request, dry_run, PlayRule::CURRENT, receipt)
    }

    /// As [`rebuild_history`](Self::rebuild_history), under `rule`.
    pub(crate) fn rebuild_history_by(
        &self,
        request: WriteRequest,
        dry_run: bool,
        rule: PlayRule,
        receipt: impl FnOnce(Rebuilt) -> String + Send + 'static,
    ) -> Result<String, Error> {
        let rebuild = move |write: &Write<'_>| -> Result<(Vec<String>, Vec<String>), Error> {
            let connection = write.connection();
            let before = derived(connection)?;
            derive_every_play_by(write, rule)?;
            Ok((before, derived(connection)?))
        };
        if dry_run {
            // The rows a rebuild would write are worked out by writing them
            // in a transaction that is then rolled back, so that a dry run
            // and a rebuild can never disagree.
            let request = WriteRequest {
                command: None,
                ..request
            };
            let outcome = self.write(request, move |write| {
                let (before, after) = rebuild(write)?;
                Err(Error::RolledBack(before, after))
            });
            return match outcome {
                Err(Error::RolledBack(before, after)) => Ok(receipt(Rebuilt {
                    before: before.len() as i64,
                    after: after.len() as i64,
                    changed: changed(&before, &after),
                    revision: None,
                })),
                Err(error) => Err(error),
                Ok(_) => Err(Error::Corrupt("a dry run that was kept")),
            };
        }
        let written = self.write(request, move |write| {
            let (before, after) = rebuild(write)?;
            Ok(receipt(Rebuilt {
                before: before.len() as i64,
                after: after.len() as i64,
                changed: changed(&before, &after),
                revision: Some(write.rev()),
            }))
        })?;
        Ok(written.result_json)
    }

    /// The zones `listener` was in, by date, as the library has them.
    pub fn zone_periods_of(&self, listener: Id) -> Result<Vec<ZonePeriodRow>, Error> {
        self.read(move |connection| {
            let mut statement = connection.prepare(
                "SELECT zone_name, valid_from_ms, valid_to_ms FROM listener_zone_period
                 WHERE listener_id = (SELECT id FROM listener WHERE public_id = ?1)
                 ORDER BY valid_from_ms IS NOT NULL, valid_from_ms, id",
            )?;
            let rows = statement.query_map([listener.as_bytes()], |row| {
                Ok(ZonePeriodRow {
                    zone: row.get(0)?,
                    from_ms: row.get(1)?,
                    to_ms: row.get(2)?,
                })
            })?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
    }

    /// Makes `periods` the zones `listener` was in, in place of what was
    /// there, and derives every play again, in one transaction: a zone that
    /// was assumed is worked out at derive time, so a corrected one moves
    /// the local dates of the plays it covers. `receipt` makes the result
    /// from how many plays changed. See "Local dates and zone periods" in
    /// docs/internals/history.md.
    pub fn set_zone_periods(
        &self,
        request: WriteRequest,
        listener: Id,
        periods: Vec<ZonePeriodRow>,
        receipt: impl FnOnce(i64) -> String + Send + 'static,
    ) -> Result<String, Error> {
        for period in &periods {
            if !derive::known_zone(&period.zone) {
                return Err(Error::Invalid("a zone the zone database does not know"));
            }
            if let (Some(from), Some(to)) = (period.from_ms, period.to_ms)
                && from >= to
            {
                return Err(Error::Invalid("a period that ends before it begins"));
            }
        }
        let written = self.write(request, move |write| {
            let connection = write.connection();
            let listener: i64 = connection
                .query_row(
                    "SELECT id FROM listener WHERE public_id = ?1",
                    [listener.as_bytes()],
                    |row| row.get(0),
                )
                .optional()?
                .ok_or(Error::NotFound("no such listener"))?;
            connection.execute(
                "DELETE FROM listener_zone_period WHERE listener_id = ?1",
                [listener],
            )?;
            for period in &periods {
                connection.execute(
                    "INSERT INTO listener_zone_period
                       (listener_id, zone_name, valid_from_ms, valid_to_ms, rev)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    (
                        listener,
                        &period.zone,
                        period.from_ms,
                        period.to_ms,
                        write.rev(),
                    ),
                )?;
            }
            let before = derived(connection)?;
            derive_every_play(write)?;
            Ok(receipt(changed(&before, &derived(connection)?)))
        })?;
        Ok(written.result_json)
    }

    /// The derived tables as a rebuild compares them.
    pub fn derived_history(&self) -> Result<Vec<String>, Error> {
        self.read(derived)
    }

    /// Whether any play was derived under an earlier rule or grouper.
    pub fn history_needs_rebuild(&self) -> Result<bool, Error> {
        self.read(|connection| {
            Ok(connection.query_row(
                "SELECT EXISTS (SELECT 1 FROM play_event WHERE play_rule_version <> ?1 OR grouper_version <> ?2)",
                [PlayRule::CURRENT.version, derive::GROUPER_VERSION],
                |row| row.get(0),
            )?)
        })
    }

    /// The newest plays of `listener`, newest first, at most `limit`, and
    /// when `after` is given only those after it. Plays are in the order of
    /// their instant and then of their ID, plays with no date last, so a
    /// page can end between two plays of the same instant and the next
    /// begins with the other.
    pub fn recent_plays(
        &self,
        listener: Id,
        limit: u32,
        after: Option<PlayCursor>,
    ) -> Result<Vec<PlayRow>, Error> {
        let at = after.and_then(|cursor| cursor.at_ms);
        let id = after.map(|cursor| cursor.play_id.as_bytes().to_vec());
        self.read_snapshot(|connection| {
            play_rows(
                connection,
                "WHERE pe.listener_id = (SELECT id FROM listener WHERE public_id = ?1)
                   AND (?2 IS NULL
                        OR (?3 IS NOT NULL
                            AND (pe.event_at_utc_ms < ?3
                                 OR (pe.event_at_utc_ms = ?3 AND pe.public_id < ?2)
                                 OR pe.event_at_utc_ms IS NULL))
                        OR (?3 IS NULL AND pe.event_at_utc_ms IS NULL AND pe.public_id < ?2))
                 ORDER BY pe.event_at_utc_ms DESC, pe.public_id DESC LIMIT ?4",
                &[&listener.as_bytes().as_slice(), &id, &at, &limit],
            )
        })
    }

    /// How often `listener` played each recording, and when last, in one
    /// pass: what the Songs view and a wall read. Only plays that count
    /// and are not excluded, dated or not, from every source. Read through
    /// the partial index on counted plays; there is no rollup table
    /// ("Queries" in docs/internals/history.md).
    pub fn play_counts(&self, listener: Id) -> Result<Vec<(Id, i64, Option<i64>)>, Error> {
        self.read(move |connection| {
            let mut statement = connection.prepare_cached(
                "SELECT r.public_id, counted.plays, counted.last FROM
                   (SELECT recording_id, count(*) AS plays, max(event_at_utc_ms) AS last
                    FROM play_event
                    WHERE listener_id = (SELECT id FROM listener WHERE public_id = ?1)
                      AND counts = 1 AND excluded = 0 AND recording_id IS NOT NULL
                    GROUP BY recording_id) counted
                 JOIN recording r ON r.id = counted.recording_id",
            )?;
            let rows = statement.query_map([listener.as_bytes()], |row| {
                Ok((Id::from_bytes(row.get(0)?), row.get(1)?, row.get(2)?))
            })?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
    }

    /// How often `listener` played one artist's recordings in each year,
    /// by the plays' local dates, oldest year first: the bars on an
    /// artist's page. Only dated plays that count and are not excluded.
    pub fn artist_plays_by_year(&self, listener: Id, artist: Id) -> Result<Vec<(i64, i64)>, Error> {
        self.read(move |connection| {
            let mut statement = connection.prepare_cached(
                "SELECT pe.local_date / 10000 AS year, count(*) FROM play_event pe
                 WHERE pe.listener_id = (SELECT id FROM listener WHERE public_id = ?1)
                   AND pe.counts = 1 AND pe.excluded = 0 AND pe.local_date IS NOT NULL
                   AND pe.recording_id IN (
                     SELECT r.id FROM artist a
                     JOIN artist_credit_name acn ON acn.artist_id = a.id
                     JOIN recording r ON r.display_artist_credit_id = acn.artist_credit_id
                     WHERE a.canonical_id = (SELECT canonical_id FROM artist WHERE public_id = ?2)
                       AND r.redirect_to_id IS NULL)
                 GROUP BY year ORDER BY year",
            )?;
            let rows = statement.query_map((listener.as_bytes(), artist.as_bytes()), |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
    }

    /// The plays of `listener` that count, are not excluded and fell on
    /// `month_day` (MMDD) of any year, newest first.
    pub fn plays_on_this_day(&self, listener: Id, month_day: i64) -> Result<Vec<PlayRow>, Error> {
        self.read_snapshot(|connection| {
            play_rows(
                connection,
                "WHERE pe.listener_id = (SELECT id FROM listener WHERE public_id = ?1)
                   AND pe.month_day = ?2 AND pe.counts = 1 AND pe.excluded = 0
                 ORDER BY pe.local_date DESC, pe.event_at_utc_ms DESC",
                &[&listener.as_bytes().as_slice(), &month_day],
            )
        })
    }

    /// The library's listener the player plays for: its first.
    pub fn default_listener(&self) -> Result<Option<Id>, Error> {
        self.first_listener()
    }

    /// Records a `set_recording` correction on the manual endpoint, moving
    /// the targeted observations to `recording` (and optional `appearance`),
    /// or leaving them unbound when `recording` is `None`, above their source
    /// item's assignment. Re-derives the affected plays in the same
    /// transaction and returns the write receipt and the correction's key.
    ///
    /// `recording` and `appearance` are stored as named (never rewritten to
    /// their canonical rows), and reads follow redirects at derivation time.
    pub fn set_observation_recording(
        &self,
        request: WriteRequest,
        targets: CorrectionTargets,
        recording: Option<Id>,
        appearance: Option<Id>,
        note: Option<String>,
    ) -> Result<(crate::Written, Id), Error> {
        if recording.is_none() && appearance.is_some() {
            return Err(Error::Invalid("an appearance requires a recording"));
        }
        match &targets {
            CorrectionTargets::Observations(list) if list.is_empty() => {
                return Err(Error::Invalid(
                    "a correction must target at least one observation",
                ));
            }
            CorrectionTargets::Window { from_ms, to_ms, .. } => {
                for bound in [from_ms, to_ms].into_iter().flatten() {
                    if jiff::Timestamp::from_millisecond(*bound).is_err() {
                        return Err(Error::Invalid("a window bound is out of timestamp range"));
                    }
                }
                if let (Some(from), Some(to)) = (from_ms, to_ms)
                    && from >= to
                {
                    return Err(Error::Invalid("a window that ends before it begins"));
                }
            }
            _ => {}
        }
        let session = request.client.map(|client| client.session);
        let written = self.write(request, move |write| {
            let connection = write.connection();
            let manual: i64 = connection
                .query_row(
                    "SELECT id FROM history_endpoint WHERE kind = 'manual'",
                    [],
                    |row| row.get(0),
                )
                .optional()?
                .ok_or(Error::Corrupt("a library with no manual endpoint"))?;
            let listener: i64 = connection
                .query_row("SELECT min(id) FROM listener", [], |row| {
                    row.get::<_, Option<i64>>(0)
                })?
                .ok_or(Error::Invalid("the library has no listener"))?;

            let named_recording: Option<(i64, i64)> = match recording {
                Some(id) => Some(
                    connection
                        .query_row(
                            "SELECT id, canonical_id FROM recording WHERE public_id = ?1",
                            [id.as_bytes()],
                            |row| Ok((row.get(0)?, row.get(1)?)),
                        )
                        .optional()?
                        .ok_or(Error::NotFound("no such recording"))?,
                ),
                None => None,
            };
            let (recording_row, appearance_row): (Option<i64>, Option<i64>) =
                match (named_recording, appearance) {
                    (Some((_, rec_canonical)), Some(app_id)) => {
                        let found: Option<(i64, i64, i64)> = connection
                            .query_row(
                                "SELECT ta.id, ta.recording_id, r.canonical_id
                                 FROM track_appearance ta
                                 JOIN recording r ON r.id = ta.recording_id
                                 WHERE ta.public_id = ?1",
                                [app_id.as_bytes()],
                                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                            )
                            .optional()?;
                        let Some((app_row, app_rec, app_rec_canonical)) = found else {
                            return Err(Error::NotFound("no such track appearance"));
                        };
                        if app_rec_canonical != rec_canonical {
                            return Err(Error::Invalid(
                                "that track appearance does not belong to that recording",
                            ));
                        }
                        // Like an assignment, an appearance keeps its own
                        // recording_id (satisfying the composite FK), and reads
                        // follow recording.canonical_id.
                        (Some(app_rec), Some(app_row))
                    }
                    (Some((rec_row, _)), None) => (Some(rec_row), None),
                    (None, _) => (None, None),
                };

            let (target_rows, window_row): (Vec<i64>, Option<Window>) = match targets {
                CorrectionTargets::Observations(list) => {
                    let mut rows = Vec::with_capacity(list.len());
                    let mut seen = BTreeSet::new();
                    let mut find = connection.prepare_cached(
                        "SELECT o.id, o.kind FROM history_observation o
                         JOIN history_endpoint e ON e.id = o.endpoint_id
                         WHERE e.public_id = ?1 AND o.source_key = ?2",
                    )?;
                    for (endpoint_id, source_key) in list {
                        let found: Option<(i64, String)> = find
                            .query_row((endpoint_id.as_bytes(), source_key.as_slice()), |row| {
                                Ok((row.get(0)?, row.get(1)?))
                            })
                            .optional()?;
                        let Some((obs_id, kind)) = found else {
                            return Err(Error::NotFound("no such observation"));
                        };
                        if kind == "correction" {
                            return Err(Error::Invalid(
                                "a recording correction cannot target another correction",
                            ));
                        }
                        if seen.insert(obs_id) {
                            rows.push(obs_id);
                        }
                    }
                    (rows, None)
                }
                CorrectionTargets::Window {
                    endpoint,
                    from_ms,
                    to_ms,
                    source_item,
                } => {
                    let endpoint_row: i64 = connection
                        .query_row(
                            "SELECT id FROM history_endpoint WHERE public_id = ?1",
                            [endpoint.as_bytes()],
                            |row| row.get(0),
                        )
                        .optional()?
                        .ok_or(Error::NotFound("no such history endpoint"))?;
                    let source_item_row: Option<i64> = match source_item {
                        Some(item_id) => Some(
                            connection
                                .query_row(
                                    "SELECT id FROM source_item WHERE public_id = ?1",
                                    [item_id.as_bytes()],
                                    |row| row.get(0),
                                )
                                .optional()?
                                .ok_or(Error::NotFound("no such source item"))?,
                        ),
                        None => None,
                    };
                    (
                        Vec::new(),
                        Some(Window {
                            endpoint: endpoint_row,
                            from_ms,
                            to_ms,
                            source_item: source_item_row,
                        }),
                    )
                }
            };

            let correction_key = write.new_id()?;
            let payload = serde_json::json!({
                "verb": "set_recording",
                "recording": recording.map(|id| id.to_string()),
                "appearance": appearance.map(|id| id.to_string()),
                "note": note,
            })
            .to_string();
            connection.execute(
                "INSERT INTO history_observation
                   (endpoint_id, source_key, kind, listener_id, captured_at_utc_ms,
                    received_at_utc_ms, zone_basis, payload_json, rev)
                 VALUES (?1, ?2, 'correction', ?3, ?4, ?4, 'none', ?5, ?6)",
                rusqlite::params![
                    manual,
                    correction_key.as_bytes(),
                    listener,
                    write.now_ms(),
                    payload,
                    write.rev(),
                ],
            )?;
            let correction_obs = connection.last_insert_rowid();
            connection.execute(
                "INSERT INTO observation_correction
                   (observation_id, verb, target_endpoint_id, target_from_utc_ms,
                    target_to_utc_ms, note, actor_session_pid, rev)
                 VALUES (?1, 'set_recording', ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![
                    correction_obs,
                    window_row.map(|window| window.endpoint),
                    window_row.and_then(|window| window.from_ms),
                    window_row.and_then(|window| window.to_ms),
                    note,
                    session.as_ref().map(Id::as_bytes),
                    write.rev(),
                ],
            )?;
            connection.execute(
                "INSERT INTO correction_recording
                   (correction_id, recording_id, appearance_id, source_item_id, rev)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![
                    correction_obs,
                    recording_row,
                    appearance_row,
                    window_row.and_then(|window| window.source_item),
                    write.rev(),
                ],
            )?;
            let mut affected = BTreeSet::new();
            for target in &target_rows {
                connection.execute(
                    "INSERT INTO correction_target (correction_id, target_observation_id, rev)
                     VALUES (?1, ?2, ?3)",
                    (correction_obs, target, write.rev()),
                )?;
                affected.insert(*target);
            }
            if let Some(window) = window_row {
                affected.extend(window_observations(connection, window)?);
            }
            if !affected.is_empty() {
                rederive(write, &affected, PlayRule::CURRENT)?;
            }
            Ok(serde_json::to_string(&correction_key.to_string())?)
        })?;
        let key: String = serde_json::from_str(&written.result_json)?;
        let key = key
            .parse()
            .map_err(|_| Error::Corrupt("a correction's ID"))?;
        Ok((written, key))
    }

    /// Records a `revoke` correction targeting an existing correction on the
    /// manual endpoint, and re-derives every play it touched.
    pub fn revoke_correction(
        &self,
        request: WriteRequest,
        correction: Id,
        note: Option<String>,
    ) -> Result<(crate::Written, Id), Error> {
        let session = request.client.map(|client| client.session);
        let written = self.write(request, move |write| {
            let connection = write.connection();
            let target: i64 = connection
                .query_row(
                    "SELECT o.id FROM history_observation o
                     JOIN history_endpoint e ON e.id = o.endpoint_id
                     WHERE e.kind = 'manual' AND o.source_key = ?1 AND o.kind = 'correction'",
                    [correction.as_bytes()],
                    |row| row.get(0),
                )
                .optional()?
                .ok_or(Error::NotFound("no such correction"))?;
            let key = revoke_by(write, target, note.as_deref(), session, PlayRule::CURRENT)?;
            Ok(serde_json::to_string(&key.to_string())?)
        })?;
        let key: String = serde_json::from_str(&written.result_json)?;
        let key = key
            .parse()
            .map_err(|_| Error::Corrupt("a correction's ID"))?;
        Ok((written, key))
    }
}

/// How many plays are new, gone or different between two sets of derived
/// rows. Each row begins with its play's ID.
fn changed(before: &[String], after: &[String]) -> i64 {
    let by_id = |rows: &[String]| -> BTreeMap<String, String> {
        rows.iter()
            .map(|row| {
                let id = row.split('|').next().unwrap_or_default().to_owned();
                (id, row.clone())
            })
            .collect()
    };
    let (before, after) = (by_id(before), by_id(after));
    let ids: BTreeSet<&String> = before.keys().chain(after.keys()).collect();
    ids.into_iter()
        .filter(|id| before.get(*id) != after.get(*id))
        .count() as i64
}

#[cfg(test)]
mod tests;
