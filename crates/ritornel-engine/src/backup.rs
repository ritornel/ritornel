//! Copies of a database: the backup taken on demand, and the one taken
//! before a migration. See "Backups and pre-migration snapshots" in
//! docs/internals/server.md.
//!
//! A copy is made with `VACUUM INTO` on a read-only connection of its own.
//! It is a consistent snapshot of the database as it was when the copy
//! began, it is made beside the writer and the readers without holding
//! either, and it is one file with no write-ahead log beside it, so it can
//! be carried anywhere and opened as it is.

use std::path::Path;

use rusqlite::{OpenFlags, OptionalExtension};

use crate::migrate::{self, Migration};
use crate::sqlite::Location;
use crate::{Error, Id};

/// One of the two databases a data directory can hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Database {
    /// `server.db`, which every server has.
    Server,
    /// `library.db`, on the server that is its library's hub.
    Library,
}

impl Database {
    fn migrations(self) -> &'static [Migration] {
        match self {
            Self::Server => migrate::SERVER,
            Self::Library => migrate::LIBRARY,
        }
    }
}

/// A database whose schema this program would take forward when it opens
/// it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pending {
    /// The schema version the file is at.
    pub from: i64,
    /// The schema version opening it would bring it to.
    pub to: i64,
}

/// Whether opening the database at `path` would migrate data that is in
/// it. A file that is missing or empty has nothing to lose and is not
/// pending, nor is one that opening would refuse, such as one written by a
/// newer version of this program: it is left as it is.
///
/// The file is only read.
pub fn pending_migrations(path: &Path, database: Database) -> Result<Option<Pending>, Error> {
    let Some(connection) = open_existing(path)? else {
        return Ok(None);
    };
    pending_in(&connection, database.migrations())
}

/// The ID the database at `path` records for itself: the server's in
/// `server.db`, the library's in `library.db`. It is read before the file
/// is opened to be migrated, so that a backup taken then says whose it is.
/// A file that is missing or empty records none, as does one without the
/// table or its row.
///
/// The file is only read.
pub fn recorded_id(path: &Path, database: Database) -> Result<Option<Id>, Error> {
    let Some(connection) = open_existing(path)? else {
        return Ok(None);
    };
    let (table, query) = match database {
        Database::Server => (
            "server_identity",
            "SELECT server_id FROM server_identity WHERE id = 1",
        ),
        Database::Library => (
            "library_meta",
            "SELECT library_id FROM library_meta WHERE id = 1",
        ),
    };
    if !has_table(&connection, table)? {
        return Ok(None);
    }
    let bytes = connection
        .query_row(query, [], |row| row.get::<_, Option<Vec<u8>>>(0))
        .optional()?
        .flatten();
    Ok(bytes
        .and_then(|bytes| <[u8; 16]>::try_from(bytes).ok())
        .map(Id::from_bytes))
}

fn open_existing(path: &Path) -> Result<Option<rusqlite::Connection>, Error> {
    match std::fs::metadata(path) {
        Ok(file) if file.len() == 0 => Ok(None),
        Ok(_) => Location::File(path.to_owned())
            .open(OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(Error::Io(error)),
    }
}

fn has_table(db: &rusqlite::Connection, name: &str) -> Result<bool, Error> {
    Ok(db.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        [name],
        |row| row.get(0),
    )?)
}

/// The committed application state a pre-migration snapshot protects. Only
/// the digest is kept: server rows include local credentials and service keys.
#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MigrationState {
    server_digest: Option<String>,
    history: HistoryStart,
    library_revision: Option<i64>,
}

impl MigrationState {
    /// Read through SQLite so committed WAL frames count, and journal-mode
    /// changes, rolled-back statements and file timestamps do not.
    pub fn read(server: &Path, library: &Path) -> Result<Self, Error> {
        Self::read_from(server, library, None)
    }

