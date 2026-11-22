//! Copies of a database: the backup taken on demand, and the one taken
//! before a migration. See "The data directory" in docs/design/server.md.
//!
//! A copy is made with `VACUUM INTO` on a read-only connection of its own.
//! It is a consistent snapshot of the database as it was when the copy
//! began, it is made beside the writer and the readers without holding
//! either, and it is one file with no write-ahead log beside it, so it can
//! be carried anywhere and opened as it is.

use std::path::Path;

use rusqlite::OpenFlags;

use crate::Error;
use crate::migrate::{self, Migration};
use crate::sqlite::Location;

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

/// Holds back scrobbling in `copy`, a backup's copy of a library taken at
/// `taken_at_ms`, for as long as it is not put back: a server that opens
/// it sends nothing until an import has brought back what the service took
/// after the backup. The library copied is not changed. The copy is synced
/// and checked again, as [`copy_database`] leaves it.
pub fn hold_sending(copy: &Path, taken_at_ms: i64) -> Result<(), Error> {
    let connection = Location::File(copy.to_owned()).open(OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    crate::delivery_store::hold_sending(&connection, taken_at_ms)?;
    let verdict: String = connection.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
    drop(connection);
    std::fs::OpenOptions::new()
        .write(true)
        .open(copy)
        .and_then(|file| file.sync_all())
        .map_err(Error::Io)?;
    if verdict == "ok" {
        Ok(())
    } else {
        Err(Error::Corrupt("the copy failed SQLite's quick check"))
    }
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
            let path = std::env::temp_dir().join(format!(
                "ritornel-engine-backup-{name}-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
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
        let copy = scratch.0.join("copy.db");
        copy_database(&path, &copy).unwrap();
        hold_sending(&copy, 1_762_000_000_000).unwrap();
        // The library copied is not held.
        assert_eq!(library.held_for_import().unwrap(), None);
        drop(library);

        // Put back and opened, it is held from then on, and opened again
        // it keeps that time.
        let restored = LibraryStore::open(
            &copy,
            identity.server_id,
            clock.clone(),
            Arc::new(OsEntropy),
        )
        .unwrap();
        let held = Some(Held {
            taken_at_ms: 1_762_000_000_000,
            since_ms: Some(1_763_000_000_000),
        });
        assert_eq!(restored.opened_restored().unwrap(), held);
        clock.set(1_763_000_500_000);
        assert_eq!(restored.opened_restored().unwrap(), held);
        assert_eq!(restored.held_for_import().unwrap(), held);
        // An import lifts it, once.
        let account = restored
            .ensure_service_account("lastfm", "someone")
            .unwrap();
        assert!(restored.release_hold(account).unwrap());
        assert!(!restored.release_hold(account).unwrap());
        assert_eq!(restored.opened_restored().unwrap(), None);
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
        newer.push(Migration {
            version: 2,
            name: "0002_next",
            sql: "",
        });
        assert_eq!(
            pending_in(&connection, &newer).unwrap(),
            Some(Pending { from: 1, to: 2 })
        );
        // And one that knows fewer would refuse to open it, so there is
        // nothing to copy for.
        assert_eq!(pending_in(&connection, &[]).unwrap(), None);
    }
}
