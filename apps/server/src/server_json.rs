//! `server.json`: how a local client finds the running server. It is written
//! once startup has succeeded and removed on a clean stop. See decision 3 and
//! "The data directory" in docs/design/server.md.
//!
//! A client must not trust it alone. A server that was killed leaves it
//! behind, so a client checks that the server it names answers.

use std::io::{ErrorKind, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use ritornel_contract::{CONTRACT_VERSION, Id, local};

use crate::Error;
use crate::error::io_error;

/// A `server.json` that names this process. Dropping it removes the file, so
/// that no way out of a run leaves one behind.
///
/// What the file holds is the contract's `local::ServerJson`, because a
/// local client reads it.
#[derive(Debug)]
pub(crate) struct ServerJson {
    path: PathBuf,
}

impl ServerJson {
    /// Writes the file under a temporary name and renames it into place, so
    /// a reader sees all of it or none. `endpoint` is the address and the
    /// port the listener really has.
    pub(crate) fn publish(path: &Path, endpoint: SocketAddr, server_id: Id) -> Result<Self, Error> {
        let contents = local::ServerJson {
            pid: std::process::id(),
            endpoint,
            server_id,
            contract_version: CONTRACT_VERSION,
        };
        let mut text = serde_json::to_string_pretty(&contents)
            .map_err(|error| io_error("write", path)(error.into()))?;
        text.push('\n');

        let temporary = path.with_extension("json.tmp");
        std::fs::File::create(&temporary)
            .and_then(|mut file| {
                file.write_all(text.as_bytes())?;
                file.sync_all()
            })
            .map_err(io_error("write", &temporary))?;
        std::fs::rename(&temporary, path).map_err(io_error("write", path))?;
        Ok(Self {
            path: path.to_owned(),
        })
    }

    /// Removes the file, and says so if that failed.
    pub(crate) fn remove(self) -> Result<(), Error> {
        remove(&self.path)
    }
}

impl Drop for ServerJson {
    fn drop(&mut self) {
        // After `remove` the file is already gone. On any other way out
        // there is nobody left to tell.
        let _ = remove(&self.path);
    }
}

/// Removes a `server.json`, if there is one. A server that holds the lock
/// calls it before anything else: whatever the file says then was written by
/// a server that is gone.
pub(crate) fn remove(path: &Path) -> Result<(), Error> {
    match std::fs::remove_file(path) {
        Err(error) if error.kind() != ErrorKind::NotFound => Err(io_error("remove", path)(error)),
        _ => Ok(()),
    }
}

/// Where the server with this pid says it listens, if the `server.json` at
/// `path` is its own. A server that holds the lock and has not started
/// listening has none, and neither has one whose file cannot be read.
pub(crate) fn endpoint_of(path: &Path, pid: u32) -> Option<SocketAddr> {
    let text = std::fs::read_to_string(path).ok()?;
    let contents: local::ServerJson = serde_json::from_str(&text).ok()?;
    (contents.pid == pid).then_some(contents.endpoint)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Scratch;

    const ENDPOINT: ([u8; 4], u16) = ([127, 0, 0, 1], 51_234);

    fn publish(path: &Path, id: u8) -> ServerJson {
        ServerJson::publish(path, ENDPOINT.into(), Id::from_bytes([id; 16])).unwrap()
    }

    #[test]
    fn it_names_this_process_the_endpoint_the_server_and_the_contract() {
        let scratch = Scratch::new();
        let path = scratch.path().join("server.json");
        let published = publish(&path, 0x11);

        let contents: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            contents,
            serde_json::json!({
                "pid": std::process::id(),
                "endpoint": "127.0.0.1:51234",
                "serverId": "11111111-1111-1111-1111-111111111111",
                "contractVersion": CONTRACT_VERSION,
            })
        );
        assert!(!scratch.path().join("server.json.tmp").exists());

        published.remove().unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn it_is_removed_on_any_way_out() {
        let scratch = Scratch::new();
        let path = scratch.path().join("server.json");
        drop(publish(&path, 0x11));
        assert!(!path.exists());
    }

    #[test]
    fn removing_one_that_is_not_there_is_fine() {
        let scratch = Scratch::new();
        remove(&scratch.path().join("server.json")).unwrap();
    }

    #[test]
    fn a_new_one_replaces_what_a_dead_server_left() {
        let scratch = Scratch::new();
        let path = scratch.path().join("server.json");
        std::fs::write(&path, "{\"pid\": 4312, \"serverId\": \"stale\"}").unwrap();
        let _published = publish(&path, 0x22);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("22222222-2222-2222-2222-222222222222"),
            "{text}"
        );
        assert!(!text.contains("stale"), "{text}");
    }

    #[test]
    fn the_endpoint_is_read_back_only_for_the_process_that_wrote_it() {
        let scratch = Scratch::new();
        let path = scratch.path().join("server.json");
        assert_eq!(endpoint_of(&path, std::process::id()), None);

        let _published = publish(&path, 0x11);
        assert_eq!(
            endpoint_of(&path, std::process::id()),
            Some(ENDPOINT.into())
        );
        assert_eq!(endpoint_of(&path, std::process::id() + 1), None);

        std::fs::write(&path, "not json").unwrap();
        assert_eq!(endpoint_of(&path, std::process::id()), None);
    }
}
