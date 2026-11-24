//! Windows: a file's owner alone may read it when it is owned by this user
//! and its access list lets nobody else in (`ritornel-windows`).

use std::fs::File;
#[cfg(test)]
use std::path::Path;

/// How a person makes a file readable by its owner alone: an access list of
/// their own user, inheriting nothing from its folder.
pub(crate) const HOW_TO_CLOSE: &str = "icacls lastfm.env /inheritance:r /grant:r %USERNAME%:F";

/// Whether the file is owned by this user and nobody else may read or
/// write it, the system and the administrators aside.
pub(crate) fn private(file: &File) -> std::io::Result<bool> {
    ritornel_windows::private::is_private(file)
}

/// Makes the file at `path` one only this user may read.
#[cfg(test)]
pub(crate) fn close_to_others(path: &Path) {
    ritornel_windows::private::restrict(path).unwrap();
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