    /// Compare with the saved state. Jobs that had finished and receipts
    /// that were final at capture can be skipped on every retry.
    pub fn unchanged(&self, server: &Path, library: &Path) -> Result<bool, Error> {
        Ok(self == &Self::read_from(server, library, Some(self.history))?)
    }

    fn read_from(
        server: &Path,
        library: &Path,
        history: Option<HistoryStart>,
    ) -> Result<Self, Error> {
        let (server_digest, history) = if let Some(db) = open_existing(server)? {
            let tx = db.unchecked_transaction()?;
            let history = match history {
                Some(known) => known,
                None => HistoryStart::read(&tx)?,
            };
            (Some(server_digest(&tx, history)?), history)
        } else {
            (None, HistoryStart::default())
        };
        let library_revision = match open_existing(library)? {
            Some(db) if has_table(&db, "library_meta")? => db
                .query_row("SELECT rev FROM library_meta WHERE id = 1", [], |row| {
                    row.get(0)
                })
                .optional()?,
            _ => None,
        };
        Ok(Self {
            server_digest,
            history,
            library_revision,
        })
    }
}

// The oldest row that may still change, or the highest existing ID if
// all rows are final. New jobs and receipts append above that ID.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct HistoryStart {
    job: i64,
    receipt: i64,
}

const ACTIVE_JOB: &str = "state IN ('queued','running','cancelling')";
const OPEN_MOVE: &str =
    "method = 'playback.move' AND json_extract(result_json, '$.status') = 'accepted'";

impl HistoryStart {
    fn read(db: &rusqlite::Connection) -> Result<Self, Error> {
        let first = |table, predicate| -> Result<i64, Error> {
            if !has_table(db, table)? {
                return Ok(0);
            }
            Ok(db.query_row(&format!(
                "SELECT coalesce((SELECT min(id) FROM {table} WHERE {predicate}), max(id), 0) FROM {table}"
            ), [], |row| row.get(0))?)
        };
        Ok(Self {
            job: first("job", ACTIVE_JOB)?,
            receipt: first("command_receipt", OPEN_MOVE)?,
        })
    }
}

fn server_digest(db: &rusqlite::Connection, history: HistoryStart) -> Result<String, Error> {
    let mut digest = blake3::Hasher::new();
    // Old schemas lack some tables. Skip the migration ledger itself: a
    // repair to a broken migration must not look like new player activity.
    let mut tables = db.prepare(
        "SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT GLOB 'sqlite_*'
         AND name != 'schema_migration' ORDER BY name",
    )?;
    for table in tables.query_map([], |row| row.get::<_, String>(0))? {
        let table = table?;
        digest.update(&(table.len() as u64).to_le_bytes());
        digest.update(table.as_bytes());
        let quoted = format!("\"{}\"", table.replace('"', "\"\""));
        // Jobs and their item records accumulate over the life of the
        // server. Finished jobs and committed items do not change.
        if matches!(
            table.as_str(),
            "job" | "job_item" | "listen_staging" | "queue_entry" | "outbox"
        ) {
            // Separate aggregates let SQLite count the b-tree and seek its
            // last row without decoding every historical payload.
            hash_rows(
                db,
                &format!(
                    "SELECT (SELECT count(*) FROM {quoted}), coalesce(max(id), 0) FROM {quoted}"
                ),
                &mut digest,
            )?;
        }
        let sql = match table.as_str() {
            // Closed listens are immutable until delivered. Include the
            // latest listen's identity too: SQLite can reuse a deleted rowid.
            "listen_staging" => "SELECT * FROM listen_staging WHERE ended_at_utc_ms IS NULL
                UNION ALL SELECT * FROM listen_staging WHERE id = (SELECT max(id) FROM listen_staging)
                ORDER BY id".to_owned(),
            // Queue edits advance playback_session.queue_revision; marking
            // an unplayable entry can also change its reason independently.
            "queue_entry" => "SELECT id, failed_reason FROM queue_entry WHERE failed_reason IS NOT NULL
                ORDER BY id".to_owned(),
            // New deliveries have zero attempts. A retry updates an existing
            // row. Include the newest delivery even before its first attempt.
            "outbox" => "SELECT * FROM outbox WHERE attempts != 0 OR id = (SELECT max(id) FROM outbox)
                ORDER BY id".to_owned(),
            "job_item" => continue,
            "job" => format!("SELECT * FROM job WHERE id >= {} AND {ACTIVE_JOB} ORDER BY id", history.job),
            // A move receipt can change from accepted to done with no new
            // receipt. Its recovery journal may already be gone by the retry.
            "command_receipt" => {
                // A new receipt advances the last ID. Expiry of older receipts
                // changes the row count even when no command has been issued.
                hash_rows(db, "SELECT coalesce(max(id), 0) FROM command_receipt", &mut digest)?;
                format!("SELECT id FROM command_receipt WHERE id >= {} AND {OPEN_MOVE} ORDER BY id", history.receipt)
            }
            _ => format!("SELECT * FROM {quoted} ORDER BY rowid"),
        };
        hash_rows(db, &sql, &mut digest)?;
    }
    Ok(digest.finalize().to_hex().to_string())
}

