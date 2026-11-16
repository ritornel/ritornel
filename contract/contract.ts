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
export const CONTRACT_VERSION = 0;

/** The HTTP routes. Each is a `GET` but `ticket`, which is a `POST`. */
export const ROUTES = {
  ws: "/v1/ws",
  ticket: "/v1/ticket",
  descriptor: "/v1/descriptor",
  health: "/v1/health",
  art: "/v1/art",
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
 * Every method, in the order of the contract's call list: what kind of call
 * it is, and whether it needs a `full` session.
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
  "queue.window": { kind: "query", needsFull: false },
  "sources.tracks": { kind: "query", needsFull: true },
  "history.recent": { kind: "query", needsFull: false },
  "history.onThisDay": { kind: "query", needsFull: false },
  "history.rebuild": { kind: "command", needsFull: true },
  "history.zones": { kind: "query", needsFull: false },
  "history.setZones": { kind: "command", needsFull: true },
  "tracks.get": { kind: "query", needsFull: false },
  "lastfm.status": { kind: "query", needsFull: true },
  "lastfm.signInStart": { kind: "session", needsFull: true },
  "lastfm.signInFinish": { kind: "command", needsFull: true },
  "lastfm.signOut": { kind: "command", needsFull: true },
  "lastfm.sync": { kind: "command", needsFull: true },
  "lastfm.setScrobbling": { kind: "command", needsFull: true },
  "lastfm.report": { kind: "query", needsFull: true },
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
  "queue.window": { params: QueueWindowParams; result: QueueWindow };
  "sources.tracks": { params: SourcesTracksParams; result: SourceTracks };
  "history.recent": { params: HistoryRecentParams; result: RecentPlays };
  "history.onThisDay": { params: OnThisDayParams; result: OnThisDay };
  "history.rebuild": { params: HistoryRebuildParams; result: HistoryRebuilt };
  "history.zones": { params: NoParams; result: ZonePeriods };
  "history.setZones": { params: SetZonesParams; result: ZonesSet };
  "tracks.get": { params: TrackGetParams; result: TrackDetail };
  "lastfm.status": { params: NoParams; result: LastfmStatus };
  "lastfm.signInStart": { params: NoParams; result: SignInStarted };
  "lastfm.signInFinish": { params: SignInFinishParams; result: SignInFinished };
  "lastfm.signOut": { params: KeyParams; result: SignedOut };
  "lastfm.sync": { params: LastfmSyncParams; result: LastfmSyncStarted };
  "lastfm.setScrobbling": { params: ScrobblingParams; result: ScrobblingSet };
  "lastfm.report": { params: LastfmReportParams; result: LastfmReport };
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
   * A picture to draw them with until there are photographs: the cover
   * of their earliest record that has one. `null` when none has.
   */
  artId: string | null,
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
   * A picture to draw them with until there are photographs: the cover
   * of their earliest record that has one. `null` when none has.
   */
  artId: string | null,
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
 * What sort of program a client is. It is for logs and for display, and
 * never decides access: the session does.
 */
export type ClientKind = "desktop" | "browser" | "phone" | "cli" | "agent";

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
   * What it is to the track: `master` in M1.
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
   * several recordings the album cannot tell apart, or `near`, when
   * nothing matches exactly but something does without accents and
   * bracketed notes.
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
 * A filter: the versioned query tree of decision 10 in
 * docs/design/contract.md. Version 1 has one predicate.
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
   * Plays they make, the echoes of plays scrobbled from here not
   * counted.
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
 * What kind of change a library write made, for a client deciding what to
 * read again.
 */
export type LibraryChange = "membership" | "metadata" | "stats";

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
   * client that holds what it read at this revision may keep it.
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
};

/**
 * How an output runs. See decision 5 in docs/design/playback.md.
 */
export type OutputMode = "shared";

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
  id: string,
};

/**
 * What a playback command returns: the receipt, and where the player is
 * after it. The whole state is read with `playback.get` or sent on the
 * `playback` topic.
 */
export type PlaybackChanged = {
  /**
   * The queue's revision after the command.
   */
  queueRevision: number,
  /**
   * The entry the player is on after it.
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
   * The track to start at, from 0. The default is the first.
   */
  start?: number,
  /**
   * Why it is played. With none, a release is played as an album and
   * tracks by hand.
   */
  context?: PlayContext,
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
   * The player's own gain, 0 to 1. Always 1 in M1's first player.
   */
  volume: number,
  /**
   * Whether it is muted.
   */
  muted: boolean,
  /**
   * Whether the queue plays in a shuffled order.
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
 * The policy one dry run decides by: a named policy, and values to put in
 * place of its own. A value left out is the named policy's.
 */
export type PolicyOverrides = {
  /**
   * The named policy, 1 or 2. Left out, it is the one scans resolve by.
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
 * The result of `queue.window`. A `queueRevision` other than the one a
 * client read before means it reads again.
 */
export type QueueWindow = {
  /**
   * The queue's revision.
   */
  queueRevision: number,
  /**
   * How many entries it has.
   */
  total: number,
  /**
   * The place of the entry the player is on.
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
 * What kind of record a release group is, as a release-type tag writes it
 * (MusicBrainz's primary type).
 */
export type RecordType = "album" | "ep" | "single" | "broadcast" | "other";

/**
 * Repeat. M1's first player has none: it is always `off`.
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
   * again reads everything again (docs/design/contract.md, decision 12).
   */
  revision: number,
};

/**
 * A server's own record of its role. The library's hub pointer decides
 * which server is the hub (docs/design/linking.md, decision 2).
 */
export type ServerRole = "unassigned" | "hub" | "satellite";

/**
 * The result of `server.status`. See "Configuration and logs" in
 * docs/design/server.md.
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
 * What a session may do (docs/design/linking.md, decision 6).
 */
export type SessionKind = "full" | "limited";

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
   * Every source, in the order they were added.
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
   * Tracks bound to the library at the end, which the library did not
   * have when they were first seen.
   */
  matchedAfter: number,
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
