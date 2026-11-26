//! Read-only database inspection, provenance, conflicts, and revision log
//! types (`inspect.*` and `library.revisions`). See "Database inspection" in
//! docs/internals/contract.md.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::Id;

/// The most rows a single `inspect.table`, `inspect.conflicts`, or
/// `library.revisions` call may return.
pub const MAX_INSPECT_ROWS: u32 = 200;

/// Which SQLite database an inspection call addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum InspectDb {
    /// The portable library database (`library.db`).
    Library,
    /// The machine-local server database (`server.db`).
    Server,
}

impl InspectDb {
    /// The wire name of this database.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Library => "library",
            Self::Server => "server",
        }
    }
}

/// Whether a schema object is a regular table or a view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum TableObjectKind {
    /// A regular SQLite table.
    Table,
    /// A SQLite view.
    View,
}

/// The durability and architectural role of a database table or view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum TableLayer {
    /// Immutable or raw facts captured from files, sidecars, lookups, or history sources.
    Evidence,
    /// Durable owner or resolver choices, bindings, ratings, edits, and answers.
    Decision,
    /// Ownership, revisions, receipts, settings, queues, jobs, and server state.
    Bookkeeping,
    /// Derived projections and caches that can be recomputed from durable tables.
    Rebuildable,
}

/// Row sort order for `inspect.table`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum InspectOrder {
    /// Ascending by primary key / rowid.
    #[default]
    Asc,
    /// Descending by primary key / rowid.
    Desc,
}

/// One applied migration recorded in `schema_migration`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct MigrationSummary {
    /// The migration's monotonic version number.
    pub version: u32,
    /// The migration's filename stem.
    pub name: String,
    /// Lowercase SHA-256 hex digest of the applied migration SQL.
    pub checksum: String,
    /// Unix timestamp in milliseconds when the migration was applied.
    pub applied_at_ms: i64,
}

/// One column in a SQLite table or view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ColumnSchema {
    /// The column's name.
    pub name: String,
    /// The declared SQLite column type (`INTEGER`, `TEXT`, `BLOB`, `REAL`, ...).
    pub col_type: String,
    /// Whether the column is declared `NOT NULL`.
    pub not_null: bool,
    /// Whether the column participates in the primary key.
    pub pk: bool,
    /// SQL default expression, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub default_value: Option<String>,
    /// Referenced foreign-key table, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub fk_table: Option<String>,
    /// Referenced foreign-key column, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub fk_column: Option<String>,
}

/// One index on a SQLite table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct IndexSchema {
    /// The index's name.
    pub name: String,
    /// Whether the index enforces uniqueness.
    pub unique: bool,
    /// Whether the index carries a `WHERE` filter.
    pub partial: bool,
    /// Indexed column names in index order.
    pub columns: Vec<String>,
}

/// Schema and row summary for one table or view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct TableSchema {
    /// The table or view's name.
    pub name: String,
    /// Whether this object is a table or a view.
    pub kind: TableObjectKind,
    /// Architectural durability layer.
    pub layer: TableLayer,
    /// Whether `inspect.table` permits reading rows from this table.
    pub browsable: bool,
    /// Current row count, when cheaply readable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub row_count: Option<u64>,
    /// Columns in declaration order.
    pub columns: Vec<ColumnSchema>,
    /// Secondary and unique indexes on this table.
    pub indexes: Vec<IndexSchema>,
    /// Trigger names attached to this table.
    pub triggers: Vec<String>,
}

/// Storage, migration, and schema summary for one SQLite database.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct DbSummary {
    /// Which database this describes.
    pub db: InspectDb,
    /// Current migration version.
    pub schema_version: u32,
    /// Size of the main database file on disk in bytes. Left out for in-memory databases.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub file_bytes: Option<u64>,
    /// Size of the `-wal` file on disk in bytes (0 when absent). Left out for in-memory databases.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub wal_bytes: Option<u64>,
    /// SQLite page size in bytes.
    pub page_size: u64,
    /// Total SQLite pages allocated.
    pub page_count: u64,
    /// Unused pages on SQLite's freelist.
    pub freelist_count: u64,
    /// Applied migrations in version order.
    pub migrations: Vec<MigrationSummary>,
    /// Tables and views with their columns, indexes, triggers, and counts.
    pub tables: Vec<TableSchema>,
}

