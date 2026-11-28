-- =============================================================================
-- Ritornel library.db, migration 0010: syncing part of the library to a
-- player or a folder.
--
-- Everything a sync must not lose follows the library: each target, what
-- goes on it and in what form, the record of every file the hub wrote there,
-- and each run with its actions. One transaction then covers a baseline and
-- the action that earned it, and one backup holds both. Only a folder's path
-- stays behind, in the server.db of the machine it is on. See
-- docs/internals/device-sync.md.
--
-- This file is never edited once it has shipped: the runner keeps a checksum
-- of every migration it applied and refuses to start if one has changed. A
-- change to the schema is a new numbered file.
-- =============================================================================

-- [sync|durable] One player or folder the library syncs to, from the first time it is seen, with its format policy and settings by value. Forgetting marks it and never deletes it. docs/internals/device-sync.md "Storage".
CREATE TABLE sync_target (
  id                 INTEGER PRIMARY KEY,
  public_id          BLOB NOT NULL UNIQUE CHECK (length(public_id) = 16),
  kind               TEXT NOT NULL CHECK (kind IN ('player', 'folder')),
  family             TEXT NOT NULL,                                -- the driver family: 'folder', later 'rockbox' and 'zune'
  name               TEXT NOT NULL,
  icon_key           TEXT,                                         -- NULL takes the family's drawing
  setup              TEXT NOT NULL DEFAULT 'new' CHECK (setup IN ('new', 'ignored', 'ready', 'forgotten')),
  made_for           TEXT CHECK (made_for IN ('car_stereo', 'phone', 'other_player', 'plain_copy')),   -- a folder: what it is made for, which supplied its codecs, cover size and playlist kind
  folder_kind        TEXT CHECK (folder_kind IN ('removable', 'always_here')),   -- a folder: a drive that comes and goes, or one that is always there
  codecs_json        TEXT NOT NULL DEFAULT '["flac","mp3","opus"]',   -- the codecs it is taken to play, by value
  playlist_kind      TEXT NOT NULL DEFAULT 'm3u8' CHECK (playlist_kind IN ('m3u', 'm3u8')),
  listener_id        INTEGER NOT NULL REFERENCES listener(id),     -- whose plays its plays are
  format_json        TEXT NOT NULL,                                -- encoder settings themselves, never a profile's name, so it means the same after the hub moves
  cover_px           INTEGER NOT NULL DEFAULT 500 CHECK (cover_px >= 0),   -- 0 embeds no cover
  id3v1              INTEGER NOT NULL DEFAULT 0 CHECK (id3v1 IN (0, 1)),
  layout             TEXT NOT NULL DEFAULT 'artist_album_track',   -- one fixed layout so far
  music_root         TEXT NOT NULL DEFAULT 'Music',                -- the folder under the target's root that music is written in
  reserve_bytes      INTEGER CHECK (reserve_bytes IS NULL OR reserve_bytes >= 0),   -- NULL is the larger of 1% and 256 MB
  limit_bytes        INTEGER CHECK (limit_bytes IS NULL OR limit_bytes > 0),        -- the capacity of a folder that is always there; NULL is its volume's
  when_full          TEXT NOT NULL DEFAULT 'ask' CHECK (when_full IN ('ask', 'fill')),
  on_connect         TEXT NOT NULL DEFAULT 'nothing' CHECK (on_connect IN ('nothing', 'sync')),
  read_plays         INTEGER NOT NULL DEFAULT 1 CHECK (read_plays IN (0, 1)),
  scrobble_plays     INTEGER NOT NULL DEFAULT 1 CHECK (scrobble_plays IN (0, 1)),
  remind_after_days  INTEGER NOT NULL DEFAULT 10,
  write_bps          INTEGER,                                      -- the write speed last measured, for the next estimate
  seen_capacity_bytes INTEGER,                                     -- its room as last read, for a plan made while it is away
  seen_free_bytes    INTEGER,
  seen_at_ms         INTEGER,
  settings_rev       INTEGER NOT NULL DEFAULT 0,                   -- counts changes to what a plan stands on: sources, format and settings
  created_at_ms      INTEGER NOT NULL,
  rev                INTEGER NOT NULL,
  CHECK ((kind = 'folder') = (made_for IS NOT NULL AND folder_kind IS NOT NULL))
) STRICT;

-- [sync|durable] How a transport knows a target again. A folder is an opaque root ID whose path is in its host's server.db; a volume is its filesystem's serial, size and partition; an MTP device is its serial. docs/internals/device-sync.md "Storage".
CREATE TABLE sync_target_identity (
  id               INTEGER PRIMARY KEY,
  target_id        INTEGER NOT NULL REFERENCES sync_target(id),
  transport        TEXT NOT NULL CHECK (transport IN ('folder', 'mass_storage', 'mtp')),
  host_server_id   INTEGER REFERENCES server(id),                  -- a folder's server
  root_pid         BLOB CHECK (root_pid IS NULL OR length(root_pid) = 16),   -- a folder's sync_root in that server's server.db
  volume_serial    TEXT,                                           -- mass storage: the filesystem's serial
  volume_bytes     INTEGER,
  partition_id     TEXT,
  device_serial    TEXT,                                           -- MTP: the device's own serial
  rev              INTEGER NOT NULL,
  CHECK ((transport = 'folder') = (root_pid IS NOT NULL))
) STRICT;
CREATE INDEX sync_target_identity_target ON sync_target_identity (target_id);
CREATE UNIQUE INDEX sync_target_identity_root ON sync_target_identity (root_pid) WHERE root_pid IS NOT NULL;

