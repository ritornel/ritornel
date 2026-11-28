//! Read-only database introspection, allowlisted table browsing, entity and
//! play provenance, conflicts, and revision logs. See "Database inspection" in
//! docs/internals/contract.md.

use std::collections::HashMap;
use std::path::Path;

use ritornel_contract::{
    ColumnSchema, DbSummary, EntityProvenance, Id, IndexSchema, InspectDb, InspectEntityKind,
    InspectOrder, InspectTableParams, InspectedAssignment, InspectedAudioAsset, InspectedDelivery,
    InspectedIdentifierClaim, InspectedIdentityDecision, InspectedIdentityExclusion,
    InspectedIngestConflict, InspectedMetadataClaim, InspectedObservation, InspectedOutboxItem,
    InspectedPlayCorrection, InspectedSourceItem, InspectedSourceRevision, LibraryRevisionEntry,
    LibraryRevisionsPage, MAX_INSPECT_ROWS, MigrationSummary, PlayProvenance, TableLayer,
    TableObjectKind, TablePage, TableSchema, hex,
};
use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, OptionalExtension};
use serde_json::{Value, json};

use crate::{Error, LibraryStore, ServerStore};

/// Tables in `server.db` that `inspect.table` explicitly allows reading.
pub(crate) const SERVER_ALLOWLIST: &[&str] = &[
    "schema_migration",
    "server_setting",
    "root",
    "command_receipt",
    "job",
    "job_item",
    "outbox",
    "queue_entry",
    "playback_session",
    "listen_staging",
    "transcode_entry",
    "transcode_pin",
    "transcode_size_hint",
    "sync_root",
];

/// Tables in `server.db` that hold private keys, session hashes, or service
/// secrets and are strictly excluded from `inspect.table`.
pub(crate) const SERVER_EXCLUDED: &[&str] =
    &["server_identity", "session", "service_session", "hub_link"];

/// Tables and views in `library.db` that `inspect.table` explicitly allows reading.
pub(crate) const LIBRARY_ALLOWLIST: &[&str] = &[
    "schema_migration",
    "server",
    "library_meta",
    "library_revision",
    "library_command",
    "library_setting",
    "artist",
    "artist_credit",
    "artist_credit_name",
    "recording",
    "release_group",
    "release",
    "track_appearance",
    "picture_folder",
    "artist_picture",
    "metadata_source",
    "library_root",
    "source_instance",
    "source_item",
    "source_item_revision",
    "identifier_claim",
    "metadata_claim",
    "metadata_edit",
    "assignment",
    "identity_exclusion",
    "audio_asset",
    "asset_location",
    "picture",
    "revision_picture",
    "listener",
    "history_endpoint",
    "service_account",
    "history_observation",
    "observation_listen",
    "observation_import",
    "correction_target",
    "listener_zone_period",
    "listener_rating",
    "history_ingest_conflict",
    "history_cursor",
    "history_delivery",
    "play_event",
    "play_event_observation",
    "play_effective",
    "observation_correction",
    "correction_recording",
    "artist_key",
    "identity_decision",
    "alias_observation",
    "observation_import_stream",
    "import_run",
    "import_run_event",
    "import_run_stream",
    "stream_device",
    "heard_artist",
    "heard_album",
    "play_event_artist",
    "play_event_album",
    "sidecar",
    "sidecar_revision",
    "evidence_disc",
    "evidence_disc_item",
    "lookup_response",
    "evidence_claim",
    "proposal",
    "proposal_item",
    "proposal_candidate",
    "proposal_evidence",
    "answer",
    "answer_value",
    "answer_redirect",
    "answer_join",
    "answer_dependency",
    "answer_rule",
    "answer_rule_item",
    "sync_target",
    "sync_target_identity",
    "sync_target_epoch",
    "sync_source",
    "sync_object",
    "sync_unmanaged",
    "sync_run",
    "sync_action",
];

/// Virtual tables in `library.db` excluded from `inspect.table` (FTS5 index and its
/// shadow tables are skipped separately).
#[cfg(test)]
const LIBRARY_EXCLUDED: &[&str] = &["search_text"];

/// Maximum text cell length returned by `inspect.table` before truncation.
const MAX_CELL_TEXT_BYTES: usize = 2048;

/// Soft serialized JSON byte budget for one `inspect.table` page (leaving headroom
/// under `MAX_MESSAGE_BYTES` = 1 MiB for envelope + column metadata).
const MAX_TABLE_PAGE_BYTES: usize = 512 * 1024;

/// Maximum raw JSON string length parsed into a structured `serde_json::Value` in
/// provenance and conflict responses before returning a truncated summary object.
const MAX_PARSED_JSON_BYTES: usize = 8192;

/// Maximum source items loaded by one `inspect.entity` call.
const MAX_ENTITY_SOURCE_ITEMS: i64 = 64;

/// Maximum child rows per provenance list (`revisions`, `claims`, `assignments`,
/// `decisions`, `exclusions`, `observations`, `deliveries`, `corrections`).
const MAX_PROVENANCE_CHILDREN: i64 = 64;

/// Maximum rows returned by `inspect.table`, `inspect.conflicts`, or `library.revisions`.
const DEFAULT_INSPECT_LIMIT: u32 = 50;

/// Errors specific to `inspect.table` validation and access control.
#[derive(Debug)]
pub enum InspectTableError {
    /// Table is explicitly excluded (secrets/sessions) or filter probes a redacted path column.
    Forbidden(String),
    /// Table does not exist or is not browsable.
    NotFound(String),
    /// Invalid limit, column name, or filter parameters.
    Invalid(String),
    /// Database error.
    Engine(Error),
}

impl From<Error> for InspectTableError {
    fn from(error: Error) -> Self {
        Self::Engine(error)
    }
}

impl From<rusqlite::Error> for InspectTableError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Engine(Error::from(error))
    }
}

/// Queue counts from `server.db`.
#[derive(Debug, Clone, Copy, Default)]
pub struct ServerQueueDepths {
    /// Rows waiting in `outbox`.
    pub outbox: u64,
    /// Open rows in `listen_staging`.
    pub listen_staging_open: u64,
    /// Closed rows in `listen_staging`.
    pub listen_staging_closed: u64,
}

/// Queue and conflict counts from `library.db`.
#[derive(Debug, Clone, Copy, Default)]
pub struct LibraryQueueDepths {
    /// Pending rows in `history_delivery`.
    pub history_delivery_pending: u64,
    /// Refused rows in `history_delivery`.
    pub history_delivery_refused: u64,
    /// Unknown-outcome rows in `history_delivery`.
    pub history_delivery_unknown: u64,
    /// Rows in `history_ingest_conflict`.
    pub history_ingest_conflicts: u64,
}

/// Paged conflicts and stuck deliveries from `library.db`.
#[derive(Debug, Clone)]
pub struct LibraryConflicts {
    /// Total rows in `history_ingest_conflict`.
    pub ingest_conflicts_total: u64,
    /// Paged rows from `history_ingest_conflict`.
    pub ingest_conflicts: Vec<InspectedIngestConflict>,
    /// Total stuck/refused/unknown rows in `history_delivery`.
    pub stuck_deliveries_total: u64,
    /// Paged stuck/refused/unknown rows in `history_delivery`.
    pub stuck_deliveries: Vec<InspectedDelivery>,
}

/// Paged outbox items from `server.db`.
#[derive(Debug, Clone)]
pub struct ServerOutboxPage {
    /// Total rows in `outbox`.
    pub total: u64,
    /// Paged rows from `outbox`.
    pub items: Vec<InspectedOutboxItem>,
}

fn clamp_limit(limit: Option<u32>) -> Result<u32, InspectTableError> {
    let value = limit.unwrap_or(DEFAULT_INSPECT_LIMIT);
    if value == 0 || value > MAX_INSPECT_ROWS {
        return Err(InspectTableError::Invalid(format!(
            "limit must be between 1 and {MAX_INSPECT_ROWS}"
        )));
    }
    Ok(value)
}

fn unsigned(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

fn unsigned_u32(value: i64) -> u32 {
    u32::try_from(value).unwrap_or(0)
}

fn id_from_blob(bytes: &[u8]) -> Option<Id> {
    let array: [u8; 16] = bytes.try_into().ok()?;
    Some(Id::from_bytes(array))
}

fn id_from_column(row: &rusqlite::Row<'_>, idx: usize) -> rusqlite::Result<Id> {
    let bytes: Vec<u8> = row.get(idx)?;
    id_from_blob(&bytes).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            idx,
            rusqlite::types::Type::Blob,
            format!("expected 16-byte UUID blob, got {} bytes", bytes.len()).into(),
        )
    })
}

fn format_source_key(bytes: &[u8]) -> String {
    if let Some(id) = id_from_blob(bytes) {
        id.to_string()
    } else {
        hex::encode(bytes)
    }
}

