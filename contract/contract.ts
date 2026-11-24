// The Ritornel contract as TypeScript: every call, event, error and route a
// client uses to talk to a server.
//
// Generated from the Rust types and the method table in
// crates/ritornel-contract. Do not edit it: change those, then run
// `mise run contract`.
//
// Fields are camelCase and enum values are snake_case. An ID is an opaque
// string. Instants are Unix milliseconds and durations are milliseconds.
// Every integer fits in 53 bits, so each is a `number`.

/** The version of the contract these types are. */
export const CONTRACT_VERSION = 4;

/** The HTTP routes. Each is a `GET` but `ticket` and `link`, which are `POST`s. */
export const ROUTES = {
  ws: "/v1/ws",
  ticket: "/v1/ticket",
  descriptor: "/v1/descriptor",
  health: "/v1/health",
  art: "/v1/art",
  link: "/v1/link",
  audio: "/v1/audio",
} as const;

/**
 * The name of the query parameter that carries a ticket on the socket's
 * route.
 */
export const TICKET_PARAM = "ticket";

/** The capability flags a server may list in its descriptor. */
export const CAPABILITIES = {
  eventsServers: "events.servers",
  eventsLibrary: "events.library",
  lists: "lists",
  mediaArt: "media.art",
  playback: "playback",
} as const;

/** The method of every event notification. */
export const EVENT_METHOD = "event";

/** The longest a client's name or version may be, in characters. */
export const MAX_CLIENT_TEXT = 100;

/**
 * The name of the query parameter that carries a media token on a media
 * route.
 */
export const MEDIA_TOKEN_PARAM = "token";

/**
 * The sizes a cover is served at, in pixels on its longer side. The
 * descriptor lists the ones a server serves.
 */
export const ART_SIZES = [64, 96, 128, 192, 256, 384, 512, 768, 1024] as const;

/** The version of the filter language a filter is written in. */
export const FILTER_VERSION = 1;

/**
 * Every method, in the order of the method table: what kind of call it is,
 * and whether it needs a `full` session.
 */
export const METHODS = {
  "server.hello": { kind: "session", needsFull: false },
  "server.status": { kind: "query", needsFull: true },
  "servers.list": { kind: "query", needsFull: false },
  "events.subscribe": { kind: "session", needsFull: false },
  "events.unsubscribe": { kind: "session", needsFull: false },
  "library.create": { kind: "command", needsFull: true },
  "sources.list": { kind: "query", needsFull: true },
  "sources.add": { kind: "command", needsFull: true },
  "scan.start": { kind: "command", needsFull: true },
  "jobs.list": { kind: "query", needsFull: true },
  "jobs.get": { kind: "query", needsFull: true },
  "jobs.cancel": { kind: "command", needsFull: true },
  "library.stats": { kind: "query", needsFull: false },
  "identity.dryRun": { kind: "query", needsFull: true },
  "identity.review": { kind: "query", needsFull: true },
  "list.open": { kind: "session", needsFull: false },
  "list.window": { kind: "query", needsFull: false },
  "list.sections": { kind: "query", needsFull: false },
  "list.locate": { kind: "query", needsFull: false },
  "list.close": { kind: "session", needsFull: false },
  "library.search": { kind: "query", needsFull: false },
  "album.get": { kind: "query", needsFull: false },
  "artist.get": { kind: "query", needsFull: false },
  "media.token": { kind: "session", needsFull: false },
  "playback.get": { kind: "query", needsFull: false },
  "playback.play": { kind: "command", needsFull: false },
  "playback.pause": { kind: "command", needsFull: false },
  "playback.resume": { kind: "command", needsFull: false },
  "playback.stop": { kind: "command", needsFull: false },
  "playback.seek": { kind: "command", needsFull: false },
  "playback.skip": { kind: "command", needsFull: false },
  "playback.jump": { kind: "command", needsFull: false },
  "playback.set": { kind: "command", needsFull: false },
  "queue.window": { kind: "query", needsFull: false },
  "queue.add": { kind: "command", needsFull: false },
  "queue.remove": { kind: "command", needsFull: false },
  "queue.move": { kind: "command", needsFull: false },
  "queue.clear": { kind: "command", needsFull: false },
  "sources.tracks": { kind: "query", needsFull: true },
  "history.recent": { kind: "query", needsFull: false },
  "history.onThisDay": { kind: "query", needsFull: false },
  "history.artist": { kind: "query", needsFull: false },
  "history.album": { kind: "query", needsFull: false },
  "history.artists": { kind: "query", needsFull: false },
  "history.albums": { kind: "query", needsFull: false },
  "history.track": { kind: "query", needsFull: false },
  "history.stream": { kind: "query", needsFull: false },
  "history.rebuild": { kind: "command", needsFull: true },
  "history.zones": { kind: "query", needsFull: false },
  "history.setZones": { kind: "command", needsFull: true },
  "tracks.get": { kind: "query", needsFull: false },
  "ratings.set": { kind: "command", needsFull: true },
  "lastfm.status": { kind: "query", needsFull: true },
  "lastfm.signInStart": { kind: "session", needsFull: true },
  "lastfm.signInFinish": { kind: "command", needsFull: true },
  "lastfm.signOut": { kind: "command", needsFull: true },
  "lastfm.sync": { kind: "command", needsFull: true },
  "lastfm.setScrobbling": { kind: "command", needsFull: true },
  "lastfm.report": { kind: "query", needsFull: true },
  "imports.stage": { kind: "command", needsFull: true },
  "imports.dryRun": { kind: "command", needsFull: true },
  "imports.apply": { kind: "command", needsFull: true },
  "imports.undo": { kind: "command", needsFull: true },
  "imports.get": { kind: "query", needsFull: true },
  "imports.list": { kind: "query", needsFull: true },
  "artists.importPictures": { kind: "command", needsFull: true },
  "artists.pictureFolders": { kind: "query", needsFull: true },
  "library.setWatching": { kind: "command", needsFull: true },
  "library.settings": { kind: "query", needsFull: true },
  "library.setSettings": { kind: "command", needsFull: true },
  "server.backup": { kind: "command", needsFull: true },
  "history.export": { kind: "command", needsFull: true },
  "record.dismiss": { kind: "command", needsFull: true },
  "link.invite": { kind: "session", needsFull: true },
  "link.join": { kind: "command", needsFull: true },
  "link.status": { kind: "query", needsFull: true },
  "link.tracks": { kind: "query", needsFull: true },
  "link.copies": { kind: "query", needsFull: true },
  "link.listen": { kind: "command", needsFull: true },
  "link.ping": { kind: "session", needsFull: true },
  "server.settings": { kind: "query", needsFull: true },
  "server.setSettings": { kind: "command", needsFull: true },
  "corrections.discs": { kind: "query", needsFull: true },
  "corrections.lookup": { kind: "command", needsFull: true },
  "corrections.summary": { kind: "query", needsFull: true },
  "corrections.list": { kind: "query", needsFull: true },
  "corrections.get": { kind: "query", needsFull: true },
  "corrections.preview": { kind: "query", needsFull: true },
  "corrections.confirm": { kind: "command", needsFull: true },
  "corrections.undo": { kind: "command", needsFull: true },
  "corrections.answers": { kind: "query", needsFull: true },
  "corrections.answer": { kind: "query", needsFull: true },
} as const;

/** Every topic a client can subscribe to. */
export const TOPICS = ["servers", "jobs", "library", "list", "playback", "progress", "queue"] as const;

/** The params and the result of each method. */
export type MethodMap = {
  "server.hello": { params: HelloParams; result: Descriptor };
  "server.status": { params: NoParams; result: ServerStatus };
  "servers.list": { params: NoParams; result: ServersList };
  "events.subscribe": { params: TopicsParams; result: Subscriptions };
  "events.unsubscribe": { params: TopicsParams; result: Subscriptions };
  "library.create": { params: LibraryCreateParams; result: LibraryCreated };
  "sources.list": { params: NoParams; result: SourcesList };
  "sources.add": { params: SourcesAddParams; result: SourceAdded };
  "scan.start": { params: ScanStartParams; result: ScanStarted };
  "jobs.list": { params: NoParams; result: JobsList };
  "jobs.get": { params: JobGetParams; result: JobDetail };
  "jobs.cancel": { params: JobCancelParams; result: JobCancelled };
  "library.stats": { params: NoParams; result: LibraryStats };
  "identity.dryRun": { params: DryRunParams; result: DryRun };
  "identity.review": { params: NoParams; result: ReviewList };
  "list.open": { params: ListOpenParams; result: ListOpened };
  "list.window": { params: ListWindowParams; result: ListWindow };
  "list.sections": { params: ViewParams; result: ListSections };
  "list.locate": { params: ListLocateParams; result: ListLocated };
  "list.close": { params: ViewParams; result: ListClosed };
  "library.search": { params: SearchParams; result: SearchResults };
  "album.get": { params: AlbumGetParams; result: AlbumPage };
  "artist.get": { params: ArtistGetParams; result: ArtistPage };
  "media.token": { params: NoParams; result: MediaToken };
  "playback.get": { params: NoParams; result: PlaybackState };
  "playback.play": { params: PlaybackPlayParams; result: PlaybackChanged };
  "playback.pause": { params: PlaybackCommandParams; result: PlaybackChanged };
  "playback.resume": { params: PlaybackCommandParams; result: PlaybackChanged };
  "playback.stop": { params: PlaybackCommandParams; result: PlaybackChanged };
  "playback.seek": { params: PlaybackSeekParams; result: PlaybackChanged };
  "playback.skip": { params: PlaybackSkipParams; result: PlaybackChanged };
  "playback.jump": { params: PlaybackJumpParams; result: PlaybackChanged };
  "playback.set": { params: PlaybackSetParams; result: PlaybackChanged };
  "queue.window": { params: QueueWindowParams; result: QueueWindow };
  "queue.add": { params: QueueAddParams; result: PlaybackChanged };
  "queue.remove": { params: QueueRemoveParams; result: PlaybackChanged };
  "queue.move": { params: QueueMoveParams; result: PlaybackChanged };
  "queue.clear": { params: QueueClearParams; result: PlaybackChanged };
  "sources.tracks": { params: SourcesTracksParams; result: SourceTracks };
  "history.recent": { params: HistoryRecentParams; result: RecentPlays };
  "history.onThisDay": { params: OnThisDayParams; result: OnThisDay };
  "history.artist": { params: HistoryArtistParams; result: HistoryOf };
  "history.album": { params: HistoryAlbumParams; result: HistoryOf };
  "history.artists": { params: HeardListParams; result: HeardArtists };
  "history.albums": { params: HeardListParams; result: HeardAlbums };
  "history.track": { params: TrackHistoryParams; result: TrackHistory };
  "history.stream": { params: NoParams; result: HistoryStream };
  "history.rebuild": { params: HistoryRebuildParams; result: HistoryRebuilt };
  "history.zones": { params: NoParams; result: ZonePeriods };
  "history.setZones": { params: SetZonesParams; result: ZonesSet };
  "tracks.get": { params: TrackGetParams; result: TrackDetail };
  "ratings.set": { params: RatingSetParams; result: RatingSet };
  "lastfm.status": { params: NoParams; result: LastfmStatus };
  "lastfm.signInStart": { params: NoParams; result: SignInStarted };
  "lastfm.signInFinish": { params: SignInFinishParams; result: SignInFinished };
  "lastfm.signOut": { params: KeyParams; result: SignedOut };
  "lastfm.sync": { params: LastfmSyncParams; result: LastfmSyncStarted };
  "lastfm.setScrobbling": { params: ScrobblingParams; result: ScrobblingSet };
  "lastfm.report": { params: LastfmReportParams; result: LastfmReport };
  "imports.stage": { params: ImportsStageParams; result: ImportsStaged };
  "imports.dryRun": { params: ImportsRunParams; result: ImportsJobStarted };
  "imports.apply": { params: ImportsRunParams; result: ImportsJobStarted };
  "imports.undo": { params: ImportsRunParams; result: ImportsJobStarted };
  "imports.get": { params: ImportsGetParams; result: ImportRun };
  "imports.list": { params: NoParams; result: ImportRuns };
  "artists.importPictures": { params: ImportPicturesParams; result: PicturesImported };
  "artists.pictureFolders": { params: NoParams; result: PictureFolders };
  "library.setWatching": { params: WatchingParams; result: WatchingSet };
  "library.settings": { params: NoParams; result: LibrarySettings };
  "library.setSettings": { params: SetLibrarySettingsParams; result: LibrarySettingsSet };
  "server.backup": { params: BackupParams; result: BackedUp };
  "history.export": { params: HistoryExportParams; result: HistoryExported };
  "record.dismiss": { params: DismissParams; result: Dismissed };
  "link.invite": { params: LinkInviteParams; result: LinkInvite };
  "link.join": { params: LinkJoinParams; result: LinkStatus };
  "link.status": { params: NoParams; result: LinkStatus };
  "link.tracks": { params: LinkTracksParams; result: LinkTracks };
  "link.copies": { params: LinkCopiesParams; result: LinkCopies };
  "link.listen": { params: LinkListenParams; result: LinkListened };
  "link.ping": { params: LinkPingParams; result: LinkPinged };
  "server.settings": { params: NoParams; result: ServerSettings };
  "server.setSettings": { params: SetServerSettingsParams; result: ServerSettingsSet };
  "corrections.discs": { params: DiscsParams; result: DiscList };
  "corrections.lookup": { params: LookupParams; result: LookupDone };
  "corrections.summary": { params: NoParams; result: CorrectionsSummary };
  "corrections.list": { params: NoParams; result: ProposalList };
  "corrections.get": { params: ProposalGetParams; result: ProposalGot };
  "corrections.preview": { params: PreviewParams; result: PreviewShown };
  "corrections.confirm": { params: ConfirmParams; result: Answered };
  "corrections.undo": { params: UndoParams; result: Answered };
  "corrections.answers": { params: AnswersParams; result: AnswerList };
  "corrections.answer": { params: AnswerParams; result: AnswerGot };
};

/** The name of a method. */
export type MethodName = keyof MethodMap;

/**
 * What an event on each topic carries: the params of its notification. Its
 * keys are the topics, as `Topic` is.
 */
export type EventMap = {
  "servers": Extract<Event, { topic: "servers" }>;
  "jobs": Extract<Event, { topic: "jobs" }>;
  "library": Extract<Event, { topic: "library" }>;
  "list": Extract<Event, { topic: "list" }>;
  "playback": Extract<Event, { topic: "playback" }>;
  "progress": Extract<Event, { topic: "progress" }>;
  "queue": Extract<Event, { topic: "queue" }>;
};

/**
 * What a client names a request by, and a response names its request by: a
 * whole number or a string.
 */
export type RequestId = number | string;

/**
 * A call: a JSON-RPC 2.0 request. It gets exactly one response, which
 * carries its `id`. A method that takes no params is sent `{}`.
 */
export type RequestFrame<M extends MethodName = MethodName> = {
  [K in M]: { jsonrpc: "2.0"; id: RequestId; method: K; params: MethodMap[K]["params"] };
}[M];

/**
 * The answer to a call: its result, or why there is none. The `id` is the
 * request's, and is `null` only when the request was too broken to read one
 * from.
 */
export type ResponseFrame<M extends MethodName = MethodName> =
  | { jsonrpc: "2.0"; id: RequestId; result: MethodMap[M]["result"]; error?: never }
  | { jsonrpc: "2.0"; id: RequestId | null; error: RpcError; result?: never };

/**
 * An event: a notification, which gets no response. A client ignores one
 * whose topic it does not know.
 */
export type EventFrame = { jsonrpc: "2.0"; method: typeof EVENT_METHOD; params: Event };

/** A frame a server sends a client. */
export type ServerFrame = ResponseFrame | EventFrame;

/**
 * The params of `album.get`.
 */
export type AlbumGetParams = {
  /**
   * The album: a release group, which gives the release the group is
   * set to show, or one release.
   */
  ref: AlbumRef,
};

/**
 * The result of `album.get`: one release, with what an album's page
 * shows of it.
 */
export type AlbumPage = {
  /**
   * The release shown: for a group, the one the group is set to show.
   */
  releaseId: string,
  /**
   * The release group it is a version of.
   */
  releaseGroupId: string,
  /**
   * What kind of record it is, where that is known.
   */
  recordType: RecordType | null,
  /**
   * The shelf its artist's discography files it on.
   */
  shelf: Shelf,
  /**
   * Its tracks, in disc and track order.
   */
  tracks: Array<SongRow>,
  /**
   * The other versions of the record: every other release of its group,
   * oldest first.
   */
  versions: Array<AlbumVersion>,
  /**
   * The colour of its cover. `null` when it has none, or it is close to
   * grey.
   */
  artColor: ArtColor | null,
  /**
   * The library revision this was read at.
   */
  libraryRevision: number,
  /**
   * What the tile is.
   */
  ref: AlbumRef,
  /**
   * The title of the release it shows. `null` until the library has one.
   */
  title: string | null,
  /**
   * Who it is by.
   */
  artistCredit: Credit | null,
  /**
   * The year it first came out, for a group shown as one tile, or the
   * release's own year.
   */
  year: number | null,
  /**
   * How many tracks the release it shows has.
   */
  trackCount: number,
  /**
   * How long they are together.
   */
  durationMs: number,
  /**
   * How many versions its group has. 1 for a record with one.
   */
  versionCount: number,
  /**
   * Its cover, for `GET /v1/art/{artId}/{size}`. `null` when it has none.
   */
  artId: string | null,
  /**
   * The best copy across its tracks. `null` when none of its tracks has
   * a copy the library has read.
   */
  quality: Quality | null,
};

/**
 * What an Albums tile points at.
 */
export type AlbumRef = { "type": "release_group",
  /**
   * The release group.
   */
  id: string, } | { "type": "release",
  /**
   * The release.
   */
  id: string,
};

/**
 * One album as a preview shows it.
 */
export type AlbumShape = {
  /**
   * The title its tile shows.
   */
  title?: string,
  /**
   * The credit its tile shows.
   */
  artist?: string,
  /**
   * Its kind.
   */
  primaryType?: string,
  /**
   * The year it first came out.
   */
  firstYear?: number,
  /**
   * How many releases it holds.
   */
  releases: number,
  /**
   * How many discs its fullest release has.
   */
  discs: number,
  /**
   * How many songs are on it.
   */
  songs: number,
};

/**
 * One tile of the Albums list.
 */
export type AlbumTile = {
  /**
   * What the tile is.
   */
  ref: AlbumRef,
  /**
   * The title of the release it shows. `null` until the library has one.
   */
  title: string | null,
  /**
   * Who it is by.
   */
  artistCredit: Credit | null,
  /**
   * The year it first came out, for a group shown as one tile, or the
   * release's own year.
   */
  year: number | null,
  /**
   * How many tracks the release it shows has.
   */
  trackCount: number,
  /**
   * How long they are together.
   */
  durationMs: number,
  /**
   * How many versions its group has. 1 for a record with one.
   */
  versionCount: number,
  /**
   * Its cover, for `GET /v1/art/{artId}/{size}`. `null` when it has none.
   */
  artId: string | null,
  /**
   * The best copy across its tracks. `null` when none of its tracks has
   * a copy the library has read.
   */
  quality: Quality | null,
};

/**
 * One version of a record: a release of its group.
 */
