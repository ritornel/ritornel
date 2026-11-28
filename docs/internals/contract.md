# Contract

The contract is the single wire schema of RPC calls, server-pushed events, errors, and HTTP endpoints that every client uses to talk to `ritornel-server`. Wire types and the method table live in `crates/ritornel-contract`, generated TypeScript definitions live in [`contract/contract.ts`](../../contract/contract.ts), server handling lives in `apps/server/src/{listener,dispatch,session,views,media,ticket}.rs`, and the shared Rust client lives in `crates/ritornel-client`.

See [`docs/internals/server.md`](server.md) for server lifecycle, credentials, and receipts, and [`docs/internals/schema.md`](schema.md) for the underlying database tables.

## Schema definition and code generation

Every RPC call is defined once in `crates/ritornel-contract/src/methods.rs` as a type implementing `Method` (`NAME`, `KIND`, `NEEDS`, `LINK_ONLY`, `Params`, `Result`). Both the server dispatcher (`apps/server/src/dispatch.rs`) and `ritornel-client` bind against those same `Method` types so names and payload types cannot drift.

- **Wire conventions:** JSON object fields use `camelCase`; enum values use `snake_case`. Timestamps are Unix milliseconds (`i64`), durations are milliseconds, and every integer fits within JavaScript's 53-bit safe integer range (`number`, never `bigint`). Public IDs (`Id`) serialize as opaque UUID strings in fields named for their kind (`appearanceId`, `recordingId`, `artistId`, `entryId`).
- **TypeScript generation:** `crates/ritornel-contract/src/typescript.rs` uses `ts-rs` (test-only dev-dependency, never linked into production binaries) and `visit_methods` to generate `contract/contract.ts`—constants (`CONTRACT_VERSION`, `ROUTES`, `CAPABILITIES`, `PERMISSIONS`, `TOKEN_ROLES`, `METHODS`, `TOPICS`, `ART_SIZES`, `FILTER_VERSION`), frame types, `MethodMap`, `EventMap`, and every reachable wire type. A unit test fails CI if `contract/contract.ts` is stale; `mise run contract` regenerates it. Another test verifies every `skip_serializing_if` field also carries `#[cfg_attr(test, ts(optional))]` so omitted fields are never typed as `null`.
- **Decoupled from engine types:** Contract types live in `ritornel-contract` (which has zero workspace dependencies) rather than exposing `ritornel-engine` structs directly, so internal refactors do not break wire compatibility.

`server.status` and `server.settings` report the effective profiles, encoder capabilities and cache usage under `transcode`. A session with `server:manage` on the hub can materialize or prune cached audio. Command receipts identify durable jobs, and `jobs.get` returns the completed artifact keys and hashes. See [Transcoding](transcoding.md).

## Transport, routes, and authentication

