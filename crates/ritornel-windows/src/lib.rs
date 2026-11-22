//! What Ritornel asks of Windows that the standard library cannot give
//! without unsafe code. Each call into Win32 is kept here, behind a safe
//! function, so that every other crate of the workspace can go on
//! forbidding unsafe code. On any other platform the crate is empty.
//!
//! Code spans rather than intra-doc links are used below so `cargo doc`
//! passes on macOS and Linux, where this crate is empty.
//!
//! - `lock`: a lock on one byte of a file, past its end, so that what the
//!   file holds can still be read while the lock is held.
//! - `private`: a file or folder only its owner can open, and the check
//!   that one is.
//! - `file`: which file a path is (its volume and file index), the path a
//!   handle really has, and the room left on a disk.
//! - `clock`: milliseconds since the machine started, through sleep.
//! - `window`: a hidden window, for a process that has none: it hears a
//!   polite stop (`taskkill /PID`) and the end of the session, and is what
//!   the system's media controls are tied to.

#![cfg(windows)]
// Tests may unwrap and index, as in the rest of the workspace.
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

pub mod clock;
pub mod file;
pub mod lock;
pub mod private;
pub mod window;

/// A path as Win32 takes it: UTF-16, ending in a NUL.
fn wide(path: &std::path::Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str().encode_wide().chain([0]).collect()
}