export type AlbumVersion = {
  /**
   * The release, for `album.get`.
   */
  releaseId: string,
  /**
   * Its title.
   */
  title: string | null,
  /**
   * The year it came out.
   */
  year: number | null,
  /**
   * How many tracks it has.
   */
  trackCount: number,
  /**
   * The best copy across its tracks. `null` when none has a copy the
   * library has read.
   */
  quality: Quality | null,
  /**
   * Whether it is the one the group is set to show.
   */
  shown: boolean,
};

/**
 * One answer in the ledger.
 */
export type Answer = {
  /**
   * Its ID.
   */
  answerId: string,
  /**
   * What it said.
   */
  verb: AnswerVerb,
  /**
   * For a yes: the proposal it answered.
   */
  proposalId?: string,
  /**
   * For a yes: the album it said yes to, by its release group ID.
   */
  releaseGroup?: string,
  /**
   * For a yes: the album's title, as it was shown.
   */
  title?: string,
  /**
   * For an undo: the answer it undid.
   */
  undoes?: string,
  /**
   * For a yes that was undone: the undo.
   */
  undoneBy?: string,
  /**
   * How many files it put values on.
   */
  files: number,
  /**
   * When it was given, as Unix milliseconds.
   */
  answeredAtMs: number,
};

/**
 * What an answer rested on and said, kept by value when it was given.
 */
export type AnswerEvidence = {
  /**
   * For a yes: the album said yes to, as it was shown.
   */
  candidate?: AnsweredCandidate,
  /**
   * For a yes: the albums it was chosen among, in the order shown.
   */
  candidates: Array<AnsweredCandidate>,
  /**
   * For a yes: each disc it rested on, with its reply.
   */
  replies: Array<AnsweredReply>,
  /**
   * For a yes: each layout of those discs.
   */
  layouts: Array<AnsweredLayout>,
  /**
   * For a yes: the values it put on the files, each with the file as it
   * is named now.
   */
  values: Array<FileValue>,
  /**
   * For an undo of an answer that rested on others: the answers it was
   * undone with.
   */
  restsOn: Array<string>,
};

/**
 * The result of `corrections.answer`: one answer, and what it rested on.
 */
export type AnswerGot = {
  /**
   * The library revision it read.
   */
  libraryRevision: number,
  /**
   * The answer.
   */
  answer: Answer,
  /**
   * What it rested on and said.
   */
  evidence: AnswerEvidence,
};

/**
 * The result of `corrections.answers`: one page of the ledger, newest
 * first.
 */
export type AnswerList = {
  /**
   * The library revision it read.
   */
  libraryRevision: number,
  /**
   * Each answer, newest first.
   */
  answers: Array<Answer>,
  /**
   * The `before` that reads the next page, while there is one.
   */
  next?: string,
};

/**
 * The params of `corrections.answer`.
 */
export type AnswerParams = {
  /**
   * The answer.
   */
  answerId: string,
};

/**
 * What an answer said.
 */
export type AnswerVerb = "confirm" | "undo";

/**
 * One answer a command wrote.
 */
export type AnswerWritten = {
  /**
   * The answer's ID.
   */
  answerId: string,
  /**
   * The proposal a yes answered.
   */
  proposalId?: string,
  /**
   * The answer an undo undid.
   */
  undoes?: string,
};

/**
 * The result of `corrections.confirm` and `corrections.undo`.
 */
export type Answered = {
  /**
   * Each answer written.
   */
  answers: Array<AnswerWritten>,
  /**
   * Whether anything shown changed: an album, a song or an artist.
   */
  moved: boolean,
  /**
   * The command's ID: the idempotency key it was sent with.
   */
  commandId: string,
  /**
   * How far the command has got.
   */
  status: ReceiptStatus,
  /**
   * The library revision the command was committed at. It is left out
   * for a command that changes no library.
   */
  revision?: number,
};

/**
 * An album a yes was given among, as it was shown: all of it for the one
 * said yes to, its ID and title for the others.
 */
export type AnsweredCandidate = {
  /**
   * Its release group ID.
   */
  releaseGroup: string,
  /**
   * Its title.
   */
  title?: string,
  /**
   * Who it is credited to.
   */
  artist?: string,
  /**
   * Its kind: `album`, `single`, `ep`.
   */
  primaryType?: string,
  /**
   * When it first came out.
   */
  firstDate?: string,
  /**
   * How many of the replies' releases were of it.
   */
  releases?: number,
};

/**
 * A layout of a disc a yes rested on, as it was then.
 */
export type AnsweredLayout = {
  /**
   * The disc ID it gives.
   */
  discId: string,
  /**
   * The folder it is of, by its path under its root.
   */
  folder: string,
  /**
   * Each way it was found, in the order found.
   */
  methods: Array<LayoutMethod>,
  /**
   * Whether it may not be the disc's.
   */
  provisional: boolean,
};

/**
 * A disc a yes rested on: its disc ID, and when the reply it was read
 * from came.
 */
export type AnsweredReply = {
  /**
   * The disc ID.
   */
  discId: string,
  /**
   * When MusicBrainz replied, as Unix milliseconds.
   */
  repliedAtMs: number,
};

/**
 * The params of `corrections.answers`: one page of the ledger.
 */
export type AnswersParams = {
  /**
   * The answers older than this one; from the newest when left out.
   */
  before?: string,
  /**
   * How many at most: 50 when left out, and never more than 500.
   */
  limit?: number,
};

/**
 * A run the library holds: one archive applied.
 */
export type AppliedRun = {
  /**
   * The run.
   */
  runId: string,
  /**
   * The streams its archive held.
   */
  streams: number,
  /**
   * Of those, the ones it stored: an earlier run stored the rest.
   */
  newStreams: number,
  /**
   * Whether it is in force. A stream is in force while any run that
   * held it is.
   */
  inForce: boolean,
  /**
   * When it was applied.
   */
  appliedAt: number,
  /**
   * When it was last undone.
   */
  undoneAt?: number,
  /**
   * What read its archive.
   */
  reader: string,
};

/**
 * The colour a picture is mostly made of: the hue it has most of,
 * counting each pixel by how coloured it is, and how strong the colour
 * is there. It is a hue and not a colour to paint with: a client sets its
 * lightness for its own palette, so that it reads on a dark window and a
 * light one alike. `null` where it is given is a picture that is close to
 * grey all over, or one that could not be read.
 */
export type ArtColor = {
  /**
   * The hue, in degrees from 0 to 359.
   */
  hue: number,
  /**
   * How strong the colour is where it is that hue, from 0 to 100.
   */
  saturation: number,
};

/**
 * The params of `artist.get`.
 */
export type ArtistGetParams = {
  /**
   * The artist.
   */
  artistId: string,
};

/**
 * The result of `artist.get`: one artist, as the Artists list has them,
 * and the colour of their picture.
 */
export type ArtistPage = {
  /**
   * The colour of that picture. `null` when it has none, or it is close
   * to grey.
   */
  artColor: ArtColor | null,
  /**
   * The library revision this was read at.
   */
  libraryRevision: number,
  /**
   * The artist.
   */
  artistId: string,
  /**
   * Their name, as displayed. `null` until the library has one.
   */
  name: string | null,
  /**
   * The name they are filed under, where it differs from how it is
   * written, such as "Beatles, The".
   */
  sortName: string | null,
  /**
   * How many records name them in their credit, counted as the Albums
   * list shows them.
   */
  albumCount: number,
  /**
   * How many songs on those records there are, and loose songs of
   * theirs.
   */
  trackCount: number,
  /**
   * Their picture: their portrait where the library has one, else the
   * cover of their earliest record that has one. `null` when neither.
   */
  artId: string | null,
  /**
   * Whether `artId` is their portrait, not a cover.
   */
  portrait: boolean,
};

/**
 * One artist.
 */
export type ArtistRow = {
  /**
   * The artist.
   */
  artistId: string,
  /**
   * Their name, as displayed. `null` until the library has one.
   */
  name: string | null,
  /**
   * The name they are filed under, where it differs from how it is
   * written, such as "Beatles, The".
   */
  sortName: string | null,
  /**
   * How many records name them in their credit, counted as the Albums
   * list shows them.
   */
  albumCount: number,
  /**
   * How many songs on those records there are, and loose songs of
   * theirs.
   */
  trackCount: number,
  /**
   * Their picture: their portrait where the library has one, else the
   * cover of their earliest record that has one. `null` when neither.
   */
  artId: string | null,
  /**
   * Whether `artId` is their portrait, not a cover.
   */
  portrait: boolean,
};

/**
 * A rescan or an import the server started on its own.
 */
export type AutoRun = {
  /**
   * What started it.
   */
  trigger: AutoTrigger,
  /**
   * When it started, or was queued, as Unix milliseconds.
   */
  startedAt: number,
  /**
   * When it ended, once it has.
   */
  finishedAt?: number,
  /**
   * Where it is. An import that found nothing to change succeeded.
   */
  state: JobState,
  /**
   * The rescan's job. An import is no job, and has none.
   */
  jobId?: string,
};

/**
 * What starts a rescan or an import on its own.
 */
export type AutoTrigger = "change" | "lost" | "returned" | "start" | "timer";

/**
 * When a hub keeps its machine from sleeping for its linked servers: a
 * preference of the server's own, set with `server.setSettings` and kept
 * with it. The display may sleep in every mode.
 */
export type Awake = { "mode": "while_active",
  /**
   * How long after a linked server's last call.
   */
  minutes: number, } | { "mode": "always" } | { "mode": "never" };

/**
 * The result of `server.backup`: the receipt, and the backup.
 */
export type BackedUp = {
  /**
   * The backup that was written.
   */
  backup: BackupMade,
  /**
   * The command's ID: the idempotency key it was sent with.
   */
  commandId: string,
  /**
   * How far the command has got.
   */
  status: ReceiptStatus,
  /**
   * The library revision the command was committed at. It is left out
   * for a command that changes no library.
   */
  revision?: number,
};

/**
 * A backup that could not be written.
 */
export type BackupFailure = {
  /**
   * When it failed, as Unix milliseconds.
   */
  at: number,
  /**
   * What went wrong, as a person reads it.
   */
  message: string,
};

/**
 * A backup that was written.
 */
export type BackupMade = {
  /**
   * When it was taken, as Unix milliseconds.
   */
  at: number,
  /**
   * Its folder, on the server's own disk.
   */
  path: string,
  /**
   * Why it was taken.
   */
  reason: BackupReason,
  /**
   * Whether its folder is still there.
   */
  present: boolean,
};

/**
 * The params of `server.backup`.
 */
export type BackupParams = {
  /**
   * The command's ID, made by the client.
   */
  idempotencyKey: string,
};

/**
 * Why a backup was taken.
 */
export type BackupReason = "on_demand" | "before_migration";

/**
 * Where a server writes its backups, and the last it wrote. A backup is a
 * copy of the server's databases in a folder of its own, written to a
 * place outside the data directory that has to be named: with none named
 * there is no backup. See "Backups and pre-migration snapshots" in
 * docs/internals/server.md.
 */
export type BackupStatus = {
  /**
   * The folder backups are written to, on the server's own disk. Left
   * out when none is named.
   */
  place?: string,
  /**
   * The last backup written. Left out when there has been none.
   */
  last?: BackupMade,
  /**
   * The last backup that could not be written, when none has been
   * written since.
   */
  failed?: BackupFailure,
};

/**
 * One side of a dry run's comparison: what an item is bound to, and how
 * that was decided.
 */
export type Binding = {
  /**
   * The recording, after redirects. Left out for one the run would
   * create.
   */
  recordingId?: string,
  /**
   * How it was decided: `user`, `same_audio`, `identifier`,
   * `tags_duration` or `new`. Left out where nothing was decided.
   */
  method?: string,
  /**
   * The distance it was decided at: 0 for an exact method.
   */
  score?: number,
  /**
   * Whether it is marked for review.
   */
  needsReview: boolean,
  /**
   * The release it is on. Left out for a loose track, and for one the
   * run would create.
   */
  releaseId?: string,
  /**
   * That release's title.
   */
  releaseTitle?: string,
  /**
   * The disc it is on there.
   */
  disc?: number,
  /**
   * The candidate it was nearest to, for a decision by tags.
   */
  nearest?: Nearest,
};

/**
 * One album a reply names.
 */
export type CatalogueAlbum = {
  /**
   * Its MusicBrainz release group ID.
   */
  releaseGroup: string,
  /**
   * Its title.
   */
  title: string,
  /**
   * Who it is credited to, as written.
   */
  artist: string,
  /**
   * The year it was first released, when the catalogue knows.
   */
  firstYear?: number,
  /**
   * How many of the releases the reply names are of this album.
   */
  releases: number,
};

/**
 * One proposal with the candidate to preview it with.
 */
export type Choice = {
  /**
   * The proposal.
   */
  proposalId: string,
  /**
   * The candidate, by its release group ID. Left out, the likeliest.
   */
  releaseGroup?: string,
};

/**
 * What sort of program a client is. It is for logs and for display, and
 * never decides access: the session does.
 */
export type ClientKind = "desktop" | "browser" | "phone" | "cli" | "agent" | "server";

/**
 * The params of `corrections.confirm`.
 */
export type ConfirmParams = {
  /**
   * The command's ID, made by the client.
   */
  idempotencyKey: string,
  /**
   * Each yes, all kept in one write or none.
   */
  answers: Array<Confirmation>,
};

/**
 * A yes to one proposal's candidate, naming the proposal as it was shown.
 */
export type Confirmation = {
  /**
   * The proposal.
   */
  proposalId: string,
  /**
   * The candidate, by its release group ID.
   */
  releaseGroup: string,
  /**
   * The proposal's fingerprint as it was shown. A proposal that has
   * changed since is refused, and nothing is written.
   */
  fingerprint: string,
};

/**
 * Why tracks were queued: a kind, and what it refers to.
 */
export type ContextKind = "manual" | "album" | "artist" | "playlist" | "query" | "agent";

/**
 * The copy the player has open for the current entry.
 */
export type CopyInUse = {
  /**
   * The server the copy is on.
   */
  serverId: string,
  /**
   * What it is to the track: currently `master`.
   */
  role: string,
  /**
   * Its codec.
   */
  codec: string,
  /**
   * Its sample rate.
   */
  sampleRateHz: number,
  /**
   * Its channels.
   */
  channels: number,
  /**
   * Its bit depth, where the codec has one.
   */
  bitDepth?: number,
};

/**
 * Which copy of a track a copy is.
 */
export type CopyRole = "master" | "derived";

/**
 * The result of `corrections.summary`.
 */
export type CorrectionsSummary = {
  /**
   * The library revision it read.
   */
  libraryRevision: number,
  /**
   * Whether the library may be asked about at MusicBrainz.
   */
  lookups: boolean,
  /**
   * Open proposals.
   */
  open: number,
  /**
   * How many groups they make.
   */
  groups: number,
  /**
   * Of the open proposals, those whose likeliest candidate moves files.
   */
  moves: number,
  /**
   * Those that add a kind or a first year.
   */
  gains: number,
  /**
   * Those that add only an ID.
   */
  knows: number,
};

/**
 * One named count.
 */
export type Count = {
  /**
   * What was counted.
   */
  name: string,
  /**
   * How many.
   */
  count: number,
};

/**
 * An artist credit as it is written, and the artists it names.
 */
export type Credit = {
  /**
   * The whole credit as one string, such as "Simon & Garfunkel".
   */
  text: string,
  /**
   * The artists it names, in order.
   */
  artistIds: Array<string>,
};

/**
 * How many scrobbles are in one state.
 */
export type DeliveryCount = {
  /**
   * The state.
   */
  state: string,
  /**
   * How many.
   */
  count: number,
};

/**
 * The descriptor: the result of `server.hello`. It is the public
 * descriptor, and what the server says to this session alone.
 */
export type Descriptor = {
  /**
   * What this session may do.
   */
  sessionKind: SessionKind,
  /**
   * The server's ID: made on first start and never changed.
   */
  serverId: string,
  /**
   * The server's display name. Text from another machine: display only.
   */
  name: string,
  /**
   * The server's role.
   */
  role: ServerRole,
  /**
   * The [contract version](crate::CONTRACT_VERSION) the server speaks.
   */
  contractVersion: number,
  /**
   * The capability flags: what this server can do beyond the calls
   * every server has. A client gates a feature on its flag, never on a
   * version, and ignores a flag it does not know. See
   * [`capability`](crate::capability).
   */
  capabilities: Array<string>,
  /**
   * The limits a client must stay within.
   */
  limits: Limits,
  /**
   * What each kind of list supports: its orders and the predicates a
   * filter on it may use. Empty from a server that has no lists.
   */
  lists: Array<ListSupport>,
  /**
   * The sizes a cover is served at, in pixels on its longer side: what
   * `{size}` in `GET /v1/art/{artId}/{size}` may be. Empty from a server
   * that serves no covers.
   */
  artSizes: Array<number>,
  /**
   * The server's Ed25519 public key, as 64 hex digits: what a client or
   * a peer pins. Left out by a server that has none to give.
   */
  publicKey?: string,
  /**
   * The signature, as 128 hex digits, of [`descriptor_proof`] for the
   * nonce the caller sent as `?nonce=`: proof that the answer came from
   * the holder of `public_key`. Left out when no nonce was sent.
   */
  proof?: string,
};

/**
 * The hardware the sound came out of.
 */
export type DeviceKind = "computer" | "phone" | "tablet" | "watch" | "speaker" | "tv" | "car" | "console" | "group" | "other";

/**
 * One layout of one folder's files: one CD, as its sidecars give it.
 */
export type Disc = {
  /**
   * The root the folder is in, by its name.
   */
  root: string,
  /**
   * The folder, by its path under its root: empty for the root.
   */
  folder: string,
  /**
   * The MusicBrainz disc ID the layout gives.
   */
  discId: string,
  /**
   * How many tracks it has.
   */
  tracks: number,
  /**
   * How long it is, in CD sectors.
   */
  sectors: number,
  /**
   * Each way it was found, in the order found.
   */
  methods: Array<LayoutMethod>,
  /**
   * Whether the layout may not be the disc's: a CUE sheet that disagrees
   * with the CDDB ID it prints.
   */
  provisional: boolean,
  /**
   * Whether one file holds the whole disc.
   */
  wholeFile: boolean,
  /**
   * Whether every track was matched to one file by its length.
   */
  matched: boolean,
  /**
   * The file at each position, when one folder was asked for. Empty
   * otherwise.
   */
  files: Array<DiscFile>,
};

/**
 * One file at one position of a disc.
 */
export type DiscFile = {
  /**
   * The track's position on the disc, from 1, or 0 for a file that holds
   * the whole disc.
   */
  position: number,
  /**
   * The file, by its path under its root.
   */
  path: string,
};

/**
 * The result of `corrections.discs`.
 */
export type DiscList = {
  /**
   * The library revision it read.
   */
  libraryRevision: number,
  /**
   * How many folders hold audio, of those asked about.
   */
  folders: number,
  /**
   * How many of those have at least one layout that is not provisional.
   */
  withLayout: number,
  /**
   * Each layout, by root, folder and disc ID.
   */
  discs: Array<Disc>,
};

/**
 * One disc a lookup was about, and what its current reply says.
 */
export type DiscLookup = {
  /**
   * The disc ID asked about.
   */
  discId: string,
  /**
   * What became of it.
   */
  outcome: LookupOutcome,
  /**
   * Why asking failed: one line that can be shown.
   */
  failure?: string,
  /**
   * The albums its current reply names, if it has one.
   */
  albums: Array<CatalogueAlbum>,
};

/**
 * The params of `corrections.discs`.
 */
