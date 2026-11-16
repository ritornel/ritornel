//! The engine's one error type.

use std::fmt;

/// Why the engine refused or failed.
///
/// `Display` is one line that is safe to print and to log: it never carries
/// a key or a token.
#[derive(Debug)]
pub enum Error {
    /// SQLite refused something.
    Sqlite(rusqlite::Error),
    /// The SQLite this program was built with is not the one it was written
    /// for. See decision 16 in docs/design/server.md.
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
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Sqlite(error) | Self::MigrationFailed { source: error, .. } => Some(error),
            Self::WriterSpawn(error) => Some(error),
            Self::Entropy(error) => Some(error),
            _ => None,
        }
    }
}

impl From<rusqlite::Error> for Error {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}
