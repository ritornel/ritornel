# Contract

The contract is the single wire schema of RPC calls, server-pushed events, errors, and HTTP endpoints that every client uses to talk to `ritornel-server`. Wire types and the method table live in `crates/ritornel-contract`, generated TypeScript definitions live in [`contract/contract.ts`](../../contract/contract.ts), server handling lives in `apps/server/src/{listener,dispatch,session,views,media,ticket}.rs`, and the shared Rust client lives in `crates/ritornel-client`.

See [`docs/internals/server.md`](server.md) for server lifecycle, credentials, and receipts, and [`docs/internals/schema.md`](schema.md) for the underlying database tables.

## Schema definition and code generation

Every RPC call is defined once in `crates/ritornel-contract/src/methods.rs` as a type implementing `Method` (`NAME`, `KIND`, `NEEDS_FULL`, `Params`, `Result`). Both the server dispatcher (`apps/server/src/dispatch.rs`) and `ritornel-client` bind against those same `Method` types so names and payload types cannot drift.

- **Wire conventions:** JSON object fields use `camelCase`; enum values use `snake_case`. Timestamps are Unix milliseconds (`i64`), durations are milliseconds, and every integer fits within JavaScript's 53-bit safe integer range (`number`, never `bigint`). Public IDs (`Id`) serialize as opaque UUID strings in fields named for their kind (`appearanceId`, `recordingId`, `artistId`, `entryId`).
- **TypeScript generation:** `crates/ritornel-contract/src/typescript.rs` uses `ts-rs` (test-only dev-dependency, never linked into production binaries) and `visit_methods` to generate `contract/contract.ts`—constants (`CONTRACT_VERSION`, `ROUTES`, `CAPABILITIES`, `METHODS`, `TOPICS`, `ART_SIZES`, `FILTER_VERSION`), frame types, `MethodMap`, `EventMap`, and every reachable wire type. A unit test fails CI if `contract/contract.ts` is stale; `mise run contract` regenerates it. Another test verifies every `skip_serializing_if` field also carries `#[cfg_attr(test, ts(optional))]` so omitted fields are never typed as `null`.
- **Decoupled from engine types:** Contract types live in `ritornel-contract` (which has zero workspace dependencies) rather than exposing `ritornel-engine` structs directly, so internal refactors do not break wire compatibility.

## Transport, routes, and authentication

The server exposes a single HTTP/1.1 listener (`apps/server/src/listener.rs`), on loopback or, with `--listen private:<port>`, on the private network (see "Process lifecycle" in docs/internals/server.md), with bounded concurrent connections (`Bounds::max_connections`), header read timeout (`Bounds::header_wait`), and a 16 KiB request header cap (`MAX_HEAD_BYTES`).

| Route | Method | Auth | Purpose |
|---|---|---|---|
| `/v1/descriptor` | `GET` | None | Returns `PublicDescriptor` so a client can verify server identity before sending a credential. It carries the server's Ed25519 `publicKey`, and with `?nonce=` (16 to 64 bytes as hex) a `proof`: the signature of `descriptor_proof(serverId, nonce)`, so a caller that pinned the key knows the holder answered. |
| `/v1/health` | `GET` | Bearer | Returns `{ "status": "ok" }`. |
| `/v1/ticket` | `POST` | Bearer (no `Origin`) | Exchanges a credential for a single-use `SocketTicket` (`Cache-Control: no-store`). |
| `/v1/ws` | `GET` | Bearer or `?ticket=` | WebSocket upgrade carrying JSON-RPC 2.0 text frames. |
| `/v1/art/{artId}/{size}` | `GET` | Bearer or `?token=` | Immutable JPEG cover art at a fixed size bucket. |
| `/v1/audio/{appearanceId}/{sourceItemId}/{revision}` | `GET` | Bearer, link session only | A master, for a satellite to play, as `link.copies` named it. |
| `/v1/link` | `POST` | Invite secret in the body (no `Origin`) | A server joining this hub as a satellite: trades a `LinkRequest` for a `LinkGranted` link-session token. |

All routes except `GET /v1/descriptor` and `POST /v1/link` require authentication (`401 Unauthorized` with `WWW-Authenticate: Bearer` on failure, including unknown paths so unauthenticated callers learn nothing about what exists). Credentials are checked in memory against loaded verifiers in constant time (`subtle::ConstantTimeEq`), never touching disk or SQLite on async worker threads.

