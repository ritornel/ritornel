//! Moving an existing queue and listen between players.

use crate::{Id, PlayContext, Repeat};
use serde::{Deserialize, Serialize};

/// A move of one current queue entry between two places from `players.list`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct PlaybackMoveParams {
    /// Retry this move using the same key.
    pub idempotency_key: Id,
    /// Player currently holding the queue.
    pub from: Id,
    /// Player that should receive the queue.
    pub to: Id,
    /// Stable ID of the current entry.
    pub entry_id: Id,
}

/// An entry in queue order. Shuffle positions preserve the separate play order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct HandoffEntry {
    /// Stable ID of the current entry.
    pub entry_id: Id,
    /// Track appearance to play.
    pub appearance_id: Id,
    /// How this track was queued.
    pub context: PlayContext,
    /// Position in the shuffled play order, when enabled.
    pub shuffle_position: Option<i64>,
}

/// The source at the instant it paused. The listen key follows the music.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct HandoffSnapshot {
    /// All entries in ordinary queue order.
    pub entries: Vec<HandoffEntry>,
    /// Stable ID of the current entry.
    pub entry_id: Id,
    /// Playback position within the current entry.
    pub position_ms: u64,
    /// Whether to start playback when the queue arrives.
    pub playing: bool,
    /// Whether to use the stored shuffle positions.
    pub shuffle: bool,
    /// Repeat mode to carry with the queue.
    pub repeat: Repeat,
    /// Source listen to continue when audio starts.
    pub listen: Option<Id>,
    /// Library listener for the continued listen.
    pub listener: Option<Id>,
    /// Source revision used to guard a delayed release.
    pub queue_revision: u64,
}

/// A request on the player's authenticated connection to its hub.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(tag = "step", rename_all = "camelCase", deny_unknown_fields)]
pub enum LinkHandoffParams {
    Take {
        movement: PlaybackMoveParams,
    },
    /// A probe without a snapshot returns the load receipt or retires this attempt.
    Load {
        movement: PlaybackMoveParams,
        attempt: Id,
        snapshot: Option<HandoffSnapshot>,
    },
    Release {
        movement: PlaybackMoveParams,
    },
}

/// The durable outcome of a step, or a load whose receipt is still absent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(tag = "step", rename_all = "camelCase")]
pub enum HandoffResult {
    /// The source is paused and still owns the queue.
    Taken { snapshot: HandoffSnapshot },
    /// The destination committed its queue and receipt.
    Loaded,
    /// The destination retired this load attempt without accepting it.
    NotLoaded,
    /// The source handled the release.
    Released,
}
