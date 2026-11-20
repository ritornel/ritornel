//! Windows: files closed to others by an access list that names the user
//! alone, a lock on one byte past the end of `server.lock`, and in place of
//! signals, the console's events and a hidden window. No background service
//! and no media session yet. What the standard library cannot ask is in
//! `ritornel-windows`.

use std::fs::{DirBuilder, File, OpenOptions};
use std::path::Path;
use std::sync::mpsc::Sender;

use ritornel_contract::local;
use ritornel_windows::lock::{Hold, try_lock, unlock};
use tokio::runtime::Runtime;

use crate::Error;
use crate::error::io_error;
use crate::serve::Stop;

#[cfg(feature = "media-session")]
pub(crate) use super::fallback::{HAS_MEDIA_SESSION, MEDIA_SESSION_ON_RUNTIME, MediaControls};
pub(crate) use super::fallback::{install_service, service_status, uninstall_service, wait};

/// Creates the folder at `path`, and those above it that are missing, with
/// an access list of the OS user alone, which what is later made inside it
/// inherits. A folder that is there already is left as it is.
pub(crate) fn create_private_dir(path: &Path) -> Result<(), Error> {
    if path.is_dir() {
        return Ok(());
    }
    // The highest folder this call makes is closed, and those it makes
    // inside it inherit that.
    let mut highest = path;
    while let Some(parent) = highest
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        if parent.exists() {
            break;
        }
        highest = parent;
    }
    DirBuilder::new()
        .recursive(true)
        .create(path)
        .map_err(io_error("create", path))?;
    ritornel_windows::private::restrict(highest).map_err(io_error("close", highest))
}

/// Closes the file at `path` to everyone but the OS user.
pub(crate) fn close_to_others(path: &Path) -> std::io::Result<()> {
    ritornel_windows::private::restrict(path)
}

/// Makes a file that `options` creates one only the OS user can open: here
/// the data directory's own access list does that, and the file inherits
/// it.
pub(crate) fn create_owner_only(_options: &mut OpenOptions) {}

/// Creates a new file at `path` that only the OS user can open, from before
/// anything is written to it. It fails if something is there.
pub(crate) fn create_private_file(path: &Path) -> std::io::Result<File> {
    ritornel_windows::private::create_restricted(path)
}

/// Whether the file's access list has no entry for anyone but the OS user,
/// the system and the administrators, who can take any file.
pub(crate) fn is_private(file: &File) -> std::io::Result<bool> {
    ritornel_windows::private::is_private(file)
}

/// What this user may still write on the disk that holds `path`, after any
/// quota, in bytes.
pub(crate) fn free_bytes(path: &Path) -> Option<u64> {
    ritornel_windows::file::free_bytes(path).ok()
}

/// Tries to take the lock as a server holds it, without waiting. Windows
/// enforces a lock on what it covers, so a server holds one byte past the
/// file's end, and the pid in it can still be read
/// ([`local::WINDOWS_LOCK_BYTE`]).
pub(crate) fn take_lock(file: &File) -> std::io::Result<bool> {
    try_lock(file, local::WINDOWS_LOCK_BYTE, 1, Hold::Exclusive)
}

/// Whether the lock can be had shared: nothing but clients looking has it.
/// It is let go at once.
pub(crate) fn lock_shareable(file: &File) -> std::io::Result<bool> {
    if !try_lock(file, local::WINDOWS_LOCK_BYTE, 1, Hold::Shared)? {
        return Ok(false);
    }
    unlock(file, local::WINDOWS_LOCK_BYTE, 1).map(|()| true)
}

/// What watches for a stop: the hidden window, which the run keeps until
/// it has stopped, and then tells so ([`Window::stopped`]), as a session
/// that is ending waits for that before it ends the process.
///
/// [`Window::stopped`]: ritornel_windows::window::Window::stopped
pub(crate) type Signals = ritornel_windows::window::Window;

/// Sends a stop on what Windows has in place of signals: Ctrl+C, Ctrl+Break
/// and the console closing, when the server runs in one, and for a server
/// with no console, as one the desktop app or the background service starts
/// is, a hidden window asked to close (`taskkill /PID <pid>`, without `/F`)
/// or told that the session is ending. The handlers and the window are in
/// place when this returns.
pub(crate) fn watch_signals(runtime: &Runtime, stop: &Sender<Stop>) -> std::io::Result<Signals> {
    use ritornel_windows::window::{Closing, Window};
    use tokio::signal::windows::{ctrl_break, ctrl_c, ctrl_close, ctrl_logoff, ctrl_shutdown};

    let _in_runtime = runtime.enter();
    macro_rules! on {
        ($listen:expr, $reason:expr) => {{
            let mut signals = $listen?;
            let stop = stop.clone();
            runtime.spawn(async move {
                if signals.recv().await.is_some() {
                    // The main thread has gone only if it is stopping already.
                    let _ = stop.send(Stop::Signal($reason));
                }
            });
        }};
    }
    on!(ctrl_c(), "Ctrl+C");
    on!(ctrl_break(), "Ctrl+Break");
    on!(ctrl_close(), "console closed");
    on!(ctrl_logoff(), "session ending");
    on!(ctrl_shutdown(), "session ending");
    let stop = stop.clone();
    Window::open("Ritornel server", move |why| {
        let _ = stop.send(Stop::Signal(match why {
            Closing::Asked => "asked to close",
            Closing::SessionEnding => "session ending",
        }));
    })
}

/// Whether only the owner can open a file or directory: an access list that
/// names the owner alone.
#[cfg(test)]
pub(crate) fn path_is_private(path: &Path) -> bool {
    ritornel_windows::private::path_is_private(path).unwrap()
}

/// Opens a file to other users, as a copy that did not keep its access list
/// might be.
#[cfg(test)]
pub(crate) fn open_to_others(path: &Path) {
    let done = std::process::Command::new("icacls")
        .arg(path)
        .args(["/grant", "*S-1-1-0:(R)"])
        .output()
        .unwrap();
    assert!(done.status.success(), "{done:?}");
}
