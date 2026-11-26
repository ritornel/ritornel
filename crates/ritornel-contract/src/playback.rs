//! The player's calls and what they carry: what is playing, the transport,
//! and the queue. See "Commands, playback, and jobs" in
//! docs/internals/contract.md.
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
    /// The cover of its release.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub art_id: Option<Id>,
    /// The listener's stars for its recording, 0 to 100, ten to the half
    /// star. Left out when it has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub rating: Option<u32>,
    /// Whether its recording is loved, here or at a service.
    #[serde(default)]
    pub loved: bool,
}

/// The copy the player has open for the current entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct CopyInUse {
    /// The server the copy is on.
    pub server_id: Id,
    /// What it is to the track: currently `master`.
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

/// How an output runs. See "Audio pipeline" in docs/internals/playback.md.
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
    /// What is wrong with it, while something is: it went away, or cannot
    /// be opened. The player waits, paused where it was, blaming no track,
    /// and Play tries the output again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub problem: Option<OutputProblem>,
}

/// What is wrong with the output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct OutputProblem {
    /// When it first failed, as Unix milliseconds.
    pub since: i64,
    /// What it said the last time. Display text only.
    pub detail: String,
}

/// Repeat. See "Queue and session" in docs/internals/playback.md.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum Repeat {
    /// The queue plays once.
    Off,
    /// The queue starts again at its end, in a new order when shuffled.
    All,
    /// The entry starts again at its end, as a new listen. Next still
    /// moves on.
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
    /// The player's own gain, 0 to 1.
    pub volume: f64,
    /// Whether it is muted.
    pub muted: bool,
    /// Whether the queue plays in its stored shuffled order.
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
    /// Every row of a list of songs this session holds a view of, in the
    /// view's order: play from here, over a list of any length.
    View {
        /// The view.
        view: String,
    },
}

/// The params of `playback.get`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct PlaybackGetParams {
    /// Target from `players.list`. Omit to use this server's own player.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub player: Option<Id>,
}

/// The params of `playback.play`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct PlaybackPlayParams {
    /// Target from `players.list`. Omit to use this server's own player.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub player: Option<Id>,
    /// The command's ID, made by the client.
    pub idempotency_key: Id,
    /// What to play. It replaces the queue.
    pub source: PlaySource,
    /// The track to start at, from 0. The default is the first, or with
    /// shuffle on, any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub start: Option<u32>,
    /// Why it is played. With none, a release is played as an album and
    /// tracks by hand.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub context: Option<PlayContext>,
    /// Shuffle on or off from now: on plays the start first and the rest
    /// in a new random order. Left out, the player's setting stays.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub shuffle: Option<bool>,
}

/// The params of `playback.jump`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct PlaybackJumpParams {
    /// Target from `players.list`. Omit to use this server's own player.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub player: Option<Id>,
    /// The command's ID, made by the client.
    pub idempotency_key: Id,
    /// The entry to play now, from its beginning.
    pub entry_id: Id,
}

/// The params of `playback.pause`, `playback.resume` and `playback.stop`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct PlaybackCommandParams {
    /// Target from `players.list`. Omit to use this server's own player.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub player: Option<Id>,
    /// The command's ID, made by the client.
    pub idempotency_key: Id,
}

/// The params of `playback.seek`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct PlaybackSeekParams {
    /// Target from `players.list`. Omit to use this server's own player.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub player: Option<Id>,
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
    /// Target from `players.list`. Omit to use this server's own player.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub player: Option<Id>,
    /// The command's ID, made by the client.
    pub idempotency_key: Id,
    /// The entry to skip from. If the player has left it, nothing changes,
    /// so a skip sent again cannot skip twice.
    pub from_entry_id: Id,
    /// Which way.
    pub direction: SkipDirection,
}

/// The params of `playback.set`: the player's settings, as absolute values.
/// One left out stays as it is.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct PlaybackSetParams {
    /// Target from `players.list`. Omit to use this server's own player.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub player: Option<Id>,
    /// The command's ID, made by the client.
    pub idempotency_key: Id,
    /// The player's own gain, 0 to 1. It is not the system's volume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub volume: Option<f64>,
    /// Whether it is muted. The volume is kept while it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub muted: Option<bool>,
    /// Shuffle. On writes a new random order with the current entry first;
    /// off goes back to queue order at the current entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub shuffle: Option<bool>,
    /// Repeat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub repeat: Option<Repeat>,
}

