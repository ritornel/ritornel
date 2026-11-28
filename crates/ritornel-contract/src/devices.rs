//! Syncing part of the library to a portable player or a folder. See
//! docs/internals/device-sync.md.
//!
//! A target is a player or a folder the hub keeps a chosen part of the
//! library on. The hub plans, makes every file and keeps every record;
//! the server a target is attached to only touches it. The types are
//! `SyncDevice…` because [`DeviceKind`](crate::DeviceKind) already names
//! what a listen played on.
//!
//! - `devices.list` answers a [`SyncDevicesList`]: every target, for the
//!   column of device tabs.
//! - `devices.get` takes a [`SyncDeviceParams`] and answers a
//!   [`SyncDeviceDetail`]: one target's page, with the plan of its next
//!   sync and the run under way or last finished.
//! - `devices.formats` takes a [`SyncDeviceParams`] and answers
//!   [`SyncFormats`]: what the content would take in each format on offer.
//! - `devices.actions` takes a [`SyncActionsParams`] and answers a
//!   [`SyncActions`]: the rows of a plan or of a run, paged.
//! - `devices.unmanaged` and `devices.plays` page the files Ritornel did
//!   not write and the plays read back.
//! - `devices.set`, `devices.addFolder`, `devices.sync`, `devices.eject`
//!   and `devices.forget` are the commands. A sync is a job, and stopping
//!   one is `jobs.cancel`.
//!
//! Nothing here carries a delta. The [`SyncDevicesEvent`] is a hint to
//! read `devices.get` again, and while a run is under way it carries the
//! run whole.

use serde::{Deserialize, Serialize};

use crate::Id;

/// What a target is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum SyncDeviceKind {
    /// A portable player: loaded when plugged in, and its plays come back.
    Player,
    /// A folder on a server's disk, or on a card in a reader.
    Folder,
}

/// How a target's storage is reached, which decides what names a file on
/// it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum SyncTransport {
    /// A directory. A file is named by its path under the root.
    Folder,
    /// A mounted volume, as a Rockbox player is. A file is named by its
    /// path under the mount.
    MassStorage,
    /// The Media Transfer Protocol, as a Zune speaks. A file is named by
    /// the device's persistent object ID.
    Mtp,
}

/// What a folder target is made for. It supplies the formats the folder is
/// taken to play, the cover's size and the kind of playlist, each of which
/// can then be changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum SyncMadeFor {
    /// A car stereo that reads a card or a stick: MP3, small covers.
    CarStereo,
    /// A phone.
    Phone,
    /// Some other player that reads files.
    OtherPlayer,
    /// A plain copy of the music, for nothing in particular.
    PlainCopy,
}

/// Whether a folder target comes and goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum SyncFolderKind {
    /// A drive or a card: synced when it is plugged in, and ejected. Its
    /// room is the drive's.
    Removable,
    /// A folder that is always there, such as one another program carries
    /// onward. It is never ejected, and its room is a limit the person
    /// sets.
    AlwaysHere,
}

/// The kind of playlist a folder target is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum SyncPlaylistKind {
    /// `.m3u`, in the system's old encoding, as older players read.
    M3u,
    /// `.m3u8`, in UTF-8.
    M3u8,
}

/// What is particular to a folder target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SyncFolder {
    /// What it is made for.
    pub made_for: SyncMadeFor,
    /// Whether it comes and goes.
    pub kind: SyncFolderKind,
    /// The kind of playlist it is written.
    pub playlists: SyncPlaylistKind,
}

/// Whether a target is Ritornel's yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum SyncSetup {
    /// Seen, read, and not written to. Its first sync sets it up.
    New,
    /// Left alone on purpose.
    Ignored,
    /// Set up: Ritornel manages the files it wrote there.
    Ready,
}

/// What Ritornel is doing with a target now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum SyncActivity {
    /// Nothing.
    Idle,
    /// Reading what is on it. Nothing is written.
    Reading,
    /// A sync is under way.
    Syncing,
    /// Being made safe to unplug.
    Ejecting,
}