/// Operational queue depths across `server.db` and (on a hub) `library.db`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct QueueDepths {
    /// Rows waiting in `server.db`'s `outbox`.
    pub outbox: u64,
    /// Open listens currently staged in `server.db`'s `listen_staging`.
    pub listen_staging_open: u64,
    /// Closed listens still staged in `server.db`'s `listen_staging`.
    pub listen_staging_closed: u64,
    /// Pending rows in `library.db`'s `history_delivery`.
    pub history_delivery_pending: u64,
    /// Refused rows in `library.db`'s `history_delivery`.
    pub history_delivery_refused: u64,
    /// Unknown-outcome rows in `library.db`'s `history_delivery`.
    pub history_delivery_unknown: u64,
    /// Recorded rows in `library.db`'s `history_ingest_conflict`.
    pub history_ingest_conflicts: u64,
}

/// The result of `inspect.db`: schema, sizes, migrations, and queue depths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct DbInspection {
    /// This server's `server.db` summary.
    pub server: DbSummary,
    /// This hub's `library.db` summary. Left out on satellites or unassigned servers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub library: Option<DbSummary>,
    /// Current `library_meta.rev`, when a library is open on this server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub library_revision: Option<u64>,
    /// Queue and conflict counts.
    pub queues: QueueDepths,
}

/// The params of `inspect.table`: paged read of one allowlisted table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct InspectTableParams {
    /// Which database holds the table.
    pub db: InspectDb,
    /// Exact allowlisted table or view name.
    pub table: String,
    /// Row offset (defaults to 0).
    #[serde(default)]
    pub offset: u64,
    /// Page size (1..=[`MAX_INSPECT_ROWS`], defaults to 50).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub limit: Option<u32>,
    /// Row order by primary key / rowid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub order: Option<InspectOrder>,
    /// Optional column name to filter by exact equality (paired with `equals`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub column: Option<String>,
    /// Exact value to match when `column` is given (matches integers, text, or UUID hex/strings).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub equals: Option<String>,
}

/// The result of `inspect.table`: one page of redacted rows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct TablePage {
    /// Database queried.
    pub db: InspectDb,
    /// Table or view queried.
    pub table: String,
    /// Architectural durability layer.
    pub layer: TableLayer,
    /// Column definitions in row-cell order.
    pub columns: Vec<ColumnSchema>,
    /// Total matching rows in the table.
    pub total: u64,
    /// Offset of the first returned row.
    pub offset: u64,
    /// Effective page limit.
    pub limit: u32,
    /// Whether the page stopped before reaching `limit` to stay within the
    /// response byte budget; callers advance `offset` by `rows.len()`.
    pub truncated: bool,
    /// Rows as positional JSON cell arrays matching `columns`.
    pub rows: Vec<Vec<Value>>,
}

/// Which entity kind `inspect.entity` resolves provenance for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum InspectEntityKind {
    /// A canonical or redirected recording (`recording.public_id`).
    Recording,
    /// A track appearance on a release (`track_appearance.public_id`).
    Appearance,
    /// A release (`release.public_id`).
    Release,
    /// A scanned file or external source item (`source_item.public_id`).
    SourceItem,
    /// A hashed audio stream (`audio_asset.public_id`).
    AudioAsset,
}

/// The params of `inspect.entity`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct InspectEntityParams {
    /// The kind of entity `id` names.
    pub kind: InspectEntityKind,
    /// The entity's public UUID.
    pub id: Id,
}

/// One immutable snapshot in `source_item_revision`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct InspectedSourceRevision {
    /// Monotonic revision number within the source item.
    pub revision_no: u32,
    /// Unix timestamp in milliseconds when the revision was observed.
    pub observed_at_ms: i64,
    /// Lowercase hex digest of `payload_hash`.
    pub payload_hash: String,
    /// Audio asset linked to this revision, if audio was decoded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub audio_asset_id: Option<Id>,
}

