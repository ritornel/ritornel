-- =============================================================================
-- Ritornel server.db, migration 0003: this server's player. Its queue, its one
-- session, saved so a restart restores it paused, and each listen while it is
-- open and until the hub acknowledges it.
-- The tables are the playback part of docs/design/schema/server.sql, which
-- drafted them for the minimal player. The conventions are those of migration
-- 0001.
--
-- This file is never edited once it has shipped.
-- =============================================================================

-- [playback|durable] One entry in this server's queue: a track appearance and why it was queued. It never names a file. playback.md decisions 9, 15, 16, 17, 22.
CREATE TABLE queue_entry (
  id                   INTEGER PRIMARY KEY,
  public_id            BLOB NOT NULL UNIQUE CHECK (length(public_id) = 16),   -- the contract's entryId
  appearance_pid       BLOB NOT NULL CHECK (length(appearance_pid) = 16),     -- public ID of a library track appearance
  position             INTEGER NOT NULL,                                      -- queue order
  shuffle_position     INTEGER,                                               -- the stored play order while shuffle is on
  context_kind         TEXT NOT NULL CHECK (context_kind IN ('manual', 'album', 'playlist', 'query', 'agent')),
  context_ref          BLOB CHECK (length(context_ref) = 16),                 -- public ID of the release, playlist, definition or agent session
  added_by_session_id  INTEGER REFERENCES session(id),
  failed_reason        TEXT,                                                  -- 'unsupported_format', 'no_copy_reachable', ...
  added_at_ms          INTEGER NOT NULL
) STRICT;

-- [playback|durable] The single row of this server's player, saved so a restart restores it paused. playback.md decisions 1 and 18.
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

-- [playback|durable] One listen while it is open and until the hub acknowledges it: written at the first frame heard, checkpointed every 5 seconds, then delivered once. It mirrors observation_listen in library.db, field for field, with public IDs where that table has keys. playback.md decisions 19, 20, 21 and "Recording a listen".
-- The envelope's send stamps (wall clock, monotonic clock, boot ID) are not columns: every send, retries included, takes them afresh.
CREATE TABLE listen_staging (
  id                    INTEGER PRIMARY KEY,
  listen_id             BLOB NOT NULL UNIQUE CHECK (length(listen_id) = 16),   -- a UUIDv7; becomes the history source key
  listener_pid          BLOB NOT NULL CHECK (length(listener_pid) = 16),       -- the listener set on the player
  actor_session_id      INTEGER REFERENCES session(id),                        -- the client session that queued the track
  played_on_server_pid  BLOB NOT NULL CHECK (length(played_on_server_pid) = 16),   -- this server's ID
  appearance_pid        BLOB NOT NULL CHECK (length(appearance_pid) = 16),     -- what was queued; history keeps it as a hint only
  asset_pid             BLOB NOT NULL CHECK (length(asset_pid) = 16),          -- the asset that played
  copy_kind             TEXT NOT NULL CHECK (copy_kind IN ('master', 'derived', 'stream_master', 'stream_transcode')),
  source_item_pid       BLOB NOT NULL CHECK (length(source_item_pid) = 16),    -- the master behind the copy, written when the track starts
  source_revision_no    INTEGER NOT NULL,
  started_at_utc_ms     INTEGER NOT NULL,
  zone_name             TEXT,                                                  -- this server's IANA zone at the start
  started_mono_ms       INTEGER NOT NULL,                                      -- this server's monotonic clock, one that keeps counting during sleep
  boot_id               BLOB NOT NULL CHECK (length(boot_id) = 16),
  start_reason          TEXT NOT NULL CHECK (start_reason IN ('user_pick', 'auto_advance', 'repeat', 'resume')),
  continues_listen_id   BLOB CHECK (length(continues_listen_id) = 16),         -- the listen this one resumes after a restart
  start_position_ms     INTEGER NOT NULL,
  track_duration_ms     INTEGER NOT NULL,
  ms_played             INTEGER NOT NULL DEFAULT 0,                            -- frames handed to the output, as time
  heard_ranges_json     TEXT NOT NULL DEFAULT '[]',                            -- merged, with no cap on how many
  max_position_ms       INTEGER NOT NULL DEFAULT 0,
  seek_count            INTEGER NOT NULL DEFAULT 0,
  pause_count           INTEGER NOT NULL DEFAULT 0,
  context_kind          TEXT NOT NULL CHECK (context_kind IN ('manual', 'album', 'playlist', 'query', 'agent')),
  context_ref           BLOB CHECK (length(context_ref) = 16),
  shuffle               INTEGER NOT NULL CHECK (shuffle IN (0, 1)),
  end_position_ms       INTEGER,
  end_reason            TEXT CHECK (end_reason IN ('finished', 'next', 'previous', 'jump', 'replaced', 'removed', 'stopped', 'error', 'shutdown', 'crash')),
  ended_at_utc_ms       INTEGER,                                               -- NULL while open; an open row found at start is closed as 'crash'
  checkpoint_at_ms      INTEGER NOT NULL,
  CHECK ((ended_at_utc_ms IS NULL) = (end_reason IS NULL)),
  CHECK ((ended_at_utc_ms IS NULL) = (end_position_ms IS NULL))
) STRICT;