/// Whether a target waits on the person, or something went wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum SyncAttentionLevel {
    /// Neither.
    None,
    /// It waits on a choice, and nothing is lost yet.
    NeedsYou,
    /// Something failed.
    Failed,
}

/// What a target needs, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SyncAttention {
    /// The worst of its reasons.
    pub level: SyncAttentionLevel,
    /// Each reason as a word a client has text for, such as `does_not_fit`,
    /// `conflicts`, `files_failed`, `cut_off` or `root_unbound`.
    pub reasons: Vec<String>,
}

/// The drawing a client shows for a target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SyncDeviceIcon {
    /// The key chosen for this target, or none for its family's.
    pub key: Option<String>,
    /// The family's own key for this model: a short lowercase word path
    /// such as `folder` or `player.ipod_video.white`. The drawings ship
    /// with each client, and a key a client does not know falls back to
    /// one it does.
    pub default: String,
}

/// Whether a target is here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SyncConnection {
    /// Whether it can be read and written now.
    pub attached: bool,
    /// The server it is attached to, or was last.
    pub host_server_id: Option<Id>,
    /// Since when it has been attached, or away.
    pub since_ms: Option<i64>,
    /// Why it cannot be used though it is known, as a word: `root_missing`
    /// for a folder that is gone, `root_unbound` for one this hub has no
    /// path for, `not_mounted` for a volume that never mounted.
    pub problem: Option<String>,
}

/// How a sync ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum SyncOutcome {
    /// Everything planned was done.
    Complete,
    /// The device left, or the server stopped, part way.
    CutOff,
    /// It finished, and some files failed.
    FailedInPart,
    /// It could not run.
    Failed,
    /// The person stopped it.
    Stopped,
}

/// When a target last synced, and how that ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SyncLastSync {
    /// When it ended.
    pub at_ms: i64,
    /// How.
    pub outcome: SyncOutcome,
    /// How many files it copied.
    pub copied: u64,
    /// How many it removed.
    pub removed: u64,
    /// How many failed.
    pub failed: u64,
}

/// One target, as the column of device tabs shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SyncDevice {
    /// The target.
    pub device_id: Id,
    /// A player or a folder.
    pub kind: SyncDeviceKind,
    /// The driver family that knows it: `folder`, and later `rockbox` and
    /// `zune`.
    pub family: String,
    /// How its storage is reached.
    pub transport: SyncTransport,
    /// What the person calls it.
    pub name: String,
    /// Its drawing.
    pub icon: SyncDeviceIcon,
    /// Whether it is Ritornel's yet.
    pub setup: SyncSetup,
    /// Whether it is here.
    pub connection: SyncConnection,
    /// What Ritornel is doing with it.
    pub activity: SyncActivity,
    /// Whether it waits on the person or something went wrong.
    pub attention: SyncAttention,
    /// What its next sync would copy, where a plan is known.
    pub to_copy_bytes: Option<u64>,
    /// Its last sync.
    pub last_sync: Option<SyncLastSync>,
    /// For a player whose plays age out of a scrobbling service: the day
    /// to plug it in by.
    pub connect_by_ms: Option<i64>,
    /// Moves with anything about the target, a run's progress included.
    pub device_revision: u64,
}

/// The result of `devices.list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SyncDevicesList {
    /// Every target that is not forgotten, in the order they were first
    /// seen.
    pub devices: Vec<SyncDevice>,
}

/// The params of a call about one target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SyncDeviceParams {
    /// The target.
    pub device_id: Id,
}

/// What a probe read of a target. Technical terms are as the device gives
/// them.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SyncFacts {
    /// The model, such as `iPod Video`.
    pub model: Option<String>,
    /// Its firmware, such as `Rockbox 4.0`.
    pub firmware: Option<String>,
    /// Its file system, such as `FAT32`.
    pub filesystem: Option<String>,
    /// Tracks Ritornel manages on it.
    pub tracks: u64,
    /// Albums those tracks are of.
    pub albums: u64,
    /// The folder under its root that music is written in.
    pub music_root: String,
    /// How files are laid out under that folder.
    pub layout: String,
}

