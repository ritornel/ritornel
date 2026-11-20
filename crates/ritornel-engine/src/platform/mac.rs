//! macOS: a Unix, which names its boot through `sysctl`.

pub(crate) use super::unix::*;

use crate::Id;

/// Milliseconds on `CLOCK_MONOTONIC`, which only goes forward and on macOS
/// counts through sleep, as the standard library's `Instant` does not.
pub(crate) fn mono_ms() -> i64 {
    super::unix::clock_ms(rustix::time::ClockId::Monotonic)
}

/// The kernel's ID for this boot. macOS has no file for it, and reading the
/// `sysctl` that holds it from Rust takes `unsafe` or a crate for one
/// value, so the system's own `sysctl` tool is asked, once, which takes a
/// few milliseconds.
pub(crate) fn read_boot() -> Option<Id> {
    let done = std::process::Command::new("/usr/sbin/sysctl")
        .args(["-n", "kern.bootsessionuuid"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|done| done.status.success())?;
    super::boot_id(&String::from_utf8(done.stdout).ok()?)
}