-- [sync|durable] One lifetime of a target's storage. A marker that is gone, or names another root, ends the epoch; adopting again starts a new one, so a reformat is all adds. docs/internals/device-sync.md "The run".
CREATE TABLE sync_target_epoch (
  id             INTEGER PRIMARY KEY,
  target_id      INTEGER NOT NULL REFERENCES sync_target(id),
  epoch          INTEGER NOT NULL,
  marker_pid     BLOB NOT NULL CHECK (length(marker_pid) = 16),    -- the root ID written in the marker
  started_at_ms  INTEGER NOT NULL,
  ended_at_ms    INTEGER,
  rev            INTEGER NOT NULL,
  UNIQUE (target_id, epoch)
) STRICT;

-- [sync|durable] One source of a target's subset, in order: a rule that claims what it selects, an always that claims first, or a leave-out that vetoes. Evaluated afresh for each plan; no membership is stored. docs/internals/device-sync.md "The subset".
CREATE TABLE sync_source (
  id             INTEGER PRIMARY KEY,
  public_id      BLOB NOT NULL UNIQUE CHECK (length(public_id) = 16),
  target_id      INTEGER NOT NULL REFERENCES sync_target(id),
  role           TEXT NOT NULL CHECK (role IN ('rule', 'always', 'leave_out')),
  position       INTEGER NOT NULL,
  selector_json  TEXT NOT NULL,                                    -- what it selects, with its version
  unit           TEXT NOT NULL CHECK (unit IN ('albums', 'tracks')),
  rev            INTEGER NOT NULL,
  UNIQUE (target_id, position)
) STRICT;

-- [sync|durable] The baseline: one row for each lifetime of a file the hub wrote on a target and verified. What names a file there is this record, never a tag. A row is closed when its file is replaced, removed or forgotten, never deleted, so a path's past holders stay known. docs/internals/device-sync.md "The run".
CREATE TABLE sync_object (
  id               INTEGER PRIMARY KEY,
  target_id        INTEGER NOT NULL REFERENCES sync_target(id),
  epoch            INTEGER NOT NULL,
  appearance_id    INTEGER NOT NULL REFERENCES track_appearance(id),
  source_item_id   INTEGER REFERENCES source_item(id),             -- the master it was made from
  revision_no      INTEGER,
  object_path      TEXT,                                           -- folder and mass storage: the path under the target's root
  object_ref       TEXT,                                           -- MTP: the device's persistent object ID
  projection_key   BLOB NOT NULL CHECK (length(projection_key) = 32),   -- the audio, format, tags and picture it was made from
  codec            TEXT NOT NULL,
  how              TEXT NOT NULL CHECK (how IN ('as_is', 'encode')),
  size_bytes       INTEGER NOT NULL CHECK (size_bytes > 0),
  sampled_hash     BLOB NOT NULL CHECK (length(sampled_hash) = 32),    -- BLAKE3 of the first and last MiB
  file_hash        BLOB NOT NULL CHECK (length(file_hash) = 32),       -- BLAKE3 of the whole file
  written_at_ms    INTEGER NOT NULL,                               -- the hub's own clock
  ended_at_ms      INTEGER,
  end_reason       TEXT CHECK (end_reason IN ('replaced', 'removed', 'missing', 'forgotten', 'released')),
  rev              INTEGER NOT NULL,
  CHECK ((object_path IS NULL) <> (object_ref IS NULL)),
  CHECK ((ended_at_ms IS NULL) = (end_reason IS NULL))
) STRICT;
-- A plan reads a target's open baselines, and a path has one open holder.
CREATE INDEX sync_object_open ON sync_object (target_id) WHERE ended_at_ms IS NULL;
CREATE UNIQUE INDEX sync_object_path ON sync_object (target_id, object_path) WHERE ended_at_ms IS NULL AND object_path IS NOT NULL;

-- [sync|derived] A file in a target's music root that Ritornel did not write, from the last inventory: left alone and counted as used. Replaced whole at each inventory.
CREATE TABLE sync_unmanaged (
  id            INTEGER PRIMARY KEY,
  target_id     INTEGER NOT NULL REFERENCES sync_target(id),
  path          TEXT NOT NULL,
  size_bytes    INTEGER NOT NULL,
  modified_ms   INTEGER,
  rev           INTEGER NOT NULL
) STRICT;
CREATE INDEX sync_unmanaged_target ON sync_unmanaged (target_id, path);

