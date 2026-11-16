//! Both stores, migrations, writers, jobs, outbox, scanner, playback and the
//! sync module. A library with no listener: only `ritornel-server` links it.
//!
//! What is here so far is the two stores and the first job:
//!
//! - How a database file is opened ([`SQLITE_VERSION`], one writer thread,
//!   read-only readers), and the migration runner.
//! - The [`ServerStore`], which holds this server's identity, the local
//!   credential's session, where each named root is, and the jobs.
//! - The [`LibraryStore`], which creates and opens a library and is the one
//!   way to write it.
//! - The scan ([`ScanRun`]): it walks a root, reads what is new or changed
//!   without writing anything there, records it, and resolves the library
//!   with the pure resolver in `ritornel-domain`.
//!
//! Nothing reads the system clock or the system's random numbers directly.
//! A store is given a [`Clock`] and an [`Entropy`], and every time, ID, key
//! and token comes from those, so a test runs in memory with fixed ones.

// Tests may unwrap and index. Everything else is held to the workspace's
// lints, which CI denies.
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

mod clock;
mod entropy;
mod error;
mod id;
mod jobs;
mod library_store;
mod media;
#[cfg(test)]
mod media_fixtures;
mod migrate;
mod resolve_store;
mod scan;
mod scan_store;
#[cfg(test)]
mod scan_tests;
#[cfg(test)]
mod schema_snapshot;
mod secret;
mod server_store;
mod sqlite;
mod stats;
#[cfg(test)]
mod testing;
mod walk;

pub use clock::{Clock, SystemClock};
pub use entropy::{Entropy, OsEntropy};
pub use error::Error;
pub use id::new_id;
pub use jobs::{Job, JobState, NewJob, Progress, step_command};
pub use library_store::{
    Client, Command, Fingerprint, LibraryMeta, LibraryStore, SubjectKind, WriteRequest, Written,
};
pub use resolve_store::Resolved;
/// What a server, a session and every stored entity is named by. The type
/// is the contract's.
pub use ritornel_contract::Id;
pub use scan::{ScanRun, ScanSettings, run_scan, scan_summary};
pub use scan_store::Source;
pub use secret::SessionToken;
pub use server_store::{Role, ServerIdentity, ServerStore};
pub use sqlite::SQLITE_VERSION;
