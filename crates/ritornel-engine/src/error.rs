//! The engine's one error type.

use std::fmt;

use crate::Id;

/// Why the engine refused or failed.
///
/// `Display` is one line that is safe to print and to log: it never carries
/// a key or a token.
#[derive(Debug)]
pub enum Error {
    /// SQLite refused something.
    Sqlite(rusqlite::Error),
    /// The SQLite this program was built with is not the one it was written
    /// for. See "SQLite" in docs/internals/server.md.
    SqliteVersion {
        /// The version found.
        found: &'static str,
    },
    /// A database file would not go into WAL mode.
    NotWal {
        /// The journal mode it stayed in.
        found: String,
    },
    /// The file has tables but no migration ledger, so something else made
    /// it.
    ForeignDatabase,
    /// The database was written by a newer version of this program.
    NewerSchema {
        /// The newest migration the database has.
        found: i64,
        /// The newest migration this program knows.
        known: i64,
    },
    /// A migration this program ships is not the one the database was built
    /// with: its checksum differs from the one recorded when it was applied.
    ChangedMigration {
        /// Which migration.
        version: i64,
    },
    /// The migration ledger skips a number, which this program never does.
    MigrationGap {
        /// The first number that is missing.
        missing: i64,
    },
    /// A migration failed. The database is as it was before that migration.
    MigrationFailed {
        /// Which migration.
        version: i64,
        /// What SQLite said.
        source: rusqlite::Error,
    },
    /// The WAL could not be truncated because a connection was still
    /// reading.
    CheckpointBusy,
    /// The writer thread has stopped, so nothing more can be written.
    WriterStopped,
    /// The writer thread could not be started.
    WriterSpawn(std::io::Error),
    /// The operating system gave no random bytes.
    Entropy(getrandom::Error),
    /// There is no library where one was looked for: the file is missing,
    /// or holds none.
    NoLibrary,
    /// A library was to be created where there is one already.
    LibraryExists,
    /// This server may not write the library: its hub pointer is empty or
    /// names another server. See "Storage split" in docs/internals/server.md.
    NotTheHub {
        /// The library.
        library_id: Id,
        /// The server the pointer names. `None` is a released library.
        hub: Option<Id>,
        /// This server.
        server_id: Id,
    },
    /// A command ID was used again with a request that is not the one it
    /// was first used with.
    CommandMismatch {
        /// The command ID.
        command: Id,
    },
    /// A root of this name is already at another path on this server.
    RootTaken {
        /// The root's name.
        name: String,
    },
    /// The library already has a source of this name on this server.
    SourceExists {
        /// The source there is.
        source: Id,
    },
    /// The library has no such source.
    NoSuchSource,
    /// This server has no path for the root a source names.
    NoRootPath {
        /// The root's name.
        name: String,
    },
    /// A root's folder is missing, is not a folder or is empty: the root is
    /// offline, not empty, and nothing in it is marked missing. See "Data
    /// directory" in docs/internals/server.md.
    RootOffline {
        /// Where the root should be.
        path: std::path::PathBuf,
    },
    /// Reading a root failed.
    Io(std::io::Error),
    /// Something stored as JSON could not be written or read back.
    Json(serde_json::Error),
    /// A row is not what this program wrote.
    Corrupt(&'static str),
    /// A job was stopped where a test asked it to be, as a crash would stop
    /// it.
    FailPoint,
    /// The library changed between a resolution's plan and its write, so
    /// the plan was not written. It is made again.
    PlanStale,
    /// A request names something that is not there.
    NotFound(&'static str),
    /// A request cannot be carried out as it is.
    Invalid(&'static str),
    /// The output failed.
    Playback(String),
    /// A queue edit was decided on a revision of the queue it has moved
    /// past, so it changed nothing.
    StaleQueue {
        /// The queue's revision now.
        current: i64,
    },
    /// The hub cannot take a listen: what it names does not hold together.
    BadListen(&'static str),
    /// A write that only worked out what it would do, and was rolled back
    /// on purpose: the derived history before and after. It never reaches
    /// a caller.
    RolledBack(Vec<String>, Vec<String>),
    /// A write that found nothing to change, and was rolled back so that
    /// it takes no revision. It never reaches a caller.
    Unchanged,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sqlite(error) => write!(f, "database: {error}"),
            Self::SqliteVersion { found } => write!(
                f,
                "built with SQLite {found}, but this program needs exactly {}",
                crate::SQLITE_VERSION
            ),
            Self::NotWal { found } => write!(
                f,
                "the database would not use a write-ahead log (journal mode {found}); is the \
                 data directory on a local disk?"
            ),
            Self::ForeignDatabase => {
                f.write_str("the database was not made by this program, and is left as it was")
            }
            Self::NewerSchema { found, known } => write!(
                f,
                "the database is at schema version {found}, and this program knows only up to \
                 {known}; run the newer version that wrote it"
            ),
            Self::ChangedMigration { version } => write!(
                f,
                "migration {version} is not the one this database was built with (its checksum \
                 differs)"
            ),
            Self::MigrationGap { missing } => write!(
                f,
                "the database's migration ledger has no migration {missing}"
            ),
            Self::MigrationFailed { version, source } => {
                write!(f, "migration {version} failed: {source}")
            }
            Self::CheckpointBusy => {
                f.write_str("the write-ahead log could not be truncated: a reader was still open")
            }
            Self::WriterStopped => f.write_str("the database writer has stopped"),
            Self::WriterSpawn(error) => write!(f, "cannot start the database writer: {error}"),
            Self::Entropy(error) => write!(f, "no random numbers from the system: {error}"),
            Self::NoLibrary => f.write_str("there is no library here"),
            Self::LibraryExists => f.write_str("there is a library here already"),
            Self::NotTheHub {
                library_id,
                hub: Some(hub),
                server_id,
            } => write!(
                f,
                "library {library_id} belongs to server {hub}, and this server is {server_id}: \
                 only the server its hub pointer names may write it"
            ),
            Self::NotTheHub {
                library_id,
                hub: None,
                ..
            } => write!(
                f,
                "library {library_id} has been released: no server may write it until one \
                 adopts it"
            ),
            Self::CommandMismatch { command } => {
                write!(f, "command {command} was sent before with other params")
            }
            Self::RootTaken { name } => {
                write!(f, "this server has a root named {name} at another path")
            }
            Self::SourceExists { source } => {
                write!(f, "the library has a source of that name already: {source}")
            }
            Self::NoSuchSource => f.write_str("the library has no such source"),
            Self::NoRootPath { name } => {
                write!(f, "this server has no path for the root {name}")
            }
            Self::RootOffline { path } => write!(
                f,
                "the root at {} is missing or holds no audio: it is taken to be offline, and nothing was changed",
                path.display()
            ),
            Self::Io(error) => write!(f, "reading a root: {error}"),
            Self::Json(error) => write!(f, "stored JSON: {error}"),
            Self::Corrupt(what) => {
                write!(f, "the database holds {what} this program did not write")
            }
            Self::FailPoint => f.write_str("stopped at a fail point"),
            Self::PlanStale => f.write_str("the library changed while a resolution was planned"),
            Self::NotFound(what) | Self::Invalid(what) => f.write_str(what),
            Self::Playback(what) => write!(f, "the output: {what}"),
            Self::StaleQueue { current } => write!(
                f,
                "the queue has changed since that edit was decided: it is at revision {current}"
            ),
            Self::BadListen(what) => write!(f, "a listen the hub cannot take: {what}"),
            Self::RolledBack(..) => f.write_str("a dry run, rolled back"),
            Self::Unchanged => f.write_str("nothing changed, so nothing was written"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Sqlite(error) | Self::MigrationFailed { source: error, .. } => Some(error),
            Self::WriterSpawn(error) | Self::Io(error) => Some(error),
            Self::Entropy(error) => Some(error),
            Self::Json(error) => Some(error),
            _ => None,
        }
    }
}

impl From<serde_json::Error> for Error {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

impl From<rusqlite::Error> for Error {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}