export type DiscsParams = {
  /**
   * One folder, by its path under its root, with `/` between its parts.
   * Its discs come with the file at each position. Left out, every
   * disc of the library comes, without its files.
   */
  folder?: string,
};

/**
 * The params of `record.dismiss`.
 */
export type DismissParams = {
  /**
   * The command's ID, made by the client.
   */
  idempotencyKey: string,
};

/**
 * The result of `record.dismiss`: the receipt, and the record as the
 * status now says it.
 */
export type Dismissed = { record: RecordStatus,
  /**
   * The command's ID: the idempotency key it was sent with.
   */
  commandId: string,
  /**
   * How far the command has got.
   */
  status: ReceiptStatus,
  /**
   * The library revision the command was committed at. It is left out
   * for a command that changes no library.
   */
  revision?: number,
};

/**
 * A scrobbled track the matcher was not sure of.
 */
export type DoubtfulTrack = {
  /**
   * The artist, as Last.fm spells it.
   */
  artist: string,
  /**
   * The album.
   */
  album?: string,
  /**
   * The title.
   */
  title: string,
  /**
   * How often it was played.
   */
  plays: number,
  /**
   * Why it was not bound: `several`, when the artist and title name
   * several recordings the album cannot tell apart; `album`, when they
   * name one and the album is none it is on, so the play may be of
   * another version; or `near`, when nothing matches exactly but
   * something does without accents and bracketed notes.
   */
  doubt: string,
  /**
   * What it nearly matched.
   */
  candidates: Array<NearlyMatched>,
};

/**
 * The result of `identity.dryRun`: the resolver run again over the stored
 * revisions, compared with the assignments in force. Nothing is written.
 */
export type DryRun = {
  /**
   * The policy it decided by.
   */
  policy: PolicyValues,
  /**
   * The library revision it read.
   */
  libraryRevision: number,
  /**
   * Whether the claims it read were made by another version of the
   * extractor, which the next scan rebuilds before it resolves.
   */
  claimsStale: boolean,
  /**
   * Whether applying the result would write anything at all, what the
   * catalog shows included.
   */
  wouldWrite: boolean,
  /**
   * Files the library has, retired ones included.
   */
  items: number,
  /**
   * Of them, those whose result is the assignment they have. A file
   * that cannot be read, or is retired, is one.
   */
  unchanged: number,
  /**
   * Those whose result differs.
   */
  wouldChange: number,
  /**
   * Of those, the ones bound to another recording. The rest keep their
   * recording and move on a release.
   */
  recordingsChanged: number,
  /**
   * Those the owner's own assignment holds.
   */
  pinned: number,
  /**
   * Those an owner's exclusion kept from a candidate.
   */
  blocked: number,
  /**
   * Recordings that would be joined to another: for each recording of
   * the result, the recordings its files have now, less one.
   */
  merges: number,
  /**
   * Recordings that would be parted: for each recording files have now,
   * the recordings of the result they would have, less one.
   */
  splits: number,
  /**
   * Files whose binding in force is marked for review.
   */
  reviewsNow: number,
  /**
   * Files the run's decisions mark for review.
   */
  reviewsAfter: number,
  /**
   * Files marked now and not by the run.
   */
  reviewsCleared: number,
  /**
   * Files marked by the run and not now.
   */
  reviewsAdded: number,
  /**
   * Releases with a file on them now.
   */
  releasesNow: number,
  /**
   * Releases the run's files would be on.
   */
  releasesAfter: number,
  /**
   * With `detail`, every item that is not unchanged, in first-seen
   * order.
   */
  changes: Array<ItemChange>,
};

/**
 * The params of `identity.dryRun`.
 */
export type DryRunParams = {
  /**
   * What to decide by: the policy scans use, unless it names another
   * or puts values in place of its own.
   */
  policy: PolicyOverrides,
  /**
   * Whether to list every item that is not unchanged. Without it the
   * answer is counts alone.
   */
  detail: boolean,
};

/**
 * What went wrong, from a closed set. A client decides what to do by this
 * and never by the message.
 */
export type ErrorCode = "invalid" | "not_found" | "conflict" | "plan_stale" | "view_expired" | "unavailable" | "forbidden" | "unsupported" | "idempotency_mismatch" | "internal";

/**
 * The `data` of an error: what a client acts on.
 */
export type ErrorData = {
  /**
   * What went wrong.
   */
  code: ErrorCode,
  /**
   * Whether sending the same call again later may succeed.
   */
  retryable: boolean,
  /**
   * What the client needs to act on the error, by code. It is an empty
   * object when there is nothing to add.
   */
  details: { [key in string]: JsonValue },
};

/**
 * The params of an event notification: the topic, and what it carries.
 */
export type Event = { "topic": "servers" } & ServersEvent | { "topic": "jobs" } & JobsEvent | { "topic": "library" } & LibraryEvent | { "topic": "list" } & ListEvent | { "topic": "playback" } & PlaybackEvent | { "topic": "progress" } & ProgressEvent | { "topic": "queue" } & QueueEvent;

/**
 * One thing worth saying about what is playing, as Lean back sets it: a
 * line in large type and a smaller one under it.
 */
export type Fact = {
  /**
   * What it is about.
   */
  kind: FactKind,
  /**
   * The line in large type, with what it turns on set apart.
   */
  head: Array<Phrase>,
  /**
   * The line under it. Left out when there is none.
   */
  more?: string,
};

/**
 * What a fact Lean back shows is about, so that a client can tell them
 * apart.
 */
export type FactKind = "song_plays" | "first_heard" | "its_year" | "time_of_day" | "on_this_day" | "played_most" | "not_since";

/**
 * A file as the owner judges it: where it is and what its tags say.
 */
export type FileFacts = {
  /**
   * The source item's ID.
   */
  itemId: string,
  /**
   * The file's path under its root.
   */
  path: string,
  /**
   * The title its tags give, or its file name gives.
   */
  title?: string,
  /**
   * The artist its tags give.
   */
  artist?: string,
  /**
   * The album its tags give.
   */
  album?: string,
  /**
   * The disc number its tags give.
   */
  disc?: number,
  /**
   * The track number its tags give.
   */
  track?: number,
  /**
   * Its length in milliseconds, from the audio read.
   */
  durationMs?: number,
};

/**
 * One tag value a yes would put on one file.
 */
export type FileValue = {
  /**
   * The file's ID.
   */
  fileId: string,
  /**
   * The root the file is in, by its name.
   */
  root: string,
  /**
   * The file, by its path under its root.
   */
  path: string,
  /**
   * The field, named as tags are read: `album`, `album_artist`,
   * `mb_album_artist`, `disc_number`, `mb_release_group`,
   * `primary_type` or `original_date`.
   */
  field: string,
  /**
   * Its position among the field's values, from 0.
   */
  ordinal: number,
  /**
   * The value, or nothing when the yes clears the field.
   */
  value?: string,
  /**
   * What the file says for it now, as the catalog reads it. Only from
   * `corrections.preview`.
   */
  now?: string,
};

/**
 * A filter: a versioned query tree ("Large lists" in
 * docs/internals/contract.md). Version 1 supports the [`Predicate`] variants
 * below.
 */
export type Filter = {
  /**
   * The version of the filter language it is written in. A server
   * refuses a version it does not read with `unsupported`.
   */
  version: number,
  /**
   * What a row must satisfy.
   */
  where: Predicate,
};

/**
 * A format, as a copy of a track has it.
 */
export type Format = {
  /**
   * The codec, such as `flac` or `mp3`.
   */
  codec: string,
  /**
   * Whether it is lossless.
   */
  lossless: boolean,
  /**
   * Bits a sample, for a format that has them.
   */
  bitDepth: number | null,
  /**
   * Samples a second.
   */
  sampleRateHz: number,
  /**
   * The bit rate, for a lossy format.
   */
  bitrateKbps: number | null,
};

/**
 * Free disk space, in bytes.
 */
export type FreeDisk = {
  /**
   * What this server may still write on the disk that holds its data
   * directory. It is left out when the system would not say.
   */
  dataBytes?: number,
};

/**
 * A row the library no longer has.
 */
export type GoneRow = {
  /**
   * Always `true`.
   */
  gone: boolean,
};

/**
 * The body of [`HEALTH`].
 */
export type Health = {
  /**
   * Always `ok`: a server that is not answers nothing.
   */
  status: HealthStatus,
};

/**
 * What a health check reports.
 */
export type HealthStatus = "ok";

/**
 * One album heard, with files or not.
 */
export type HeardAlbum = {
  /**
   * Its artist's key and its own, which `history.album` takes.
   */
  artistKey: string, key: string,
  /**
   * Its artist's name and its own.
   */
  artistName: string, name: string,
  /**
   * The library's record, when it is one.
   */
  albumId?: string,
  /**
   * The plays.
   */
  plays: number,
  /**
   * The local date of the first, as `YYYY-MM-DD`, when one has a date.
   */
  firstPlayed?: string,
  /**
   * The local date of the last, as `YYYY-MM-DD`, when one has a date.
   */
  lastPlayed?: string,
};

/**
 * The result of `history.albums`: every album heard, as asked.
 */
export type HeardAlbums = {
  /**
   * This page of them.
   */
  albums: Array<HeardAlbum>,
  /**
   * How many there are in all.
   */
  total: number,
  /**
   * The library revision this was read at.
   */
  libraryRevision: number,
};

/**
 * One artist heard, with files or not.
 */
export type HeardArtist = {
  /**
   * Their name's key, which `history.artist` takes.
   */
  key: string,
  /**
   * Their name: the library's, or as their plays spell it most.
   */
  name: string,
  /**
   * The library's artist, when they are one.
   */
  artistId?: string,
  /**
   * The plays.
   */
  plays: number,
  /**
   * The local date of the first, as `YYYY-MM-DD`, when one has a date.
   */
  firstPlayed?: string,
  /**
   * The local date of the last, as `YYYY-MM-DD`, when one has a date.
   */
  lastPlayed?: string,
};

/**
 * The result of `history.artists`: every artist heard, as asked.
 */
export type HeardArtists = {
  /**
   * This page of them.
   */
  artists: Array<HeardArtist>,
  /**
   * How many there are in all.
   */
  total: number,
  /**
   * The library revision this was read at.
   */
  libraryRevision: number,
};

/**
 * The params of `history.artists` and `history.albums`.
 */
export type HeardListParams = {
  /**
   * The order. Played most first when left out.
   */
  order?: HeardOrder,
  /**
   * How many to give, at most [`MAX_HEARD`]. [`DEFAULT_HEARD`] when left
   * out.
   */
  limit?: number,
  /**
   * How many to pass over first. None when left out.
   */
  offset?: number,
};

/**
 * How `history.artists` and `history.albums` order what they list.
 */
export type HeardOrder = "plays" | "first" | "last";

/**
 * The params of `server.hello`: who is calling.
 */
export type HelloParams = {
  /**
   * The program's name, such as `ritornelctl`. At most
   * [`MAX_CLIENT_TEXT`] characters, and not empty.
   */
  name: string,
  /**
   * What sort of program it is.
   */
  kind: ClientKind,
  /**
   * The program's own version. At most [`MAX_CLIENT_TEXT`] characters,
   * and not empty.
   */
  version: string,
  /**
   * The [contract version](crate::CONTRACT_VERSION) the program was
   * built with. A server refuses one that is not its own.
   */
  contractVersion: number,
};

/**
 * The params of `history.album`: the library's album, by its record or
 * one release, or an album heard, by its artist's key and its own, as
 * `history.albums` gives them. Keys that name exactly one of the library's
 * records answer with that record.
 */
export type HistoryAlbumParams = {
  /**
   * The library's album.
   */
  ref?: AlbumRef,
  /**
   * An album heard: its artist's key.
   */
  artistKey?: string,
  /**
   * An album heard: its own key, under its artist.
   */
  key?: string,
};

/**
 * The params of `history.artist`: the library's artist, by its ID, or
 * an artist heard, by their name's key as `history.artists` gives it. A
 * key that names exactly one of the library's artists a name can be (one
 * with a record, one a bound song's credit names, or one credited on a
 * record heard from) answers with that artist, so a key kept from before
 * their record was bought still arrives.
 */
export type HistoryArtistParams = {
  /**
   * The library's artist.
   */
  artistId?: string,
  /**
   * An artist heard, by their name's key.
   */
  key?: string,
};

/**
 * The params of `history.export`.
 */
export type HistoryExportParams = {
  /**
   * The command's ID, made by the client.
   */
  idempotencyKey: string,
  /**
   * The folder, on the server's own disk, an absolute path, that the
   * export's own folder is made in. It must be there.
   */
  folder: string,
};

/**
 * The result of `history.export`: where the record was written, and how
 * much of it.
 */
export type HistoryExported = {
  /**
   * The export's folder, on the server's own disk.
   */
  path: string,
  /**
   * The format's version, in its `manifest.json` too.
   */
  version: number,
  /**
   * How many observations: listens, import records and corrections.
   */
  observations: number,
  /**
   * How many ratings and loves, each one said or cleared.
   */
  ratings: number,
  /**
   * How many plays sent, or to be sent, to a service such as Last.fm.
   */
  deliveries: number,
  /**
   * How many plays in the table derived from them.
   */
  plays: number,
  /**
   * The command's ID: the idempotency key it was sent with.
   */
  commandId: string,
  /**
   * How far the command has got.
   */
  status: ReceiptStatus,
  /**
   * The library revision the command was committed at. It is left out
   * for a command that changes no library.
   */
  revision?: number,
};

/**
 * The result of `history.artist` and `history.album`: what a page says of
 * the listener's history with an artist or an album.
 *
 * It counts the plays that count and are not excluded. Of the library's
 * artist or album, they are of songs the library has bound them to: an
 * artist's are the plays of their songs, by the credit of the appearance
 * each was heard from or else its recording's; an album's are those heard
 * from one of its versions. Of a name heard that is none of the
 * library's, they are the plays heard under it ("Queries" in
 * docs/internals/history.md). It is read again whenever plays change: a
 * binding made later moves them here.
 */
export type HistoryOf = {
  /**
   * The plays, dated or not.
   */
  plays: number,
  /**
   * The local date of the first play, as `YYYY-MM-DD`. Left out when the
   * first play has no local date, though a later one has. A play whose
   * instant is after now, by a clock that was wrong, is neither the
   * first nor the last, so both are left out when every play is.
   */
  firstPlayed?: string,
  /**
   * The local date of the last play, as `YYYY-MM-DD`. Left out when the
   * last play has no local date, though an earlier one has.
   */
  lastPlayed?: string,
  /**
   * The plays in the 365 days before now.
   */
  lastYear: number,
  /**
   * The dated plays by year, oldest first. A year with none is left out,
   * and so is a play whose instant is after now.
   */
  years: Array<YearPlays>,
  /**
   * The years the listener's whole history runs over. Left out while
   * there is no dated play at all, of anything.
   */
  span?: YearSpan,
  /**
   * For an artist, the records played most, most first, at most six. For
   * an album, none.
   */
  records: Array<RecordPlayed>,
  /**
   * The one sentence a page says of it, in its banner. Empty when there
   * is nothing true to say, which is when there are no plays.
   */
  sentence: Array<Phrase>,
  /**
   * The library revision this was read at.
   */
  libraryRevision: number,
};

/**
 * The params of `history.rebuild`.
 */
export type HistoryRebuildParams = {
  /**
   * The command's ID, made by the client.
   */
  idempotencyKey: string,
  /**
   * Say what would change, and change nothing.
   */
  dryRun: boolean,
};

/**
 * The result of `history.rebuild`.
 */
export type HistoryRebuilt = {
  /**
   * Whether nothing was changed.
   */
  dryRun: boolean,
  /**
   * Plays before.
   */
  before: number,
  /**
   * Plays after.
   */
  after: number,
  /**
   * Plays that are new, gone or different.
   */
  changed: number,
  /**
   * The command's ID: the idempotency key it was sent with.
   */
  commandId: string,
  /**
   * How far the command has got.
   */
  status: ReceiptStatus,
  /**
   * The library revision the command was committed at. It is left out
   * for a command that changes no library.
   */
  revision?: number,
};

/**
 * The params of `history.recent`.
 */
export type HistoryRecentParams = {
  /**
   * How many plays, at most 500. The default is 20.
   */
  limit?: number,
  /**
   * Only plays after this one, newest first: the `next` of the page
   * before.
   */
  before?: PlayCursor,
};

/**
 * The result of `history.stream`: the listener's whole history as the
 * History tab draws it, a stream across the months in layers by the year
 * each artist was first heard ("History tab and Lean back" in
 * docs/internals/desktop.md).
 *
 * It counts every dated play that counts and is not excluded, bound to the
 * library or not: History is the record of all of it. A play is the
 * artist's its source named, and an artist is known by their name,
 * whatever its case. A day of more than 300 plays is a device catching up,
 * not listening, and is left out.
 */
export type HistoryStream = {
  /**
   * The first month, as `YYYY-MM`. Left out while there is no history.
   */
  from?: string,
  /**
   * How many months there are, from the first to this one.
   */
  months: number,
  /**
   * All the plays in each month.
   */
  total: Array<number>,
  /**
   * The layers, the oldest year first.
   */
  layers: Array<StreamLayer>,
  /**
   * The artists the layers name.
   */
  artists: Array<string>,
  /**
   * The days left out, as devices catching up.
   */
  daysLeftOut: number,
  /**
   * The library revision this was read at.
   */
  libraryRevision: number,
};

/**
 * One master a hub holds of a track.
 */
export type HubCopy = {
  /**
   * The master's source item: what a listen names.
   */
  sourceItemId: string,
  /**
   * The revision of it the scan last read.
   */
  revision: number,
  /**
   * The audio asset it plays as.
   */
  assetId: string,
  /**
   * Its length, as the scan read it. 0 when unknown.
   */
  durationMs: number,
  /**
   * Its size in bytes.
   */
  sizeBytes: number,
  /**
   * The file's extension, in lower case, which a decoder may go by.
   */
  extension: string,
};

/**
 * A satellite's hub, as the satellite knows it.
 */
export type HubStatus = {
  /**
   * The hub's server ID.
   */
  serverId: string,
  /**
   * The hub's name. Text from another machine: display only.
   */
  name: string,
  /**
   * The library the hub holds.
   */
  libraryId: string,
  /**
   * The hub epoch the satellite last saw.
   */
  epoch: number,
  /**
   * Where the hub is reached, in the order they are tried.
   */
  routes: Array<string>,
  /**
   * When the satellite joined, as Unix milliseconds.
   */
  linkedAtMs: number,
  /**
   * Whether the hub answered the last time the satellite tried it. Left
   * out until it has tried, which it does when it is first asked for
   * something only the hub has.
   */
  answers?: boolean,
  /**
   * Since when it has, or has not, as Unix milliseconds.
   */
  sinceMs?: number,
  /**
   * How the satellite last reached it: the route as the link names it,
   * and the address it resolved to where that differs, such as
   * `studio-mac.local:4747 (192.168.1.20:4747)`.
   */
  route?: string,
  /**
   * Why the last try failed, while it does not answer.
   */
  problem?: string,
};

/**
 * The params of `artists.importPictures`.
 */
export type ImportPicturesParams = {
  /**
   * The command's ID, made by the client.
   */
  idempotencyKey: string,
  /**
   * The picture folder's name, as a root's: 1 to 100 characters, with
   * no `/`.
   */
  name: string,
  /**
   * The folder's absolute path on the server. The server only ever
   * reads it.
   */
  path: string,
};

