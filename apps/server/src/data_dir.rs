//! The data directory: where it is, and what is in it. See "The data
//! directory" in docs/design/server.md.
//!
//! ```text
//! <data-dir>/
//!   server.lock    held while running; holds the pid of the server that has it
//!   server.json    how a local client finds the running server
//!   credential     the local owner's session token; only the OS user can read it
//!   config.toml    optional: the settings needed before a database opens
//!   server.db      what describes this server
//!   logs/          JSON lines, one file a day
//! ```
//!
//! `library.db`, `backups/` and `cache/` arrive with what first uses them.
//!
//! Where the directory is, and the names of `server.lock`, `server.json` and
//! `credential`, are the contract's (`ritornel_contract::local`): a local client reads
//! what the server writes here.

use std::fs::{DirBuilder, OpenOptions, Permissions};
use std::io::ErrorKind;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use ritornel_contract::local;

use crate::error::io_error;
use crate::lock::DirLock;
use crate::{Env, Error, server_json};

/// Where the data directory is: `--data-dir`, then `RITORNEL_DATA_DIR`, then
/// the platform's default. A relative path is taken from the current
/// directory.
///
/// The order and the default are the contract's, because a local client
/// must look in the same place (`ritornel_contract::local`).
pub(crate) fn resolve(flag: Option<&Path>, env: Env<'_>) -> Result<PathBuf, Error> {
    let chosen = local::data_dir(flag, env).ok_or(Error::NoDataDir)?;
    std::path::absolute(&chosen).map_err(io_error("resolve", &chosen))
}

/// The mode of a file that only its owner may read or write.
const OWNER_ONLY: u32 = 0o600;

/// A data directory that exists.
#[derive(Debug, Clone)]
pub(crate) struct DataDir {
    root: PathBuf,
}

impl DataDir {
    /// Makes sure the directory and what this layer keeps in it exist.
    ///
    /// What it creates, only the OS user can read: the directory holds the
    /// local credential, and `server.db` holds the server's signing key. A
    /// directory that already exists keeps the permissions it has, so
    /// `server.db` is closed too, whether this call creates it or finds it:
    /// a database restored from a backup may arrive open to everyone.
    pub(crate) fn prepare(root: PathBuf) -> Result<Self, Error> {
        let dir = Self { root };
        for path in [dir.root.clone(), dir.logs()] {
            DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&path)
                .map_err(io_error("create", &path))?;
        }
        // SQLite keeps the mode of a file it finds, and gives the same mode
        // to the write-ahead log it puts beside it. The mode asked for here
        // applies only to a file this call creates, so one that was already
        // there is closed as well, with any log left beside it.
        let database = dir.server_db();
        OpenOptions::new()
            .append(true)
            .create(true)
            .mode(OWNER_ONLY)
            .open(&database)
            .and_then(|file| file.set_permissions(Permissions::from_mode(OWNER_ONLY)))
            .map_err(io_error("create", &database))?;
        for suffix in ["-wal", "-shm"] {
            let mut sidecar = database.clone().into_os_string();
            sidecar.push(suffix);
            let sidecar = PathBuf::from(sidecar);
            match std::fs::set_permissions(&sidecar, Permissions::from_mode(OWNER_ONLY)) {
                Err(error) if error.kind() != ErrorKind::NotFound => {
                    return Err(io_error("close", &sidecar)(error));
                }
                _ => {}
            }
        }
        Ok(dir)
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn lock_file(&self) -> PathBuf {
        self.root.join(local::SERVER_LOCK)
    }

    pub(crate) fn server_json(&self) -> PathBuf {
        self.root.join(local::SERVER_JSON)
    }

    pub(crate) fn credential(&self) -> PathBuf {
        self.root.join(local::CREDENTIAL)
    }

    pub(crate) fn config_toml(&self) -> PathBuf {
        self.root.join("config.toml")
    }

    pub(crate) fn server_db(&self) -> PathBuf {
        self.root.join("server.db")
    }

    pub(crate) fn logs(&self) -> PathBuf {
        self.root.join("logs")
    }
}

/// A data directory this process holds: it exists, this process has its
/// lock, and whatever `server.json` said before is gone. From taking it to
/// dropping it, this is the only server on the directory.
#[derive(Debug)]
pub struct HeldDir {
    dir: DataDir,
    _lock: DirLock,
}

impl HeldDir {
    /// Makes sure the directory exists and takes its lock. It fails with
    /// [`Error::InUse`] if another server has it.
    pub fn take(root: PathBuf) -> Result<Self, Error> {
        let dir = DataDir::prepare(root)?;
        let lock = DirLock::acquire(dir.root(), &dir.lock_file()).map_err(|error| match error {
            // The server that has it says where it listens in `server.json`,
            // once it listens.
            Error::InUse {
                dir: root,
                pid: Some(pid),
                ..
            } => Error::InUse {
                endpoint: server_json::endpoint_of(&dir.server_json(), pid),
                dir: root,
                pid: Some(pid),
            },
            other => other,
        })?;
        // Whatever `server.json` says now, a server that is gone wrote it.
        server_json::remove(&dir.server_json())?;
        Ok(Self { dir, _lock: lock })
    }

