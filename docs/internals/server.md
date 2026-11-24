# Server

`ritornel-server` is the single process that owns a Ritornel data directory: its SQLite databases, master audio roots, audio output, media session, background jobs, and backups. Every client—the GPUI desktop app and `ritornelctl`—talks to it over the local contract. For OS-specific behavior, see [`docs/internals/platforms.md`](platforms.md); for workspace crate boundaries, see [`ARCHITECTURE.md`](../../ARCHITECTURE.md). Wire protocol, linking, and playback belong to their own guides.

## Process lifecycle

A local client resolves the data directory in order—`--data-dir`, `RITORNEL_DATA_DIR`, then the platform default (`ritornel_contract::local`)—and checks `server.json`:

1. Read `server.json` and request the server's public descriptor. If the server ID in `server.json` matches the descriptor, connect and stop here.
2. Otherwise spawn `ritornel-server serve --data-dir <dir>` (when the client is allowed to launch a server).
3. On startup, the server takes the exclusive OS lock on `server.lock`, loads configuration, initializes daily JSON-line logs under a fresh `boot_id`, opens and migrates `server.db` (and `library.db` on a hub), requeues jobs left by any earlier boot ID, binds its listener, atomically writes `server.json` (temp file then rename), and prints one ready line to standard output. While a step before listening takes more than a moment (the copy before a migration, the migration, or deriving history again under a new play rule), it says which in `preparing.json` (`local::PreparingJson`: its PID and the step), written the same way and removed once `server.json` is written (`preparing::Steps`). A client believes it, as it does `server.json`, only while the lock is held by the PID it names (`ritornel_client::local::preparing`); the desktop shows it in place of "Not connected".

By default the server tries `127.0.0.1:4747` and falls back to an OS-assigned loopback port if that one is taken or refused: Windows refuses a port in a reserved range, such as Hyper-V's or WSL's, with permission denied rather than address in use. An explicit `--listen` address (or `RITORNEL_LISTEN` / `listen` in `config.toml`) binds that exact loopback port or fails startup (`127.0.0.1:0` asks the OS for an ephemeral port). `private:<port>` binds every IPv4 interface on that port and closes, before reading a byte, any connection whose peer is not loopback, private (RFC 1918 or IPv6 unique-local), link-local, or in the tailnet range `100.64.0.0/10` (`Listen::admits`); it logs a warning that traffic is plain HTTP. No other address can be named, so a public address is never bound. `server.json` records literal `127.0.0.1` in either mode, never `localhost`.

`ritornelctl` only finds a running server; if none answers, it exits with an error rather than starting one that would die when the command finishes. `ritornelctl server stop` stops the server on a data directory as the system does at shutdown, and waits up to 15 seconds for it to let the lock go (`local::stop` in `ritornel-client`). It sends SIGTERM through `/bin/kill` on Unix, and a polite `taskkill /PID` from the system's own folder on Windows. So it reaches only a server on this machine, and the system lets only the server's own user, or an administrator, ask. It asks a server of any contract version alike, so an old server can be stopped before it is replaced. A server already gone, or one started in its place, as by an open desktop app, counts as stopped. A server still stopping when the wait ends is a failure, so a script does not go on to find it there. `ritornelctl stop` only stops playback.

### Owned and detached launches

- **Owned (`--owned`):** The parent holds the server's standard-input pipe open; when stdin closes, the server shuts down cleanly so a crashed parent does not leave an orphan holding the lock.
- **Detached (desktop app on Linux and Windows):** The desktop app launches `ritornel-server` detached in its own session with `RITORNEL_STARTED_BY_APP=1` so closing a window never stops playback, while Quit can distinguish a server the app started from one it merely found.
- **Window closed vs. Quit:** Closing the desktop window never stops the server, playback, or jobs. Only Quit stops a server the app started; a server the app found already running (such as the background service or one started by hand) is never stopped by Quit.
- **Bundled server:** The desktop app bundle ships `ritornel-server` beside the desktop binary (`scripts/desktop.sh app` / `scripts/desktop.ps1 app`). A development checkout without a bundled server beside it only attaches to a running server unless started via `scripts/desktop.sh run` (or `scripts/desktop.ps1 run`).

### Background service

