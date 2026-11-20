//! Linux: a Unix, whose monotonic clock that counts through sleep is
//! `CLOCK_BOOTTIME`, and whose boot ID is a file.

pub(crate) use super::unix::*;

use crate::Id;

/// Milliseconds on `CLOCK_BOOTTIME`, which only goes forward and counts
/// through sleep, as `CLOCK_MONOTONIC` does not on Linux.
pub(crate) fn mono_ms() -> i64 {
    super::unix::clock_ms(rustix::time::ClockId::Boottime)
}

/// The kernel's ID for this boot.
pub(crate) fn read_boot() -> Option<Id> {
    super::boot_id(&std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?)
}