    pub(crate) fn dir(&self) -> &DataDir {
        &self.dir
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Scratch, env, mode};

    #[test]
    fn the_flag_wins_then_the_variable_then_the_default() {
        let both = env(&[(local::DATA_DIR_VAR, "/from/env"), ("HOME", "/home/pat")]);
        assert_eq!(
            resolve(Some(Path::new("/from/flag")), &both).unwrap(),
            Path::new("/from/flag")
        );
        assert_eq!(resolve(None, &both).unwrap(), Path::new("/from/env"));

        let home_only = env(&[("HOME", "/home/pat")]);
        assert_eq!(
            resolve(None, &home_only).unwrap(),
            local::default_data_dir(&home_only).unwrap()
        );
    }

    #[test]
    fn with_nothing_to_go_on_there_is_no_data_directory() {
        let refused = resolve(None, &env(&[]));
        assert!(matches!(refused, Err(Error::NoDataDir)), "{refused:?}");
    }

    #[test]
    fn a_relative_path_is_taken_from_the_current_directory() {
        let resolved = resolve(Some(Path::new("some/where")), &env(&[])).unwrap();
        assert_eq!(
            resolved,
            std::env::current_dir().unwrap().join("some/where")
        );
    }

    #[test]
    fn a_new_data_directory_is_closed_to_other_users() {
        let scratch = Scratch::new();
        let root = scratch.path().join("deep/inside/data");
        let dir = DataDir::prepare(root.clone()).unwrap();
        assert_eq!(mode(&root), 0o700);
        assert_eq!(mode(&dir.logs()), 0o700);
        assert_eq!(mode(&dir.server_db()), 0o600);
    }

    #[test]
    fn a_database_that_was_open_to_other_users_is_closed() {
        let scratch = Scratch::new();
        // As after a restore that did not keep modes: the directory, the
        // database and a log left beside it can all be read by anyone.
        let root = scratch.path().join("data");
        std::fs::create_dir(&root).unwrap();
        let open = Permissions::from_mode(0o644);
        for name in ["server.db", "server.db-wal", "server.db-shm"] {
            std::fs::write(root.join(name), "").unwrap();
            std::fs::set_permissions(root.join(name), open.clone()).unwrap();
        }

        let dir = DataDir::prepare(root.clone()).unwrap();
        assert_eq!(mode(&dir.server_db()), 0o600);
        assert_eq!(mode(&root.join("server.db-wal")), 0o600);
        assert_eq!(mode(&root.join("server.db-shm")), 0o600);
    }

    #[test]
    fn preparing_again_leaves_what_is_there() {
        let scratch = Scratch::new();
        let dir = DataDir::prepare(scratch.path().join("data")).unwrap();
        std::fs::write(dir.server_db(), b"not really a database").unwrap();
        DataDir::prepare(dir.root().to_owned()).unwrap();
        assert_eq!(
            std::fs::read(dir.server_db()).unwrap(),
            b"not really a database"
        );
    }

    #[test]
    fn a_held_directory_is_refused_to_the_next_taker_until_it_is_let_go() {
        let scratch = Scratch::new();
        let root = scratch.path().join("data");
        let held = HeldDir::take(root.clone()).unwrap();
        assert_eq!(held.dir().root(), root);

        let refused = HeldDir::take(root.clone());
        let Err(Error::InUse { dir, pid, endpoint }) = &refused else {
            panic!("{refused:?}");
        };
        assert_eq!((dir, *pid), (&root, Some(std::process::id())));
        // The holder has not said where it listens.
        assert_eq!(*endpoint, None);

        drop(held);
        HeldDir::take(root).unwrap();
    }

    #[test]
    fn the_refusal_names_the_endpoint_once_the_holder_has_published_one() {
        let scratch = Scratch::new();
        let root = scratch.path().join("data");
        let held = HeldDir::take(root.clone()).unwrap();
        let endpoint = std::net::SocketAddr::from(([127, 0, 0, 1], 4747));
        let _published = server_json::ServerJson::publish(
            &held.dir().server_json(),
            endpoint,
            ritornel_contract::Id::from_bytes([0x11; 16]),
        )
        .unwrap();

        let refused = HeldDir::take(root.clone()).unwrap_err();
        assert_eq!(
            refused.to_string(),
            format!(
                "data directory in use by pid {} at 127.0.0.1:4747: {}",
                std::process::id(),
                root.display()
            )
        );
    }

    #[test]
    fn taking_a_directory_removes_the_server_json_a_dead_server_left() {
        let scratch = Scratch::new();
        let root = scratch.path().join("data");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("server.json"), "left behind").unwrap();
        let held = HeldDir::take(root).unwrap();
        assert!(!held.dir().server_json().exists());
    }
}
