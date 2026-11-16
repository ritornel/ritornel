//! The client's one error type.

use std::fmt;
use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;

use ritornel_contract::{CONTRACT_VERSION, RpcError, local};

/// Why the client could not find a server, reach it, or get an answer.
///
/// `Display` is one line that is safe to print: it never carries a
/// credential.
#[derive(Debug)]
pub enum Error {
    /// No data directory was named and the platform's default could not be
    /// worked out.
    NoDataDir,
    /// No server is running on the data directory.
    NotRunning {
        /// The data directory.
        data_dir: PathBuf,
        /// How the client knows.
        why: NotRunning,
    },
    /// `server.json` is not what a server writes.
    ServerJson {
        /// The file.
        path: PathBuf,
    },
    /// The server speaks another version of the contract than this program
    /// was built with.
    ContractVersion {
        /// The server's version.
        server: u32,
    },
    /// A file in the data directory could not be read.
    File {
        /// What was being done to it, as a verb: "read".
        doing: &'static str,
        /// The file.
        path: PathBuf,
        /// What the system said.
        source: io::Error,
    },
    /// The server refused the connection before the contract began: 401
    /// for a credential it does not accept, 403 for an origin it does not
    /// know.
    Refused {
        /// The HTTP status it answered with.
        status: u16,
    },
    /// The connection could not be made, or was lost. This is the client's
    /// own error: a server never sends it. A call that was waiting may or
    /// may not have been carried out.
    Transport(String),
    /// What answered is not speaking the contract.
    Protocol(String),
    /// The server answered a call with an error.
    Call(RpcError),
    /// The system gave no random numbers to make an idempotency key from.
    NoRandom(String),
}

/// How a client knows that there is no server to talk to on a data
/// directory. A server that is alive holds the directory's lock, so the
/// lock is asked first, and nothing is sent anywhere unless it is held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotRunning {
    /// Nothing holds the lock and there is no `server.json`: no server has
    /// started, or the last one stopped cleanly.
    NoServerJson,
    /// Nothing holds the lock, and there is a `server.json`: the server
    /// that wrote it was killed. Whatever is at the address it names now
    /// is not that server, and is not asked.
    Killed,
    /// A server holds the lock and `server.json` is not there, or is not
    /// its own: it has just started and has not said where it listens. A
    /// moment later it will have.
    Starting,
    /// The server that holds the lock does not answer where it says it
    /// listens. It is stopping, or stuck.
    NoAnswer(SocketAddr),
    /// What answers where the server says it listens is not that server.
    AnotherServer(SocketAddr),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoDataDir => write!(
                f,
                "no data directory: give --data-dir or set {}",
                local::DATA_DIR_VAR
            ),
            Self::NotRunning { data_dir, why } => {
                write!(f, "no server is running on {}", data_dir.display())?;
                match why {
                    NotRunning::NoServerJson => Ok(()),
                    NotRunning::Killed => write!(
                        f,
                        " ({} is left by one that was killed)",
                        local::SERVER_JSON
                    ),
                    NotRunning::Starting => {
                        f.write_str(" (one has just started and has not said where it listens)")
                    }
                    NotRunning::NoAnswer(endpoint) => {
                        write!(f, " (the one that holds it does not answer at {endpoint})")
                    }
                    NotRunning::AnotherServer(endpoint) => write!(
                        f,
                        " (what answers at {endpoint} is not the one that holds it)"
                    ),
                }
            }
            Self::ServerJson { path } => {
                write!(f, "{} is not what a server writes", path.display())
            }
            Self::ContractVersion { server } => write!(
                f,
                "the server speaks contract version {server} and this program version \
                 {CONTRACT_VERSION}: use the two from one release"
            ),
            Self::File {
                doing,
                path,
                source,
            } => write!(f, "cannot {doing} {}: {source}", path.display()),
            Self::Refused { status: 401 } => {
                f.write_str("the server did not accept the credential")
            }
            Self::Refused { status } => {
                write!(f, "the server refused the connection (HTTP {status})")
            }
            Self::Transport(why) => write!(f, "no connection to the server: {why}"),
            Self::Protocol(why) => write!(f, "the server did not speak the contract: {why}"),
            Self::Call(error) => write!(f, "the server answered with an error: {error}"),
            Self::NoRandom(why) => write!(f, "no random numbers from the system: {why}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::File { source, .. } => Some(source),
            Self::Call(error) => Some(error),
            _ => None,
        }
    }
}

/// Makes a transport error out of whatever a library said.
pub(crate) fn transport(why: impl fmt::Display) -> Error {
    Error::Transport(why.to_string())
}

#[cfg(test)]
mod tests {
    use ritornel_contract::ErrorCode;

    use super::*;

    #[test]
    fn having_no_server_reads_plainly_whatever_the_reason() {
        let endpoint = SocketAddr::from(([127, 0, 0, 1], 4747));
        let message = |why| {
            Error::NotRunning {
                data_dir: "/srv/ritornel".into(),
                why,
            }
            .to_string()
        };
        assert_eq!(
            message(NotRunning::NoServerJson),
            "no server is running on /srv/ritornel"
        );
        assert_eq!(
            message(NotRunning::Killed),
            "no server is running on /srv/ritornel (server.json is left by one that was killed)"
        );
        assert_eq!(
            message(NotRunning::Starting),
            "no server is running on /srv/ritornel (one has just started and has not said where \
             it listens)"
        );
        assert_eq!(
            message(NotRunning::NoAnswer(endpoint)),
            "no server is running on /srv/ritornel (the one that holds it does not answer at \
             127.0.0.1:4747)"
        );
        assert_eq!(
            message(NotRunning::AnotherServer(endpoint)),
            "no server is running on /srv/ritornel (what answers at 127.0.0.1:4747 is not the \
             one that holds it)"
        );
    }

    #[test]
    fn the_other_errors_say_what_to_do_or_what_was_said() {
        assert_eq!(
            Error::NoDataDir.to_string(),
            "no data directory: give --data-dir or set RITORNEL_DATA_DIR"
        );
        assert_eq!(
            Error::ContractVersion { server: 7 }.to_string(),
            format!(
                "the server speaks contract version 7 and this program version \
                 {CONTRACT_VERSION}: use the two from one release"
            )
        );
        assert_eq!(
            Error::Refused { status: 401 }.to_string(),
            "the server did not accept the credential"
        );
        assert_eq!(
            Error::Refused { status: 403 }.to_string(),
            "the server refused the connection (HTTP 403)"
        );
        assert_eq!(
            Error::Call(RpcError::new(ErrorCode::Forbidden, "needs a full session")).to_string(),
            "the server answered with an error: forbidden: needs a full session"
        );
    }
}
