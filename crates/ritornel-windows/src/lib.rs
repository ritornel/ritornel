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
//!   handle really has, the room left on a disk, and a rename that is on
//!   the disk when it returns.
//! - `clock`: milliseconds since the machine started, through sleep.
//! - `name`: the machine's host name, in the case it was set.
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
pub mod name;
pub mod private;
pub mod window;

/// A path as Win32 takes it: UTF-16, ending in a NUL.
fn wide(path: &std::path::Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str().encode_wide().chain([0]).collect()
}

/// A path as Win32 takes it past `MAX_PATH`, as the standard library passes
/// one: made absolute, in the `\\?\` form, in UTF-16 and ending in a NUL. A
/// path with a NUL in it is refused, since Win32 would read only what comes
/// before it.
fn wide_verbatim(path: &std::path::Path) -> std::io::Result<Vec<u16>> {
    use std::os::windows::ffi::OsStrExt;
    let path = std::path::absolute(path)?;
    let units: Vec<u16> = path.as_os_str().encode_wide().collect();
    if units.contains(&0) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "the path holds a NUL",
        ));
    }
    let starts = |text: &str| {
        units
            .iter()
            .copied()
            .take(text.len())
            .eq(text.encode_utf16())
    };
    let (prefix, skip) = if starts(r"\\?\") || starts(r"\\.\") {
        ("", 0)
    } else if starts(r"\\") {
        // `\\server\share` is `\\?\UNC\server\share`.
        (r"\\?\UNC\", 2)
    } else {
        (r"\\?\", 0)
    };
    Ok(prefix
        .encode_utf16()
        .chain(units.into_iter().skip(skip))
        .chain([0])
        .collect())
}