/// What kind of plays a target gives back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum SyncPlaysKind {
    /// None: a folder has no Plays view.
    None,
    /// A log of dated plays, as Rockbox writes.
    PlayLog,
    /// A count for each track with no times, as a Zune keeps.
    Counters,
}

/// What a target can do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SyncCapabilities {
    /// What plays it gives back.
    pub plays: SyncPlaysKind,
    /// Whether it is made safe to unplug with `devices.eject`.
    pub ejectable: bool,
    /// The cover sizes it is offered, in pixels on the longest side.
    pub cover_sizes: Vec<u32>,
    /// The codecs it plays, such as `flac`, `mp3` and `opus`. A folder's
    /// come from what it is made for, and can be set.
    pub codecs: Vec<String>,
}

/// Whether room was measured on the target now or is what was last seen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum SyncBasis {
    /// Read from the target while it is attached.
    Live,
    /// The last record of it, while it is away.
    LastSeen,
}

/// The room on a target, now and after the plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SyncRoom {
    /// All of it: the volume's size, or a folder's limit where it has one.
    pub capacity_bytes: u64,
    /// What is always kept free.
    pub reserve_bytes: u64,
    /// The files Ritornel wrote and manages.
    pub managed_bytes: u64,
    /// Everything else on it, which is never reclaimed.
    pub unmanaged_bytes: u64,
    /// What is free now.
    pub free_bytes: u64,
    /// Managed bytes after the plan.
    pub managed_after_bytes: u64,
    /// Free bytes after the plan.
    pub free_after_bytes: u64,
    /// How far the wanted content is over what fits, before the cut.
    pub over_bytes: u64,
    /// Whether it was measured now.
    pub basis: SyncBasis,
}

/// What a lossless master is sent as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(tag = "as", rename_all = "snake_case")]
pub enum SyncLossless {
    /// As it is.
    AsIs,
    /// Encoded, with the encoder's settings carried by value so that they
    /// mean the same after the hub moves.
    Encode {
        /// The codec and its options.
        target: crate::transcode::Target,
    },
}

/// A target's format policy. A lossy master is always sent as it is, since
/// lossy is never encoded to lossy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SyncFormat {
    /// What a lossless master is sent as.
    pub lossless: SyncLossless,
}

/// What a sync does with a plan that does not fit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum SyncWhenFull {
    /// Asks: `devices.sync` needs `acceptCut`.
    Ask,
    /// Fills in order and stops at what does not fit.
    Fill,
}

/// What happens when a target is attached.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum SyncOnConnect {
    /// It is read, and waits.
    Nothing,
    /// It is synced, unless something needs the person.
    Sync,
}

/// A target's settings, each carried by value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SyncSettings {
    /// What masters are sent as.
    pub format: SyncFormat,
    /// The embedded cover's longest side in pixels: a baseline JPEG, never
    /// progressive. 0 embeds none.
    pub cover_px: u32,
    /// Whether an MP3 also gets an ID3v1 trailer, for older players.
    pub id3v1: bool,
    /// What is always kept free, or none for the larger of 1% and 256 MB.
    pub reserve_bytes: Option<u64>,
    /// For a folder that is always here, the most it may hold, or none
    /// for its volume's size. A removable one's room is its drive's.
    pub limit_bytes: Option<u64>,
    /// What a sync does with a plan that does not fit.
    pub when_full: SyncWhenFull,
    /// What happens when it is attached.
    pub on_connect: SyncOnConnect,
    /// Whether its plays are read.
    pub read_plays: bool,
    /// Whether the plays read are scrobbled.
    pub scrobble_plays: bool,
    /// After how many days away a reminder to plug it in is shown.
    pub remind_after_days: u32,
}

