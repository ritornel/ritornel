//! Reading history: recent listening, "on this day", and the rebuild of
//! the derived tables. See "Queries M1 must answer" and "Rebuild" in
//! docs/design/history.md.
//!
//! History in M1 is meant to be read through `list.open` with `kind:
//! "history"` (docs/design/contract.md). Until list views are built, these
//! two direct calls read it, for `ritornelctl history`.

use serde::{Deserialize, Serialize};

use crate::{Id, Receipt};

/// What became of a play.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum PlayOutcome {
    /// It counts.
    Listened,
    /// It does not count, and the listener moved on.
    Skipped,
    /// It does not count, and the listener did not move on.
    Partial,
    /// Nothing says.
    Unknown,
}

/// What a play was of: its track as the library shows it. For a play whose
/// file is bound to no track yet, the fields are those of the track that
/// was queued, and the IDs are left out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct PlayedTrack {
    /// The appearance the play is bound to. Left out while the file that
    /// was heard is bound to no track.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub appearance_id: Option<Id>,
    /// The recording the play is bound to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub recording_id: Option<Id>,
    /// The title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub title: Option<String>,
    /// The artist credit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub artist: Option<String>,
    /// The album.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub album: Option<String>,
}

/// One listen behind a play.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct PlayedListen {
    /// The listen.
    pub listen_id: Id,
    /// When it began, by the playing server's clock.
    pub started_at: i64,
    /// When it ended, by the same clock.
    pub ended_at: i64,
    /// Why it began: `user_pick`, `auto_advance`, `repeat` or `resume`.
    pub start_reason: String,
    /// Why it ended: `finished`, `next`, `previous`, `jump`, `replaced`,
    /// `removed`, `stopped`, `error`, `output_lost`, `shutdown` or `crash`.
    pub end_reason: String,
    /// Where in the track it began.
    pub start_position_ms: u64,
    /// Where it ended.
    pub end_position_ms: u64,
    /// Audio actually rendered.
    pub ms_played: u64,
    /// The listen it continues after a restart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub continues: Option<Id>,
    /// The zone the playing server was in at its start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub zone: Option<String>,
}

/// One play.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct Play {
    /// The play. Its ID is computed, and a client that meets one it does
    /// not know reads again.
    pub play_id: Id,
    /// When it began. Left out for a play with no date.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub at: Option<i64>,
    /// Its local date, as `YYYY-MM-DD`, in `zone`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub local_date: Option<String>,
    /// Its local hour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub local_hour: Option<u32>,
    /// The zone its local date is in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub zone: Option<String>,
    /// That zone's offset from UTC then, in minutes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub utc_offset_minutes: Option<i32>,
    /// Which clock its time rests on: `hub_age` or `server_wall` for a
    /// listen.
    pub clock_basis: String,
    /// What it was of.
    pub track: PlayedTrack,
    /// Audio rendered, over every listen of it.
    pub ms_played: u64,
    /// The track's length.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub duration_ms: Option<u64>,
    /// What became of it.
    pub outcome: PlayOutcome,
    /// Whether it counts, by the play rule.
    pub counts: bool,
    /// Whether it is excluded.
    pub excluded: bool,
    /// The listens behind it, the first first.
    pub listens: Vec<PlayedListen>,
}

/// Where a page of `history.recent` ended: its last play's instant and
/// ID. The next page is asked for with it, and begins with the play after
/// that one, even one of the same instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct PlayCursor {
    /// The play's instant. Left out for a play with no date.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub at: Option<i64>,
    /// The play's ID.
    pub play_id: Id,
}

/// The params of `history.recent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct HistoryRecentParams {
    /// How many plays, at most 500. The default is 20.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub limit: Option<u32>,
    /// Only plays after this one, newest first: the `next` of the page
    /// before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub before: Option<PlayCursor>,
}

/// The result of `history.recent`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct RecentPlays {
    /// The plays, newest first.
    pub plays: Vec<Play>,
    /// Where the next page begins, when this one was full.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub next: Option<PlayCursor>,
    /// Listens this server has finished and not yet delivered to history.
    pub undelivered: u64,
}

/// The params of `history.onThisDay`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct OnThisDayParams {
    /// The day, as `MM-DD`. The default is today, where the server is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub day: Option<String>,
}

/// The result of `history.onThisDay`: the plays that count on that day of
/// any year, newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct OnThisDay {
    /// The day, as `MM-DD`.
    pub day: String,
    /// The plays.
    pub plays: Vec<Play>,
}

/// The params of `history.rebuild`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct HistoryRebuildParams {
    /// The command's ID, made by the client.
    pub idempotency_key: Id,
    /// Say what would change, and change nothing.
    #[serde(default)]
    pub dry_run: bool,
}

/// The result of `history.rebuild`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct HistoryRebuilt {
    /// The receipt.
    #[serde(flatten)]
    pub receipt: Receipt,
    /// Whether nothing was changed.
    pub dry_run: bool,
    /// Plays before.
    pub before: u64,
    /// Plays after.
    pub after: u64,
    /// Plays that are new, gone or different.
    pub changed: u64,
}