/// Hashed audio stream properties from `audio_asset`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct InspectedAudioAsset {
    /// Public UUID of the audio asset.
    pub audio_asset_id: Id,
    /// Hash scheme version.
    pub hash_scheme: u32,
    /// Lowercase BLAKE3 hex digest of the decoded/packet audio stream.
    pub audio_hash: String,
    /// Lowercase FLAC STREAMINFO MD5 hex digest, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub flac_md5: Option<String>,
    /// Codec name (`flac`, `mp3`, `aac`, ...).
    pub codec: String,
    /// Sample rate in Hz.
    pub sample_rate_hz: u32,
    /// Channel layout description (`stereo`, `mono`, ...).
    pub channel_layout: String,
    /// Bits per sample, when lossless/known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub bit_depth: Option<u32>,
    /// Bitrate in kbps, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub bitrate_kbps: Option<u32>,
    /// Whether the audio stream is lossless.
    pub lossless: bool,
    /// Measured duration in milliseconds.
    pub duration_ms: u64,
}

/// One extracted identifier claim from `identifier_claim`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct InspectedIdentifierClaim {
    /// Claim scope (`recording`, `release`, `artist`, ...).
    pub scope: String,
    /// Identifier namespace (`mb_recording`, `isrc`, ...).
    pub namespace: String,
    /// Claimed identifier value.
    pub value: String,
    /// Metadata source kind (`file_tags`, `musicbrainz`, ...).
    pub source: String,
}

/// One extracted metadata claim from `metadata_claim`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct InspectedMetadataClaim {
    /// Claim scope (`recording`, `release`, `artist`, ...).
    pub scope: String,
    /// Field name (`title`, `artist`, `album`, ...).
    pub field: String,
    /// Raw claimed value.
    pub value: String,
    /// Metadata source kind (`file_tags`, `inferred`, ...).
    pub source: String,
}

/// One insert-only binding row from `assignment`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct InspectedAssignment {
    /// Internal assignment row ID.
    pub id: i64,
    /// Bound recording's public ID.
    pub recording_id: Id,
    /// Bound track appearance's public ID, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub appearance_id: Option<Id>,
    /// `identity_decision.id` that created this assignment.
    pub decision_id: i64,
    /// When this assignment took effect.
    pub effective_from_ms: i64,
    /// When a later assignment superseded this row, if superseded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub superseded_at_ms: Option<i64>,
    /// Whether this row is the active assignment.
    pub current: bool,
}

/// One immutable audit row from `identity_decision`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct InspectedIdentityDecision {
    /// Internal decision row ID.
    pub id: i64,
    /// Decision action (`bind`, `unbind`, `merge`, `unmerge`).
    pub action: String,
    /// Resolution method (`same_audio`, `identifier`, `tags_duration`, `new`, `user`, ...).
    pub method: String,
    /// Resolver version that made this decision.
    pub resolver_version: u32,
    /// Distance score in `0..=1`, when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub score: Option<f64>,
    /// Gap to runner-up candidate, when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub runner_up_gap: Option<f64>,
    /// Whether the decision was flagged for review.
    pub needs_review: bool,
    /// Who made the decision (`resolver`, `person`, `agent`).
    pub actor_kind: String,
    /// Optional human/resolver rationale.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub rationale: Option<String>,
    /// Unix timestamp in milliseconds when decided.
    pub decided_at_ms: i64,
    /// Parsed structured evidence payload.
    pub evidence: Value,
}

/// One exclusion preventing a source item from binding to a recording.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct InspectedIdentityExclusion {
    /// Internal exclusion row ID.
    pub id: i64,
    /// Excluded recording's public ID.
    pub recording_id: Id,
    /// Who recorded the exclusion (`person`, `agent`).
    pub actor_kind: String,
    /// Optional reason text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub reason: Option<String>,
    /// Unix timestamp in milliseconds when created.
    pub created_at_ms: i64,
}

