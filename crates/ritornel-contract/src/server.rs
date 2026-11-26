//! The session calls and what they return: the handshake and the
//! descriptor, the status, and the list of servers. See "Session and
//! frames" in docs/internals/contract.md.
//!
//! A field that describes something a server does not have yet is left
//! out. What a server says of its library is in its status.
//! The descriptor carries the library revision, and the public one the
//! library and its hub, when the first client needs them there.

use serde::{Deserialize, Serialize};

use crate::{Id, ListSupport, Permission, TokenRole};

/// A server's own record of its role. The library's hub pointer decides
/// which server is the hub ("Server identity and hub role" in
/// docs/internals/linking.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum ServerRole {
    /// It has no library and is linked to none.
    Unassigned,
    /// It holds the library.
    Hub,
    /// It is linked to a hub.
    Satellite,
}

impl ServerRole {
    /// The role as it is written on the wire.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unassigned => "unassigned",
            Self::Hub => "hub",
            Self::Satellite => "satellite",
        }
    }
}

/// What sort of program a client is. It is for logs and for display, and
/// never decides access: the session does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum ClientKind {
    /// The desktop app.
    Desktop,
    /// A page in a browser.
    Browser,
    /// A phone app.
    Phone,
    /// A command-line program, such as `ritornelctl`.
    Cli,
    /// The agent adapter.
    Agent,
    /// Another Ritornel server: a satellite, through its link.
    Server,
}

impl ClientKind {
    /// The kind as it is written on the wire.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Desktop => "desktop",
            Self::Browser => "browser",
            Self::Phone => "phone",
            Self::Cli => "cli",
            Self::Agent => "agent",
            Self::Server => "server",
        }
    }
}

/// The params of `server.hello`: who is calling.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct HelloParams {
    /// The program's name, such as `ritornelctl`. At most
    /// [`MAX_CLIENT_TEXT`] characters, and not empty.
    pub name: String,
    /// What sort of program it is.
    pub kind: ClientKind,
    /// The program's own version. At most [`MAX_CLIENT_TEXT`] characters,
    /// and not empty.
    pub version: String,
    /// The [contract version](crate::CONTRACT_VERSION) the program was
    /// built with. A server refuses one that is not its own.
    pub contract_version: u32,
    /// Present only on the satellite server's own link.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub presence: Option<PresenceHello>,
}

/// What a satellite announces on its own connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct PresenceHello {
    /// Whether this satellite drives an output.
    pub has_player: bool,
    /// Whether it tells Last.fm what is playing itself.
    pub sends_now_playing: bool,
    /// Initial state when this satellite has a player.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub player: Option<PlayerSummary>,
}

/// The fields shown for a reachable player.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct PlayerSummary {
    /// What the player is doing.
    pub status: crate::PlaybackStatus,
    /// The current track while playing, paused or buffering.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub title: Option<String>,
    /// The output once opened.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub output: Option<crate::OutputInfo>,
}

/// One place the library can play.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct PlayerInfo {
    /// Stable ID for this place. Clients use the ID returned by the list.
    pub player_id: Id,
    /// Name of the place as shown to the listener.
    pub name: String,
    /// The server driving this output.
    pub server_id: Id,
    /// Whether this is the player on the server the client called.
    pub is_local: bool,
    /// Whether the player can be reached.
    pub reachable: bool,
    /// When this reachability was established, in Unix milliseconds.
    pub since_ms: i64,
    /// Current status, while reachable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub status: Option<crate::PlaybackStatus>,
    /// Current title, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub title: Option<String>,
    /// Open output, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub output: Option<crate::OutputInfo>,
}

/// The result of `players.list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct PlayersList {
    /// The places this library can play.
    pub players: Vec<PlayerInfo>,
}

/// The longest a client's name or version may be, in characters. Both are
/// text from another program, and both are logged.
pub const MAX_CLIENT_TEXT: usize = 100;

