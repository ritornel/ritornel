//! What the unit tests share.

use std::fs::File;

use crate::platform::try_hold_as_a_server;

/// Holds a lock file as a running server does, or fails the test.
pub(crate) fn hold_as_a_server(file: &File) {
    assert!(try_hold_as_a_server(file), "the lock is held elsewhere");
}