/// Full provenance chain for one `source_item`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct InspectedSourceItem {
    /// Public UUID of the source item.
    pub source_item_id: Id,
    /// Source instance name (for example the folder name or Last.fm account).
    pub source_name: String,
    /// Source instance kind (`folder`, `lastfm`, ...).
    pub source_kind: String,
    /// Item kind (`file`, `scrobble_subject`, ...).
    pub item_kind: String,
    /// Server holding the root, when backed by a library root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub server_id: Option<Id>,
    /// Display name of the server holding the root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub server_name: Option<String>,
    /// Logical root name in `library_root`, when backed by a root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub root_name: Option<String>,
    /// Provider key (root-relative path for files, service key for external sources).
    pub rel_path: String,
    /// Absolute filesystem path on this server. Only included when the session holds
    /// `folders:manage` and the root is local to this server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub abs_path: Option<String>,
    /// File size in bytes, for files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub size_bytes: Option<u64>,
    /// File modification timestamp in milliseconds, for files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub mtime_ms: Option<i64>,
    /// When the file was first noticed missing, if missing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub missing_since_ms: Option<i64>,
    /// When this source item was retired, if retired.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub retired_at_ms: Option<i64>,
    /// When this item was first scanned/seen.
    pub first_seen_at_ms: i64,
    /// Current revision's hashed audio stream, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub audio_asset: Option<InspectedAudioAsset>,
    /// Revisions of this source item, newest first.
    pub revisions: Vec<InspectedSourceRevision>,
    /// Identifier claims from the current revision.
    pub identifier_claims: Vec<InspectedIdentifierClaim>,
    /// Metadata claims from the current revision.
    pub metadata_claims: Vec<InspectedMetadataClaim>,
    /// Insert-only assignment history, newest first.
    pub assignments: Vec<InspectedAssignment>,
    /// Identity decisions naming this source item, newest first.
    pub decisions: Vec<InspectedIdentityDecision>,
    /// Identity exclusions on this source item.
    pub exclusions: Vec<InspectedIdentityExclusion>,
}

/// The result of `inspect.entity`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct EntityProvenance {
    /// Requested entity kind.
    pub kind: InspectEntityKind,
    /// Requested entity public ID.
    pub id: Id,
    /// Canonical entity public ID when the requested entity was merged/redirected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub canonical_id: Option<Id>,
    /// Display title of the entity, when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub title: Option<String>,
    /// Display artist credit of the entity, when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub artist: Option<String>,
    /// Underlying source items and their provenance chains.
    pub source_items: Vec<InspectedSourceItem>,
    /// Entity-level merge/unmerge `identity_decision` rows naming this entity or
    /// its canonical target, newest first.
    pub entity_decisions: Vec<InspectedIdentityDecision>,
}

/// The params of `inspect.play`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct InspectPlayParams {
    /// Public ID of the `play_event` to inspect.
    pub play_id: Id,
}

/// One raw `history_observation` attached to a derived `play_event`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct InspectedObservation {
    /// Internal observation row ID.
    pub id: i64,
    /// Reporting endpoint display name.
    pub endpoint_name: String,
    /// Reporting endpoint kind (`player`, `service`, `import`, ...).
    pub endpoint_kind: String,
    /// Observation kind (`listen`, `import`, `device_log`, `correction`).
    pub kind: String,
    /// Role in the play event (`primary` or `corroborating`).
    pub role: String,
    /// Join/dedupe reason (`chain`, `echo`, `repeat`, `start`, `during`, `renamed`, `pin`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub joined_by: Option<String>,
    /// Source key (UUID string when 16 bytes, otherwise lowercase hex).
    pub source_key: String,
    /// Reporter wall clock timestamp in milliseconds.
    pub captured_at_utc_ms: i64,
    /// Hub ingest timestamp in milliseconds.
    pub received_at_utc_ms: i64,
    /// Measured sender-minus-hub clock skew in milliseconds, when stamped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub clock_skew_ms: Option<i64>,
    /// Rendered/reported milliseconds played, when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub ms_played: Option<u64>,
    /// End reason (`finished`, `next`, `crash`, ...), for listens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub end_reason: Option<String>,
    /// Reported artist spelling, for imports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub artist: Option<String>,
    /// Reported title spelling, for imports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub title: Option<String>,
    /// Reported album spelling, for imports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub album: Option<String>,
    /// Verbatim JSON payload stored on the observation.
    pub payload: Value,
}

