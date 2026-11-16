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
} as const;

/**
 * The name of the query parameter that carries a ticket on the socket's
 * route.
 */
export const TICKET_PARAM = "ticket";

/** The capability flags a server may list in its descriptor. */
export const CAPABILITIES = {
  eventsServers: "events.servers",
} as const;

/** The method of every event notification. */
export const EVENT_METHOD = "event";

/** The longest a client's name or version may be, in characters. */
export const MAX_CLIENT_TEXT = 100;

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
} as const;

/** Every topic a client can subscribe to. */
export const TOPICS = ["servers", "jobs", "library"] as const;

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
 * What sort of program a client is. It is for logs and for display, and
 * never decides access: the session does.
 */
export type ClientKind = "desktop" | "browser" | "phone" | "cli" | "agent";

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
export type Event = { "topic": "servers" } & ServersEvent | { "topic": "jobs" } & JobsEvent | { "topic": "library" } & LibraryEvent;

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
 * What about the library changed.
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
 * What an event on the `library` topic carries: a bare hint to read
 * again whatever is on screen and older than the revision.
 */
export type LibraryEvent = {
  /**
   * The library revision after the change.
   */
  libraryRevision: number,
  /**
   * What changed.
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
};

/**
 * The params of a method that takes none: an empty object, which may also
 * be left out of the frame.
 */
export type NoParams = Record<symbol, never>;

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
 * The result of `events.subscribe` and `events.unsubscribe`.
 */
export type Subscriptions = {
  /**
   * Every topic the session is subscribed to after the call.
   */
  topics: Array<Topic>,
};

/**
 * What a client can subscribe to.
 */
export type Topic = "servers" | "jobs" | "library";

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
