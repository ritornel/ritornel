# History

Ritornel separates raw listening evidence from derived plays so play-counting rules, time-zone assignments, and identity fixes can always be recomputed without losing or mutating what happened. Pure derivation and play-rule logic live in `crates/ritornel-domain/src/history.rs`; SQLite ingest, derivation, queries, scrobble delivery, timekeeping, and export live in `crates/ritornel-engine/src/{history_store,history_pages,history_stream,history_facts,import_store,delivery_store,export,timekeeping}.rs`; server Last.fm workers and RPC handlers live in `apps/server/src/{lastfm,dispatch/history,dispatch/lastfm,dispatch/export}.rs`; CLI commands live in `apps/ritornelctl/src/{history,lastfm,export}.rs`.

See [`docs/internals/schema.md`](schema.md) for the immutable history tables, [`docs/internals/playback.md`](playback.md) for listen staging and outbox delivery, [`docs/internals/identity.md`](identity.md) for scrobble matching, and [`docs/internals/plugins.md`](plugins.md) for the `ritornel-lastfm` plugin boundary.

## Observations and derivation

All listening history enters `library.db` through one ingest seam (`Observations::{Listen, Imports}`, write method `history.ingest` in `history_store.rs`). Each record is stored verbatim in `history_observation` (`payload_json`) plus its kind-specific detail table (`observation_listen`, `observation_import`, or `observation_correction`), keyed uniquely by `(endpoint_id, source_key)`. Re-ingesting the same key with an identical payload is a no-op; a conflicting payload under an existing key is recorded in `history_ingest_conflict` without altering the original observation.

Pure derivation (`ritornel_domain::history`, `GROUPER_VERSION = 2`) turns durable observations, active corrections, `listener_zone_period`s, and current `source_item` bindings into `play_event` and `play_event_observation` rows:

1. **Bind:** Each listen chain or import resolves its recording and optional track appearance through its `source_item`'s current `assignment` (following `recording.canonical_id`), unless overridden by an active `set_recording` correction. Unbound items still become `play_event` rows with `recording_id = NULL`.
2. **Chain:** Listens on the same endpoint linked by `continues` join into one chain when the later listen starts within `CHAIN_WINDOW_MS` (24 hours) of the earlier listen's end. The chain takes its start time from the first (`primary`) listen and sums `ms_played` across all listens (`corroborating` for subsequent parts).
3. **Echo matching:** An imported scrobble that matches a recorded delivery from this library (`history_delivery` in `sending`, `accepted`, or `unknown` on the same endpoint, matching exact whole-second `sent_timestamp_s` and normalized artist + title via `same_track` / `words_key`) joins that listen's play as a `corroborating` observation instead of creating a duplicate play.
4. **Judge (`PlayRule`):** Listen chains are judged by `PlayRule::CURRENT` (`PlayRule::V1`, version `1`): a track counts when `track_duration_ms > 30_000` and (`ms_played >= track_duration_ms / 2` or `ms_played >= 240_000`). The outcome (`Outcome` in `ritornel-domain`, `PlayOutcome` in the contract) is `listened` (counted), `skipped` (did not count and `end_reason.moved_on()`), `partial` (did not count otherwise), or `unknown`. Imported scrobbles count by `source_verdict`.
5. **Exclude:** Any play containing an observation targeted by an active `exclude` correction is marked `excluded = 1`.

Each `play_event.public_id` is a deterministic 16-byte hash derived from its primary observation's endpoint and `source_key`, so play IDs are deterministic for a given grouping, though a grouper or chaining change that alters the primary observation can change a play's ID.

## Time and time zones

Every derived play separates the UTC instant from the local calendar date and records the provenance of each (`clock_basis`, `zone_basis`, `calendar_basis`, `time_precision_ms`, `time_anchor`).

### Clocks and monotonic age dating

Every listen and every delivery attempt records three clocks (`crates/ritornel-engine/src/timekeeping.rs`):

- Wall clock (`started_at_utc_ms`, `sent_at_utc_ms`).
- Sleep-counting monotonic clock (`started_mono_ms`, `ended_mono_ms`, `sent_mono_ms`): `CLOCK_BOOTTIME` on Linux, `CLOCK_MONOTONIC` on macOS, and interrupt time on Windows (all continue counting through system sleep).
- Boot ID (`boot_id`, `sent_boot_id`): the machine's kernel boot ID, falling back to the server run ID where unavailable.