/// One outbound delivery row from `history_delivery`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct InspectedDelivery {
    /// Internal delivery row ID.
    pub id: i64,
    /// Primary observation row ID.
    pub observation_id: i64,
    /// Destination endpoint display name.
    pub endpoint_name: String,
    /// Delivery state (`pending`, `sending`, `accepted`, `ignored`, `refused`, `unknown`, `withdrawn`).
    pub state: String,
    /// Unix timestamp in seconds sent to the service.
    pub sent_timestamp_s: i64,
    /// Sent artist string.
    pub artist: String,
    /// Sent title string.
    pub title: String,
    /// Sent album string, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub album: Option<String>,
    /// Service-corrected artist string, when accepted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub accepted_artist: Option<String>,
    /// Service-corrected title string, when accepted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub accepted_title: Option<String>,
    /// Service ignore reason code, when ignored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub ignored_code: Option<i64>,
    /// Service detail or refusal/withdrawal reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub detail: Option<String>,
    /// Number of send attempts made.
    pub attempts: u32,
    /// Next retry timestamp in milliseconds, when pending.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub next_attempt_at_ms: Option<i64>,
    /// Last update timestamp in milliseconds.
    pub updated_at_ms: i64,
}

/// One `observation_correction` targeting an observation of a play.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct InspectedPlayCorrection {
    /// Correction's `history_observation.id`.
    pub observation_id: i64,
    /// Correction verb (`exclude`, `set_listener`, `split`, `join`, `set_clock_offset`, `set_recording`, `revoke`).
    pub verb: String,
    /// Optional note attached to the correction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub note: Option<String>,
    /// Specific target observation ID, when targeted directly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub target_observation_id: Option<i64>,
    /// Target recording public ID for `set_recording`, when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub recording_id: Option<Id>,
    /// When the correction observation was recorded.
    pub created_at_ms: i64,
}

/// The result of `inspect.play`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct PlayProvenance {
    /// Public ID of the derived `play_event`.
    pub play_id: Id,
    /// Listener public ID.
    pub listener_id: Id,
    /// Listener display name.
    pub listener_name: String,
    /// Bound canonical recording public ID, if bound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub recording_id: Option<Id>,
    /// Bound track appearance public ID, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub appearance_id: Option<Id>,
    /// Source item public ID, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub source_item_id: Option<Id>,
    /// Display title of the bound recording, if bound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub title: Option<String>,
    /// Display artist of the bound recording, if bound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub artist: Option<String>,
    /// Derived start instant in Unix milliseconds, if dated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub event_at_utc_ms: Option<i64>,
    /// Time anchor (`start`, `end`, `none`).
    pub time_anchor: String,
    /// Clock basis (`hub_age`, `server_wall`, `device_wall`, `device_offset`, `source_utc`, `none`).
    pub clock_basis: String,
    /// Resolved IANA time zone name, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub zone_name: Option<String>,
    /// Zone basis (`captured`, `assumed`, `offset_only`, `none`).
    pub zone_basis: String,
    /// Calendar basis (`zone`, `device_wall`, `offset`, `none`).
    pub calendar_basis: String,
    /// Evidence class (`observed`, `manual`).
    pub evidence_class: String,
    /// Played duration in milliseconds, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub ms_played: Option<u64>,
    /// Track duration in milliseconds, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub track_duration_ms: Option<u64>,
    /// Derived outcome (`listened`, `skipped`, `partial`, `unknown`).
    pub outcome: String,
    /// Whether this play counts toward play totals.
    pub counts: bool,
    /// How `counts` was decided (`play_rule`, `source_verdict`, `device_position`).
    pub count_basis: String,
    /// Play-rule version used to derive this row.
    pub play_rule_version: u32,
    /// Grouper version used to derive this row.
    pub grouper_version: u32,
    /// Whether an active `exclude` correction hides this play.
    pub excluded: bool,
    /// Raw observations that produced or corroborated this play.
    pub observations: Vec<InspectedObservation>,
    /// Outbound scrobble delivery rows for these observations.
    pub deliveries: Vec<InspectedDelivery>,
    /// Corrections targeting any of this play's observations.
    pub corrections: Vec<InspectedPlayCorrection>,
}