/**
 * What stands in the way of applying a run, and what to do about it.
 */
export type ImportProblem = "no_zone";

/**
 * What a dry run found: what the run holds, and what applying it would
 * do, by year. Nothing was written to find it.
 */
export type ImportReport = {
  /**
   * When it was worked out.
   */
  madeAt: number,
  /**
   * The grouper's version, which the stream matcher is part of. An
   * apply is refused when it has changed since the dry run.
   */
  grouperVersion: number,
  /**
   * The play rule's version. An apply is refused when it has changed
   * since the dry run.
   */
  playRuleVersion: number,
  /**
   * Streams in the run, repeats included.
   */
  streams: number,
  /**
   * Listens they report: each repeat counted once.
   */
  events: number,
  /**
   * Streams stored already whose record this archive spells otherwise.
   * The stored record is kept.
   */
  respelled: number,
  /**
   * Scrobbles offered to the streams: Last.fm's, less those set aside
   * and those that are this library's own listens come back.
   */
  scrobbles: number,
  /**
   * Scrobbles set aside, which no stream may take.
   */
  scrobblesExcluded: number,
  /**
   * Scrobbles that are this library's own listens come back.
   */
  scrobblesEchoes: number,
  /**
   * What each tier joins.
   */
  tiers: Array<TierJoins>,
  /**
   * By year.
   */
  years: Array<ImportYear>,
  /**
   * Streams played offline, and of those, the ones with the time
   * offline mode was used.
   */
  offline: number,
  /**
   * See `offline`.
   */
  offlineTimestamped: number,
  /**
   * Of those, the ones whose offline time reads as seconds near the
   * stream's end, and as milliseconds near it; the rest are neither.
   */
  offlineSeconds: number,
  /**
   * See `offline_seconds`.
   */
  offlineMilliseconds: number,
  /**
   * Scrobbles no stream took, stamped between the stream before and the
   * worked-out start of a stream of the same names that has no
   * scrobble: where a pause would put them.
   */
  paused: number,
  /**
   * What stands in the way of applying.
   */
  problems: Array<ImportProblem>,
  /**
   * A sample of the joins of tiers B and C, for judging them.
   */
  samples: Array<JoinSample>,
};

/**
 * A run of streams staged on the hub. It is removed a day after it was
 * begun, and when the hub starts.
 */
export type ImportRun = {
  /**
   * The run.
   */
  runId: string,
  /**
   * How many pieces it has.
   */
  pieces: number,
  /**
   * How many of them the hub holds.
   */
  received: number,
  /**
   * The streams in those pieces.
   */
  streams: number,
  /**
   * When the hub removes it.
   */
  expiresAt: number,
  /**
   * The last report, once a dry run or an apply has finished: an
   * apply works its report out again in the write that applies it, and
   * that one is what applying did.
   */
  report?: ImportReport,
  /**
   * The run as the library holds it, once it has been applied.
   */
  applied?: AppliedRun,
};

/**
 * The result of `imports.list`: every run the library holds, newest
 * first.
 */
export type ImportRuns = {
  /**
   * The runs.
   */
  runs: Array<AppliedRun>,
};

/**
 * When the account's history is imported. Only the library's hub imports:
 * when it starts, when a client such as the desktop app opens and the
 * last import ended more than an hour before, and six hours after the
 * last import ended. A sync by hand counts as an import.
 */
export type ImportSchedule = {
  /**
   * The last sync of the history to end, started by hand or on its own.
   */
  last?: LastImport,
  /**
   * Whether a sync is queued or running now.
   */
  running: boolean,
  /**
   * When the server next starts one on its own. Left out while one runs
   * and while none will start on its own.
   */
  next?: number,
  /**
   * From this time on, a client opening starts one. Left out as `next`
   * is.
   */
  onOpenAfter?: number,
  /**
   * Why none starts on its own, when none will: this server is not the
   * hub, has no API key, is not signed in, was refused its session, or
   * the listener has no zones.
   */
  off?: string,
};

/**
 * What a dry run found for one year, by each stream's worked-out start in
 * UTC. The joins and the plays after are by the tiers in force.
 */
export type ImportYear = {
  /**
   * The year.
   */
  year: number,
  /**
   * Listens in force once the run is applied, the run's and those in
   * force now: streams, each repeat counted once.
   */
  streams: number,
  /**
   * The run's own listens of the year that are stored already, whoever's
   * they stay: one of another listener's is not among `streams`.
   */
  alreadyStored: number,
  /**
   * Of those, the ones joined to a scrobble by tier A.
   */
  joinedStart: number,
  /**
   * By tier B.
   */
  joinedDuring: number,
  /**
   * By tier C.
   */
  joinedRenamed: number,
  /**
   * By a correction that joins them.
   */
  joinedPinned: number,
  /**
   * With no scrobble, and counted by the play rule.
   */
  aloneCounted: number,
  /**
   * With no scrobble, and not counted: skipped or partial.
   */
  aloneNotCounted: number,
  /**
   * With no scrobble, and no length for its track to judge it by.
   */
  aloneUnknown: number,
  /**
   * Scrobbles of the year that no stream took, of those offered.
   */
  scrobblesAlone: number,
  /**
   * Plays of the year now, by their start in UTC.
   */
  playsBefore: number,
  /**
   * Plays of the year once applied.
   */
  playsAfter: number,
  /**
   * Of the plays now, the ones that count.
   */
  countedBefore: number,
  /**
   * Of the plays once applied, the ones that count.
   */
  countedAfter: number,
  /**
   * Streams that start where no zone the listener was in covers, so
   * that their local date is unknown.
   */
  noZone: number,
};

/**
 * The params of `imports.get`.
 */
export type ImportsGetParams = {
  /**
   * The run.
   */
  runId: string,
};

/**
 * The result of `imports.dryRun`, `imports.apply` and `imports.undo`.
 */
export type ImportsJobStarted = {
  /**
   * The job that does it. A dry run's report, and an apply's, is the
   * run's, from `imports.get`, once the job has succeeded.
   */
  job: Job,
  /**
   * The command's ID: the idempotency key it was sent with.
   */
  commandId: string,
  /**
   * How far the command has got.
   */
  status: ReceiptStatus,
  /**
   * The library revision the command was committed at. It is left out
   * for a command that changes no library.
   */
  revision?: number,
};

/**
 * The params of `imports.dryRun`, `imports.apply` and `imports.undo`.
 */
export type ImportsRunParams = {
  /**
   * The command's ID, made by the client.
   */
  idempotencyKey: string,
  /**
   * The run.
   */
  runId: string,
};

/**
 * The params of `imports.stage`: one piece of a run of streams read from
 * an archive, which the hub keeps until the run is dry run.
 */
export type ImportsStageParams = {
  /**
   * The command's ID, made by the client.
   */
  idempotencyKey: string,
  /**
   * The run, made by the client for its first piece and named by every
   * piece after.
   */
  runId: string,
  /**
   * Which piece this is, from 0.
   */
  piece: number,
  /**
   * How many pieces the run has.
   */
  pieces: number,
  /**
   * What read the archive, by name and version, such as
   * `ritornelctl 0.1.0`, at most [`MAX_CLIENT_TEXT`](crate::MAX_CLIENT_TEXT)
   * characters. The same in every piece.
   */
  reader: string,
  /**
   * Its streams, at most [`MAX_STREAMS_PER_PIECE`].
   */
  streams: Array<SpotifyStream>,
};

/**
 * The result of `imports.stage`.
 */
export type ImportsStaged = {
  /**
   * The run as the hub now holds it.
   */
  run: ImportRun,
  /**
   * The command's ID: the idempotency key it was sent with.
   */
  commandId: string,
  /**
   * How far the command has got.
   */
  status: ReceiptStatus,
  /**
   * The library revision the command was committed at. It is left out
   * for a command that changes no library.
   */
  revision?: number,
};

/**
 * One item that the dry run does not find unchanged.
 */
export type ItemChange = {
  /**
   * What the run says of it.
   */
  outcome: ItemOutcome,
  /**
   * The file.
   */
  file: FileFacts,
  /**
   * What it is bound to now.
   */
  now?: Binding,
  /**
   * What the run would bind it to.
   */
  after?: Binding,
};

/**
 * What the dry run says of one item.
 */
export type ItemOutcome = "unchanged" | "would_change" | "pinned" | "blocked";

/**
 * One job: a durable resource, readable after clients disconnect and
 * after it ends.
 */
export type Job = {
  /**
   * The job's ID.
   */
  jobId: string,
  /**
   * What kind of work it is, such as `scan`.
   */
  kind: string,
  /**
   * What it works on.
   */
  target?: JobTarget,
  /**
   * Where it is in its life.
   */
  state: JobState,
  /**
   * Items done. For a scan, files read.
   */
  done: number,
  /**
   * Items in all. It is left out while that is not known.
   */
  total?: number,
  /**
   * Bytes read.
   */
  bytes: number,
  /**
   * Items that failed. For a scan, files that could not be read.
   */
  failed: number,
  /**
   * Items passed over. For a scan, files that had not changed.
   */
  skipped: number,
  /**
   * What it is doing now.
   */
  label?: string,
  /**
   * Why it failed.
   */
  error?: string,
  /**
   * When it was created, as Unix milliseconds.
   */
  createdAt: number,
  /**
   * When it first started.
   */
  startedAt?: number,
  /**
   * When it ended.
   */
  finishedAt?: number,
  /**
   * What a scan has found so far, and at its end what came out.
   */
  scan?: ScanSummary,
  /**
   * What a sync of an account's history has read so far, and at its end
   * what came out.
   */
  sync?: SyncSummary,
};

/**
 * The params of `jobs.cancel`.
 */
export type JobCancelParams = {
  /**
   * The command's ID, made by the client.
   */
  idempotencyKey: string,
  /**
   * The job to stop.
   */
  jobId: string,
};

/**
 * The result of `jobs.cancel`: the receipt, and the job as it then is.
 * Cancel is a request: a running job stops after the step in flight.
 */
export type JobCancelled = {
  /**
   * The job.
   */
  job: Job,
  /**
   * The command's ID: the idempotency key it was sent with.
   */
  commandId: string,
  /**
   * How far the command has got.
   */
  status: ReceiptStatus,
  /**
   * The library revision the command was committed at. It is left out
   * for a command that changes no library.
   */
  revision?: number,
};

/**
 * The result of `jobs.get`.
 */
export type JobDetail = {
  /**
   * The job.
   */
  job: Job,
  /**
   * The first of its items that failed.
   */
  failedItems: Array<JobItem>,
};

/**
 * The params of `jobs.get`.
 */
export type JobGetParams = {
  /**
   * The job.
   */
  jobId: string,
};

/**
 * One item of a job that failed.
 */
export type JobItem = {
  /**
   * What the item was. For a scan, a path relative to the root.
   */
  item: string,
  /**
   * Why it failed.
   */
  detail: string,
};

/**
 * Where a job is in its life.
 */
export type JobState = "queued" | "running" | "cancelling" | "succeeded" | "failed" | "cancelled";

/**
 * What a job works on, and so what its events invalidate.
 */
export type JobTarget = { "type": "source",
  /**
   * The source's ID.
   */
  id: string, } | { "type": "account",
  /**
   * The account's ID.
   */
  id: string, } | { "type": "import_run",
  /**
   * The run's ID.
   */
  id: string,
};

/**
 * What an event on the `jobs` topic carries.
 */
export type JobsEvent = {
  /**
   * The job, whole, with its target. The client replaces its copy.
   */
  job: Job,
};

/**
 * The result of `jobs.list`.
 */
export type JobsList = {
  /**
   * The newest jobs, newest first.
   */
  jobs: Array<Job>,
};

/**
 * One join a dry run shows for judging a tier: both records' names and how
 * far apart they were.
 */
export type JoinSample = {
  /**
   * The tier that joined them.
   */
  tier: StreamTier,
  /**
   * Whether the scrobble is a second report.
   */
  second: boolean,
  /**
   * The stream's artist and title.
   */
  streamArtist: string,
  /**
   * See `stream_artist`.
   */
  streamTitle: string,
  /**
   * The scrobble's artist and title.
   */
  scrobbleArtist: string,
  /**
   * See `scrobble_artist`.
   */
  scrobbleTitle: string,
  /**
   * The scrobble's stamp less the stream's worked-out start.
   */
  fromStartMs: number,
  /**
   * How long the stream played.
   */
  msPlayed: number,
};

export type JsonValue = number | string | boolean | Array<JsonValue> | { [key in string]: JsonValue } | null;

/**
 * The params of a command that takes nothing but its key.
 */
export type KeyParams = {
  /**
   * The command's ID, made by the client.
   */
  idempotencyKey: string,
};

/**
 * How the last sync of a history ended.
 */
export type LastImport = {
  /**
   * When it ended.
   */
  ended: number,
  /**
   * `succeeded`, `failed` or `cancelled`.
   */
  state: JobState,
  /**
   * Why it failed.
   */
  failure?: string,
};

/**
 * The result of `lastfm.report`: how much of the imported history matched
 * the library.
 */
export type LastfmReport = {
  /**
   * The account.
   */
  account: string,
  /**
   * Scrobbles imported.
   */
  imported: number,
  /**
   * Plays they make, the echoes of plays scrobbled from here and the
   * scrobbles joined to a Spotify stream, whose plays the stream speaks
   * for, not counted.
   */
  plays: number,
  /**
   * Of those, the ones bound to a recording.
   */
  matchedPlays: number,
  /**
   * Scrobbles that are echoes of plays scrobbled from here.
   */
  echoes: number,
  /**
   * Scrobbles joined to a Spotify stream: reports of the same listen.
   */
  joinedToStreams: number,
  /**
   * Distinct tracks the history names.
   */
  tracks: number,
  /**
   * Of those, the ones bound.
   */
  matchedTracks: number,
  /**
   * Tracks the matcher was not sure of.
   */
  doubtfulTracks: number,
  /**
   * Their plays.
   */
  doubtfulPlays: number,
  /**
   * Tracks nothing in the library comes close to.
   */
  absentTracks: number,
  /**
   * Their plays.
   */
  absentPlays: number,
  /**
   * The doubtful tracks, most played first.
   */
  doubtful: Array<DoubtfulTrack>,
  /**
   * Tracks loved now.
   */
  loved: number,
  /**
   * Of those, the ones bound.
   */
  lovedMatched: number,
  /**
   * The first and the last play imported.
   */
  first?: number,
  /**
   * See `first`.
   */
  last?: number,
};

/**
 * The params of `lastfm.report`.
 */
export type LastfmReportParams = {
  /**
   * How many doubtful tracks to list, at most 1000. The default is 50.
   */
  limit?: number,
};

/**
 * The result of `lastfm.status`.
 */
export type LastfmStatus = {
  /**
   * Whether this server was built with Last.fm.
   */
  available: boolean,
  /**
   * Whether it has an API key and secret, from its environment or its
   * data directory. The values are never sent.
   */
  configured: boolean,
  /**
   * The account it is signed in to.
   */
  account?: string,
  /**
   * Last.fm refused the session: sign in again.
   */
  signInAgain: boolean,
  /**
   * Plays from this time on are scrobbled. Left out while scrobbling is
   * off.
   */
  scrobblingSince?: number,
  /**
   * Since when nothing is scrobbled until an import from Last.fm has
   * run, because the library was restored from a backup and Last.fm may
   * already have plays it holds waiting. The hub imports by itself; the
   * plays waiting go once it has. Left out when nothing waits for that.
   */
  waitingForImportSince?: number,
  /**
   * Scrobbles recorded, by state: `pending`, `sending`, `accepted`,
   * `ignored`, `refused` or `unknown`.
   */
  deliveries: Array<DeliveryCount>,
  /**
   * Plays imported from the account.
   */
  imported: number,
  /**
   * Whether the listener has a zone for any date, without which an
   * imported play has no local date. A sync is refused until they have.
   */
  zonesSet: boolean,
  /**
   * When the history was last imported, and when it next is.
   */
  import: ImportSchedule,
};

/**
 * The params of `lastfm.sync`.
 */
export type LastfmSyncParams = {
  /**
   * The command's ID, made by the client.
   */
  idempotencyKey: string,
  /**
   * Read the whole history again rather than what is new. What is
   * stored already writes nothing.
   */
  full: boolean,
};

/**
 * The result of `lastfm.sync`.
 */
export type LastfmSyncStarted = {
  /**
   * The job that syncs. A sync that is running already is returned.
   */
  job: Job,
  /**
   * The command's ID: the idempotency key it was sent with.
   */
  commandId: string,
  /**
   * How far the command has got.
   */
  status: ReceiptStatus,
  /**
   * The library revision the command was committed at. It is left out
   * for a command that changes no library.
   */
  revision?: number,
};

/**
 * How a disc's layout was found.
 */
export type LayoutMethod = "log" | "cue" | "renamed_cue" | "accuraterip" | "cddb";

/**
 * What kind of change a library write made, for a client deciding what to
 * read again.
 */
export type LibraryChange = "membership" | "metadata" | "stats" | "ratings";

/**
 * The params of `library.create`.
 */
export type LibraryCreateParams = {
  /**
   * The command's ID, made by the client.
   */
  idempotencyKey: string,
};

/**
 * The result of `library.create`: the receipt, and the library that now
 * exists.
 */
export type LibraryCreated = {
  /**
   * The new library's ID.
   */
  libraryId: string,
  /**
   * The command's ID: the idempotency key it was sent with.
   */
  commandId: string,
  /**
   * How far the command has got.
   */
  status: ReceiptStatus,
  /**
   * The library revision the command was committed at. It is left out
   * for a command that changes no library.
   */
  revision?: number,
};

/**
 * What an event on the `library` topic carries. Events that were not yet
 * sent are merged: the highest revision, and every kind of change either
 * had.
 */
export type LibraryEvent = {
  /**
   * The library revision after the change. A client that has read
   * something at this revision or a later one may keep it.
   */
  libraryRevision: number,
  /**
   * What kind of change it was, in order and each once.
   */
  changed: Array<LibraryChange>,
};

/**
 * The result of `library.settings`: the library's settings, each with
 * what gave it. They are kept with the library, on its hub, and a
 * satellite reads its hub's.
 */
export type LibrarySettings = {
  /**
   * Whether MusicBrainz may be asked about the library's discs: one album
   * or folder at a time, when `corrections.lookup` asks. Nothing is asked
   * by itself. Off by default, until a call turns it on.
   */
  lookups: Setting<boolean>,
};

/**
 * The result of `library.setSettings`: the receipt, and the settings as
 * this change left them, at the receipt's revision. Sent again, it is
 * answered as it was; `library.settings` says what they are now.
 */
export type LibrarySettingsSet = {
  /**
   * The settings as this change left them.
   */
  settings: LibrarySettings,
  /**
   * The command's ID: the idempotency key it was sent with.
   */
  commandId: string,
  /**
   * How far the command has got.
   */
  status: ReceiptStatus,
  /**
   * The library revision the command was committed at. It is left out
   * for a command that changes no library.
   */
  revision?: number,
};

/**
 * The result of `library.stats`: what the library holds, and what the
 * scans made of it.
 */