`ritornel-server service install [--data-dir <dir>]` registers the binary as a user-session service—a `launchd` user agent on macOS, a `systemd` user unit on Linux, or a Task Scheduler task at sign-in on Windows, which runs a supervisor (`service run`) that restarts the server ("Windows" in docs/internals/platforms.md)—so it runs inside the user's login session where audio output and media keys are reachable, and restarts after an unclean exit. `service uninstall` stops and removes it, and `service status` reports whether it is installed and running. When the background service is active, the desktop app attaches to it and starts no server of its own.

### Single-instance lock

Only one server may run on a data directory at a time, enforced by an exclusive OS lock on `server.lock` held for the lifetime of the process (on Unix via `flock`; on Windows via an exclusive one-byte lock past EOF so the PID stays readable). Because the OS releases the lock on process exit, there is no stale-PID cleanup. A second start fails immediately with exit code `3` and names the PID and endpoint already holding the directory; a client launcher that loses a startup race retries discovery from `server.json`.

Exit codes: `0` clean stop, `1` failure, `2` wrong command line, `3` data directory already in use.

### Shutdown and bounds

On `SIGINT`, `SIGTERM`, a polite Windows stop (`taskkill /PID` via the hidden window in `ritornel-windows`), or stdin EOF under `--owned`, the server stops accepting new requests, pauses playback and persists position, lets in-flight requests and transports drain up to `STOP_GRACE` (5 seconds, spent at most twice; callers that stop an owned server kill it after 15 seconds), gives a satellite's hub the same grace from the start of the stop (`Core::stop_hub_within`: every wait on the hub, those already waiting included and those on a link a join still under way makes, ends by then, and no call is made after it), checkpoints and truncates both WAL files, removes `server.json`, and exits. On macOS, `main` starts the Tokio runtime on worker threads and runs the platform main run loop on the main thread so the media session works; shutdown stops that run loop last.

Runtime bounds (`apps/server/src/bounds.rs`) keep resource usage bounded: `MAX_CONNECTIONS = 128` concurrent HTTP connections, `MAX_SESSIONS = 64` open WebSocket/in-memory sessions, `HEADER_WAIT = 10s`, `STOP_GRACE = 5s`, `TICKET_LIFE = 10s` for single-use socket tickets, and `HUB_CALL_WAIT = 8s`, the longest a satellite waits for its hub to answer a call.

### Server name

A server is named after its computer (`platform::computer_name`), so a hub's satellites and a satellite's hub say which machine each is: the computer name on macOS, `PRETTY_HOSTNAME` in `/etc/machine-info` or else the hostname on Linux, and on Windows the DNS host name in the case it was set (`ritornel_windows::name`), or else `COMPUTERNAME`, which Windows upper-cases and cuts to 15 characters. A server still carrying the first-start name `DEFAULT_NAME` takes its computer's name at start, and so does one still carrying the name an older build took from the computer (`platform::legacy_computer_name`): on Windows, `COMPUTERNAME`, as `STUDIO-PC` for a machine set up as `Studio-PC`. Any other name was chosen, and is kept (`takes_computer_name`); a name chosen to equal the old one exactly cannot be told apart from it. A hub lists a satellite by the name it last said hello under, so a satellite renamed this way is listed by its new name once it connects again. A hub's own row in the library's registry (`server` in `library.db`) follows its name in `server.db`, at start and on a rename while it runs (`library::keep_registry_name`); the two files cannot commit together, so a row left behind by a crash between them, or by a write that failed, is put right at the next start or rename. `RITORNEL_TEST_COMPUTER_NAME` stands in for the computer's name in tests.

A name given in the settings (`--name`, `RITORNEL_NAME`, or `name` in `config.toml`) wins over the computer's name and the stored name at every start (`Server::start_run`), so a headless machine is called what a person calls its speakers rather than its hostname. It is trimmed and must be 1 to `MAX_CLIENT_TEXT` (100) characters with no control characters. Without one, the stored name stands, including the last one a setting gave, and a server still called `DEFAULT_NAME`, or the name an older build took from the computer, takes its computer's name, as above.

### Keeping a hub awake

A sleeping hub is one its satellites cannot reach, but a satellite or a player stays linked for good, so being linked is not reason enough to keep a machine awake. When a hub keeps its machine from idle sleep is a setting of the server's own, kept in `server.db` (`server_setting` row `link.awake`) so it follows the server, and set with `server.setSettings` (`ritornelctl settings awake`; see "Settings" below). It is one of three modes (`Awake`):

