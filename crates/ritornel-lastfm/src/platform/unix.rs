//! Unix: a file's owner alone may read it when its mode says so.

use std::fs::File;
#[cfg(test)]
use std::path::Path;

/// How a person makes a file readable by its owner alone.
pub(crate) const HOW_TO_CLOSE: &str = "chmod 600";

/// Whether only the file's owner may read or write it: no permission bit
/// for the group or others.
pub(crate) fn private(file: &File) -> std::io::Result<bool> {
    use std::os::unix::fs::PermissionsExt;
    Ok(file.metadata()?.permissions().mode() & 0o077 == 0)
}

/// Makes the file at `path` one only its owner may read: mode `0600`.
#[cfg(test)]
pub(crate) fn close_to_others(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

/// Makes the file at `path` one everyone may read: mode `0644`.
#[cfg(test)]
pub(crate) fn open_to_others(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644)).unwrap();
}
