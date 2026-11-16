//! The player's calls and what they carry: what is playing, the transport,
//! and the queue. See "Playback and jobs" and decisions 13, 14 and 16 in
//! docs/design/contract.md.
//!
//! Playback commands are safe to retry by construction: there is no
//! toggle, a position is absolute, and a skip or a seek names the entry it
//! acts on, so one sent late changes nothing once the player has moved on.

use serde::{Deserialize, Serialize};

use crate::{Id, Receipt};

/// What the player is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum PlaybackStatus {
    /// Nothing is playing.
    Stopped,
    /// The current entry is playing.
    Playing,
    /// The current entry is paused.
    Paused,
    /// Playing, and waiting for audio.
    Buffering,
}

/// What a track shows: one appearance's displayed fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct TrackSummary {
    /// The track appearance.
    pub appearance_id: Id,
    /// Its recording.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub recording_id: Option<Id>,
    /// Its release. Left out for a loose file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub release_id: Option<Id>,
    /// The title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub title: Option<String>,
    /// The artist credit, as one string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub artist: Option<String>,
    /// The album's title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub album: Option<String>,
    /// The disc number.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub disc_number: Option<u32>,
    /// The track number.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub track_number: Option<u32>,
    /// The length.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub duration_ms: Option<u64>,
}

/// The copy the player has open for the current entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct CopyInUse {
    /// The server the copy is on.
    pub server_id: Id,
    /// What it is to the track: `master` in M1.
    pub role: String,
    /// Its codec.
    pub codec: String,
    /// Its sample rate.
    pub sample_rate_hz: u32,
    /// Its channels.
    pub channels: u32,
    /// Its bit depth, where the codec has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub bit_depth: Option<u32>,
}

/// How an output runs. See decision 5 in docs/design/playback.md.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum OutputMode {
    /// One stream at the device's rate, shared with other programs; a track
    /// at another rate is resampled.
    Shared,
}

/// The output the player renders through.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct OutputInfo {
    /// Its name: the device's, or `null` for the output that makes no
    /// sound.
    pub name: String,
    /// How it runs.
    pub mode: OutputMode,
    /// Its rate.
    pub sample_rate_hz: u32,
    /// Its channels.
    pub channels: u32,
}

/// Repeat. M1's first player has none: it is always `off`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum Repeat {
    /// The queue plays once.
    Off,
    /// The queue starts again at its end.
    All,
    /// The entry starts again at its end.
    One,
}

/// The player's state, whole: the result of `playback.get` and what a
/// `playback` event carries.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct PlaybackState {
    /// Counts the changes to this state. A client keeps the copy with the
    /// highest.
    pub revision: u64,
    /// What the player is doing.
    pub status: PlaybackStatus,
    /// The entry it is on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub entry_id: Option<Id>,
    /// That entry's place in the queue, from 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub index: Option<u32>,
    /// What that entry's track shows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub track: Option<TrackSummary>,
    /// The copy open for it, while one is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub copy: Option<CopyInUse>,
    /// Where in the track.
    pub position_ms: u64,
    /// How long the track is, when that is known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub duration_ms: Option<u64>,
    /// The player's own gain, 0 to 1. Always 1 in M1's first player.
    pub volume: f64,
    /// Whether it is muted.
    pub muted: bool,
    /// Whether the queue plays in a shuffled order.
    pub shuffle: bool,
    /// Repeat.
    pub repeat: Repeat,
    /// The output.
    pub output: OutputInfo,
    /// The queue's revision.
    pub queue_revision: u64,
    /// How many entries the queue has.
    pub queue_length: u64,
}

/// Why tracks were queued: a kind, and what it refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum ContextKind {
    /// By hand.
    Manual,
    /// As an album.
    Album,
    /// From an artist's page.
    Artist,
    /// From a playlist.
    Playlist,
    /// From a list query.
    Query,
    /// By an agent.
    Agent,
}

/// Why tracks were queued, which every listen of them records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct PlayContext {
    /// The kind.
    pub kind: ContextKind,
    /// What it refers to: the release, the artist, the playlist, the stored
    /// definition or the agent's session.
    #[serde(rename = "ref", default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub reference: Option<Id>,
}

/// What `playback.play` plays.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PlaySource {
    /// These track appearances, in this order.
    Tracks {
        /// The appearances.
        ids: Vec<Id>,
    },
    /// A release, in disc and track order.
    Release {
        /// The release.
        id: Id,
    },
}

/// The params of `playback.play`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct PlaybackPlayParams {
    /// The command's ID, made by the client.
    pub idempotency_key: Id,
    /// What to play. It replaces the queue.
    pub source: PlaySource,
    /// The track to start at, from 0. The default is the first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub start: Option<u32>,
    /// Why it is played. With none, a release is played as an album and
    /// tracks by hand.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub context: Option<PlayContext>,
}

/// The params of `playback.pause`, `playback.resume` and `playback.stop`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct PlaybackCommandParams {
    /// The command's ID, made by the client.
    pub idempotency_key: Id,
}

