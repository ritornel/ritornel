//! Syncing part of the library to a player or a folder. See
//! docs/internals/device-sync.md.
//!
//! The hub pushes, and trusts nothing a device reports. A pure planner
//! ([`plan()`]) compares what is wanted, what was seen and what the hub
//! recorded it wrote, and emits actions; what identifies a file on a device
//! is the hub's record, never a tag. The server carries a plan out through
//! a `ritornel-device` session, a window of actions at a time, and each
//! action is written down before the device is asked to do it.
//!
//! - [`Settings`] and [`FormatPolicy`] are what a target is set to, by
//!   value.
//! - [`TrackRow`] and [`SourceRow`] are what the library has and what a
//!   target's sources select of it.
//! - [`Plan`] is the preview of a target's next sync, and [`rows`] pages it.
//! - [`FolderSession`] is the folder family, built in behind the trait.
//! - The stores' `sync_*` methods are where every record is kept.

mod folder;
mod naming;
mod plan;
#[cfg(test)]
mod plan_tests;
mod policy;
pub mod rows;
mod select;
mod store;

pub use folder::{FolderSession, Overlap, overlap};
pub use plan::{
    Action, Baseline, Encodes, Make, Old, Plan, ProjectionKey, Seen, Shown, SourceTotals, Speed,
    plan,
};
pub use policy::{
    Blocked, FormatPolicy, Lossless, Preset, Sent, Settings, nominal_bytes, preset, tag_overhead,
};
pub use select::{Art, CopyRow, Matcher, SourceRow, TrackRow};
pub use store::{
    ActionRow, InDoubt, Issued, Left, Outcome, RUN_METHOD, Recorded, RunRow, Stored, TargetChanges,
    TargetRow,
};

/// The marker that makes a target's storage Ritornel's: which target it
/// is, which lifetime of its storage, and the root ID that lifetime was
/// started under. A marker that is gone, or names another root, ends the
/// lifetime: adopting again starts a new one, and everything there is then
/// a file Ritornel did not write.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Marker {
    /// The version of this file.
    pub v: u32,
    /// The target, as the library names it.
    pub target: crate::Id,
    /// The lifetime of its storage.
    pub epoch: i64,
    /// The root ID that lifetime was started under.
    pub root: crate::Id,
    /// The last run that finished, once one has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<crate::Id>,
}

impl Marker {
    /// The marker as it is written.
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec_pretty(self).unwrap_or_default()
    }

    /// Reads a marker: `None` for bytes that are not one.
    pub fn read(bytes: &[u8]) -> Option<Self> {
        serde_json::from_slice::<Self>(bytes)
            .ok()
            .filter(|marker| marker.v == 1)
    }
}

/// Ritornel's own identifier for a file it makes: one versioned value
/// naming the library, the master's source item and the track appearance.
/// It is written from the first sync as a recovery hint, for finding a file
/// again after a rename, and no plan, check or readback uses it.
pub fn ritornel_id(library: crate::Id, source_item: crate::Id, appearance: crate::Id) -> String {
    format!("1:{library}:{source_item}:{appearance}")
}
