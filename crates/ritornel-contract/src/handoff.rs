//! Moving an existing queue and listen between players.

use crate::{Id, PlayContext, Repeat};
use serde::{Deserialize, Serialize};

/// A move of one current queue entry between two places from `players.list`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct PlaybackMoveParams {
    pub idempotency_key: Id,
    pub from: Id,
    pub to: Id,
    pub entry_id: Id,
}

/// An entry in queue order. Shuffle positions preserve the separate play order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct HandoffEntry {
    pub entry_id: Id,
    pub appearance_id: Id,
    pub context: PlayContext,
    pub shuffle_position: Option<i64>,
}

/// The source at the instant it paused. The listen key follows the music.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct HandoffSnapshot {
    pub entries: Vec<HandoffEntry>,
    pub entry_id: Id,
    pub position_ms: i64,
    pub playing: bool,
    pub shuffle: bool,
    pub repeat: Repeat,
    pub listen: Option<Id>,
    pub listener: Option<Id>,
    pub queue_revision: i64,
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
    Taken { snapshot: HandoffSnapshot },
    Loaded,
    Pending,
    Released,
}