export type LibraryStats = {
  /**
   * The library revision the counts were read at.
   */
  libraryRevision: number,
  /**
   * Files the library has, which are not retired.
   */
  files: number,
  /**
   * Their size in bytes.
   */
  fileBytes: number,
  /**
   * Of them, files the last scan of their root did not find.
   */
  filesMissing: number,
  /**
   * Of them, files that could not be read as audio.
   */
  filesUnreadable: number,
  /**
   * Why, most common first.
   */
  unreadableReasons: Array<Count>,
  /**
   * Files whose old path a different song took.
   */
  filesRetired: number,
  /**
   * Readable files by codec, most common first.
   */
  codecs: Array<Count>,
  /**
   * Files that carry any MusicBrainz ID in their tags.
   */
  filesWithMusicbrainzId: number,
  /**
   * Readable files with no binding yet.
   */
  unresolved: number,
  /**
   * Artists with something credited to them.
   */
  artists: number,
  /**
   * Release groups with a release in them.
   */
  releaseGroups: number,
  /**
   * Releases with a track on them.
   */
  releases: number,
  /**
   * Recordings with a file bound to them.
   */
  recordings: number,
  /**
   * Track appearances with a file bound to them.
   */
  appearances: number,
  /**
   * Of them, appearances on no release.
   */
  looseTracks: number,
  /**
   * Current bindings by how they were decided.
   */
  decisions: Array<Count>,
  /**
   * Current bindings marked for review.
   */
  needsReview: number,
  /**
   * Release groups whose kind is known.
   */
  groupsWithKind: number,
  /**
   * Those by kind.
   */
  kinds: Array<Count>,
  /**
   * Release groups that have a first year.
   */
  groupsWithFirstYear: number,
  /**
   * Of them, those whose year came from an original-date tag and not
   * from the earliest date among their releases.
   */
  groupsWithFirstYearFromTag: number,
  /**
   * Distinct pictures recorded.
   */
  pictures: number,
  /**
   * Releases that have a cover.
   */
  releasesWithCover: number,
};

/**
 * The library a server holds, as a status reports it.
 */
export type LibraryStatus = {
  /**
   * The library's ID: made when the library was created and never
   * changed.
   */
  libraryId: string,
  /**
   * The library revision: it counts the writes to the library, and a
   * client that holds what it read at this revision may keep it. On a
   * satellite it is 0: the library is its hub's, and a status answers
   * without waiting on the hub.
   */
  revision: number,
};

/**
 * The limits a client must stay within.
 */
export type Limits = {
  /**
   * The largest frame the server reads, in bytes. It closes a socket
   * that sends a larger one.
   */
  maxMessageBytes: number,
  /**
   * How many views a session may hold. Opening one more ends the one
   * used least recently, and a call on it fails with `view_expired`.
   * 0 from a server that has no lists.
   */
  maxViews: number,
  /**
   * The most rows one `list.window` sends.
   */
  maxWindowRows: number,
  /**
   * How long a view lasts unused, in milliseconds, before the server
   * ends it.
   */
  viewIdleMs: number,
};

/**
 * The result of `link.copies`: the masters of a track the hub holds, best
 * first, each fetched from `GET /v1/audio/{appearanceId}/{sourceItemId}/
 * {revision}`.
 */
export type LinkCopies = {
  /**
   * The copies, best first.
   */
  copies: Array<HubCopy>,
};

/**
 * The params of `link.copies`.
 */
export type LinkCopiesParams = {
  /**
   * The track appearance.
   */
  appearance: string,
};

/**
 * What the hub answers a satellite that joined.
 */
export type LinkGranted = {
  /**
   * The link session's token, which the satellite presents from now on.
   */
  token: string,
  /**
   * The library the hub holds.
   */
  libraryId: string,
  /**
   * The hub epoch.
   */
  epoch: number,
};

/**
 * The result of `link.invite`.
 */
export type LinkInvite = {
  /**
   * The link to give the satellite: `link.join` takes it. It holds a
   * secret, and works once.
   */
  link: string,
  /**
   * When it stops working, as Unix milliseconds.
   */
  expiresAtMs: number,
};

/**
 * The params of `link.invite`.
 */
export type LinkInviteParams = {
  /**
   * Where the satellite may reach the hub, as `host:port`, in the order
   * to try: an address on a private network or a tailnet, or a name the
   * satellite resolves at each dial. Left empty, the hub offers its
   * multicast DNS name and the addresses it finds it has.
   */
  routes: Array<string>,
};

/**
 * The params of `link.join`.
 */
export type LinkJoinParams = {
  /**
   * The command's ID, made by the client.
   */
  idempotencyKey: string,
  /**
   * The link `link.invite` gave on the hub.
   */
  link: string,
};

/**
 * The params of `link.listen`: one listen a satellite's player recorded,
 * as its outbox sends it, with the stamps of this send.
 */
export type LinkListenParams = {
  /**
   * The listen's ID, which is also its source key at the hub: sent
   * again, it is taken in once.
   */
  idempotencyKey: string,
  /**
   * The listen record, as the playing server staged it.
   */
  record: JsonValue,
  /**
   * The satellite's wall clock when it sent this.
   */
  sentAtMs: number,
  /**
   * Its monotonic clock then.
   */
  sentMonoMs: number,
  /**
   * The boot that clock counts from.
   */
  sentBoot: string,
};

/**
 * The result of `link.listen`.
 */
export type LinkListened = {
  /**
   * What became of it.
   */
  taken: ListenTaken,
};

/**
 * What a satellite tells its hub every few seconds with `link.ping`.
 */
export type LinkPingParams = {
  /**
   * Whether the satellite's player is playing.
   */
  playing: boolean,
};

/**
 * The result of `link.ping`.
 */
export type LinkPinged = {
  /**
   * Whether the hub keeps its machine from sleeping for its linked
   * servers now.
   */
  keptAwake: boolean,
};

/**
 * What a satellite sends to `POST /v1/link` to join: the invite's secret
 * and who it is.
 */
export type LinkRequest = {
  /**
   * The invite's secret, as hex.
   */
  secret: string,
  /**
   * The satellite's server ID.
   */
  serverId: string,
  /**
   * The satellite's name.
   */
  name: string,
  /**
   * The satellite's public key, as 64 hex digits.
   */
  publicKey: string,
  /**
   * The signature, as 128 hex digits, of [`link_request_proof`] for the
   * hub and this secret: proof that the satellite holds its key.
   */
  proof: string,
  /**
   * The [contract version](crate::CONTRACT_VERSION) the satellite
   * speaks. A hub refuses another.
   */
  contractVersion: number,
};

/**
 * The result of `link.status`, and of `link.join`.
 */
export type LinkStatus = {
  /**
   * This server's role.
   */
  role: ServerRole,
  /**
   * On a satellite, its hub. Left out on any other server.
   */
  hub?: HubStatus,
  /**
   * On a hub, the satellites linked to it, oldest first.
   */
  satellites: Array<SatelliteStatus>,
  /**
   * When this server keeps its machine from sleeping for its linked
   * servers.
   */
  awake: Awake,
  /**
   * Whether it does now.
   */
  keptAwake: boolean,
};

/**
 * The result of `link.tracks`: what a satellite's player reads of the
 * library.
 */
export type LinkTracks = {
  /**
   * Each appearance asked for, in order: `null` for one the library
   * does not have.
   */
  tracks: Array<TrackSummary | null>,
  /**
   * The release's tracks, when one was asked for and the library has
   * it.
   */
  release?: Array<TrackSummary>,
  /**
   * The library's first listener: whom a new player plays for.
   */
  listener?: string,
};

/**
 * The params of `link.tracks`.
 */
export type LinkTracksParams = {
  /**
   * Track appearances, each answered in its place.
   */
  appearances: Array<string>,
  /**
   * A release, whose tracks are answered after them, in disc and track
   * order.
   */
  release?: string,
};

/**
 * The result of `list.close`.
 */
export type ListClosed = {
  /**
   * Whether a view by that ID was open. Closing one that is not changes
   * nothing.
   */
  closed: boolean,
};

/**
 * What an event on the `list` topic carries: one view this session holds
 * may be out of date.
 */
export type ListEvent = {
  /**
   * The view.
   */
  view: string,
  /**
   * The library revision after the change.
   */
  libraryRevision: number,
  /**
   * `false`: only what rows say changed, and the client reads the
   * windows on screen again in place. `true`: which rows the view should
   * hold, or their order, may have changed, and the client opens a view
   * that replaces it.
   */
  reorder: boolean,
};

/**
 * What a list holds.
 */
export type ListKind = "artists" | "albums" | "songs";

/**
 * The params of `list.locate`: a view, and a row's ID or the start of its
 * name. Exactly one of the two is given.
 */
export type ListLocateParams = {
  /**
   * The view.
   */
  view: string,
  /**
   * The row's ID: an `artistId`, an album's `ref` ID or an
   * `appearanceId`.
   */
  id?: string,
  /**
   * The start of what the view is ordered by, as typed, for
   * type-to-find. It finds the first row whose sort text begins with it,
   * or else the first row after where it would be.
   */
  prefix?: string,
};

/**
 * The result of `list.locate`.
 */
export type ListLocated = {
  /**
   * The row's index, or `null` when the view does not hold it.
   */
  index: number | null,
};

/**
 * The params of `list.open`.
 */
export type ListOpenParams = {
  /**
   * What the list holds.
   */
  kind: ListKind,
  /**
   * Which rows, and in what order.
   */
  query: Query,
  /**
   * The rows to send with the answer, so that a screen can draw at once.
   * Left out, none are sent.
   */
  window?: Window,
  /**
   * The view this one replaces.
   */
  replaces?: Replaces,
};

/**
 * The result of `list.open`.
 */
export type ListOpened = {
  /**
   * The view's ID, for the calls that read it.
   */
  view: string,
  /**
   * How many rows the view has. It never changes: a scrollbar can be
   * exact.
   */
  total: number,
  /**
   * The library revision the view was resolved at.
   */
  libraryRevision: number,
  /**
   * The index of the first row in `rows`.
   */
  offset: number,
  /**
   * The rows of the window asked for, in order. Empty when none was.
   */
  rows: Array<Row>,
  /**
   * Where the anchor row of the view this replaces is in this one. It is
   * left out when nothing was replaced, no anchor was named, or the
   * anchor is not in this view.
   */
  anchorIndex?: number,
};

/**
 * The result of `list.sections`.
 */
export type ListSections = {
  /**
   * The sections, in the view's order. Letters for an order by name or
   * title, years for an order by year, stretches of time for an order
   * by when. Empty for an order that has none.
   */
  sections: Array<Section>,
};

/**
 * What a list of each kind supports, as the descriptor says.
 */
export type ListSupport = {
  /**
   * The kind.
   */
  kind: ListKind,
  /**
   * The orders it can be read in, the first being the one it takes
   * when none is named.
   */
  sorts: Array<SortKey>,
  /**
   * The predicates a filter on it may use.
   */
  predicates: Array<PredicateName>,
};

/**
 * The result of `list.window`.
 */
export type ListWindow = {
  /**
   * The view.
   */
  view: string,
  /**
   * The index of the first row in `rows`.
   */
  offset: number,
  /**
   * The rows, in order. Fewer than asked for at the end of the view.
   */
  rows: Array<Row>,
};

/**
 * The params of `list.window`.
 */
export type ListWindowParams = {
  /**
   * The view.
   */
  view: string,
  /**
   * The index of the first row.
   */
  offset: number,
  /**
   * How many rows, at most the descriptor's `maxWindowRows`.
   */
  limit: number,
};

/**
 * Which network a server listens on.
 */
export type ListenNetwork = "loopback" | "private";

/**
 * Where a server is set to listen.
 */
export type ListenOn = {
  /**
   * The network.
   */
  network: ListenNetwork,
  /**
   * The port it is set to take. 0 asks the system for one. The port it
   * took is in `server.json`, and differs where the system picked it.
   */
  port: number,
};

/**
 * What became of a listen a satellite sent.
 */
export type ListenTaken = "stored" | "duplicate" | "conflict";

/**
 * The least severe level the server logs.
 */
export type LogLevel = "error" | "warn" | "info" | "debug" | "trace";

/**
 * The result of `corrections.lookup`.
 */
export type LookupDone = {
  /**
   * Each disc, in the order asked.
   */
  discs: Array<DiscLookup>,
  /**
   * The command's ID: the idempotency key it was sent with.
   */
  commandId: string,
  /**
   * How far the command has got.
   */
  status: ReceiptStatus,
  /**
   * The library revision the command was committed at. It is left out
   * for a command that changes no library.
   */
  revision?: number,
};

/**
 * What became of one disc a lookup was about.
 */
export type LookupOutcome = "replied" | "not_found" | "kept" | "failed";

/**
 * The params of `corrections.lookup`. It names one album: a release, or a
 * folder.
 */
export type LookupParams = {
  /**
   * The command's ID, made by the client.
   */
  idempotencyKey: string,
  /**
   * A release, by its ID: the discs of the folders its files are in.
   */
  album?: string,
  /**
   * A folder, by its path under its root: its discs.
   */
  folder?: string,
  /**
   * The root the folder is in, by its name. Needed only when more than
   * one root has a folder of that path.
   */
  root?: string,
  /**
   * Ask again about a disc that has a reply already.
   */
  again: boolean,
};

/**
 * The result of `media.token`: what a page puts in the query string of a
 * media URL.
 */
export type MediaToken = {
  /**
   * The token. It reads media and nothing else, and works until the
   * session that asked for it ends.
   */
  token: string,
};

/**
 * The candidate an item was nearest to.
 */
export type Nearest = {
  /**
   * The file whose claims it was compared with.
   */
  file: FileFacts,
  /**
   * The distance, from 0 to 1.
   */
  distance: number,
  /**
   * What made the distance up, when the resolver can say.
   */
  penalties?: Penalties,
  /**
   * The recording it nearly matched, after redirects, where one exists:
   * for a review, the one the decision names, or else the one that file
   * is bound to now.
   */
  recordingId?: string,
};

/**
 * A track a doubtful scrobble nearly matched.
 */
export type NearlyMatched = {
  /**
   * Its title.
   */
  title?: string,
  /**
   * Its artist credit.
   */
  artist?: string,
};

/**
 * The params of a method that takes none: an empty object, which may also
 * be left out of the frame.
 */
export type NoParams = Record<symbol, never>;

/**
 * The result of `history.onThisDay`: the plays that count on that day of
 * any year, newest first.
 */
export type OnThisDay = {
  /**
   * The day, as `MM-DD`.
   */
  day: string,
  /**
   * The plays.
   */
  plays: Array<Play>,
};

/**
 * The params of `history.onThisDay`.
 */
export type OnThisDayParams = {
  /**
   * The day, as `MM-DD`. The default is today, where the server is.
   */
  day?: string,
};

/**
 * The output the player renders through.
 */
export type OutputInfo = {
  /**
   * Its name: the device's, or `null` for the output that makes no
   * sound.
   */
  name: string,
  /**
   * How it runs.
   */
  mode: OutputMode,
  /**
   * Its rate.
   */
  sampleRateHz: number,
  /**
   * Its channels.
   */
  channels: number,
  /**
   * What is wrong with it, while something is: it went away, or cannot
   * be opened. The player waits, paused where it was, blaming no track,
   * and Play tries the output again.
   */
  problem?: OutputProblem,
};

/**
 * How an output runs. See "Audio pipeline" in docs/internals/playback.md.
 */
export type OutputMode = "shared";

/**
 * What is wrong with the output.
 */
export type OutputProblem = {
  /**
   * When it first failed, as Unix milliseconds.
   */
  since: number,
  /**
   * What it said the last time. Display text only.
   */
  detail: string,
};

/**
 * What made a tags-and-duration distance, each from 0 to 1.
 */
export type Penalties = {
  /**
   * The titles differ in a bracketed note.
   */
  title: number,
  /**
   * How far apart the artists are.
   */
  artist: number,
  /**
   * How far apart the lengths are, beyond the free band.
   */
  duration: number,
  /**
   * Whether the albums differ: 1, or 0.5 when one has none.
   */
  album: number,
  /**
   * Whether a shared ISRC or AcoustID halved the distance.
   */
  sharedIdentifier: boolean,
};

/**
 * A run of words in a sentence, set apart where it is `strong`: the facts
 * a sentence turns on, a month or a count, are.
 */
export type Phrase = {
  /**
   * The words.
   */
  text: string,
  /**
   * Whether they are set apart.
   */
  strong: boolean,
};

/**
 * One picture folder the library takes artists' portraits from.
 */
export type PictureFolder = {
  /**
   * Its name, as a root's.
   */
  name: string,
  /**
   * Where it is on the server that answered. It is left out when that
   * server has no path for it, as for a source's root.
   */
  path?: string,
  /**
   * The portraits the library holds from it now.
   */
  portraits: number,
};

/**
 * The result of `artists.pictureFolders`: the picture folders, and the
 * last import of one.
 */
export type PictureFolders = {
  /**
   * Every picture folder, in the order they were first imported.
   */
  folders: Array<PictureFolder>,
  /**
   * When the last import that wrote something was committed, asked for
   * or started on its own, as Unix milliseconds. Left out before any.
   */
  lastImportAt?: number,
  /**
   * What that import answered, while the library keeps its receipt. One
   * started on its own has none.
   */
  lastImport?: PicturesImported,
};

/**
 * The result of `artists.importPictures`: the receipt, and what the
 * import found among the artists with a record.
 */
export type PicturesImported = {
  /**
   * The artists with a record.
   */
  artists: number,
  /**
   * Those whose portrait was found by their MusicBrainz ID.
   */
  byMbid: number,
  /**
   * Those with no ID whose portrait was found by their name.
   */
  byName: number,
  /**
   * Those the folder has no portrait for.
   */
  without: number,
  /**
   * Portraits the folder chose that could not be read.
   */
  unreadable: number,
  /**
   * Portraits from this folder the library had, and the folder no
   * longer chooses.
   */
  dropped: number,
  /**
   * Artists with no ID whose names the folder makes the same as
   * another's, whose portrait's manifest names neither. They are
   * counted in `without` too.
   */
  ambiguous: number,
  /**
   * The command's ID: the idempotency key it was sent with.
   */
  commandId: string,
  /**
   * How far the command has got.
   */
  status: ReceiptStatus,
  /**
   * The library revision the command was committed at. It is left out
   * for a command that changes no library.
   */
  revision?: number,
};

/**
 * The operating system of the device that made the sound, and only
 * that: a web player or a cast is how it played, never a system.
 */
export type Platform = "ios" | "android" | "mac" | "windows" | "linux" | "other";

/**
 * One play.
 */
export type Play = {
  /**
   * The play. Its ID is computed, and a client that meets one it does
   * not know reads again.
   */
  playId: string,
  /**
   * When it began. Left out for a play with no date.
   */
  at?: number,
  /**
   * Its local date, as `YYYY-MM-DD`, in `zone`.
   */
  localDate?: string,
  /**
   * Its local hour.
   */
  localHour?: number,
  /**
   * The zone its local date is in.
   */
  zone?: string,
  /**
   * That zone's offset from UTC then, in minutes.
   */
  utcOffsetMinutes?: number,
  /**
   * Which clock its time rests on: `hub_age` or `server_wall` for a
   * listen, `source_utc` for a play another service reported.
   */
  clockBasis: string,
  /**
   * What it was of.
   */
  track: PlayedTrack,
  /**
   * Audio rendered, over every listen of it. Left out for a play
   * another service reported, whose played time is not known.
   */
  msPlayed?: number,
  /**
   * The track's length.
   */
  durationMs?: number,
  /**
   * What became of it.
   */
  outcome: PlayOutcome,
  /**
   * Whether it counts, by the play rule.
   */
  counts: boolean,
  /**
   * Whether it is excluded.
   */
  excluded: boolean,
  /**
   * The listens behind it, the first first.
   */
  listens: Array<PlayedListen>,
};