When dating a listen (`date_listen`), if the listen was delivered in the same boot it was heard in (`sent_boot == boot` and `sent_mono_ms >= started_mono_ms`), the hub dates it by age: `received_at_ms - (sent_mono_ms - started_mono_ms)` (`ClockBasis::HubAge`, `HUB_AGE_PRECISION_MS = 1_000` ms), making the result immune to a wrong sender wall clock. Otherwise it uses `started_at_ms`, subtracting `clock_skew_ms` when skew exceeds `SKEW_TOLERANCE_MS` (5,000 ms) (`ClockBasis::ServerWall`, `SERVER_WALL_PRECISION_MS = 5_000` ms). Imported scrobbles use their source UTC timestamp (`ClockBasis::SourceUtc`, `SOURCE_SECOND_PRECISION_MS = 1_000` ms).

### Local dates and zone periods

Local calendar fields (`local_date` as `YYYYMMDD`, `month_day` as `MMDD`, `local_hour`, `local_dow`, `utc_offset_minutes`) are computed using `jiff`'s bundled IANA time-zone database (never the host OS database) so derivation produces identical results on any machine:

- **Listens (`zone_of`):** Prefer a recognized non-UTC IANA zone captured on the listen (`ZoneBasis::Captured`). If the captured zone is missing, unknown, or UTC, look up the listener's most specific covering `listener_zone_period` (`assumed_zone`, `ZoneBasis::Assumed`). If no period matches, fall back to a captured UTC zone (`ZoneBasis::Captured`), or leave local date unset (`ZoneBasis::None`).
- **Imports (`import_time`):** Use `assumed_zone` from `listener_zone_period` (`ZoneBasis::Assumed`), or leave local date unset (`ZoneBasis::None`) if no period covers the timestamp.
- Clients manage periods via `history.zones` and `history.setZones` (which rederives plays in the same write).

## Incremental updates and rebuilds

- **Incremental:** Ingesting a listen derives only that listen's chain in the same transaction; identity resolution updates affected items (`rebind`); delivery state transitions rederive matching echo imports (`rederive_delivery`).
- **Full rebuild:** `history.rebuild` (also run automatically on server startup via `history_rule` in `apps/server/src/server.rs` whenever any play's `play_rule_version` or `grouper_version` differs from the build's current version) drops and rederives all `play_event` and `play_event_observation` rows in a single transaction. `history.rebuild` supports `dryRun: true` to report counts without writing.

## Queries

All history views query `play_event` directly using its partial indexes (`play_event_recent`, `play_event_counted`, `play_event_on_this_day`) and the `play_effective` view—there is no separate rollup table to keep in sync:

| Query / Module | Behavior |
|---|---|
| `history.recent` (`history_store.rs`) | Cursor-paged recent plays (`PlayCursor`), newest first, including unbound plays and per-listen chain details. |
| `history.onThisDay` (`history_store.rs`) | Counted, non-excluded plays matching a `MMDD` `month_day` across all years. |
| `history.artist`, `history.album` (`history_pages.rs`) | Banner summary (`Phrase`), yearly play counts (`YearPlays`), and top 6 artist records (`RECORDS = 6`). Counts only bound, counted, non-excluded plays; uses a 26-hour boundary window (`ZONES_APART_MS`) around the first and last instants so first/last local dates remain accurate across time zones. |
| `history.track` (`history_facts.rs`) | Track and release history facts for the playing track (play counts, time-of-day patterns, and album history). |
| `history.stream` (`history_stream.rs`) | Monthly play totals across the entire history (bound and unbound), layered by the year each artist was first heard, naming the top 3 artists per layer-month and excluding catch-up sync days with more than `SYNC_DAY` (300) plays. |

`ritornelctl history` prints recent plays directly, and exposes `artist`, `album`, `on-this-day`, `rebuild`, `zones`, and `zone {add, clear}`.

## Last.fm

Last.fm integration is compiled behind the server's default-on `lastfm` feature (`crates/ritornel-lastfm`, `apps/server/src/lastfm.rs`, `crates/ritornel-engine/src/{import_store,delivery_store}.rs`). API credentials come from `SCROBL_API_KEY` (`KEY_VARIABLE`) and `SCROBL_API_SECRET` (`SECRET_VARIABLE`), or `lastfm.env` (`CREDENTIALS_FILE`, owner-readable only) in the server data directory; the signed-in account's `session_key` lives only in `server.db`'s `service_session` and is never logged or sent to clients.