- **While active**, the default, for 30 minutes: while a linked server plays, or has made a call within the minutes given. The window is at most a week (`Awake::MAX_MINUTES`), and zero means while one plays only. A stored setting that cannot be read, or is out of range, gives way to the default with a warning in the log.
- **Always**, for as long as the server runs.
- **Never**: the machine sleeps as its own settings say.

`awake::keeps_awake` in `apps/server/src/awake.rs` is the rule. Every call on a link session counts as activity but those that keep the link itself (`awake::is_activity`): `server.hello` and `events.subscribe`, which a satellite makes whenever its connection comes back, and `link.ping`, which it sends every ten seconds to say whether it plays. A word that it plays holds for 30 seconds. The hub asks the system (`platform::stay_awake`) when the rule first holds, and lets go when it no longer does. It checks on each call, once a minute, and when the setting changes. The display may still sleep. Only `serve` asks the system (`Run::keep_awake`): a server a test runs in its own process follows the rule but holds nothing, so tests leave the machine's power settings alone. Where the system refuses, or has no way to ask, the server logs a warning, runs on, and asks again a minute later at the soonest. [`platforms.md`](platforms.md) says how each system is asked, and `link.status` says the setting and whether the machine is kept awake now.

## Data directory

```text
<data-dir>/
  server.lock    exclusive lock held while running; contains the server PID
  server.json    endpoint, PID, server ID, contract version; rewritten at each start
  preparing.json PID and step, while a slow step runs before the server listens
  credential     local session token, readable only by the OS user
  config.toml    optional settings needed before a database opens
  server.db      server-local SQLite database (every server)
  library.db     library SQLite database (hub only)
  logs/          daily JSON-line log files
  cache/         covers/ (extracted on demand) and tmp/ (atomic writes before rename)
  backups/       pre-migration rollback copy, created only when no backup folder is configured
  imports/       runs of streams staged for a dry run, owner-only, removed at every start
```

The data directory must sit on a local filesystem because SQLite WAL shared memory and OS file locks are unreliable on network shares. Master music folders may live on local disks or network mounts; a missing or empty root is treated as offline rather than empty so scans skip it without dropping locations.

`DataDir::prepare` creates private directories (`0700` on Unix, owner-only ACL on Windows) and tightens permissions on `server.db` and `library.db` at startup (protecting restored databases that may arrive world-readable).

Cache files are written in `cache/tmp/`, synced, and renamed into `cache/covers/` before being indexed. Cover art bytes remain in the audio/folder files and are extracted into `cache/covers/` on demand; nothing in `cache/`, `logs/`, `backups/`, `imports/`, `server.json`, `preparing.json`, or `server.lock` needs to be backed up.

### Backups and pre-migration snapshots

Backups are never written inside the data directory, so losing the data directory does not also lose its backups. Configure an external directory via `--backup-dir`, `RITORNEL_BACKUP_DIR`, or `backup_dir` in `config.toml`; the directory must already exist and must resolve outside `<data-dir>`.

Each backup—taken on demand (`ritornelctl backup` / `server.backup`) or automatically before a database migration—writes into a dotted temporary folder inside `<backup-dir>` and renames it to `ritornel-backup-<UTC-timestamp>/` (with a `-before-migration` suffix for pre-migration copies and `-2`, `-3`, … on same-second collisions) only once every file is complete:

```text
<backup-dir>/ritornel-backup-YYYY-MM-DD-HHMMSS/
  server.db     consistent copy of server.db
  library.db    consistent copy of library.db (on a hub)
  config.toml   copy of config.toml, if present
  backup.json   manifest (version 1): timestamp, reason, files
  README.txt    plain-text restore instructions
```

Each database copy runs `VACUUM INTO` on its own read-only SQLite connection (`crates/ritornel-engine/src/backup.rs`), followed by `sync_all()` and `PRAGMA quick_check`. Because it runs beside the writer, playback and recording continue while a backup runs, and the output is a single self-contained file with no WAL sidecar. The two databases are copied sequentially rather than at a single cross-file instant, and `README.txt` records the data directory the backup came from. When the backed-up library has scrobbles waiting to be sent, the backup marks scrobbling to wait for a Last.fm import after restore so plays accepted after the backup are not sent twice. If a migration runs with no `--backup-dir` configured, the server still saves a pre-migration rollback copy inside `<data-dir>/backups/` (explicitly distinguished from a backup in `ritornelctl status`).