/**
 * Why tracks were queued, which every listen of them records.
 */
export type PlayContext = {
  /**
   * The kind.
   */
  kind: ContextKind,
  /**
   * What it refers to: the release, the artist, the playlist, the stored
   * definition or the agent's session.
   */
  ref?: string,
};

/**
 * Where a page of `history.recent` ended: its last play's instant and
 * ID. The next page is asked for with it, and begins with the play after
 * that one, even one of the same instant.
 */
export type PlayCursor = {
  /**
   * The play's instant. Left out for a play with no date.
   */
  at?: number,
  /**
   * The play's ID.
   */
  playId: string,
};

/**
 * What became of a play.
 */
export type PlayOutcome = "listened" | "skipped" | "partial" | "unknown";

/**
 * What `playback.play` plays.
 */
export type PlaySource = { "type": "tracks",
  /**
   * The appearances.
   */
  ids: Array<string>, } | { "type": "release",
  /**
   * The release.
   */
  id: string, } | { "type": "view",
  /**
   * The view.
   */
  view: string,
};

/**
 * What a playback command returns: the receipt, and where the command put
 * the player. The whole state is read with `playback.get` or sent on the
 * `playback` topic.
 */
export type PlaybackChanged = {
  /**
   * The queue's revision after the command.
   */
  queueRevision: number,
  /**
   * The entry the command put the player on. One that cannot be played
   * is moved past at once, so the player may already be on a later
   * entry: `playback.get` and the `playback` topic say where it is.
   */
  entryId?: string,
  /**
   * The command's ID: the idempotency key it was sent with.
   */
  commandId: string,
  /**
   * How far the command has got.
   */
  status: ReceiptStatus,
  /**
   * The library revision the command was committed at. It is left out
   * for a command that changes no library.
   */
  revision?: number,
};

/**
 * The params of `playback.pause`, `playback.resume` and `playback.stop`.
 */
export type PlaybackCommandParams = {
  /**
   * The command's ID, made by the client.
   */
  idempotencyKey: string,
};

/**
 * What an event on the `playback` topic carries: the state, whole.
 */
export type PlaybackEvent = {
  /**
   * The state.
   */
  state: PlaybackState,
};

/**
 * The params of `playback.jump`.
 */
export type PlaybackJumpParams = {
  /**
   * The command's ID, made by the client.
   */
  idempotencyKey: string,
  /**
   * The entry to play now, from its beginning.
   */
  entryId: string,
};

/**
 * The params of `playback.play`.
 */
export type PlaybackPlayParams = {
  /**
   * The command's ID, made by the client.
   */
  idempotencyKey: string,
  /**
   * What to play. It replaces the queue.
   */
  source: PlaySource,
  /**
   * The track to start at, from 0. The default is the first, or with
   * shuffle on, any.
   */
  start?: number,
  /**
   * Why it is played. With none, a release is played as an album and
   * tracks by hand.
   */
  context?: PlayContext,
  /**
   * Shuffle on or off from now: on plays the start first and the rest
   * in a new random order. Left out, the player's setting stays.
   */
  shuffle?: boolean,
};

/**
 * The params of `playback.seek`.
 */
export type PlaybackSeekParams = {
  /**
   * The command's ID, made by the client.
   */
  idempotencyKey: string,
  /**
   * The entry to seek in. If the player has left it, nothing changes.
   */
  entryId: string,
  /**
   * Where to.
   */
  positionMs: number,
};

/**
 * The params of `playback.set`: the player's settings, as absolute values.
 * One left out stays as it is.
 */
export type PlaybackSetParams = {
  /**
   * The command's ID, made by the client.
   */
  idempotencyKey: string,
  /**
   * The player's own gain, 0 to 1. It is not the system's volume.
   */
  volume?: number,
  /**
   * Whether it is muted. The volume is kept while it is.
   */
  muted?: boolean,
  /**
   * Shuffle. On writes a new random order with the current entry first;
   * off goes back to queue order at the current entry.
   */
  shuffle?: boolean,
  /**
   * Repeat.
   */
  repeat?: Repeat,
};

/**
 * The params of `playback.skip`.
 */
export type PlaybackSkipParams = {
  /**
   * The command's ID, made by the client.
   */
  idempotencyKey: string,
  /**
   * The entry to skip from. If the player has left it, nothing changes,
   * so a skip sent again cannot skip twice.
   */
  fromEntryId: string,
  /**
   * Which way.
   */
  direction: SkipDirection,
};

/**
 * The player's state, whole: the result of `playback.get` and what a
 * `playback` event carries.
 */
export type PlaybackState = {
  /**
   * Counts the changes to this state. A client keeps the copy with the
   * highest.
   */
  revision: number,
  /**
   * What the player is doing.
   */
  status: PlaybackStatus,
  /**
   * The entry it is on.
   */
  entryId?: string,
  /**
   * That entry's place in the queue, from 0.
   */
  index?: number,
  /**
   * What that entry's track shows.
   */
  track?: TrackSummary,
  /**
   * The copy open for it, while one is.
   */
  copy?: CopyInUse,
  /**
   * Where in the track.
   */
  positionMs: number,
  /**
   * How long the track is, when that is known.
   */
  durationMs?: number,
  /**
   * The player's own gain, 0 to 1.
   */
  volume: number,
  /**
   * Whether it is muted.
   */
  muted: boolean,
  /**
   * Whether the queue plays in its stored shuffled order.
   */
  shuffle: boolean,
  /**
   * Repeat.
   */
  repeat: Repeat,
  /**
   * The output.
   */
  output: OutputInfo,
  /**
   * The queue's revision.
   */
  queueRevision: number,
  /**
   * How many entries the queue has.
   */
  queueLength: number,
};

/**
 * What the player is doing.
 */
export type PlaybackStatus = "stopped" | "playing" | "paused" | "buffering";

/**
 * One listen behind a play.
 */
export type PlayedListen = {
  /**
   * The listen.
   */
  listenId: string,
  /**
   * When it began, by the playing server's clock.
   */
  startedAt: number,
  /**
   * When it ended, by the same clock.
   */
  endedAt: number,
  /**
   * Why it began: `user_pick`, `auto_advance`, `repeat` or `resume`.
   */
  startReason: string,
  /**
   * Why it ended: `finished`, `next`, `previous`, `jump`, `replaced`,
   * `removed`, `stopped`, `error`, `output_lost`, `shutdown` or `crash`.
   */
  endReason: string,
  /**
   * Where in the track it began.
   */
  startPositionMs: number,
  /**
   * Where it ended.
   */
  endPositionMs: number,
  /**
   * Audio actually rendered.
   */
  msPlayed: number,
  /**
   * The listen it continues after a restart.
   */
  continues?: string,
  /**
   * The zone the playing server was in at its start.
   */
  zone?: string,
};

/**
 * What a play was of: its track as the library shows it. For a play whose
 * file is bound to no track yet, the fields are those of the track that
 * was queued, and the IDs are left out.
 */
export type PlayedTrack = {
  /**
   * The appearance the play is bound to. Left out while the file that
   * was heard is bound to no track.
   */
  appearanceId?: string,
  /**
   * The recording the play is bound to.
   */
  recordingId?: string,
  /**
   * The title.
   */
  title?: string,
  /**
   * The artist credit.
   */
  artist?: string,
  /**
   * The album.
   */
  album?: string,
};

/**
 * How the device that made the sound played it.
 */
export type PlayedVia = "app" | "web" | "cast" | "connect" | "airplay" | "other";

/**
 * The policy one dry run decides by: a named policy, and values to put in
 * place of its own. A value left out is the named policy's.
 */
export type PolicyOverrides = {
  /**
   * The named policy, 1, 2 or 3. Left out, it is the one scans resolve by.
   */
  version?: number,
  /**
   * A tags-and-duration match binds at this distance or less: 0 to 1.
   */
  bindMax?: number,
  /**
   * And only if the runner-up is at least this much further: 0 to 1.
   */
  minGap?: number,
  /**
   * A new recording is marked for review if a candidate scored under
   * this: 0 to 1.
   */
  reviewBelow?: number,
  /**
   * A difference in duration costs nothing up to this, in milliseconds:
   * at most a minute.
   */
  durationGraceMs?: number,
  /**
   * Whether candidate text has its accents taken off before it is
   * compared.
   */
  foldAccents?: boolean,
  /**
   * Whether sibling folders of one album whose track numbers collide,
   * which share no title and no year, and whose names each give a disc,
   * are discs of one release.
   */
  discsFromFolders?: boolean,
  /**
   * Whether a trailing "Disc 2" or "CD 2" in an album title is the
   * disc number.
   */
  discFromAlbumTitle?: boolean,
  /**
   * Whether a match on tags and length alone may bind a file to a
   * recording on another album.
   */
  tagsBindAcrossAlbums?: boolean,
  /**
   * When they may not, the largest difference in length, in
   * milliseconds, at which they still may: at most a minute, and 0 for
   * none.
   */
  acrossAlbumsWithinMs?: number,
  /**
   * Whether two different tracks of one release are never one recording
   * by tags.
   */
  tracksOfOneReleaseDiffer?: boolean,
  /**
   * Which list of version markers: 1 or 2.
   */
  markerList?: number,
};

/**
 * The policy a dry run decided by: a named policy with any values put in
 * its place.
 */
export type PolicyValues = {
  /**
   * The named policy the values are, or are a variant of: the resolver
   * version its decisions record.
   */
  resolverVersion: number,
  /**
   * Whether every value is that policy's own.
   */
  exact: boolean,
  /**
   * The bind distance.
   */
  bindMax: number,
  /**
   * The gap to the runner-up.
   */
  minGap: number,
  /**
   * The review threshold.
   */
  reviewBelow: number,
  /**
   * The free duration band, in milliseconds.
   */
  durationGraceMs: number,
  /**
   * Whether candidate text is accent-folded.
   */
  foldAccents: boolean,
  /**
   * Whether sibling folders can be discs of one release.
   */
  discsFromFolders: boolean,
  /**
   * Whether a disc marker in an album title is the disc number.
   */
  discFromAlbumTitle: boolean,
  /**
   * Whether a match on tags alone may bind across albums.
   */
  tagsBindAcrossAlbums: boolean,
  /**
   * When it may not, the difference in length within which it still
   * may, in milliseconds. 0 is none.
   */
  acrossAlbumsWithinMs: number,
  /**
   * Whether two different tracks of one release are never one
   * recording by tags.
   */
  tracksOfOneReleaseDiffer: boolean,
  /**
   * The list of version markers: 1, or 2 with every spelling of "a
   * cappella" and part numbers.
   */
  markerList: number,
};

/**
 * A condition on a row.
 */
export type Predicate = { "is": "by_artist",
  /**
   * The artist.
   */
  artistId: string, } | { "is": "with_artist",
  /**
   * The artist.
   */
  artistId: string, } | { "is": "no_record" } | { "is": "all",
  /**
   * The conditions.
   */
  of: Array<Predicate>,
};

/**
 * The name of a predicate, as the descriptor lists what a list supports.
 */
export type PredicateName = "by_artist" | "with_artist" | "no_record" | "all";

/**
 * The params of `corrections.preview`.
 */
export type PreviewParams = {
  /**
   * The proposals, each once with its candidate, previewed together as
   * one. Choices whose values disagree for one field of one file are
   * refused.
   */
  choices: Array<Choice>,
  /**
   * Or answers, to preview their undo: what it would give back, and
   * which answers resting on them it would undo too. Not with
   * `choices`.
   */
  undo: Array<string>,
};

/**
 * The result of `corrections.preview`: the albums the files are on now,
 * with everything else on them, and the albums they would be on. Nothing
 * is written.
 */
export type PreviewShown = {
  /**
   * The library revision it read.
   */
  libraryRevision: number,
  /**
   * The albums now.
   */
  now: Array<AlbumShape>,
  /**
   * The albums after.
   */
  after: Array<AlbumShape>,
  /**
   * For an undo: every answer it would undo, those asked for and those
   * resting on them, oldest first. Empty for choices.
   */
  undoes: Array<string>,
  /**
   * How many songs would be joined into another: copies of one song,
   * such as two editions' of it, that are two songs now and would be one.
   */
  songsJoined: number,
  /**
   * How many songs would be parted from one they are now: copies that
   * are one song now and would be two.
   */
  songsParted: number,
  /**
   * The values the choices would put on the files.
   */
  values: Array<FileValue>,
};

/**
 * What an event on the `progress` topic carries: where the playing entry
 * is, about once a second and at once on a change. The client carries
 * the position on from when the event arrived.
 */
export type ProgressEvent = {
  /**
   * The entry.
   */
  entryId: string,
  /**
   * Where in it.
   */
  positionMs: number,
  /**
   * Whether it is playing.
   */
  playing: boolean,
};

/**
 * One proposal: that one disc of the library is one album.
 */
export type Proposal = {
  /**
   * Its ID. Worked out again, the same proposal keeps it.
   */
  proposalId: string,
  /**
   * Its kind: `album`.
   */
  kind: string,
  /**
   * The proposal its group is named by. Proposals about the same album
   * share it, and are shown and answered together.
   */
  groupId: string,
  /**
   * The root its folder is in, by its name.
   */
  root: string,
  /**
   * The folder, by its path under its root.
   */
  folder: string,
  /**
   * How many files it is about.
   */
  fileCount: number,
  /**
   * Its candidates, the likeliest first.
   */
  candidates: Array<ProposalCandidate>,
  /**
   * A digest of all of it, which a yes names.
   */
  fingerprint: string,
  /**
   * The files, by their paths. Only from `corrections.get`.
   */
  files: Array<string>,
  /**
   * What it rests on. Only from `corrections.get`.
   */
  restsOn: Array<ProposalEvidence>,
};

/**
 * An album a proposal's disc may be on.
 */
export type ProposalCandidate = {
  /**
   * Its MusicBrainz release group ID.
   */
  releaseGroup: string,
  /**
   * Its title.
   */
  title?: string,
  /**
   * Who it is credited to.
   */
  artist: string,
  /**
   * Its kind, such as `Album`.
   */
  primaryType?: string,
  /**
   * The year it first came out.
   */
  firstYear?: number,
  /**
   * How many of the replies' releases are of it.
   */
  releases: number,
  /**
   * What a yes would change.
   */
  change: ProposalChange,
  /**
   * The values a yes would put on the files. Only from
   * `corrections.get`; empty in a list.
   */
  values: Array<FileValue>,
};

/**
 * What a yes to a candidate would change, worked out cheaply. A preview
 * says exactly.
 */
export type ProposalChange = "knows" | "gains" | "moves";

/**
 * What a proposal rests on: one disc, by its layout and the reply about it.
 */
export type ProposalEvidence = {
  /**
   * The disc ID.
   */
  discId: string,
  /**
   * How its layout was found.
   */
  methods: Array<LayoutMethod>,
  /**
   * How many releases the current reply about it names.
   */
  releases: number,
  /**
   * When that reply came, as Unix milliseconds.
   */
  repliedAtMs: number,
};

/**
 * The params of `corrections.get`.
 */
export type ProposalGetParams = {
  /**
   * The proposal.
   */
  proposalId: string,
};

/**
 * The result of `corrections.get`.
 */
export type ProposalGot = {
  /**
   * The library revision it read.
   */
  libraryRevision: number,
  /**
   * The proposal, whole.
   */
  proposal: Proposal,
};

/**
 * The result of `corrections.list`: every open proposal, by group, then
 * root and folder.
 */
export type ProposalList = {
  /**
   * The library revision it read.
   */
  libraryRevision: number,
  /**
   * Each proposal, without its files, evidence or values.
   */
  proposals: Array<Proposal>,
};

/**
 * The public descriptor: what a server says about itself to anyone who
 * asks, with no credential, at [`route::DESCRIPTOR`](crate::route). A
 * client reads it to check which server answered before it sends one.
 */
export type PublicDescriptor = {
  /**
   * The server's ID: made on first start and never changed.
   */
  serverId: string,
  /**
   * The server's display name. Text from another machine: display only.
   */
  name: string,
  /**
   * The server's role.
   */
  role: ServerRole,
  /**
   * The [contract version](crate::CONTRACT_VERSION) the server speaks.
   */
  contractVersion: number,
  /**
   * The capability flags: what this server can do beyond the calls
   * every server has. A client gates a feature on its flag, never on a
   * version, and ignores a flag it does not know. See
   * [`capability`](crate::capability).
   */
  capabilities: Array<string>,
  /**
   * The limits a client must stay within.
   */
  limits: Limits,
  /**
   * What each kind of list supports: its orders and the predicates a
   * filter on it may use. Empty from a server that has no lists.
   */
  lists: Array<ListSupport>,
  /**
   * The sizes a cover is served at, in pixels on its longer side: what
   * `{size}` in `GET /v1/art/{artId}/{size}` may be. Empty from a server
   * that serves no covers.
   */
  artSizes: Array<number>,
  /**
   * The server's Ed25519 public key, as 64 hex digits: what a client or
   * a peer pins. Left out by a server that has none to give.
   */
  publicKey?: string,
  /**
   * The signature, as 128 hex digits, of [`descriptor_proof`] for the
   * nonce the caller sent as `?nonce=`: proof that the answer came from
   * the holder of `public_key`. Left out when no nonce was sent.
   */
  proof?: string,
};

/**
 * The quality of a record: the best copy across its tracks, and whether
 * the tracks differ.
 */
export type Quality = {
  /**
   * The best format any of its tracks has: lossless before lossy, then
   * more bits, a higher rate, a higher bit rate.
   */
  best: Format,
  /**
   * Whether its tracks are in more than one format.
   */
  varies: boolean,
};

/**
 * What a list is of, and in what order.
 */
export type Query = {
  /**
   * A substring that the row's title, artist or album must contain,
   * ignoring case and accents. A query of one or two characters matches
   * the start of a word instead, since nearly every row contains a
   * letter or two.
   */
  text?: string,
  /**
   * A condition every row must satisfy.
   */
  filter?: Filter,
  /**
   * The order. At most one key in this version: each key carries its
   * own tiebreak. Left out, a list takes its kind's first order.
   */
  sort?: Array<Sort>,
};

/**
 * The params of `queue.add`.
 */
export type QueueAddParams = {
  /**
   * The command's ID, made by the client.
   */
  idempotencyKey: string,
  /**
   * What to add.
   */
  source: PlaySource,
  /**
   * Where.
   */
  place: QueuePlace,
  /**
   * Why it is queued. With none, a release is queued as an album and
   * tracks by hand.
   */
  context?: PlayContext,
  /**
   * The queue revision the edit was decided on. If the queue has moved
   * past it, the edit is refused with `conflict` and changes nothing.
   */
  expectedRevision?: number,
  /**
   * Whether to play what was added, from the entry that plays first,
   * if the queue was empty when the server made the edit. Left out, or
   * with anything in the queue, the player stays as it is, and with
   * nothing current waits, stopped, on that entry.
   */
  playIfEmpty?: boolean,
};

/**
 * The params of `queue.clear`.
 */
export type QueueClearParams = {
  /**
   * The command's ID, made by the client.
   */
  idempotencyKey: string,
  /**
   * As for `queue.add`.
   */
  expectedRevision?: number,
};

/**
 * One entry of the queue.
 */
export type QueueEntry = {
  /**
   * The entry.
   */
  entryId: string,
  /**
   * Its track. Its display fields are left out when the library no
   * longer has the appearance.
   */
  track: TrackSummary,
  /**
   * Why it could not be played, once it could not.
   */
  failedReason?: string,
};