/// Frame each value by its SQLite type and length so adjacent text and
/// blob fields cannot run together in the digest.
fn hash_rows(
    db: &rusqlite::Connection,
    sql: &str,
    digest: &mut blake3::Hasher,
) -> Result<(), Error> {
    use rusqlite::types::ValueRef;
    let mut stmt = db.prepare(sql)?;
    let columns = stmt.column_count();
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        digest.update(b"row");
        for col in 0..columns {
            match row.get_ref(col)? {
                ValueRef::Null => {
                    digest.update(b"n");
                }
                ValueRef::Integer(i) => {
                    digest.update(b"i").update(&i.to_le_bytes());
                }
                ValueRef::Real(f) => {
                    digest.update(b"f").update(&f.to_le_bytes());
                }
                value @ (ValueRef::Text(bytes) | ValueRef::Blob(bytes)) => {
                    digest
                        .update(if matches!(value, ValueRef::Text(_)) {
                            b"t"
                        } else {
                            b"b"
                        })
                        .update(&(bytes.len() as u64).to_le_bytes())
                        .update(bytes);
                }
            }
        }
    }
    Ok(())
}

fn pending_in(
    connection: &rusqlite::Connection,
    migrations: &[Migration],
) -> Result<Option<Pending>, Error> {
    let known = migrations.last().map_or(0, |last| last.version);
    let from = match migrate::recorded(connection) {
        Ok(recorded) => recorded.last().map_or(0, |&(version, _)| version),
        // Opening it will say what is wrong; a copy cannot help.
        Err(Error::ForeignDatabase) => return Ok(None),
        Err(error) => return Err(error),
    };
    Ok((from > 0 && from < known).then_some(Pending { from, to: known }))
}

