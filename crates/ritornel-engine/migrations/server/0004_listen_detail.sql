-- =============================================================================
-- Ritornel server.db, migration 0004: a staged listen made firmer before the
-- first real one is recorded, and two closed value lists widened, to match
-- library.db's migration 0003.
--
-- - `listen_staging` keeps each listen's own place: the queue entry it belongs
--   to and where the track was at its last checkpoint, so that closing a listen
--   a crash left open needs nothing from the session. And the monotonic clock
--   at each checkpoint and at the end, beside the wall clock and the boot: an
--   end clock cannot be backfilled, and played time cannot stand in for it
--   across a pause. A row staged before this migration has no clock readings;
--   its position is the session's for the open listen and the end's for a
--   closed one, and its entry is the session's for the open listen.
-- - `listen_staging.heard_ranges_approximate`: whether neighbouring ranges were
--   joined to keep them under the cap, as a fact of its own rather than read
--   back from how many there are.
-- - This player has at most one open listen, by a unique index.
-- - `listen_staging` checks what it can: played time, positions, the track's
--   length and the counters are not negative, the revision number is
--   positive, and the ranges are JSON. A wall clock's end is not required to
--   follow its start, since that clock can be set back in between.
-- - `context_kind` takes `artist` on `queue_entry` and `listen_staging`, and
--   `end_reason` takes `output_lost` on `listen_staging`.
-- - `listen_staging.boot_id` is the machine's boot: only its comment changes.
--
-- Each table is rebuilt as library.db's migration 0003 rebuilds its own, the
-- runner turning foreign keys off and checking them before the commit.
--
-- This file is never edited once it has shipped.
-- =============================================================================

PRAGMA legacy_alter_table = ON;

-- -----------------------------------------------------------------------------
-- queue_entry
-- -----------------------------------------------------------------------------

ALTER TABLE queue_entry RENAME TO queue_entry_0003;

-- [playback|durable] One entry in this server's queue: a track appearance and why it was queued. It never names a file. playback.md decisions 9, 15, 16, 17, 22.
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

INSERT INTO queue_entry SELECT * FROM queue_entry_0003;
DROP TABLE queue_entry_0003;

-- -----------------------------------------------------------------------------
-- listen_staging
-- -----------------------------------------------------------------------------

ALTER TABLE listen_staging RENAME TO listen_staging_0003;

-- [playback|durable] One listen while it is open and until the hub acknowledges it: written at the first frame heard, checkpointed every 5 seconds, then delivered once. It mirrors observation_listen in library.db, field for field, with public IDs where that table has keys. playback.md decisions 19, 20, 21 and "Recording a listen".
-- The envelope's send stamps (wall clock, monotonic clock, boot ID) are not columns: every send, retries included, takes them afresh.
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

-- A row staged before this migration: the open one is the session's current
-- entry's, at the session's position, which is how it was closed until now.
INSERT INTO listen_staging (
  id, listen_id, queue_entry_pid, listener_pid, actor_session_id, played_on_server_pid,
  appearance_pid, asset_pid, copy_kind, source_item_pid, source_revision_no, started_at_utc_ms,
  zone_name, started_mono_ms, boot_id, start_reason, continues_listen_id, start_position_ms,
  track_duration_ms, ms_played, heard_ranges_json, heard_ranges_approximate, max_position_ms,
  seek_count, pause_count, context_kind, context_ref, shuffle, checkpoint_position_ms,
  checkpoint_at_ms, checkpoint_mono_ms, end_position_ms, end_reason, ended_at_utc_ms,
  ended_mono_ms)
SELECT
  l.id, l.listen_id,
  CASE WHEN l.ended_at_utc_ms IS NULL THEN
    (SELECT q.public_id FROM playback_session s JOIN queue_entry q ON q.id = s.current_entry_id)
  END,
  l.listener_pid, l.actor_session_id, l.played_on_server_pid,
  l.appearance_pid, l.asset_pid, l.copy_kind, l.source_item_pid, l.source_revision_no, l.started_at_utc_ms,
  l.zone_name, l.started_mono_ms, l.boot_id, l.start_reason, l.continues_listen_id, l.start_position_ms,
  l.track_duration_ms, l.ms_played, l.heard_ranges_json, 0, l.max_position_ms,
  l.seek_count, l.pause_count, l.context_kind, l.context_ref, l.shuffle,
  CASE WHEN l.ended_at_utc_ms IS NULL THEN
    max(l.start_position_ms, coalesce((SELECT position_ms FROM playback_session), 0))
  ELSE l.end_position_ms END,
  l.checkpoint_at_ms,
  NULL,
  l.end_position_ms, l.end_reason, l.ended_at_utc_ms, NULL
FROM listen_staging_0003 l;
DROP TABLE listen_staging_0003;

-- This player has one listen open at most.
CREATE UNIQUE INDEX listen_staging_open ON listen_staging (played_on_server_pid)
  WHERE ended_at_utc_ms IS NULL;

PRAGMA legacy_alter_table = OFF;