/// What a source does in the subset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum SyncSourceRole {
    /// Claims what it selects, in its place in the order.
    Rule,
    /// Claims what it selects before every rule.
    Always,
    /// Vetoes what it selects, whatever claims it.
    LeaveOut,
}

/// Whether a source is taken an album or a track at a time: a source cut
/// short stops at a whole unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum SyncUnit {
    /// Whole albums.
    Albums,
    /// Single tracks.
    Tracks,
}

/// What a source selects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SyncSelector {
    /// The whole library.
    Everything,
    /// Every record by one artist.
    #[serde(rename_all = "camelCase")]
    Artist {
        /// The artist.
        artist_id: Id,
    },
    /// One record: a release group, or one release of it.
    #[serde(rename_all = "camelCase")]
    Album {
        /// The record, as `list` rows name it.
        album_id: Id,
    },
    /// What came into the library in the last so many days.
    AddedWithin {
        /// How many days.
        days: u32,
    },
}

/// One source as `devices.set` takes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SyncSourceSpec {
    /// What it does.
    pub role: SyncSourceRole,
    /// What it selects.
    pub selector: SyncSelector,
    /// Whether it is taken an album or a track at a time.
    pub unit: SyncUnit,
}

/// How much of something: tracks and their bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SyncAmount {
    /// How many tracks.
    pub tracks: u64,
    /// Their bytes on the target, in its format.
    pub bytes: u64,
}

/// One source of a target, with what it comes to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SyncSource {
    /// The source. It is new whenever the list of sources is set.
    pub source_id: Id,
    /// What it does, what it selects and its unit.
    #[serde(flatten)]
    pub spec: SyncSourceSpec,
    /// What it selects, in words: an artist's name, a record's title.
    /// Empty where the selector says it all.
    pub label: String,
    /// A second line for the label: a record's artist.
    pub detail: Option<String>,
    /// The cover to show beside it.
    pub art_id: Option<Id>,
    /// The albums it claims: what no earlier source claimed and nothing
    /// leaves out. For a source that leaves out, what it vetoes.
    pub albums: u64,
    /// The tracks it claims, and their bytes in the target's format.
    pub claimed: SyncAmount,
    /// What of it the capacity cut left off.
    pub cut: SyncAmount,
}

/// The three figures of a sync.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SyncFigures {
    /// What it would copy.
    pub to_copy_bytes: u64,
    /// What would be free after it.
    pub free_after_bytes: u64,
    /// How long it would take.
    pub estimated_ms: u64,
    /// Whether every size is known, with none estimated.
    pub exact: bool,
    /// The share of the bytes to copy that is estimated, in thousandths.
    pub estimated_permille: u32,
}

/// The counts a plan is filtered by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SyncCounts {
    /// Files to add.
    pub add: SyncAmount,
    /// Files to remove.
    pub remove: SyncAmount,
    /// Files to send again because their tags, cover, audio or format
    /// changed.
    pub replace: SyncAmount,
    /// Of those, the ones whose only reason is tags or cover, which
    /// `skipRetags` leaves for another day.
    pub retag: SyncAmount,
    /// Files changed on the target, left alone until a choice is made.
    pub conflict: SyncAmount,
    /// Tracks wanted that cannot be sent, each with its reason.
    pub blocked: SyncAmount,
}

/// What a plan does for one source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SyncPlanSource {
    /// The source, or none for what no source wants any more.
    pub source_id: Option<Id>,
    /// What is added for it.
    pub add: SyncAmount,
    /// What is sent again for it.
    pub replace: SyncAmount,
    /// What is removed.
    pub remove: SyncAmount,
}

