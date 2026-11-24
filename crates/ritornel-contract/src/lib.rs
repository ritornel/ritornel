//! Wire types: calls, events, errors and public IDs. The one schema every
//! client uses to talk to a server (docs/internals/contract.md).
//!
//! - [`Frame`] is a JSON-RPC 2.0 request, response or notification.
//! - [`methods`] is the method table: each call's name, params and result,
//!   what kind of call it is, and whether it needs a `full` session.
//! - [`Receipt`] is what a command returns: a call that changes something
//!   lasting, and carries an idempotency key.
//! - [`Event`] is what the server pushes, by [`Topic`].
//! - [`RpcError`] is the error model: a code from the closed set
//!   [`ErrorCode`], `retryable`, and `details`.
//! - [`route`] is the HTTP side: the socket, the public descriptor, the
//!   health check, and the [ticket](SocketTicket) a page opens the socket
//!   with.
//! - [`local`] is what a server and the clients on its machine agree on
//!   outside the socket.
//!
//! Fields are camelCase and enum values are snake_case. Instants are Unix
//! milliseconds, durations are milliseconds, and every integer fits in 53
//! bits. An ID is an opaque string in a field named for its kind.
//!
//! `contract/contract.ts` at the repository root is these types and the
//! method table as TypeScript. This crate's tests generate it and fail when
//! it is stale; `mise run contract` writes it again.
//!
//! This crate does no I/O and depends on no other crate of the workspace.

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

mod command;
mod device;
mod error;
mod event;
mod frame;
pub mod hex;
mod history;
mod id;
mod identity;
mod imports;
mod lastfm;
mod library;
mod link;
mod list;
pub mod local;
mod media;
pub mod methods;
mod pages;
mod platform;
mod playback;
mod rating;
mod search;
mod server;
mod settings;
mod ticket;
#[cfg(test)]
mod typescript;

pub use command::{LibraryCreateParams, LibraryCreated, Receipt, ReceiptStatus};
pub use device::{DeviceKind, Platform, PlayedVia};
pub use error::{ErrorCode, ErrorData, RpcError};
pub use event::{
    EVENT_METHOD, Event, JobsEvent, LibraryChange, LibraryEvent, ListEvent, ServersEvent,
    Subscriptions, Topic, TopicsParams,
};
pub use frame::{Frame, Malformed, Notification, Request, RequestId, Response};
pub use history::{
    Fact, FactKind, HistoryExportParams, HistoryExported, HistoryOf, HistoryRebuildParams,
    HistoryRebuilt, HistoryRecentParams, HistoryStream, OnThisDay, OnThisDayParams, Phrase, Play,
    PlayCursor, PlayOutcome, PlayedListen, PlayedTrack, RecentPlays, RecordPlayed, StreamLayer,
    TrackHistory, TrackHistoryParams, YearPlays, YearSpan,
};
pub use id::{Id, NotAnId};
pub use identity::{
    Binding, DryRun, DryRunParams, FileFacts, ItemChange, ItemOutcome, Nearest, Penalties,
    PolicyOverrides, PolicyValues, Review, ReviewCause, ReviewList,
};
pub use imports::{
    AppliedRun, ImportProblem, ImportReport, ImportRun, ImportRuns, ImportYear, ImportsGetParams,
    ImportsJobStarted, ImportsRunParams, ImportsStageParams, ImportsStaged, JoinSample,
    LATEST_STREAM_END, LONGEST_STREAM_MS, MAX_STREAMS_PER_PIECE, SpotifyStream, StreamTier,
    TierJoins, is_track_uri,
};
pub use lastfm::{
    DeliveryCount, DoubtfulTrack, ImportSchedule, KeyParams, LastImport, LastfmReport,
    LastfmReportParams, LastfmStatus, LastfmSyncParams, LastfmSyncStarted, NearlyMatched,
    ScrobblingParams, ScrobblingSet, SetZonesParams, SignInFinishParams, SignInFinished,
    SignInStarted, SignedOut, SyncSummary, TrackDetail, TrackGetParams, TrackLove, ZonePeriod,
    ZonePeriods, ZonesSet,
};
pub use library::{
    Count, ImportPicturesParams, Job, JobCancelParams, JobCancelled, JobDetail, JobGetParams,
    JobItem, JobState, JobTarget, JobsList, LibraryStats, PictureFolder, PictureFolders,
    PicturesImported, ScanStartParams, ScanStarted, ScanSummary, Source, SourceAdded, SourceKind,
    SourcesAddParams, SourcesList,
};
pub use link::{
    Awake, HubCopy, HubStatus, InviteLink, LinkCopies, LinkCopiesParams, LinkGranted, LinkInvite,
    LinkInviteParams, LinkJoinParams, LinkListenParams, LinkListened, LinkPingParams, LinkPinged,
    LinkRequest, LinkStatus, LinkTracks, LinkTracksParams, ListenTaken, SatelliteStatus,
    link_request_proof,
};
pub use list::{
    AlbumRef, AlbumTile, ArtistRow, CopyRole, Credit, FILTER_VERSION, Filter, Format, GoneRow,
    ListClosed, ListKind, ListLocateParams, ListLocated, ListOpenParams, ListOpened, ListSections,
    ListSupport, ListWindow, ListWindowParams, Predicate, PredicateName, Quality, Query, Replaces,
    Row, Section, Shelf, SongRow, Sort, SortKey, TrackCopy, ViewParams, Window,
};
pub use media::{ART_SIZES, MEDIA_TOKEN_PARAM, MediaToken};
pub use methods::{METHODS, Method, MethodInfo, MethodKind, MethodVisitor, visit_methods};
pub use pages::{
    AlbumGetParams, AlbumPage, AlbumVersion, ArtColor, ArtistGetParams, ArtistPage, RecordType,
};
pub use playback::{
    ContextKind, CopyInUse, OutputInfo, OutputMode, OutputProblem, PlayContext, PlaySource,
    PlaybackChanged, PlaybackCommandParams, PlaybackEvent, PlaybackJumpParams, PlaybackPlayParams,
    PlaybackSeekParams, PlaybackSetParams, PlaybackSkipParams, PlaybackState, PlaybackStatus,
    ProgressEvent, QueueAddParams, QueueClearParams, QueueEntry, QueueEvent, QueueMoveParams,
    QueueOrder, QueuePlace, QueueRemoveParams, QueueWindow, QueueWindowParams, Repeat,
    SkipDirection, SourceTracks, SourcesTracksParams, TrackSummary,
};
pub use rating::{MAX_RATING, RatingSet, RatingSetParams};
pub use search::{MAX_SEARCH_RESULTS, SearchParams, SearchResults, SongFound};
pub use server::{
    AutoRun, AutoTrigger, BackedUp, BackupFailure, BackupMade, BackupParams, BackupReason,
    BackupStatus, ClientKind, Descriptor, DismissParams, Dismissed, FreeDisk, HelloParams,
    LibraryStatus, Limits, MAX_CLIENT_TEXT, NONCE_PARAM, NoParams, PublicDescriptor, RecordProblem,
    RecordProblemKind, RecordStatus, ServerInfo, ServerRole, ServerStatus, ServersList,
    SessionKind, Versions, WatchState, WatchStatus, WatchedFolder, WatchedKind, WatchingParams,
    WatchingSet, descriptor_proof,
};
pub use settings::{
    ListenNetwork, ListenOn, LogLevel, ServerSettings, ServerSettingsSet, SetBy,
    SetServerSettingsParams, Setting,
};
pub use ticket::{SocketTicket, TICKET_PARAM};

