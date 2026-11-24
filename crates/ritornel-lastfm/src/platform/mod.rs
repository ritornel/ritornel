//! Everything that differs from one system to another, behind one set of
//! plain functions. The rest of the plugin never names a system: there is
//! no `cfg` of an operating system outside this module.

#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix as system;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as system;

pub(crate) use system::{HOW_TO_CLOSE, private};
#[cfg(test)]
pub(crate) use system::{close_to_others, open_to_others};