-- [sync|durable] One run of a sync, kept. At most one is open for a target. docs/internals/device-sync.md "The run".
CREATE TABLE sync_run (
  id               INTEGER PRIMARY KEY,
  public_id        BLOB NOT NULL UNIQUE CHECK (length(public_id) = 16),
  target_id        INTEGER NOT NULL REFERENCES sync_target(id),
  job_pid          BLOB CHECK (job_pid IS NULL OR length(job_pid) = 16),   -- the job that carries it, in the hub's server.db
  host_server_id   INTEGER REFERENCES server(id),
  epoch            INTEGER NOT NULL,
  plan_revision    TEXT NOT NULL,
  library_rev      INTEGER NOT NULL,                               -- the library revision the plan was frozen at
  step             TEXT NOT NULL DEFAULT 'read' CHECK (step IN ('read', 'plan', 'adopt', 'copy', 'finish')),
  adopted          INTEGER NOT NULL DEFAULT 0 CHECK (adopted IN (0, 1)),   -- whether this run set the target up
  outcome          TEXT CHECK (outcome IN ('complete', 'cut_off', 'failed_in_part', 'failed', 'stopped')),
  error            TEXT,
  actions_total    INTEGER NOT NULL DEFAULT 0,
  actions_done     INTEGER NOT NULL DEFAULT 0,
  actions_failed   INTEGER NOT NULL DEFAULT 0,
  copied           INTEGER NOT NULL DEFAULT 0,
  removed          INTEGER NOT NULL DEFAULT 0,
  bytes_total      INTEGER NOT NULL DEFAULT 0,
  bytes_done       INTEGER NOT NULL DEFAULT 0,
  probe_json       TEXT,                                           -- the probe as the device gave it
  started_at_ms    INTEGER NOT NULL,
  ended_at_ms      INTEGER,
  rev              INTEGER NOT NULL,
  CHECK ((ended_at_ms IS NULL) = (outcome IS NULL))
) STRICT;
CREATE UNIQUE INDEX sync_run_open ON sync_run (target_id) WHERE ended_at_ms IS NULL;
CREATE INDEX sync_run_target ON sync_run (target_id, id);

-- [sync|durable] One frozen action of a run and what became of it, written down before the device is asked to do it: the signature it may destroy and the digest it must leave. Kept until the next run for that target has finished. docs/internals/device-sync.md "The run".
CREATE TABLE sync_action (
  id                 INTEGER PRIMARY KEY,
  run_id             INTEGER NOT NULL REFERENCES sync_run(id),
  seq                INTEGER NOT NULL,                             -- the order actions are carried out in
  kind               TEXT NOT NULL CHECK (kind IN ('add', 'replace', 'remove', 'conflict', 'blocked')),
  reason             TEXT NOT NULL,
  appearance_id      INTEGER REFERENCES track_appearance(id),
  source_pid         BLOB CHECK (source_pid IS NULL OR length(source_pid) = 16),   -- the sync_source that wanted it, which may since have been replaced
  group_key          TEXT NOT NULL,                                -- the album it is listed under
  title              TEXT NOT NULL,                                -- as shown when it was frozen, so a row reads the same after the library moves on
  artist             TEXT NOT NULL,
  album              TEXT NOT NULL,
  album_artist       TEXT NOT NULL,
  art_pid            BLOB CHECK (art_pid IS NULL OR length(art_pid) = 16),
  album_tracks       INTEGER NOT NULL DEFAULT 0,                   -- how many tracks the album has in the library
  object_path        TEXT,
  object_ref         TEXT,
  projection_key     BLOB CHECK (projection_key IS NULL OR length(projection_key) = 32),
  codec              TEXT,
  how                TEXT CHECK (how IS NULL OR how IN ('as_is', 'encode')),
  bytes              INTEGER NOT NULL DEFAULT 0,
  estimated          INTEGER NOT NULL DEFAULT 0 CHECK (estimated IN (0, 1)),
  expect_old_size    INTEGER,                                      -- what it may destroy
  expect_old_sampled BLOB CHECK (expect_old_sampled IS NULL OR length(expect_old_sampled) = 32),
  expect_new_size    INTEGER,                                      -- what it must leave, known once its transfer is prepared
  expect_new_hash    BLOB CHECK (expect_new_hash IS NULL OR length(expect_new_hash) = 32),
  expect_new_sampled BLOB CHECK (expect_new_sampled IS NULL OR length(expect_new_sampled) = 32),
  state              TEXT NOT NULL DEFAULT 'planned' CHECK (state IN ('planned', 'issued', 'done', 'failed', 'conflict', 'skipped')),
  error_code         TEXT,
  error_detail       TEXT,
  rev                INTEGER NOT NULL,
  UNIQUE (run_id, seq)
) STRICT;
CREATE INDEX sync_action_group ON sync_action (run_id, group_key, seq);
