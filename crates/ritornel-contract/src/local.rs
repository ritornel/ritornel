//! What a server and the clients on its machine agree on outside the
//! socket: where the data directory is, the lock that says a server is
//! alive on it, the shape of `server.json`, and the name of the credential
//! file. See "Process lifecycle" in docs/internals/server.md.
//!
//! It is here because the server writes these and a local client reads
//! them, and this is the one crate both may depend on. Nothing here touches
//! a file: the server does the writing and `ritornel-client` the reading.

use std::ffi::OsString;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::Id;

/// The environment variable that names the data directory.
pub const DATA_DIR_VAR: &str = "RITORNEL_DATA_DIR";

/// The file in the data directory that a running server holds an exclusive
/// advisory lock on for the whole of its life, with its pid in it as text
/// on one line.
///
/// The system drops the lock when the process ends, however it ends, so the
/// lock being held is what says a server is alive. `server.json` cannot say
/// it: a server that was killed leaves that file behind. A client therefore
/// tries the lock before it believes `server.json`, and believes it only if
/// both name the same pid ([`lock_holder`]). Only the user who owns the
/// data directory can open the lock file, so nobody else can hold it.
///
/// On Unix the lock is `flock` on the whole file, taken exclusively by the
/// server and shared, for an instant, by a client that asks. Windows
/// enforces a lock: bytes held exclusively cannot be read through another
/// handle. So a server there holds [`WINDOWS_LOCK_BYTE`] alone, past the
/// end of the file, and its pid stays readable. A client asks as on Unix,
/// with a shared lock on the whole file, which covers that byte.
pub const SERVER_LOCK: &str = "server.lock";

/// The one byte of [`SERVER_LOCK`] that a server on Windows holds
/// exclusively for its life: the last byte a lock on the whole file covers,
/// as the standard library takes one there (`u64::MAX` bytes from 0), and
/// far past anything the file holds.
pub const WINDOWS_LOCK_BYTE: u64 = u64::MAX - 1;

/// The pid that the text of [`SERVER_LOCK`] names, if it names one. It
/// names none for the instant between a server taking the lock and writing
/// its pid.
pub fn lock_holder(text: &str) -> Option<u32> {
    text.trim().parse().ok()
}

/// The file in the data directory that says where the running server is.
pub const SERVER_JSON: &str = "server.json";

/// The file in the data directory that holds the local owner's session
/// token, on one line. Only the OS user can read it, and a program that can
/// holds a `full` session.
pub const CREDENTIAL: &str = "credential";

/// The first word of the one line `ritornel-server serve` prints on standard
/// output, once it can be found and stopped: `ready pid=<pid>
/// server_id=<id>`. A launcher waits for it.
pub const READY: &str = "ready";

/// The exit code `ritornel-server serve` reserves for one case: another
/// server holds the data directory. A launcher that lost the race to start
/// one goes back to finding the one that won.
pub const EXIT_IN_USE: u8 = 3;

/// How a program reads an environment variable. It is passed in, so that a
/// test can say what the environment holds without changing its own.
pub type Env<'a> = &'a dyn Fn(&str) -> Option<OsString>;

/// Where the data directory is: `flag` (`--data-dir`), then
/// [`DATA_DIR_VAR`], then the platform's default. `None` when none of the
/// three says. A relative path is returned as it was given.
pub fn data_dir(flag: Option<&Path>, env: Env<'_>) -> Option<PathBuf> {
    flag.map(Path::to_owned)
        .or_else(|| set(env, DATA_DIR_VAR))
        .or_else(|| default_data_dir(env))
}

