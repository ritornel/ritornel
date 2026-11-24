//! Pure logic: the extractor, the resolver and the projector, and later
//! history derivation, the play rule and the planner. No I/O, no clock and
//! no SQLite.
//!
//! - [`payload`] is what reading one file gives, stored verbatim.
//! - [`extract`](mod@extract) turns a payload into claims.
//! - [`resolve`](mod@resolve) decides which recording each source item is,
//!   using the current file policy. Dry runs can also select earlier policies.
//! - [`catalog`] forms releases, appearances, release groups and artists
//!   from the same claims and projects what each shows. The disc numbers
//!   the policy's two release rules imply are in `discs`.
//! - [`library`] is what those are given and what they return.
//! - [`history`] derives plays from listens and judges them by the play
//!   rule (docs/internals/history.md).
//! - [`scrobble`] matches a play another service reported by its text to
//!   the library, or leaves it unbound.
//! - [`selection`] orders the copies of a track for the player
//!   (docs/internals/playback.md).
//! - [`streams`] tells which of Spotify's streams and Last.fm's scrobbles
//!   are one listen, and whether a stream with no scrobble counts.
//!
//! See docs/internals/identity.md.

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

pub mod catalog;
mod discs;
pub mod extract;
pub mod history;
#[cfg(test)]
mod history_tests;
pub mod layout;
pub mod library;
pub mod normalize;
pub mod payload;
pub mod propose;
pub mod reply;
pub mod resolve;
pub mod scrobble;
pub mod selection;
pub mod stream_report;
pub mod streams;
#[cfg(test)]
mod tests;
pub mod values;

pub use catalog::{CATALOG_VERSION, resolve};
pub use extract::{Claim, EXTRACTOR_VERSION, Facts, Scope, Source, extract};
pub use library::{Library, Resolution};
pub use normalize::NORMALIZER_VERSION;
pub use resolve::{Policy, RESOLVER_VERSION, Replacement, judge_replacements};