fn is_safe_ident(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn is_fts_shadow(name: &str) -> bool {
    name.starts_with("search_text_")
}

fn library_table_layer(name: &str) -> TableLayer {
    match name {
        "source_item"
        | "source_item_revision"
        | "audio_asset"
        | "picture"
        | "revision_picture"
        | "sidecar"
        | "sidecar_revision"
        | "lookup_response"
        | "history_observation"
        | "observation_listen"
        | "observation_import"
        | "observation_import_stream"
        | "import_run"
        | "import_run_event"
        | "import_run_stream"
        | "stream_device"
        | "alias_observation"
        | "history_ingest_conflict" => TableLayer::Evidence,
        "metadata_edit"
        | "identity_decision"
        | "assignment"
        | "identity_exclusion"
        | "listener_rating"
        | "observation_correction"
        | "correction_target"
        | "correction_recording"
        | "listener_zone_period"
        | "answer"
        | "answer_value"
        | "answer_redirect"
        | "answer_join"
        | "answer_dependency"
        | "answer_rule"
        | "answer_rule_item"
        | "artist"
        | "artist_credit"
        | "artist_credit_name"
        | "recording"
        | "release_group"
        | "release"
        | "track_appearance"
        | "picture_folder"
        | "sync_target"
        | "sync_source" => TableLayer::Decision,
        "schema_migration"
        | "server"
        | "library_meta"
        | "library_revision"
        | "library_command"
        | "library_setting"
        | "metadata_source"
        | "library_root"
        | "source_instance"
        | "listener"
        | "history_endpoint"
        | "service_account"
        | "history_cursor"
        | "history_delivery"
        | "sync_target_identity"
        | "sync_target_epoch"
        | "sync_object"
        | "sync_run"
        | "sync_action" => TableLayer::Bookkeeping,
        _ => TableLayer::Rebuildable,
    }
}

fn server_table_layer(name: &str) -> TableLayer {
    match name {
        "listen_staging" => TableLayer::Evidence,
        "transcode_entry" | "transcode_pin" | "transcode_size_hint" => TableLayer::Rebuildable,
        _ => TableLayer::Bookkeeping,
    }
}

fn is_browsable(db: InspectDb, name: &str) -> bool {
    match db {
        InspectDb::Server => SERVER_ALLOWLIST.contains(&name),
        InspectDb::Library => LIBRARY_ALLOWLIST.contains(&name),
    }
}

fn is_hash_blob_column(column: &str) -> bool {
    matches!(
        column,
        "checksum"
            | "payload_hash"
            | "audio_hash"
            | "flac_md5"
            | "request_fingerprint"
            | "file_hash"
            | "sampled_hash"
            | "token_hash"
            | "media_token_hash"
            | "public_key"
            | "signing_key"
            | "peer_public_key"
            | "hub_public_key"
    )
}

fn is_redacted_server_column(table: &str, column: &str) -> bool {
    matches!(
        (table, column),
        ("root", "abs_path")
            | ("server_setting", "value_json")
            | ("command_receipt", "result_json")
            | (
                "job",
                "params_json" | "current_label" | "error_json" | "checkpoint_json"
            )
            | ("job_item", "item_ref" | "detail_json")
            | ("outbox", "last_error")
    )
}

fn path_carrying_setting_key(key: &str) -> bool {
    matches!(key, "backup_dir" | "backup.last" | "backup.failed")
}

fn path_carrying_receipt_method(method: &str) -> bool {
    matches!(
        method,
        "sources.add"
            | "sources.tracks"
            | "server.backup"
            | "history.export"
            | "server.setSettings"
    )
}

fn truncate_text(mut text: String) -> String {
    if text.len() <= MAX_CELL_TEXT_BYTES {
        return text;
    }
    let mut end = MAX_CELL_TEXT_BYTES;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    text.push_str("… (truncated)");
    text
}

fn cell_to_json(column: &str, value: SqlValue) -> Value {
    match value {
        SqlValue::Null => Value::Null,
        SqlValue::Integer(n) => Value::from(n),
        SqlValue::Real(f) => serde_json::Number::from_f64(f).map_or(Value::Null, Value::Number),
        SqlValue::Text(s) => Value::String(truncate_text(s)),
        SqlValue::Blob(bytes) => {
            if bytes.len() == 16
                && !is_hash_blob_column(column)
                && let Some(id) = id_from_blob(&bytes)
            {
                Value::String(id.to_string())
            } else if bytes.len() <= 64 {
                Value::String(hex::encode(&bytes))
            } else {
                json!({ "blobBytes": bytes.len() })
            }
        }
    }
}

fn parse_json_or_string(raw: &str) -> Value {
    if raw.len() > MAX_PARSED_JSON_BYTES {
        return json!({
            "truncatedBytes": raw.len(),
            "preview": truncate_text(raw.to_owned()),
        });
    }
    serde_json::from_str(raw).unwrap_or_else(|_| Value::String(truncate_text(raw.to_owned())))
}

fn table_columns(connection: &Connection, table: &str) -> Result<Vec<ColumnSchema>, Error> {
    if !is_safe_ident(table) {
        return Ok(Vec::new());
    }
    let mut fk_map: HashMap<String, (String, String)> = HashMap::new();
    {
        let mut stmt = connection.prepare(&format!("PRAGMA foreign_key_list(\"{table}\")"))?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(3)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        for row in rows {
            let (from_col, to_table, to_col) = row?;
            fk_map.entry(from_col).or_insert((to_table, to_col));
        }
    }
    let mut stmt = connection.prepare(&format!("PRAGMA table_info(\"{table}\")"))?;
    let rows = stmt.query_map([], |row| {
        let name: String = row.get(1)?;
        let col_type: String = row.get(2)?;
        let not_null: i64 = row.get(3)?;
        let default_value: Option<String> = row.get(4)?;
        let pk: i64 = row.get(5)?;
        Ok((name, col_type, not_null != 0, default_value, pk > 0))
    })?;
    let mut columns = Vec::new();
    for row in rows {
        let (name, col_type, not_null, default_value, pk) = row?;
        let (fk_table, fk_column) = match fk_map.get(&name) {
            Some((t, c)) => (Some(t.clone()), Some(c.clone())),
            None => (None, None),
        };
        columns.push(ColumnSchema {
            name,
            col_type,
            not_null,
            pk,
            default_value,
            fk_table,
            fk_column,
        });
    }
    Ok(columns)
}

fn table_indexes(connection: &Connection, table: &str) -> Result<Vec<IndexSchema>, Error> {
    if !is_safe_ident(table) {
        return Ok(Vec::new());
    }
    let mut index_rows = Vec::new();
    {
        let mut stmt = connection.prepare(&format!("PRAGMA index_list(\"{table}\")"))?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)? != 0,
                row.get::<_, i64>(4)? != 0,
            ))
        })?;
        for row in rows {
            index_rows.push(row?);
        }
    }
    let mut indexes = Vec::new();
    for (name, unique, partial) in index_rows {
        if !is_safe_ident(&name) {
            continue;
        }
        let mut stmt = connection.prepare(&format!("PRAGMA index_info(\"{name}\")"))?;
        let cols: Vec<String> = stmt
            .query_map([], |row| row.get::<_, Option<String>>(2))?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .flatten()
            .collect();
        indexes.push(IndexSchema {
            name,
            unique,
            partial,
            columns: cols,
        });
    }
    indexes.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(indexes)
}

