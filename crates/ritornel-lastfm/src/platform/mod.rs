//! Everything that differs from one system to another, behind one set of
//! plain functions. The rest of the plugin never names a system: there is
//! no `cfg` of an operating system outside this module.
//!
//! Unix has a file of its own (`unix.rs`). A system without one takes the
//! fallback whole.

#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix as system;

#[cfg(not(unix))]
use fallback as system;

#[cfg(test)]
pub(crate) use system::close_to_others;
pub(crate) use system::private;

/// The plain behaviour, for what a system has no code of its own for.
#[allow(dead_code, reason = "each system takes only what it lacks")]
mod fallback {
    use std::path::Path;

    /// Whether only the file's owner may read or write it. Nothing to
    /// check where there are no Unix modes.
    pub(crate) fn private(_path: &Path) -> bool {
        true
    }

    /// Makes the file at `path` one only its owner may read: nothing to do
    /// where there are no Unix modes.
    #[cfg(test)]
    pub(crate) fn close_to_others(_path: &Path) {}
}
