-- =============================================================================
-- Ritornel server.db, migration 0002: where this server's named roots are, and
-- durable work: receipts of server-local commands, jobs with their per-item
-- results, and the outbox.
-- The tables are the M1 slice 1 part of docs/design/schema/server.sql. The
-- conventions are those of migration 0001.
--
-- This file is never edited once it has shipped.
-- =============================================================================

-- [files|durable] One named root and where it is on this machine. The library stores only the name. server.md decision 12.
CREATE TABLE root (
  id             INTEGER PRIMARY KEY,
  name           TEXT NOT NULL UNIQUE,   -- matches library_root.name for this server in library.db
  abs_path       TEXT NOT NULL,
  created_at_ms  INTEGER NOT NULL
) STRICT;

-- [work|durable] The receipt of one server-local command, such as a queue edit, committed with its change. Library commands keep theirs in library.db. server.md decision 21, flag 2 and "A command, end to end".
CREATE TABLE command_receipt (
  id                   INTEGER PRIMARY KEY,
  command_id           BLOB NOT NULL UNIQUE CHECK (length(command_id) = 16),
  request_fingerprint  BLOB NOT NULL,
  method               TEXT NOT NULL,
  result_json          TEXT NOT NULL,
  created_at_ms        INTEGER NOT NULL
) STRICT;

-- [work|durable] One piece of work longer than a call, with a claim, a heartbeat and a checkpoint. It stays readable after it ends. server.md decision 23; contract.md decision 17.
CREATE TABLE job (
  id                      INTEGER PRIMARY KEY,
  public_id               BLOB NOT NULL UNIQUE CHECK (length(public_id) = 16),
  kind                    TEXT NOT NULL,                              -- 'scan', 'rebuild_history', ...
  dedupe_key              TEXT,                                       -- for example the root name, so one root has one active scan
  target_kind             TEXT,                                       -- what its events invalidate, for example 'source'
  target_pid              BLOB CHECK (length(target_pid) = 16),
  params_json             TEXT NOT NULL,
  state                   TEXT NOT NULL DEFAULT 'queued' CHECK (state IN ('queued', 'running', 'cancelling', 'succeeded', 'failed', 'cancelled')),
  claimed_boot_id         BLOB CHECK (length(claimed_boot_id) = 16),  -- a claim from another boot is requeued at start
  heartbeat_at_ms         INTEGER,
  step                    INTEGER NOT NULL DEFAULT 0,                 -- each library write's 16-byte command ID is derived from the job's ID and this number
  checkpoint_json         TEXT,
  cancel_requested_at_ms  INTEGER,
  done                    INTEGER NOT NULL DEFAULT 0,
  total                   INTEGER,                                    -- NULL while unknown
  bytes                   INTEGER NOT NULL DEFAULT 0,
  failed_count            INTEGER NOT NULL DEFAULT 0,
  skipped_count           INTEGER NOT NULL DEFAULT 0,
  current_label           TEXT,
  error_json              TEXT,
  created_by_session_id   INTEGER REFERENCES session(id),
  created_at_ms           INTEGER NOT NULL,
  started_at_ms           INTEGER,
  finished_at_ms          INTEGER
) STRICT;

-- One active job per dedupe key.
CREATE UNIQUE INDEX job_active_dedupe
  ON job (kind, dedupe_key)
  WHERE dedupe_key IS NOT NULL AND state IN ('queued', 'running', 'cancelling');

-- [work|durable] The outcome of one item of a job, such as one file a scan failed to read. contract.md decision 17 and "Playback and jobs" (server.md names no table for it).
CREATE TABLE job_item (
  id          INTEGER PRIMARY KEY,
  job_id      INTEGER NOT NULL REFERENCES job(id),
  item_ref    TEXT NOT NULL,   -- what the item was, for example a root-relative path
  state       TEXT NOT NULL CHECK (state IN ('succeeded', 'failed', 'skipped')),
  detail_json TEXT
) STRICT;

-- [work|durable] One side effect waiting to run after the commit that recorded its intent. It names what to send by key and holds only retry state; the data stays in its own table. Deleted when the effect succeeds. server.md decision 25; playback.md decision 21.
CREATE TABLE outbox (
  id                  INTEGER PRIMARY KEY,
  kind                TEXT NOT NULL,   -- 'history.ingest' in M1
  dedupe_key          BLOB NOT NULL,   -- for a listen, listen_staging.listen_id, which is also the history source key
  attempts            INTEGER NOT NULL DEFAULT 0,
  next_attempt_at_ms  INTEGER NOT NULL,
  last_error          TEXT,
  created_at_ms       INTEGER NOT NULL,
  UNIQUE (kind, dedupe_key)
) STRICT;

