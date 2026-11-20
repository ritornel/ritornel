//! What the unit tests share.

use std::fs::File;

use ritornel_contract::local;

/// Tries to hold a lock file as a running server does: the whole file on
/// Unix, and on Windows the one byte past its end that a server holds,
/// which leaves the pid in it readable (`local::WINDOWS_LOCK_BYTE`).
pub(crate) fn try_hold_as_a_server(file: &File) -> bool {
    #[cfg(unix)]
    {
        let _ = local::WINDOWS_LOCK_BYTE;
        file.try_lock().is_ok()
    }
    #[cfg(windows)]
    {
        ritornel_windows::lock::try_lock(
            file,
            local::WINDOWS_LOCK_BYTE,
            1,
            ritornel_windows::lock::Hold::Exclusive,
        )
        .unwrap_or(false)
    }
}

/// Holds a lock file as a running server does, or fails the test.
pub(crate) fn hold_as_a_server(file: &File) {
    assert!(try_hold_as_a_server(file), "the lock is held elsewhere");
}
