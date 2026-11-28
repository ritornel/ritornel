//! The method table: every call of the contract, defined once. See "Schema
//! definition and code generation" in docs/internals/contract.md.
//!
//! A method is a type in this module that implements [`Method`], named for
//! its call: [`ServerHello`] is `server.hello`. The server's dispatcher and
//! the Rust client both take the name, the params type and the result type
//! from that one implementation, so they cannot disagree.
//!
//! A generator walks the table two ways. [`METHODS`] lists each method with
//! its types by name, for anything that needs only text. [`visit_methods`]
//! hands a [`MethodVisitor`] each method as a type, and [`Described`] is
//! what a generator may ask of the params and the result. The TypeScript in
//! `contract/contract.ts` is written that way, by this crate's tests.

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::Permission;

/// What sort of call a method is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MethodKind {
    /// It changes only the session it is called on: who the client is, and
    /// what it is subscribed to.
    Session,
    /// It reads and changes nothing.
    Query,
    /// It changes something lasting. Its params carry an `idempotencyKey`
    /// and its result is a [`Receipt`](crate::Receipt). Sent again with the
    /// same key, as a client does when it lost the reply, it is not carried
    /// out twice, and never undoes a command that came after it; the same
    /// key with other params is refused. A method that says otherwise is
    /// carried out again: a backup and an export each write another folder.
    Command,
}

/// One call of the contract.
pub trait Method {
    /// The name on the wire, such as `server.hello`.
    const NAME: &'static str;
    /// What sort of call it is.
    const KIND: MethodKind;
    /// Every permission a session must hold to call this method. Empty when
    /// open to any authenticated session (or restricted by [`Self::LINK_ONLY`]).
    const NEEDS: &'static [Permission];
    /// Whether only a linked satellite server's link session may call this
    /// method.
    const LINK_ONLY: bool;
    /// The params. A method that takes none has [`NoParams`](crate::NoParams).
    type Params: Serialize + DeserializeOwned + Described;
    /// The result. It is always an object, so that it can grow fields.
    type Result: Serialize + DeserializeOwned + Described;
}

/// What a generator asks of a method's params and result.
///
/// In this crate's tests it is ts-rs's `TS`: the type can be written as
/// TypeScript. A method whose params or result cannot be does not compile
/// there, so it cannot be left out of `contract/contract.ts` unnoticed. In
/// every other build it asks nothing and every type has it, so no program
/// is built with ts-rs.
#[cfg(test)]
pub trait Described: ts_rs::TS + 'static {}

#[cfg(test)]
impl<T: ts_rs::TS + 'static> Described for T {}

/// What a generator asks of a method's params and result. In this build it
/// asks nothing: see the same trait as this crate's tests have it.
#[cfg(not(test))]
pub trait Described {}

#[cfg(not(test))]
impl<T> Described for T {}

/// One row of the method table, as text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MethodInfo {
    /// The name on the wire.
    pub name: &'static str,
    /// What sort of call it is.
    pub kind: MethodKind,
    /// The permissions a session must hold to call this method.
    pub needs: &'static [Permission],
    /// Whether only a linked satellite server's link session may call this
    /// method.
    pub link_only: bool,
    /// The name of the params type in this crate.
    pub params: &'static str,
    /// The name of the result type in this crate.
    pub result: &'static str,
}

/// Something that is shown every method in turn, as a type.
pub trait MethodVisitor {
    /// Called once for each method, in the table's order.
    fn visit<M: Method>(&mut self);
}

macro_rules! opt_bool {
    () => {
        false
    };
    ($value:literal) => {
        $value
    };
}