/// Copies the database at `from` to a new file at `to`, which must not
/// exist, and checks the copy. The source may be in use: the copy is of
/// the last commit before it began.
///
/// The copy is synced to its disk before this returns, and a copy that
/// fails SQLite's quick check is an error, so what is returned is a
/// database that opens.
pub fn copy_database(from: &Path, to: &Path) -> Result<(), Error> {
    let target = to.to_str().ok_or(Error::Invalid(
        "a backup's path must be text that SQLite can be given",
    ))?;
    if to.try_exists().unwrap_or(true) {
        return Err(Error::Invalid("a backup is never written over a file"));
    }
    let source = Location::File(from.to_owned()).open(OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    source.execute("VACUUM INTO ?1", [target])?;
    drop(source);
    // Opened to write, as Windows asks of a file it is to flush.
    std::fs::OpenOptions::new()
        .write(true)
        .open(to)
        .and_then(|file| file.sync_all())
        .map_err(Error::Io)?;

    let copy = Location::File(to.to_owned()).open(OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let verdict: String = copy.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
    if verdict == "ok" {
        Ok(())
    } else {
        Err(Error::Corrupt("the copy failed SQLite's quick check"))
    }
}

/// Holds back scrobbling per account in `copy`, a backup's copy of a library
/// taken at `taken_at_ms`, when the library has any service account: a server
/// that opens it sends nothing for an account until an import has brought
/// back what the service took after the backup (covering pending deliveries,
/// unplanned listens, staged listens, and satellite outboxes). The library
/// copied is not changed. The copy is synced again, as [`copy_database`]
/// leaves it. Returns whether any hold was written.
pub fn hold_sending(copy: &Path, taken_at_ms: i64) -> Result<bool, Error> {
    let connection = Location::File(copy.to_owned()).open(OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    let held = crate::delivery_store::hold_sending(&connection, taken_at_ms)?;
    drop(connection);
    std::fs::OpenOptions::new()
        .write(true)
        .open(copy)
        .and_then(|file| file.sync_all())
        .map_err(Error::Io)?;
    Ok(held)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::testing::FixedClock;
    use crate::{OsEntropy, ServerStore};

    struct Scratch(std::path::PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let path = tempfile::Builder::new()
                .prefix(&format!("ritornel-engine-backup-{name}-"))
                .tempdir()
                .unwrap()
                .keep();
            Self(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const NOW_MS: i64 = 1_763_000_000_000;

    fn open(path: &Path) -> ServerStore {
        ServerStore::open(path, Arc::new(FixedClock::at(NOW_MS)), Arc::new(OsEntropy)).unwrap()
    }

    #[test]
    fn migration_state_tracks_commits_in_the_wal_and_ignores_rolled_back_updates() {
        let scratch = Scratch::new("migration-state");
        let path = scratch.0.join("server.db");
        drop(open(&path));
        let writer = rusqlite::Connection::open(&path).unwrap();
        writer
            .execute_batch(
                "PRAGMA journal_mode=DELETE;
             INSERT INTO outbox(kind, dedupe_key, next_attempt_at_ms, created_at_ms)
             VALUES ('history.ingest', x'01', 0, 0), ('history.ingest', x'02', 0, 0)",
            )
            .unwrap();
        let state = || MigrationState::read(&path, &scratch.0.join("library.db")).unwrap();
        let before = state();
        writer.execute_batch("PRAGMA journal_mode=WAL").unwrap();
        assert_eq!(state(), before);
        writer
            .execute_batch("BEGIN; UPDATE outbox SET attempts = attempts + 1 WHERE id = 1")
            .unwrap();
        assert_eq!(state(), before);
        writer.execute_batch("ROLLBACK").unwrap();
        assert_eq!(state(), before);
        writer
            .execute("UPDATE outbox SET attempts = attempts + 1 WHERE id = 1", [])
            .unwrap();
        assert_ne!(state(), before);
        writer
            .execute_batch(
                r#"INSERT INTO command_receipt
            (command_id, request_fingerprint, method, result_json, created_at_ms)
            VALUES (zeroblob(16), zeroblob(32), 'playback.move', '{"status":"accepted"}', 0)"#,
            )
            .unwrap();
        let accepted = state();
        writer
            .execute(
                r#"UPDATE command_receipt SET result_json = '{"status":"done"}'"#,
                [],
            )
            .unwrap();
        assert!(
            !accepted
                .unchanged(&path, &scratch.0.join("library.db"))
                .unwrap()
        );
    }

    #[test]
    fn the_receipt_fingerprint_tracks_new_commands_as_older_receipts_expire() {
        let scratch = Scratch::new("receipt-state");
        let server = scratch.0.join("server.db");
        let library = scratch.0.join("library.db");
        let store = open(&server);
        let db = rusqlite::Connection::open(&server).unwrap();
        let record = |key: u8, at_ms: i64| {
            db.execute(
                "INSERT INTO command_receipt
                 (command_id, request_fingerprint, method, result_json, created_at_ms)
                 VALUES (?1, zeroblob(32), 'queue.clear', 'null', ?2)",
                ([key; 16], at_ms),
            )
            .unwrap();
        };
        record(1, 0);
        record(2, NOW_MS);
        let snapshot = MigrationState::read(&server, &library).unwrap();
        store.prune_receipts().unwrap();
        let remaining: i64 = db
            .query_row("SELECT count(*) FROM command_receipt", [], |r| r.get(0))
            .unwrap();
        assert_eq!(remaining, 1);
        assert!(snapshot.unchanged(&server, &library).unwrap());
        record(3, NOW_MS);
        assert!(!snapshot.unchanged(&server, &library).unwrap());
    }

    #[test]
    fn a_live_database_is_copied_whole_while_it_is_written() {
        let scratch = Scratch::new("live");
        let path = scratch.0.join("server.db");
        let store = open(&path);
        store.ensure_identity("desk").unwrap();
        store.set_watching(false).unwrap();

        // The store is open, its writer holds the file and its log, and
        // what was committed is in the log rather than the file.
        let copy = scratch.0.join("copy.db");
        copy_database(&path, &copy).unwrap();
        // One file, with nothing of a log beside it.
        assert!(!scratch.0.join("copy.db-wal").exists());
        let reopened = open(&copy);
        assert!(!reopened.watching().unwrap());
        assert_eq!(
            reopened.ensure_identity("other").unwrap().0,
            store.ensure_identity("desk").unwrap().0
        );

        // A copy is never written over anything.
        assert!(matches!(
            copy_database(&path, &copy),
            Err(Error::Invalid(_))
        ));
    }

    // A link to itself is a file whose details cannot be read, as one on a
    // failing disk or in a folder closed to the server is; the link is
    // made differently on each system, and on Windows only with a
    // privilege a test does not have.
    #[cfg(unix)]
    #[test]
    fn a_database_whose_details_cannot_be_read_is_an_error_and_not_nothing_to_migrate() {
        let scratch = Scratch::new("unreadable");
        let path = scratch.0.join("library.db");
        std::os::unix::fs::symlink(&path, &path).unwrap();
        assert!(pending_migrations(&path, Database::Library).is_err());
    }

    #[test]
    fn a_library_put_back_from_a_backup_scrobbles_nothing_until_an_import_has_run() {
        use crate::{Held, LibraryStore, WriteRequest};
        let scratch = Scratch::new("held");
        let clock = Arc::new(FixedClock::at(1_763_000_000_000));
        let server = open(&scratch.0.join("server.db"));
        let (identity, _) = server.ensure_identity("desk").unwrap();
        let path = scratch.0.join("library.db");
        let (library, _) = LibraryStore::create(
            &path,
            &identity,
            WriteRequest {
                method: "library.create",
                command: None,
                client: None,
            },
            |_| String::new(),
            clock.clone(),
            Arc::new(OsEntropy),
        )
        .unwrap();
        // With no service accounts, a copy is not held.
        let idle = scratch.0.join("idle.db");
        copy_database(&path, &idle).unwrap();
        assert!(!hold_sending(&idle, 1_762_000_000_000).unwrap());

        // With two service accounts and zero pending deliveries (e.g. only
        // unplanned, staged, or satellite outbox plays may exist), the copy
        // holds each account independently.
        let first = library.ensure_service_account("lastfm", "someone").unwrap();
        let second = library.ensure_service_account("lastfm", "another").unwrap();
        let copy = scratch.0.join("copy.db");
        copy_database(&path, &copy).unwrap();
        assert!(hold_sending(&copy, 1_762_000_000_000).unwrap());
        // The library copied is not held.
        assert_eq!(library.held_for_import(first).unwrap(), None);
        assert_eq!(library.held_for_import(second).unwrap(), None);
        assert_eq!(library.any_held_for_import().unwrap(), None);
        drop(library);

        // Put back and opened, each account is held from then on, and opened
        // again it keeps that time.
        let restored = LibraryStore::open(
            &copy,
            identity.server_id,
            clock.clone(),
            Arc::new(OsEntropy),
        )
        .unwrap();
        // The backup copy also keeps the legacy global key for downgrade/revert
        // safety; opening on this binary expands and removes the global key.
        let has_legacy = || {
            restored
                .read(|connection| {
                    Ok(connection.query_row(
                        "SELECT EXISTS (SELECT 1 FROM library_setting WHERE key = ?1)",
                        [crate::delivery_store::HELD_SETTING],
                        |row| row.get::<_, bool>(0),
                    )?)
                })
                .unwrap()
        };
        assert!(has_legacy());
        let held = Some(Held {
            taken_at_ms: 1_762_000_000_000,
            since_ms: Some(1_763_000_000_000),
        });
        assert_eq!(restored.opened_restored(Some(first)).unwrap(), held);
        assert!(!has_legacy());
        clock.set(1_763_000_500_000);
        assert_eq!(restored.opened_restored(Some(first)).unwrap(), held);
        assert_eq!(restored.held_for_import(first).unwrap(), held);
        assert_eq!(restored.held_for_import(second).unwrap(), held);
        // An import for the first account lifts only its hold, leaving the
        // second account held until it syncs too. Once the active account is
        // released (or when signed out after first open), a dormant orphan
        // hold stays persisted without re-reporting a restore on later starts.
        assert!(restored.release_hold(first).unwrap());
        assert!(!restored.release_hold(first).unwrap());
        assert_eq!(restored.held_for_import(first).unwrap(), None);
        assert_eq!(restored.held_for_import(second).unwrap(), held);
        assert_eq!(restored.opened_restored(Some(first)).unwrap(), None);
        assert_eq!(restored.opened_restored(None).unwrap(), None);
        assert_eq!(restored.opened_restored(Some(second)).unwrap(), held);
        assert!(restored.release_hold(second).unwrap());
        assert!(!restored.release_hold(second).unwrap());
        assert_eq!(restored.held_for_import(second).unwrap(), None);
        assert_eq!(restored.opened_restored(Some(second)).unwrap(), None);
    }

    #[test]
    fn the_ids_a_database_records_are_read_without_changing_it() {
        let scratch = Scratch::new("recorded");
        let server = scratch.0.join("server.db");
        let library = scratch.0.join("library.db");
        // Missing, then empty: none.
        assert_eq!(recorded_id(&server, Database::Server).unwrap(), None);
        std::fs::write(&server, b"").unwrap();
        assert_eq!(recorded_id(&server, Database::Server).unwrap(), None);
        std::fs::remove_file(&server).unwrap();

        // A database with none of the table: none.
        rusqlite::Connection::open(&library)
            .unwrap()
            .execute_batch("CREATE TABLE other (id INTEGER)")
            .unwrap();
        assert_eq!(recorded_id(&library, Database::Library).unwrap(), None);
        std::fs::remove_file(&library).unwrap();

        // The table, but no row yet: none.
        let mut db = rusqlite::Connection::open(&server).unwrap();
        migrate::run(
            &mut db,
            &migrate::SERVER[..1],
            &FixedClock::at(1_763_000_000_000),
        )
        .unwrap();
        drop(db);
        assert_eq!(recorded_id(&server, Database::Server).unwrap(), None);
        std::fs::remove_file(&server).unwrap();

        // A server that knows itself.
        let store = open(&server);
        let (identity, _) = store.ensure_identity("desk").unwrap();
        drop(store);
        assert_eq!(
            recorded_id(&server, Database::Server).unwrap(),
            Some(identity.server_id)
        );

        // A library at an older schema, which opening would migrate, and
        // which reading leaves as it was.
        let mut db = rusqlite::Connection::open(&library).unwrap();
        migrate::run(
            &mut db,
            &migrate::LIBRARY[..2],
            &FixedClock::at(1_763_000_000_000),
        )
        .unwrap();
        db.execute_batch(include_str!("../saved/library/0002.sql"))
            .unwrap();
        drop(db);
        let before = std::fs::read(&library).unwrap();
        assert_eq!(
            recorded_id(&library, Database::Library).unwrap(),
            Some(Id::from_bytes([0x02; 16]))
        );
        assert!(
            pending_migrations(&library, Database::Library)
                .unwrap()
                .is_some()
        );
        assert_eq!(std::fs::read(&library).unwrap(), before);
    }

    #[test]
    fn the_id_of_a_database_in_use_is_its_last_commit_and_reading_it_changes_nothing() {
        let scratch = Scratch::new("recorded-wal");
        let path = scratch.0.join("server.db");
        let mut writer = rusqlite::Connection::open(&path).unwrap();
        writer.pragma_update(None, "journal_mode", "WAL").unwrap();
        // What is committed stays in the log, not the file.
        writer.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
        migrate::run(
            &mut writer,
            &migrate::SERVER[..1],
            &FixedClock::at(1_763_000_000_000),
        )
        .unwrap();
        writer
            .execute(
                "INSERT INTO server_identity
                   (id, server_id, signing_key, public_key, name, created_at_ms)
                 VALUES (1, ?1, zeroblob(32), zeroblob(32), 'desk', 0)",
                [vec![0x0a_u8; 16]],
            )
            .unwrap();
        let log = scratch.0.join("server.db-wal");
        assert!(std::fs::metadata(&log).unwrap().len() > 0);
        // And a change not yet committed.
        writer
            .execute_batch(
                "BEGIN; UPDATE server_identity SET server_id = zeroblob(16) WHERE id = 1",
            )
            .unwrap();
        let before = (std::fs::read(&path).unwrap(), std::fs::read(&log).unwrap());

        assert_eq!(
            recorded_id(&path, Database::Server).unwrap(),
            Some(Id::from_bytes([0x0a; 16]))
        );
        assert_eq!(
            (std::fs::read(&path).unwrap(), std::fs::read(&log).unwrap()),
            before
        );
        writer.execute_batch("ROLLBACK").unwrap();
    }

    #[test]
    fn a_file_that_is_not_a_database_has_no_id_to_read() {
        let scratch = Scratch::new("recorded-corrupt");
        let path = scratch.0.join("library.db");
        std::fs::write(&path, b"not a database at all ".repeat(200)).unwrap();
        assert!(recorded_id(&path, Database::Library).is_err());
    }

    #[test]
    fn only_a_database_with_data_and_an_older_schema_is_pending() {
        let scratch = Scratch::new("pending");
        let path = scratch.0.join("server.db");
        // Missing, then empty: nothing to lose.
        assert_eq!(pending_migrations(&path, Database::Server).unwrap(), None);
        std::fs::write(&path, b"").unwrap();
        assert_eq!(pending_migrations(&path, Database::Server).unwrap(), None);

        // Up to date.
        drop(open(&path));
        assert_eq!(pending_migrations(&path, Database::Server).unwrap(), None);

        // A program that knows one migration more would take it forward.
        let connection = Location::File(path.clone())
            .open(OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
        let mut newer = migrate::SERVER.to_vec();
        let current = crate::SERVER_SCHEMA_VERSION;
        newer.push(Migration {
            version: current + 1,
            name: "next",
            sql: "",
        });
        assert_eq!(
            pending_in(&connection, &newer).unwrap(),
            Some(Pending {
                from: current,
                to: current + 1
            })
        );
        // And one that knows fewer would refuse to open it, so there is
        // nothing to copy for.
        assert_eq!(pending_in(&connection, &[]).unwrap(), None);
    }
}
