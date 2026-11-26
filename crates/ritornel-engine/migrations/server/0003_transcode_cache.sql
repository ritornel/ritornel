-- Machine-local files and the runs using them. See "Transcode cache" in
-- docs/internals/server.md. Once shipped, this migration is immutable.
-- The server verifies its checksum before opening the database.

-- [media|rebuildable] One completely published file. Its content key,
-- layer and codec determine its relative path in the cache.
CREATE TABLE transcode_entry (
  id INTEGER PRIMARY KEY,
  cache_key TEXT NOT NULL UNIQUE CHECK (length(cache_key) = 64 AND cache_key NOT GLOB '*[^0-9a-f]*'),
  layer TEXT NOT NULL CHECK (layer IN ('derivative', 'packaged')),
  codec TEXT NOT NULL CHECK (codec IN ('mp3', 'opus', 'flac')),
  master_scheme INTEGER CHECK (master_scheme IS NULL OR (source_key IS NOT NULL AND master_scheme >= 0)),
  source_key TEXT CHECK (source_key IS NULL OR (length(source_key) = 64 AND source_key NOT GLOB '*[^0-9a-f]*')), -- derivative key or master audio hash used to package the file
  size_bytes INTEGER NOT NULL CHECK (size_bytes > 0),
  file_hash BLOB NOT NULL CHECK (length(file_hash) = 32),
  sampled_hash BLOB NOT NULL CHECK (length(sampled_hash) = 32),
  created_at_ms INTEGER NOT NULL,
  last_used_at_ms INTEGER NOT NULL,
  CHECK ((source_key IS NULL) = (layer = 'derivative'))
) STRICT;
CREATE INDEX transcode_lru ON transcode_entry(layer DESC, last_used_at_ms, id);

-- [media|bookkeeping] A caller holds its files until it releases its pin.
CREATE TABLE transcode_pin (
  id INTEGER PRIMARY KEY,
  entry_id INTEGER NOT NULL REFERENCES transcode_entry(id) ON DELETE CASCADE,
  pin_id BLOB NOT NULL CHECK (length(pin_id) = 16),
  boot_id BLOB NOT NULL CHECK (length(boot_id) = 16),
  owner_kind TEXT NOT NULL CHECK (owner_kind IN ('sync_batch', 'http_transfer', 'ab_session')),
  created_at_ms INTEGER NOT NULL,
  UNIQUE(entry_id, pin_id)
) STRICT;
CREATE INDEX transcode_pin_owner ON transcode_pin(pin_id);

-- [media|rebuildable] The last exact length survives eviction.
CREATE TABLE transcode_size_hint (
  id INTEGER PRIMARY KEY,
  cache_key TEXT NOT NULL UNIQUE CHECK (length(cache_key) = 64 AND cache_key NOT GLOB '*[^0-9a-f]*'),
  size_bytes INTEGER NOT NULL CHECK (size_bytes > 0)
) STRICT;
