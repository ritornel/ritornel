# Schema

Ritornel stores all persistent state in two SQLite databases managed by `crates/ritornel-engine`: `library.db` and `server.db`. The authoritative table, index, trigger, and view definitions live in the generated snapshots [`schema/library.sql`](../../schema/library.sql) and [`schema/server.sql`](../../schema/server.sql), produced directly from the numbered migrations in `crates/ritornel-engine/migrations/` and checked in CI by `schema_snapshot.rs`.

This guide documents the cross-cutting rules behind those schemas: why state is split across two files, which tables are durable versus rebuildable, how immutability and ratings work, and how revisions and migrations are enforced. See [`docs/internals/server.md`](server.md) for SQLite connection pragmas, backups, and write serialization.

## Two-database split

Keeping library state and machine-local server state in separate databases allows a library (`library.db`) to move between machines without carrying host-specific paths, credentials, playback queues, or active jobs.

### `library.db`

Holds everything that belongs to the music library itself:

- **Ownership and bookkeeping:** `schema_migration`, `server`, `library_meta` (singleton row holding `library_id`, `hub_server_id`, `hub_epoch`, and the current `rev`), `library_revision`, `library_command`, `library_setting`.
- **Catalog entities:** `artist`, `artist_credit`, `artist_credit_name`, `recording`, `release_group`, `release`, `track_appearance`, plus `artist_key`, `picture`, `revision_picture`, `picture_folder`, `artist_picture`, and the FTS5 virtual table `search_text`.
- **Sources, evidence, and identity:** `metadata_source`, `library_root`, `source_instance`, `source_item`, `source_item_revision`, `identifier_claim`, `metadata_claim`, `metadata_edit`, `identity_decision`, `assignment`, `identity_exclusion`, `alias_observation`, `audio_asset`, and view `asset_location`.
- **Corrections evidence:** `sidecar`, `sidecar_revision`, `evidence_disc`, `evidence_disc_item`, `lookup_response`, `evidence_claim`, `proposal`, `proposal_item`, `proposal_candidate`, `proposal_evidence`, `answer`, `answer_value`, `answer_redirect`, `answer_join`, `answer_dependency`, `answer_rule`, `answer_rule_item` (see [`docs/internals/corrections.md`](corrections.md)).
- **Device sync:** `sync_target`, `sync_target_identity`, `sync_target_epoch`, `sync_source`, `sync_object`, `sync_unmanaged`, `sync_run`, `sync_action` (see [`docs/internals/device-sync.md`](device-sync.md)).
- **Listening history and taste:** `listener`, `history_endpoint`, `service_account`, `history_observation`, `observation_listen`, `observation_import`, `observation_import_stream`, `observation_correction`, `correction_target`, `correction_recording`, `listener_zone_period`, `listener_rating`, `history_ingest_conflict`, `history_cursor`, `history_delivery`, `import_run`, `import_run_event`, `import_run_stream`, `stream_device`, `play_event`, `play_event_observation`, `heard_artist`, `heard_album`, `play_event_artist`, `play_event_album`, and view `play_effective`. A `history_delivery` row records the server that last took the scrobble to be sent (`sent_by_server_id`, set when it is taken to `sending`); today that is always the hub. A row taken before this was kept has a NULL sender and `attempts > 0`, and was sent by the hub `library_meta` names.

### `server.db`

Holds everything private to one `ritornel-server` installation:

