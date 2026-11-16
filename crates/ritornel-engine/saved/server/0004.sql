-- A server.db as migration 0004 left it: the database of saved/server/0003.sql
-- (an invented album queued, a listen closed and waiting for the hub, and one
-- left open by a crash) with migration 0004's file run on it and its ledger
-- row, as the runner does. This is what `sqlite3 server.db .dump` printed. A
-- test loads it and migrates it forward (docs/design/server.md, "Testing and
-- CI").
--
-- Nothing here is real. The clock was fixed and the random bytes counted up
-- from 00; the root is an invented path.
PRAGMA foreign_keys=OFF;
BEGIN TRANSACTION;
CREATE TABLE schema_migration (
  version        INTEGER PRIMARY KEY,
  name           TEXT NOT NULL,
  checksum       BLOB NOT NULL,
  applied_at_ms  INTEGER NOT NULL
) STRICT;
INSERT INTO schema_migration VALUES(1,'0001_identity',X'a9c16ff49737779be6423a76d68a42a3c76bc81d94fc8dc0ed2afcf94cdb5837',1794776400000);
INSERT INTO schema_migration VALUES(2,'0002_work',X'dd5cc576aed05d719357a659ce74ed214a0bea1481040599138e3bae36407174',1794776400000);
INSERT INTO schema_migration VALUES(3,'0003_playback',X'e094e2a143da2b95885551e53e98ed0749e975c234846f1035626cee4e5afea0',1794776400000);
INSERT INTO schema_migration VALUES(4,'0004_listen_detail',X'fa9a7a0cdc46d229caa74d94d5891d442015bf209da6ff153991023df2b70655',1794776400000);
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
INSERT INTO server_identity VALUES(1,X'01a1e10280807021a223242526272829',X'000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f',X'03a107bff3ce10be1d70dd18e74bc09967e4d6309ba50d5f1ddc8664125531b8','desk','hub',X'01a1e10280807e7f8081828384858687',1794776400000);
CREATE TABLE server_setting (
  id          INTEGER PRIMARY KEY,
  key         TEXT NOT NULL UNIQUE,
  value_json  TEXT NOT NULL
) STRICT;
INSERT INTO server_setting VALUES(1,'local_credential_session_id','2');
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
INSERT INTO session VALUES(1,X'01a1e10280807a4b8c4d4e4f50515253','person','local owner',NULL,'full',X'bdd44d520c6f39dfecdcd2122a900e0c18dc125d688b0f26379d574e8f9c533f',NULL,1794776400000,1794776400000);
INSERT INTO session VALUES(2,X'01a1e10280807475b67778797a7b7c7d','person','local owner',NULL,'full',X'dc6046356202634846682a2a86901efb725c9ef427e83a7f798019708db75802',NULL,1794776400000,NULL);
CREATE TABLE root (
  id             INTEGER PRIMARY KEY,
  name           TEXT NOT NULL UNIQUE,   -- matches library_root.name for this server in library.db
  abs_path       TEXT NOT NULL,
  created_at_ms  INTEGER NOT NULL
) STRICT;
INSERT INTO root VALUES(1,'music','/invented/music',1794776400000);
CREATE TABLE command_receipt (
  id                   INTEGER PRIMARY KEY,
  command_id           BLOB NOT NULL UNIQUE CHECK (length(command_id) = 16),
  request_fingerprint  BLOB NOT NULL,
  method               TEXT NOT NULL,
  result_json          TEXT NOT NULL,
  created_at_ms        INTEGER NOT NULL
) STRICT;
INSERT INTO command_receipt VALUES(1,X'01010101010101010101010101010101',X'44f10b63897f4383b47fc38bb0733859c82b9a44100ba3f10eb76e5d0365a88f','playback.play','1',1794776400000);
INSERT INTO command_receipt VALUES(2,X'02020202020202020202020202020202',X'452c7f44d7a06ae9ec4edea470e4330b5e46267364e95529b9d92fee39bdf5b5','playback.skip','1',1794776500000);
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
CREATE TABLE job_item (
  id          INTEGER PRIMARY KEY,
  job_id      INTEGER NOT NULL REFERENCES job(id),
  item_ref    TEXT NOT NULL,   -- what the item was, for example a root-relative path
  state       TEXT NOT NULL CHECK (state IN ('succeeded', 'failed', 'skipped')),
  detail_json TEXT
) STRICT;
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
INSERT INTO outbox VALUES(1,'history.ingest',X'01a1e102846876e7a8e9eaebecedeeef',0,1794776500000,NULL,1794776500000);
CREATE TABLE playback_session (
  id                INTEGER PRIMARY KEY CHECK (id = 1),
  queue_revision    INTEGER NOT NULL DEFAULT 0,                -- bumped by every queue edit, in the same transaction
  state_revision    INTEGER NOT NULL DEFAULT 0,                -- stamps the playback state sent to clients
  current_entry_id  INTEGER REFERENCES queue_entry(id),
  position_ms       INTEGER NOT NULL DEFAULT 0,                -- saved with each listen checkpoint
  resume_listen_pid BLOB CHECK (length(resume_listen_pid) = 16),   -- the listen a resume of this track would continue; set when a listen ends by crash or shutdown mid-track, kept after that listen is delivered, cleared when the track finishes or the entry changes
  volume            REAL NOT NULL DEFAULT 1.0 CHECK (volume BETWEEN 0 AND 1),   -- the player's own gain
  muted             INTEGER NOT NULL DEFAULT 0 CHECK (muted IN (0, 1)),
  shuffle           INTEGER NOT NULL DEFAULT 0 CHECK (shuffle IN (0, 1)),
  repeat            TEXT NOT NULL DEFAULT 'off' CHECK (repeat IN ('off', 'all', 'one')),
  listener_pid      BLOB CHECK (length(listener_pid) = 16),    -- the listener set on the player
  updated_at_ms     INTEGER NOT NULL
) STRICT;
INSERT INTO playback_session VALUES(1,1,4,2,10000,NULL,1.0,0,0,'off',X'01a1e102808078898a8b8c8d8e8f9091',1794776500000);
CREATE TABLE queue_entry (
  id                   INTEGER PRIMARY KEY,
  public_id            BLOB NOT NULL UNIQUE CHECK (length(public_id) = 16),   -- the contract's entryId
  appearance_pid       BLOB NOT NULL CHECK (length(appearance_pid) = 16),     -- public ID of a library track appearance
  position             INTEGER NOT NULL,                                      -- queue order
  shuffle_position     INTEGER,                                               -- the stored play order while shuffle is on
  context_kind         TEXT NOT NULL CHECK (context_kind IN ('manual', 'album', 'artist', 'playlist', 'query', 'agent')),
  context_ref          BLOB CHECK (length(context_ref) = 16),                 -- public ID of the release, artist, playlist, definition or agent session
  added_by_session_id  INTEGER REFERENCES session(id),
  failed_reason        TEXT,                                                  -- 'unsupported_format', 'no_copy_reachable', 'file_changed', ...
  added_at_ms          INTEGER NOT NULL
) STRICT;
INSERT INTO queue_entry VALUES(1,X'01a1e10280807ebf80c1c2c3c4c5c6c7',X'01a1e102808078d99adbdcdddedfe0e1',0,NULL,'album',X'a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1',2,NULL,1794776400000);
INSERT INTO queue_entry VALUES(2,X'01a1e102808078c98acbcccdcecfd0d1',X'01a1e10280807415961718191a1b1c1d',1,NULL,'album',X'a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1',2,NULL,1794776400000);
INSERT INTO queue_entry VALUES(3,X'01a1e102808072d394d5d6d7d8d9dadb',X'01a1e102808070519253545556575859',2,NULL,'album',X'a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1',2,NULL,1794776400000);
INSERT INTO queue_entry VALUES(4,X'01a1e10280807cdd9edfe0e1e2e3e4e5',X'01a1e10280807c8d8e8f909192939495',3,NULL,'album',X'a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1',2,NULL,1794776400000);
CREATE TABLE listen_staging (
  id                        INTEGER PRIMARY KEY,
  listen_id                 BLOB NOT NULL UNIQUE CHECK (length(listen_id) = 16),   -- a UUIDv7; becomes the history source key
  queue_entry_pid           BLOB CHECK (length(queue_entry_pid) = 16),     -- the queue entry it was heard from, by public ID, so the entry can go while the listen waits. NULL only for a listen staged before migration 0004 that was closed by then
  listener_pid              BLOB NOT NULL CHECK (length(listener_pid) = 16),       -- the listener set on the player
  actor_session_id          INTEGER REFERENCES session(id),                        -- the client session that queued the track
  played_on_server_pid      BLOB NOT NULL CHECK (length(played_on_server_pid) = 16),   -- this server's ID
  appearance_pid            BLOB NOT NULL CHECK (length(appearance_pid) = 16),     -- what was queued; history keeps it as a hint only
  asset_pid                 BLOB NOT NULL CHECK (length(asset_pid) = 16),          -- the asset that played
  copy_kind                 TEXT NOT NULL CHECK (copy_kind IN ('master', 'derived', 'stream_master', 'stream_transcode')),
  source_item_pid           BLOB NOT NULL CHECK (length(source_item_pid) = 16),    -- the master behind the copy, written when the track starts
  source_revision_no        INTEGER NOT NULL CHECK (source_revision_no > 0),
  started_at_utc_ms         INTEGER NOT NULL,
  zone_name                 TEXT,                                                  -- this server's IANA zone at the start
  started_mono_ms           INTEGER NOT NULL,                                      -- this server's monotonic clock, one that keeps counting during sleep
  boot_id                   BLOB NOT NULL CHECK (length(boot_id) = 16),            -- the boot that clock counts from: the machine's, or this server's run where the machine's could not be read
  start_reason              TEXT NOT NULL CHECK (start_reason IN ('user_pick', 'auto_advance', 'repeat', 'resume')),
  continues_listen_id       BLOB CHECK (length(continues_listen_id) = 16),         -- the listen this one resumes after a restart
  start_position_ms         INTEGER NOT NULL CHECK (start_position_ms >= 0),
  track_duration_ms         INTEGER NOT NULL CHECK (track_duration_ms >= 0),
  ms_played                 INTEGER NOT NULL DEFAULT 0 CHECK (ms_played >= 0),     -- frames handed to the output, as time
  heard_ranges_json         TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(heard_ranges_json)),   -- merged, with no cap on how many
  heard_ranges_approximate  INTEGER NOT NULL DEFAULT 0 CHECK (heard_ranges_approximate IN (0, 1)),   -- neighbours were joined to keep them under the cap; once set, never cleared
  max_position_ms           INTEGER NOT NULL DEFAULT 0 CHECK (max_position_ms >= 0),
  seek_count                INTEGER NOT NULL DEFAULT 0 CHECK (seek_count >= 0),
  pause_count               INTEGER NOT NULL DEFAULT 0 CHECK (pause_count >= 0),
  context_kind              TEXT NOT NULL CHECK (context_kind IN ('manual', 'album', 'artist', 'playlist', 'query', 'agent')),
  context_ref               BLOB CHECK (length(context_ref) = 16),
  shuffle                   INTEGER NOT NULL CHECK (shuffle IN (0, 1)),
  checkpoint_position_ms    INTEGER NOT NULL CHECK (checkpoint_position_ms >= 0),   -- where the track was at the last checkpoint: where a listen a crash left open ended
  checkpoint_at_ms          INTEGER NOT NULL,                                      -- the wall clock at the last checkpoint
  checkpoint_mono_ms        INTEGER,                                               -- the monotonic clock then. NULL only for a listen staged before migration 0004
  end_position_ms           INTEGER CHECK (end_position_ms >= 0),
  end_reason                TEXT CHECK (end_reason IN ('finished', 'next', 'previous', 'jump', 'replaced', 'removed', 'stopped', 'error', 'output_lost', 'shutdown', 'crash')),
  ended_at_utc_ms           INTEGER,                                               -- NULL while open; an open row found at start is closed as 'crash'
  ended_mono_ms             INTEGER,                                               -- the monotonic clock at the end; for a crash, the last checkpoint's
  CHECK ((ended_at_utc_ms IS NULL) = (end_reason IS NULL)),
  CHECK ((ended_at_utc_ms IS NULL) = (end_position_ms IS NULL)),
  CHECK (ended_at_utc_ms IS NOT NULL OR ended_mono_ms IS NULL)
) STRICT;
INSERT INTO listen_staging VALUES(1,X'01a1e102846876e7a8e9eaebecedeeef',NULL,X'01a1e102808078898a8b8c8d8e8f9091',2,X'01a1e10280807021a223242526272829',X'01a1e102808078d99adbdcdddedfe0e1',X'01a1e102808072e3a4e5e6e7e8e9eaeb','master',X'01a1e102808076f7b8f9fafbfcfdfeff',1,1794776400000,'America/New_York',1000000,X'babababababababababababababababa','user_pick',NULL,0,240000,100000,'[[0,100000]]',0,100000,0,0,'album',X'a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1',0,100000,1794776500000,NULL,100000,'next',1794776500000,NULL);
INSERT INTO listen_staging VALUES(2,X'01a1e1040b0870f1b2f3f4f5f6f7f8f9',X'01a1e102808078c98acbcccdcecfd0d1',X'01a1e102808078898a8b8c8d8e8f9091',2,X'01a1e10280807021a223242526272829',X'01a1e10280807415961718191a1b1c1d',X'01a1e10280807e1fa021222324252627','master',X'01a1e10280807233b435363738393a3b',1,1794776500000,'America/New_York',1100000,X'babababababababababababababababa','user_pick',NULL,0,300000,10000,'[[0,10000]]',0,10000,0,0,'album',X'a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1',0,10000,1794776510000,NULL,NULL,NULL,NULL,NULL);
CREATE UNIQUE INDEX job_active_dedupe
  ON job (kind, dedupe_key)
  WHERE dedupe_key IS NOT NULL AND state IN ('queued', 'running', 'cancelling');
CREATE UNIQUE INDEX listen_staging_open ON listen_staging (played_on_server_pid)
  WHERE ended_at_utc_ms IS NULL;
COMMIT;