### Query-string credentials and origin checks

Native clients (`ritornel-desktop-gpui`, `ritornelctl`) authenticate with `Authorization: Bearer <token>`. Clients that cannot attach headers to WebSocket upgrades or `<img>` requests use scoped, memory-only query-string credentials instead of ever placing the session token in a URL:

- **Socket tickets (`apps/server/src/ticket.rs`):** `POST /v1/ticket` issues 32 random bytes (64 lowercase hex digits) valid for one `GET /v1/ws?ticket=<ticket>` upgrade within `Bounds::ticket_life` (default 10 s), capped at 64 waiting tickets (oldest evicted when full). Redeeming a ticket consumes it immediately regardless of whether the upgrade succeeds. `POST /v1/ticket` rejects any request carrying an `Origin` header (`403 Forbidden`), and no route answers CORS preflight (`OPTIONS`), so arbitrary web pages cannot obtain tickets.
- **Origin check on `/v1/ws`:** A WebSocket upgrade with no `Origin` header (native clients) is allowed. A request carrying an `Origin` header is rejected (`403 Forbidden`) unless it authenticated with a ticket and every `Origin` equals the known desktop webview origin (`tauri://localhost`).
- **Media tokens (`apps/server/src/media.rs`):** `media.token` returns a session-scoped 64-hex-digit token valid only on `GET /v1/art/...?token=<token>` for the lifetime of that WebSocket session, revoked when the session closes.

### Link sessions

A hub links a satellite with a one-time invite (`link.invite`): an `InviteLink` (`ritornel://link/<hubId>?key=<publicKey>&route=<address:port>…#<secret>`) valid for ten minutes and kept in memory only. The joining server (`link.join`) dials only private routes, checks the hub's descriptor `proof` against the pinned key, and posts a `LinkRequest` to `POST /v1/link` with the secret, its server ID, its public key and a signature of `link_request_proof(hubId, secret)`. The hub refuses another `CONTRACT_VERSION`, saying which side is the older and so which to update, a missing or wrong proof, a spent or expired secret, a request that names an `Origin`, and a server ID already linked under another key; otherwise it registers the satellite in the library and issues a link session (`apps/server/src/dispatch/link.rs`).

A link session is a `full` session that knows its satellite (`Session::peer`). It authenticates on the socket upgrade with the bearer token, never with a ticket, and the hub refuses it the calls that name the hub's own folders, backups and links (`kept_from_a_link`). A satellite joining again closes the sockets of its previous session.

### Satellites

A satellite (`apps/server/src/uplink.rs`) answers its own status, events, player and queue, link and backups, and forwards every other call to its hub unchanged, on a hub connection of the client session's own, with the client's request IDs. That connection is opened on the session's first forwarded call or subscription, subscribed to the hub's `library` and `jobs` topics the session holds, and closed with the session; the hub's `library`, `list` and `jobs` events on it go to that session alone. Calls that name the hub's folders are refused with `unsupported` and the reason `on_the_hub`. Covers are fetched from the hub with the link session, on the route where the hub last proved its key, and kept in the satellite's cover cache.

Each connection walks the hub's routes in order (at most eight), checking the descriptor `proof` before presenting the link session. A connection gets four seconds a route and a call eight; a lost connection is replaced once at once, but not after a wait ran out. While the hub does not answer, a forwarded call fails with `unavailable`, reason `hub_unreachable`, the hub's name and `sinceMs`, and for two seconds after a failed try without trying again. `server.status` on a satellite reports the hub's library ID as its `library` and a `hub` (`HubStatus`) with `answers`, `sinceMs`, `route` and `problem`. A satellite says hello to its hub with `ClientKind::Server`. It tells its own clients each time the hub begins or stops answering, with a `servers` event carrying its status and so its `hub`, and asks the hub every ten seconds so that a hub that went while nothing was asked of it is found out (`uplink::follow`).

A satellite's listens reach the hub with `link.listen`, keyed on the satellite's player endpoint and the listen's ID, so a resend is `duplicate` and a differing one is `conflict` and kept beside the first in `history_ingest_conflict`. The hub takes them from a link session only, for that satellite's own player, of the current `LISTEN_RECORD_VERSION`, and with every number within 2^50.

### Transport-neutral dispatcher