/**
 * What an event on the `queue` topic carries: a hint to read the queue
 * again.
 */
export type QueueEvent = {
  /**
   * The queue's revision after the change.
   */
  queueRevision: number,
};

/**
 * The params of `queue.move`.
 */
export type QueueMoveParams = {
  /**
   * The command's ID, made by the client.
   */
  idempotencyKey: string,
  /**
   * The entries, in the order they are to have.
   */
  entryIds: Array<string>,
  /**
   * Where they go: straight after the current entry, straight after
   * another, or last, in both orders.
   */
  place: QueuePlace,
  /**
   * As for `queue.add`.
   */
  expectedRevision?: number,
};

/**
 * Which order `queue.window` reads the queue in.
 */
export type QueueOrder = "queue" | "play" | "up_next";

/**
 * Where `queue.add` puts what it adds.
 */
export type QueuePlace = { "type": "next" } | { "type": "end" } | { "type": "after",
  /**
   * The entry.
   */
  entryId: string,
};

/**
 * The params of `queue.remove`.
 */
export type QueueRemoveParams = {
  /**
   * The command's ID, made by the client.
   */
  idempotencyKey: string,
  /**
   * The entries. One the queue no longer has is passed over. Taking out
   * the current entry moves the player to the next.
   */
  entryIds: Array<string>,
  /**
   * As for `queue.add`.
   */
  expectedRevision?: number,
};

/**
 * The result of `queue.window`. A `queueRevision` other than the one a
 * client read before means it reads again.
 */
export type QueueWindow = {
  /**
   * The queue's revision.
   */
  queueRevision: number,
  /**
   * How many entries there are in the order asked for.
   */
  total: number,
  /**
   * The place of the entry the player is on, in the order asked for.
   */
  currentIndex?: number,
  /**
   * The entries asked for, in queue order.
   */
  entries: Array<QueueEntry>,
};

/**
 * The params of `queue.window`.
 */
export type QueueWindowParams = {
  /**
   * The first entry, from 0.
   */
  offset: number,
  /**
   * How many entries, at most 500. The default is 100.
   */
  limit?: number,
  /**
   * The order. The default is queue order.
   */
  order?: QueueOrder,
};

/**
 * The result of `ratings.set`.
 */
export type RatingSet = {
  /**
   * The recording, as it is now: one merged since is named by the one
   * it was merged into.
   */
  recordingId: string,
  /**
   * Its stars now.
   */
  rating: number | null,
  /**
   * The command's ID: the idempotency key it was sent with.
   */
  commandId: string,
  /**
   * How far the command has got.
   */
  status: ReceiptStatus,
  /**
   * The library revision the command was committed at. It is left out
   * for a command that changes no library.
   */
  revision?: number,
};

/**
 * The params of `ratings.set`.
 */
export type RatingSetParams = {
  /**
   * The command's ID, made by the client.
   */
  idempotencyKey: string,
  /**
   * The recording.
   */
  recordingId: string,
  /**
   * Its stars, 0 to 100, ten to the half star; `null` takes them away.
   */
  rating: number | null,
};

/**
 * What a command returns: that its intent was recorded, and what is known
 * of it so far. A command's own result has these fields and whatever else
 * the command returns.
 *
 * The job a command started and the outcome of each item it covered join
 * with the first commands that have them.
 */
export type Receipt = {
  /**
   * The command's ID: the idempotency key it was sent with.
   */
  commandId: string,
  /**
   * How far the command has got.
   */
  status: ReceiptStatus,
  /**
   * The library revision the command was committed at. It is left out
   * for a command that changes no library.
   */
  revision?: number,
};

/**
 * How far a command has got.
 */
export type ReceiptStatus = "accepted" | "done";

/**
 * The result of `history.recent`.
 */
export type RecentPlays = {
  /**
   * The plays, newest first.
   */
  plays: Array<Play>,
  /**
   * Where the next page begins, when this one was full.
   */
  next?: PlayCursor,
  /**
   * Listens this server has finished and not yet delivered to history.
   */
  undelivered: number,
};

/**
 * One of the records of an artist that was played most.
 */
export type RecordPlayed = {
  /**
   * Its plays, dated or not.
   */
  plays: number,
  /**
   * The year it was played most: the first such, when two tie. Left out
   * when none of its plays has a date.
   */
  peakYear?: number,
  /**
   * What the tile is.
   */
  ref: AlbumRef,
  /**
   * The title of the release it shows. `null` until the library has one.
   */
  title: string | null,
  /**
   * Who it is by.
   */
  artistCredit: Credit | null,
  /**
   * The year it first came out, for a group shown as one tile, or the
   * release's own year.
   */
  year: number | null,
  /**
   * How many tracks the release it shows has.
   */
  trackCount: number,
  /**
   * How long they are together.
   */
  durationMs: number,
  /**
   * How many versions its group has. 1 for a record with one.
   */
  versionCount: number,
  /**
   * Its cover, for `GET /v1/art/{artId}/{size}`. `null` when it has none.
   */
  artId: string | null,
  /**
   * The best copy across its tracks. `null` when none of its tracks has
   * a copy the library has read.
   */
  quality: Quality | null,
};

/**
 * A warning, one thing that has stopped the record advancing, or a note,
 * one thing that happened that no one can act on; and since when.
 */
export type RecordProblem = { kind: RecordProblemKind,
  /**
   * When it began, as Unix milliseconds.
   */
  since: number,
  /**
   * How many listens, scrobbles or imports.
   */
  count: number,
  /**
   * What the newest said went wrong, as the server or the service put
   * it. Display text only.
   */
  detail?: string,
  /**
   * Whether it lasts until `record.dismiss` sets it aside. The others
   * end when what is wrong is put right.
   */
  dismissable: boolean,
};

/**
 * What a warning or a note is about.
 */
export type RecordProblemKind = "listen_unsaved" | "listens_undelivered" | "scrobbles_unknown" | "scrobbles_withdrawn" | "scrobbles_refused" | "sign_in_needed" | "import_failed" | "unreadable";

/**
 * Whether the listening record is advancing: each listen saved and taken
 * into history, each scrobble answered, each import done. Music can go on
 * playing while it does not, so this is said wherever the status is.
 */
export type RecordStatus = {
  /**
   * The warnings: what is wrong now and can be put right, oldest first.
   * Empty when the record advances.
   */
  problems: Array<RecordProblem>,
  /**
   * The notes: what the person should know and can do nothing about,
   * oldest first. They do not stop the record.
   */
  notes: Array<RecordProblem>,
};

/**
 * What kind of record a release group is, as a release-type tag writes it
 * (MusicBrainz's primary type).
 */
export type RecordType = "album" | "ep" | "single" | "broadcast" | "other";

/**
 * Repeat. See "Queue and session" in docs/internals/playback.md.
 */
export type Repeat = "off" | "all" | "one";

/**
 * The view a new one replaces, and the row to keep in place.
 */
export type Replaces = {
  /**
   * The view. It is closed when the new one opens, and calls on it fail
   * with `view_expired` from then on.
   */
  view: string,
  /**
   * The ID of the row the client wants to keep where it is on screen,
   * such as the first one visible: an `artistId`, an album's `ref` ID
   * or an `appearanceId`.
   */
  anchor?: string,
};

/**
 * One decision marked for review.
 */
export type Review = {
  /**
   * Why it was marked.
   */
  cause: ReviewCause,
  /**
   * The file.
   */
  file: FileFacts,
  /**
   * The candidate it nearly matched.
   */
  nearest?: Nearest,
  /**
   * The gap from that candidate to the runner-up.
   */
  runnerUpGap?: number,
  /**
   * How many candidates were weighed, at most five.
   */
  candidates: number,
  /**
   * What stopped an identifier from binding.
   */
  contradictions: Array<string>,
};

/**
 * Why a decision was marked for review, from what made up the distance
 * to the candidate it nearly matched.
 */
export type ReviewCause = "contradiction" | "other_album" | "same_release" | "close_runner_up" | "artist_spelling" | "title_note" | "length_other_album" | "length_same_album" | "other";

/**
 * The result of `identity.review`: every current binding marked for
 * review, grouped by cause.
 */
export type ReviewList = {
  /**
   * The library revision it read.
   */
  libraryRevision: number,
  /**
   * How many there are by cause, in the order they are listed.
   */
  causes: Array<Count>,
  /**
   * Each one, by cause and then by path.
   */
  reviews: Array<Review>,
};

/**
 * One row of a view.
 */
export type Row = { "type": "artist" } & ArtistRow | { "type": "album" } & AlbumTile | { "type": "song" } & SongRow | { "type": "gone" } & GoneRow;

/**
 * A JSON-RPC error object.
 */
export type RpcError = {
  /**
   * The JSON-RPC number. It says less than `data.code` and nothing
   * should be decided by it.
   */
  code: number,
  /**
   * For logs and for a person reading them. Never for parsing.
   */
  message: string,
  /**
   * What a client acts on.
   */
  data: ErrorData,
};

/**
 * A satellite, as its hub knows it.
 */
export type SatelliteStatus = {
  /**
   * The satellite's server ID.
   */
  serverId: string,
  /**
   * Its name when it joined. Text from another machine: display only.
   */
  name: string,
  /**
   * When it joined, as Unix milliseconds.
   */
  linkedAtMs: number,
};

/**
 * The params of `scan.start`.
 */
export type ScanStartParams = {
  /**
   * The command's ID, made by the client.
   */
  idempotencyKey: string,
  /**
   * The source to scan.
   */
  sourceId: string,
};

/**
 * The result of `scan.start`: the receipt, whose status is `accepted`,
 * and the job. Starting a scan of a source that is being scanned returns
 * the job that is running.
 */
export type ScanStarted = {
  /**
   * The job.
   */
  job: Job,
  /**
   * The command's ID: the idempotency key it was sent with.
   */
  commandId: string,
  /**
   * How far the command has got.
   */
  status: ReceiptStatus,
  /**
   * The library revision the command was committed at. It is left out
   * for a command that changes no library.
   */
  revision?: number,
};

/**
 * What a scan found and did.
 */
export type ScanSummary = {
  /**
   * Audio files found under the root.
   */
  files: number,
  /**
   * Files left alone: the same size and time as when last read.
   */
  unchanged: number,
  /**
   * Files at paths the library had not seen.
   */
  new: number,
  /**
   * Files read again at a path it knew: retagged, touched, or the same
   * song encoded again.
   */
  changed: number,
  /**
   * Files whose audio turned up at a new path while the old one was
   * gone.
   */
  moved: number,
  /**
   * Files that had been missing and are back.
   */
  returned: number,
  /**
   * Files where a different song took an old path, so the old item was
   * retired.
   */
  retired: number,
  /**
   * Files the library knew and the scan no longer found.
   */
  missing: number,
  /**
   * Files that could not be read as audio.
   */
  unreadable: number,
  /**
   * Bytes of the files read.
   */
  bytesRead: number,
  /**
   * Bindings written when the library was resolved.
   */
  assignments: number,
  /**
   * Recordings created.
   */
  recordings: number,
  /**
   * Releases created.
   */
  releases: number,
};

/**
 * The params of `lastfm.setScrobbling`.
 */
export type ScrobblingParams = {
  /**
   * The command's ID, made by the client.
   */
  idempotencyKey: string,
  /**
   * On, for plays from now on, or off.
   */
  on: boolean,
};

/**
 * The result of `lastfm.setScrobbling`.
 */
export type ScrobblingSet = {
  /**
   * Plays from this time on are scrobbled. Left out when it is off.
   */
  scrobblingSince?: number,
  /**
   * The command's ID: the idempotency key it was sent with.
   */
  commandId: string,
  /**
   * How far the command has got.
   */
  status: ReceiptStatus,
  /**
   * The library revision the command was committed at. It is left out
   * for a command that changes no library.
   */
  revision?: number,
};

/**
 * The params of `library.search`.
 */
export type SearchParams = {
  /**
   * What was typed. Case and accents are ignored, as `list.open` ignores
   * them, and so is the punctuation between words.
   */
  text: string,
  /**
   * How many of each kind, at most [`MAX_SEARCH_RESULTS`]. Left out, 5.
   */
  limit?: number,
};

/**
 * The result of `library.search`: the best matches of each kind.
 *
 * A row matches when its own name has a word that starts with what was
 * typed: an artist by their name or the name they are filed under, a
 * record or a song by its title. "pine" finds Pinegrove and The Pines and
 * not Happiness. Each kind is ranked the same way: a name that is what was
 * typed, then one that starts with it, then one with a word that does;
 * within each, the most played first, then as the walls file them. A song
 * on several records is listed once.
 */
export type SearchResults = {
  /**
   * Artists who have a record in the library, as the Artists list has
   * them.
   */
  artists: Array<ArtistRow>,
  /**
   * Records, as the Albums list has them.
   */
  albums: Array<AlbumTile>,
  /**
   * Songs, as the Songs list has them, with their record's cover.
   */
  songs: Array<SongFound>,
  /**
   * The kind whose first row is the best match of all: artists before
   * records before songs among equals. Return opens it. `null` when
   * nothing matched.
   */
  top: ListKind | null,
  /**
   * The library revision this was read at.
   */
  libraryRevision: number,
};

/**
 * Where one section of a view begins.
 */
export type Section = {
  /**
   * What the section is: a letter, `#` for names that begin with no
   * letter, a year, a stretch of time such as "This week" or
   * "October", or in a discography the kind of record, such as
   * "Singles".
   */
  label: string,
  /**
   * The index of its first row.
   */
  index: number,
  /**
   * In a discography, the kind of record the section holds. A client
   * chooses its own words and its layout by this, and not by `label`.
   */
  shelf?: Shelf,
};

/**
 * One server's full status: a row of `servers.list`, and what an event on
 * the `servers` topic carries.
 */
export type ServerInfo = {
  /**
   * The server's ID.
   */
  serverId: string,
  /**
   * The server's display name.
   */
  name: string,
  /**
   * The server's role.
   */
  role: ServerRole,
  /**
   * Whether this is the server the client is connected to.
   */
  isThisServer: boolean,
  /**
   * Whether the server can be reached now.
   */
  reachable: boolean,
  /**
   * Counts the changes to this status. A client keeps the copy with the
   * higher revision and drops the other. It counts from when the
   * answering server started, which is enough: a client that connects
   * again reads everything again ("Events and subscriptions" in
   * docs/internals/contract.md).
   */
  revision: number,
};

/**
 * A server's own record of its role. The library's hub pointer decides
 * which server is the hub ("Server identity and hub role" in
 * docs/internals/linking.md).
 */
export type ServerRole = "unassigned" | "hub" | "satellite";

/**
 * The result of `server.settings`: the settings of the server called,
 * a satellite's own on a satellite.
 *
 * Only `awake` is changed by a call. The rest are given as the server
 * starts, by a flag, a `RITORNEL_*` variable or `config.toml`, and hold
 * until it starts again.
 */
export type ServerSettings = {
  /**
   * What the server is called. With no name given as it starts, it is
   * the default: the name it already had, which a new server takes
   * from its computer.
   */
  name: Setting<string>,
  /**
   * When it keeps its machine from sleeping for its linked servers.
   */
  awake: Setting<Awake>,
  /**
   * The most a satellite's cache of masters pulled from its hub holds,
   * in bytes. 2 GiB by default.
   */
  cacheBytes: Setting<number>,
  /**
   * Where it is set to listen. By default on loopback, port 4747, and
   * on a port the system picks when that one is taken.
   */
  listen: Setting<ListenOn>,
  /**
   * The least severe level it logs.
   */
  logLevel: Setting<LogLevel>,
  /**
   * The folder backups are written to, outside the data directory.
   * With none there is no backup.
   */
  backupDir: Setting<string | null>,
};

/**
 * The result of `server.setSettings`: the receipt, and the settings as
 * `server.settings` now reports them.
 */
export type ServerSettingsSet = {
  /**
   * The settings, as `server.settings` now reports them.
   */
  settings: ServerSettings,
  /**
   * The command's ID: the idempotency key it was sent with.
   */
  commandId: string,
  /**
   * How far the command has got.
   */
  status: ReceiptStatus,
  /**
   * The library revision the command was committed at. It is left out
   * for a command that changes no library.
   */
  revision?: number,
};

/**
 * The result of `server.status`. See "Configuration and logging" in
 * docs/internals/server.md.
 */
export type ServerStatus = {
  /**
   * The server's ID.
   */
  serverId: string,
  /**
   * The server's display name.
   */
  name: string,
  /**
   * The server's role.
   */
  role: ServerRole,
  /**
   * What the server is built from and what its database is at.
   */
  versions: Versions,
  /**
   * The library this server holds, as its hub. It is left out when the
   * server holds none.
   */
  library?: LibraryStatus,
  /**
   * The space left where the server keeps what it owns.
   */
  freeDisk: FreeDisk,
  /**
   * The directory the server writes its logs in, on the server's own
   * disk.
   */
  logPath: string,
  /**
   * Whether the server watches its folders for changes, and what it last
   * did on its own. Left out by a server that does not say.
   */
  watch?: WatchStatus,
  /**
   * Where this server backs up, and what its backups last did. Left out
   * by a server that does not say.
   */
  backup?: BackupStatus,
  /**
   * Whether the listening record is advancing, and what has stopped it
   * if not. Left out by a server that does not say.
   */
  record?: RecordStatus,
  /**
   * On a satellite, its hub and whether it answers. Its `library` is
   * then the hub's. Left out on any other server.
   */
  hub?: HubStatus,
};

/**
 * What an event on the `servers` topic carries.
 */
export type ServersEvent = {
  /**
   * The server's status, whole. The client replaces its copy if this
   * one's revision is higher.
   */
  server: ServerInfo,
};

/**
 * The result of `servers.list`.
 */
export type ServersList = {
  /**
   * Every server this one knows, itself included.
   */
  servers: Array<ServerInfo>,
};

/**
 * What a session may do ("Local credentials, sessions, and tokens" in
 * docs/internals/linking.md).
 */
export type SessionKind = "full" | "limited";

/**
 * What gave a setting its value.
 */
export type SetBy = "default" | "file" | "environment" | "flag" | "call";

/**
 * The params of `library.setSettings`: the settings to change, each left
 * out to keep it as it is. At least one is given.
 */
export type SetLibrarySettingsParams = {
  /**
   * The command's ID, made by the client.
   */
  idempotencyKey: string,
  /**
   * Whether MusicBrainz may be asked about the library's discs, one album
   * or folder at a time, when `corrections.lookup` asks.
   */
  lookups?: boolean,
};

/**
 * The params of `server.setSettings`: the settings to change, each left
 * out to keep it as it is. At least one is given.
 */
export type SetServerSettingsParams = {
  /**
   * The command's ID, made by the client.
   */
  idempotencyKey: string,
  /**
   * When to keep the machine awake from now on.
   */
  awake?: Awake,
};

/**
 * The params of `history.setZones`.
 */
export type SetZonesParams = {
  /**
   * The command's ID, made by the client.
   */
  idempotencyKey: string,
  /**
   * Every period, in place of what there was.
   */
  periods: Array<ZonePeriod>,
};

/**
 * A setting's value, and what gave it.
 */
export type Setting<T> = {
  /**
   * The value the server, or the library, runs with.
   */
  value: T,
  /**
   * What gave it.
   */
  setBy: SetBy,
};