/// Defines the table: a type for each method, [`METHODS`] and
/// [`visit_methods`].
macro_rules! methods {
    ($(
        $(#[$doc:meta])*
        $marker:ident: $name:literal, $kind:ident, needs: [$($perm:ident),* $(,)?],
        $params:ident => $result:ident $(, link_only: $link_only:literal)?;
    )*) => {
        $(
            $(#[$doc])*
            #[derive(Debug, Clone, Copy)]
            pub struct $marker;

            impl Method for $marker {
                const NAME: &'static str = $name;
                const KIND: MethodKind = MethodKind::$kind;
                const NEEDS: &'static [Permission] = &[$(Permission::$perm),*];
                const LINK_ONLY: bool = opt_bool!($($link_only)?);
                type Params = crate::$params;
                type Result = crate::$result;
            }
        )*

        /// Every method of the contract. See "Method summary" in
        /// docs/internals/contract.md.
        pub const METHODS: &[MethodInfo] = &[$(
            MethodInfo {
                name: $name,
                kind: MethodKind::$kind,
                needs: &[$(Permission::$perm),*],
                link_only: opt_bool!($($link_only)?),
                params: stringify!($params),
                result: stringify!($result),
            },
        )*];

        /// Shows `visitor` every method, in [`METHODS`]' order.
        pub fn visit_methods<V: MethodVisitor>(visitor: &mut V) {
            $(visitor.visit::<$marker>();)*
        }
    };
}

methods! {
    /// `server.hello`: the handshake. The client says who it is and the
    /// server answers with its descriptor. It is the first call on a
    /// socket, and every other call is refused until it has succeeded.
    ServerHello: "server.hello", Session, needs: [],
    HelloParams => Descriptor;

    /// `server.status`: the health of the server, for a person or an agent
    /// explaining a failure. Filesystem paths are omitted unless the session
    /// also holds `folders:manage`.
    ServerStatus: "server.status", Query, needs: [StatusRead],
    NoParams => ServerStatus;

    /// `servers.list`: every server this one knows, itself included. A
    /// client joins the copies of a track against it.
    ServersList: "servers.list", Query, needs: [],
    NoParams => ServersList;

    /// `players.list`: the places this library can play.
    PlayersListCall: "players.list", Query, needs: [],
    NoParams => PlayersList;

    /// `events.subscribe`: adds topics to what this session is sent. Each
    /// topic checks its own permission requirement.
    EventsSubscribe: "events.subscribe", Session, needs: [],
    TopicsParams => Subscriptions;

    /// `events.unsubscribe`: removes topics from what this session is sent.
    /// No event on those topics follows the response.
    EventsUnsubscribe: "events.unsubscribe", Session, needs: [],
    TopicsParams => Subscriptions;

    /// `library.create`: creates a library on this server and makes the
    /// server its hub. A server that has a library already refuses with
    /// `conflict`.
    LibraryCreate: "library.create", Command, needs: [ServerManage],
    LibraryCreateParams => LibraryCreated;

    /// `sources.list`: every folder the library scans. Paths are omitted
    /// unless the session also holds `folders:manage`.
    SourcesList: "sources.list", Query, needs: [StatusRead],
    NoParams => SourcesList;

    /// `sources.add`: adds a folder on this server as a named root. The
    /// server only ever reads it. A name the library has already is
    /// refused with `conflict`. A server that holds no library refuses with
    /// `unavailable` and the detail `reason: "no_library"`, having written
    /// nothing: a client that means to make one calls `library.create` and
    /// sends the same command again.
    SourcesAdd: "sources.add", Command, needs: [FoldersManage],
    SourcesAddParams => SourceAdded;

    /// `scan.start`: starts a scan of a source as a job, and returns the
    /// job. A source that is being scanned returns the job that is running.
    ScanStart: "scan.start", Command, needs: [LibraryWrite],
    ScanStartParams => ScanStarted;

    /// `transcode.start`: warms the encode cache for tracks with a named
    /// profile on the hub, and answers one outcome for each.
    TranscodeStart: "transcode.start", Command, needs: [ServerManage],
    TranscodeParams => JobStarted;

    /// `transcode.prune`: removes the cached encodes nothing holds, on the hub.
    TranscodePrune: "transcode.prune", Command, needs: [ServerManage],
    TranscodePruneParams => JobStarted;

    /// `jobs.list`: the newest jobs, newest first.
    JobsList: "jobs.list", Query, needs: [StatusRead],
    NoParams => JobsList;

    /// `jobs.get`: one job, with the first of its items that failed.
    JobsGet: "jobs.get", Query, needs: [StatusRead],
    JobGetParams => JobDetail;

    /// `jobs.cancel`: asks a job to stop. It stops after the step in
    /// flight, and what it has committed stays.
    JobsCancel: "jobs.cancel", Command, needs: [LibraryWrite],
    JobCancelParams => JobCancelled;

    /// `library.stats`: what the library holds and what the scans made of
    /// it.
    LibraryStats: "library.stats", Query, needs: [LibraryRead],
    NoParams => LibraryStats;

    /// `identity.dryRun`: runs the resolver again over the stored
    /// revisions, by policy 1 or by other values put in its place, and
    /// says what that would change. It writes nothing. It names files by
    /// path, so it needs `folders:manage`.
    IdentityDryRun: "identity.dryRun", Query, needs: [FoldersManage],
    DryRunParams => DryRun;

    /// `identity.review`: every binding marked for review, with the
    /// candidate it nearly matched and why it did not bind. It names files
    /// by path, so it needs `folders:manage`.
    IdentityReview: "identity.review", Query, needs: [FoldersManage],
    NoParams => ReviewList;

    /// `list.open`: resolves a query into a view this session holds, and
    /// answers with its ID, its exact total and, if asked, its first rows.
    /// With `replaces` it closes that view and says where its anchor row
    /// is in this one.
    ListOpen: "list.open", Session, needs: [LibraryRead],
    ListOpenParams => ListOpened;

    /// `list.window`: rows of a view, read as the library has them now.
    ListWindowCall: "list.window", Query, needs: [LibraryRead],
    ListWindowParams => ListWindow;

    /// `list.sections`: where each letter, year or stretch of time begins
    /// in a view.
    ListSectionsCall: "list.sections", Query, needs: [LibraryRead],
    ViewParams => ListSections;

    /// `list.locate`: the index of a row in a view, by its ID or by the
    /// start of its name.
    ListLocate: "list.locate", Query, needs: [LibraryRead],
    ListLocateParams => ListLocated;

    /// `list.close`: ends a view.
    ListClose: "list.close", Session, needs: [LibraryRead],
    ViewParams => ListClosed;

    /// `library.search`: the few artists, records and songs whose names
    /// match what was typed, best first, for the results under the search
    /// field.
    LibrarySearch: "library.search", Query, needs: [LibraryRead],
    SearchParams => SearchResults;

    /// `album.get`: one release, with its tracks in disc and track order
    /// and the other versions of its group. Given a release group, the
    /// release the group is set to show.
    AlbumGet: "album.get", Query, needs: [LibraryRead],
    AlbumGetParams => AlbumPage;

    /// `artist.get`: one artist, as the Artists list has them, and the
    /// colour of their picture.
    ArtistGet: "artist.get", Query, needs: [LibraryRead],
    ArtistGetParams => ArtistPage;

    /// `media.token`: the token a page puts in the query string of a media
    /// URL. It is the same for the life of the session.
    MediaTokenCall: "media.token", Session, needs: [LibraryRead],
    NoParams => MediaToken;

    /// `playback.get`: the player's state, whole.
    PlaybackGet: "playback.get", Query, needs: [PlaybackControl],
    PlaybackGetParams => PlaybackState;

    /// `playback.move`: pauses a place, loads its queue on another and releases the source.
    PlaybackMove: "playback.move", Command, needs: [PlaybackControl],
    PlaybackMoveParams => Receipt;

    /// `playback.play`: replaces the queue with what it names and plays it.
    PlaybackPlay: "playback.play", Command, needs: [PlaybackControl],
    PlaybackPlayParams => PlaybackChanged;

    /// `playback.pause`: pauses. Paused already, it changes nothing.
    PlaybackPause: "playback.pause", Command, needs: [PlaybackControl],
    PlaybackCommandParams => PlaybackChanged;

    /// `playback.resume`: plays on from where the player is: after a pause,
    /// a stop or a restart.
    PlaybackResume: "playback.resume", Command, needs: [PlaybackControl],
    PlaybackCommandParams => PlaybackChanged;

    /// `playback.stop`: stops, and the entry starts again from its
    /// beginning.
    PlaybackStop: "playback.stop", Command, needs: [PlaybackControl],
    PlaybackCommandParams => PlaybackChanged;

    /// `playback.seek`: moves the entry it names to a position.
    PlaybackSeek: "playback.seek", Command, needs: [PlaybackControl],
    PlaybackSeekParams => PlaybackChanged;

    /// `playback.skip`: skips from the entry it names, forward or back.
    PlaybackSkip: "playback.skip", Command, needs: [PlaybackControl],
    PlaybackSkipParams => PlaybackChanged;

    /// `playback.jump`: plays the entry it names now, from its beginning.
    PlaybackJump: "playback.jump", Command, needs: [PlaybackControl],
    PlaybackJumpParams => PlaybackChanged;

    /// `playback.set`: the player's settings, as absolute values: the
    /// volume, whether it is muted, shuffle and repeat.
    PlaybackSet: "playback.set", Command, needs: [PlaybackControl],
    PlaybackSetParams => PlaybackChanged;

    /// `queue.window`: a window of the queue, in queue order, in play
    /// order or from the entry after the current one, with its revision.
    QueueWindow: "queue.window", Query, needs: [PlaybackControl],
    QueueWindowParams => QueueWindow;

    /// `queue.add`: adds tracks after the current entry, at the end, or
    /// after an entry it names.
    QueueAdd: "queue.add", Command, needs: [PlaybackControl],
    QueueAddParams => PlaybackChanged;

    /// `queue.remove`: takes entries out of the queue, by their IDs.
    QueueRemove: "queue.remove", Command, needs: [PlaybackControl],
    QueueRemoveParams => PlaybackChanged;

    /// `queue.move`: puts entries straight after another, by their IDs.
    QueueMove: "queue.move", Command, needs: [PlaybackControl],
    QueueMoveParams => PlaybackChanged;

    /// `queue.clear`: empties the queue and stops the player.
    QueueClear: "queue.clear", Command, needs: [PlaybackControl],
    QueueClearParams => PlaybackChanged;

    /// `sources.tracks`: the tracks of the files in a folder of one of this
    /// server's roots, in the order they are played. It takes a path, so
    /// it needs `folders:manage`.
    SourcesTracks: "sources.tracks", Query, needs: [FoldersManage],
    SourcesTracksParams => SourceTracks;

    /// `history.recent`: the newest plays, newest first. Until list views
    /// arrive, this is how history is read.
    HistoryRecent: "history.recent", Query, needs: [LibraryRead],
    HistoryRecentParams => RecentPlays;

    /// `history.onThisDay`: the plays that count on one day of the year,
    /// in every year.
    HistoryOnThisDay: "history.onThisDay", Query, needs: [LibraryRead],
    OnThisDayParams => OnThisDay;

    /// `history.artist`: what an artist's page says of the listener's
    /// history with them: the sentence, the plays by year and the records
    /// played most.
    HistoryArtist: "history.artist", Query, needs: [LibraryRead],
    HistoryArtistParams => HistoryOf;

    /// `history.album`: what an album's page says of the listener's history
    /// with it, across every version of it: the sentence and the plays by
    /// year.
    HistoryAlbum: "history.album", Query, needs: [LibraryRead],
    HistoryAlbumParams => HistoryOf;

    /// `history.artists`: every artist heard, with files or not, by their
    /// plays or when they were first or last heard.
    HistoryArtists: "history.artists", Query, needs: [LibraryRead],
    HeardListParams => HeardArtists;

    /// `history.albums`: every album heard, with files or not, by its plays
    /// or when it was first or last heard.
    HistoryAlbums: "history.albums", Query, needs: [LibraryRead],
    HeardListParams => HeardAlbums;

    /// `history.track`: what Lean back says about a song and its record,
    /// one fact at a time.
    HistoryTrack: "history.track", Query, needs: [LibraryRead],
    TrackHistoryParams => TrackHistory;

    /// `history.stream`: the listener's whole history as the History tab
    /// draws it: every month's plays, in layers by the year each artist
    /// was first heard.
    HistoryStreamCall: "history.stream", Query, needs: [LibraryRead],
    NoParams => HistoryStream;

    /// `history.rebuild`: drops the plays and derives them again from what
    /// the sources reported, in one transaction, or says what that would
    /// change.
    HistoryRebuild: "history.rebuild", Command, needs: [LibraryWrite],
    HistoryRebuildParams => HistoryRebuilt;

    /// `history.zones`: the zones the listener was in, by date, which date
    /// a play whose source gave no zone.
    HistoryZones: "history.zones", Query, needs: [LibraryRead],
    NoParams => ZonePeriods;

    /// `history.setZones`: replaces the zones the listener was in, and
    /// derives the plays again with them.
    HistorySetZones: "history.setZones", Command, needs: [LibraryWrite],
    SetZonesParams => ZonesSet;

    /// `tracks.get`: one track, with its plays and the loves that reach it.
    TracksGet: "tracks.get", Query, needs: [LibraryRead],
    TrackGetParams => TrackDetail;

    /// `ratings.set`: the listener's stars for a recording, or none.
    RatingsSet: "ratings.set", Command, needs: [LibraryWrite],
    RatingSetParams => RatingSet;

    /// `lastfm.status`: whether this server has Last.fm, the account it is
    /// signed in to, and whether it scrobbles.
    LastfmStatusCall: "lastfm.status", Query, needs: [StatusRead],
    NoParams => LastfmStatus;

    /// `lastfm.signInStart`: asks Last.fm for a sign-in, and answers with
    /// the address where the listener approves it. The sign-in is this
    /// session's.
    LastfmSignInStart: "lastfm.signInStart", Session, needs: [ServicesConnect],
    NoParams => SignInStarted;

    /// `lastfm.signInFinish`: once the listener has approved the sign-in,
    /// exchanges it for a session and keeps it. Before that it answers
    /// `accepted`, and is called again. The sign-in it names, not its key,
    /// is what makes sending it again safe: once finished, it answers with
    /// the same account, and its receipt is made and not stored.
    LastfmSignInFinish: "lastfm.signInFinish", Command, needs: [ServicesConnect],
    SignInFinishParams => SignInFinished;

    /// `lastfm.signOut`: forgets the session. What was imported stays.
    LastfmSignOut: "lastfm.signOut", Command, needs: [ServicesConnect],
    KeyParams => SignedOut;

    /// `lastfm.sync`: starts the import of what is new in the account's
    /// history, and of its loved tracks, as a job.
    LastfmSync: "lastfm.sync", Command, needs: [LibraryWrite],
    LastfmSyncParams => LastfmSyncStarted;

    /// `lastfm.setScrobbling`: sends the plays that count to Last.fm from
    /// now on, or stops.
    LastfmSetScrobbling: "lastfm.setScrobbling", Command, needs: [ServicesConnect],
    ScrobblingParams => ScrobblingSet;

    /// `lastfm.report`: how much of the imported history matched the
    /// library, and the tracks the matcher was not sure of.
    LastfmReportCall: "lastfm.report", Query, needs: [LibraryRead],
    LastfmReportParams => LastfmReport;

    /// `imports.stage`: keeps one piece of a run of streams read from an
    /// archive on the hub, for a dry run. The library is not written.
    ImportsStage: "imports.stage", Command, needs: [LibraryWrite],
    ImportsStageParams => ImportsStaged;

    /// `imports.dryRun`: starts a job that works out, read-only, what
    /// applying a staged run would do. A dry run of the run that is running
    /// already is returned.
    ImportsDryRun: "imports.dryRun", Command, needs: [LibraryWrite],
    ImportsRunParams => ImportsJobStarted;

    /// `imports.apply`: starts a job that stores a staged run's streams
    /// that are new and records the run, in one library write. It is
    /// refused when the run is gone, or, applied for the first time, has no
    /// dry run or one made before the matcher or the play rule changed. A
    /// run applied already needs neither, and its job ends as done.
    ImportsApply: "imports.apply", Command, needs: [LibraryWrite],
    ImportsRunParams => ImportsJobStarted;

    /// `imports.undo`: starts a job that takes an applied run out of force,
    /// in one library write. Its streams stay stored.
    ImportsUndo: "imports.undo", Command, needs: [LibraryWrite],
    ImportsRunParams => ImportsJobStarted;

    /// `imports.get`: a staged run, with its last report and, once applied,
    /// the run as the library holds it.
    ImportsGet: "imports.get", Query, needs: [LibraryRead],
    ImportsGetParams => ImportRun;

    /// `imports.list`: every run the library holds, newest first.
    ImportsList: "imports.list", Query, needs: [LibraryRead],
    NoParams => ImportRuns;

    /// `artists.importPictures`: adds a picture folder on this server as a
    /// named root, if it is not one, and takes each artist's portrait from
    /// it, dropping those it no longer chooses. The server only ever reads
    /// it.
    ArtistsImportPictures: "artists.importPictures", Command, needs: [FoldersManage],
    ImportPicturesParams => PicturesImported;

    /// `artists.pictureFolders`: the picture folders, with where each is on
    /// this server, and what the last import did. It names paths, so it
    /// needs `folders:manage`.
    ArtistsPictureFolders: "artists.pictureFolders", Query, needs: [FoldersManage],
    NoParams => PictureFolders;

    /// `library.setWatching`: turns watching the library's folders for
    /// changes on or off for this machine, and keeps it so. It answers with
    /// watching as the status then says it.
    LibrarySetWatching: "library.setWatching", Command, needs: [FoldersManage],
    WatchingParams => WatchingSet;

    /// `library.settings`: the library's settings, each with what gave it.
    /// A satellite answers with its hub's.
    LibrarySettingsCall: "library.settings", Query, needs: [StatusRead],
    NoParams => LibrarySettings;

    /// `library.setSettings`: changes the library's settings, and keeps
    /// them with it. A satellite's call is its hub's to answer.
    LibrarySetSettings: "library.setSettings", Command, needs: [LibraryWrite],
    SetLibrarySettingsParams => LibrarySettingsSet;

    /// `server.backup`: copies this server's databases into a new folder in
    /// the place its backups go, and answers when the copy is whole. It
    /// fails with `invalid` when no place is named, and with `unavailable`
    /// when the place cannot be written to now. Sent again, even with the
    /// same key, it takes another backup: the receipt is made and not
    /// stored.
    ServerBackup: "server.backup", Command, needs: [ServerManage],
    BackupParams => BackedUp;

    /// `history.export`: writes the record into a new folder of its own in
    /// the folder named, readable without Ritornel: every observation,
    /// every rating and what became of every play sent to a service, as
    /// JSON Lines, and the plays derived from them as a table. Which plays
    /// there are, what makes each and whether it counts can be derived
    /// again from the other files alone, by the rule the manifest names. It
    /// answers when the folder is whole. Sent again, even
    /// with the same key, it writes another: the receipt is made and not
    /// stored.
    HistoryExport: "history.export", Command, needs: [FoldersManage],
    HistoryExportParams => HistoryExported;

    /// `record.dismiss`: sets aside the scrobbles that ended unknown,
    /// withdrawn or refused until now, so that the status stops saying the record has
    /// stopped for them. A new one says it again. What else stops the
    /// record ends only when it is put right.
    RecordDismiss: "record.dismiss", Command, needs: [LibraryWrite],
    DismissParams => Dismissed;

    /// `link.invite`: on a hub listening on the private network, makes a
    /// one-time link, good for ten minutes, that another server joins with
    /// to become this hub's satellite.
    LinkInvite: "link.invite", Session, needs: [AccessManage],
    LinkInviteParams => LinkInvite;

    /// `link.join`: makes this server, which holds no library and is
    /// linked to none, the satellite of the hub that made the link. It
    /// checks that the hub holds the key the link names before it sends
    /// anything. Sent again once it has joined that hub, it answers as
    /// `link.status` does.
    LinkJoin: "link.join", Command, needs: [AccessManage],
    LinkJoinParams => LinkStatus;

    /// `link.status`: this server's hub, if it is a satellite, or its
    /// satellites, if it is a hub.
    LinkStatusCall: "link.status", Query, needs: [StatusRead],
    NoParams => LinkStatus;

    /// `link.tracks`: on a hub, what a satellite's player reads of the
    /// library: what tracks show, a release's tracks in order, and whom a
    /// player plays for.
    LinkTracksCall: "link.tracks", Query, needs: [LibraryRead],
    LinkTracksParams => LinkTracks, link_only: true;

    /// `link.copies`: on a hub, the masters of a track, best first, for a
    /// satellite to fetch and play.
    LinkCopiesCall: "link.copies", Query, needs: [LibraryRead],
    LinkCopiesParams => LinkCopies, link_only: true;

    /// `link.listen`: on a hub, takes in one listen a satellite's player
    /// recorded, from the satellite's link session alone and only for its
    /// own player. Keyed on the listen's ID, so sent again it changes
    /// nothing.
    LinkListen: "link.listen", Command, needs: [LibraryRead, StatusRead],
    LinkListenParams => LinkListened, link_only: true;

    /// `link.event`: a live player event from a satellite's presence connection.
    LinkEvent: "link.event", Session, needs: [LibraryRead, StatusRead],
    LinkPlayerEvent => NoParams, link_only: true;

    /// `link.handoff`: one step of a move, accepted only from the player's hub.
    LinkHandoff: "link.handoff", Session, needs: [PlaybackControl],
    LinkHandoffParams => HandoffResult, link_only: true;

    /// `link.ping`: on a hub, a satellite's word, every few seconds, that
    /// it is there and whether it plays. A satellite that plays keeps the
    /// hub's machine awake; the ping alone does not.
    LinkPing: "link.ping", Session, needs: [StatusRead],
    LinkPingParams => LinkPinged, link_only: true;

    /// `server.settings`: the settings of the server called, each with
    /// what gave it. A satellite answers with its own. Paths are omitted
    /// unless the session also holds `folders:manage`.
    ServerSettingsCall: "server.settings", Query, needs: [ServerManage],
    NoParams => ServerSettings;

    /// `server.setSettings`: changes the settings of the server called
    /// that a call may change, and keeps them with it. A satellite changes
    /// its own.
    ServerSetSettings: "server.setSettings", Command, needs: [ServerManage],
    SetServerSettingsParams => ServerSettingsSet;

    /// `corrections.discs`: each disc layout the library's sidecars give:
    /// every one, or one folder's with the file at each position. It names
    /// paths, so it needs `folders:manage`.
    CorrectionsDiscs: "corrections.discs", Query, needs: [FoldersManage],
    DiscsParams => DiscList;

    /// `corrections.lookup`: asks MusicBrainz about the discs of one album,
    /// a release or a folder, and keeps each reply. A disc with a reply is
    /// not asked about again unless `again` says to. It is refused as
    /// `unavailable` with the reason `lookups_off` until
    /// `library.setSettings` allows it, and with `no_catalogue` on a
    /// server built without MusicBrainz.
    CorrectionsLookup: "corrections.lookup", Command, needs: [LibraryWrite],
    LookupParams => LookupDone;

    /// `corrections.summary`: the open proposals counted, and whether the
    /// library may be asked about.
    CorrectionsSummaryCall: "corrections.summary", Query, needs: [LibraryRead],
    NoParams => CorrectionsSummary;

    /// `corrections.list`: every open proposal, by group.
    CorrectionsList: "corrections.list", Query, needs: [LibraryRead],
    NoParams => ProposalList;

    /// `corrections.get`: one proposal whole: its files, what it rests on,
    /// and the values a yes to each candidate would put on the files.
    CorrectionsGet: "corrections.get", Query, needs: [LibraryRead],
    ProposalGetParams => ProposalGot;

    /// `corrections.preview`: what proposals, each with a candidate, would
    /// change, as one: the albums now and after. It writes nothing.
    CorrectionsPreview: "corrections.preview", Query, needs: [LibraryRead],
    PreviewParams => PreviewShown;

    /// `corrections.confirm`: yes to proposals, each with its candidate and
    /// the fingerprint it was shown, in one write: the values each puts on
    /// its files, and the catalog worked out again with them. A proposal
    /// that has changed since is refused as `conflict` with the reason
    /// `plan_stale`, and nothing is written.
    CorrectionsConfirm: "corrections.confirm", Command, needs: [LibraryWrite],
    ConfirmParams => Answered;

    /// `corrections.undo`: undoes answers, in one write. Each undo is an
    /// answer of its own; the values the undone answer put are withdrawn,
    /// and the files go back to the rows they were on.
    CorrectionsUndo: "corrections.undo", Command, needs: [LibraryWrite],
    UndoParams => Answered;

    /// `corrections.decline`: not this candidate for a proposal's files, or
    /// ask no more about them, in one write. Undone like any answer.
    CorrectionsDecline: "corrections.decline", Command, needs: [LibraryWrite],
    DeclineParams => Answered;

    /// `corrections.keepApart`: the files of two albums are never one
    /// album, in one write. Undone like any answer.
    CorrectionsKeepApart: "corrections.keepApart", Command, needs: [LibraryWrite],
    KeepApartParams => Answered;

    /// `corrections.answers`: the ledger of answers, newest first, a page
    /// at a time.
    CorrectionsAnswers: "corrections.answers", Query, needs: [LibraryRead],
    AnswersParams => AnswerList;

    /// `corrections.answer`: one answer, with what it rested on and said
    /// as it was kept when it was given.
    CorrectionsAnswer: "corrections.answer", Query, needs: [LibraryRead],
    AnswerParams => AnswerGot;

    /// `inspect.db`: read-only summary of `server.db` and (on a hub)
    /// `library.db`—file/WAL sizes, migration checksums, tables grouped by
    /// durability layer with columns, indexes, triggers, row counts, and
    /// queue depths.
    InspectDbCall: "inspect.db", Query, needs: [StatusRead, ServerManage],
    NoParams => DbInspection;

    /// `inspect.table`: paged read-only view of one allowlisted table in
    /// `library.db` or `server.db`, excluding secret/session tables and
    /// redacting host paths when the caller lacks `folders:manage`.
    InspectTable: "inspect.table", Query, needs: [ServerManage, LibraryRead],
    InspectTableParams => TablePage;

    /// `inspect.entity`: full provenance chain for a recording, appearance,
    /// release, source item, or audio asset across source items, revisions,
    /// audio assets, claims, assignments, identity decisions, and exclusions.
    /// Absolute paths are included only when the caller holds `folders:manage`.
    InspectEntity: "inspect.entity", Query, needs: [LibraryRead],
    InspectEntityParams => EntityProvenance;

    /// `inspect.play`: derived `play_event` together with its raw
    /// `history_observation` rows, join/dedupe reasons, `history_delivery`
    /// rows, and targeting `observation_correction` rows.
    InspectPlay: "inspect.play", Query, needs: [LibraryRead],
    InspectPlayParams => PlayProvenance;

    /// `inspect.conflicts`: paged `history_ingest_conflict` rows alongside
    /// stuck/refused/unknown `history_delivery` rows and waiting `outbox`
    /// items.
    InspectConflicts: "inspect.conflicts", Query, needs: [StatusRead, LibraryRead],
    InspectConflictsParams => ConflictsReport;

    /// `library.revisions`: paged `library_revision` write log, newest first.
    LibraryRevisions: "library.revisions", Query, needs: [StatusRead],
    LibraryRevisionsParams => LibraryRevisionsPage;

    /// `devices.list`: every player and folder the library syncs to.
    DevicesList: "devices.list", Query, needs: [LibraryRead],
    NoParams => SyncDevicesList;

    /// `devices.get`: one target's page, with the plan of its next sync
    /// and the run under way or last finished. A folder's path is left out
    /// unless the session also holds `folders:manage`.
    DevicesGet: "devices.get", Query, needs: [LibraryRead],
    SyncDeviceParams => SyncDeviceDetail;

    /// `devices.formats`: each format on offer for a target, with what its
    /// content would take in it.
    DevicesFormats: "devices.formats", Query, needs: [LibraryRead],
    SyncDeviceParams => SyncFormats;

    /// `devices.actions`: the rows of a plan or of a run, paged, filtered
    /// and grouped.
    DevicesActions: "devices.actions", Query, needs: [LibraryRead],
    SyncActionsParams => SyncActions;

    /// `devices.unmanaged`: the files on a target that Ritornel did not
    /// write.
    DevicesUnmanaged: "devices.unmanaged", Query, needs: [LibraryRead],
    SyncPageParams => SyncUnmanaged;

    /// `devices.plays`: the plays read from a target, newest first.
    DevicesPlays: "devices.plays", Query, needs: [LibraryRead],
    SyncPageParams => SyncPlays;

    /// `devices.set`: a target's name, drawing, sources, format and every
    /// other setting, and whether it is ignored. A new target takes
    /// settings before it is set up, so its first plan is the real one.
    DevicesSet: "devices.set", Command, needs: [LibraryWrite],
    SyncSetParams => SyncDeviceChanged;

    /// `devices.addFolder`: a folder on the server called, as a target.
    /// Nothing is written to it until its first sync. A folder that is,
    /// holds or lies inside one of the library's folders, the data
    /// directory or another target is refused as `invalid`.
    DevicesAddFolder: "devices.addFolder", Command, needs: [LibraryWrite, FoldersManage],
    SyncAddFolderParams => SyncDeviceChanged;

    /// `devices.sync`: starts a sync at the plan the person saw, as a job,
    /// and for a new target sets it up first. `jobs.cancel` stops it. A
    /// target that is syncing already refuses with `conflict`.
    DevicesSync: "devices.sync", Command, needs: [LibraryWrite],
    SyncStartParams => SyncStarted;

    /// `devices.eject`: makes a target safe to unplug. One that cannot be
    /// ejected, as a folder on a fixed disk, refuses with `unsupported`,
    /// and one that is syncing with `conflict`.
    DevicesEject: "devices.eject", Command, needs: [LibraryWrite],
    SyncDeviceCommandParams => SyncDeviceChanged;

    /// `devices.forget`: ends a target. Its records of what it wrote are
    /// closed, its runs stay as history, and the files on it are left as
    /// they are, no longer Ritornel's. One that is syncing refuses with
    /// `conflict`.
    DevicesForget: "devices.forget", Command, needs: [LibraryWrite],
    SyncDeviceCommandParams => SyncDeviceChanged;
}

/// The row of the table for the method named `name`, if there is one.
pub fn method(name: &str) -> Option<&'static MethodInfo> {
    METHODS.iter().find(|method| method.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PermissionSet, TokenRole};

    #[test]
    fn the_table_is_the_m0_call_list_and_the_calls_added_since() {
        let names: Vec<_> = METHODS.iter().map(|method| method.name).collect();
        assert_eq!(
            names,
            [
                "server.hello",
                "server.status",
                "servers.list",
                "players.list",
                "events.subscribe",
                "events.unsubscribe",
                "library.create",
                "sources.list",
                "sources.add",
                "scan.start",
                "transcode.start",
                "transcode.prune",
                "jobs.list",
                "jobs.get",
                "jobs.cancel",
                "library.stats",
                "identity.dryRun",
                "identity.review",
                "list.open",
                "list.window",
                "list.sections",
                "list.locate",
                "list.close",
                "library.search",
                "album.get",
                "artist.get",
                "media.token",
                "playback.get",
                "playback.move",
                "playback.play",
                "playback.pause",
                "playback.resume",
                "playback.stop",
                "playback.seek",
                "playback.skip",
                "playback.jump",
                "playback.set",
                "queue.window",
                "queue.add",
                "queue.remove",
                "queue.move",
                "queue.clear",
                "sources.tracks",
                "history.recent",
                "history.onThisDay",
                "history.artist",
                "history.album",
                "history.artists",
                "history.albums",
                "history.track",
                "history.stream",
                "history.rebuild",
                "history.zones",
                "history.setZones",
                "tracks.get",
                "ratings.set",
                "lastfm.status",
                "lastfm.signInStart",
                "lastfm.signInFinish",
                "lastfm.signOut",
                "lastfm.sync",
                "lastfm.setScrobbling",
                "lastfm.report",
                "imports.stage",
                "imports.dryRun",
                "imports.apply",
                "imports.undo",
                "imports.get",
                "imports.list",
                "artists.importPictures",
                "artists.pictureFolders",
                "library.setWatching",
                "library.settings",
                "library.setSettings",
                "server.backup",
                "history.export",
                "record.dismiss",
                "link.invite",
                "link.join",
                "link.status",
                "link.tracks",
                "link.copies",
                "link.listen",
                "link.event",
                "link.handoff",
                "link.ping",
                "server.settings",
                "server.setSettings",
                "corrections.discs",
                "corrections.lookup",
                "corrections.summary",
                "corrections.list",
                "corrections.get",
                "corrections.preview",
                "corrections.confirm",
                "corrections.undo",
                "corrections.decline",
                "corrections.keepApart",
                "corrections.answers",
                "corrections.answer",
                "inspect.db",
                "inspect.table",
                "inspect.entity",
                "inspect.play",
                "inspect.conflicts",
                "library.revisions",
                "devices.list",
                "devices.get",
                "devices.formats",
                "devices.actions",
                "devices.unmanaged",
                "devices.plays",
                "devices.set",
                "devices.addFolder",
                "devices.sync",
                "devices.eject",
                "devices.forget",
            ]
        );
        assert_eq!(method("server.status").unwrap().result, "ServerStatus");
        assert_eq!(method("server.statu"), None);
    }

    #[test]
    fn every_command_requires_a_scoped_permission_or_link_session() {
        let guest = PermissionSet::for_role(TokenRole::Guest);
        let monitor = PermissionSet::for_role(TokenRole::Monitor);
        for method in METHODS {
            // Every command must be guarded by at least one permission or be link-only.
            if method.kind == MethodKind::Command {
                assert!(
                    !method.needs.is_empty() || method.link_only,
                    "{} is an unguarded command",
                    method.name
                );
            }
            // A Guest token can only run playback/queue commands, never library/server mutations.
            if method.kind == MethodKind::Command
                && !method.link_only
                && guest.contains_all(method.needs)
            {
                assert!(
                    method.name.starts_with("playback.") || method.name.starts_with("queue."),
                    "guest should not run command {}",
                    method.name
                );
            }
            // A Monitor token can never run any command.
            if method.kind == MethodKind::Command && !method.link_only {
                assert!(
                    !monitor.contains_all(method.needs),
                    "monitor should not run command {}",
                    method.name
                );
            }
        }
    }

    #[test]
    fn a_visitor_sees_every_row_as_a_type() {
        #[derive(Default)]
        struct Rows(Vec<MethodInfo>);

        impl MethodVisitor for Rows {
            fn visit<M: Method>(&mut self) {
                let short = |name: &'static str| name.rsplit("::").next().unwrap();
                self.0.push(MethodInfo {
                    name: M::NAME,
                    kind: M::KIND,
                    needs: M::NEEDS,
                    link_only: M::LINK_ONLY,
                    params: short(std::any::type_name::<M::Params>()),
                    result: short(std::any::type_name::<M::Result>()),
                });
            }
        }

        let mut rows = Rows::default();
        visit_methods(&mut rows);
        assert_eq!(rows.0, METHODS);
    }
}