`Core::dispatch` takes a decoded `Frame` and a `Session` and returns an `Option<Frame>` without knowing anything about sockets. Both the WebSocket transport (`listener.rs`) and the in-memory test transport (`apps/server/src/memory.rs`) run through `Core::receive`, enforcing `MAX_MESSAGE_BYTES` (1 MiB) and full JSON text encoding/decoding (`Frame::encode` / `Frame::decode`). Production clients always connect over HTTP/WebSocket; the in-memory channel is used only by tests.

## Session and frames

Frames follow JSON-RPC 2.0 (`crates/ritornel-contract/src/frame.rs`), one text message per frame:

- **Requests, responses, notifications:** Calls are requests (`id`, `method`, optional `params`) receiving exactly one response (`id`, `result` or `error`). Server-pushed events are notifications (`method: "event"`, `params`, no `id`). JSON-RPC batches (arrays) are refused, and binary WebSocket messages close the connection.
- **Strict request IDs:** `RequestId` accepts only a JSON string or whole integer (`i64`). `null` and fractional/exponent numbers (`1.0`, `1.5`, `1e2`) are rejected as `-32600` invalid requests (matching MCP conventions). Unknown top-level members are ignored.
- **Frame size cap:** Messages larger than `MAX_MESSAGE_BYTES` (1 MiB) are rejected before decoding and close the connection.
- **Handshake:** The first call on every session must be `server.hello` with `{ name, kind, version, contractVersion }` (`name` and `version` 1–`MAX_CLIENT_TEXT` = 100 chars). Any other call before a successful handshake fails with `invalid` (`details.reason = "hello_required"`). The server rejects any other `CONTRACT_VERSION` with `unsupported`. The number changes whenever the wire does: `contract/contract.version` records it with a digest of `contract.ts` (comments left out), and a test fails when the digest changes and the number has not, so two builds with one number speak one contract. On success, `server.hello` returns `Descriptor` (`PublicDescriptor` plus `sessionKind`).
- **Session kinds:** Every method declares `NEEDS_FULL` (`needsFull` in `METHODS`). `limited` sessions may read the library (`list.*`, `library.search`, `library.stats`, `album.get`, `artist.get`, `tracks.get`, `history.*` reads, `servers.list`, `media.token`) and control playback/queue (`playback.*`, `queue.*`). `full` sessions are required for everything else—including `server.status`, `library.create`, sources, scans, jobs (and subscribing to the `jobs` topic), identity diagnostics, `ratings.set`, history rebuild/zones/export, Last.fm calls, artist picture folders/imports, `library.setWatching`, `server.backup`, and `record.dismiss` (see `METHODS` in `methods.rs` for the authoritative list).

## Entities and identifiers

Clients address library entities by opaque IDs, never file paths (paths appear only in source/picture-folder management, backup/export, and identity diagnostics):

- **Polymorphic references:** An Albums tile uses tagged `AlbumRef` (`{"type": "release_group", "id": "..."}` when collapsed or `{"type": "release", "id": "..."}` when expanded). `album.get` accepts either variant.
- **Copies and reachability:** `SongRow.copies` lists `TrackCopy` entries (`server`, `role: master | derived`, and structured `Format`: `codec`, `lossless`, `bitDepth`, `sampleRateHz`, `bitrateKbps`) without embedding reachability per row. Clients join `server` against `servers.list` (`reachable`) so a server state change updates one small resource rather than every row.
- **Play counts and ratings:** `SongRow` carries recording-level `playCount`, `lastPlayedAt`, `rating` (0–100 or `null`), and `loved`. Loose files have `releaseId: null` and appear in Songs and on artist pages rather than in Albums.
- **Redirects:** Calls naming a merged recording (such as `ratings.set`) follow `canonical_id` and return the canonical ID in the response.

## Large lists

Browsing `artists`, `albums`, and `songs` (`ListKind`) uses server-held immutable **views** (`crates/ritornel-contract/src/list.rs`, `apps/server/src/views.rs`, `crates/ritornel-engine/src/list/`):

