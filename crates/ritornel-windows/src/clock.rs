//! The clock a listen is stamped with: one that every process on the
//! machine reads alike, and that counts through sleep and hibernation, as
//! `CLOCK_BOOTTIME` does on Linux.

use windows_sys::Win32::System::WindowsProgramming::QueryInterruptTimePrecise;

/// Milliseconds since the machine started, sleep included: the interrupt
/// time, read precisely. `GetTickCount64` counts the same, but only in
/// steps of the system's timer, 10 to 16 ms, which can give two listens
/// played one after the other the same instant. The unbiased interrupt
/// time would leave sleep out.
pub fn since_boot_ms() -> u64 {
    let mut hundreds_of_ns = 0_u64;
    // SAFETY: the call writes one u64 through the pointer, which is to a
    // local that outlives it, and cannot fail. It is in Windows 10 and
    // later.
    unsafe { QueryInterruptTimePrecise(&raw mut hundreds_of_ns) };
    hundreds_of_ns / 10_000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_clock_moves_forward() {
        let before = since_boot_ms();
        // sleep: a known time for the clock to move over; only a lower bound is
        // checked.
        std::thread::sleep(std::time::Duration::from_millis(50));
        let after = since_boot_ms();
        assert!(after >= before + 30, "{before} {after}");
    }

    #[test]
    fn the_clock_moves_in_milliseconds_not_timer_ticks() {
        // A timer tick is 10 to 16 ms. Sleeping 2 ms at a time, a precise
        // clock is seen to move by less than a tick at least once.
        let mut last = since_boot_ms();
        let mut smallest = u64::MAX;
        for _ in 0..50 {
            // sleep: steps between readings of the clock; only the smallest is
            // checked.
            std::thread::sleep(std::time::Duration::from_millis(2));
            let now = since_boot_ms();
            if now > last {
                smallest = smallest.min(now - last);
            }
            last = now;
        }
        assert!(smallest < 10, "the smallest step was {smallest} ms");
    }
}