/// The plan of a target's next sync: a preview, made again whenever its
/// inputs change, and never the record of anything.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SyncPlan {
    /// Names this plan. It moves only when the plan's own inputs do: the
    /// library, the settings, what is on the target. `devices.sync` names
    /// the revision the person saw.
    pub plan_revision: String,
    /// Whether it stands on the target as read now or as last seen.
    pub basis: SyncBasis,
    /// To copy, free after and how long.
    pub figures: SyncFigures,
    /// The same with retags skipped.
    pub figures_skipping_retags: SyncFigures,
    /// Whether everything wanted fits.
    pub fits: bool,
    /// What the capacity cut left off.
    pub cut: SyncAmount,
    /// Counts by kind.
    pub counts: SyncCounts,
    /// What it does for each source, in the sources' order.
    pub by_source: Vec<SyncPlanSource>,
    /// Tracks that must be encoded first, and how many of those encodes
    /// are cached already.
    pub encodes_needed: u64,
    /// Of those, the ones cached already.
    pub encodes_cached: u64,
    /// Why a sync would wait on the person, as words: `does_not_fit`,
    /// `conflicts`.
    pub needs_you: Vec<String>,
}

/// What kind of row a plan or a run has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum SyncActionKind {
    /// A file to write that is not there.
    Add,
    /// A file to send again.
    Replace,
    /// A file to take off.
    Remove,
    /// A file changed on the target. Nothing is touched.
    Conflict,
    /// A wanted track that cannot be sent.
    Blocked,
}

/// Where a row of a run stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum SyncActionState {
    /// Not started.
    Waiting,
    /// Its file is being encoded or tagged.
    Preparing,
    /// Its bytes are going to the target.
    Copying,
    /// Done, and what is on the target is what was meant.
    Done,
    /// Asked of the target with no answer recorded: checked next time.
    InDoubt,
    /// It failed, and says why.
    Failed,
    /// Left for another day.
    Skipped,
}

/// How a file is made from its master.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum SyncHow {
    /// The master's own audio, tagged afresh.
    AsIs,
    /// Encoded from the master.
    Encode,
}

/// Why a row failed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SyncActionError {
    /// A word for it, such as `write_failed` or `master_unavailable`.
    pub code: String,
    /// The whole reason. It never holds a path on a host.
    pub detail: String,
}

/// How the rows of a plan or a run are grouped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum SyncGroupBy {
    /// One row for each album and kind, opening to its tracks.
    #[default]
    Album,
    /// One row for each track.
    Track,
}

/// The params of `devices.actions`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SyncActionsParams {
    /// The target.
    pub device_id: Id,
    /// The plan whose preview to read. A revision that is no longer the
    /// plan's is refused with `plan_stale`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub plan_revision: Option<String>,
    /// The run whose records to read, instead of a plan.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub run_id: Option<Id>,
    /// Only rows of this kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub kind: Option<SyncActionKind>,
    /// Only rows in this state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub state: Option<SyncActionState>,
    /// How rows are grouped.
    #[serde(default)]
    pub group_by: SyncGroupBy,
    /// The group whose tracks to read, in place of the groups.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub parent_id: Option<String>,
    /// The first row.
    #[serde(default)]
    pub offset: u32,
    /// How many rows, 500 at most.
    pub limit: u32,
}

/// One row of a plan or a run: a group, or a track.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SyncActionRow {
    /// A track's action, the same for as long as its plan or run is.
    pub action_id: Option<String>,
    /// A group's ID, to read its tracks with as `parentId`.
    pub group_id: Option<String>,
    /// What is done.
    pub kind: SyncActionKind,
    /// Why, as a word: `wanted`, `tags`, `cover`, `audio`, `format`,
    /// `not_wanted`, `missing`, `changed_on_device`, `path_taken`,
    /// `no_source`, `no_format` or `master_unavailable`.
    pub reason: String,
    /// The source that wants it.
    pub source_id: Option<Id>,
    /// The track's title, or the album's for a group.
    pub title: String,
    /// The artist.
    pub artist: String,
    /// The cover.
    pub art_id: Option<Id>,
    /// For a group, how many tracks it holds.
    pub tracks: Option<u32>,
    /// For a group, how many tracks the album has in the library, so that
    /// "3 of 12 tracks" needs no second call.
    pub album_tracks: Option<u32>,
    /// The file's codec on the target, such as `flac` or `mp3`.
    pub codec: Option<String>,
    /// How it is made.
    pub how: Option<SyncHow>,
    /// Its bytes: what it copies, or for a removal what it frees.
    pub bytes: u64,
    /// Whether that size is an estimate.
    pub estimated: bool,
    /// Where it stands. A plan's rows are all waiting.
    pub state: SyncActionState,
    /// Bytes sent so far.
    pub bytes_done: u64,
    /// Why it failed.
    pub error: Option<SyncActionError>,
    /// For a track, its path under the target's root.
    pub path: Option<String>,
}

