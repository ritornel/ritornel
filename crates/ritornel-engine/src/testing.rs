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
        let path = tempfile::Builder::new()
            .prefix("ritornel-engine-test-")
            .tempdir()
            .unwrap()
            .keep();
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

/// Numbers drawn from a seed, the same on every run, for the stress tests.
#[derive(Debug)]
pub(crate) struct Draws(u64);

impl Draws {
    pub(crate) fn seeded(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1)
    }

    /// A number from 0 up to, not including, `n`.
    pub(crate) fn below(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d) % n.max(1)
    }
}

/// A WAV file of CD audio exactly `sectors` long, its samples made from
/// `seed` so that no two seeds share audio.
pub(crate) fn cd_wav(sectors: u32, seed: u32) -> Vec<u8> {
    let frames = sectors * 588;
    let data = frames * 4;
    let mut bytes = Vec::with_capacity(44 + data as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&44_100u32.to_le_bytes());
    bytes.extend_from_slice(&(44_100u32 * 4).to_le_bytes());
    bytes.extend_from_slice(&4u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data.to_le_bytes());
    for frame in 0..frames {
        let sample = (frame.wrapping_mul(seed.wrapping_mul(2_654_435_761)) >> 16) as u16;
        bytes.extend_from_slice(&sample.to_le_bytes());
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    bytes
}