/// The params of `inspect.conflicts`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct InspectConflictsParams {
    /// Offset into each list (defaults to 0).
    #[serde(default)]
    pub offset: u64,
    /// Page size (1..=[`MAX_INSPECT_ROWS`], defaults to 50).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub limit: Option<u32>,
}

/// One row from `history_ingest_conflict`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct InspectedIngestConflict {
    /// Internal conflict row ID.
    pub id: i64,
    /// Endpoint display name.
    pub endpoint_name: String,
    /// Endpoint kind.
    pub endpoint_kind: String,
    /// Conflicting source key (UUID string when 16 bytes, otherwise hex).
    pub source_key: String,
    /// Existing observation row ID that already claimed `(endpoint, source_key)`.
    pub existing_observation_id: i64,
    /// Received conflicting payload.
    pub payload: Value,
    /// Unix timestamp in milliseconds when the conflict arrived.
    pub received_at_utc_ms: i64,
    /// Library revision that recorded this conflict.
    pub rev: u64,
}

/// One waiting row from `server.db`'s `outbox`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct InspectedOutboxItem {
    /// Internal outbox row ID.
    pub id: i64,
    /// Outbox item kind (for example `history.ingest`).
    pub kind: String,
    /// Deduplication key (UUID string when 16 bytes, otherwise hex).
    pub dedupe_key: String,
    /// Number of delivery attempts made so far.
    pub attempts: u32,
    /// Next retry timestamp in milliseconds.
    pub next_attempt_at_ms: i64,
    /// Last delivery error message, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub last_error: Option<String>,
    /// Unix timestamp in milliseconds when queued.
    pub created_at_ms: i64,
}

/// The result of `inspect.conflicts`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ConflictsReport {
    /// Total rows in `history_ingest_conflict`.
    pub ingest_conflicts_total: u64,
    /// Paged rows from `history_ingest_conflict`, newest first.
    pub ingest_conflicts: Vec<InspectedIngestConflict>,
    /// Total stuck, refused, or unknown rows in `history_delivery`.
    pub stuck_deliveries_total: u64,
    /// Paged stuck, refused, or unknown rows in `history_delivery`, newest first.
    pub stuck_deliveries: Vec<InspectedDelivery>,
    /// Total rows waiting in this server's `outbox`.
    pub outbox_total: u64,
    /// Paged rows from this server's `outbox`, oldest due first.
    pub outbox: Vec<InspectedOutboxItem>,
}

/// The params of `library.revisions`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct LibraryRevisionsParams {
    /// Offset from the newest revision (defaults to 0).
    #[serde(default)]
    pub offset: u64,
    /// Page size (1..=[`MAX_INSPECT_ROWS`], defaults to 50).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub limit: Option<u32>,
}

/// One row from `library_revision`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct LibraryRevisionEntry {
    /// Monotonic library revision number.
    pub rev: u64,
    /// Client command ID, if the write came from an idempotent command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub command_id: Option<Id>,
    /// Contract method or internal writer name (`scan.step`, `ratings.set`, ...).
    pub method: String,
    /// Handling session's public ID, when initiated by a session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub client_session_id: Option<Id>,
    /// Handling session's subject kind (`person`, `agent`, `server`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub client_kind: Option<String>,
    /// Unix timestamp in milliseconds when committed.
    pub committed_at_ms: i64,
}

/// The result of `library.revisions`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct LibraryRevisionsPage {
    /// Current `library_meta.rev`.
    pub current_rev: u64,
    /// Total rows in `library_revision`.
    pub total: u64,
    /// Revision log entries, newest first (`rev DESC`).
    pub revisions: Vec<LibraryRevisionEntry>,
}
