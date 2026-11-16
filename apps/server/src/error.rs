//! The server program's one error type, and its exit codes.

use std::fmt;
use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;

use crate::config::ConfigError;

/// The exit code of a server that failed for a reason with no code of its
/// own.
pub const EXIT_FAILED: u8 = 1;

/// The exit code when the command line was wrong.
pub const EXIT_USAGE: u8 = 2;

/// The exit code reserved for one case: another server holds the data
/// directory. A launcher that lost the race to start a server sees it and
/// goes back to looking for the one that won.
pub const EXIT_IN_USE: u8 = 3;

/// Why the server did not start, or stopped.
///
/// `Display` is one line that is safe to print and to log.
#[derive(Debug)]
pub enum Error {
    /// The command line was wrong. The text is fixed, so it repeats nothing
    /// it was given.
    Usage(&'static str),
    /// No data directory was named and the platform's default could not be
    /// worked out.
    NoDataDir,
    /// Another server holds the data directory's lock.
    InUse {
        /// The data directory.
        dir: PathBuf,
        /// The process that holds it, if it has said.
        pid: Option<u32>,
        /// Where that server listens, if it has said.
        endpoint: Option<SocketAddr>,
    },
    /// A setting has a value the server cannot use.
    Config(ConfigError),
    /// A file or directory in the data directory could not be used.
    Io {
        /// What was being done to it, as a verb: "create", "read".
        doing: &'static str,
        /// The file or directory.
        path: PathBuf,
        /// What the system said.
        source: io::Error,
    },
    /// `server.db` could not be opened or used.
    Store(ritornel_engine::Error),
    /// The listener could not be bound.
    Listen {
        /// The address it was to listen on.
        address: SocketAddr,
        /// What the system said.
        source: io::Error,
    },
    /// The server has stopped, or is stopping, and `server.db` is closed.
    Stopped,
    /// The async runtime or one of the threads that watch for a stop could
    /// not be started.
    Runtime(io::Error),
    /// The ready line could not be written to standard output.
    Output(io::Error),
    /// The process already has a logger, so this run cannot install its
    /// own.
    LoggerSet,
}

impl Error {
    /// The exit code a process ends with after this error.
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Usage(_) => EXIT_USAGE,
            Self::InUse { .. } => EXIT_IN_USE,
            _ => EXIT_FAILED,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage(message) => f.write_str(message),
            Self::NoDataDir => write!(
                f,
                "no data directory: give --data-dir or set {}",
                ritornel_contract::local::DATA_DIR_VAR
            ),
            // docs/design/server.md, "Second start".
            Self::InUse {
                dir,
                pid: Some(pid),
                endpoint: Some(endpoint),
            } => write!(
                f,
                "data directory in use by pid {pid} at {endpoint}: {}",
                dir.display()
            ),
            Self::InUse {
                dir,
                pid: Some(pid),
                endpoint: None,
            } => write!(f, "data directory in use by pid {pid}: {}", dir.display()),
            Self::InUse { dir, pid: None, .. } => write!(
                f,
                "data directory in use by another server: {}",
                dir.display()
            ),
            Self::Config(error) => write!(f, "{error}"),
            Self::Io {
                doing,
                path,
                source,
            } => write!(f, "cannot {doing} {}: {source}", path.display()),
            Self::Store(error) => write!(f, "server.db: {error}"),
            Self::Listen { address, source } => write!(f, "cannot listen on {address}: {source}"),
            Self::Stopped => f.write_str("the server has stopped"),
            Self::Runtime(error) => write!(f, "cannot start: {error}"),
            Self::Output(error) => write!(f, "cannot write to standard output: {error}"),
            Self::LoggerSet => f.write_str("this process already has a logger"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Config(error) => Some(error),
            Self::Io { source, .. }
            | Self::Listen { source, .. }
            | Self::Runtime(source)
            | Self::Output(source) => Some(source),
            Self::Store(error) => Some(error),
            _ => None,
        }
    }
}

impl From<ritornel_engine::Error> for Error {
    fn from(error: ritornel_engine::Error) -> Self {
        Self::Store(error)
    }
}

impl From<ConfigError> for Error {
    fn from(error: ConfigError) -> Self {
        Self::Config(error)
    }
}

/// Makes the conversion from an I/O error that names what was being done to
/// which path: `.map_err(io_error("read", &path))`.
pub(crate) fn io_error(
    doing: &'static str,
    path: &std::path::Path,
) -> impl FnOnce(io::Error) -> Error {
    let path = path.to_owned();
    move |source| Error::Io {
        doing,
        path,
        source,
    }
}
