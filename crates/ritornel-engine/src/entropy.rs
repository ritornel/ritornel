//! Random bytes, as something a store is given.

use std::fmt::Debug;

use crate::Error;

/// Where a store gets random bytes: for the random part of an ID, for a
/// signing key and for a session token.
pub trait Entropy: Debug + Send + Sync {
    /// Fills `bytes` with random bytes that nobody can predict.
    fn fill(&self, bytes: &mut [u8]) -> Result<(), Error>;
}

/// The operating system's random number generator.
#[derive(Debug, Clone, Copy, Default)]
pub struct OsEntropy;

impl Entropy for OsEntropy {
    fn fill(&self, bytes: &mut [u8]) -> Result<(), Error> {
        getrandom::fill(bytes).map_err(Error::Entropy)
    }
}