/// The result of `devices.actions`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SyncActions {
    /// How many rows the filter has in all.
    pub total: u64,
    /// The first row's index.
    pub offset: u32,
    /// The rows.
    pub rows: Vec<SyncActionRow>,
}

/// A step of a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum SyncStep {
    /// Reading the target: its files, its room and its plays. Nothing is
    /// written before this returns.
    Read,
    /// Freezing the plan the person saw.
    Plan,
    /// A first sync writing the marker that makes the target Ritornel's.
    Adopt,
    /// Removing, replacing and adding files.
    Copy,
    /// Flushing, and recording how it ended.
    Finish,
}

/// Where a step of a run stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum SyncStepState {
    /// Done.
    Done,
    /// Running.
    Running,
    /// To come.
    ToCome,
}

/// One step of a run and where it stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SyncRunStep {
    /// The step.
    pub step: SyncStep,
    /// Where it stands.
    pub state: SyncStepState,
}

/// A file a run is working on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SyncRunFile {
    /// Its action.
    pub action_id: String,
    /// The track's title.
    pub title: String,
    /// Its artist.
    pub artist: String,
    /// Where it stands.
    pub state: SyncActionState,
    /// Bytes sent so far.
    pub bytes_done: u64,
    /// Its bytes in all.
    pub bytes: u64,
}

/// A run: the one under way, or the last.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SyncRun {
    /// The run.
    pub run_id: Id,
    /// Its job, which `jobs.cancel` stops.
    pub job_id: Option<Id>,
    /// Whether it is still under way.
    pub running: bool,
    /// Whether it has been asked to stop.
    pub stopping: bool,
    /// Its steps in order.
    pub steps: Vec<SyncRunStep>,
    /// How it ended.
    pub outcome: Option<SyncOutcome>,
    /// Why it could not run or was cut off, in words.
    pub error: Option<String>,
    /// Actions finished, done or failed.
    pub actions_done: u64,
    /// Actions in all.
    pub actions_total: u64,
    /// Actions that failed.
    pub failed: u64,
    /// Bytes written.
    pub bytes_done: u64,
    /// Bytes to write in all.
    pub bytes_total: u64,
    /// The write speed measured so far.
    pub rate_bytes_per_sec: Option<u64>,
    /// How long is left at that speed.
    pub eta_ms: Option<u64>,
    /// When it started.
    pub started_at_ms: i64,
    /// When it ended.
    pub ended_at_ms: Option<i64>,
    /// The files being worked on now.
    pub current: Vec<SyncRunFile>,
}

/// What came back from a player's plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SyncPlaysSummary {
    /// When they were last read.
    pub last_read_at_ms: Option<i64>,
    /// How many were read in all.
    pub plays: u64,
    /// How many were scrobbled.
    pub scrobbled: u64,
    /// How many were too old to scrobble and went to the history alone.
    pub too_old: u64,
}

/// The files on a target that Ritornel did not write: left alone, and
/// counted as used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SyncUnmanagedSummary {
    /// How many files.
    pub files: u64,
    /// Their bytes.
    pub bytes: u64,
}