/// The public descriptor: what a server says about itself to anyone who
/// asks, with no credential, at [`route::DESCRIPTOR`](crate::route). A
/// client reads it to check which server answered before it sends one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct PublicDescriptor {
    /// The server's ID: made on first start and never changed.
    pub server_id: Id,
    /// The server's display name. Text from another machine: display only.
    pub name: String,
    /// The server's role.
    pub role: ServerRole,
    /// The [contract version](crate::CONTRACT_VERSION) the server speaks.
    pub contract_version: u32,
    /// The capability flags: what this server can do beyond the calls
    /// every server has. A client gates a feature on its flag, never on a
    /// version, and ignores a flag it does not know. See
    /// [`capability`](crate::capability).
    pub capabilities: Vec<String>,
    /// The limits a client must stay within.
    pub limits: Limits,
    /// What each kind of list supports: its orders and the predicates a
    /// filter on it may use. Empty from a server that has no lists.
    #[serde(default)]
    pub lists: Vec<ListSupport>,
    /// The sizes a cover is served at, in pixels on its longer side: what
    /// `{size}` in `GET /v1/art/{artId}/{size}` may be. Empty from a server
    /// that serves no covers.
    #[serde(default)]
    pub art_sizes: Vec<u32>,
    /// The server's Ed25519 public key, as 64 hex digits: what a client or
    /// a peer pins. Left out by a server that has none to give.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub public_key: Option<String>,
    /// The signature, as 128 hex digits, of [`descriptor_proof`] for the
    /// nonce the caller sent as `?nonce=`: proof that the answer came from
    /// the holder of `public_key`. Left out when no nonce was sent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub proof: Option<String>,
}

/// The query parameter that carries a nonce to
/// [`route::DESCRIPTOR`](crate::route::DESCRIPTOR): 32 to 128 hex digits.
pub const NONCE_PARAM: &str = "nonce";

/// What a server signs to prove it holds its key: a fixed label, its ID and
/// the caller's nonce, so a signature made for one question answers no
/// other.
pub fn descriptor_proof(server_id: Id, nonce: &[u8]) -> Vec<u8> {
    const LABEL: &[u8] = b"ritornel descriptor proof v1\0";
    let mut message = Vec::with_capacity(LABEL.len() + 16 + nonce.len());
    message.extend_from_slice(LABEL);
    message.extend_from_slice(server_id.as_bytes());
    message.extend_from_slice(nonce);
    message
}

impl PublicDescriptor {
    /// Whether the server has this capability flag.
    pub fn has(&self, capability: &str) -> bool {
        self.capabilities.iter().any(|flag| flag == capability)
    }
}

/// The limits a client must stay within.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct Limits {
    /// The largest frame the server reads, in bytes. It closes a socket
    /// that sends a larger one.
    pub max_message_bytes: u64,
    /// How many views a session may hold. Opening one more ends the one
    /// used least recently, and a call on it fails with `view_expired`.
    /// 0 from a server that has no lists.
    #[serde(default)]
    pub max_views: u32,
    /// The most rows one `list.window` sends.
    #[serde(default)]
    pub max_window_rows: u32,
    /// How long a view lasts unused, in milliseconds, before the server
    /// ends it.
    #[serde(default)]
    pub view_idle_ms: u64,
}

/// The descriptor: the result of `server.hello`. It is the public
/// descriptor, and what the server says to this session alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct Descriptor {
    /// What anyone may know.
    #[serde(flatten)]
    pub server: PublicDescriptor,
    /// The permissions this session holds.
    pub permissions: Vec<Permission>,
    /// The named role this session's permissions match, when unchanged from it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub token_role: Option<TokenRole>,
    /// On a satellite, the permissions its hub link holds when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub hub_permissions: Option<Vec<Permission>>,
}

/// The params of a method that takes none: an empty object, which may also
/// be left out of the frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct NoParams {}

/// The result of `server.status`. See "Configuration and logging" in
/// docs/internals/server.md.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ServerStatus {
    /// Encoder capabilities, effective profiles and cache usage on this server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub transcode: Option<crate::transcode::TranscodeStatus>,
    /// The server's ID.
    pub server_id: Id,
    /// The server's display name.
    pub name: String,
    /// The server's role.
    pub role: ServerRole,
    /// What the server is built from and what its database is at.
    pub versions: Versions,
    /// The library this server holds, as its hub. It is left out when the
    /// server holds none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub library: Option<LibraryStatus>,
    /// The space left where the server keeps what it owns.
    pub free_disk: FreeDisk,
    /// The directory the server writes its logs in, on the server's own
    /// disk. Left out when the session lacks `folders:manage`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub log_path: Option<String>,
    /// Whether the server watches its folders for changes, and what it last
    /// did on its own. Left out by a server that does not say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub watch: Option<WatchStatus>,
    /// Where this server backs up, and what its backups last did. Left out
    /// by a server that does not say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub backup: Option<BackupStatus>,
    /// Whether the listening record is advancing, and what has stopped it
    /// if not. Left out by a server that does not say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub record: Option<RecordStatus>,
    /// On a satellite, its hub and whether it answers. Its `library` is
    /// then the hub's. Left out on any other server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub hub: Option<crate::HubStatus>,
}