fn table_triggers(connection: &Connection, table: &str) -> Result<Vec<String>, Error> {
    let mut stmt = connection.prepare(
        "SELECT name FROM sqlite_schema WHERE type = 'trigger' AND tbl_name = ?1 ORDER BY name",
    )?;
    let rows = stmt.query_map([table], |row| row.get::<_, String>(0))?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

fn inspect_db_connection(
    connection: &Connection,
    db: InspectDb,
    schema_version: u32,
    file_sizes: Option<(u64, u64)>,
) -> Result<DbSummary, Error> {
    let page_size: i64 = connection.query_row("PRAGMA page_size", [], |row| row.get(0))?;
    let page_count: i64 = connection.query_row("PRAGMA page_count", [], |row| row.get(0))?;
    let freelist_count: i64 =
        connection.query_row("PRAGMA freelist_count", [], |row| row.get(0))?;

    let migrations = {
        let mut stmt = connection.prepare(
            "SELECT version, name, checksum, applied_at_ms FROM schema_migration ORDER BY version",
        )?;
        let rows = stmt.query_map([], |row| {
            let version: i64 = row.get(0)?;
            let name: String = row.get(1)?;
            let checksum: Vec<u8> = row.get(2)?;
            let applied_at_ms: i64 = row.get(3)?;
            Ok(MigrationSummary {
                version: unsigned_u32(version),
                name,
                checksum: hex::encode(&checksum),
                applied_at_ms,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()?
    };

    let objects: Vec<(String, String)> = {
        let mut stmt = connection.prepare(
            "SELECT name, type FROM sqlite_schema
             WHERE type IN ('table', 'view')
               AND name NOT LIKE 'sqlite_%'
             ORDER BY name",
        )?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect::<Result<Vec<_>, _>>()?
    };

    let mut tables = Vec::new();
    for (name, object_type) in objects {
        if db == InspectDb::Library && is_fts_shadow(&name) {
            continue;
        }
        if !is_safe_ident(&name) {
            continue;
        }
        let kind = if object_type == "view" {
            TableObjectKind::View
        } else {
            TableObjectKind::Table
        };
        let layer = match db {
            InspectDb::Library => library_table_layer(&name),
            InspectDb::Server => server_table_layer(&name),
        };
        let browsable = is_browsable(db, &name);
        let columns = table_columns(connection, &name)?;
        let indexes = if kind == TableObjectKind::Table && name != "search_text" {
            table_indexes(connection, &name)?
        } else {
            Vec::new()
        };
        let triggers = if kind == TableObjectKind::Table {
            table_triggers(connection, &name)?
        } else {
            Vec::new()
        };
        let row_count = if name == "search_text" {
            None
        } else {
            connection
                .query_row(&format!("SELECT COUNT(*) FROM \"{name}\""), [], |row| {
                    row.get::<_, i64>(0)
                })
                .ok()
                .map(unsigned)
        };
        tables.push(TableSchema {
            name,
            kind,
            layer,
            browsable,
            row_count,
            columns,
            indexes,
            triggers,
        });
    }

    Ok(DbSummary {
        db,
        schema_version,
        file_bytes: file_sizes.map(|(main, _)| main),
        wal_bytes: file_sizes.map(|(_, wal)| wal),
        page_size: unsigned(page_size),
        page_count: unsigned(page_count),
        freelist_count: unsigned(freelist_count),
        migrations,
        tables,
    })
}

fn inspect_table_connection(
    connection: &Connection,
    db: InspectDb,
    params: &InspectTableParams,
    show_paths: bool,
) -> Result<TablePage, InspectTableError> {
    let table = params.table.as_str();
    if db == InspectDb::Server && SERVER_EXCLUDED.contains(&table) {
        return Err(InspectTableError::Forbidden(format!(
            "table '{table}' holds private credentials or keys and cannot be browsed"
        )));
    }
    if !is_browsable(db, table) {
        return Err(InspectTableError::NotFound(format!(
            "table '{table}' is not available in {}",
            db.as_str()
        )));
    }
    let exists: bool = connection
        .query_row(
            "SELECT 1 FROM sqlite_schema WHERE type IN ('table', 'view') AND name = ?1",
            [table],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !exists {
        return Err(InspectTableError::NotFound(format!(
            "table '{table}' does not exist in {}",
            db.as_str()
        )));
    }

    let limit = clamp_limit(params.limit)?;
    let order = params.order.unwrap_or_default();
    let columns = table_columns(connection, table)?;
    let layer = match db {
        InspectDb::Library => library_table_layer(table),
        InspectDb::Server => server_table_layer(table),
    };

    let filter: Option<(String, SqlValue)> = match (&params.column, &params.equals) {
        (None, None) => None,
        (Some(col), Some(raw)) => {
            let Some(schema) = columns.iter().find(|c| c.name == *col) else {
                return Err(InspectTableError::Invalid(format!(
                    "column '{col}' does not exist on '{table}'"
                )));
            };
            if db == InspectDb::Server && !show_paths && is_redacted_server_column(table, col) {
                return Err(InspectTableError::Forbidden(format!(
                    "filtering '{table}.{col}' requires folders:manage"
                )));
            }
            let bind_val = match schema.col_type.to_ascii_uppercase().as_str() {
                "BLOB" => {
                    if let Ok(id) = raw.parse::<Id>() {
                        SqlValue::Blob(id.as_bytes().to_vec())
                    } else if let Some(bytes) = hex::decode(raw) {
                        SqlValue::Blob(bytes)
                    } else {
                        SqlValue::Text(raw.clone())
                    }
                }
                "INTEGER" => raw
                    .parse::<i64>()
                    .map_or_else(|_| SqlValue::Text(raw.clone()), SqlValue::Integer),
                _ => SqlValue::Text(raw.clone()),
            };
            Some((schema.name.clone(), bind_val))
        }
        _ => {
            return Err(InspectTableError::Invalid(
                "column and equals must be provided together".to_owned(),
            ));
        }
    };

    let where_sql = filter
        .as_ref()
        .map_or(String::new(), |(col, _)| format!(" WHERE \"{col}\" = ?1"));
    let total: i64 = match &filter {
        Some((_, val)) => connection.query_row(
            &format!("SELECT COUNT(*) FROM \"{table}\"{where_sql}"),
            [val],
            |row| row.get(0),
        )?,
        None => connection.query_row(&format!("SELECT COUNT(*) FROM \"{table}\""), [], |row| {
            row.get(0)
        })?,
    };

    let order_cols: Vec<String> = {
        let pks: Vec<String> = columns
            .iter()
            .filter(|c| c.pk)
            .map(|c| format!("\"{}\"", c.name))
            .collect();
        if pks.is_empty() {
            columns.iter().map(|c| format!("\"{}\"", c.name)).collect()
        } else {
            pks
        }
    };
    let dir = match order {
        InspectOrder::Asc => "ASC",
        InspectOrder::Desc => "DESC",
    };
    let order_sql = if order_cols.is_empty() {
        String::new()
    } else {
        let clauses: Vec<String> = order_cols
            .iter()
            .map(|col| format!("{col} {dir}"))
            .collect();
        format!(" ORDER BY {}", clauses.join(", "))
    };

    let select_cols: Vec<String> = columns.iter().map(|c| format!("\"{}\"", c.name)).collect();
    let offset_i64 = i64::try_from(params.offset).unwrap_or(i64::MAX);
    let limit_i64 = i64::from(limit);

    let key_col_idx = columns.iter().position(|c| c.name == "key");
    let method_col_idx = columns.iter().position(|c| c.name == "method");

    let map_row = |row: &rusqlite::Row<'_>| -> rusqlite::Result<Vec<Value>> {
        let mut cells = Vec::with_capacity(columns.len());
        for (idx, col) in columns.iter().enumerate() {
            let val: SqlValue = row.get(idx)?;
            cells.push(cell_to_json(&col.name, val));
        }
        if db == InspectDb::Server && !show_paths {
            let setting_has_path = table == "server_setting"
                && key_col_idx
                    .and_then(|idx| cells.get(idx))
                    .and_then(Value::as_str)
                    .is_some_and(path_carrying_setting_key);
            let receipt_has_path = table == "command_receipt"
                && method_col_idx
                    .and_then(|idx| cells.get(idx))
                    .and_then(Value::as_str)
                    .is_some_and(path_carrying_receipt_method);
            for (idx, col) in columns.iter().enumerate() {
                let redact = match (table, col.name.as_str()) {
                    ("root", "abs_path")
                    | ("job", "params_json" | "current_label" | "error_json" | "checkpoint_json")
                    | ("job_item", "item_ref" | "detail_json")
                    | ("outbox", "last_error") => true,
                    ("server_setting", "value_json") => setting_has_path,
                    ("command_receipt", "result_json") => receipt_has_path,
                    _ => false,
                };
                if redact && let Some(slot) = cells.get_mut(idx) {
                    *slot = Value::Null;
                }
            }
        }
        Ok(cells)
    };

    let mut rows: Vec<Vec<Value>> = Vec::new();
    let mut truncated = false;
    let mut used_bytes: usize = 0;

    let mut collect_rows = |mapped: &mut dyn Iterator<Item = rusqlite::Result<Vec<Value>>>| -> Result<(), InspectTableError> {
        for row_res in mapped {
            let row = row_res?;
            let row_bytes = serde_json::to_string(&row).map_or(0, |s| s.len() + 1);
            if !rows.is_empty() && used_bytes.saturating_add(row_bytes) > MAX_TABLE_PAGE_BYTES {
                truncated = true;
                break;
            }
            used_bytes = used_bytes.saturating_add(row_bytes);
            rows.push(row);
        }
        Ok(())
    };

    match &filter {
        Some((_, val)) => {
            let sql = format!(
                "SELECT {} FROM \"{table}\"{where_sql}{order_sql} LIMIT ?2 OFFSET ?3",
                select_cols.join(", ")
            );
            let mut stmt = connection.prepare(&sql)?;
            let mut mapped =
                stmt.query_map(rusqlite::params![val, limit_i64, offset_i64], map_row)?;
            collect_rows(&mut mapped)?;
        }
        None => {
            let sql = format!(
                "SELECT {} FROM \"{table}\"{order_sql} LIMIT ?1 OFFSET ?2",
                select_cols.join(", ")
            );
            let mut stmt = connection.prepare(&sql)?;
            let mut mapped = stmt.query_map(rusqlite::params![limit_i64, offset_i64], map_row)?;
            collect_rows(&mut mapped)?;
        }
    }

    Ok(TablePage {
        db,
        table: table.to_owned(),
        layer,
        columns,
        total: unsigned(total),
        offset: params.offset,
        limit,
        truncated,
        rows,
    })
}

impl ServerStore {
    /// Introspects `server.db`: sizes, migrations, tables, columns, indexes, and triggers.
    pub fn inspect_db(&self, file_sizes: Option<(u64, u64)>) -> Result<DbSummary, Error> {
        let version = u32::try_from(self.schema_version()).unwrap_or(0);
        self.read(move |connection| {
            crate::sqlite::snapshot(connection, |connection| {
                inspect_db_connection(connection, InspectDb::Server, version, file_sizes)
            })
        })
    }

    /// Reads one allowlisted page from `server.db`.
    pub fn inspect_table(
        &self,
        params: &InspectTableParams,
        show_paths: bool,
    ) -> Result<TablePage, InspectTableError> {
        let params = params.clone();
        self.read(move |connection| {
            crate::sqlite::snapshot(connection, |connection| {
                Ok(inspect_table_connection(
                    connection,
                    InspectDb::Server,
                    &params,
                    show_paths,
                ))
            })
        })
        .map_err(InspectTableError::Engine)?
    }

    /// Reads queue depths for `outbox` and `listen_staging`.
    pub fn queue_depths(&self) -> Result<ServerQueueDepths, Error> {
        self.read(|connection| {
            crate::sqlite::snapshot(connection, |connection| {
                let outbox: i64 =
                    connection.query_row("SELECT COUNT(*) FROM outbox", [], |row| row.get(0))?;
                let listen_staging_open: i64 = connection.query_row(
                    "SELECT COUNT(*) FROM listen_staging WHERE ended_at_utc_ms IS NULL",
                    [],
                    |row| row.get(0),
                )?;
                let listen_staging_closed: i64 = connection.query_row(
                    "SELECT COUNT(*) FROM listen_staging WHERE ended_at_utc_ms IS NOT NULL",
                    [],
                    |row| row.get(0),
                )?;
                Ok(ServerQueueDepths {
                    outbox: unsigned(outbox),
                    listen_staging_open: unsigned(listen_staging_open),
                    listen_staging_closed: unsigned(listen_staging_closed),
                })
            })
        })
    }

    /// Reads paged `outbox` items, oldest due first.
    pub fn inspect_outbox(
        &self,
        offset: u64,
        limit: Option<u32>,
        show_paths: bool,
    ) -> Result<ServerOutboxPage, InspectTableError> {
        let limit = clamp_limit(limit)?;
        let offset_i64 = i64::try_from(offset).unwrap_or(i64::MAX);
        let limit_i64 = i64::from(limit);
        self.read(move |connection| {
            crate::sqlite::snapshot(connection, |connection| {
                let total: i64 =
                    connection.query_row("SELECT COUNT(*) FROM outbox", [], |row| row.get(0))?;
                let mut stmt = connection.prepare(
                    "SELECT id, kind, dedupe_key, attempts, next_attempt_at_ms, last_error, created_at_ms
                     FROM outbox
                     ORDER BY next_attempt_at_ms ASC, id ASC
                     LIMIT ?1 OFFSET ?2",
                )?;
                let rows = stmt.query_map([limit_i64, offset_i64], |row| {
                    let dedupe_key: Vec<u8> = row.get(2)?;
                    let last_error: Option<String> = if show_paths { row.get(5)? } else { None };
                    Ok(InspectedOutboxItem {
                        id: row.get(0)?,
                        kind: row.get(1)?,
                        dedupe_key: format_source_key(&dedupe_key),
                        attempts: unsigned_u32(row.get(3)?),
                        next_attempt_at_ms: row.get(4)?,
                        last_error,
                        created_at_ms: row.get(6)?,
                    })
                })?;
                let items = rows.collect::<Result<Vec<_>, _>>()?;
                Ok(ServerOutboxPage {
                    total: unsigned(total),
                    items,
                })
            })
        })
        .map_err(InspectTableError::Engine)
    }
}

struct EntityTarget {
    canonical_id: Option<Id>,
    title: Option<String>,
    artist: Option<String>,
    source_item_ids: Vec<i64>,
    entity_decision_target: Option<(&'static str, i64, i64)>,
}

impl LibraryStore {
    /// Introspects `library.db`: sizes, migrations, tables, columns, indexes, and triggers.
    pub fn inspect_db(&self, file_sizes: Option<(u64, u64)>) -> Result<DbSummary, Error> {
        let version = u32::try_from(self.schema_version()).unwrap_or(0);
        self.read(move |connection| {
            crate::sqlite::snapshot(connection, |connection| {
                inspect_db_connection(connection, InspectDb::Library, version, file_sizes)
            })
        })
    }

    /// Reads one allowlisted page from `library.db`.
    pub fn inspect_table(
        &self,
        params: &InspectTableParams,
        show_paths: bool,
    ) -> Result<TablePage, InspectTableError> {
        let params = params.clone();
        self.read(move |connection| {
            crate::sqlite::snapshot(connection, |connection| {
                Ok(inspect_table_connection(
                    connection,
                    InspectDb::Library,
                    &params,
                    show_paths,
                ))
            })
        })
        .map_err(InspectTableError::Engine)?
    }

    /// Reads queue and conflict counts from `library.db`.
    pub fn queue_depths(&self) -> Result<LibraryQueueDepths, Error> {
        self.read(|connection| {
            crate::sqlite::snapshot(connection, |connection| {
                let pending: i64 = connection.query_row(
                    "SELECT COUNT(*) FROM history_delivery WHERE state = 'pending'",
                    [],
                    |row| row.get(0),
                )?;
                let refused: i64 = connection.query_row(
                    "SELECT COUNT(*) FROM history_delivery WHERE state = 'refused'",
                    [],
                    |row| row.get(0),
                )?;
                let unknown: i64 = connection.query_row(
                    "SELECT COUNT(*) FROM history_delivery WHERE state = 'unknown'",
                    [],
                    |row| row.get(0),
                )?;
                let conflicts: i64 = connection.query_row(
                    "SELECT COUNT(*) FROM history_ingest_conflict",
                    [],
                    |row| row.get(0),
                )?;
                Ok(LibraryQueueDepths {
                    history_delivery_pending: unsigned(pending),
                    history_delivery_refused: unsigned(refused),
                    history_delivery_unknown: unsigned(unknown),
                    history_ingest_conflicts: unsigned(conflicts),
                })
            })
        })
    }

    /// Reads paged `library_revision` entries, newest first.
    pub fn library_revisions(
        &self,
        offset: u64,
        limit: Option<u32>,
    ) -> Result<LibraryRevisionsPage, InspectTableError> {
        let limit = clamp_limit(limit)?;
        let offset_i64 = i64::try_from(offset).unwrap_or(i64::MAX);
        let limit_i64 = i64::from(limit);
        self.read(move |connection| {
            crate::sqlite::snapshot(connection, |connection| {
                let current_rev: i64 =
                    connection.query_row("SELECT rev FROM library_meta WHERE id = 1", [], |row| {
                        row.get(0)
                    })?;
                let total: i64 =
                    connection.query_row("SELECT COUNT(*) FROM library_revision", [], |row| {
                        row.get(0)
                    })?;
                let mut stmt = connection.prepare(
                    "SELECT rev, command_id, method, client_session_pid, client_kind, committed_at_ms
                         FROM library_revision
                         ORDER BY rev DESC
                         LIMIT ?1 OFFSET ?2",
                )?;
                let rows = stmt.query_map([limit_i64, offset_i64], |row| {
                    let command_id: Option<Vec<u8>> = row.get(1)?;
                    let session_pid: Option<Vec<u8>> = row.get(3)?;
                    Ok(LibraryRevisionEntry {
                        rev: unsigned(row.get(0)?),
                        command_id: command_id.as_deref().and_then(id_from_blob),
                        method: row.get(2)?,
                        client_session_id: session_pid.as_deref().and_then(id_from_blob),
                        client_kind: row.get(4)?,
                        committed_at_ms: row.get(5)?,
                    })
                })?;
                let revisions = rows.collect::<Result<Vec<_>, _>>()?;
                Ok(LibraryRevisionsPage {
                    current_rev: unsigned(current_rev),
                    total: unsigned(total),
                    revisions,
                })
            })
        })
        .map_err(InspectTableError::Engine)
    }

    /// Reads paged ingest conflicts and stuck/refused/unknown deliveries.
    pub fn inspect_conflicts(
        &self,
        offset: u64,
        limit: Option<u32>,
    ) -> Result<LibraryConflicts, InspectTableError> {
        let limit = clamp_limit(limit)?;
        let offset_i64 = i64::try_from(offset).unwrap_or(i64::MAX);
        let limit_i64 = i64::from(limit);
        self.read(move |connection| {
            crate::sqlite::snapshot(connection, |connection| {
                let ingest_conflicts_total: i64 = connection.query_row(
                    "SELECT COUNT(*) FROM history_ingest_conflict",
                    [],
                    |row| row.get(0),
                )?;
                let ingest_conflicts = {
                    let mut stmt = connection.prepare(
                        "SELECT c.id, e.name, e.kind, c.source_key, c.existing_observation_id,
                                c.payload_json, c.received_at_utc_ms, c.rev
                         FROM history_ingest_conflict c
                         JOIN history_endpoint e ON e.id = c.endpoint_id
                         ORDER BY c.id DESC
                         LIMIT ?1 OFFSET ?2",
                    )?;
                    let rows = stmt.query_map([limit_i64, offset_i64], |row| {
                        let source_key: Vec<u8> = row.get(3)?;
                        let payload_json: String = row.get(5)?;
                        Ok(InspectedIngestConflict {
                            id: row.get(0)?,
                            endpoint_name: row.get(1)?,
                            endpoint_kind: row.get(2)?,
                            source_key: format_source_key(&source_key),
                            existing_observation_id: row.get(4)?,
                            payload: parse_json_or_string(&payload_json),
                            received_at_utc_ms: row.get(6)?,
                            rev: unsigned(row.get(7)?),
                        })
                    })?;
                    rows.collect::<Result<Vec<_>, _>>()?
                };

                let stuck_where = "state IN ('refused', 'unknown', 'sending') OR (state = 'pending' AND attempts > 0)";
                let stuck_deliveries_total: i64 = connection.query_row(
                    &format!("SELECT COUNT(*) FROM history_delivery WHERE {stuck_where}"),
                    [],
                    |row| row.get(0),
                )?;
                let stuck_deliveries = {
                    let mut stmt = connection.prepare(&format!(
                        "SELECT d.id, d.observation_id, e.name, d.state, d.sent_timestamp_s,
                                d.artist, d.title, d.album, d.accepted_artist, d.accepted_title,
                                d.ignored_code, d.detail, d.attempts, d.next_attempt_at_ms, d.updated_at_ms
                         FROM history_delivery d
                         JOIN history_endpoint e ON e.id = d.endpoint_id
                         WHERE {stuck_where}
                         ORDER BY d.updated_at_ms DESC, d.id DESC
                         LIMIT ?1 OFFSET ?2"
                    ))?;
                    let rows = stmt.query_map([limit_i64, offset_i64], map_delivery_row)?;
                    rows.collect::<Result<Vec<_>, _>>()?
                };

                Ok(LibraryConflicts {
                    ingest_conflicts_total: unsigned(ingest_conflicts_total),
                    ingest_conflicts,
                    stuck_deliveries_total: unsigned(stuck_deliveries_total),
                    stuck_deliveries,
                })
            })
        })
        .map_err(InspectTableError::Engine)
    }

    /// Reads full provenance for a recording, appearance, release, source item, or audio asset.
    ///
    /// `local_roots` maps local root names to their absolute filesystem paths on this server;
    /// pass `None` when the calling session lacks `folders:manage`.
    pub fn inspect_entity(
        &self,
        kind: InspectEntityKind,
        id: Id,
        this_server_id: Id,
        local_roots: Option<HashMap<String, String>>,
    ) -> Result<Option<EntityProvenance>, Error> {
        self.read(move |connection| {
            crate::sqlite::snapshot(connection, |connection| {
                let target = match kind {
                    InspectEntityKind::Recording => {
                        let Some((row_id, canonical_row_id, canonical_pid, title, artist)) =
                            connection
                                .query_row(
                                    "SELECT r.id, c.id, c.public_id, c.display_title, ac.display_text
                                     FROM recording r
                                     JOIN recording c ON c.id = r.canonical_id
                                     LEFT JOIN artist_credit ac ON ac.id = c.display_artist_credit_id
                                     WHERE r.public_id = ?1",
                                    [id.as_bytes().as_slice()],
                                    |row| {
                                        Ok((
                                            row.get::<_, i64>(0)?,
                                            row.get::<_, i64>(1)?,
                                            id_from_column(row, 2)?,
                                            row.get::<_, Option<String>>(3)?,
                                            row.get::<_, Option<String>>(4)?,
                                        ))
                                    },
                                )
                                .optional()?
                        else {
                            return Ok(None);
                        };
                        let mut stmt = connection.prepare(
                            "SELECT DISTINCT a.source_item_id
                             FROM assignment a
                             JOIN recording r ON r.id = a.recording_id
                             WHERE r.canonical_id = ?1
                             ORDER BY a.source_item_id ASC
                             LIMIT ?2",
                        )?;
                        let ids: Vec<i64> = stmt
                            .query_map([canonical_row_id, MAX_ENTITY_SOURCE_ITEMS], |row| {
                                row.get(0)
                            })?
                            .collect::<Result<Vec<_>, _>>()?;
                        let redirected = (row_id != canonical_row_id).then_some(canonical_pid);
                        EntityTarget {
                            canonical_id: redirected,
                            title,
                            artist,
                            source_item_ids: ids,
                            entity_decision_target: Some(("recording", row_id, canonical_row_id)),
                        }
                    }
                    InspectEntityKind::Appearance => {
                        let Some((row_id, canonical_row_id, canonical_pid, title, artist)) =
                            connection
                                .query_row(
                                    "SELECT ta.id, c.id, c.public_id,
                                            COALESCE(c.display_title, rec.display_title),
                                            ac.display_text
                                     FROM track_appearance ta
                                     JOIN track_appearance c ON c.id = ta.canonical_id
                                     JOIN recording rec ON rec.id = c.recording_id
                                     LEFT JOIN artist_credit ac ON ac.id = COALESCE(c.display_artist_credit_id, rec.display_artist_credit_id)
                                     WHERE ta.public_id = ?1",
                                    [id.as_bytes().as_slice()],
                                    |row| {
                                        Ok((
                                            row.get::<_, i64>(0)?,
                                            row.get::<_, i64>(1)?,
                                            id_from_column(row, 2)?,
                                            row.get::<_, Option<String>>(3)?,
                                            row.get::<_, Option<String>>(4)?,
                                        ))
                                    },
                                )
                                .optional()?
                        else {
                            return Ok(None);
                        };
                        let mut stmt = connection.prepare(
                            "SELECT DISTINCT a.source_item_id
                             FROM assignment a
                             JOIN track_appearance ta ON ta.id = a.appearance_id
                             WHERE ta.canonical_id = ?1
                             ORDER BY a.source_item_id ASC
                             LIMIT ?2",
                        )?;
                        let ids: Vec<i64> = stmt
                            .query_map([canonical_row_id, MAX_ENTITY_SOURCE_ITEMS], |row| {
                                row.get(0)
                            })?
                            .collect::<Result<Vec<_>, _>>()?;
                        let redirected = (row_id != canonical_row_id).then_some(canonical_pid);
                        EntityTarget {
                            canonical_id: redirected,
                            title,
                            artist,
                            source_item_ids: ids,
                            entity_decision_target: Some((
                                "track_appearance",
                                row_id,
                                canonical_row_id,
                            )),
                        }
                    }
                    InspectEntityKind::Release => {
                        let Some((row_id, canonical_row_id, canonical_pid, title, artist)) =
                            connection
                                .query_row(
                                    "SELECT rel.id, c.id, c.public_id, c.display_title, ac.display_text
                                     FROM release rel
                                     JOIN release c ON c.id = rel.canonical_id
                                     LEFT JOIN artist_credit ac ON ac.id = c.display_artist_credit_id
                                     WHERE rel.public_id = ?1",
                                    [id.as_bytes().as_slice()],
                                    |row| {
                                        Ok((
                                            row.get::<_, i64>(0)?,
                                            row.get::<_, i64>(1)?,
                                            id_from_column(row, 2)?,
                                            row.get::<_, Option<String>>(3)?,
                                            row.get::<_, Option<String>>(4)?,
                                        ))
                                    },
                                )
                                .optional()?
                        else {
                            return Ok(None);
                        };
                        let mut stmt = connection.prepare(
                            "SELECT DISTINCT a.source_item_id
                             FROM assignment a
                             JOIN track_appearance ta ON ta.id = a.appearance_id
                             JOIN release rel ON rel.id = ta.release_id
                             WHERE rel.canonical_id = ?1
                             ORDER BY a.source_item_id ASC
                             LIMIT ?2",
                        )?;
                        let ids: Vec<i64> = stmt
                            .query_map([canonical_row_id, MAX_ENTITY_SOURCE_ITEMS], |row| {
                                row.get(0)
                            })?
                            .collect::<Result<Vec<_>, _>>()?;
                        let redirected = (row_id != canonical_row_id).then_some(canonical_pid);
                        EntityTarget {
                            canonical_id: redirected,
                            title,
                            artist,
                            source_item_ids: ids,
                            entity_decision_target: Some(("release", row_id, canonical_row_id)),
                        }
                    }
                    InspectEntityKind::SourceItem => {
                        let Some(row_id) = connection
                            .query_row(
                                "SELECT id FROM source_item WHERE public_id = ?1",
                                [id.as_bytes().as_slice()],
                                |row| row.get::<_, i64>(0),
                            )
                            .optional()?
                        else {
                            return Ok(None);
                        };
                        EntityTarget {
                            canonical_id: None,
                            title: None,
                            artist: None,
                            source_item_ids: vec![row_id],
                            entity_decision_target: None,
                        }
                    }
                    InspectEntityKind::AudioAsset => {
                        let Some(asset_id) = connection
                            .query_row(
                                "SELECT id FROM audio_asset WHERE public_id = ?1",
                                [id.as_bytes().as_slice()],
                                |row| row.get::<_, i64>(0),
                            )
                            .optional()?
                        else {
                            return Ok(None);
                        };
                        let mut stmt = connection.prepare(
                            "SELECT DISTINCT source_item_id
                             FROM source_item_revision
                             WHERE audio_asset_id = ?1
                             ORDER BY source_item_id ASC
                             LIMIT ?2",
                        )?;
                        let ids: Vec<i64> = stmt
                            .query_map([asset_id, MAX_ENTITY_SOURCE_ITEMS], |row| row.get(0))?
                            .collect::<Result<Vec<_>, _>>()?;
                        EntityTarget {
                            canonical_id: None,
                            title: None,
                            artist: None,
                            source_item_ids: ids,
                            entity_decision_target: None,
                        }
                    }
                };
                let EntityTarget {
                    canonical_id,
                    title,
                    artist,
                    source_item_ids,
                    entity_decision_target,
                } = target;

                let mut source_items = Vec::with_capacity(source_item_ids.len());
                for si_id in source_item_ids {
                    if let Some(item) = load_inspected_source_item(
                        connection,
                        si_id,
                        this_server_id,
                        local_roots.as_ref(),
                    )? {
                        source_items.push(item);
                    }
                }

                let entity_decisions = if let Some((entity_kind, row_id, canonical_row_id)) =
                    entity_decision_target
                {
                    let mut stmt = connection.prepare(
                        "SELECT id, action, method, resolver_version, score, runner_up_gap,
                                needs_review, actor_kind, rationale, decided_at_ms, evidence_json
                         FROM identity_decision
                         WHERE entity_kind = ?1
                           AND (entity_id IN (?2, ?3) OR target_entity_id IN (?2, ?3))
                         ORDER BY id DESC
                         LIMIT ?4",
                    )?;
                    let rows = stmt.query_map(
                        rusqlite::params![
                            entity_kind,
                            row_id,
                            canonical_row_id,
                            MAX_PROVENANCE_CHILDREN
                        ],
                        map_identity_decision_row,
                    )?;
                    rows.collect::<Result<Vec<_>, _>>()?
                } else {
                    Vec::new()
                };

                Ok(Some(EntityProvenance {
                    kind,
                    id,
                    canonical_id,
                    title,
                    artist,
                    source_items,
                    entity_decisions,
                }))
            })
        })
    }

    /// Reads a derived `play_event` along with its raw observations, deliveries, and corrections.
    pub fn inspect_play(&self, play_id: Id) -> Result<Option<PlayProvenance>, Error> {
        self.read(move |connection| {
            crate::sqlite::snapshot(connection, |connection| {
                let Some(play_row) = connection
                    .query_row(
                        "SELECT pe.id, l.public_id, l.name,
                                rec.public_id, ta.public_id, si.public_id,
                                rec.display_title, ac.display_text,
                                pe.event_at_utc_ms, pe.time_anchor, pe.clock_basis,
                                pe.zone_name, pe.zone_basis, pe.calendar_basis,
                                pe.evidence_class, pe.ms_played, pe.track_duration_ms,
                                pe.outcome, pe.counts, pe.count_basis,
                                pe.play_rule_version, pe.grouper_version, pe.excluded
                         FROM play_event pe
                         JOIN listener l ON l.id = pe.listener_id
                         LEFT JOIN recording rec ON rec.id = pe.recording_id
                         LEFT JOIN artist_credit ac ON ac.id = rec.display_artist_credit_id
                         LEFT JOIN track_appearance ta ON ta.id = pe.appearance_id
                         LEFT JOIN source_item si ON si.id = pe.source_item_id
                         WHERE pe.public_id = ?1",
                        [play_id.as_bytes().as_slice()],
                        |row| {
                            let rec_pid: Option<Vec<u8>> = row.get(3)?;
                            let app_pid: Option<Vec<u8>> = row.get(4)?;
                            let si_pid: Option<Vec<u8>> = row.get(5)?;
                            Ok((
                                row.get::<_, i64>(0)?,
                                id_from_column(row, 1)?,
                                row.get::<_, String>(2)?,
                                rec_pid.as_deref().and_then(id_from_blob),
                                app_pid.as_deref().and_then(id_from_blob),
                                si_pid.as_deref().and_then(id_from_blob),
                                row.get::<_, Option<String>>(6)?,
                                row.get::<_, Option<String>>(7)?,
                                row.get::<_, Option<i64>>(8)?,
                                row.get::<_, String>(9)?,
                                row.get::<_, String>(10)?,
                                row.get::<_, Option<String>>(11)?,
                                row.get::<_, String>(12)?,
                                row.get::<_, String>(13)?,
                                row.get::<_, String>(14)?,
                                row.get::<_, Option<i64>>(15)?.map(unsigned),
                                row.get::<_, Option<i64>>(16)?.map(unsigned),
                                row.get::<_, String>(17)?,
                                row.get::<_, i64>(18)? != 0,
                                row.get::<_, String>(19)?,
                                unsigned_u32(row.get(20)?),
                                unsigned_u32(row.get(21)?),
                                row.get::<_, i64>(22)? != 0,
                            ))
                        },
                    )
                    .optional()?
                else {
                    return Ok(None);
                };

                let (
                    play_row_id,
                    listener_id,
                    listener_name,
                    recording_id,
                    appearance_id,
                    source_item_id,
                    title,
                    artist,
                    event_at_utc_ms,
                    time_anchor,
                    clock_basis,
                    zone_name,
                    zone_basis,
                    calendar_basis,
                    evidence_class,
                    ms_played,
                    track_duration_ms,
                    outcome,
                    counts,
                    count_basis,
                    play_rule_version,
                    grouper_version,
                    excluded,
                ) = play_row;

                let observations = {
                    let mut stmt = connection.prepare(
                        "SELECT ho.id, e.name, e.kind, ho.kind, peo.role, peo.joined_by,
                                ho.source_key, ho.captured_at_utc_ms, ho.received_at_utc_ms,
                                ho.clock_skew_ms,
                                COALESCE(ol.ms_played, ois.ms_played),
                                ol.end_reason, oi.artist, oi.title, oi.album, ho.payload_json
                         FROM play_event_observation peo
                         JOIN history_observation ho ON ho.id = peo.observation_id
                         JOIN history_endpoint e ON e.id = ho.endpoint_id
                         LEFT JOIN observation_listen ol ON ol.observation_id = ho.id
                         LEFT JOIN observation_import oi ON oi.observation_id = ho.id
                         LEFT JOIN observation_import_stream ois ON ois.observation_id = ho.id
                         WHERE peo.play_event_id = ?1
                         ORDER BY CASE peo.role WHEN 'primary' THEN 0 ELSE 1 END, ho.id ASC
                         LIMIT ?2",
                    )?;
                    let rows = stmt.query_map([play_row_id, MAX_PROVENANCE_CHILDREN], |row| {
                        let source_key: Vec<u8> = row.get(6)?;
                        let payload_json: String = row.get(15)?;
                        Ok(InspectedObservation {
                            id: row.get(0)?,
                            endpoint_name: row.get(1)?,
                            endpoint_kind: row.get(2)?,
                            kind: row.get(3)?,
                            role: row.get(4)?,
                            joined_by: row.get(5)?,
                            source_key: format_source_key(&source_key),
                            captured_at_utc_ms: row.get(7)?,
                            received_at_utc_ms: row.get(8)?,
                            clock_skew_ms: row.get(9)?,
                            ms_played: row.get::<_, Option<i64>>(10)?.map(unsigned),
                            end_reason: row.get(11)?,
                            artist: row.get(12)?,
                            title: row.get(13)?,
                            album: row.get(14)?,
                            payload: parse_json_or_string(&payload_json),
                        })
                    })?;
                    rows.collect::<Result<Vec<_>, _>>()?
                };

                let deliveries = {
                    let mut stmt = connection.prepare(
                        "SELECT d.id, d.observation_id, e.name, d.state, d.sent_timestamp_s,
                                d.artist, d.title, d.album, d.accepted_artist, d.accepted_title,
                                d.ignored_code, d.detail, d.attempts, d.next_attempt_at_ms, d.updated_at_ms
                         FROM history_delivery d
                         JOIN history_endpoint e ON e.id = d.endpoint_id
                         WHERE d.observation_id IN (
                             SELECT observation_id FROM play_event_observation WHERE play_event_id = ?1
                         )
                         ORDER BY d.id ASC
                         LIMIT ?2",
                    )?;
                    let rows =
                        stmt.query_map([play_row_id, MAX_PROVENANCE_CHILDREN], map_delivery_row)?;
                    rows.collect::<Result<Vec<_>, _>>()?
                };

                let corrections = {
                    let mut stmt = connection.prepare(
                        "SELECT DISTINCT oc.observation_id, oc.verb, oc.note, ct.target_observation_id,
                                rec.public_id, ho.received_at_utc_ms
                         FROM observation_correction oc
                         JOIN history_observation ho ON ho.id = oc.observation_id
                         LEFT JOIN correction_target ct
                           ON ct.correction_id = oc.observation_id
                          AND ct.target_observation_id IN (
                              SELECT observation_id FROM play_event_observation WHERE play_event_id = ?1
                          )
                         LEFT JOIN correction_recording cr ON cr.correction_id = oc.observation_id
                         LEFT JOIN recording rec ON rec.id = cr.recording_id
                         WHERE ct.correction_id IS NOT NULL
                            OR (
                                oc.target_endpoint_id IS NOT NULL
                                AND EXISTS (
                                    SELECT 1
                                    FROM play_event_observation peo
                                    JOIN history_observation target_ho ON target_ho.id = peo.observation_id
                                    LEFT JOIN observation_listen ol ON ol.observation_id = target_ho.id
                                    LEFT JOIN observation_import oi ON oi.observation_id = target_ho.id
                                    WHERE peo.play_event_id = ?1
                                      AND target_ho.endpoint_id = oc.target_endpoint_id
                                      AND (ol.observation_id IS NOT NULL OR oi.observation_id IS NOT NULL)
                                      AND (cr.source_item_id IS NULL OR target_ho.source_item_id = cr.source_item_id)
                                      AND (
                                          oc.target_from_utc_ms IS NULL
                                          OR COALESCE(ol.started_at_utc_ms, oi.played_at_utc_ms) >= oc.target_from_utc_ms
                                      )
                                      AND (
                                          oc.target_to_utc_ms IS NULL
                                          OR COALESCE(ol.started_at_utc_ms, oi.played_at_utc_ms) < oc.target_to_utc_ms
                                      )
                                )
                            )
                         ORDER BY oc.observation_id ASC
                         LIMIT ?2",
                    )?;
                    let rows = stmt.query_map([play_row_id, MAX_PROVENANCE_CHILDREN], |row| {
                        let rec_pid: Option<Vec<u8>> = row.get(4)?;
                        Ok(InspectedPlayCorrection {
                            observation_id: row.get(0)?,
                            verb: row.get(1)?,
                            note: row.get(2)?,
                            target_observation_id: row.get(3)?,
                            recording_id: rec_pid.as_deref().and_then(id_from_blob),
                            created_at_ms: row.get(5)?,
                        })
                    })?;
                    rows.collect::<Result<Vec<_>, _>>()?
                };

                Ok(Some(PlayProvenance {
                    play_id,
                    listener_id,
                    listener_name,
                    recording_id,
                    appearance_id,
                    source_item_id,
                    title,
                    artist,
                    event_at_utc_ms,
                    time_anchor,
                    clock_basis,
                    zone_name,
                    zone_basis,
                    calendar_basis,
                    evidence_class,
                    ms_played,
                    track_duration_ms,
                    outcome,
                    counts,
                    count_basis,
                    play_rule_version,
                    grouper_version,
                    excluded,
                    observations,
                    deliveries,
                    corrections,
                }))
            })
        })
    }
}

fn map_identity_decision_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<InspectedIdentityDecision> {
    let evidence_json: String = row.get(10)?;
    Ok(InspectedIdentityDecision {
        id: row.get(0)?,
        action: row.get(1)?,
        method: row.get(2)?,
        resolver_version: unsigned_u32(row.get(3)?),
        score: row.get(4)?,
        runner_up_gap: row.get(5)?,
        needs_review: row.get::<_, i64>(6)? != 0,
        actor_kind: row.get(7)?,
        rationale: row.get(8)?,
        decided_at_ms: row.get(9)?,
        evidence: parse_json_or_string(&evidence_json),
    })
}

fn map_delivery_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<InspectedDelivery> {
    Ok(InspectedDelivery {
        id: row.get(0)?,
        observation_id: row.get(1)?,
        endpoint_name: row.get(2)?,
        state: row.get(3)?,
        sent_timestamp_s: row.get(4)?,
        artist: row.get(5)?,
        title: row.get(6)?,
        album: row.get(7)?,
        accepted_artist: row.get(8)?,
        accepted_title: row.get(9)?,
        ignored_code: row.get(10)?,
        detail: row.get(11)?,
        attempts: unsigned_u32(row.get(12)?),
        next_attempt_at_ms: row.get(13)?,
        updated_at_ms: row.get(14)?,
    })
}

fn load_inspected_source_item(
    connection: &Connection,
    si_id: i64,
    this_server_id: Id,
    local_roots: Option<&HashMap<String, String>>,
) -> Result<Option<InspectedSourceItem>, Error> {
    let Some((
        public_id,
        source_name,
        source_kind,
        item_kind,
        server_id,
        server_name,
        root_name,
        rel_path,
        current_revision_id,
        size_bytes,
        mtime_ms,
        missing_since_ms,
        retired_at_ms,
        first_seen_at_ms,
    )) = connection
        .query_row(
            "SELECT si.public_id, src.name, src.kind, si.item_kind,
                    srv.public_id, srv.name, lr.name, si.provider_key,
                    si.current_revision_id, si.size_bytes, si.mtime_ms,
                    si.missing_since_ms, si.retired_at_ms, si.first_seen_at_ms
             FROM source_item si
             JOIN source_instance src ON src.id = si.source_instance_id
             LEFT JOIN library_root lr ON lr.id = src.root_id
             LEFT JOIN server srv ON srv.id = lr.server_id
             WHERE si.id = ?1",
            [si_id],
            |row| {
                let srv_pid: Option<Vec<u8>> = row.get(4)?;
                Ok((
                    id_from_column(row, 0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    srv_pid.as_deref().and_then(id_from_blob),
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, Option<i64>>(8)?,
                    row.get::<_, Option<i64>>(9)?.map(unsigned),
                    row.get::<_, Option<i64>>(10)?,
                    row.get::<_, Option<i64>>(11)?,
                    row.get::<_, Option<i64>>(12)?,
                    row.get::<_, i64>(13)?,
                ))
            },
        )
        .optional()?
    else {
        return Ok(None);
    };

    let abs_path = match (local_roots, server_id, root_name.as_deref()) {
        (Some(roots), Some(srv), Some(root)) if srv == this_server_id => {
            roots.get(root).map(|base| {
                Path::new(base)
                    .join(&rel_path)
                    .to_string_lossy()
                    .into_owned()
            })
        }
        _ => None,
    };

    let revisions = {
        let mut stmt = connection.prepare(
            "SELECT r.revision_no, r.observed_at_ms, r.payload_hash, aa.public_id
             FROM source_item_revision r
             LEFT JOIN audio_asset aa ON aa.id = r.audio_asset_id
             WHERE r.source_item_id = ?1
             ORDER BY r.revision_no DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map([si_id, MAX_PROVENANCE_CHILDREN], |row| {
            let payload_hash: Vec<u8> = row.get(2)?;
            let asset_pid: Option<Vec<u8>> = row.get(3)?;
            Ok(InspectedSourceRevision {
                revision_no: unsigned_u32(row.get(0)?),
                observed_at_ms: row.get(1)?,
                payload_hash: hex::encode(&payload_hash),
                audio_asset_id: asset_pid.as_deref().and_then(id_from_blob),
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()?
    };

    let audio_asset = if let Some(rev_id) = current_revision_id {
        connection
            .query_row(
                "SELECT aa.public_id, aa.hash_scheme, aa.audio_hash, aa.flac_md5,
                        aa.codec, aa.sample_rate_hz, aa.channel_layout, aa.bit_depth,
                        aa.bitrate_kbps, aa.lossless, aa.duration_ms
                 FROM source_item_revision r
                 JOIN audio_asset aa ON aa.id = r.audio_asset_id
                 WHERE r.id = ?1",
                [rev_id],
                |row| {
                    let audio_hash: Vec<u8> = row.get(2)?;
                    let flac_md5: Option<Vec<u8>> = row.get(3)?;
                    Ok(InspectedAudioAsset {
                        audio_asset_id: id_from_column(row, 0)?,
                        hash_scheme: unsigned_u32(row.get(1)?),
                        audio_hash: hex::encode(&audio_hash),
                        flac_md5: flac_md5.as_deref().map(hex::encode),
                        codec: row.get(4)?,
                        sample_rate_hz: unsigned_u32(row.get(5)?),
                        channel_layout: row.get(6)?,
                        bit_depth: row.get::<_, Option<i64>>(7)?.map(unsigned_u32),
                        bitrate_kbps: row.get::<_, Option<i64>>(8)?.map(unsigned_u32),
                        lossless: row.get::<_, i64>(9)? != 0,
                        duration_ms: unsigned(row.get(10)?),
                    })
                },
            )
            .optional()?
    } else {
        None
    };

    let identifier_claims = if let Some(rev_id) = current_revision_id {
        let mut stmt = connection.prepare(
            "SELECT ic.scope, ic.namespace, ic.value, ms.kind
             FROM identifier_claim ic
             JOIN metadata_source ms ON ms.id = ic.metadata_source_id
             WHERE ic.source_item_id = ?1 AND ic.revision_id = ?2
             ORDER BY ic.scope, ic.namespace, ic.ordinal
             LIMIT ?3",
        )?;
        let rows = stmt.query_map([si_id, rev_id, MAX_PROVENANCE_CHILDREN], |row| {
            Ok(InspectedIdentifierClaim {
                scope: row.get(0)?,
                namespace: row.get(1)?,
                value: row.get(2)?,
                source: row.get(3)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()?
    } else {
        Vec::new()
    };

    let metadata_claims = if let Some(rev_id) = current_revision_id {
        let mut stmt = connection.prepare(
            "SELECT mc.scope, mc.field, mc.value, ms.kind
             FROM metadata_claim mc
             JOIN metadata_source ms ON ms.id = mc.metadata_source_id
             WHERE mc.source_item_id = ?1 AND mc.revision_id = ?2
             ORDER BY mc.scope, mc.field, mc.ordinal
             LIMIT ?3",
        )?;
        let rows = stmt.query_map([si_id, rev_id, MAX_PROVENANCE_CHILDREN], |row| {
            Ok(InspectedMetadataClaim {
                scope: row.get(0)?,
                field: row.get(1)?,
                value: row.get(2)?,
                source: row.get(3)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()?
    } else {
        Vec::new()
    };

    let assignments = {
        let mut stmt = connection.prepare(
            "SELECT a.id, rec.public_id, ta.public_id, a.decision_id,
                    a.effective_from_ms, a.superseded_at_ms
             FROM assignment a
             JOIN recording rec ON rec.id = a.recording_id
             LEFT JOIN track_appearance ta ON ta.id = a.appearance_id
             WHERE a.source_item_id = ?1
             ORDER BY a.id DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map([si_id, MAX_PROVENANCE_CHILDREN], |row| {
            let app_pid: Option<Vec<u8>> = row.get(2)?;
            let superseded_at_ms: Option<i64> = row.get(5)?;
            Ok(InspectedAssignment {
                id: row.get(0)?,
                recording_id: id_from_column(row, 1)?,
                appearance_id: app_pid.as_deref().and_then(id_from_blob),
                decision_id: row.get(3)?,
                effective_from_ms: row.get(4)?,
                superseded_at_ms,
                current: superseded_at_ms.is_none(),
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()?
    };

    let decisions = {
        let mut stmt = connection.prepare(
            "SELECT id, action, method, resolver_version, score, runner_up_gap,
                    needs_review, actor_kind, rationale, decided_at_ms, evidence_json
             FROM identity_decision
             WHERE source_item_id = ?1
             ORDER BY id DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map([si_id, MAX_PROVENANCE_CHILDREN], map_identity_decision_row)?;
        rows.collect::<Result<Vec<_>, _>>()?
    };

    let exclusions = {
        let mut stmt = connection.prepare(
            "SELECT ie.id, rec.public_id, ie.actor_kind, ie.reason, ie.created_at_ms
             FROM identity_exclusion ie
             JOIN recording rec ON rec.id = ie.recording_id
             WHERE ie.source_item_id = ?1
             ORDER BY ie.id DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map([si_id, MAX_PROVENANCE_CHILDREN], |row| {
            Ok(InspectedIdentityExclusion {
                id: row.get(0)?,
                recording_id: id_from_column(row, 1)?,
                actor_kind: row.get(2)?,
                reason: row.get(3)?,
                created_at_ms: row.get(4)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()?
    };

    Ok(Some(InspectedSourceItem {
        source_item_id: public_id,
        source_name,
        source_kind,
        item_kind,
        server_id,
        server_name,
        root_name,
        rel_path,
        abs_path,
        size_bytes,
        mtime_ms,
        missing_since_ms,
        retired_at_ms,
        first_seen_at_ms,
        audio_asset,
        revisions,
        identifier_claims,
        metadata_claims,
        assignments,
        decisions,
        exclusions,
    }))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::{OsEntropy, SystemClock};

    #[test]
    fn every_server_table_is_either_explicitly_allowed_or_excluded() {
        let store =
            ServerStore::open_in_memory(Arc::new(SystemClock), Arc::new(OsEntropy)).unwrap();
        let summary = store.inspect_db(None).unwrap();
        assert!(!summary.tables.is_empty());
        for table in &summary.tables {
            let allowed = SERVER_ALLOWLIST.contains(&table.name.as_str());
            let excluded = SERVER_EXCLUDED.contains(&table.name.as_str());
            assert!(
                allowed ^ excluded,
                "server.db table '{}' must be in exactly one of SERVER_ALLOWLIST or SERVER_EXCLUDED",
                table.name
            );
            assert_eq!(table.browsable, allowed);
        }
    }

    #[test]
    fn every_library_table_is_either_explicitly_allowed_or_excluded() {
        let server =
            ServerStore::open_in_memory(Arc::new(SystemClock), Arc::new(OsEntropy)).unwrap();
        let (identity, _) = server.ensure_identity("desk").unwrap();
        let (store, _) = LibraryStore::create_in_memory(
            &identity,
            crate::WriteRequest {
                method: "test.init",
                command: None,
                client: None,
            },
            |_| "{}".to_owned(),
            Arc::new(SystemClock),
            Arc::new(OsEntropy),
        )
        .unwrap();
        let summary = store.inspect_db(None).unwrap();
        assert!(!summary.tables.is_empty());
        for table in &summary.tables {
            let allowed = LIBRARY_ALLOWLIST.contains(&table.name.as_str());
            let excluded = LIBRARY_EXCLUDED.contains(&table.name.as_str());
            assert!(
                allowed ^ excluded,
                "library.db table '{}' must be in exactly one of LIBRARY_ALLOWLIST or LIBRARY_EXCLUDED",
                table.name
            );
            assert_eq!(table.browsable, allowed);
        }
    }

    #[test]
    fn server_secret_tables_are_forbidden_and_paths_are_redacted_without_folders_manage() {
        let store =
            ServerStore::open_in_memory(Arc::new(SystemClock), Arc::new(OsEntropy)).unwrap();
        store.ensure_identity("desk").unwrap();
        store.add_root("music", "/srv/music").unwrap();
        store
            .backup_taken(&crate::BackupTaken {
                at_ms: 1_700_000_000_000,
                path: "/srv/backups/snap-1".to_owned(),
                reason: "on_demand".to_owned(),
            })
            .unwrap();
        store
            .backup_failed(&crate::BackupFailed {
                at_ms: 1_700_000_001_000,
                message: "could not write /srv/backups/snap-2: permission denied".to_owned(),
            })
            .unwrap();

        for secret_table in SERVER_EXCLUDED {
            let err = store
                .inspect_table(
                    &InspectTableParams {
                        db: InspectDb::Server,
                        table: (*secret_table).to_owned(),
                        offset: 0,
                        limit: Some(10),
                        order: None,
                        column: None,
                        equals: None,
                    },
                    true,
                )
                .unwrap_err();
            assert!(
                matches!(err, InspectTableError::Forbidden(_)),
                "expected Forbidden for {secret_table}, got {err:?}"
            );
        }

        // With show_paths = true, root.abs_path is visible.
        let with_paths = store
            .inspect_table(
                &InspectTableParams {
                    db: InspectDb::Server,
                    table: "root".to_owned(),
                    offset: 0,
                    limit: Some(10),
                    order: None,
                    column: None,
                    equals: None,
                },
                true,
            )
            .unwrap();
        let abs_idx = with_paths
            .columns
            .iter()
            .position(|c| c.name == "abs_path")
            .unwrap();
        assert_eq!(
            with_paths.rows[0][abs_idx],
            Value::String("/srv/music".to_owned())
        );

        // With show_paths = false, root.abs_path is redacted to null and cannot be filtered on.
        let redacted = store
            .inspect_table(
                &InspectTableParams {
                    db: InspectDb::Server,
                    table: "root".to_owned(),
                    offset: 0,
                    limit: Some(10),
                    order: None,
                    column: None,
                    equals: None,
                },
                false,
            )
            .unwrap();
        assert_eq!(redacted.rows[0][abs_idx], Value::Null);

        let probe = store
            .inspect_table(
                &InspectTableParams {
                    db: InspectDb::Server,
                    table: "root".to_owned(),
                    offset: 0,
                    limit: Some(10),
                    order: None,
                    column: Some("abs_path".to_owned()),
                    equals: Some("/srv/music".to_owned()),
                },
                false,
            )
            .unwrap_err();
        assert!(matches!(probe, InspectTableError::Forbidden(_)));

        // backup.last and backup.failed in server_setting carry paths and are redacted without folders:manage.
        let settings = store
            .inspect_table(
                &InspectTableParams {
                    db: InspectDb::Server,
                    table: "server_setting".to_owned(),
                    offset: 0,
                    limit: Some(20),
                    order: None,
                    column: None,
                    equals: None,
                },
                false,
            )
            .unwrap();
        let key_idx = settings
            .columns
            .iter()
            .position(|c| c.name == "key")
            .unwrap();
        let val_idx = settings
            .columns
            .iter()
            .position(|c| c.name == "value_json")
            .unwrap();
        let mut saw_last = false;
        let mut saw_failed = false;
        for row in &settings.rows {
            if row[key_idx].as_str() == Some("backup.last") {
                saw_last = true;
                assert_eq!(row[val_idx], Value::Null);
            }
            if row[key_idx].as_str() == Some("backup.failed") {
                saw_failed = true;
                assert_eq!(row[val_idx], Value::Null);
            }
        }
        assert!(saw_last && saw_failed);

        // Filtering on job.error_json or outbox.last_error without folders:manage is forbidden.
        for (tbl, col) in [
            ("job", "error_json"),
            ("job", "checkpoint_json"),
            ("job_item", "detail_json"),
            ("outbox", "last_error"),
        ] {
            let err = store
                .inspect_table(
                    &InspectTableParams {
                        db: InspectDb::Server,
                        table: tbl.to_owned(),
                        offset: 0,
                        limit: Some(10),
                        order: None,
                        column: Some(col.to_owned()),
                        equals: Some("probe".to_owned()),
                    },
                    false,
                )
                .unwrap_err();
            assert!(
                matches!(err, InspectTableError::Forbidden(_)),
                "expected Forbidden filtering {tbl}.{col} without folders:manage, got {err:?}"
            );
        }
    }
}