- **Import (`lastfm.sync` job):** Runs on the hub on manual request (`lastfm.sync`), on server startup, when a non-CLI/non-agent client connects more than `IMPORT_ON_OPEN_AFTER` (1 hour) after the last import, and every `IMPORT_EVERY` (6 hours). `LastfmSource` reads spans of up to 5,000 scrobbles (200 per page), re-reading the trailing 14 days (`recent_s`) on incremental syncs to catch late arrivals. Plays are stored as `observation_import` rows referencing `scrobble_subject` source items, and loved tracks are reconciled in `listener_rating`.
- **Scrobbling and Now Playing:** Eligible plays (`ELIGIBILITY_VERSION = 1`: counted, dated, trusted clock, `observed`, not excluded, not already present on that account, within `SEND_WITHIN_MS = 13 days`, at or after `scrobble_from_ms`) are tracked durably in `history_delivery`. A delivery moves from `pending` to `sending` in its own committed transaction *before* the HTTP request leaves, then settles to `accepted` (storing service-corrected names), `ignored` (storing the service code), `refused`, `unknown` (unanswered; never resent), or `withdrawn` (no longer eligible before sending). A play is withdrawn with reason `ALREADY_SCROBBLED` when the account already has a scrobble of the same track from another player within 1 second.
- **Batching and backoff:** Sent in batches up to `MAX_BATCH` (50). On Last.fm's daily limit (`DAILY_LIMIT`), affected deliveries defer by 1 hour (`DAILY_LIMIT_RETRY_MS`) and the account pauses for 1 hour (`DAILY_LIMIT_WAIT`). Refused or unanswered batches pause the account for 5 minutes (`REFUSED_WAIT`, `UNANSWERED_WAIT`) so held-back plays retry in order once the pause lifts. A rejected session key sets `service_session.sign_in_again = 1` and halts scrobbling until re-authenticated.
- **Record health:** Unanswered (`unknown`), `withdrawn`, and `refused` deliveries, `sign_in_again`, and failed imports surface in `server.status` (`docs/internals/server.md`, "Record health").

## Export

`history.export` (`crates/ritornel-engine/src/export.rs`, `apps/server/src/dispatch/export.rs`, `ritornelctl export`) writes a portable, self-contained directory (`ritornel-record-<timestamp>`, staged under a dotted name and atomically renamed, with owner-only file permissions) from a single read-only SQLite transaction (`EXPORT_FORMAT = "ritornel-record"`, `EXPORT_VERSION = 2`):

| File | Contents |
|---|---|
| `manifest.json` | Format, version, library ID, export timestamp, rule/grouper versions, endpoints, listeners, sources, and line counts. |
| `observations.jsonl` | Every observation oldest first, keyed by endpoint and `source_key`, with verbatim payloads. |
| `ratings.jsonl` | Every rating and love row, including clears. |
| `deliveries.jsonl` | Every scrobble delivery and its outcome, enabling exact echo matching outside Ritornel. |
| `plays.csv` | Derived plays table with current track display names for immediate spreadsheet/script use. |

## Decisions

- **Immutable observations plus pure versioned derivation.** Keeping raw listens, imports, and corrections immutable while versioning `PlayRule` and `GROUPER_VERSION` means rule changes or bug fixes are safe full rebuilds.
- **Monotonic boot-age dating and bundled IANA TZDB.** Dating listens by monotonic age within a boot neutralizes skewed local wall clocks, and bundling the time-zone database makes local dates reproducible across machines.
- **No rollup tables.** Indexed scans over `play_event` are fast enough for large histories and eliminate cache-invalidation drift between raw plays and displayed counts.
- **Commit `sending` before HTTP scrobble send.** Persisting `history_delivery` before calling Last.fm guarantees exact echo matching on subsequent imports and prevents duplicate resends after network timeouts.
- **Self-contained plain-text export.** Exporting observations, ratings, deliveries, and derived plays as JSON Lines, JSON, and CSV ensures the entire listening record remains readable and re-derivable without Ritornel.

## Later

- `observation_correction`, `correction_target`, and `correction_recording` already define all seven correction verbs (`exclude`, `set_listener`, `split`, `join`, `set_clock_offset`, `set_recording`, `revoke`) in schema and domain; `exclude`, `set_recording`, and `revoke` are wired in derivation, while interactive correction commands and cross-source grouping by time window/rank await future work.
