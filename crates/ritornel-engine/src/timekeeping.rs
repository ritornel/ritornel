//! The clocks a listen is stamped with besides the wall clock: a monotonic
//! clock that keeps counting while the machine sleeps, the boot it counts
//! from, and the zone the machine is in. None can be backfilled, so every
//! listen and every send records them. See decision 20 and "Clocks" in
//! docs/design/history.md.
//!
//! The monotonic clock is the machine's, and two readings of it compare
//! only within one boot of the machine. So the boot ID that goes with each
//! reading is the kernel's: a listen closed when the server stopped or
//! crashed, and sent by its next run, is still dated by its age as long as
//! the machine has not restarted in between. Where the kernel's boot ID
//! cannot be read, this run of the server stands in for it ([`clock_boot`]),
//! which is always correct and only less often of use: readings from two
//! runs never compare. A container reads its host's boot ID, whose clock it
//! shares.

use std::fmt::Debug;
use std::sync::OnceLock;

use crate::Id;

/// Where the player reads the monotonic clock and the zone.
pub trait Timekeeper: Debug + Send + Sync {
    /// Milliseconds on a clock that only goes forward and keeps counting
    /// while the machine sleeps.
    fn mono_ms(&self) -> i64;
    /// The boot that clock counts from: the machine's, as its kernel names
    /// it. `None` when the system does not say.
    fn boot(&self) -> Option<Id>;
    /// The IANA name of the zone the machine is in now. `None` when the
    /// system does not say.
    fn zone(&self) -> Option<String>;
}

/// The boot ID every listen and every send is stamped with: the machine's,
/// or this run of the server where the machine's cannot be read.
pub fn clock_boot(timekeeper: &dyn Timekeeper, run: Id) -> Id {
    timekeeper.boot().unwrap_or(run)
}

/// The system's clocks: `CLOCK_BOOTTIME` on Linux, `CLOCK_MONOTONIC` on
/// macOS and the precise interrupt time on Windows, all of which count
/// through sleep (the standard library's `Instant` stops on macOS), the
/// kernel's boot ID, and the system's zone.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemTimekeeper;

/// A boot ID as the kernel writes it: a UUID, in either case.
#[cfg(any(target_os = "linux", target_os = "android", target_os = "macos", test))]
fn boot_id(text: &str) -> Option<Id> {
    text.trim().to_ascii_lowercase().parse().ok()
}

/// The kernel's ID for this boot of the machine, read once.
fn kernel_boot() -> Option<Id> {
    static BOOT: OnceLock<Option<Id>> = OnceLock::new();
    *BOOT.get_or_init(read_boot)
}

/// On Linux the boot ID is a file.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn read_boot() -> Option<Id> {
    boot_id(&std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?)
}

/// macOS has no file for it, and reading the `sysctl` that holds it from
/// Rust takes `unsafe` or a crate for one value, so the system's own
/// `sysctl` tool is asked, once, which takes a few milliseconds.
#[cfg(target_os = "macos")]
fn read_boot() -> Option<Id> {
    let done = std::process::Command::new("/usr/sbin/sysctl")
        .args(["-n", "kern.bootsessionuuid"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|done| done.status.success())?;
    boot_id(&String::from_utf8(done.stdout).ok()?)
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    windows
)))]
fn read_boot() -> Option<Id> {
    None
}

/// Windows names no boot with a UUID. It counts them: `BootId` under the
/// prefetcher's parameters goes up by one at each start. With the machine's
/// own GUID, which tells this machine from another that has started as
/// often, it makes an ID that is the same for the whole of one boot and
/// another at the next. Both are asked of the system's `reg` tool, once, as
/// macOS's `sysctl` is.
#[cfg(windows)]
fn read_boot() -> Option<Id> {
    let boot = reg_value(
        r"HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Memory Management\PrefetchParameters",
        "BootId",
    )?;
    let machine = reg_value(r"HKLM\SOFTWARE\Microsoft\Cryptography", "MachineGuid")?;
    Some(windows_boot_id(&machine, &boot))
}

/// The boot ID made from the machine's GUID and its count of boots.
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