/// Whether the listening record is advancing: each listen saved and taken
/// into history, each scrobble answered, each import done. Music can go on
/// playing while it does not, so this is said wherever the status is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct RecordStatus {
    /// The warnings: what is wrong now and can be put right, oldest first.
    /// Empty when the record advances.
    pub problems: Vec<RecordProblem>,
    /// The notes: what the person should know and can do nothing about,
    /// oldest first. They do not stop the record.
    #[serde(default)]
    pub notes: Vec<RecordProblem>,
}

/// A warning, one thing that has stopped the record advancing, or a note,
/// one thing that happened that no one can act on; and since when.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct RecordProblem {
    pub kind: RecordProblemKind,
    /// When it began, as Unix milliseconds.
    pub since: i64,
    /// How many listens, scrobbles or imports.
    pub count: u64,
    /// What the newest said went wrong, as the server or the service put
    /// it. Display text only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub detail: Option<String>,
    /// Whether it lasts until `record.dismiss` sets it aside. The others
    /// end when what is wrong is put right.
    pub dismissable: bool,
}

/// What a warning or a note is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum RecordProblemKind {
    /// The player cannot save the listen it is recording.
    ListenUnsaved,
    /// Listens saved on this server have failed to reach history. They are
    /// tried again.
    ListensUndelivered,
    /// A note: scrobbles were sent and never answered, and no import has
    /// brought them back since, so the service may not have them. They are
    /// not sent again. One an import brings back stops counting here.
    ScrobblesUnknown,
    /// A note: scrobbles were never sent, because their plays stopped being
    /// ones to send.
    ScrobblesWithdrawn,
    /// The service refuses the requests that carry scrobbles, so none is
    /// taken: a key it no longer accepts, or a request it cannot read.
    /// Those refused are not sent again. It ends when one is taken again,
    /// or on `record.dismiss`. A play the service declines alone, in a
    /// request it took, is not this: no one can do anything about that.
    ScrobblesRefused,
    /// The service no longer accepts this server's sign-in, so nothing is
    /// scrobbled until someone signs in again.
    SignInNeeded,
    /// Importing the service's history failed, and has not worked since.
    ImportFailed,
    /// What would say whether the record advances could not be read, so
    /// that is not known.
    Unreadable,
}

impl RecordProblemKind {
    /// The kind as it is written on the wire.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ListenUnsaved => "listen_unsaved",
            Self::ListensUndelivered => "listens_undelivered",
            Self::ScrobblesUnknown => "scrobbles_unknown",
            Self::ScrobblesWithdrawn => "scrobbles_withdrawn",
            Self::ScrobblesRefused => "scrobbles_refused",
            Self::SignInNeeded => "sign_in_needed",
            Self::ImportFailed => "import_failed",
            Self::Unreadable => "unreadable",
        }
    }
}

/// The params of `record.dismiss`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct DismissParams {
    /// The command's ID, made by the client.
    pub idempotency_key: Id,
}

/// The result of `record.dismiss`: the receipt, and the record as the
/// status now says it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct Dismissed {
    /// The receipt.
    #[serde(flatten)]
    pub receipt: crate::Receipt,
    pub record: RecordStatus,
}

/// Where a server writes its backups, and the last it wrote. A backup is a
/// copy of the server's databases in a folder of its own, written to a
/// place outside the data directory that has to be named: with none named
/// there is no backup. See "Backups and pre-migration snapshots" in
/// docs/internals/server.md.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct BackupStatus {
    /// The folder backups are written to, on the server's own disk. Left
    /// out when none is named.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub place: Option<String>,
    /// The last backup written. Left out when there has been none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub last: Option<BackupMade>,
    /// The last backup that could not be written, when none has been
    /// written since.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub failed: Option<BackupFailure>,
}

/// A backup that was written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct BackupMade {
    /// When it was taken, as Unix milliseconds.
    pub at: i64,
    /// Its folder, on the server's own disk. Left out when the session lacks
    /// `folders:manage`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub path: Option<String>,
    /// Why it was taken.
    pub reason: BackupReason,
    /// Whether its folder is still there.
    pub present: bool,
}