/// The version of the contract these types are.
///
/// It changes whenever the wire does: a test fails when `contract.ts`
/// changes and this number has not, so two builds with one number speak
/// one contract. A server refuses a client whose number is not its own,
/// and a satellite refuses a hub of another ("Session and frames" in
/// docs/internals/contract.md).
pub const CONTRACT_VERSION: u32 = 3;

/// The capability flags a server may list in its descriptor.
pub mod capability {
    /// The server has the `servers` topic: it sends one server's full
    /// status whenever it changes.
    pub const EVENTS_SERVERS: &str = "events.servers";

    /// The server has the `library` topic: a hint after each library write.
    pub const EVENTS_LIBRARY: &str = "events.library";

    /// The server reads large lists through views: `list.open`,
    /// `list.window`, `list.sections`, `list.locate` and `list.close`, and
    /// the `list` topic. The descriptor's `lists` says which kinds, orders
    /// and predicates.
    pub const LISTS: &str = "lists";

    /// The server serves covers at `GET /v1/art/{artId}/{size}`, with the
    /// media token from `media.token`.
    pub const MEDIA_ART: &str = "media.art";
    /// The server can be linked to another: `link.invite` on a hub,
    /// `link.join` on a server with no library, and `link.status`.
    pub const LINK: &str = "link";
    /// The server has a player: the `playback.*` and `queue.*` calls and
    /// the `playback`, `progress` and `queue` topics. A server with no
    /// audio output has none, and refuses those calls as `unsupported`
    /// ("Audio pipeline" in docs/internals/playback.md).
    pub const PLAYBACK: &str = "playback";
}

/// The HTTP routes. Each is a `GET` but [`TICKET`](route::TICKET) and
/// [`LINK`](route::LINK), which are `POST`s.
pub mod route {
    /// The socket that carries calls and events. It needs a credential: in
    /// an `Authorization` header, or as a [ticket](crate::SocketTicket) in
    /// the query string. It refuses an `Origin` the server does not know.
    pub const WS: &str = "/v1/ws";

    /// Trades the credential for a [ticket](crate::SocketTicket) that
    /// opens one socket. It is a `POST`, it needs the credential in an
    /// `Authorization` header, and it answers no request that names an
    /// `Origin`: a page never calls it.
    pub const TICKET: &str = "/v1/ticket";

    /// The [public descriptor](crate::PublicDescriptor). It is the one
    /// route that needs no credential.
    pub const DESCRIPTOR: &str = "/v1/descriptor";

    /// Where a satellite joins a hub: a `POST` of a
    /// [`LinkRequest`](crate::LinkRequest), answered with a
    /// [`LinkGranted`](crate::LinkGranted). Like the descriptor it needs no
    /// credential: the invite's secret is the credential.
    pub const LINK: &str = "/v1/link";

    /// A master, for a satellite to play: `GET /v1/audio/{appearanceId}/
    /// {sourceItemId}/{revision}` under this prefix, as `link.copies` names
    /// it. Only a link session reads it, with its token in the header.
    pub const AUDIO: &str = "/v1/audio";

    /// Whether the server is up: a [`Health`]. It needs a credential.
    pub const HEALTH: &str = "/v1/health";

    /// Covers: `GET /v1/art/{artId}/{size}` under this prefix. It takes the
    /// credential in a header, or a [media token](crate::MediaToken) in the
    /// query string, and nothing else takes a media token.
    pub const ART: &str = "/v1/art";

    /// The body of [`HEALTH`].
    #[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    #[cfg_attr(test, derive(ts_rs::TS))]
    #[serde(rename_all = "camelCase")]
    pub struct Health {
        /// Always `ok`: a server that is not answers nothing.
        pub status: HealthStatus,
    }

    /// What a health check reports.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    #[cfg_attr(test, derive(ts_rs::TS))]
    #[serde(rename_all = "snake_case")]
    pub enum HealthStatus {
        /// The server is taking calls.
        Ok,
    }
}
