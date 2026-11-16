//! A fixed clock and a predictable source of bytes, for the tests.

use std::sync::atomic::{AtomicI64, AtomicU8, Ordering};

use crate::{Clock, Entropy, Error};

/// A clock that says what it was last told.
#[derive(Debug)]
pub(crate) struct FixedClock(AtomicI64);

impl FixedClock {
    pub(crate) fn at(now_ms: i64) -> Self {
        Self(AtomicI64::new(now_ms))
    }

    pub(crate) fn set(&self, now_ms: i64) {
        self.0.store(now_ms, Ordering::SeqCst);
    }
}

impl Clock for FixedClock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// Bytes that count up from 0 and wrap, so every run draws the same ones.
#[derive(Debug, Default)]
pub(crate) struct CountingEntropy(AtomicU8);

impl Entropy for CountingEntropy {
    fn fill(&self, bytes: &mut [u8]) -> Result<(), Error> {
        for byte in bytes {
            *byte = self.0.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    }
}

/// A directory that is removed when the test ends.
#[derive(Debug)]
pub(crate) struct Scratch(std::path::PathBuf);

impl Scratch {
    pub(crate) fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "ritornel-engine-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    pub(crate) fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