/// Why a backup was taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum BackupReason {
    /// Someone asked for it, with `server.backup`.
    OnDemand,
    /// The server took it before migrating a database.
    BeforeMigration,
}

impl BackupReason {
    /// The reason as it is written on the wire.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OnDemand => "on_demand",
            Self::BeforeMigration => "before_migration",
        }
    }

    /// The reason a wire name names.
    pub fn from_wire(name: &str) -> Option<Self> {
        match name {
            "on_demand" => Some(Self::OnDemand),
            "before_migration" => Some(Self::BeforeMigration),
            _ => None,
        }
    }
}

/// A backup that could not be written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct BackupFailure {
    /// When it failed, as Unix milliseconds.
    pub at: i64,
    /// What went wrong, as a person reads it. Left out when the session lacks
    /// `folders:manage`, since OS errors may carry filesystem paths.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub message: Option<String>,
}

/// The params of `server.backup`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct BackupParams {
    /// The command's ID, made by the client.
    pub idempotency_key: Id,
}

/// The result of `server.backup`: the receipt, and the backup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct BackedUp {
    /// The receipt.
    #[serde(flatten)]
    pub receipt: crate::Receipt,
    /// The backup that was written.
    pub backup: BackupMade,
}

/// How a hub notices changes to its folders: it watches them, rescans a
/// music root or imports a picture folder once changes settle, and rescans
/// on a timer as well. See "Watching" in docs/internals/identity.md.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct WatchStatus {
    /// Whether this server is noticing changes on its own now.
    pub on: bool,
    /// Whether watching is turned on for this machine, by
    /// `library.setWatching`. It is on until turned off. Watching also
    /// needs this server to be the hub, and a run that watches.
    pub enabled: bool,
    /// Why it is not, when it is not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub off: Option<String>,
    /// Each folder it watches, or would.
    pub folders: Vec<WatchedFolder>,
}

/// The params of `library.setWatching`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct WatchingParams {
    /// The command's ID, made by the client.
    pub idempotency_key: Id,
    /// Watch the folders for changes, or stop.
    pub on: bool,
}

/// The result of `library.setWatching`: the receipt, and watching as the
/// status now says it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct WatchingSet {
    /// The receipt.
    #[serde(flatten)]
    pub receipt: crate::Receipt,
    /// Watching, as `server.status` now says it.
    pub watch: WatchStatus,
}

/// A folder a hub watches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct WatchedFolder {
    /// The root's name.
    pub name: String,
    /// What the folder holds.
    pub kind: WatchedKind,
    /// How changes to it are noticed.
    pub state: WatchState,
    /// Why it is rescanned on a timer alone, when it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub detail: Option<String>,
    /// When the oldest change not yet taken in was seen, as Unix
    /// milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub pending_since: Option<i64>,
    /// The last rescan or import the server started on its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub last_auto: Option<AutoRun>,
}

/// What a watched folder holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum WatchedKind {
    /// Music: a source's root, rescanned.
    Music,
    /// Artist pictures, imported again.
    Pictures,
}

impl WatchedKind {
    /// The kind as it is written on the wire.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Music => "music",
            Self::Pictures => "pictures",
        }
    }
}

/// How changes to a watched folder are noticed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum WatchState {
    /// The system tells the server of changes as they happen.
    Watching,
    /// The system cannot: the folder is looked at again on a timer.
    Polling,
    /// The folder is not there, as an unmounted disk is not. Nothing in it
    /// is taken to be gone, and it is looked at again when it is back.
    Offline,
}

impl WatchState {
    /// The state as it is written on the wire.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Watching => "watching",
            Self::Polling => "polling",
            Self::Offline => "offline",
        }
    }
}

/// A rescan or an import the server started on its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct AutoRun {
    /// What started it.
    pub trigger: AutoTrigger,
    /// When it started, or was queued, as Unix milliseconds.
    pub started_at: i64,
    /// When it ended, once it has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub finished_at: Option<i64>,
    /// Where it is. An import that found nothing to change succeeded.
    pub state: crate::JobState,
    /// The rescan's job. An import is no job, and has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub job_id: Option<Id>,
}

/// What starts a rescan or an import on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum AutoTrigger {
    /// Files changed, and the changes settled.
    Change,
    /// The system lost track of changes, so all of them are looked at.
    Lost,
    /// A folder that was offline is back.
    Returned,
    /// The hub started, and the folder was last looked at too long ago.
    Start,
    /// The timer: the folder was last looked at too long ago.
    Timer,
}