/// One value under a registry key, as `reg query` prints it: the last word
/// of the line that names it.
#[cfg(windows)]
fn reg_value(key: &str, name: &str) -> Option<String> {
    let done = std::process::Command::new("reg")
        .args(["query", key, "/v", name])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|done| done.status.success())?;
    let text = String::from_utf8(done.stdout).ok()?;
    reg_line(&text, name)
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

impl Timekeeper for SystemTimekeeper {
    #[cfg(windows)]
    fn mono_ms(&self) -> i64 {
        i64::try_from(ritornel_windows::clock::since_boot_ms()).unwrap_or(i64::MAX)
    }

    #[cfg(not(windows))]
    fn mono_ms(&self) -> i64 {
        #[cfg(any(target_os = "linux", target_os = "android"))]
        let clock = rustix::time::ClockId::Boottime;
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        let clock = rustix::time::ClockId::Monotonic;
        let now = rustix::time::clock_gettime(clock);
        now.tv_sec
            .saturating_mul(1000)
            .saturating_add(now.tv_nsec / 1_000_000)
    }

    fn boot(&self) -> Option<Id> {
        kernel_boot()
    }

    fn zone(&self) -> Option<String> {
        jiff::tz::TimeZone::try_system()
            .ok()
            .and_then(|zone| zone.iana_name().map(str::to_owned))
    }
}

#[cfg(test)]
pub(crate) mod fixed {
    //! A timekeeper whose clock moves when the test moves it.

    use std::sync::Mutex;
    use std::sync::atomic::{AtomicI64, Ordering};

    use super::Timekeeper;
    use crate::Id;

    /// A monotonic clock, the boot it counts from, and a zone, all set by
    /// the test.
    #[derive(Debug)]
    pub(crate) struct FixedTimekeeper {
        mono: AtomicI64,
        boot: Mutex<Option<Id>>,
        zone: Mutex<Option<String>>,
    }

    impl FixedTimekeeper {
        pub(crate) fn new(mono_ms: i64, zone: Option<&str>) -> Self {
            Self {
                mono: AtomicI64::new(mono_ms),
                boot: Mutex::new(Some(Id::from_bytes([0xba; 16]))),
                zone: Mutex::new(zone.map(str::to_owned)),
            }
        }

        pub(crate) fn advance(&self, ms: i64) {
            self.mono.fetch_add(ms, Ordering::SeqCst);
        }

        /// The machine restarts: a new boot, and its clock starts again.
        pub(crate) fn reboot(&self, boot: Option<Id>, mono_ms: i64) {
            *self.boot.lock().unwrap() = boot;
            self.mono.store(mono_ms, Ordering::SeqCst);
        }

        pub(crate) fn set_zone(&self, zone: Option<&str>) {
            *self.zone.lock().unwrap() = zone.map(str::to_owned);
        }
    }

    impl Timekeeper for FixedTimekeeper {
        fn mono_ms(&self) -> i64 {
            self.mono.load(Ordering::SeqCst)
        }

        fn boot(&self) -> Option<Id> {
            *self.boot.lock().unwrap()
        }

        fn zone(&self) -> Option<String> {
            self.zone.lock().unwrap().clone()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_system_monotonic_clock_goes_forward_and_the_zone_is_a_name_or_nothing() {
        let keeper = SystemTimekeeper;
        let (first, second) = (keeper.mono_ms(), keeper.mono_ms());
        assert!(first > 0 && second >= first);
        if let Some(zone) = keeper.zone() {
            assert!(!zone.is_empty());
        }
    }

    #[test]
    fn the_kernels_boot_id_is_read_on_macos_linux_and_windows_and_is_the_same_each_time() {
        let keeper = SystemTimekeeper;
        let boot = keeper.boot();
        if cfg!(any(target_os = "linux", target_os = "macos", windows)) {
            assert!(boot.is_some());
        }
        assert_eq!(keeper.boot(), boot);
        let run = Id::from_bytes([0x5e; 16]);
        assert_eq!(clock_boot(&keeper, run), boot.unwrap_or(run));
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
