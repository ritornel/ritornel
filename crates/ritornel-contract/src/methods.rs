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
    /// Whether it needs a `full` session. A method that does not is open to
    /// any session.
    const NEEDS_FULL: bool;
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
    /// Whether it needs a `full` session.
    pub needs_full: bool,
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

/// Defines the table: a type for each method, [`METHODS`] and
/// [`visit_methods`].
macro_rules! methods {
    ($(
        $(#[$doc:meta])*
        $marker:ident: $name:literal, $kind:ident, needs_full: $needs_full:literal,
        $params:ident => $result:ident;
    )*) => {
        $(
            $(#[$doc])*
            #[derive(Debug, Clone, Copy)]
            pub struct $marker;

            impl Method for $marker {
                const NAME: &'static str = $name;
                const KIND: MethodKind = MethodKind::$kind;
                const NEEDS_FULL: bool = $needs_full;
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
                needs_full: $needs_full,
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
    ServerHello: "server.hello", Session, needs_full: false,
    HelloParams => Descriptor;

    /// `server.status`: the health of the server, for a person or an agent
    /// explaining a failure. It names a path on the server's disk, so it
    /// needs a `full` session.
    ServerStatus: "server.status", Query, needs_full: true,
    NoParams => ServerStatus;

    /// `servers.list`: every server this one knows, itself included. A
    /// client joins the copies of a track against it.
    ServersList: "servers.list", Query, needs_full: false,
    NoParams => ServersList;

    /// `events.subscribe`: adds topics to what this session is sent.
    EventsSubscribe: "events.subscribe", Session, needs_full: false,
    TopicsParams => Subscriptions;

    /// `events.unsubscribe`: removes topics from what this session is sent.
    /// No event on those topics follows the response.
    EventsUnsubscribe: "events.unsubscribe", Session, needs_full: false,
    TopicsParams => Subscriptions;

    /// `library.create`: creates a library on this server and makes the
    /// server its hub. A server that has a library already refuses with
    /// `conflict`.
    LibraryCreate: "library.create", Command, needs_full: true,
    LibraryCreateParams => LibraryCreated;

    /// `sources.list`: every folder the library scans, with where its root
    /// is on this server. An account a history is imported from is a source
    /// too, and is not listed. It names paths, so it needs a `full` session.
    SourcesList: "sources.list", Query, needs_full: true,
    NoParams => SourcesList;

    /// `sources.add`: adds a folder on this server as a named root. The
    /// server only ever reads it. A name the library has already is
    /// refused with `conflict`. A server that holds no library refuses with
    /// `unavailable` and the detail `reason: "no_library"`, having written
    /// nothing: a client that means to make one calls `library.create` and
    /// sends the same command again.
    SourcesAdd: "sources.add", Command, needs_full: true,
    SourcesAddParams => SourceAdded;

    /// `scan.start`: starts a scan of a source as a job, and returns the
    /// job. A source that is being scanned returns the job that is running.
    ScanStart: "scan.start", Command, needs_full: true,
    ScanStartParams => ScanStarted;
    TranscodeStart: "transcode.start", Command, needs_full: true,
    TranscodeParams => TranscodeStarted;
    TranscodePrune: "transcode.prune", Command, needs_full: true,
    TranscodePruneParams => TranscodeStarted;

    /// `jobs.list`: the newest jobs, newest first.
    JobsList: "jobs.list", Query, needs_full: true,
    NoParams => JobsList;

    /// `jobs.get`: one job, with the first of its items that failed.
    JobsGet: "jobs.get", Query, needs_full: true,
    JobGetParams => JobDetail;

    /// `jobs.cancel`: asks a job to stop. It stops after the step in
    /// flight, and what it has committed stays.
    JobsCancel: "jobs.cancel", Command, needs_full: true,
    JobCancelParams => JobCancelled;

    /// `library.stats`: what the library holds and what the scans made of
    /// it.
    LibraryStats: "library.stats", Query, needs_full: false,
    NoParams => LibraryStats;

    /// `identity.dryRun`: runs the resolver again over the stored
    /// revisions, by policy 1 or by other values put in its place, and
    /// says what that would change. It writes nothing. It names files by
    /// path, so it needs a `full` session.
    IdentityDryRun: "identity.dryRun", Query, needs_full: true,
    DryRunParams => DryRun;

    /// `identity.review`: every binding marked for review, with the
    /// candidate it nearly matched and why it did not bind. It names files
    /// by path, so it needs a `full` session.
    IdentityReview: "identity.review", Query, needs_full: true,
    NoParams => ReviewList;

    /// `list.open`: resolves a query into a view this session holds, and
    /// answers with its ID, its exact total and, if asked, its first rows.
    /// With `replaces` it closes that view and says where its anchor row
    /// is in this one.
    ListOpen: "list.open", Session, needs_full: false,
    ListOpenParams => ListOpened;

    /// `list.window`: rows of a view, read as the library has them now.
    ListWindowCall: "list.window", Query, needs_full: false,
    ListWindowParams => ListWindow;

    /// `list.sections`: where each letter, year or stretch of time begins
    /// in a view.
    ListSectionsCall: "list.sections", Query, needs_full: false,
    ViewParams => ListSections;

    /// `list.locate`: the index of a row in a view, by its ID or by the
    /// start of its name.
    ListLocate: "list.locate", Query, needs_full: false,
    ListLocateParams => ListLocated;

    /// `list.close`: ends a view.
    ListClose: "list.close", Session, needs_full: false,
    ViewParams => ListClosed;

    /// `library.search`: the few artists, records and songs whose names
    /// match what was typed, best first, for the results under the search
    /// field.
    LibrarySearch: "library.search", Query, needs_full: false,
    SearchParams => SearchResults;

    /// `album.get`: one release, with its tracks in disc and track order
    /// and the other versions of its group. Given a release group, the
    /// release the group is set to show.
    AlbumGet: "album.get", Query, needs_full: false,
    AlbumGetParams => AlbumPage;

    /// `artist.get`: one artist, as the Artists list has them, and the
    /// colour of their picture.
    ArtistGet: "artist.get", Query, needs_full: false,
    ArtistGetParams => ArtistPage;

    /// `media.token`: the token a page puts in the query string of a media
    /// URL. It is the same for the life of the session.
    MediaTokenCall: "media.token", Session, needs_full: false,
    NoParams => MediaToken;
    /// `playback.get`: the player's state, whole.
    PlaybackGet: "playback.get", Query, needs_full: false,
    NoParams => PlaybackState;

    /// `playback.play`: replaces the queue with what it names and plays it.
    PlaybackPlay: "playback.play", Command, needs_full: false,
    PlaybackPlayParams => PlaybackChanged;

    /// `playback.pause`: pauses. Paused already, it changes nothing.
    PlaybackPause: "playback.pause", Command, needs_full: false,
    PlaybackCommandParams => PlaybackChanged;

    /// `playback.resume`: plays on from where the player is: after a pause,
    /// a stop or a restart.
    PlaybackResume: "playback.resume", Command, needs_full: false,
    PlaybackCommandParams => PlaybackChanged;

    /// `playback.stop`: stops, and the entry starts again from its
    /// beginning.
    PlaybackStop: "playback.stop", Command, needs_full: false,
    PlaybackCommandParams => PlaybackChanged;

    /// `playback.seek`: moves the entry it names to a position.
    PlaybackSeek: "playback.seek", Command, needs_full: false,
    PlaybackSeekParams => PlaybackChanged;

    /// `playback.skip`: skips from the entry it names, forward or back.
    PlaybackSkip: "playback.skip", Command, needs_full: false,
    PlaybackSkipParams => PlaybackChanged;

    /// `playback.jump`: plays the entry it names now, from its beginning.
    PlaybackJump: "playback.jump", Command, needs_full: false,
    PlaybackJumpParams => PlaybackChanged;

    /// `playback.set`: the player's settings, as absolute values: the
    /// volume, whether it is muted, shuffle and repeat.
    PlaybackSet: "playback.set", Command, needs_full: false,
    PlaybackSetParams => PlaybackChanged;

    /// `queue.window`: a window of the queue, in queue order, in play
    /// order or from the entry after the current one, with its revision.
    QueueWindow: "queue.window", Query, needs_full: false,
    QueueWindowParams => QueueWindow;

    /// `queue.add`: adds tracks after the current entry, at the end, or
    /// after an entry it names.
    QueueAdd: "queue.add", Command, needs_full: false,
    QueueAddParams => PlaybackChanged;

    /// `queue.remove`: takes entries out of the queue, by their IDs.
    QueueRemove: "queue.remove", Command, needs_full: false,
    QueueRemoveParams => PlaybackChanged;

    /// `queue.move`: puts entries straight after another, by their IDs.
    QueueMove: "queue.move", Command, needs_full: false,
    QueueMoveParams => PlaybackChanged;

    /// `queue.clear`: empties the queue and stops the player.
    QueueClear: "queue.clear", Command, needs_full: false,
    QueueClearParams => PlaybackChanged;

    /// `sources.tracks`: the tracks of the files in a folder of one of this
    /// server's roots, in the order they are played. It takes a path, so
    /// it needs a `full` session.
    SourcesTracks: "sources.tracks", Query, needs_full: true,
    SourcesTracksParams => SourceTracks;

    /// `history.recent`: the newest plays, newest first. Until list views
    /// arrive, this is how history is read.
    HistoryRecent: "history.recent", Query, needs_full: false,
    HistoryRecentParams => RecentPlays;

    /// `history.onThisDay`: the plays that count on one day of the year,
    /// in every year.
    HistoryOnThisDay: "history.onThisDay", Query, needs_full: false,
    OnThisDayParams => OnThisDay;

    /// `history.artist`: what an artist's page says of the listener's
    /// history with them: the sentence, the plays by year and the records
    /// played most.
    HistoryArtist: "history.artist", Query, needs_full: false,
    HistoryArtistParams => HistoryOf;

    /// `history.album`: what an album's page says of the listener's history
    /// with it, across every version of it: the sentence and the plays by
    /// year.
    HistoryAlbum: "history.album", Query, needs_full: false,
    HistoryAlbumParams => HistoryOf;

    /// `history.artists`: every artist heard, with files or not, by their
    /// plays or when they were first or last heard.
    HistoryArtists: "history.artists", Query, needs_full: false,
    HeardListParams => HeardArtists;

    /// `history.albums`: every album heard, with files or not, by its plays
    /// or when it was first or last heard.
    HistoryAlbums: "history.albums", Query, needs_full: false,
    HeardListParams => HeardAlbums;

    /// `history.track`: what Lean back says about a song and its record,
    /// one fact at a time.
    HistoryTrack: "history.track", Query, needs_full: false,
    TrackHistoryParams => TrackHistory;

    /// `history.stream`: the listener's whole history as the History tab
    /// draws it: every month's plays, in layers by the year each artist
    /// was first heard.
    HistoryStreamCall: "history.stream", Query, needs_full: false,
    NoParams => HistoryStream;

    /// `history.rebuild`: drops the plays and derives them again from what
    /// the sources reported, in one transaction, or says what that would
    /// change.
    HistoryRebuild: "history.rebuild", Command, needs_full: true,
    HistoryRebuildParams => HistoryRebuilt;

    /// `history.zones`: the zones the listener was in, by date, which date
    /// a play whose source gave no zone.
    HistoryZones: "history.zones", Query, needs_full: false,
    NoParams => ZonePeriods;

    /// `history.setZones`: replaces the zones the listener was in, and
    /// derives the plays again with them.
    HistorySetZones: "history.setZones", Command, needs_full: true,
    SetZonesParams => ZonesSet;

    /// `tracks.get`: one track, with its plays and the loves that reach it.
    TracksGet: "tracks.get", Query, needs_full: false,
    TrackGetParams => TrackDetail;

    /// `ratings.set`: the listener's stars for a recording, or none.
    RatingsSet: "ratings.set", Command, needs_full: true,
    RatingSetParams => RatingSet;

    /// `lastfm.status`: whether this server has Last.fm, the account it is
    /// signed in to, and whether it scrobbles.
    LastfmStatusCall: "lastfm.status", Query, needs_full: true,
    NoParams => LastfmStatus;

    /// `lastfm.signInStart`: asks Last.fm for a sign-in, and answers with
    /// the address where the listener approves it. The sign-in is this
    /// session's.
    LastfmSignInStart: "lastfm.signInStart", Session, needs_full: true,
    NoParams => SignInStarted;

    /// `lastfm.signInFinish`: once the listener has approved the sign-in,
    /// exchanges it for a session and keeps it. Before that it answers
    /// `accepted`, and is called again. The sign-in it names, not its key,
    /// is what makes sending it again safe: once finished, it answers with
    /// the same account, and its receipt is made and not stored.
    LastfmSignInFinish: "lastfm.signInFinish", Command, needs_full: true,
    SignInFinishParams => SignInFinished;

    /// `lastfm.signOut`: forgets the session. What was imported stays.
    LastfmSignOut: "lastfm.signOut", Command, needs_full: true,
    KeyParams => SignedOut;

    /// `lastfm.sync`: starts the import of what is new in the account's
    /// history, and of its loved tracks, as a job.
    LastfmSync: "lastfm.sync", Command, needs_full: true,
    LastfmSyncParams => LastfmSyncStarted;

    /// `lastfm.setScrobbling`: sends the plays that count to Last.fm from
    /// now on, or stops.
    LastfmSetScrobbling: "lastfm.setScrobbling", Command, needs_full: true,
    ScrobblingParams => ScrobblingSet;

    /// `lastfm.report`: how much of the imported history matched the
    /// library, and the tracks the matcher was not sure of.
    LastfmReportCall: "lastfm.report", Query, needs_full: true,
    LastfmReportParams => LastfmReport;

    /// `imports.stage`: keeps one piece of a run of streams read from an
    /// archive on the hub, for a dry run. The library is not written.
    ImportsStage: "imports.stage", Command, needs_full: true,
    ImportsStageParams => ImportsStaged;

    /// `imports.dryRun`: starts a job that works out, read-only, what
    /// applying a staged run would do. A dry run of the run that is running
    /// already is returned.
    ImportsDryRun: "imports.dryRun", Command, needs_full: true,
    ImportsRunParams => ImportsJobStarted;

    /// `imports.apply`: starts a job that stores a staged run's streams
    /// that are new and records the run, in one library write. It is
    /// refused when the run is gone, or, applied for the first time, has no
    /// dry run or one made before the matcher or the play rule changed. A
    /// run applied already needs neither, and its job ends as done.
    ImportsApply: "imports.apply", Command, needs_full: true,
    ImportsRunParams => ImportsJobStarted;

    /// `imports.undo`: starts a job that takes an applied run out of force,
    /// in one library write. Its streams stay stored.
    ImportsUndo: "imports.undo", Command, needs_full: true,
    ImportsRunParams => ImportsJobStarted;

    /// `imports.get`: a staged run, with its last report and, once applied,
    /// the run as the library holds it.
    ImportsGet: "imports.get", Query, needs_full: true,
    ImportsGetParams => ImportRun;

    /// `imports.list`: every run the library holds, newest first.
    ImportsList: "imports.list", Query, needs_full: true,
    NoParams => ImportRuns;

    /// `artists.importPictures`: adds a picture folder on this server as a
    /// named root, if it is not one, and takes each artist's portrait from
    /// it, dropping those it no longer chooses. The server only ever reads
    /// it.
    ArtistsImportPictures: "artists.importPictures", Command, needs_full: true,
    ImportPicturesParams => PicturesImported;

    /// `artists.pictureFolders`: the picture folders, with where each is on
    /// this server, and what the last import did. It names paths, so it
    /// needs a `full` session.
    ArtistsPictureFolders: "artists.pictureFolders", Query, needs_full: true,
    NoParams => PictureFolders;

    /// `library.setWatching`: turns watching the library's folders for
    /// changes on or off for this machine, and keeps it so. It answers with
    /// watching as the status then says it.
    LibrarySetWatching: "library.setWatching", Command, needs_full: true,
    WatchingParams => WatchingSet;

    /// `library.settings`: the library's settings, each with what gave it.
    /// A satellite answers with its hub's.
    LibrarySettingsCall: "library.settings", Query, needs_full: true,
    NoParams => LibrarySettings;

    /// `library.setSettings`: changes the library's settings, and keeps
    /// them with it. A satellite's call is its hub's to answer.
    LibrarySetSettings: "library.setSettings", Command, needs_full: true,
    SetLibrarySettingsParams => LibrarySettingsSet;

    /// `server.backup`: copies this server's databases into a new folder in
    /// the place its backups go, and answers when the copy is whole. It
    /// fails with `invalid` when no place is named, and with `unavailable`
    /// when the place cannot be written to now. Sent again, even with the
    /// same key, it takes another backup: the receipt is made and not
    /// stored.
    ServerBackup: "server.backup", Command, needs_full: true,
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
    HistoryExport: "history.export", Command, needs_full: true,
    HistoryExportParams => HistoryExported;

    /// `record.dismiss`: sets aside the scrobbles that ended unknown,
    /// withdrawn or refused until now, so that the status stops saying the record has
    /// stopped for them. A new one says it again. What else stops the
    /// record ends only when it is put right.
    RecordDismiss: "record.dismiss", Command, needs_full: true,
    DismissParams => Dismissed;

    /// `link.invite`: on a hub listening on the private network, makes a
    /// one-time link, good for ten minutes, that another server joins with
    /// to become this hub's satellite. Only a session on the hub's own
    /// machine may ask for one, never a satellite's.
    LinkInvite: "link.invite", Session, needs_full: true,
    LinkInviteParams => LinkInvite;

    /// `link.join`: makes this server, which holds no library and is
    /// linked to none, the satellite of the hub that made the link. It
    /// checks that the hub holds the key the link names before it sends
    /// anything. Sent again once it has joined that hub, it answers as
    /// `link.status` does.
    LinkJoin: "link.join", Command, needs_full: true,
    LinkJoinParams => LinkStatus;

    /// `link.status`: this server's hub, if it is a satellite, or its
    /// satellites, if it is a hub.
    LinkStatusCall: "link.status", Query, needs_full: true,
    NoParams => LinkStatus;

    /// `link.tracks`: on a hub, what a satellite's player reads of the
    /// library: what tracks show, a release's tracks in order, and whom a
    /// player plays for.
    LinkTracksCall: "link.tracks", Query, needs_full: true,
    LinkTracksParams => LinkTracks;

    /// `link.copies`: on a hub, the masters of a track, best first, for a
    /// satellite to fetch and play.
    LinkCopiesCall: "link.copies", Query, needs_full: true,
    LinkCopiesParams => LinkCopies;

    /// `link.listen`: on a hub, takes in one listen a satellite's player
    /// recorded, from the satellite's link session alone and only for its
    /// own player. Keyed on the listen's ID, so sent again it changes
    /// nothing.
    LinkListen: "link.listen", Command, needs_full: true,
    LinkListenParams => LinkListened;

    /// `link.ping`: on a hub, a satellite's word, every few seconds, that
    /// it is there and whether it plays. A satellite that plays keeps the
    /// hub's machine awake; the ping alone does not.
    LinkPing: "link.ping", Session, needs_full: true,
    LinkPingParams => LinkPinged;

    /// `server.settings`: the settings of the server called, each with
    /// what gave it. A satellite answers with its own. Never a linked
    /// server's to read.
    ServerSettingsCall: "server.settings", Query, needs_full: true,
    NoParams => ServerSettings;

    /// `server.setSettings`: changes the settings of the server called
    /// that a call may change, and keeps them with it. A satellite changes
    /// its own. Only a session on the server's own machine may, never a
    /// linked server's.
    ServerSetSettings: "server.setSettings", Command, needs_full: true,
    SetServerSettingsParams => ServerSettingsSet;

    /// `corrections.discs`: each disc layout the library's sidecars give:
    /// every one, or one folder's with the file at each position. It names
    /// paths, so it needs a `full` session.
    CorrectionsDiscs: "corrections.discs", Query, needs_full: true,
    DiscsParams => DiscList;

    /// `corrections.lookup`: asks MusicBrainz about the discs of one album,
    /// a release or a folder, and keeps each reply. A disc with a reply is
    /// not asked about again unless `again` says to. It is refused as
    /// `unavailable` with the reason `lookups_off` until
    /// `library.setSettings` allows it, and with `no_catalogue` on a
    /// server built without MusicBrainz.
    CorrectionsLookup: "corrections.lookup", Command, needs_full: true,
    LookupParams => LookupDone;

    /// `corrections.summary`: the open proposals counted, and whether the
    /// library may be asked about.
    CorrectionsSummaryCall: "corrections.summary", Query, needs_full: true,
    NoParams => CorrectionsSummary;

    /// `corrections.list`: every open proposal, by group.
    CorrectionsList: "corrections.list", Query, needs_full: true,
    NoParams => ProposalList;

    /// `corrections.get`: one proposal whole: its files, what it rests on,
    /// and the values a yes to each candidate would put on the files.
    CorrectionsGet: "corrections.get", Query, needs_full: true,
    ProposalGetParams => ProposalGot;

    /// `corrections.preview`: what proposals, each with a candidate, would
    /// change, as one: the albums now and after. It writes nothing.
    CorrectionsPreview: "corrections.preview", Query, needs_full: true,
    PreviewParams => PreviewShown;

    /// `corrections.confirm`: yes to proposals, each with its candidate and
    /// the fingerprint it was shown, in one write: the values each puts on
    /// its files, and the catalog worked out again with them. A proposal
    /// that has changed since is refused as `conflict` with the reason
    /// `plan_stale`, and nothing is written.
    CorrectionsConfirm: "corrections.confirm", Command, needs_full: true,
    ConfirmParams => Answered;

    /// `corrections.undo`: undoes answers, in one write. Each undo is an
    /// answer of its own; the values the undone answer put are withdrawn,
    /// and the files go back to the rows they were on.
    CorrectionsUndo: "corrections.undo", Command, needs_full: true,
    UndoParams => Answered;

    /// `corrections.decline`: not this candidate for a proposal's files, or
    /// ask no more about them, in one write. Undone like any answer.
    CorrectionsDecline: "corrections.decline", Command, needs_full: true,
    DeclineParams => Answered;

    /// `corrections.keepApart`: the files of two albums are never one
    /// album, in one write. Undone like any answer.
    CorrectionsKeepApart: "corrections.keepApart", Command, needs_full: true,
    KeepApartParams => Answered;

    /// `corrections.answers`: the ledger of answers, newest first, a page
    /// at a time.
    CorrectionsAnswers: "corrections.answers", Query, needs_full: true,
    AnswersParams => AnswerList;

    /// `corrections.answer`: one answer, with what it rested on and said
    /// as it was kept when it was given.
    CorrectionsAnswer: "corrections.answer", Query, needs_full: true,
    AnswerParams => AnswerGot;
}

/// The row of the table for the method named `name`, if there is one.
pub fn method(name: &str) -> Option<&'static MethodInfo> {
    METHODS.iter().find(|method| method.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_is_the_m0_call_list_and_the_calls_added_since() {
        let names: Vec<_> = METHODS.iter().map(|method| method.name).collect();
        assert_eq!(
            names,
            [
                "server.hello",
                "server.status",
                "servers.list",
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
                "corrections.answer"
            ]
        );
        assert_eq!(method("server.status").unwrap().result, "ServerStatus");
        assert_eq!(method("server.statu"), None);
    }

    #[test]
    fn the_status_and_every_command_need_a_full_session() {
        let full: Vec<_> = METHODS
            .iter()
            .filter(|method| method.needs_full)
            .map(|method| method.name)
            .collect();
        assert_eq!(
            full,
            [
                "server.status",
                "library.create",
                "sources.list",
                "sources.add",
                "scan.start",
                "transcode.start",
                "transcode.prune",
                "jobs.list",
                "jobs.get",
                "jobs.cancel",
                "identity.dryRun",
                "identity.review",
                "sources.tracks",
                "history.rebuild",
                "history.setZones",
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
                "corrections.answer"
            ]
        );
        // Everything but reading the library and controlling playback
        // needs one, so no command is open to a limited session but the
        // player's and its queue's.
        for method in METHODS {
            assert!(
                method.needs_full
                    || method.kind != MethodKind::Command
                    || method.name.starts_with("playback.")
                    || method.name.starts_with("queue."),
                "{}",
                method.name
            );
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
                    needs_full: M::NEEDS_FULL,
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