The server exposes a single HTTP/1.1 listener (`apps/server/src/listener.rs`), on loopback or, with `--listen private:<port>`, on the private network (see ["Process lifecycle" in `docs/internals/server.md`](server.md#process-lifecycle)), with bounded concurrent connections (`Bounds::max_connections`), header read timeout (`Bounds::header_wait`), and a 16 KiB request header cap (`MAX_HEAD_BYTES`).

| Route | Method | Auth | Purpose |
|---|---|---|---|
| `/v1/descriptor` | `GET` | None | Returns `PublicDescriptor` so a client can verify server identity before sending a credential. It carries the server's Ed25519 `publicKey`, and with `?nonce=` (16 to 64 bytes as hex) a `proof`: the signature of `descriptor_proof(serverId, nonce)`, so a caller that pinned the key knows the holder answered. |
| `/v1/health` | `GET` | Bearer (`status:read`) | Returns `{ "status": "ok" }`. |
| `/v1/ticket` | `POST` | Bearer (no `Origin`) | Exchanges a credential for a single-use `SocketTicket` (`Cache-Control: no-store`). |
| `/v1/ws` | `GET` | Bearer or `?ticket=` | WebSocket upgrade carrying JSON-RPC 2.0 text frames. |
| `/v1/art/{artId}/{size}` | `GET` | Bearer (`library:read`) or `?token=` | Immutable JPEG cover art at a fixed size bucket. |
| `/v1/audio/{appearanceId}/{sourceItemId}/{revision}` | `GET` | Bearer, link session only, over TLS | A master, for a satellite to play, as `link.copies` named it. |
| `/v1/cache/derivative/{key}` | `GET`, `HEAD` | Bearer, link session only, over TLS | Encoded audio with its exact byte count. |
| `/v1/link` | `POST` | Invite secret in the body, over TLS (no `Origin`) | A server joining this hub as a satellite: trades a `LinkRequest` for a `LinkGranted` link-session token. |

Every route is served over plain HTTP or over TLS pinned to the server's identity key, which the connection's first byte chooses (see ["Listener and transport boundaries" in `docs/internals/linking.md`](linking.md#listener-and-transport-boundaries)). A link session's token is taken over TLS alone, and over plain HTTP gets the same `401` as an unknown one. `POST /v1/link` over plain HTTP is `409`, saying the joining server runs an older release, and withdraws the invite it names, since its secret has travelled where anyone could read it.

All routes except `GET /v1/descriptor` and `POST /v1/link` require authentication (`401 Unauthorized` with `WWW-Authenticate: Bearer` on failure, including unknown paths so unauthenticated callers learn nothing about what exists). Credentials are checked in memory against loaded verifiers in constant time (`subtle::ConstantTimeEq`), never touching disk or SQLite on async worker threads.

On both cache routes the key is a 64-digit hexadecimal digest. A valid link receives `400` for a malformed key and `404` for a missing entry in that layer. A valid local credential receives `403`. A single satisfiable byte range returns `206` with `Content-Range` and the selected `Content-Length`. An invalid or unsatisfiable range returns `416` with `Content-Range: bytes */<size>`.

### Query-string credentials and origin checks

Native clients (`ritornel-desktop-gpui`, `ritornelctl`) authenticate with `Authorization: Bearer <token>`. Clients that cannot attach headers to WebSocket upgrades or `<img>` requests use scoped, memory-only query-string credentials instead of ever placing the session token in a URL:

- **Socket tickets (`apps/server/src/ticket.rs`):** `POST /v1/ticket` issues 32 random bytes (64 lowercase hex digits) valid for one `GET /v1/ws?ticket=<ticket>` upgrade within `Bounds::ticket_life` (default 10 s), capped at 64 waiting tickets (oldest evicted when full). Redeeming a ticket consumes it immediately regardless of whether the upgrade succeeds. `POST /v1/ticket` rejects any request carrying an `Origin` header (`403 Forbidden`), and no route answers CORS preflight (`OPTIONS`), so arbitrary web pages cannot obtain tickets.
- **Origin check on `/v1/ws`:** A WebSocket upgrade with no `Origin` header (native clients) is allowed. A request carrying an `Origin` header is rejected (`403 Forbidden`) unless it authenticated with a ticket and every `Origin` equals the legacy webview origin (`tauri://localhost`).
- **Media tokens (`apps/server/src/media.rs`):** `media.token` returns a session-scoped 64-hex-digit token valid only on `GET /v1/art/...?token=<token>` for the lifetime of that WebSocket session, revoked when the session closes.

### Link sessions

A hub links a satellite with a one-time invite (`link.invite`): an `InviteLink` (`ritornel://link/<hubId>?key=<publicKey>&route=<host:port>…#<secret>`) valid for ten minutes and kept in memory only. `link.invite` defaults to `TokenRole::Personal` and may name a different `role` or explicit `permissions` subset, refusing any permission the caller does not hold as well as hub-machine administration permissions (`folders:manage`, `server:manage`, `services:connect`, `access:manage`). The joining server (`link.join`) dials only private routes, over TLS pinned to the invite's key, checks the hub's descriptor `proof` against that key, and posts a `LinkRequest` to `POST /v1/link` on another such connection with the secret, its server ID, its public key and a signature of `link_request_proof(hubId, serverId, contractVersion, secret)`, so a join read on its way cannot be sent again as another server or another release. The secret is what lets a server join, so it travels only inside TLS, and a hub withdraws an invite whose secret came to it over plain HTTP, from the moment it has the request. The hub serializes link grants and refuses another `CONTRACT_VERSION`, saying which side is the older and so which to update, a missing or wrong proof, a server ID already linked under another key (checked before consuming the invite), a spent or expired secret, and a request that names an `Origin`; otherwise it registers the satellite in the library and issues a link session (`apps/server/src/dispatch/link.rs`).

A link session knows its satellite (`Session::peer`) and holds the `SessionGrant` carried by its invite (persisted in `server.db`'s `session` table as `role` and `permissions_json`). It authenticates on the socket upgrade with the bearer token, over TLS, never with a ticket. It may call only what a satellite sends ("What a link carries" in [`docs/internals/linking.md`](linking.md#hub-and-satellites)), and only link sessions holding the required permissions may call `link_only` methods (`link.tracks`, `link.copies`, `link.listen`, `link.event`, `link.handoff`, `link.ping`) or fetch `/v1/audio/*` and `/v1/cache/*`. A satellite joining again closes the sockets of its previous session.

A satellite forwards library reads and writes (including `scan.start` on existing hub roots) to the hub, while refusing calls that manage hub folders, transcode caches, or Last.fm credentials with `unsupported` (`on_the_hub`).

A satellite sends `link.event` on its current player presence connection. The params name a `topic` of `playback`, `progress`, `queue`, or `began`. The first three carry the same fields as the corresponding client event. The hub takes the player ID from the authenticated session and overwrites any player ID in the payload. A `began` report carries `appearanceId` and `durationMs` for each track start, including repeat-one. `link.event` is internal to the link. Presence summaries continue to travel in `link.ping`. On connection and after the live buffer overflows, the satellite sends its current playback state, queue revision and current entry's position before continuing with new reports. Missed track starts are lost. A refresh never creates a `began` report.

### Satellites

A satellite (`apps/server/src/uplink.rs`) answers its own status, settings, events, player and queue, link and backups, and forwards every other call to its hub unchanged, on a hub connection of the client session's own, with the client's request IDs. That connection is opened on the session's first forwarded call or subscription, subscribed to the hub's `library`, `players` and `jobs` topics the session holds, and closed with the session; the hub's `library`, `list`, `players` and `jobs` events on it go to that session alone. Unsubscribing from those topics unsubscribes the connection at the hub too. When the hub closes the connection, as it does when it stops, the satellite lets it go; if the session still follows `library`, `players` or `jobs`, a new connection is opened and subscribed when the hub begins to answer again, before the satellite's `servers` event says it is back, or at the next ten-second check if the hub was back before it was missed. A session that subscribes while the hub is away, or whose subscription at the hub fails on its connection, is subscribed the same way. Views held on the old connection end with it, and a call on one is answered `view_expired`. Calls that name the hub's folders are refused with `unsupported` and the reason `on_the_hub`. Covers are fetched from the hub with the link session, over TLS on the route where the hub last proved its key, and kept in the satellite's cover cache.

Each connection resolves the hub's routes again (at most eight; a route may be a name), tries what they give this network first and a tailnet last (see ["Hub and satellites" in `docs/internals/linking.md`](linking.md#hub-and-satellites)), over TLS pinned to the hub's key at each address, and presents the link session only once the handshake has proved it; `server.hello` on that connection then confirms the hub's ID, that it is a hub, and its contract version. A connection gets three seconds to resolve a name, four at each address and a call eight; a lost connection is replaced once at once, but not after a wait ran out. While the hub does not answer, a forwarded call fails with `unavailable`, reason `hub_unreachable`, the hub's name and `sinceMs`, and for two seconds after a failed try without trying again. `server.status` on a satellite reports the hub's library ID as its `library`, with a revision of 0, since a status answers without waiting on the hub (`ritornelctl status` leaves the revision out there), and a `hub` (`HubStatus`) with `answers`, `sinceMs`, `route` and `problem`. A satellite says hello to its hub with `ClientKind::Server`. It tells its own clients each time the hub begins or stops answering, with a `servers` event, on which they read `server.status` again for its `hub`, and pings the hub every ten seconds with `link.ping`, saying whether it plays, so that a hub that went while nothing was asked of it is found out (`uplink::follow`). Whether it plays is read from a flag each call and tick sets as it lets the player go (`Core::playing`), never by waiting on the player: the ping runs on the async runtime, and a call that holds the player may itself be waiting on that runtime for the hub.

A satellite's listens reach the hub with `link.listen`, keyed on the satellite's player endpoint and the listen's ID, so a resend is `duplicate` and a differing one is `conflict` and kept beside the first in `history_ingest_conflict`. The hub takes them from a link session only, for that satellite's own player, of the current `LISTEN_RECORD_VERSION`, and with every number within 2^50.

### Player presence

`players.list` lists the local player and the linked satellites that last announced a player. Each row has an opaque `playerId`, the driving `serverId`, a name, `isLocal`, `reachable` and the Unix timestamp `sinceMs` for that reachability. A reachable row has its playback status, the title while a track is playing, paused or buffering, and the output after it opens. An away row keeps its identity and name. On a satellite the call returns the hub's list with the local row marked. When the hub cannot answer it returns just the local player, if any.

The satellite's presence connection identifies itself in `server.hello` with `presence.hasPlayer`, `presence.sendsNowPlaying` and its initial `presence.player` summary. Subsequent state changes travel in `link.ping.player`. The hub takes identity from the authenticated link. Each change to the visible places or their state sends a `players` event, a hint to read the list again. Every hub in this contract advertises the `players` capability. A linked satellite includes it in its first descriptor, even when the hub is away and only the local fallback is available.

### Routing playback and queue calls

Single-player playback and queue methods accept an optional `player` ID from `players.list`. Omitting it uses the player on the server the client connects to. A satellite sends calls for another player up to the hub. The hub handles its own target locally. For a linked satellite with a player, it removes `player` and sends the call with its original idempotency key on that player's current presence connection. Receipts belong to the player, so a retry there returns the same receipt. Any session with `playback:control` may route these calls.

For a remote `playback.play` or `queue.add` from a song view, the hub resolves the calling session's view in order. It omits gone songs, adjusts the start index and returns `not_found` if the selected song is gone. The context defaults to `query`. A view over 20,000 rows or a rewritten request over the 1 MiB message limit returns `invalid` before anything is sent. The hub keeps the rewritten params for three minutes per session, player and idempotency key, up to 32 rewrites and 2 MiB per session. Retrying within that cache returns the player's receipt even after the view closes. Reusing the key with different params returns `idempotency_mismatch`.

The hub marks a resolved or validated track list with `tracksKnown: true` on its authenticated call to the satellite. Clients cannot supply this assurance.

The hub releases its stores before waiting up to four seconds for the player's answer, within the satellite's eight-second wait for its hub. The satellite handles the request separately from its socket reader so it can read track details back from the hub while answering. It accepts the declared single-player playback and queue methods and rejects a forwarded call containing `player`. A missing player returns `unsupported` with reason `no_player`. An away player or a missed answer returns `unavailable` with reason `player_unreachable`, its `name` and `sinceMs`. A missed answer leaves the outcome unknown. Retry a command with the same idempotency key. A full request queue refuses the call with `unavailable` and reason `player_busy`.

### Moving playback

`playback.move` takes `from` and `to` IDs from `players.list`, the current `entryId` at the source, and an `idempotencyKey`. Either server accepts it from any session with `playback:control`. The hub coordinates it even when both players are satellites. Equal targets, a stopped source, a stale entry or a snapshot too large for a single frame return `invalid`. An overlapping move may return `unavailable`. Retry with the original key.

The hub gives the source three seconds to pause and the destination three seconds to accept the queue. The `done` receipt means the destination has written the queue. Sound can start later. The hub then releases the source in the background, including after a reconnect or hub restart. A refused load leaves the source paused with its queue. If the load answer was lost, the hub checks the destination's receipt before releasing anything.

`link.handoff` carries the internal `take`, `load` and `release` steps on the satellite's verified connection to its hub. Ordinary client calls are forbidden. The hub calls its own player directly. See [Queue and session](playback.md#queue-and-session) for the snapshot and listen recording.

### Transport-neutral dispatcher

`Core::dispatch` takes a decoded `Frame` and a `Session` and returns an `Option<Frame>` without knowing anything about sockets. Both the WebSocket transport (`listener.rs`) and the in-memory test transport (`apps/server/src/memory.rs`) run through `Core::receive`, enforcing `MAX_MESSAGE_BYTES` (1 MiB) and full JSON text encoding/decoding (`Frame::encode` / `Frame::decode`). Production clients always connect over HTTP/WebSocket; the in-memory channel is used only by tests.

## Session and frames

Frames follow JSON-RPC 2.0 (`crates/ritornel-contract/src/frame.rs`), one text message per frame:

- **Requests, responses, notifications:** Calls are requests (`id`, `method`, optional `params`) receiving exactly one response (`id`, `result` or `error`). Server-pushed events are notifications (`method: "event"`, `params`, no `id`). JSON-RPC batches (arrays) are refused, and binary WebSocket messages close the connection.
- **Strict request IDs:** `RequestId` accepts only a JSON string or whole integer (`i64`). `null` and fractional/exponent numbers (`1.0`, `1.5`, `1e2`) are rejected as `-32600` invalid requests (matching MCP conventions). Unknown top-level members are ignored.
- **Frame size cap:** Messages larger than `MAX_MESSAGE_BYTES` (1 MiB) are rejected before decoding and close the connection.
- **Handshake:** The first call on every session must be `server.hello` with `{ name, kind, version, contractVersion }` (`name` and `version` 1–`MAX_CLIENT_TEXT` = 100 chars). Any other call before a successful handshake fails with `invalid` (`details.reason = "hello_required"`). The server rejects any other `CONTRACT_VERSION` with `unsupported`. The number changes whenever the wire does: `contract/contract.version` records it with a digest of `contract.ts` (comments left out), a test fails when that file is not this build's, and `scripts/check-contract.py` (`mise run contract-version`) compares it with main's where the work leaves main (its merge base). A wire that differs from that must be one version above it, so two builds with one number speak one contract, and a change made in a stack of pull requests that lands together is one raise. On success, `server.hello` returns `Descriptor` (`PublicDescriptor` plus `permissions`, optional `tokenRole`, and on a satellite optional `hubPermissions`).
- **Scoped permissions (`crates/ritornel-contract/src/permissions.rs`):** Every method declares `NEEDS: &'static [Permission]` and `LINK_ONLY: bool` (`needs` and `linkOnly` in `METHODS`). The eight `area:action` permissions are `library:read`, `library:write`, `playback:control`, `status:read`, `folders:manage`, `server:manage`, `services:connect`, and `access:manage`, bundled into five named `TokenRole` presets (`owner`, `personal`, `player`, `guest`, `monitor`). Dispatch enforces `session.has_all(row.needs)` (and `session.peer().is_some()` when `row.link_only` is true) before reading params. Sessions lacking `folders:manage` receive `server.status`, `server.settings`, `server.backup`, `sources.list`, `jobs.list`, `jobs.get`, and `jobs` events with host filesystem paths and error strings that may carry paths (`logPath`, `backup.place`, `backup.last.path`, `backup.failed.message`, `backupDir.value`, `Source.path`, `Job.error`, `Job.label`, and `JobDetail.failedItems`) omitted, in every reply that carries a job. `jobs.cancel` needs, beyond `library:write`, what starting that kind of job needs, so a session without `server:manage` cannot cancel a transcode. See `METHODS` in `methods.rs` for the authoritative permission requirements per method.

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
5. **Invalidation and replacement:** When `library.db` commits, `Fanout` (`apps/server/src/dispatch/lists.rs`) emits `list` events for affected views. Play-count or rating-only writes (`LibraryChange::Stats` or `LibraryChange::Ratings`) notify only views that display plays (`songs`) with `reorder: false` so visible windows re-read in place. Portrait imports (`artists.importPictures`, and the hub's own re-import) change `LibraryChange::Metadata` alone and notify only `artists` views, with `reorder: false`. All other library writes set `reorder: true`; clients replace the view via `list.open` with `replaces: { view, anchor }`, which atomically closes the old view and returns `anchorIndex` in the new view.
6. **Lifetime:** Views belong to their session and end on `list.close`, replacement, session close, idle timeout (`VIEW_IDLE` = 10 minutes), or LRU eviction past `MAX_VIEWS` (16 per session). Calling an expired view fails with `view_expired`.

Three read paths do not use views:

- **`library.search`** — Stateless single-call dropdown search returning top matching `artists`, `albums`, and `songs` (with album `artId`) plus `top` kind (default 5 per kind, max `MAX_SEARCH_RESULTS` = 20).
- **`queue.window`** — Direct revision-stamped reads of the playback queue (`offset`, `limit` up to 500, `order: queue | play | up_next`) returning `queueRevision`, `total`, `currentIndex`, and `entries`.
- **Direct history queries** — `history.recent` (cursor-paged), `history.onThisDay`, `history.artist`, `history.album`, `history.artists`, `history.albums` (paged), `history.track`, and `history.stream` answer listening-history reads directly without opening a view.

## Events and subscriptions

Sessions opt into server-pushed notifications via `events.subscribe` and `events.unsubscribe`. Holding an open list view automatically subscribes the session to `list` events for that view. Unsubscribing drops any queued unsent events for those topics immediately so no event on an unsubscribed topic follows the response.

| Topic | Payload | Coalescing key | Client action |
|---|---|---|---|
| `servers` | Full `ServerInfo` snapshot | `serverId` | Replace server entry (keep higher `revision`). |
| `players` | Change hint | Singleton | Read `players.list` again. |
| `jobs` | Full `Job` summary (`target` included) | `jobId` | Replace job entry (requires `status:read`). |
| `library` | `libraryRevision` + `changed` (`membership`, `metadata`, `stats`, `ratings`) | Singleton | Re-read non-view screens older than `libraryRevision`. |
| `list` | `view`, `libraryRevision`, `reorder` | `view` | Re-read visible windows (`reorder: false`) or replace view (`reorder: true`). |
| `playback` | Full `PlaybackState` snapshot | Player | Replace playback state (keep `revision >=` current). |
| `progress` | `entryId`, `positionMs`, `playing` | Player | Anchor position and extrapolate locally between ~1 s ticks. |
| `queue` | `queueRevision` | Player | Re-read visible queue windows. |
| `devices` | `deviceId`, `deviceRevision`, `changed` (`state`, `settings`, `plan`, `actions`, `plays`), and the `run` whole while one is under way | `deviceId` | Read `devices.get` again, and the visible window of `devices.actions` when `actions` changed. |

For `playback`, `progress` and `queue`, `events.subscribe` and `events.unsubscribe` accept a `player` ID from `players.list`. The reply echoes the requested player and lists the topics held for that player. Without a target the reply lists the local session topics. Remote player subscriptions coexist with local topics. A satellite restores its clients' remote subscriptions when their hub connections return, and drops them when the client disconnects. Delivered player events carry `player` only for another server's player.

Events never carry deltas and are never replayed across disconnects. Each session maintains an ordered outbound queue (`apps/server/src/session.rs`) that never drops responses and holds at most one unsent event per `(topic, key)`. Merging a newer event into an unsent slot preserves its queue position while taking the highest revision, the union of `LibraryChange` values, and `reorder |= new.reorder` so a reorder signal is never masked by a subsequent play-count update. After reconnecting, clients re-handshake, resubscribe, reopen views, and re-read visible state.

## Commands, playback, and jobs

Every mutating call (`MethodKind::Command`) carries a client-generated UUID `idempotencyKey` and returns a `Receipt` (`commandId`, `status: accepted | done`, optional `revision`) flattened into its result object:

- **Idempotency:** Library commands (`library_command` in `library.db`) and server/job/playback commands (`command_receipt` in `server.db` or the player's recent-command buffer) replay the original receipt when retried with the same key and request fingerprint, and fail with `idempotency_mismatch` if the same key is reused with different parameters. Five commands intentionally return a fresh non-stored receipt instead: `server.backup` and `history.export` (each invocation writes a new timestamped folder), `lastfm.signInFinish` (deduplicated by its sign-in token), `record.dismiss` (idempotent by current library revision), and `imports.stage` (idempotent by its run, piece number and streams).
- **Optimistic concurrency:** Queue mutation calls (`queue.add`, `queue.remove`, `queue.move`, `queue.clear`) accept optional `expectedRevision` and return `conflict` with `details.currentRevision` if the queue moved.
- **Retry-safe playback:** There is no play/pause toggle; `playback.set` takes absolute `volume` (0–1), `muted`, `shuffle`, and `repeat`; `playback.seek` and `playback.skip` name the target `entryId` / `fromEntryId` so late retries cannot seek or skip a different track; queue mutations address entries by `entryId`, never by index. `playback.play` and `queue.add` accept `PlaySource` (`tracks`, `release`, or `view`, copying IDs out of the view snapshot into the queue); `queue.add` supports `playIfEmpty`. A server without an audio output omits the `playback` capability and refuses calls for its own player with `unsupported` and reason `no_player`. See [Routing playback and queue calls](#routing-playback-and-queue-calls) for remote targets.
- **Durable jobs:** Long-running work (`scan.start`, `transcode.start`, `transcode.prune`, `lastfm.sync`, `imports.dryRun`, `imports.apply`, `imports.undo`) creates or returns a durable `Job` in `server.db` (`queued`, `running`, `cancelling`, `succeeded`, `failed`, `cancelled`) with counters (`done`, `total`, `bytes`, `failed`, `skipped`), `label`, `error`, and kind-specific summaries (`scan`, `sync`). Starting a scan for a root already being scanned returns the running job. `jobs.list` returns up to 100 recent jobs; `jobs.get` returns a job, up to 100 failed `JobItem` rows and, for a successful materialization, its ordered artifact keys and hashes; `jobs.cancel` requests cooperative cancellation before the next library transaction begins, and once acknowledged the job never ends `succeeded` (ending `cancelled` even if a transaction already in flight committed, or `failed` if an in-flight step errored), so clients read what was applied from the library or import run rather than inferring it from the job's terminal state.

## Devices

The `devices.*` calls serve the Devices tab; the [devices guide](device-sync.md) covers what is behind them. Reads need `library:read` and commands `library:write`; `devices.addFolder` also needs `folders:manage`, since it names a folder on the server called. The wire types are `SyncDevice…`, since `DeviceKind` already names what a listen played on.

| Call | Kind | Serves |
|---|---|---|
| `devices.list` | Query | Every target, for the column of device tabs: kind, family, transport, name, icon, `setup`, `connection`, `activity`, `attention`, what its next sync would copy, its last sync and `deviceRevision`. |
| `devices.get` | Query | One target's page: facts, capabilities, room, settings, sources with what each comes to, the plan of its next sync, and the run under way or the last. A folder's path is left out without `folders:manage`. |
| `devices.formats` | Query | Each format on offer, with what the target's content would take in it and whether it fits. |
| `devices.actions` | Query | The rows of a plan (`planRevision`) or of a run (`runId`), filtered by `kind` and `state`, grouped by album or a row a track, at most 500 a call. `parentId` reads one group's tracks. A revision that is no longer the plan's answers `plan_stale`. |
| `devices.unmanaged` | Query | The files in the music root that Ritornel did not write. |
| `devices.plays` | Query | The plays read from a target. No family reads any yet. |
| `devices.set` | Command | Name, icon, whether it is ignored, the whole list of sources, format, cover size, ID3v1, reserve, limit, and for a folder what it is made for, its kind and the codecs it plays. |
| `devices.addFolder` | Command | A folder on the server called, as a target. Nothing is written until its first sync. |
| `devices.sync` | Command | Starts the `devices.sync` job at the `planRevision` the person saw. `adopt` is required for a target not yet set up, and `acceptCut` when the plan does not fit and the target asks. `jobs.cancel` stops it. |
| `devices.eject` | Command | Makes a removable target safe to unplug. It records nothing, so a resend ejects again. |
| `devices.forget` | Command | Ends a target. Its files are left as they are. |

A plan is a preview and its rows are all `waiting`; a run's rows are its stored actions, so after a run the done, skipped, failed and in-doubt rows are all still there to read until the next run replaces them, when a call for the older run answers `not_found` with `reason: "actions_expired"`. A target that is syncing refuses `devices.sync`, `devices.eject`, `devices.forget` and a change of what goes on it with `conflict` and `reason: "busy"`.

## Media

Cover art and artist portraits are served over plain HTTP at `GET /v1/art/{artId}/{size}`:

- **Fixed buckets and immutability:** `{size}` must be one of `ART_SIZES` (`[64, 96, 128, 192, 256, 384, 512, 768, 1024]`, longest edge in pixels), also advertised in `PublicDescriptor.artSizes`. Responses are `image/jpeg` with `Cache-Control: private, max-age=31536000, immutable`; a changed image gets a new `artId`. Invalid IDs, non-bucket sizes, or missing covers return `404 Not Found`.
- **Masters for satellites:** `GET /v1/audio/{appearanceId}/{sourceItemId}/{revision}` serves a master to a link session alone, streamed in 256 KiB chunks, and only while the file is at the revision `link.copies` offered and its size and modification time are what the scan recorded (`409 Conflict` otherwise). A satellite pulls the whole file into `cache/audio/` in its data directory, stopping if it grows past the offered `sizeBytes`, and plays it from there (`apps/server/src/hub_catalog.rs`); the cache keeps the newest 2 GiB, or what the `cache_size` setting says, and is tidied as the server starts.
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
| `forbidden` | `-32000` | `false` | Call, topic, or table requires a permission or link session the caller does not hold. |
| `unsupported` | `-32601` | `false` | Unknown method, contract version mismatch, unsupported filter version, or missing server capability (such as no audio output). |
| `idempotency_mismatch` | `-32000` | `false` | Same `idempotencyKey` reused with different parameters. |
| `internal` | `-32603` | `false` | Unexpected server failure (logged by server). |

Connection drops are client-side transport errors; the server never emits a `transport` code.

Clients gate optional behavior on `PublicDescriptor.capabilities` rather than version numbers:

- `events.servers` — `servers` event topic.
- `events.library` — `library` event topic.
- `lists` — `list.*` view calls and `list` event topic (supported kinds/sorts/predicates in `lists`).
- `media.art` — `media.token` and `GET /v1/art/{artId}/{size}` (supported sizes in `artSizes`).
- `players` — player discovery, the `players` topic, and routing playback and queue calls on a hub or a linked satellite.
- `playback` — `playback.*` and `queue.*` calls and `playback`, `progress`, `queue` topics (advertised only when the server has a player).
- `devices` — `devices.*` calls and the `devices` topic (advertised by a hub).

## Database inspection

Developer tools inspect `library.db` and `server.db` through six read-only RPCs (`crates/ritornel-contract/src/inspect.rs`, `crates/ritornel-engine/src/inspect.rs`, `apps/server/src/dispatch/inspect.rs`) without exposing arbitrary SQL over the wire:

- **`inspect.db`** (`status:read`, `server:manage`): returns file/WAL sizes, SQLite page stats, applied migrations with SHA-256 checksums, every table and view grouped by durability layer (`evidence`, `decision`, `bookkeeping`, `rebuildable`) with row counts, columns, foreign keys, indexes, triggers, and operational queue depths (`outbox`, `listen_staging`, `history_delivery`, `history_ingest_conflict`). On a satellite, answers locally for the satellite's own `server.db` (`library` is omitted).
- **`inspect.table`** (`server:manage`, `library:read`): returns paged rows (up to `MAX_INSPECT_ROWS` = 200, stopping early with `truncated: true` if a page reaches the 512 KiB response budget) and optional single-column exact equality filtering for explicitly allowlisted tables in `library.db` and `server.db`. Secret-bearing tables in `server.db` (`server_identity`, `session`, `service_session`, `hub_link`) are strictly forbidden, FTS5 virtual/shadow tables are excluded, large binary blobs are summarized as `{ "blobBytes": N }`, and path-bearing or error-bearing columns in `server.db` (`root.abs_path`, `server_setting.value_json` for `backup_dir`/`backup.last`/`backup.failed`, path-carrying `command_receipt.result_json`, `job.params_json`/`current_label`/`error_json`/`checkpoint_json`, `job_item.item_ref`/`detail_json`, and `outbox.last_error`) are redacted to `null` (and refused as filter targets) unless the session holds `folders:manage`. On a satellite, `db: "server"` reads the satellite's `server.db` and `db: "library"` returns `unsupported` (`reason: "on_the_hub"`).
- **`inspect.entity`** (`library:read`): traces a `recording`, `appearance`, `release`, `source_item`, or `audio_asset` through its `source_item`, `source_item_revision`, `audio_asset`, identifier/metadata claims, insert-only `assignment` history, `identity_decision` (both item-level and entity-level merge/unmerge `entityDecisions`), and `identity_exclusion` rows. Absolute host paths (`absPath`) are included only when the session holds `folders:manage` and the root lives on this server. On a satellite, forwarded to the hub.
- **`inspect.play`** (`library:read`): returns a derived `play_event` alongside its raw `history_observation` rows, join/dedupe reasons, `history_delivery` rows, and targeting `observation_correction` rows (both direct `correction_target` and endpoint-window corrections). On a satellite, forwarded to the hub.
- **`inspect.conflicts`** (`status:read`, `library:read`): pages `history_ingest_conflict`, stuck/refused/unknown `history_delivery` rows, and waiting `outbox` items (`lastError` omitted without `folders:manage`). On a satellite, forwarded to the hub (inspect a satellite's own `outbox` via `inspect.table` on `db: "server"`).
- **`library.revisions`** (`status:read`): pages the `library_revision` write log newest first. On a satellite, forwarded to the hub.

## Method summary

The methods registered in `crates/ritornel-contract/src/methods.rs`, by area (see `contract/contract.ts` for exact field types):

| Area | Methods |
|---|---|
| Session & server | `server.hello`, `server.status`, `servers.list`, `players.list`, `events.subscribe`, `events.unsubscribe`, `server.backup`, `record.dismiss`, `server.settings`, `server.setSettings` |
| Transcoding | `transcode.start`, `transcode.prune` |
| Devices | `devices.list`, `devices.get`, `devices.formats`, `devices.actions`, `devices.unmanaged`, `devices.plays`, `devices.set`, `devices.addFolder`, `devices.sync`, `devices.eject`, `devices.forget` |
| Linking | `link.invite`, `link.join`, `link.status`, `link.tracks`, `link.copies`, `link.listen`, `link.event`, `link.ping`, `link.handoff` |
| Library & sources | `library.create`, `sources.list`, `sources.add`, `sources.tracks`, `scan.start`, `library.stats`, `library.setWatching`, `library.settings`, `library.setSettings`, `artists.importPictures`, `artists.pictureFolders` |
| Jobs & identity | `jobs.list`, `jobs.get`, `jobs.cancel`, `identity.dryRun`, `identity.review` |
| Browse & media | `list.open`, `list.window`, `list.sections`, `list.locate`, `list.close`, `library.search`, `album.get`, `artist.get`, `tracks.get`, `media.token` |
| Playback & queue | `playback.get`, `playback.play`, `playback.pause`, `playback.resume`, `playback.stop`, `playback.seek`, `playback.skip`, `playback.jump`, `playback.set`, `playback.move`, `queue.window`, `queue.add`, `queue.remove`, `queue.move`, `queue.clear` |
| History & ratings | `history.recent`, `history.onThisDay`, `history.artist`, `history.album`, `history.artists`, `history.albums`, `history.track`, `history.stream`, `history.rebuild`, `history.zones`, `history.setZones`, `history.export`, `ratings.set` |
| Last.fm | `lastfm.status`, `lastfm.signInStart`, `lastfm.signInFinish`, `lastfm.signOut`, `lastfm.sync`, `lastfm.setScrobbling`, `lastfm.report` |
| Imports | `imports.stage`, `imports.dryRun`, `imports.apply`, `imports.undo`, `imports.get`, `imports.list` |
| Inspection | `inspect.db`, `inspect.table`, `inspect.entity`, `inspect.play`, `inspect.conflicts`, `library.revisions` |

## Decisions

- **JSON-RPC 2.0 over one WebSocket plus plain HTTP for media.** One socket orders calls and pushed events without a proxy; plain HTTP lets cover art use standard caching and `Cache-Control: immutable`.
- **Views freeze keys, not contents.** Holding a `Vec<i64>` per view keeps memory low and avoids holding SQLite read transactions open across client think time, while `list.window` always returns fresh metadata, play counts, and ratings.
- **Events carry no deltas and no replay log.** Coalescing per `(topic, key)` bounds server memory for slow clients and keeps reconnect recovery simple (re-handshake, resubscribe, reopen views).
- **A satellite restores its clients' hub subscriptions itself.** A client that only listens makes no call that would find the hub's connection gone, so the satellite opens and subscribes a new one when the hub answers again, and does so before its `servers` event says the hub is back: a client told the hub is back hears its events from then on. It never redials the moment a connection closes, so a hub that closes each new connection is not dialled in a loop.
- **Idempotency keys on every command, retry-safe playback by design.** Commands carry an `idempotencyKey` and replay stored receipts (except the five commands noted under "Commands, playback, and jobs" whose semantics do not store receipts), so clients can safely retry timed-out commands without duplicating side effects, and late playback commands targeting a previous entry become no-ops rather than skipping ahead twice.

## Later

A few wire types already carry variants reserved for upcoming work:

- `ErrorCode::PlanStale` is defined for future plan-then-commit destructive commands.
- `ClientKind` already includes `browser`, `phone`, and `agent` alongside `desktop` and `cli`.