/// The result of `devices.get`: a target's page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SyncDeviceDetail {
    /// The target, as `devices.list` gives it.
    pub device: SyncDevice,
    /// For a folder, its path on the server it is on, for a session that
    /// holds `folders:manage`.
    pub path: Option<String>,
    /// For a folder, what it is made for and whether it comes and goes.
    pub folder: Option<SyncFolder>,
    /// What the probe read.
    pub facts: SyncFacts,
    /// What it can do.
    pub capabilities: SyncCapabilities,
    /// Its room, or none while it has never been read.
    pub room: Option<SyncRoom>,
    /// Its settings.
    pub settings: SyncSettings,
    /// What goes on, in order.
    pub sources: Vec<SyncSource>,
    /// The files on it that are not Ritornel's.
    pub unmanaged: SyncUnmanagedSummary,
    /// The plan of its next sync, or none while it cannot be planned: an
    /// ignored target, or one never read.
    pub plan: Option<SyncPlan>,
    /// The run under way, or the last.
    pub run: Option<SyncRun>,
    /// Its plays.
    pub plays: SyncPlaysSummary,
}

/// One format on offer for a target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SyncFormatOption {
    /// The format.
    pub format: SyncFormat,
    /// Whether it is the target's now.
    pub current: bool,
    /// The encoder that makes it, where it has one.
    pub encoder: Option<crate::transcode::EncoderCapability>,
    /// Whether the hub can make it and the target plays it. One that
    /// cannot be chosen is still listed.
    pub available: bool,
    /// What the target's content would take in it.
    pub estimated_bytes: u64,
    /// Whether that fits.
    pub fits: bool,
    /// By how much it does not.
    pub over_bytes: u64,
    /// How long a first sync in it would take.
    pub estimated_ms: u64,
}

/// The result of `devices.formats`: the format menu, which is also the
/// way out of a plan that does not fit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SyncFormats {
    /// Each format on offer, lossless first.
    pub formats: Vec<SyncFormatOption>,
}

/// The params of `devices.unmanaged` and `devices.plays`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SyncPageParams {
    /// The target.
    pub device_id: Id,
    /// The first row.
    #[serde(default)]
    pub offset: u32,
    /// How many rows, 500 at most.
    pub limit: u32,
}

/// A file on a target that Ritornel did not write.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SyncUnmanagedFile {
    /// Its path under the target's root. A path on the device is no
    /// secret.
    pub path: String,
    /// Its size.
    pub size_bytes: u64,
    /// When it was last changed, as the target says.
    pub modified_ms: Option<i64>,
}

/// The result of `devices.unmanaged`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SyncUnmanaged {
    /// How many there are in all.
    pub total: u64,
    /// The first row's index.
    pub offset: u32,
    /// The files, by path.
    pub files: Vec<SyncUnmanagedFile>,
}

/// One play read from a target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SyncPlay {
    /// The track it matched, or none.
    pub appearance_id: Option<Id>,
    /// The track's title, or the path a line named where it matched
    /// nothing.
    pub title: String,
    /// The artist.
    pub artist: Option<String>,
    /// The cover.
    pub art_id: Option<Id>,
    /// When it was played, as the device had it, or none for an undated
    /// play.
    pub played_at_ms: Option<i64>,
    /// What became of it, as a word: a delivery state such as `accepted`
    /// or `pending`, or why it was not sent, such as `too_old`,
    /// `undated`, `scrobbling_off` or `unmatched`.
    pub delivery: String,
}

/// The result of `devices.plays`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SyncPlays {
    /// How many there are in all.
    pub total: u64,
    /// The first row's index.
    pub offset: u32,
    /// The plays, newest first.
    pub plays: Vec<SyncPlay>,
}

