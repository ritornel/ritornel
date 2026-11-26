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
    // One whose details cannot be read is an error, never taken for one
    // that is not there.
    match std::fs::metadata(path) {
        Ok(file) if file.len() == 0 => return Ok(None),
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(Error::Io(error)),
    }
    let connection = Location::File(path.to_owned()).open(OpenFlags::SQLITE_OPEN_READ_ONLY)?;
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
    match std::fs::metadata(path) {
        Ok(file) if file.len() == 0 => return Ok(None),
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(Error::Io(error)),
    }
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
    let connection = Location::File(path.to_owned()).open(OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let made: bool = connection.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        [table],
        |row| row.get(0),
    )?;
    if !made {
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

    fn open(path: &Path) -> ServerStore {
        ServerStore::open(
            path,
            Arc::new(FixedClock::at(1_763_000_000_000)),
            Arc::new(OsEntropy),
        )
        .unwrap()
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