/// The params of `playback.seek`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct PlaybackSeekParams {
    /// The command's ID, made by the client.
    pub idempotency_key: Id,
    /// The entry to seek in. If the player has left it, nothing changes.
    pub entry_id: Id,
    /// Where to.
    pub position_ms: u64,
}

/// Which way a skip goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum SkipDirection {
    /// To the next entry.
    Next,
    /// To the previous entry, or to the start of this one when it is more
    /// than three seconds in.
    Previous,
}

/// The params of `playback.skip`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct PlaybackSkipParams {
    /// The command's ID, made by the client.
    pub idempotency_key: Id,
    /// The entry to skip from. If the player has left it, nothing changes,
    /// so a skip sent again cannot skip twice.
    pub from_entry_id: Id,
    /// Which way.
    pub direction: SkipDirection,
}

/// What a playback command returns: the receipt, and where the player is
/// after it. The whole state is read with `playback.get` or sent on the
/// `playback` topic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct PlaybackChanged {
    /// The receipt.
    #[serde(flatten)]
    pub receipt: Receipt,
    /// The queue's revision after the command.
    pub queue_revision: u64,
    /// The entry the player is on after it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub entry_id: Option<Id>,
}

/// The params of `queue.window`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct QueueWindowParams {
    /// The first entry, from 0.
    #[serde(default)]
    pub offset: u32,
    /// How many entries, at most 500. The default is 100.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub limit: Option<u32>,
}

/// One entry of the queue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct QueueEntry {
    /// The entry.
    pub entry_id: Id,
    /// Its track. Its display fields are left out when the library no
    /// longer has the appearance.
    pub track: TrackSummary,
    /// Why it could not be played, once it could not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub failed_reason: Option<String>,
}

/// The result of `queue.window`. A `queueRevision` other than the one a
/// client read before means it reads again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct QueueWindow {
    /// The queue's revision.
    pub queue_revision: u64,
    /// How many entries it has.
    pub total: u64,
    /// The place of the entry the player is on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub current_index: Option<u32>,
    /// The entries asked for, in queue order.
    pub entries: Vec<QueueEntry>,
}

/// What an event on the `playback` topic carries: the state, whole.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct PlaybackEvent {
    /// The state.
    pub state: PlaybackState,
}

/// What an event on the `progress` topic carries: where the playing entry
/// is, about once a second and at once on a change. The client carries
/// the position on from when the event arrived.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ProgressEvent {
    /// The entry.
    pub entry_id: Id,
    /// Where in it.
    pub position_ms: u64,
    /// Whether it is playing.
    pub playing: bool,
}

/// What an event on the `queue` topic carries: a hint to read the queue
/// again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct QueueEvent {
    /// The queue's revision after the change.
    pub queue_revision: u64,
}

/// The params of `sources.tracks`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SourcesTracksParams {
    /// An absolute path on the server: a folder in one of its roots, or a
    /// file.
    pub path: String,
}

/// The result of `sources.tracks`: the tracks of the files there, in the
/// order they are played: album by album, each in disc and track order. A
/// file the library has not bound to a track yet is left out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SourceTracks {
    /// The tracks.
    pub tracks: Vec<TrackSummary>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::ReceiptStatus;

    #[test]
    fn a_source_is_tagged_and_a_context_names_what_it_refers_to_as_ref() {
        let params = PlaybackPlayParams {
            idempotency_key: Id::from_bytes([0xc0; 16]),
            source: PlaySource::Release {
                id: Id::from_bytes([0x7e; 16]),
            },
            start: Some(2),
            context: Some(PlayContext {
                kind: ContextKind::Agent,
                reference: Some(Id::from_bytes([0x5e; 16])),
            }),
        };
        let wire = json!({
            "idempotencyKey": "c0c0c0c0-c0c0-c0c0-c0c0-c0c0c0c0c0c0",
            "source": { "type": "release", "id": "7e7e7e7e-7e7e-7e7e-7e7e-7e7e7e7e7e7e" },
            "start": 2,
            "context": { "kind": "agent", "ref": "5e5e5e5e-5e5e-5e5e-5e5e-5e5e5e5e5e5e" },
        });
        assert_eq!(serde_json::to_value(&params).unwrap(), wire);
        assert_eq!(
            serde_json::from_value::<PlaybackPlayParams>(wire).unwrap(),
            params
        );
    }

    #[test]
    fn a_playback_command_returns_its_receipt_and_where_the_player_is() {
        let changed = PlaybackChanged {
            receipt: Receipt {
                command_id: Id::from_bytes([0xc1; 16]),
                status: ReceiptStatus::Done,
                revision: None,
            },
            queue_revision: 4,
            entry_id: None,
        };
        assert_eq!(
            serde_json::to_value(changed).unwrap(),
            json!({
                "commandId": "c1c1c1c1-c1c1-c1c1-c1c1-c1c1c1c1c1c1",
                "status": "done",
                "queueRevision": 4,
            })
        );
    }
}