## Storage split

Two WAL-mode SQLite files cannot commit atomically together, so state lives in the file whose transaction must commit it: everything that follows a library to another machine lives in `library.db`, and everything describing this specific machine lives in `server.db`.

| State | File | Why |
|---|---|---|
| Catalog, evidence, audio assets, roots, history, ratings, rebuildable projections and FTS search index | `library.db` | Library scope; projections commit in the same transaction as their sources |
| `library_meta` (library ID, hub server ID, epoch, revision counter) | `library.db` | Identifies the library and the single server allowed to write it |
| `server` (registry of known servers) | `library.db` | Referenced by roots, history endpoints, and the hub pointer |
| `library_revision` and `library_command` | `library.db` | Revision log and command idempotency receipts must commit with the library write |
| Library settings (policy/rule versions, metadata source ranks) | `library.db` | Must follow the library if moved to another hub |
| `server_identity` (this server's UUIDv7 and Ed25519 key), local role record, local root paths (`root`) | `server.db` | Machine-local identity and filesystem paths |
| Client `session` rows, `service_session` tokens | `server.db` | Local authentication and external-service credentials |
| Persisted playback queue, outputs, `listen_staging` | `server.db` | Machine-local playback state |
| `command_receipt` (server-local commands), `job`, `outbox`, `server_setting` | `server.db` | Local durable work and delivery queue |

`library.db` records the server ID allowed to write it (`library_meta`). Startup verifies the hub pointer and library ID on a read-only connection before opening `library.db` for writing, so a refused library is left byte-for-byte untouched—neither switching journal mode nor running migrations. Every file location is stored as a named root plus a relative path, while the root's absolute path lives only in that server's `server.db`.

## SQLite

Both stores open SQLite through `crates/ritornel-engine/src/sqlite.rs`:

- **Pinned bundled SQLite.** `ritornel-engine` links the bundled SQLite from its pinned `rusqlite` and asserts `SQLITE_VERSION` (`"3.54.1"`) at startup, refusing to open under any other version (well above the 3.51.3 floor that fixed SQLite's WAL-reset bug).
- **One writer thread per database.** Each database has exactly one read-write connection owned by a dedicated writer thread (`Writer`). Writes run as closures sent to that thread, ordering mutations without application locks. On clean close, after readers close, the writer runs `PRAGMA wal_checkpoint(TRUNCATE)`.
- **Read-only reader pool.** Each database keeps `READERS = 4` `SQLITE_OPEN_READ_ONLY` connections (`Reader`) so concurrent UI queries (list windows, cover lookups) run beside the writer without blocking each other.
- **Connection pragmas.** Every connection sets `foreign_keys = ON`, `busy_timeout = 5000`, and `temp_store = MEMORY`, sets `SQLITE_OPEN_NO_MUTEX` (each connection is used from one thread or behind a `Mutex`), and leaves out `SQLITE_OPEN_URI` so file paths are never interpreted as URIs. File-backed write connections require `journal_mode = WAL` and set `synchronous = FULL` so acknowledged commits survive power loss. Tables are `STRICT`.
- **Batched writes.** Bulk operations such as scans commit in batches (500 files per transaction) so `synchronous = FULL` pays one sync per batch rather than per row.
- **External inspection.** Nothing else opens either database for writing while the server runs; manual inspection uses `sqlite3 -readonly`.

### Migrations

Migrations (`crates/ritornel-engine/src/migrate.rs`) are append-only numbered SQL files applied in order on the writer connection and recorded in `schema_migration` with each file's SHA-256 checksum.

- A migration on `main` is never edited: startup refuses a database if any applied migration's checksum differs, or if the database's schema version is newer than the binary understands. Because shipped migration files are immutable, historical doc-path comments inside them stay frozen as written.
- Before applying pending migrations to a non-empty database, the server takes a pre-migration snapshot (to `--backup-dir` if configured, otherwise `<data-dir>/backups/`).
- Every table in `library.db` carries a `rev` column (verified by test in `schema_snapshot.rs`), except `schema_migration` and the rebuildable FTS5 `search_text` table.

## Commands, receipts, and revisions

Ritornel uses ordinary SQLite tables with idempotent command receipts, an outbox, and a per-transaction revision counter—not an event-sourced log.

- **Library commands (`LibraryStore::write`):** Every state-changing library command carries a client-minted `Command` (`id` + SHA-256 `Fingerprint` of method name and parameters). Inside one `library.db` transaction, the writer checks `library_command`: an exact hit replays the stored result without mutating state; the same ID with a different fingerprint is rejected; a miss advances the library revision counter, stamps the new `rev` on every inserted or modified row, logs a `library_revision` audit row, and writes the `library_command` receipt. Rejected calls write no receipt.
- **Server-local commands:** Mutations to `server.db` (such as queue edits) write their state change and `command_receipt` in one `server.db` transaction. Idempotency never spans two files.
- **Events are post-commit hints:** Change notifications emitted over WebSocket are sent only after commit. A reconnecting client compares its last seen library revision against the server's and refreshes state rather than trusting event delivery.

## Jobs and outbox

- **Outbox (`outbox` in `server.db`):** Side effects run after commit from an outbox keyed by `(kind, dedupe_key)`. When playback finishes a track, one `server.db` transaction saves the listen and queues a `history.ingest` outbox row. The delivery worker ingests it into `library.db` (unique on endpoint + source key, so retries are harmless) and retries with backoff on failure, up to five minutes. On a satellite the same worker delivers to the hub with `link.listen` over the satellite's own hub connection, retrying within thirty seconds while the hub is away, and removes the rows only once the hub has answered; a full pass of 100 makes the next pass due at once. Once a stop's grace for the hub has run out, a pass ends before its next listen and leaves the rest staged and due for the next run; one whose answer was cut short is sent again then, and the hub, keyed on the listen's ID, takes it once.
- **Jobs (`job` in `server.db`):** Long-running work (such as library scans and Last.fm syncs) runs on a dedicated worker thread (`apps/server/src/jobs.rs`) independent of any connected client. Active jobs are deduplicated by `(kind, dedupe_key)`, claimed under the server's `boot_id`, and save progress counters about once per second (`SAVE_EVERY = 1s`).
- **No transactions across I/O:** The worker reads files or talks to external services without holding a database transaction, then commits each batch with a deterministic command ID derived from the job ID and step number.
- **Crash recovery and cancellation:** On startup, any job still claimed under a previous `boot_id` is requeued and resumes from its checkpoint; an already-committed batch is skipped via its command receipt, and a scan reads that step's counters back from the receipt, which its commit wrote, so none are lost when the crash came before the checkpoint was saved (`docs/internals/identity.md`, "Scanning and audio hashing"). Cancellation sets `cancel_requested_at` and is checked between steps and at I/O boundaries; committed steps remain.

## Record health

`crates/ritornel-engine/src/record_health.rs` reports whether listening history ingestion or scrobbling is failing, surfaced through `ritornelctl status` and Settings:

- **Failing deliveries (`failing_deliveries`):** `history.ingest` outbox rows that have failed at least once (earliest timestamp, count, latest error).
- **Failing import jobs (`failing_jobs`):** Failed `lastfm.sync` jobs for an account since its last succeeded job (cleared as soon as a subsequent import succeeds).
- **Refused service session (`session_refused`):** When Last.fm has refused the stored session key, recorded from the first refusal until a new sign-in.
- **Lost scrobbles (`lost_scrobbles`):** `history_delivery` rows in `unknown`, `withdrawn`, or `refused` state after the active dismissal revision whose play was not subsequently corroborated by an imported echo from the same account.
- **Monotonic dismissal (`record.dismiss` / `ritornelctl status --dismiss`):** Stores the dismissed library revision and the last 64 command IDs in `server_setting` (`record.dismissed`). A dismissal never moves backwards and ignores replayed command IDs so late retries cannot hide newer problems.

## Configuration and logging

Settings resolve in order: **CLI flag → `RITORNEL_*` environment variable → `config.toml` → default** (`apps/server/src/config.rs`).

`config.toml` holds only what the server needs as it starts (`#[serde(deny_unknown_fields)]` rejects unknown keys so typos fail loudly):

| Setting | Flag | Env var | Default |
|---|---|---|---|
| Listen address | `--listen` | `RITORNEL_LISTEN` | `127.0.0.1:4747` (fallback: OS port); or `private:<port>` |
| Log level | `--log-level` | `RITORNEL_LOG_LEVEL` | `info` |
| Backup folder | `--backup-dir` | `RITORNEL_BACKUP_DIR` | None (backups disabled until set) |
| Server name | `--name` | `RITORNEL_NAME` | None (the stored name, or the computer's in place of the default) |
| Cache of masters a satellite pulls | `--cache-size` | `RITORNEL_CACHE_SIZE` | `2 GiB` |

The name setting is described under "Server name" above.

### Settings

`server.settings` reports the settings of the server called, each as a `Setting`: its value and `setBy`, what gave it (`SetBy`: `default`, `file` for `config.toml`, `environment`, `flag`, or `call`). It covers the name, the stay-awake policy (`awake`), the cache size (`cacheBytes`), where it is set to listen (`ListenOn`: the network and the port it is set to take, not the one the system picked), the log level and the backup folder (`ServerSettings` in `crates/ritornel-contract/src/settings.rs`, answered in `apps/server/src/dispatch/settings.rs`). `ritornelctl settings` prints it.

- **Where a start-up setting came from is kept.** `config::resolve` records each setting's `Origin` in `Origins`, which `serve` hands the run (`Run::origins`). A setting nothing gave is the default. The name a server already had, or took from its computer, is reported as the default when no setting gives it at this start.
- **Only the stay-awake policy is changed by a call.** `server.setSettings` takes the settings to change, each optional, and refuses one that names none. The change and its receipt are written in one `server.db` transaction (`ServerStore::change_once`), the same command sent again changes nothing, and the server follows the new policy at once. A policy a call stored is reported as `call`, even when it equals the default. The other settings are given as the server starts and hold until it starts again.
- **Each server answers its own.** A satellite answers both calls itself and never forwards them, so they read and change the satellite's settings. Both are kept from a link session, so a satellite can neither read nor change its hub's.

`--data-dir` / `RITORNEL_DATA_DIR` selects the data directory before `config.toml` is read, and `--output` (`default`, `null`, `none`) selects the audio output mode. All other persistent settings live in `library.db` (library-wide) or `server.db` (machine-local).

Logs (`apps/server/src/logs.rs`) write daily-rotated JSON lines under `<data-dir>/logs/`, stamped with `boot_id` and correlation IDs where applicable, without logging secrets. `ritornelctl status` reports server/contract/SQLite/schema versions, role, library revision, job counts, outbox depth and oldest age, free disk space, log path, backup configuration, watch state, and record health.

## Workspace and tooling

- **One-way dependencies:** Enforced by `scripts/check-deps.sh` in `mise run deps` (see [`ARCHITECTURE.md`](../../ARCHITECTURE.md)).
- **Thin binary over library:** `apps/server` exposes a library crate (`ritornel-server`) beneath a minimal `main.rs`, allowing integration and contract tests to run a real listener in-process with injected clocks, entropy, and fake media sessions.
- **Authoritative toolchain pins:** `rust-toolchain.toml` pins the Rust toolchain; `mise.toml` enables `idiomatic_version_file_enable_tools = ["rust"]` so `mise` reads `rust-toolchain.toml` without overriding it, and pins `conda:ffmpeg`, `aqua:EmbarkStudios/cargo-deny` and `aqua:nextest-rs/nextest/cargo-nextest`.

## Testing and CI

- **In-memory engine & pure domain tests:** `ritornel-domain` runs pure unit and property tests with no I/O; `ritornel-engine` opens `ServerStore` and `LibraryStore` in SQLite's shared `memdb` VFS with injected `Clock` and `Entropy`.
- **In-process contract & process tests:** `apps/server/tests/` exercises the real HTTP/WebSocket listener in-process and spawns the real `ritornel-server` binary to test lock contention, crash/kill recovery, and owned shutdown.
- **Tests that do not depend on timing:** a test waits on an event or a condition, with a deadline only as a backstop; the rules are in `AGENTS.md`, and `scripts/check-test-sleeps.sh` (`mise run sleeps`) fails a sleep in test code that has no `// sleep:` comment saying why no event will do. Scratch folders come from `tempfile`, and nextest's `ci` profile ends a test still running after five minutes, so a stuck test fails with its output.
- **Schema & contract snapshots:** `mise run schema` and `mise run contract` regenerate `schema/{library,server}.sql` and `contract/contract.ts`; tests in `mise run test` fail if either snapshot drifts. Saved database dumps under `crates/ritornel-engine/saved/` test forward migrations in CI.
- **CI jobs (`.github/workflows/ci.yml`):** One Linux job runs `fmt`, `deps`, `sleeps`, `docs`, and `licenses`. On Linux, macOS, and Windows, a `workspace` job runs `lint` and `test` (and on macOS and Windows `rustdoc`, since each system documents its own platform code), and a `desktop` job runs `desktop:check` (on Windows through `scripts/desktop.ps1 check`). A `hub` job builds and tests `ritornel-server --no-default-features` with `PKG_CONFIG_LIBDIR=/nonexistent` to check that no audio or desktop system library is linked, and `player-arm64` builds and tests the headless player on arm64 Linux. On every system `mise run test` runs the tests under `cargo-nextest`, each in a process of its own, and the doc tests with `cargo test --doc`; nextest's `ci` profile lists the tests that took over ten seconds. CI builds with line-table debug info, which keeps the line numbers backtraces need and makes debug builds and their caches smaller. The `CI passed` job passes only when every other job passed or was skipped as not needed for the change or the tier, so it is the one check branch protection needs. Runs on `main` are never cancelled, and a newer push to a pull request cancels its older run.
- **Which jobs a run needs:** On a pull request, a `changes` job runs `scripts/ci-changes.sh` on the paths it changes: one that touches only the desktop app skips the main workspace's jobs, one that touches only what the desktop app does not build skips the desktop jobs (the crates it builds are those its lock file lists without a source), and documentation alone skips both. It also sets the tier, the systems the run covers. A pull request whose branch is the base of another open one from this repository, a layer under the top of a stack, has the Linux tier: the checks, the `workspace` and `desktop` jobs on Linux, and `hub`. Every other pull request, the top of a stack among them, and every push to `main` has the full tier, every system and `player-arm64` too. The top of a stack holds every layer under it and is where the stack lands, so each change is tested on every system before it merges. If the open pull requests cannot be listed, the tier is full, and the job's summary says which tier ran and why. When the pull request above a layer closes without merging or moves to another base, `.github/workflows/ci-new-top.yml` runs the layer's latest CI again, which now finds it on top; one that merges into the layer pushes to it, which starts its CI. The `changes` job's log also records the event's action and the commits before and after it, which tell apart the two runs a stacked push can start for one commit.
- **CI caches:** The jobs share their setup through `.github/actions/setup`, which caches Rust builds with `Swatinem/rust-cache`, keyed by job, system, and architecture: only runs on `main` save a cache, and pull requests restore from it. The repository's cache quota is 10 GB and evicts the least recently used cache when full, so the Linux `checks` and `hub` jobs restore the `workspace` job's cache rather than keep their own, `player-arm64` keeps only the downloaded crates, and a `prune-caches` job on `main` deletes each cache a newer one of the same kind replaced. Because runs on `main` are never cancelled, its caches always save.

## Decisions

- **Single server binary owns all storage and I/O.** Keeping `ritornel-engine` and `rusqlite` out of every client enforces the server boundary at compile time and prevents competing writers.
- **`ritornelctl` never opens SQLite directly.** All ordinary commands go through the running server's contract so locking, receipts, revisions, and live events stay consistent.
- **Two SQLite files rather than one.** Separating `library.db` from `server.db` lets a library move cleanly between machines without carrying machine-local keys, paths, or queues, while keeping every atomic transaction inside one file.
- **Ordinary tables with receipts and revision stamps, not event sourcing.** Durable domain facts are already stored as append-only evidence with tested rebuilds; a parallel generic event log would duplicate existing append-only evidence.
- **`synchronous = FULL` on both WAL databases.** Pays one sync per committed batch so acknowledged writes and staged listens survive power loss.
- **Typed settings, each with what gave it.** `server.settings` answers with a struct of named settings rather than a bag of keys, so `contract.ts` and `ritornelctl settings` carry every setting the day it is added, and a person can tell a value they chose from one a start-up setting forces.
- **Backups live outside `<data-dir>` and run beside the writer.** Putting backups on a separate path prevents losing the backup with the data directory, and running `VACUUM INTO` on a separate read-only connection keeps playback uninterrupted.

## Later

- **Hub handoff:** Moving `library.db` to another server using the `library_meta` hub pointer/epoch and `server` registry already in the schema.
- **Scheduled backups and retention pruning:** Extending the `--backup-dir` backup runner to run on a schedule and prune older backup folders.
- **HTTP health reporting:** Surfacing `record_health` state on the existing HTTP health route for unattended hub monitoring.
