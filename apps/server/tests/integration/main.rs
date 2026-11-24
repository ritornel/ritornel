//! Integration tests for `ritornel-server`.
//!
//! Kept in one integration test binary so Cargo links it once rather than
//! once per file.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod support;

mod contract;
mod launch;
mod link;
mod lists;
mod pages;
mod playback;
mod process;
mod ticket;
mod watch;
