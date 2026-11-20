//! Everything that differs from one system to another, behind one set of
//! plain functions. The rest of the client never names a system: there is
//! no `cfg` of an operating system outside this module.
//!
//! Each system has a file of its own, and each offers the same names. The
//! client needs little: Unix (`unix.rs`) is one, and Windows
//! (`windows.rs`) the other.

#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix as system;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as system;

pub(crate) use system::start_without_console;
#[cfg(test)]
pub(crate) use system::{script, try_hold_as_a_server};