/**
 * The kinds of record an artist's discography is filed under, in the
 * order it files them. See [`SortKey::Discography`].
 */
export type Shelf = "albums" | "singles" | "live" | "collections" | "appears_on";

/**
 * The params of `lastfm.signInFinish`.
 */
export type SignInFinishParams = {
  /**
   * The command's ID, made by the client.
   */
  idempotencyKey: string,
  /**
   * The sign-in `lastfm.signInStart` began.
   */
  signIn: string,
};

/**
 * The result of `lastfm.signInFinish`.
 */
export type SignInFinished = {
  /**
   * The account signed in to, once approved.
   */
  account?: string,
  /**
   * The command's ID: the idempotency key it was sent with.
   */
  commandId: string,
  /**
   * How far the command has got.
   */
  status: ReceiptStatus,
  /**
   * The library revision the command was committed at. It is left out
   * for a command that changes no library.
   */
  revision?: number,
};

/**
 * The result of `lastfm.signInStart`: where to approve the sign-in.
 */
export type SignInStarted = {
  /**
   * The sign-in, for `lastfm.signInFinish`.
   */
  signIn: string,
  /**
   * The address at Last.fm where the listener approves it. It carries
   * the API key and a one-hour token, as Last.fm's desktop flow has it.
   */
  approvalUrl: string,
};

/**
 * The result of `lastfm.signOut`.
 */
export type SignedOut = {
  /**
   * Whether there was a session to forget.
   */
  wasSignedIn: boolean,
  /**
   * The command's ID: the idempotency key it was sent with.
   */
  commandId: string,
  /**
   * How far the command has got.
   */
  status: ReceiptStatus,
  /**
   * The library revision the command was committed at. It is left out
   * for a command that changes no library.
   */
  revision?: number,
};

/**
 * Which way a skip goes.
 */
export type SkipDirection = "next" | "previous";

/**
 * The body of [`route::TICKET`](crate::route::TICKET): a ticket that opens
 * one socket.
 */
export type SocketTicket = {
  /**
   * The ticket: what goes in the query string of the socket's URL. It
   * works once.
   */
  ticket: string,
  /**
   * How long the ticket works for, counted from when the server made
   * it. A client opens the socket at once and asks for a new ticket
   * each time it connects.
   */
  expiresInMs: number,
};

/**
 * A song that search found: its row, and the cover of the record it is
 * on, which a Songs row does not carry.
 */
export type SongFound = {
  /**
   * Its record's cover, for `GET /v1/art/{artId}/{size}`. `null` when it
   * has none, or the song is on no record.
   */
  artId: string | null,
  /**
   * The track appearance.
   */
  appearanceId: string,
  /**
   * The recording it is of.
   */
  recordingId: string,
  /**
   * The release it is on. `null` for a loose file.
   */
  releaseId: string | null,
  /**
   * Its title.
   */
  title: string | null,
  /**
   * Who it is by.
   */
  artistCredit: Credit | null,
  /**
   * The title of its release.
   */
  albumTitle: string | null,
  /**
   * Its disc number.
   */
  disc: number | null,
  /**
   * Its track number.
   */
  track: number | null,
  /**
   * The year of its release.
   */
  year: number | null,
  /**
   * How long it is.
   */
  durationMs: number | null,
  /**
   * How many times its recording has been played, by the current rule.
   */
  playCount: number,
  /**
   * When its recording was last played, or `null` when no dated play
   * exists.
   */
  lastPlayedAt: number | null,
  /**
   * When it came into the library.
   */
  addedAt: number | null,
  /**
   * The copies of it that exist, and where.
   */
  copies: Array<TrackCopy>,
  /**
   * The listener's stars for its recording, 0 to 100, ten to the half
   * star, or `null` when it has none.
   */
  rating: number | null,
  /**
   * Whether its recording is loved, here or at a service.
   */
  loved: boolean,
};

/**
 * One row of the Songs list: a track appearance.
 */
export type SongRow = {
  /**
   * The track appearance.
   */
  appearanceId: string,
  /**
   * The recording it is of.
   */
  recordingId: string,
  /**
   * The release it is on. `null` for a loose file.
   */
  releaseId: string | null,
  /**
   * Its title.
   */
  title: string | null,
  /**
   * Who it is by.
   */
  artistCredit: Credit | null,
  /**
   * The title of its release.
   */
  albumTitle: string | null,
  /**
   * Its disc number.
   */
  disc: number | null,
  /**
   * Its track number.
   */
  track: number | null,
  /**
   * The year of its release.
   */
  year: number | null,
  /**
   * How long it is.
   */
  durationMs: number | null,
  /**
   * How many times its recording has been played, by the current rule.
   */
  playCount: number,
  /**
   * When its recording was last played, or `null` when no dated play
   * exists.
   */
  lastPlayedAt: number | null,
  /**
   * When it came into the library.
   */
  addedAt: number | null,
  /**
   * The copies of it that exist, and where.
   */
  copies: Array<TrackCopy>,
  /**
   * The listener's stars for its recording, 0 to 100, ten to the half
   * star, or `null` when it has none.
   */
  rating: number | null,
  /**
   * Whether its recording is loved, here or at a service.
   */
  loved: boolean,
};

/**
 * One key of a query's order.
 */
export type Sort = {
  /**
   * The order.
   */
  key: SortKey,
};

/**
 * An order a list can be read in. Every order ends in a fixed tiebreak,
 * so that it is total and a view of the same query twice has the same
 * order.
 */
export type SortKey = "name" | "artist" | "title" | "year" | "added" | "album" | "discography";

/**
 * One place the library's items come from.
 */
export type Source = {
  /**
   * The source's ID.
   */
  sourceId: string,
  /**
   * Its name, which is the name of its root.
   */
  name: string,
  /**
   * What kind of place it is.
   */
  kind: SourceKind,
  /**
   * Where its root is on the server that answered. It is left out when
   * that server has no path for the root, which is on another server.
   */
  path?: string,
};

/**
 * The result of `sources.add`: the receipt, and the source that now
 * exists.
 */
export type SourceAdded = {
  /**
   * The new source.
   */
  source: Source,
  /**
   * The command's ID: the idempotency key it was sent with.
   */
  commandId: string,
  /**
   * How far the command has got.
   */
  status: ReceiptStatus,
  /**
   * The library revision the command was committed at. It is left out
   * for a command that changes no library.
   */
  revision?: number,
};

/**
 * What kind of place a source is.
 */
export type SourceKind = "folder";

/**
 * The result of `sources.tracks`: the tracks of the files there, in the
 * order they are played: album by album, each in disc and track order. A
 * file the library has not bound to a track yet is left out.
 */
export type SourceTracks = {
  /**
   * The tracks.
   */
  tracks: Array<TrackSummary>,
};

/**
 * The params of `sources.add`.
 */
export type SourcesAddParams = {
  /**
   * The command's ID, made by the client.
   */
  idempotencyKey: string,
  /**
   * The root's name: 1 to 100 characters, with no `/`.
   */
  name: string,
  /**
   * The folder's absolute path on the server. The server only ever
   * reads it.
   */
  path: string,
};

/**
 * The result of `sources.list`.
 */
export type SourcesList = {
  /**
   * Every folder source, in the order they were added.
   */
  sources: Array<Source>,
};

/**
 * The params of `sources.tracks`.
 */
export type SourcesTracksParams = {
  /**
   * An absolute path on the server: a folder in one of its roots, or a
   * file.
   */
  path: string,
};

/**
 * One row of Spotify's extended streaming history, as Ritornel keeps it.
 *
 * It is the whole of what is taken from a row. The row's IP address, user
 * agent, account name and country, its platform as text, and any field
 * this type does not name are never read into it, and the type refuses a
 * field it does not name, so nothing else can ride along with it.
 */
export type SpotifyStream = {
  /**
   * When the stream ended, in Unix milliseconds, to the second.
   */
  endedAt: number,
  /**
   * How long it played.
   */
  msPlayed: number,
  /**
   * The track, as `spotify:track:` and its ID.
   */
  trackUri: string,
  /**
   * The track's title.
   */
  title: string,
  /**
   * The artist, as Spotify named it.
   */
  artist: string,
  /**
   * The album, as Spotify named it.
   */
  album?: string,
  /**
   * Why it began, as Spotify says it, such as `clickrow`.
   */
  reasonStart?: string,
  /**
   * Why it ended, as Spotify says it, such as `trackdone`.
   */
  reasonEnd?: string,
  /**
   * Whether shuffle was on.
   */
  shuffle?: boolean,
  /**
   * Spotify's own mark that the track was skipped.
   */
  skipped?: boolean,
  /**
   * Whether it played offline.
   */
  offline?: boolean,
  /**
   * When offline mode was used, as the row gives it. Its unit is not
   * documented, so it is kept as given.
   */
  offlineTimestamp?: number,
  /**
   * Whether it was played in a private session.
   */
  incognito?: boolean,
  /**
   * The operating system of the device that played it, when the row
   * says.
   */
  platform?: Platform,
  /**
   * The hardware the sound came out of, when the row says.
   */
  deviceKind?: DeviceKind,
  /**
   * How that device played it, when the row says.
   */
  via?: PlayedVia,
};

/**
 * One layer of the picture of a listener's history: the artists first
 * heard in one year, and how much they were played in each month after.
 */
export type StreamLayer = {
  /**
   * The year its artists were first heard.
   */
  year: number,
  /**
   * Their plays in each month of the history, the first first.
   */
  plays: Array<number>,
  /**
   * In each month, the artists of it played most, at most three, most
   * first, by their place in [`HistoryStream::artists`].
   */
  most: Array<Array<number>>,
};

/**
 * A rule that joins a scrobble to a stream.
 */
export type StreamTier = "start" | "during" | "renamed";

/**
 * The result of `events.subscribe` and `events.unsubscribe`.
 */
export type Subscriptions = {
  /**
   * Every topic the session is subscribed to after the call.
   */
  topics: Array<Topic>,
};

/**
 * What a sync of an account's history read and stored.
 */
export type SyncSummary = {
  /**
   * Scrobbles read.
   */
  read: number,
  /**
   * Of those, the ones that were new.
   */
  new: number,
  /**
   * Scrobbles stored before that Last.fm no longer lists. They stay.
   */
  gone: number,
  /**
   * How many scrobbles Last.fm counts in the account's whole history.
   */
  sourceTotal?: number,
  /**
   * How many this library holds, once the sync has ended.
   */
  stored?: number,
  /**
   * Tracks loved now.
   */
  loved?: number,
  /**
   * Tracks whose binding changed when every track was judged again at
   * the end: newly bound, moved to another recording, or released.
   */
  matchedAfter: number,
};

/**
 * What a tier joins.
 */
export type TierJoins = {
  /**
   * The tier.
   */
  tier: StreamTier,
  /**
   * Scrobbles it joins as a stream's own.
   */
  joined: number,
  /**
   * Scrobbles it joins as a second report of a stream that has one.
   */
  second: number,
};

/**
 * What a client can subscribe to.
 */
export type Topic = "servers" | "jobs" | "library" | "list" | "playback" | "progress" | "queue";

/**
 * The params of `events.subscribe` and `events.unsubscribe`.
 */
export type TopicsParams = {
  /**
   * The topics to add or to remove. Naming one twice, adding one the
   * session has, or removing one it has not, changes nothing.
   */
  topics: Array<Topic>,
};

/**
 * One copy of a track: which server has it, and its format.
 */
export type TrackCopy = {
  /**
   * The server that has it. `servers.list` says whether it can be
   * reached.
   */
  server: string,
  /**
   * Which copy it is.
   */
  role: CopyRole,
  /**
   * The codec, such as `flac` or `mp3`.
   */
  codec: string,
  /**
   * Whether it is lossless.
   */
  lossless: boolean,
  /**
   * Bits a sample, for a format that has them.
   */
  bitDepth: number | null,
  /**
   * Samples a second.
   */
  sampleRateHz: number,
  /**
   * The bit rate, for a lossy format.
   */
  bitrateKbps: number | null,
};

/**
 * The result of `tracks.get`: one track, its plays and its loves.
 */
export type TrackDetail = {
  /**
   * The track.
   */
  trackId: string,
  /**
   * Its recording.
   */
  recordingId: string,
  /**
   * The title.
   */
  title?: string,
  /**
   * The artist credit.
   */
  artist?: string,
  /**
   * The album.
   */
  album?: string,
  /**
   * Plays of its recording that count, from every source.
   */
  plays: number,
  /**
   * The last of them.
   */
  lastPlayed?: number,
  /**
   * The loves in force.
   */
  loves: Array<TrackLove>,
};

/**
 * The params of `tracks.get`.
 */
export type TrackGetParams = {
  /**
   * The track.
   */
  trackId: string,
};

/**
 * The result of `history.track`: what Lean back says about the song that
 * is playing and its record, one fact at a time ("History tab and Lean
 * back" in docs/internals/desktop.md).
 *
 * It counts as a page does: the plays that count and are not excluded, of
 * songs the library has bound them to. The song's are the plays of its
 * recording, wherever it was heard; the record's are those heard from one
 * of its versions. A song with no such plays has no facts, and a client
 * shows its title and artist alone.
 */
export type TrackHistory = {
  /**
   * The facts, in the order they are shown.
   */
  facts: Array<Fact>,
  /**
   * The library revision this was read at.
   */
  libraryRevision: number,
};

/**
 * The params of `history.track`.
 */
export type TrackHistoryParams = {
  /**
   * The track: the appearance that is playing, which names its song and
   * its record.
   */
  appearanceId: string,
};

/**
 * A love that reaches a track.
 */
export type TrackLove = {
  /**
   * Who said it: a service and account, as `lastfm:name`, or `you`.
   */
  by: string,
  /**
   * When.
   */
  at: number,
};

/**
 * What a track shows: one appearance's displayed fields.
 */
export type TrackSummary = {
  /**
   * The track appearance.
   */
  appearanceId: string,
  /**
   * Its recording.
   */
  recordingId?: string,
  /**
   * Its release. Left out for a loose file.
   */
  releaseId?: string,
  /**
   * The title.
   */
  title?: string,
  /**
   * The artist credit, as one string.
   */
  artist?: string,
  /**
   * The album's title.
   */
  album?: string,
  /**
   * The disc number.
   */
  discNumber?: number,
  /**
   * The track number.
   */
  trackNumber?: number,
  /**
   * The length.
   */
  durationMs?: number,
  /**
   * The cover of its release.
   */
  artId?: string,
  /**
   * The listener's stars for its recording, 0 to 100, ten to the half
   * star. Left out when it has none.
   */
  rating?: number,
  /**
   * Whether its recording is loved, here or at a service.
   */
  loved: boolean,
};

/**
 * The params of `corrections.undo`.
 */
export type UndoParams = {
  /**
   * The command's ID, made by the client.
   */
  idempotencyKey: string,
  /**
   * The answers to undo, all in one write or none.
   */
  answers: Array<string>,
};

/**
 * The versions a status reports.
 */
export type Versions = {
  /**
   * The server program.
   */
  server: string,
  /**
   * The [contract version](crate::CONTRACT_VERSION) it speaks.
   */
  contract: number,
  /**
   * The SQLite it is built with.
   */
  sqlite: string,
  /**
   * The newest migration `server.db` has had applied.
   */
  serverSchema: number,
  /**
   * The newest migration `library.db` has had applied. It is left out
   * when the server holds no library.
   */
  librarySchema?: number,
};

/**
 * The params of a call that names only a view: `list.sections` and
 * `list.close`.
 */
export type ViewParams = {
  /**
   * The view.
   */
  view: string,
};

/**
 * How changes to a watched folder are noticed.
 */
export type WatchState = "watching" | "polling" | "offline";

/**
 * How a hub notices changes to its folders: it watches them, rescans a
 * music root or imports a picture folder once changes settle, and rescans
 * on a timer as well. See "Watching" in docs/internals/identity.md.
 */
export type WatchStatus = {
  /**
   * Whether this server is noticing changes on its own now.
   */
  on: boolean,
  /**
   * Whether watching is turned on for this machine, by
   * `library.setWatching`. It is on until turned off. Watching also
   * needs this server to be the hub, and a run that watches.
   */
  enabled: boolean,
  /**
   * Why it is not, when it is not.
   */
  off?: string,
  /**
   * Each folder it watches, or would.
   */
  folders: Array<WatchedFolder>,
};

/**
 * A folder a hub watches.
 */
export type WatchedFolder = {
  /**
   * The root's name.
   */
  name: string,
  /**
   * What the folder holds.
   */
  kind: WatchedKind,
  /**
   * How changes to it are noticed.
   */
  state: WatchState,
  /**
   * Why it is rescanned on a timer alone, when it is.
   */
  detail?: string,
  /**
   * When the oldest change not yet taken in was seen, as Unix
   * milliseconds.
   */
  pendingSince?: number,
  /**
   * The last rescan or import the server started on its own.
   */
  lastAuto?: AutoRun,
};

/**
 * What a watched folder holds.
 */
export type WatchedKind = "music" | "pictures";

/**
 * The params of `library.setWatching`.
 */
export type WatchingParams = {
  /**
   * The command's ID, made by the client.
   */
  idempotencyKey: string,
  /**
   * Watch the folders for changes, or stop.
   */
  on: boolean,
};

/**
 * The result of `library.setWatching`: the receipt, and watching as the
 * status now says it.
 */
export type WatchingSet = {
  /**
   * Watching, as `server.status` now says it.
   */
  watch: WatchStatus,
  /**
   * The command's ID: the idempotency key it was sent with.
   */
  commandId: string,
  /**
   * How far the command has got.
   */
  status: ReceiptStatus,
  /**
   * The library revision the command was committed at. It is left out
   * for a command that changes no library.
   */
  revision?: number,
};

/**
 * A stretch of a view's rows: from `offset`, at most `limit` of them.
 */
export type Window = {
  /**
   * The index of the first row.
   */
  offset: number,
  /**
   * How many rows, at most the descriptor's `maxWindowRows`.
   */
  limit: number,
};

/**
 * How many plays fell in one year, by their local dates.
 */
export type YearPlays = {
  /**
   * The year.
   */
  year: number,
  /**
   * The plays.
   */
  plays: number,
};

/**
 * The years a listener's history runs over: from the year of their first
 * dated play to this one. Every picture of years is drawn across it, so
 * that a year stands in the same place on every page.
 */
export type YearSpan = {
  /**
   * The first year.
   */
  from: number,
  /**
   * This year.
   */
  to: number,
};

/**
 * One stretch of time the listener spent in one zone.
 */
export type ZonePeriod = {
  /**
   * The zone, by its IANA name, such as `Europe/London`.
   */
  zone: string,
  /**
   * The first day, as `YYYY-MM-DD` in that zone. Left out for every day
   * before.
   */
  from?: string,
  /**
   * The day after the last, as `YYYY-MM-DD` in that zone. Left out for
   * every day after.
   */
  until?: string,
};

/**
 * The result of `history.zones`. Where periods overlap, the one that
 * began last holds.
 */
export type ZonePeriods = {
  /**
   * The periods.
   */
  periods: Array<ZonePeriod>,
};

/**
 * The result of `history.setZones`.
 */
export type ZonesSet = {
  /**
   * Plays whose date or zone changed.
   */
  changed: number,
  /**
   * The command's ID: the idempotency key it was sent with.
   */
  commandId: string,
  /**
   * How far the command has got.
   */
  status: ReceiptStatus,
  /**
   * The library revision the command was committed at. It is left out
   * for a command that changes no library.
   */
  revision?: number,
};
