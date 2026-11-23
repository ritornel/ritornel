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
//!   handle really has, the room left on a disk, and a rename that asks
//!   to be written through to the disk.
//! - `clock`: milliseconds since the machine started, through sleep.
//! - `name`: the machine's host name, in the case it was set.
//! - `window`: a hidden window, for a process that has none: it hears a
//!   polite stop (`taskkill /PID`) and the end of the session, and is what
//!   the system's media controls are tied to.
//! - `media`, with the `media` feature: the system's media transport
//!   controls, tied to such a window.

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
#[cfg(feature = "media")]
pub mod media;
pub mod name;
pub mod private;
pub mod window;

/// A path as Win32 takes it: UTF-16, ending in a NUL.
fn wide(path: &std::path::Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str().encode_wide().chain([0]).collect()
}

/// A path made absolute, as Win32 takes it whatever its length, and as the
/// standard library passes one: in UTF-16, ending in a NUL, and in the
/// `\\?\` form once it is too long for a plain one. A path with a NUL in it
/// is refused, since Win32 would read only what comes before it.
fn wide_absolute(path: &std::path::Path) -> std::io::Result<Vec<u16>> {
    use std::os::windows::ffi::OsStrExt;
    let units: Vec<u16> = std::path::absolute(path)?
        .as_os_str()
        .encode_wide()
        .collect();
    if units.contains(&0) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "the path holds a NUL",
        ));
    }
    Ok(long_form(units))
}

/// The longest plain path, with its NUL, that every Win32 call takes:
/// `CreateDirectoryW` takes 12 fewer than `MAX_PATH`.
const PLAIN_PATH: usize = 248;

/// An absolute path as the standard library hands it to Win32: as it is
/// when it is short or verbatim already, and otherwise in the `\\?\` form,
/// `\\?\UNC\` for a share and `\\?\` in place of a device's `\\.\`; with
/// its NUL.
fn long_form(units: Vec<u16>) -> Vec<u16> {
    let starts = |text: &str| units.starts_with(&text.encode_utf16().collect::<Vec<_>>());
    let (prefix, skip) = if starts(r"\\?\") || units.len() + 1 < PLAIN_PATH {
        ("", 0)
    } else if starts(r"\\.\") {
        (r"\\?\", 4)
    } else if starts(r"\\") {
        (r"\\?\UNC\", 2)
    } else {
        (r"\\?\", 0)
    };
    prefix
        .encode_utf16()
        .chain(units.into_iter().skip(skip))
        .chain([0])
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn long_form_of(path: &str) -> String {
        let units = long_form(path.encode_utf16().collect());
        String::from_utf16(units.strip_suffix(&[0]).unwrap()).unwrap()
    }

    #[test]
    fn a_short_path_is_passed_as_it_is_and_a_long_one_in_the_verbatim_form() {
        let long = "a".repeat(250);
        for (path, passed) in [
            (r"C:\Backups".to_owned(), r"C:\Backups".to_owned()),
            (
                r"\\nas\music\backups".to_owned(),
                r"\\nas\music\backups".to_owned(),
            ),
            (r"\\?\C:\Backups".to_owned(), r"\\?\C:\Backups".to_owned()),
            (format!(r"C:\{long}"), format!(r"\\?\C:\{long}")),
            (
                format!(r"\\nas\music\{long}"),
                format!(r"\\?\UNC\nas\music\{long}"),
            ),
            (format!(r"\\.\C:\{long}"), format!(r"\\?\C:\{long}")),
            (
                format!(r"\\?\UNC\nas\{long}"),
                format!(r"\\?\UNC\nas\{long}"),
            ),
        ] {
            assert_eq!(long_form_of(&path), passed, "{path}");
        }
    }

    #[test]
    fn a_relative_path_is_made_absolute_and_one_with_a_nul_refused() {
        let made = wide_absolute(std::path::Path::new("backups")).unwrap();
        let made = String::from_utf16(made.strip_suffix(&[0]).unwrap()).unwrap();
        assert!(std::path::Path::new(&made).is_absolute(), "{made}");
        assert!(made.ends_with(r"\backups"));
        // A verbatim path is taken as absolute as it is, NUL and all.
        for nul in ["a\0b", "\\\\?\\C:\\a\0b"] {
            assert_eq!(
                wide_absolute(std::path::Path::new(nul)).unwrap_err().kind(),
                std::io::ErrorKind::InvalidInput,
                "{nul:?}"
            );
        }
    }
}
