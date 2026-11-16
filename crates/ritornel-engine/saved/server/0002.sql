-- A server.db as migration 0002 left it: a server's identity and the local
-- credential's session, as in 0001.sql, and a root, a finished scan with one
-- file that could not be read, and the scan command's receipt. It is what
-- `sqlite3 server.db .dump` printed for a database at that schema. A test
-- loads it and migrates it forward (docs/design/server.md, "Testing and
-- CI").
--
-- Nothing here is real. The identity and the session are 0001.sql's, and the
-- rest is invented: no path, ID or file name is anyone's.
PRAGMA foreign_keys=OFF;
BEGIN TRANSACTION;
CREATE TABLE schema_migration (
  version        INTEGER PRIMARY KEY,
  name           TEXT NOT NULL,
  checksum       BLOB NOT NULL,
  applied_at_ms  INTEGER NOT NULL
) STRICT;
INSERT INTO schema_migration VALUES(1,'0001_identity',X'a9c16ff49737779be6423a76d68a42a3c76bc81d94fc8dc0ed2afcf94cdb5837',1763000000000);
INSERT INTO schema_migration VALUES(2,'0002_work',X'dd5cc576aed05d719357a659ce74ed214a0bea1481040599138e3bae36407174',1763000000000);
CREATE TABLE server_identity (
  id             INTEGER PRIMARY KEY CHECK (id = 1),
  server_id      BLOB NOT NULL CHECK (length(server_id) = 16),
  signing_key    BLOB NOT NULL CHECK (length(signing_key) = 32),   -- Ed25519 private key; never leaves this file
  public_key     BLOB NOT NULL CHECK (length(public_key) = 32),    -- what clients and peers pin
  name           TEXT NOT NULL,
  role           TEXT NOT NULL DEFAULT 'unassigned' CHECK (role IN ('unassigned', 'hub', 'satellite')),   -- this server's own claim; the library's pointer decides
  library_id     BLOB CHECK (length(library_id) = 16),             -- the library it claims or is linked to
  created_at_ms  INTEGER NOT NULL
) STRICT;
INSERT INTO server_identity VALUES(1,X'019a7afd1e007021a223242526272829',X'000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f',X'03a107bff3ce10be1d70dd18e74bc09967e4d6309ba50d5f1ddc8664125531b8','desk','unassigned',NULL,1763000000000);
CREATE TABLE server_setting (
  id          INTEGER PRIMARY KEY,
  key         TEXT NOT NULL UNIQUE,
  value_json  TEXT NOT NULL
) STRICT;
INSERT INTO server_setting VALUES(1,'local_credential_session_id','1');
CREATE TABLE session (
  id                  INTEGER PRIMARY KEY,
  public_id           BLOB NOT NULL UNIQUE CHECK (length(public_id) = 16),   -- the actor on every command this session sends
  subject_kind        TEXT NOT NULL CHECK (subject_kind IN ('person', 'agent', 'server')),
  label               TEXT NOT NULL,                                         -- display text; never decides access
  listener_pid        BLOB CHECK (length(listener_pid) = 16),                -- public ID of a library listener
  kind                TEXT NOT NULL CHECK (kind IN ('full', 'limited')),     -- 'limited' may read the library and control playback, and may change or delete nothing
  token_hash          BLOB NOT NULL UNIQUE,                                  -- hash of the session token
  media_token_hash    BLOB UNIQUE,                                           -- hash of the media-only token from media.token (contract.md)
  created_at_ms       INTEGER NOT NULL,
  revoked_at_ms       INTEGER                                                -- sessions end by revocation, not by time
) STRICT;
INSERT INTO session VALUES(1,X'019a7afd1e007a4b8c4d4e4f50515253','person','local owner',NULL,'full',X'bdd44d520c6f39dfecdcd2122a900e0c18dc125d688b0f26379d574e8f9c533f',NULL,1763000000000,NULL);
CREATE TABLE root (
  id             INTEGER PRIMARY KEY,
  name           TEXT NOT NULL UNIQUE,   -- matches library_root.name for this server in library.db
  abs_path       TEXT NOT NULL,
  created_at_ms  INTEGER NOT NULL
) STRICT;
INSERT INTO root VALUES(1,'music','/srv/invented/music',1763000060000);
CREATE TABLE command_receipt (
  id                   INTEGER PRIMARY KEY,
  command_id           BLOB NOT NULL UNIQUE CHECK (length(command_id) = 16),
  request_fingerprint  BLOB NOT NULL,
  method               TEXT NOT NULL,
  result_json          TEXT NOT NULL,
  created_at_ms        INTEGER NOT NULL
) STRICT;
INSERT INTO command_receipt VALUES(1,X'019a7afd2c107c0d8e0f101112131415',X'0f0e0d0c0b0a09080706050403020100','scan.start','019a7afd-2c10-7c0d-8e0f-101112131415',1763000062000);
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
INSERT INTO job VALUES(1,X'019a7afd2c107c0d8e0f101112131415','scan','music','source',X'019a7afd2c107a6b8c6d6e6f70717273','{"idempotencyKey":"019a7afd-2c10-7c0d-8e0f-101112131415","sourceId":"019a7afd-2c10-7a6b-8c6d-6e6f70717273"}','succeeded',NULL,NULL,1,'{"files":2}',NULL,2,2,81234,0,0,NULL,NULL,1,1763000062000,1763000062100,1763000063000);
CREATE TABLE job_item (
  id          INTEGER PRIMARY KEY,
  job_id      INTEGER NOT NULL REFERENCES job(id),
  item_ref    TEXT NOT NULL,   -- what the item was, for example a root-relative path
  state       TEXT NOT NULL CHECK (state IN ('succeeded', 'failed', 'skipped')),
  detail_json TEXT
) STRICT;
INSERT INTO job_item VALUES(1,1,'Moss Arcade/Tin Weather/broken.flac','failed','{"kind":"malformed"}');
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
CREATE UNIQUE INDEX job_active_dedupe
  ON job (kind, dedupe_key)
  WHERE dedupe_key IS NOT NULL AND state IN ('queued', 'running', 'cancelling');
COMMIT;