/// The params of `devices.set`. Each field left out stays as it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SyncSetParams {
    /// The command's ID, made by the client.
    pub idempotency_key: Id,
    /// The target.
    pub device_id: Id,
    /// A new name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub name: Option<String>,
    /// A drawing by its key. An empty key goes back to the family's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub icon_key: Option<String>,
    /// Whether Ritornel leaves it alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub ignored: Option<bool>,
    /// The whole list of sources, in order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub sources: Option<Vec<SyncSourceSpec>>,
    /// The format policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub format: Option<SyncFormat>,
    /// The cover's longest side in pixels: 0 embeds none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub cover_px: Option<u32>,
    /// Whether an MP3 also gets an ID3v1 trailer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub id3v1: Option<bool>,
    /// What is always kept free: 0 goes back to the default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub reserve_bytes: Option<u64>,
    /// For a folder, the most it may hold: 0 takes the limit away.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub limit_bytes: Option<u64>,
    /// What a sync does with a plan that does not fit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub when_full: Option<SyncWhenFull>,
    /// What happens when it is attached.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub on_connect: Option<SyncOnConnect>,
    /// For a folder, what it is made for. It sets the codecs, the cover's
    /// size and the kind of playlist to that purpose's own, and the format
    /// too where the folder no longer plays the one it had; any of them
    /// given in the same call stands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub made_for: Option<SyncMadeFor>,
    /// For a folder, whether it comes and goes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub folder_kind: Option<SyncFolderKind>,
    /// For a folder, the codecs it plays: `flac`, `mp3`, `opus`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub codecs: Option<Vec<String>>,
    /// For a folder, the kind of playlist it is written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub playlists: Option<SyncPlaylistKind>,
}

/// The result of a command that changes a target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SyncDeviceChanged {
    /// The receipt.
    #[serde(flatten)]
    pub receipt: crate::Receipt,
    /// The target.
    pub device_id: Id,
}

/// The params of `devices.addFolder`. The folder is on the server called.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SyncAddFolderParams {
    /// The command's ID, made by the client.
    pub idempotency_key: Id,
    /// The folder's absolute path. It must exist, and be neither one of
    /// the library's folders nor the data directory nor another target,
    /// nor hold or lie inside one.
    pub path: String,
    /// What to call the target.
    pub name: String,
    /// The most it may hold, for one that is always here: none is its
    /// volume's size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub limit_bytes: Option<u64>,
    /// What it is made for: a plain copy where none is said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub made_for: Option<SyncMadeFor>,
    /// Whether it comes and goes: always here where none is said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub kind: Option<SyncFolderKind>,
}

/// The params of `devices.sync`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SyncStartParams {
    /// The command's ID, made by the client.
    pub idempotency_key: Id,
    /// The target.
    pub device_id: Id,
    /// The plan the person saw. If the plan is no longer that one the run
    /// ends with `plan_stale` and writes nothing.
    pub plan_revision: String,
    /// Makes a new target Ritornel's. It is required, and true, for one
    /// not yet set up, so that first contact is explicit.
    #[serde(default)]
    pub adopt: bool,
    /// Agrees to the capacity cut. It is required, and true, when the plan
    /// does not fit and the target asks.
    #[serde(default)]
    pub accept_cut: bool,
    /// Leaves replaces whose only reason is tags or cover for another day.
    #[serde(default)]
    pub skip_retags: bool,
}

/// The result of `devices.sync`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SyncStarted {
    /// The receipt.
    #[serde(flatten)]
    pub receipt: crate::Receipt,
    /// The run's job: `jobs.cancel` stops the sync.
    pub job: crate::Job,
}

/// The params of `devices.eject` and `devices.forget`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SyncDeviceCommandParams {
    /// The command's ID, made by the client.
    pub idempotency_key: Id,
    /// The target.
    pub device_id: Id,
}

/// What about a target changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum SyncDeviceChange {
    /// Whether it is here, what is being done with it, or that it exists.
    State,
    /// Its name, sources or settings.
    Settings,
    /// The plan of its next sync.
    Plan,
    /// Rows of the run under way: read the visible window again.
    Actions,
    /// Its plays.
    Plays,
}

/// What an event on the `devices` topic carries: a hint to read the
/// target again. Events not yet sent are merged by target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SyncDevicesEvent {
    /// The target.
    pub device_id: Id,
    /// Its revision after the change.
    pub device_revision: u64,
    /// What changed, in order and each once.
    pub changed: Vec<SyncDeviceChange>,
    /// While a run is under way, the run whole.
    pub run: Option<SyncRun>,
}