impl AutoTrigger {
    /// The trigger as it is written on the wire.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Change => "change",
            Self::Lost => "lost",
            Self::Returned => "returned",
            Self::Start => "start",
            Self::Timer => "timer",
        }
    }
}

/// The library a server holds, as a status reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct LibraryStatus {
    /// The library's ID: made when the library was created and never
    /// changed.
    pub library_id: Id,
    /// The library revision: it counts the writes to the library, and a
    /// client that holds what it read at this revision may keep it. On a
    /// satellite it is 0: the library is its hub's, and a status answers
    /// without waiting on the hub.
    pub revision: u64,
}

/// The versions a status reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct Versions {
    /// The server program.
    pub server: String,
    /// The [contract version](crate::CONTRACT_VERSION) it speaks.
    pub contract: u32,
    /// The SQLite it is built with.
    pub sqlite: String,
    /// The newest migration `server.db` has had applied.
    pub server_schema: i64,
    /// The newest migration `library.db` has had applied. It is left out
    /// when the server holds no library.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub library_schema: Option<i64>,
}

/// Free disk space, in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct FreeDisk {
    /// What this server may still write on the disk that holds its data
    /// directory. It is left out when the system would not say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub data_bytes: Option<u64>,
}

/// The result of `servers.list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ServersList {
    /// Every server this one knows, itself included.
    pub servers: Vec<ServerInfo>,
}

