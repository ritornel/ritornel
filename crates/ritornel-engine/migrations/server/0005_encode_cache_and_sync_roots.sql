-- =============================================================================
-- Ritornel server.db, migration 0005: the transcode cache keeps encodes
-- alone, and a folder a library syncs to has its path here.
--
-- A tagged copy is made for one transfer, held while it is sent and deleted,
-- so the cache's index loses its layer and its packaged rows, and the budget
-- caps what is kept: encodes. See "Transcode cache" in docs/internals/server.md
-- and docs/internals/device-sync.md.
--
-- This file is never edited once it has shipped: the runner keeps a checksum
-- of every migration it applied and refuses to start if one has changed. A
-- change to the schema is a new numbered file.
-- =============================================================================

-- A pin names a run of the server that has ended by the time a migration
-- runs, and the cache drops them at every start.
DROP TABLE transcode_pin;
DROP INDEX transcode_lru;

-- [media|rebuildable] One cached encode, completely published. Its content key and codec determine its relative path in the cache.
CREATE TABLE transcode_encode (
  id INTEGER PRIMARY KEY,
  cache_key TEXT NOT NULL UNIQUE CHECK (length(cache_key) = 64 AND cache_key NOT GLOB '*[^0-9a-f]*'),
  codec TEXT NOT NULL CHECK (codec IN ('mp3', 'opus', 'flac')),
  size_bytes INTEGER NOT NULL CHECK (size_bytes > 0),
  file_hash BLOB NOT NULL CHECK (length(file_hash) = 32),
  sampled_hash BLOB NOT NULL CHECK (length(sampled_hash) = 32),
  created_at_ms INTEGER NOT NULL,
  last_used_at_ms INTEGER NOT NULL
) STRICT;

-- The packaged rows are not carried over: each can be made again, and the
-- cache removes their files when it next opens.
INSERT INTO transcode_encode
  (id, cache_key, codec, size_bytes, file_hash, sampled_hash, created_at_ms, last_used_at_ms)
SELECT id, cache_key, codec, size_bytes, file_hash, sampled_hash, created_at_ms, last_used_at_ms
FROM transcode_entry WHERE layer = 'derivative';

DROP TABLE transcode_entry;
ALTER TABLE transcode_encode RENAME TO transcode_entry;
CREATE INDEX transcode_lru ON transcode_entry(last_used_at_ms, id);

-- [media|bookkeeping] A caller holds an encode until it releases its pin: a batch warming the cache, a transfer being prepared or sent, or a listening session.
CREATE TABLE transcode_pin (
  id INTEGER PRIMARY KEY,
  entry_id INTEGER NOT NULL REFERENCES transcode_entry(id) ON DELETE CASCADE,
  pin_id BLOB NOT NULL CHECK (length(pin_id) = 16),
  boot_id BLOB NOT NULL CHECK (length(boot_id) = 16),
  owner_kind TEXT NOT NULL CHECK (owner_kind IN ('batch', 'transfer', 'http_transfer', 'ab_session')),
  created_at_ms INTEGER NOT NULL,
  UNIQUE(entry_id, pin_id)
) STRICT;
CREATE INDEX transcode_pin_owner ON transcode_pin(pin_id);

-- A transcode still waiting or running for the packaged layer has nothing
-- left to make. A finished one keeps its row and answers with no outcomes.
UPDATE job
SET state = 'cancelled', finished_at_ms = coalesce(finished_at_ms, created_at_ms)
WHERE kind = 'transcode.start'
  AND state IN ('queued', 'running', 'cancelling')
  AND json_extract(params_json, '$.layer') = 'packaged';

-- [files|durable] A folder a library syncs to, and where it is on this machine. The library stores only the opaque root ID, as it stores only a root's name, so a folder's path never leaves the server it is on.
CREATE TABLE sync_root (
  id             INTEGER PRIMARY KEY,
  root_id        BLOB NOT NULL UNIQUE CHECK (length(root_id) = 16),
  abs_path       TEXT NOT NULL,
  created_at_ms  INTEGER NOT NULL
) STRICT;
