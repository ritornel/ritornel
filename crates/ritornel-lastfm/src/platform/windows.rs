//! Windows: a file's owner alone may read it when it is owned by this user
//! and its access list lets nobody else in (`ritornel-windows`).

use std::fs::File;
#[cfg(test)]
use std::path::Path;

/// How a person makes a file readable by its owner alone: a new file, made
/// by them, takes the access of the data directory, which only they can
/// open once the server has started on it.
pub(crate) const HOW_TO_CLOSE: &str = "delete it, then save it again in the data directory";

/// Whether the file is owned by this user and nobody else may read or
/// write it, the system and the administrators aside.
pub(crate) fn private(file: &File) -> std::io::Result<bool> {
    ritornel_windows::private::is_private(file)
}

/// Makes the file at `path` one only this user may read, as
/// [`HOW_TO_CLOSE`] says: made again, in a folder only this user may open.
#[cfg(test)]
pub(crate) fn close_to_others(path: &Path) {
    if let Some(parent) = path.parent() {
        ritornel_windows::private::restrict(parent).unwrap();
    }
    let text = std::fs::read(path).unwrap();
    std::fs::remove_file(path).unwrap();
    std::fs::write(path, text).unwrap();
}

/// Lets everyone read the file at `path`, as a copy from another machine
/// might.
#[cfg(test)]
pub(crate) fn open_to_others(path: &Path) {
    let done = std::process::Command::new("icacls")
        .arg(path)
        .args(["/grant", "*S-1-1-0:(R)"])
        .output()
        .unwrap();
    assert!(done.status.success(), "{done:?}");
}