1. **Open (`list.open`):** Resolves `Query` (`text`, `filter`, `sort`) once inside a short SQLite read transaction into an ordered `Vec<i64>` of internal keys and precomputed `sections`, returning `{ view, total, libraryRevision, offset, rows, anchorIndex }`. Because only keys are frozen (not a long-lived SQLite read transaction), WAL checkpointing is never blocked and scrollbars have an exact `total`.
2. **Window (`list.window`):** Reads up to `MAX_WINDOW_ROWS` (500) rows at `[offset, offset + limit)` fresh from `library.db`. A row removed since the view opened returns `{"type": "gone", "gone": true}` at its original index.
3. **Jump and locate (`list.sections`, `list.locate`):** `list.sections` returns letter, year, time-range, or discography `Shelf` (`albums`, `singles`, `live`, `collections`, `appears_on`) section offsets. `list.locate` finds a row index by row ID or by a typed `prefix` folded the same way as the sort order.
4. **Querying and sorting:** `Query.text` matches title, artist, and album case- and accent-insensitively (FTS5 trigram for 3+ characters; word-prefix matching for 1–2 characters). `Query.filter` is a versioned predicate tree (`FILTER_VERSION = 1`: `by_artist`, `with_artist`, `no_record`, `all`). `SortKey` (`name`, `artist`, `title`, `year`, `added`, `album`, `discography`) applies library filing rules (ignoring leading articles, accents, and case) with a fixed tiebreak so every order is total. Supported sorts and predicates per `ListKind` are advertised in `PublicDescriptor.lists`.
5. **Invalidation and replacement:** When `library.db` commits, `Fanout` (`apps/server/src/dispatch/lists.rs`) emits `list` events for affected views. Play-count or rating-only writes (`LibraryChange::Stats` or `LibraryChange::Ratings`) notify only views that display plays (`songs`) with `reorder: false` so visible windows re-read in place. All other library writes set `reorder: true`; clients replace the view via `list.open` with `replaces: { view, anchor }`, which atomically closes the old view and returns `anchorIndex` in the new view.
6. **Lifetime:** Views belong to their session and end on `list.close`, replacement, session close, idle timeout (`VIEW_IDLE` = 10 minutes), or LRU eviction past `MAX_VIEWS` (16 per session). Calling an expired view fails with `view_expired`.

Three read paths do not use views:

- **`library.search`** — Stateless single-call dropdown search returning top matching `artists`, `albums`, and `songs` (with album `artId`) plus `top` kind (default 5 per kind, max `MAX_SEARCH_RESULTS` = 20).
- **`queue.window`** — Direct revision-stamped reads of the playback queue (`offset`, `limit` up to 500, `order: queue | play | up_next`) returning `queueRevision`, `total`, `currentIndex`, and `entries`.
- **Direct history queries** — `history.recent` (cursor-paged), `history.onThisDay`, `history.artist`, `history.album`, `history.track`, and `history.stream` answer listening-history reads directly without opening a view.

## Events and subscriptions

Sessions opt into server-pushed notifications via `events.subscribe` and `events.unsubscribe`. Holding an open list view automatically subscribes the session to `list` events for that view. Unsubscribing drops any queued unsent events for those topics immediately so no event on an unsubscribed topic follows the response.

| Topic | Payload | Coalescing key | Client action |
|---|---|---|---|
| `servers` | Full `ServerInfo` snapshot | `serverId` | Replace server entry (keep higher `revision`). |
| `jobs` | Full `Job` summary (`target` included) | `jobId` | Replace job entry (requires `full` session). |
| `library` | `libraryRevision` + `changed` (`membership`, `metadata`, `stats`, `ratings`) | Singleton | Re-read non-view screens older than `libraryRevision`. |
| `list` | `view`, `libraryRevision`, `reorder` | `view` | Re-read visible windows (`reorder: false`) or replace view (`reorder: true`). |
| `playback` | Full `PlaybackState` snapshot | Singleton | Replace playback state (keep `revision >=` current). |
| `progress` | `entryId`, `positionMs`, `playing` | Singleton | Anchor position and extrapolate locally between ~1 s ticks. |
| `queue` | `queueRevision` | Singleton | Re-read visible queue windows. |

Events never carry deltas and are never replayed across disconnects. Each session maintains an ordered outbound queue (`apps/server/src/session.rs`) that never drops responses and holds at most one unsent event per `(topic, key)`. Merging a newer event into an unsent slot preserves its queue position while taking the highest revision, the union of `LibraryChange` values, and `reorder |= new.reorder` so a reorder signal is never masked by a subsequent play-count update. After reconnecting, clients re-handshake, resubscribe, reopen views, and re-read visible state.

