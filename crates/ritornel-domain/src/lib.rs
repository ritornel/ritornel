//! Pure logic: the extractor, the resolver and the projector, and later
//! history derivation, the play rule and the planner. No I/O, no clock and
//! no SQLite.
//!
//! - [`payload`] is what reading one file gives, stored verbatim.
//! - [`extract`] turns a payload into claims: extractor 1.
//! - [`resolve`](mod@resolve) decides which recording each source item is:
//!   resolver policy 1.
//! - [`catalog`] forms releases, appearances, release groups and artists
//!   from the same claims and projects what each shows.
//! - [`library`] is what those are given and what they return.
//!
//! See docs/design/identity.md.

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

// Nothing here uses the contract yet. The import keeps the dependency in
// place, and Cargo's unused-dependency lint quiet, until something does.
use ritornel_contract as _;

pub mod catalog;
pub mod extract;
pub mod library;
pub mod normalize;
pub mod payload;
pub mod resolve;
#[cfg(test)]
mod tests;

pub use catalog::resolve;
pub use extract::{Claim, EXTRACTOR_VERSION, Facts, Scope, Source, extract};
pub use library::{Library, Resolution};
pub use normalize::NORMALIZER_VERSION;
pub use resolve::{Policy, RESOLVER_VERSION, Replacement, judge_replacements};