/// One server's full status: a row of `servers.list`, and what an event on
/// the `servers` topic carries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ServerInfo {
    /// The server's ID.
    pub server_id: Id,
    /// The server's display name.
    pub name: String,
    /// The server's role.
    pub role: ServerRole,
    /// Whether this is the server the client is connected to.
    pub is_this_server: bool,
    /// Whether the server can be reached now.
    pub reachable: bool,
    /// Counts the changes to this status. A client keeps the copy with the
    /// higher revision and drops the other. It counts from when the
    /// answering server started, which is enough: a client that connects
    /// again reads everything again ("Events and subscriptions" in
    /// docs/internals/contract.md).
    pub revision: u64,
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_record_from_a_server_that_gives_no_notes_has_none() {
        let record: super::RecordStatus = serde_json::from_str(r#"{"problems":[]}"#).unwrap();
        assert!(record.notes.is_empty());
    }

    use serde_json::json;

    use super::*;
    use crate::capability;

    fn public() -> PublicDescriptor {
        PublicDescriptor {
            server_id: Id::from_bytes([0x11; 16]),
            name: "desk".to_owned(),
            role: ServerRole::Unassigned,
            contract_version: 0,
            capabilities: vec![capability::EVENTS_SERVERS.to_owned()],
            limits: Limits {
                max_message_bytes: 1_048_576,
                max_views: 16,
                max_window_rows: 500,
                view_idle_ms: 600_000,
            },
            lists: Vec::new(),
            art_sizes: vec![64, 128],
            public_key: None,
            proof: None,
        }
    }

    #[test]
    fn fields_are_camel_case_and_enum_values_snake_case() {
        let hello = HelloParams {
            presence: None,
            name: "ritornelctl".to_owned(),
            kind: ClientKind::Cli,
            version: "0.1.0".to_owned(),
            contract_version: 0,
        };
        let wire = json!({
            "name": "ritornelctl",
            "kind": "cli",
            "version": "0.1.0",
            "contractVersion": 0,
        });
        assert_eq!(serde_json::to_value(&hello).unwrap(), wire);
        assert_eq!(serde_json::from_value::<HelloParams>(wire).unwrap(), hello);

        for role in [
            ServerRole::Unassigned,
            ServerRole::Hub,
            ServerRole::Satellite,
        ] {
            assert_eq!(serde_json::to_value(role).unwrap(), role.as_str());
        }
        for kind in [
            ClientKind::Desktop,
            ClientKind::Browser,
            ClientKind::Phone,
            ClientKind::Cli,
            ClientKind::Agent,
        ] {
            assert_eq!(serde_json::to_value(kind).unwrap(), kind.as_str());
        }
    }

    #[test]
    fn the_descriptor_is_the_public_one_and_the_sessions_permissions_in_one_object() {
        let descriptor = Descriptor {
            server: public(),
            permissions: Permission::ALL.to_vec(),
            token_role: Some(TokenRole::Owner),
            hub_permissions: None,
        };
        let wire = json!({
            "serverId": "11111111-1111-1111-1111-111111111111",
            "name": "desk",
            "role": "unassigned",
            "contractVersion": 0,
            "capabilities": ["events.servers"],
            "limits": {
                "maxMessageBytes": 1_048_576,
                "maxViews": 16,
                "maxWindowRows": 500,
                "viewIdleMs": 600_000,
            },
            "lists": [],
            "artSizes": [64, 128],
            "permissions": [
                "library:read",
                "library:write",
                "playback:control",
                "status:read",
                "folders:manage",
                "server:manage",
                "services:connect",
                "access:manage",
            ],
            "tokenRole": "owner",
        });
        assert_eq!(serde_json::to_value(&descriptor).unwrap(), wire);
        assert_eq!(
            serde_json::from_value::<Descriptor>(wire.clone()).unwrap(),
            descriptor
        );
        // The public descriptor is the same object without the session.
        assert_eq!(
            serde_json::from_value::<PublicDescriptor>(wire).unwrap(),
            public()
        );
        assert!(public().has(capability::EVENTS_SERVERS));
        assert!(!public().has("albums.collapse"));
    }

    #[test]
    fn a_field_this_program_does_not_know_is_ignored() {
        let newer = json!({
            "serverId": "11111111-1111-1111-1111-111111111111",
            "name": "desk",
            "role": "unassigned",
            "contractVersion": 0,
            "capabilities": ["events.servers", "from.the.future"],
            "limits": {
                "maxMessageBytes": 1_048_576,
                "maxViews": 16,
                "maxWindowRows": 500,
                "viewIdleMs": 600_000,
                "maxIds": 1000,
            },
            "lists": [],
            "artSizes": [64, 128],
            "libraryRevision": 4711,
        });
        let read = serde_json::from_value::<PublicDescriptor>(newer).unwrap();
        assert_eq!(read.limits, public().limits);
        assert!(read.has("from.the.future"));
    }

    #[test]
    fn a_status_leaves_out_what_the_system_would_not_say() {
        let mut status = ServerStatus {
            transcode: None,
            server_id: Id::from_bytes([0x11; 16]),
            name: "desk".to_owned(),
            role: ServerRole::Hub,
            versions: Versions {
                server: "0.1.0".to_owned(),
                contract: 0,
                sqlite: "3.54.1".to_owned(),
                server_schema: 2,
                library_schema: Some(1),
            },
            library: Some(LibraryStatus {
                library_id: Id::from_bytes([0x1b; 16]),
                revision: 58,
            }),
            free_disk: FreeDisk {
                data_bytes: Some(1_000),
            },
            log_path: Some("/srv/ritornel/logs".to_owned()),
            watch: None,
            backup: None,
            record: None,
            hub: None,
        };
        let mut wire = json!({
            "serverId": "11111111-1111-1111-1111-111111111111",
            "name": "desk",
            "role": "hub",
            "versions": {
                "server": "0.1.0",
                "contract": 0,
                "sqlite": "3.54.1",
                "serverSchema": 2,
                "librarySchema": 1,
            },
            "library": { "libraryId": "1b1b1b1b-1b1b-1b1b-1b1b-1b1b1b1b1b1b", "revision": 58 },
            "freeDisk": { "dataBytes": 1_000 },
            "logPath": "/srv/ritornel/logs",
        });
        assert_eq!(serde_json::to_value(&status).unwrap(), wire);

        // A server with no library says nothing of one.
        status.role = ServerRole::Unassigned;
        status.versions.library_schema = None;
        status.library = None;
        wire["role"] = json!("unassigned");
        wire["versions"]
            .as_object_mut()
            .unwrap()
            .remove("librarySchema");
        wire.as_object_mut().unwrap().remove("library");
        assert_eq!(serde_json::to_value(&status).unwrap(), wire);

        status.free_disk.data_bytes = None;
        wire["freeDisk"] = json!({});
        assert_eq!(serde_json::to_value(&status).unwrap(), wire);
        assert_eq!(
            serde_json::from_value::<ServerStatus>(wire).unwrap(),
            status
        );
    }

    #[test]
    fn no_params_is_an_empty_object() {
        assert_eq!(serde_json::to_value(NoParams {}).unwrap(), json!({}));
        serde_json::from_value::<NoParams>(json!({})).unwrap();
        serde_json::from_value::<NoParams>(json!({ "later": true })).unwrap();
        assert!(serde_json::from_value::<NoParams>(json!(null)).is_err());
    }
}