- **Identity and access:** `schema_migration`, `server_identity` (singleton row holding `server_id`, Ed25519 signing/public keypair, display name, role, and `library_id`), `server_setting`, `session` (holds the token's SHA-256 hash, legacy `kind` column retained for backward compatibility, optional `role`, and `permissions_json`; the local owner credential always authenticates as `owner`, while on a hub a satellite's link session records `peer_server_pid`, the `peer_public_key` it proved when it joined, and its granted permissions).
- **Hub link:** `hub_link`, on a satellite only: a singleton row with the hub's ID and pinned public key, its name, the library ID and hub epoch at joining, the routes to try in order, and the link session's token.
- **Local paths and receipts:** `root` (maps logical `library_root.name` values to absolute filesystem paths on this machine), `command_receipt`.
- **Background work:** `job`, `job_item`, `outbox`.
- **Playback and external sessions:** `queue_entry`, `playback_session` (singleton row), `listen_staging` (at most one open listen per server via partial unique index `listen_staging_open`), `service_session` (stores external service session secrets such as Last.fm's `session_key`).

The machine-local transcode cache uses three tables in `server.db`. `transcode_entry` indexes cached encodes, `transcode_pin` records which boot and consumer hold each file, and `transcode_size_hint` retains the last exact byte count after eviction. See [the transcode cache](server.md#transcode-cache).

- **Sync folders:** `sync_root` maps a folder target's opaque root ID to its absolute path on this machine, as `root` maps a library root's name.

## Durable and rebuildable tables

Tables in `library.db` fall into two classes:

| Class | Tables | Rule |
|---|---|---|
| **Durable** | `library_meta`, `server`, `library_revision`, `library_command`, `library_setting`, `metadata_source`, `library_root`, `source_instance`, `source_item` (identity & lifecycle), `source_item_revision`, `sidecar`, `sidecar_revision`, `lookup_response`, `answer`, `answer_value`, `answer_redirect`, `answer_join`, `answer_dependency`, `answer_rule`, `answer_rule_item`, `audio_asset`, `picture`, `revision_picture`, `picture_folder`, `metadata_edit`, `identity_decision`, `assignment`, `identity_exclusion`, `alias_observation`, entity identity & redirect/choice rows in `artist`, `artist_credit`, `artist_credit_name`, `recording`, `release_group`, `release`, `track_appearance`, plus `listener`, `history_endpoint`, `service_account`, `history_observation`, `observation_listen`, `observation_import`, `observation_import_stream`, `observation_correction`, `correction_target`, `correction_recording`, `listener_zone_period`, `listener_rating`, `history_ingest_conflict`, `history_cursor`, `history_delivery`, `import_run`, `import_run_event`, `import_run_stream`, `stream_device` (what a stream played on, which a later reader says again), plus `sync_target`, `sync_target_identity`, `sync_target_epoch`, `sync_source`, `sync_object`, `sync_run`, `sync_action` | Source of truth. Never dropped or recomputed by a rebuild. |
| **Rebuildable** | `identifier_claim`, `metadata_claim`, `evidence_disc`, `evidence_disc_item`, `evidence_claim`, `proposal`, `proposal_item`, `proposal_candidate`, `proposal_evidence`, `artist_key` (derived rows; owner-pinned keys carry `decision_id`), `artist_picture` (rebuilt by re-importing its picture folder), `search_text`, `play_event`, `play_event_observation`, `heard_artist`, `heard_album`, `play_event_artist`, `play_event_album`, `sync_unmanaged` (replaced whole at each sync's inventory), plus cached display/canonical columns on `artist`, `recording`, `release_group`, `release`, `track_appearance`, `source_item.current_recording_id`, and views `asset_location` and `play_effective` | Derived projections that can be recomputed from durable tables (or re-imported from folders for `artist_picture`) without re-reading audio files or external services. |

A few entity tables mix durable owner choices with cached display columns on the same row: `release_group` keeps durable `presentation` and `preferred_release_id` beside derived `display_*` fields, and `release` keeps durable `chosen_release_group_id` beside derived `release_group_id` and `display_*` fields.

## Immutability and append-only tables

SQLite `BEFORE UPDATE` and `BEFORE DELETE` triggers enforce append-only history and identity invariants inside the database itself:

- **`source_item_revision`** (`source_item_revision_no_update`, `source_item_revision_no_delete`) — Raw file/source snapshots are immutable once stored; re-reading a changed item inserts a new `revision_no`.
- **`sidecar_revision`** (`sidecar_revision_no_update`, `sidecar_revision_no_delete`) — The bytes of a rip log, CUE sheet or checksum report as a scan read them are immutable once stored.
- **`lookup_response`** (`lookup_response_no_update`, `lookup_response_no_delete`) — A catalogue's reply is kept as it came and never changed.
- **`answer`** (`answer_no_update`, `answer_no_delete`, `answer_no_replace`) and **`answer_value`** (`answer_value_withdrawn_once`, `answer_value_no_delete`, `answer_value_no_replace`) and **`answer_redirect`**, **`answer_join`**, **`answer_dependency`** and **`answer_rule_item`** (each `_no_update`, `_no_delete`, `_no_replace`), and **`answer_rule`** (`answer_rule_withdrawn_once`, `answer_rule_written_in_force`, `answer_rule_no_delete`, `answer_rule_no_replace`) — The ledger of answers is append-only; a value changes only by being withdrawn, once, by the undo of the answer that wrote it.
- **`assignment`** (`assignment_no_rebind`, `assignment_no_delete`) — Insert-only binding from a `source_item` to a `recording` (and optional `track_appearance`). Only `superseded_at_ms` and `superseded_by_id` may transition from `NULL` once when a new row replaces it. Partial unique index `assignment_current` guarantees at most one active assignment per `source_item_id`, and a composite foreign key `(appearance_id, recording_id)` ensures an assignment's appearance belongs to its recording.
- **`identity_decision`** (`identity_decision_no_update`, `identity_decision_no_delete`) — Immutable audit log of every `bind`, `unbind`, `merge`, and `unmerge` decision, with method, versions, score, evidence JSON, and actor.
- **`history_observation` and its detail tables** (`observation_listen`, `observation_import`, `observation_import_stream`, `observation_correction`, `correction_target`, `correction_recording`) — Every table has `no_update` and `no_delete` triggers, plus `BEFORE INSERT` triggers confirming the parent `history_observation.kind` matches the detail table (`listen`, `import` for both import tables, `correction`) and `correction_recording` matches `verb = 'set_recording'`. History rows are never edited; corrections append a new observation or revoke a prior correction.
- **Import runs** (`import_run`, `import_run_stream`, `import_run_event`) — A run and the streams it held never change; its ledger only grows, and its newest row says whether it is in force ("Spotify streams" in `docs/internals/history.md`).
- **Entity redirects** — `artist`, `recording`, `release`, and `track_appearance` support reversible merges via `redirect_to_id` and cached `canonical_id`, checked by `CHECK ((redirect_to_id IS NULL) = (canonical_id = id))`. Durable references store the row originally named and resolve through `canonical_id` on read, so undoing a merge restores original ownership.

## Ratings and loves

Listener ratings and service-reported loves live in `listener_rating`, enforced as append-only by `listener_rating_no_update` and `listener_rating_no_delete`:

- **Two distinct kinds.** `kind` is `'rating'` or `'love'`. A love is never converted into stars, and stars are never converted into a love.
- **Scale and clearing.** Ratings use integers `0..=100` (`MAX_RATING = 100` in `ritornel-contract`), ten per half star. Clearing a rating or un-loving a track inserts a new row with `cleared = 1` and `value = NULL` (`CHECK ((kind = 'rating' AND cleared = 0) = (value IS NOT NULL))`).
- **Subject and source.** Ratings entered in Ritornel set `recording_id` and `actor_kind` (`endpoint_id` and `source_item_id` are `NULL`). Loves imported from an external service (such as Last.fm) set `endpoint_id` and `source_item_id` (`recording_id` and `actor_kind` are `NULL`) and reach recordings through the source item's current `assignment`.
- **Reading current state (`ritornel-engine/src/ratings.rs`).** Rows are folded in `id` order per canonical recording (`recording.canonical_id`). The newest local `'rating'` row across a canonical recording (including any recordings merged into it) wins; `cleared = 1` clears the stars. A recording is `loved` if at least one `(source, subject)` pair's newest `'love'` row has `cleared = 0`. Because each row stores the originally named `recording_id`, undoing a recording merge restores each recording's own rating history.

## Revision stamping and migrations

- **Every library table carries `rev INTEGER NOT NULL`.** Each write transaction in `LibraryStore` increments `library_meta.rev`, inserts a `library_revision` row, and stamps every inserted or updated row with that revision. `schema_snapshot::every_library_table_carries_the_revision_it_last_changed_at` enforces this in CI with exactly two exemptions: `schema_migration` (maintained by the migration runner outside library revisions) and `search_text` (an FTS5 virtual table and its SQLite shadow tables).
- **Strict typing and public IDs.** Every regular table is declared `STRICT`. External public IDs are 16-byte `BLOB`s (`CHECK (length(...) = 16)`), generated as UUIDv7 except `play_event.public_id`, which is deterministically derived from its earliest observation so rebuilds preserve stable play IDs. Internal joins use `INTEGER PRIMARY KEY` rowids that never leave the server.
- **Immutable checksummed migrations.** Migrations under `crates/ritornel-engine/migrations/{library,server}/` are embedded into the binary and recorded in `schema_migration` with their SHA-256 checksum. Editing a shipped migration causes startup and `schema_snapshot` tests to fail. Schema changes always add a new numbered migration and regenerate `schema/library.sql` and `schema/server.sql` with `mise run schema`.

## Decisions

- **Authoritative schema snapshots in `schema/*.sql`.** Checking the exact `sqlite_schema` output of running all migrations from scratch keeps a readable single-file reference in Git while guaranteeing it never drifts from the migration files.
- **History and external loves bind to `source_item`, not `recording`.** Binding durable external records to `source_item_id` ensures that fixing a file's identity or re-matching scrobbles rebinds history and loves automatically on rebuild without mutating historical rows.
- **Triggers guard the record at the database layer.** Enforcing immutability on `source_item_revision`, `assignment`, `identity_decision`, `history_observation`, and `listener_rating` inside SQLite prevents accidental in-place updates from any code path or future migration.

## Later

The current schema already carries columns and tables reserved for planned capabilities whose engine or RPC surfaces are not yet complete:

- `library_meta.hub_epoch` (alongside the already-enforced `hub_server_id` write guard) prepares for transferring hub ownership across servers.
- `library_root.kind = 'cache'`, `manage_tags`, and `manage_layout` prepare for synced caches and opt-in file management (today every root is `'master'` and read-only).
- `observation_correction`, `correction_target`, and `correction_recording` define the durable correction schema (including `set_recording`), awaiting interactive correction commands and UI.