/// What a playback command returns: the receipt, and where the command put
/// the player. The whole state is read with `playback.get` or sent on the
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
    /// The entry the command put the player on. One that cannot be played
    /// is moved past at once, so the player may already be on a later
    /// entry: `playback.get` and the `playback` topic say where it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub entry_id: Option<Id>,
}

/// Which order `queue.window` reads the queue in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum QueueOrder {
    /// The order the entries were put in.
    Queue,
    /// The order they play in: the stored shuffled order while shuffle is
    /// on, queue order otherwise.
    Play,
    /// What plays after the current entry, in play order: Up next. The
    /// current entry is not among them.
    UpNext,
}

/// The params of `queue.window`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct QueueWindowParams {
    /// Target from `players.list`. Omit to use this server's own player.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub player: Option<Id>,
    /// The first entry, from 0.
    #[serde(default)]
    pub offset: u32,
    /// How many entries, at most 500. The default is 100.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub limit: Option<u32>,
    /// The order. The default is queue order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub order: Option<QueueOrder>,
}

/// Where `queue.add` puts what it adds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum QueuePlace {
    /// Straight after the current entry, in queue order and in play order.
    Next,
    /// At the end of the queue, and while shuffled at a random place among
    /// the entries still to play.
    End,
    /// Straight after this entry, in both orders.
    After {
        /// The entry.
        #[serde(rename = "entryId")]
        entry_id: Id,
    },
}

/// The params of `queue.add`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct QueueAddParams {
    /// Target from `players.list`. Omit to use this server's own player.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub player: Option<Id>,
    /// The command's ID, made by the client.
    pub idempotency_key: Id,
    /// What to add.
    pub source: PlaySource,
    /// Where.
    pub place: QueuePlace,
    /// Why it is queued. With none, a release is queued as an album and
    /// tracks by hand.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub context: Option<PlayContext>,
    /// The queue revision the edit was decided on. If the queue has moved
    /// past it, the edit is refused with `conflict` and changes nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub expected_revision: Option<u64>,
    /// Whether to play what was added, from the entry that plays first,
    /// if the queue was empty when the server made the edit. Left out, or
    /// with anything in the queue, the player stays as it is, and with
    /// nothing current waits, stopped, on that entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub play_if_empty: Option<bool>,
}

/// The params of `queue.remove`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct QueueRemoveParams {
    /// Target from `players.list`. Omit to use this server's own player.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub player: Option<Id>,
    /// The command's ID, made by the client.
    pub idempotency_key: Id,
    /// The entries. One the queue no longer has is passed over. Taking out
    /// the current entry moves the player to the next.
    pub entry_ids: Vec<Id>,
    /// As for `queue.add`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub expected_revision: Option<u64>,
}

/// The params of `queue.move`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct QueueMoveParams {
    /// Target from `players.list`. Omit to use this server's own player.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub player: Option<Id>,
    /// The command's ID, made by the client.
    pub idempotency_key: Id,
    /// The entries, in the order they are to have.
    pub entry_ids: Vec<Id>,
    /// Where they go: straight after the current entry, straight after
    /// another, or last, in both orders.
    pub place: QueuePlace,
    /// As for `queue.add`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub expected_revision: Option<u64>,
}

/// The params of `queue.clear`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct QueueClearParams {
    /// Target from `players.list`. Omit to use this server's own player.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub player: Option<Id>,
    /// The command's ID, made by the client.
    pub idempotency_key: Id,
    /// As for `queue.add`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub expected_revision: Option<u64>,
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
    /// How many entries there are in the order asked for.
    pub total: u64,
    /// The place of the entry the player is on, in the order asked for.
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
    /// The selected player. Omitted for this server's own player.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub player: Option<Id>,
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
    /// The selected player. Omitted for this server's own player.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub player: Option<Id>,
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
    /// The selected player. Omitted for this server's own player.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub player: Option<Id>,
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
            player: None,
            idempotency_key: Id::from_bytes([0xc0; 16]),
            source: PlaySource::Release {
                id: Id::from_bytes([0x7e; 16]),
            },
            start: Some(2),
            context: Some(PlayContext {
                kind: ContextKind::Agent,
                reference: Some(Id::from_bytes([0x5e; 16])),
            }),
            shuffle: None,
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
