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
} as const;

/** Every topic a client can subscribe to. */
export const TOPICS = ["servers"] as const;

/** The params and the result of each method. */
export type MethodMap = {
  "server.hello": { params: HelloParams; result: Descriptor };
  "server.status": { params: NoParams; result: ServerStatus };
  "servers.list": { params: NoParams; result: ServersList };
  "events.subscribe": { params: TopicsParams; result: Subscriptions };
  "events.unsubscribe": { params: TopicsParams; result: Subscriptions };
};

/** The name of a method. */
export type MethodName = keyof MethodMap;

/**
 * What an event on each topic carries: the params of its notification. Its
 * keys are the topics, as `Topic` is.
 */
export type EventMap = {
  "servers": Extract<Event, { topic: "servers" }>;
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
export type Event = { "topic": "servers" } & ServersEvent;

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

export type JsonValue = number | string | boolean | Array<JsonValue> | { [key in string]: JsonValue } | null;

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
export type Topic = "servers";

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
};
