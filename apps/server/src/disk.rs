//! How much room is left on a disk.

use std::path::Path;

/// The bytes this process may still write on the disk that holds `path`.
/// `None` if the system would not say.
pub(crate) fn free_bytes(path: &Path) -> Option<u64> {
    #[cfg(unix)]
    {
        let disk = rustix::fs::statvfs(path).ok()?;
        // Blocks an unprivileged process may use, in the size they are
        // counted in.
        disk.f_bavail.checked_mul(disk.f_frsize)
    }
    // What this user may still write, after any quota.
    #[cfg(windows)]
    {
        ritornel_windows::file::free_bytes(path).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Scratch;

    #[test]
    fn a_directory_that_exists_has_room_and_one_that_does_not_has_no_answer() {
        let scratch = Scratch::new();
        assert!(free_bytes(scratch.path()).unwrap() > 0);
        assert_eq!(free_bytes(&scratch.path().join("missing")), None);
    }
}