## Commands, playback, and jobs

Every mutating call (`MethodKind::Command`) carries a client-generated UUID `idempotencyKey` and returns a `Receipt` (`commandId`, `status: accepted | done`, optional `revision`) flattened into its result object:

- **Idempotency:** Library commands (`library_command` in `library.db`) and server/job/playback commands (`command_receipt` in `server.db` or the player's recent-command buffer) replay the original receipt when retried with the same key and request fingerprint, and fail with `idempotency_mismatch` if the same key is reused with different parameters. Four commands intentionally return a fresh non-stored receipt instead: `server.backup` and `history.export` (each invocation writes a new timestamped folder), `lastfm.signInFinish` (deduplicated by its sign-in token), and `record.dismiss` (idempotent by current library revision).
- **Optimistic concurrency:** Queue mutation calls (`queue.add`, `queue.remove`, `queue.move`, `queue.clear`) accept optional `expectedRevision` and return `conflict` with `details.currentRevision` if the queue moved.
- **Retry-safe playback:** There is no play/pause toggle; `playback.set` takes absolute `volume` (0–1), `muted`, `shuffle`, and `repeat`; `playback.seek` and `playback.skip` name the target `entryId` / `fromEntryId` so late retries cannot seek or skip a different track; queue mutations address entries by `entryId`, never by index. `playback.play` and `queue.add` accept `PlaySource` (`tracks`, `release`, or `view`, copying IDs out of the view snapshot into the queue); `queue.add` supports `playIfEmpty`. A server without an audio output omits the `playback` capability and refuses playback/queue calls with `unsupported`.
- **Durable jobs:** Long-running work (`scan.start`, `lastfm.sync`) creates or returns a durable `Job` in `server.db` (`queued`, `running`, `cancelling`, `succeeded`, `failed`, `cancelled`) with counters (`done`, `total`, `bytes`, `failed`, `skipped`), `label`, `error`, and kind-specific summaries (`scan`, `sync`). Starting a scan for a root already being scanned returns the running job. `jobs.list` returns up to 100 recent jobs; `jobs.get` returns a job plus up to 100 failed `JobItem` rows; `jobs.cancel` requests cooperative cancellation after the in-flight step commits.

## Media

Cover art and artist portraits are served over plain HTTP at `GET /v1/art/{artId}/{size}`:

- **Fixed buckets and immutability:** `{size}` must be one of `ART_SIZES` (`[64, 96, 128, 192, 256, 384, 512, 768, 1024]`, longest edge in pixels), also advertised in `PublicDescriptor.artSizes`. Responses are `image/jpeg` with `Cache-Control: private, max-age=31536000, immutable`; a changed image gets a new `artId`. Invalid IDs, non-bucket sizes, or missing covers return `404 Not Found`.
- **Masters for satellites:** `GET /v1/audio/{appearanceId}/{sourceItemId}/{revision}` serves a master to a link session alone, streamed in 256 KiB chunks, and only while the file is at the revision `link.copies` offered and its size and modification time are what the scan recorded (`409 Conflict` otherwise). A satellite pulls the whole file into `cache/audio/` in its data directory, stopping if it grows past the offered `sizeBytes`, and plays it from there (`apps/server/src/hub_catalog.rs`); the cache keeps the newest 2 GiB.
- **On-demand rendering (`apps/server/src/media.rs`):** Cache hits are read via `spawn_blocking`. Cache misses run on a dedicated 4-thread `CoverPool` in LIFO (newest-first) order so covers currently scrolled into view render before stale off-screen requests. Dropping a cancelled HTTP request cancels its queued job before it starts, and each worker catches panics so a bad image cannot shrink the pool.

## Errors and capabilities

Every RPC failure returns a JSON-RPC error object `{ code, message, data: { code, retryable, details } }` (`crates/ritornel-contract/src/error.rs`). Clients branch strictly on `data.code`, never on `message` or the JSON-RPC integer `code`.

| `data.code` | JSON-RPC `code` | `retryable` | Meaning |
|---|---|---|---|
| `invalid` | `-32602` (`-32700` parse, `-32600` frame) | `false` | Malformed frame, bad params, or missing `server.hello`. |
| `not_found` | `-32000` | `false` | Referenced ID does not exist. |
| `conflict` | `-32000` | `false` | State or `expectedRevision` conflict (`details.currentRevision` when applicable). |
| `plan_stale` | `-32000` | `false` | A previously computed plan is no longer valid. |
| `view_expired` | `-32000` | `false` | List view closed, evicted, or timed out; reopen it. |
| `unavailable` | `-32000` | `true` | Resource exists or server is healthy, but cannot serve now (for example no library yet, stopping, or missing export folder). |
| `forbidden` | `-32000` | `false` | Call or topic requires a `full` session. |
| `unsupported` | `-32601` | `false` | Unknown method, contract version mismatch, unsupported filter version, or missing server capability (such as no audio output). |
| `idempotency_mismatch` | `-32000` | `false` | Same `idempotencyKey` reused with different parameters. |
| `internal` | `-32603` | `false` | Unexpected server failure (logged by server). |

Connection drops are client-side transport errors; the server never emits a `transport` code.

Clients gate optional behavior on `PublicDescriptor.capabilities` rather than version numbers:

- `events.servers` — `servers` event topic.
- `events.library` — `library` event topic.
- `lists` — `list.*` view calls and `list` event topic (supported kinds/sorts/predicates in `lists`).
- `media.art` — `media.token` and `GET /v1/art/{artId}/{size}` (supported sizes in `artSizes`).
- `playback` — `playback.*` and `queue.*` calls and `playback`, `progress`, `queue` topics (advertised only when the server has a player).

## Method summary

All 69 methods are registered in `crates/ritornel-contract/src/methods.rs` (see `contract/contract.ts` for exact field types):

| Area | Methods |
|---|---|
| Session & server | `server.hello`, `server.status`, `servers.list`, `events.subscribe`, `events.unsubscribe`, `server.backup`, `record.dismiss` |
| Linking | `link.invite`, `link.join`, `link.status`, `link.tracks`, `link.copies`, `link.listen` |
| Library & sources | `library.create`, `sources.list`, `sources.add`, `sources.tracks`, `scan.start`, `library.stats`, `library.setWatching`, `artists.importPictures`, `artists.pictureFolders` |
| Jobs & identity | `jobs.list`, `jobs.get`, `jobs.cancel`, `identity.dryRun`, `identity.review` |
| Browse & media | `list.open`, `list.window`, `list.sections`, `list.locate`, `list.close`, `library.search`, `album.get`, `artist.get`, `tracks.get`, `media.token` |
| Playback & queue | `playback.get`, `playback.play`, `playback.pause`, `playback.resume`, `playback.stop`, `playback.seek`, `playback.skip`, `playback.jump`, `playback.set`, `queue.window`, `queue.add`, `queue.remove`, `queue.move`, `queue.clear` |
| History & ratings | `history.recent`, `history.onThisDay`, `history.artist`, `history.album`, `history.track`, `history.stream`, `history.rebuild`, `history.zones`, `history.setZones`, `history.export`, `ratings.set` |
| Last.fm | `lastfm.status`, `lastfm.signInStart`, `lastfm.signInFinish`, `lastfm.signOut`, `lastfm.sync`, `lastfm.setScrobbling`, `lastfm.report` |

## Decisions

- **JSON-RPC 2.0 over one WebSocket plus plain HTTP for media.** One socket orders calls and pushed events without a proxy; plain HTTP lets cover art use standard caching and `Cache-Control: immutable`.
- **Views freeze keys, not contents.** Holding a `Vec<i64>` per view keeps memory low and avoids holding SQLite read transactions open across client think time, while `list.window` always returns fresh metadata, play counts, and ratings.
- **Events carry no deltas and no replay log.** Coalescing per `(topic, key)` bounds server memory for slow clients and keeps reconnect recovery simple (re-handshake, resubscribe, reopen views).
- **Idempotency keys on every command, retry-safe playback by design.** Commands carry an `idempotencyKey` and replay stored receipts (except the four commands noted under "Commands, playback, and jobs" whose semantics do not store receipts), so clients can safely retry timed-out commands without duplicating side effects, and late playback commands targeting a previous entry become no-ops rather than skipping ahead twice.

## Later

A few wire types already carry variants reserved for upcoming work:

- `ErrorCode::PlanStale` is defined for future plan-then-commit destructive commands.
- `PlaybackStatus::Buffering` is defined for streamed playback.
- `ClientKind` already includes `browser`, `phone`, and `agent` alongside `desktop` and `cli`.
