//! The time, as something a store is given.

use std::fmt::Debug;
use std::time::{SystemTime, UNIX_EPOCH};

/// Where a store gets the time.
pub trait Clock: Debug + Send + Sync {
    /// Now, as Unix milliseconds in UTC.
    fn now_ms(&self) -> i64;
}

/// The system clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    /// 0 if the clock is set before 1970.
    fn now_ms(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| {
                i64::try_from(since.as_millis()).unwrap_or(i64::MAX)
            })
    }
}
