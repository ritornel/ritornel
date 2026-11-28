//! Integration tests for `ritornelctl` against the real `ritornel-server`.
//!
//! Kept in one integration test binary so Cargo links it once rather than
//! once per file. `ritornel-server` belongs to another package and is taken
//! from beside `ritornelctl`, where a workspace build (`mise run test`)
//! puts it.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod support;

mod backup;
mod devices;
mod lastfm;
mod play;
mod scan;
mod spotify;
mod status;
mod transcode;
