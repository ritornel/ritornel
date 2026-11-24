//! What every Unix shares: files closed to others by their mode, a lock on
//! the whole file, free space from `statvfs`, and SIGINT and SIGTERM.

use std::fs::{DirBuilder, File, OpenOptions};
use std::path::Path;
use std::sync::mpsc::Sender;

use tokio::runtime::Runtime;

use crate::Error;
use crate::error::io_error;
use crate::serve::Stop;

/// The mode of a file that only its owner may read or write.
const OWNER_ONLY: u32 = 0o600;

/// Creates the folder at `path`, and those above it that are missing, with
/// mode `0700`. A folder that is there already is left as it is.
pub(crate) fn create_private_dir(path: &Path) -> Result<(), Error> {
    use std::os::unix::fs::DirBuilderExt;
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .map_err(io_error("create", path))
}

/// Creates the one folder at `path`, with mode `0700`, and fails if it is
/// there already: two writers never share it. Its parent must be there.
pub(crate) fn create_new_private_dir(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    DirBuilder::new().mode(0o700).create(path)
}

/// Syncs the folder at `path`, so that the names made, renamed or removed
/// in it survive a loss of power.
pub(crate) fn sync_dir(path: &Path) -> std::io::Result<()> {
    File::open(path)?.sync_all()
}

/// Renames the folder at `from` to `to`, and nothing more: a caller that
/// needs the new name to survive a loss of power syncs the folder that
/// holds it after ([`sync_dir`]), as `backup::publish` does.
pub(crate) fn rename_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::rename(from, to)
}

/// Closes the file at `path` to everyone but the OS user: mode `0600`.
pub(crate) fn close_to_others(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(OWNER_ONLY))
}

/// Makes a file that `options` creates one only the OS user can open:
/// mode `0600`.
pub(crate) fn create_owner_only(options: &mut OpenOptions) {
    std::os::unix::fs::OpenOptionsExt::mode(options, OWNER_ONLY);
}

/// Creates a new file at `path` with mode `0600`, from before anything is
/// written to it. It fails if something is there.
pub(crate) fn create_private_file(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    create_owner_only(&mut options);
    options.open(path)
}

/// Whether the file has no permission bit for the group or others.
pub(crate) fn is_private(file: &File) -> std::io::Result<bool> {
    use std::os::unix::fs::PermissionsExt;
    Ok(file.metadata()?.permissions().mode() & 0o077 == 0)
}

/// The space left on the disk that holds `path`, in bytes: the blocks an
/// unprivileged process may use, in the size they are counted in.
pub(crate) fn free_bytes(path: &Path) -> Option<u64> {
    let disk = rustix::fs::statvfs(path).ok()?;
    disk.f_bavail.checked_mul(disk.f_frsize)
}

/// Tries to take the lock as a server holds it, without waiting: the whole
/// file, to itself.
pub(crate) fn take_lock(file: &File) -> std::io::Result<bool> {
    match file.try_lock() {
        Ok(()) => Ok(true),
        Err(std::fs::TryLockError::WouldBlock) => Ok(false),
        Err(std::fs::TryLockError::Error(source)) => Err(source),
    }
}

/// Whether the lock can be had shared: nothing but clients looking has it.
/// It is let go at once.
pub(crate) fn lock_shareable(file: &File) -> std::io::Result<bool> {
    match file.try_lock_shared() {
        Ok(()) => file.unlock().map(|()| true),
        Err(std::fs::TryLockError::WouldBlock) => Ok(false),
        Err(std::fs::TryLockError::Error(source)) => Err(source),
    }
}

/// What watches for a stop. On Unix the handlers live in the runtime, and
/// nothing is held.
#[derive(Debug)]
pub(crate) struct Signals;

impl Signals {
    /// Says the server has stopped: nothing on Unix waits for it.
    pub(crate) fn stopped(&self) {}
}

/// Sends a stop when the process gets SIGINT or SIGTERM. The handlers are in
/// place when this returns.
pub(crate) fn watch_signals(runtime: &Runtime, stop: &Sender<Stop>) -> std::io::Result<Signals> {
    use tokio::signal::unix::{SignalKind, signal};
    let _in_runtime = runtime.enter();
    for (kind, name) in [
        (SignalKind::interrupt(), "SIGINT"),
        (SignalKind::terminate(), "SIGTERM"),
    ] {
        let mut signals = signal(kind)?;
        let stop = stop.clone();
        runtime.spawn(async move {
            if signals.recv().await.is_some() {
                // The main thread has gone only if it is stopping already.
                let _ = stop.send(Stop::Signal(name));
            }
        });
    }
    Ok(Signals)
}

/// Whether only the owner can open a file or directory: the modes `0600`
/// and `0700`.
#[cfg(test)]
pub(crate) fn path_is_private(path: &Path) -> bool {
    mode(path) & 0o077 == 0
}

/// Opens a file or folder to other users, as a copy that did not keep its
/// mode might be.
#[cfg(test)]
pub(crate) fn open_to_others(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mode = if path.is_dir() { 0o755 } else { 0o644 };
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

/// The permission bits of a file or directory.
#[cfg(test)]
fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::HeldDir;
    use crate::testing::Scratch;

    #[test]
    fn a_new_data_directory_has_the_modes_of_its_owner_alone() {
        let scratch = Scratch::new();
        let root = scratch.path().join("deep/inside/data");
        let held = HeldDir::take(root.clone()).unwrap();
        assert_eq!(mode(&root), 0o700);
        assert_eq!(mode(&held.dir().logs()), 0o700);
        assert_eq!(mode(&held.dir().server_db()), 0o600);
    }
}
