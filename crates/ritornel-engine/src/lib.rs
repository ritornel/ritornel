//! Both stores, migrations, writers, jobs, outbox, scanner, playback and the
//! sync module. A library with no listener: only `ritornel-server` links it.
//!
//! What is here so far is `server.db`: how a database file is opened
//! ([`SQLITE_VERSION`], one writer thread, read-only readers), the migration
//! runner, and the [`ServerStore`] that holds this server's identity and the
//! local credential's session.
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

// Nothing here uses the domain yet. The import keeps the dependency in
// place, and Cargo's unused-dependency lint quiet, until something does.
use ritornel_domain as _;

mod clock;
mod entropy;
mod error;
mod id;
mod migrate;
#[cfg(test)]
mod schema_snapshot;
mod secret;
mod server_store;
mod sqlite;
#[cfg(test)]
mod testing;

pub use clock::{Clock, SystemClock};
pub use entropy::{Entropy, OsEntropy};
pub use error::Error;
pub use id::new_id;
/// What a server, a session and every stored entity is named by. The type
/// is the contract's.
pub use ritornel_contract::Id;
pub use secret::SessionToken;
pub use server_store::{Role, ServerIdentity, ServerStore};
pub use sqlite::SQLITE_VERSION;
