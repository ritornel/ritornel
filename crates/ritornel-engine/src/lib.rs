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
//! - The [`playback`] module: the player, its queue and session, the
//!   listen recorder and the audio pipeline, and the outbox that delivers
//!   each listen to the history ingest call in `history_store`.
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

mod artist_pictures;
pub mod backup;
mod clock;
pub mod covers;
mod delivery_store;
mod entropy;
mod error;
mod export;
mod history_facts;
mod history_pages;
#[cfg(test)]
mod history_size;
mod history_store;
mod history_stream;
mod id;
mod identity_store;
#[cfg(test)]
mod identity_tests;
mod import_store;
pub mod invent;
mod jobs;
mod library_store;
mod list;
mod media;
#[cfg(test)]
mod media_fixtures;
mod migrate;
mod platform;
pub mod playback;
mod ratings;
mod record_health;
mod resolve_store;
mod scan;
mod scan_store;
#[cfg(test)]
mod scan_tests;
#[cfg(test)]
mod schema_snapshot;
mod secret;
mod server_store;
mod service_session;
mod sqlite;
mod stats;
#[cfg(test)]
mod testing;
mod timekeeping;
mod walk;

pub use artist_pictures::{
    KnownPictureFolder, LastPictureImport, PictureFolder, PicturesImported, check_picture_folder,
};
pub use clock::{Clock, SystemClock};
pub use delivery_store::{
    DELIVERY_METHOD, Due, ELIGIBILITY_VERSION, Playing, SEND_WITHIN_MS, Settled,
};
pub use entropy::{Entropy, OsEntropy};
pub use error::Error;
pub use export::{EXPORT_FORMAT, EXPORT_VERSION, Exported};
pub use history_pages::Today;
pub use history_store::{
    INGEST_METHOD, IngestOutcome, Ingested, LISTEN_RECORD_VERSION, ListenRecord, ListenRow,
    Observations, PlayCursor, PlayRow, Rebuilt, SendStamps, ZonePeriodRow,
};
pub use id::new_id;
pub use identity_store::is_policy;
pub use import_store::{
    Doubtful, ImportReport, LOVES_METHOD, LoveRow, MATCH_METHOD, SERVICES, ServiceAccount,
    TrackDetail,
};
pub use jobs::{Job, JobState, NewJob, Progress, step_command};
pub use library_store::{
    Client, Command, CommitHook, Committed, Fingerprint, LibraryMeta, LibraryStore, SubjectKind,
    WriteRequest, Written,
};
pub use list::{ListError, View};
pub use ratings::Said;
pub use record_health::{Failing, LostScrobbles};
pub use resolve_store::Resolved;
/// What a server, a session and every stored entity is named by. The type
/// is the contract's.
pub use ritornel_contract::Id;
/// The wall clock in a zone at an instant: what a local date is read with.
pub use ritornel_domain::history::{LocalTime, known_zone, local_time, start_of_day_ms};
pub use scan::{ScanRun, ScanSettings, run_scan, scan_summary};
pub use scan_store::Source;
pub use secret::SessionToken;
pub use server_store::{BackupFailed, BackupTaken, Role, ServerIdentity, ServerStore};
pub use service_session::{ServiceKey, ServiceSession};
pub use sqlite::SQLITE_VERSION;
pub use timekeeping::{SystemTimekeeper, Timekeeper, clock_boot};
pub use walk::folder_id;
