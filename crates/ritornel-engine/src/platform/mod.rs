//! Everything that differs from one system to another, behind one set of
//! plain functions. The rest of the engine never names a system: there is
//! no `cfg` of an operating system outside this module.
//!
//! Each system has a file of its own (`mac.rs`, `linux.rs`, `windows.rs`),
//! and each offers the same names. A Mac and Linux are Unix, and take what
//! they share from `unix.rs`; any other Unix takes it from `other_unix`.
//! The rest of `mod.rs` is pure: how each kernel writes the boot it is on,
//! read by every system's tests.

use crate::Id;

#[cfg(unix)]
mod unix;

#[cfg(target_os = "macos")]
mod mac;
#[cfg(target_os = "macos")]
use mac as system;

#[cfg(any(target_os = "linux", target_os = "android"))]
mod linux;
#[cfg(any(target_os = "linux", target_os = "android"))]
use linux as system;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as system;

#[cfg(all(
    unix,
    not(any(target_os = "macos", target_os = "linux", target_os = "android"))
))]
use other_unix as system;

pub(crate) use system::{
    Folder, file_id, met_a_link, mono_ms, open_folder, open_inside, read_at, read_boot,
};
#[cfg(test)]
pub(crate) use system::{link_file, link_folder, mode_of, seal, set_listable};

/// What a folder's listing says one of its entries is, without following
/// it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Folder,
    /// A symbolic link, or on Windows a junction too.
    Link,
    /// A file, or anything else that is not a folder.
    Other,
}

/// What the system says of a file in a folder, following a link to it:
/// whether it is a regular file, its size, and when it was last modified,
/// in milliseconds since the epoch and 0 before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Described {
    pub(crate) is_file: bool,
    pub(crate) len: u64,
    pub(crate) mtime_ms: i64,
}

/// Any other Unix: its monotonic clock, and no boot ID.
#[cfg(all(
    unix,
    not(any(target_os = "macos", target_os = "linux", target_os = "android"))
))]
mod other_unix {
    pub(crate) use super::unix::*;

    use crate::Id;

    pub(crate) fn mono_ms() -> i64 {
        super::unix::clock_ms(rustix::time::ClockId::Monotonic)
    }

    pub(crate) fn read_boot() -> Option<Id> {
        None
    }
}

/// A boot ID as the kernel writes it: a UUID, in either case.
#[cfg(any(target_os = "linux", target_os = "android", target_os = "macos", test))]
fn boot_id(text: &str) -> Option<Id> {
    text.trim().to_ascii_lowercase().parse().ok()
}

/// The boot ID made from a Windows machine's GUID and its count of boots.
#[cfg(any(windows, test))]
fn windows_boot_id(machine: &str, boot: &str) -> Id {
    let mut hasher = blake3::Hasher::new();
    for part in ["ritornel windows boot", machine.trim(), boot.trim()] {
        hasher.update(part.as_bytes());
        hasher.update(&[0]);
    }
    let hash = hasher.finalize();
    Id::from_bytes(hash.as_bytes().first_chunk().copied().unwrap_or_default())
}

/// The value on the line of `reg query`'s output that names `name`:
/// `    BootId    REG_DWORD    0x4d`.
#[cfg(any(windows, test))]
fn reg_line(text: &str, name: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let mut words = line.split_whitespace();
        (words.next() == Some(name)).then(|| words.last().map(str::to_owned))?
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    fn macos_linux_and_windows_name_the_boot() {
        assert!(read_boot().is_some());
    }

    #[test]
    fn a_boot_id_is_read_in_either_case_and_nothing_else_is_one() {
        let lower = boot_id("3b82e997-81c4-49bd-92d2-e45d57c83696\n").unwrap();
        assert_eq!(boot_id("3B82E997-81C4-49BD-92D2-E45D57C83696"), Some(lower));
        assert_eq!(boot_id(""), None);
        assert_eq!(boot_id("not a boot"), None);
    }

    #[test]
    fn a_windows_boot_is_its_machine_and_its_count_and_reg_is_read_by_name() {
        let text = "\r\nHKEY_LOCAL_MACHINE\\SYSTEM\r\n    BootId    REG_DWORD    0x4d\r\n\r\n";
        assert_eq!(reg_line(text, "BootId").as_deref(), Some("0x4d"));
        assert_eq!(reg_line(text, "Boot"), None);
        let machine = "6f3c0d0e-0000-4000-8000-000000000001";
        let this = windows_boot_id(machine, "0x4d");
        assert_eq!(windows_boot_id(machine, "0x4d\r\n"), this);
        assert_ne!(windows_boot_id(machine, "0x4e"), this);
        assert_ne!(
            windows_boot_id("6f3c0d0e-0000-4000-8000-000000000002", "0x4d"),
            this
        );
    }
}