/// A variable's value as a path, unless it is unset or empty.
pub(crate) fn set(env: Env<'_>, name: &str) -> Option<PathBuf> {
    env(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// The system's default data directory: `~/Library/Application
/// Support/Ritornel` on a Mac, `%LOCALAPPDATA%\Ritornel` on Windows, and
/// elsewhere `$XDG_DATA_HOME/ritornel`, which is `~/.local/share/ritornel`
/// unless the user moved it. Each is explained where it is chosen, in this
/// crate's `platform` module.
pub fn default_data_dir(env: Env<'_>) -> Option<PathBuf> {
    crate::platform::default_data_dir(env)
}

/// What `server.json` holds: how a local client finds the running server.
///
/// A client must not trust it alone. A server that was killed leaves it
/// behind, and its port may have gone to anything. A client believes the
/// file only while [`SERVER_LOCK`] is held and names the same pid, and
/// sends a credential to nothing else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerJson {
    /// The server's process.
    pub pid: u32,
    /// Where the server listens: an address and the port really bound,
    /// such as `127.0.0.1:4747`. It is always the address as digits and
    /// never a name like `localhost`, because a page's content security
    /// policy matches the host as written.
    pub endpoint: SocketAddr,
    /// The server's ID.
    pub server_id: Id,
    /// The [contract version](crate::CONTRACT_VERSION) the server speaks.
    pub contract_version: u32,
}

#[cfg(test)]
pub(crate) mod tests {
    use serde_json::json;

    use super::*;

    /// An environment that holds exactly `variables`.
    pub(crate) fn env(variables: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
        let variables: Vec<(String, OsString)> = variables
            .iter()
            .map(|(name, value)| ((*name).to_owned(), OsString::from(value)))
            .collect();
        move |wanted| {
            variables
                .iter()
                .find(|(name, _)| name == wanted)
                .map(|(_, value)| value.clone())
        }
    }

    #[test]
    fn the_flag_wins_then_the_variable_then_the_default() {
        let both = env(&[
            (DATA_DIR_VAR, "/from/env"),
            ("HOME", "/home/pat"),
            ("LOCALAPPDATA", r"C:\Users\pat\AppData\Local"),
        ]);
        assert_eq!(
            data_dir(Some(Path::new("/from/flag")), &both).unwrap(),
            Path::new("/from/flag")
        );
        assert_eq!(data_dir(None, &both).unwrap(), Path::new("/from/env"));

        let home_only = env(&[
            ("HOME", "/home/pat"),
            ("LOCALAPPDATA", r"C:\Users\pat\AppData\Local"),
        ]);
        assert_eq!(
            data_dir(None, &home_only).unwrap(),
            default_data_dir(&home_only).unwrap()
        );
    }

    #[test]
    fn an_empty_variable_counts_as_unset() {
        let empty = env(&[
            (DATA_DIR_VAR, ""),
            ("HOME", "/home/pat"),
            ("LOCALAPPDATA", r"C:\Users\pat\AppData\Local"),
        ]);
        assert_eq!(
            data_dir(None, &empty).unwrap(),
            default_data_dir(&empty).unwrap()
        );
    }

    #[test]
    fn with_nothing_to_go_on_there_is_no_data_directory() {
        assert_eq!(data_dir(None, &env(&[])), None);
    }

    #[test]
    fn a_relative_path_is_returned_as_it_was_given() {
        assert_eq!(
            data_dir(Some(Path::new("some/where")), &env(&[])).unwrap(),
            Path::new("some/where")
        );
    }

    #[test]
    fn the_lock_file_names_its_holder_as_a_number_on_a_line() {
        assert_eq!(lock_holder("4312\n"), Some(4312));
        assert_eq!(lock_holder("4312"), Some(4312));
        for none in ["", "\n", "pid 4312", "-1", "4312 4313"] {
            assert_eq!(lock_holder(none), None, "{none:?}");
        }
    }

    #[test]
    fn server_json_names_the_process_the_endpoint_the_server_and_the_contract() {
        let contents = ServerJson {
            pid: 4312,
            endpoint: SocketAddr::from(([127, 0, 0, 1], 4747)),
            server_id: Id::from_bytes([0x11; 16]),
            contract_version: 0,
        };
        let wire = json!({
            "pid": 4312,
            "endpoint": "127.0.0.1:4747",
            "serverId": "11111111-1111-1111-1111-111111111111",
            "contractVersion": 0,
        });
        assert_eq!(serde_json::to_value(&contents).unwrap(), wire);
        assert_eq!(
            serde_json::from_value::<ServerJson>(wire).unwrap(),
            contents
        );
    }

    #[test]
    fn an_endpoint_that_is_a_name_is_not_read() {
        let named = json!({
            "pid": 4312,
            "endpoint": "localhost:4747",
            "serverId": "11111111-1111-1111-1111-111111111111",
            "contractVersion": 0,
        });
        assert!(serde_json::from_value::<ServerJson>(named).is_err());
    }
}
