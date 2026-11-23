//! One server per data directory: an advisory lock on `server.lock`, held for
//! the life of the process. See "Single-instance lock" in docs/internals/server.md.
//!
//! The operating system drops the lock when the process dies, however it
//! dies, so a lock that is held always means a server that is running. A
//! local client relies on that: it tries the lock, for an instant and
//! shared, to learn whether a server is alive before it believes
//! `server.json` (`ritornel_contract::local::SERVER_LOCK`).
//!
//! On Unix the server holds the whole file. On Windows, which enforces
//! locks, it holds one byte past the file's end, so that the pid in it can
//! still be read ([`local::WINDOWS_LOCK_BYTE`]).

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use ritornel_contract::local;

use crate::error::io_error;
use crate::{Error, platform};

/// How long a starting server goes on trying for the lock while all that is
/// in its way is clients looking at it. A look lasts microseconds, so this
/// is never reached by anything but a fault.
const LOOK_PATIENCE: Duration = Duration::from_secs(2);

/// How long it waits between two tries.
const LOOK_PAUSE: Duration = Duration::from_millis(1);

/// The lock on a data directory. Dropping it, or the process ending, lets
/// the next server in.
#[derive(Debug)]
pub(crate) struct DirLock {
    // Held, never read: the lock lasts as long as the file is open.
    _file: File,
}

impl DirLock {
    /// Takes the lock on the data directory `dir`, whose lock file is
    /// `path`, and writes this process's pid into the file, so that a
    /// server that is refused can say who has it and a client can tell
    /// whose `server.json` it is reading.
    ///
    /// A server that has the lock is the only thing that refuses it for
    /// good, and it is refused at once. A client that is looking holds the
    /// lock shared for an instant, and that is waited out.
    pub(crate) fn acquire(dir: &Path, path: &Path) -> Result<Self, Error> {
        Self::acquire_waiting_for_look(dir, path, || std::thread::sleep(LOOK_PAUSE))
    }

    /// Calls `wait_for_look` before retrying a lock that clients can share.
    fn acquire_waiting_for_look(
        dir: &Path,
        path: &Path,
        mut wait_for_look: impl FnMut(),
    ) -> Result<Self, Error> {
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        platform::create_owner_only(&mut options);
        let mut file = options.open(path).map_err(io_error("open", path))?;
        let started = Instant::now();
        loop {
            if platform::take_lock(&file).map_err(io_error("lock", path))? {
                break;
            }
            // Something has it. A server has it to itself, and shares it
            // with nobody. If it can be shared, there is no server, only
            // clients looking.
            if !platform::lock_shareable(&file).map_err(io_error("lock", path))? {
                return Err(Error::InUse {
                    dir: dir.to_owned(),
                    pid: holder(&mut file),
                    endpoint: None,
                });
            }
            if started.elapsed() >= LOOK_PATIENCE {
                return Err(Error::InUse {
                    dir: dir.to_owned(),
                    pid: None,
                    endpoint: None,
                });
            }
            wait_for_look();
        }
        // The pid a dead server left is replaced before anything else is
        // done, so that the lock and the file disagree for as short a time
        // as two calls take.
        file.set_len(0)
            .and_then(|()| file.rewind())
            .and_then(|()| writeln!(file, "{}", std::process::id()))
            .map_err(io_error("write", path))?;
        Ok(Self { _file: file })
    }
}

/// The pid the holder wrote. It is missing for the instant between a server
/// taking the lock and writing its pid.
fn holder(file: &mut File) -> Option<u32> {
    let mut text = String::new();
    file.read_to_string(&mut text).ok()?;
    local::lock_holder(&text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Scratch;

    #[test]
    fn a_second_lock_is_refused_and_names_the_holder() {
        let scratch = Scratch::new();
        let path = scratch.path().join("server.lock");
        let _held = DirLock::acquire(scratch.path(), &path).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("{}\n", std::process::id())
        );

        let refused = DirLock::acquire(scratch.path(), &path);
        let Err(Error::InUse { dir, pid, .. }) = &refused else {
            panic!("{refused:?}");
        };
        assert_eq!(dir, scratch.path());
        assert_eq!(*pid, Some(std::process::id()));
        // Being refused leaves the holder's pid in the file.
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("{}\n", std::process::id())
        );
    }

    #[test]
    fn the_lock_is_free_again_when_its_holder_lets_go() {
        let scratch = Scratch::new();
        let path = scratch.path().join("server.lock");
        drop(DirLock::acquire(scratch.path(), &path).unwrap());
        DirLock::acquire(scratch.path(), &path).unwrap();
    }

    #[test]
    fn a_lock_file_left_by_a_dead_server_is_no_obstacle() {
        let scratch = Scratch::new();
        let path = scratch.path().join("server.lock");
        // What a killed server leaves: the file, a pid, and no lock.
        std::fs::write(&path, "4312\n").unwrap();
        DirLock::acquire(scratch.path(), &path).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("{}\n", std::process::id())
        );
    }

    #[test]
    fn clients_looking_at_the_lock_are_waited_out_and_do_not_refuse_a_server() {
        let scratch = Scratch::new();
        let path = scratch.path().join("server.lock");
        let client = File::create_new(&path).unwrap();
        client.try_lock_shared().unwrap();
        let mut client = Some(client);
        // Let the client finish its look only when the server has seen
        // the shared lock and is about to retry.
        let _held = DirLock::acquire_waiting_for_look(scratch.path(), &path, || {
            let client = client.take().expect("the client has already let go");
            client.unlock().unwrap();
        })
        .unwrap();
        assert!(client.is_none(), "the server did not wait for the client");

        let refused = DirLock::acquire(scratch.path(), &path);
        assert!(
            matches!(refused, Err(Error::InUse { pid: Some(_), .. })),
            "{refused:?}"
        );
    }

    #[test]
    fn the_refusal_reads_as_the_notes_say() {
        let listening = Error::InUse {
            dir: "/srv/ritornel".into(),
            pid: Some(4312),
            endpoint: Some(([127, 0, 0, 1], 4747).into()),
        };
        assert_eq!(
            listening.to_string(),
            "data directory in use by pid 4312 at 127.0.0.1:4747: /srv/ritornel"
        );
        assert_eq!(listening.exit_code(), crate::EXIT_IN_USE);
        let with_pid = Error::InUse {
            dir: "/srv/ritornel".into(),
            pid: Some(4312),
            endpoint: None,
        };
        assert_eq!(
            with_pid.to_string(),
            "data directory in use by pid 4312: /srv/ritornel"
        );
        let without = Error::InUse {
            dir: "/srv/ritornel".into(),
            pid: None,
            endpoint: None,
        };
        assert_eq!(
            without.to_string(),
            "data directory in use by another server: /srv/ritornel"
        );
    }
}
