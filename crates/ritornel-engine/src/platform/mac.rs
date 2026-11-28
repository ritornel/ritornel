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

/// Ejects the drive that holds `folder`, as Finder's Eject does, and
/// answers once the system has let go of it. The volume is found as the
/// outermost folder above `folder` on the same device; the one the system
/// itself is on is never ejected.
pub(crate) fn eject(folder: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    let refuse = |why: &str| std::io::Error::new(std::io::ErrorKind::Unsupported, why.to_owned());
    let folder = std::fs::canonicalize(folder)?;
    let device = std::fs::metadata(&folder)?.dev();
    if std::fs::metadata("/")?.dev() == device {
        return Err(refuse(
            "this folder is on the system's own disk, which is not ejected",
        ));
    }
    let mut volume = folder.as_path();
    while let Some(above) = volume.parent() {
        if std::fs::metadata(above)?.dev() != device {
            break;
        }
        volume = above;
    }
    let done = std::process::Command::new("/usr/sbin/diskutil")
        .arg("eject")
        .arg(volume)
        .stdin(std::process::Stdio::null())
        .output()?;
    if done.status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(
            String::from_utf8_lossy(&done.stderr).trim().to_owned(),
        ))
    }
}
