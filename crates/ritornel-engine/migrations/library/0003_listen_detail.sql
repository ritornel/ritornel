-- =============================================================================
-- Ritornel library.db, migration 0003: a listen's detail made firmer before the
-- first real one is recorded, two closed value lists widened, and an index for
-- finding a listen by the one it continues.
--
-- - `observation_listen` gains `ended_mono_ms`, the playing server's monotonic
--   clock at the end, which played time cannot stand in for across a pause and
--   which cannot be backfilled; a listen recorded before this migration has
--   none. And `heard_ranges_approximate`, which a listen record has always
--   carried, as a fact of its own: copied from each stored record.
-- - `observation_listen` checks what it can: played time, positions, the
--   track's length and the counters are not negative, the revision number is
--   positive, the ranges are JSON, and a continuation key is 16 bytes. A wall
--   clock's end is not required to follow its start, since that clock can be
--   set back in between.
-- - `context_kind` takes `artist`: a play started from an artist's page
--   (docs/design/ui.md), on `observation_listen` and `play_event`.
-- - `end_reason` takes `output_lost`: the output went away in the middle of a
--   track, on `observation_listen`.
-- - `observation_listen.boot_id` is the machine's boot, which the monotonic
--   clock counts from (history.md decision 20): only its comment changes.
-- - `play_event_recent` orders plays of the same instant by their ID, so a
--   page of recent history can end between two of them.
--
-- SQLite cannot change a CHECK in place, so each table is rebuilt: the old one
-- renamed out of the way, the new one made under its name, every row copied
-- across, the old one dropped, and its indexes and triggers made again. The
-- runner turns foreign keys off for a migration and checks them before the
-- commit (SQLite's procedure for changing a table), and the legacy rename
-- leaves what points at the table by name, the view and the other tables'
-- foreign keys, pointing at the new one. `play_event` is derived, and is
-- carried across rather than derived again so a library keeps its plays
-- without a rebuild.
--
-- This file is never edited once it has shipped.
-- =============================================================================

PRAGMA legacy_alter_table = ON;

-- -----------------------------------------------------------------------------
-- observation_listen
-- -----------------------------------------------------------------------------

ALTER TABLE observation_listen RENAME TO observation_listen_0002;

-- [history|durable] The detail of one listen: every field the player's staged row carries. Immutable. None of it can be backfilled. playback.md "Recording a listen"; history.md "What a listen contains".
CREATE TABLE observation_listen (
  observation_id            INTEGER PRIMARY KEY REFERENCES history_observation(id),
  played_on_server_id       INTEGER NOT NULL REFERENCES server(id),          -- the server whose player made the sound
  actor_session_pid         BLOB CHECK (length(actor_session_pid) = 16),     -- the client session that queued the track
  source_revision_no        INTEGER NOT NULL CHECK (source_revision_no > 0),   -- which payload of the envelope's source item was heard
  queued_appearance_id      INTEGER REFERENCES track_appearance(id),         -- a hint for display only; the recording always comes from the source item
  audio_asset_id            INTEGER REFERENCES audio_asset(id),              -- the asset that played
  copy_kind                 TEXT NOT NULL CHECK (copy_kind IN ('master', 'derived', 'stream_master', 'stream_transcode')),
  started_at_utc_ms         INTEGER NOT NULL,                                -- the playing server's wall clock; its zone is on the envelope
  ended_at_utc_ms           INTEGER NOT NULL,                                -- the same clock, which can have been set back since the start
  started_mono_ms           INTEGER NOT NULL,                                -- the playing server's monotonic clock when the track started
  ended_mono_ms             INTEGER,                                         -- the same clock at the end: for a crash, at the last checkpoint. NULL only for a listen recorded before migration 0003
  boot_id                   BLOB NOT NULL CHECK (length(boot_id) = 16),      -- the boot that clock counts from: the machine's, as its kernel names it, or the playing server's run where the machine's could not be read
  start_reason              TEXT NOT NULL CHECK (start_reason IN ('user_pick', 'auto_advance', 'repeat', 'resume')),
  continues_source_key      BLOB CHECK (length(continues_source_key) = 16),   -- the earlier listen on this endpoint that this one resumes
  start_position_ms         INTEGER NOT NULL CHECK (start_position_ms >= 0),
  end_position_ms           INTEGER NOT NULL CHECK (end_position_ms >= 0),
  max_position_ms           INTEGER NOT NULL CHECK (max_position_ms >= 0),
  track_duration_ms         INTEGER NOT NULL CHECK (track_duration_ms >= 0),
  ms_played                 INTEGER NOT NULL CHECK (ms_played >= 0),         -- audio actually rendered
  heard_ranges_json         TEXT NOT NULL CHECK (json_valid(heard_ranges_json)),   -- merged ranges of the track that were heard, with no cap on how many
  heard_ranges_approximate  INTEGER NOT NULL CHECK (heard_ranges_approximate IN (0, 1)),   -- neighbouring ranges were joined to keep them under the player's cap
  seek_count                INTEGER NOT NULL CHECK (seek_count >= 0),
  pause_count               INTEGER NOT NULL CHECK (pause_count >= 0),
  end_reason                TEXT NOT NULL CHECK (end_reason IN ('finished', 'next', 'previous', 'jump', 'replaced', 'removed', 'stopped', 'error', 'output_lost', 'shutdown', 'crash')),
  context_kind              TEXT NOT NULL CHECK (context_kind IN ('manual', 'album', 'artist', 'playlist', 'query', 'agent')),
  context_ref               BLOB CHECK (length(context_ref) = 16),           -- public ID of the release, playlist, definition or agent session
  shuffle                   INTEGER NOT NULL CHECK (shuffle IN (0, 1)),
  rev                       INTEGER NOT NULL
) STRICT;

-- Every column as it was. A listen record has always said whether its ranges
-- were approximate, and the stored record is the observation's payload.
INSERT INTO observation_listen (
  observation_id, played_on_server_id, actor_session_pid, source_revision_no,
  queued_appearance_id, audio_asset_id, copy_kind, started_at_utc_ms, ended_at_utc_ms,
  started_mono_ms, ended_mono_ms, boot_id, start_reason, continues_source_key,
  start_position_ms, end_position_ms, max_position_ms, track_duration_ms, ms_played,
  heard_ranges_json, heard_ranges_approximate, seek_count, pause_count, end_reason,
  context_kind, context_ref, shuffle, rev)
SELECT
  l.observation_id, l.played_on_server_id, l.actor_session_pid, l.source_revision_no,
  l.queued_appearance_id, l.audio_asset_id, l.copy_kind, l.started_at_utc_ms, l.ended_at_utc_ms,
  l.started_mono_ms, NULL, l.boot_id, l.start_reason, l.continues_source_key,
  l.start_position_ms, l.end_position_ms, l.max_position_ms, l.track_duration_ms, l.ms_played,
  l.heard_ranges_json,
  coalesce((SELECT json_extract(o.payload_json, '$.heardRangesApproximate') = 1
            FROM history_observation o WHERE o.id = l.observation_id), 0),
  l.seek_count, l.pause_count, l.end_reason,
  l.context_kind, l.context_ref, l.shuffle, l.rev
FROM observation_listen_0002 l;
DROP TABLE observation_listen_0002;

-- A detail row belongs to an envelope of its own kind.
CREATE TRIGGER observation_listen_kind
BEFORE INSERT ON observation_listen
WHEN (SELECT kind FROM history_observation WHERE id = NEW.observation_id) IS NOT 'listen'
BEGIN
  SELECT RAISE(ABORT, 'observation_listen holds the detail of a listen and of nothing else');
END;

-- An observation is immutable in all its parts.
CREATE TRIGGER observation_listen_no_update
BEFORE UPDATE ON observation_listen
BEGIN
  SELECT RAISE(ABORT, 'observation_listen is immutable: record a correction instead');
END;

CREATE TRIGGER observation_listen_no_delete
BEFORE DELETE ON observation_listen
BEGIN
  SELECT RAISE(ABORT, 'observation_listen is immutable: nothing is deleted');
END;

-- "Which listen continues this one?": the chain a new listen joins. history.md "From observations to plays" step 2.
CREATE INDEX observation_listen_continues ON observation_listen (continues_source_key)
  WHERE continues_source_key IS NOT NULL;


-- -----------------------------------------------------------------------------
-- play_event
-- -----------------------------------------------------------------------------

ALTER TABLE play_event RENAME TO play_event_0002;

-- [history|derived] One deduplicated listen, bound to a recording once its source item is. The only history table that names a recording. history.md decisions 4, 7, 17, 19, 23 and "Time".
-- Play counts are computed from these rows by the indexes below and are stored nowhere.
CREATE TABLE play_event (
  id                  INTEGER PRIMARY KEY,
  public_id           BLOB NOT NULL UNIQUE CHECK (length(public_id) = 16),   -- 16 bytes computed from the earliest observation's endpoint and source key; not a UUIDv7. A rebuild returns the same ID, and the ID changes if that observation changes
  listener_id         INTEGER NOT NULL REFERENCES listener(id),
  recording_id        INTEGER REFERENCES recording(id),          -- canonical; NULL while the source item is unbound
  appearance_id       INTEGER REFERENCES track_appearance(id),   -- from the assignment, when it names one
  source_item_id      INTEGER REFERENCES source_item(id),        -- so an unbound play can still be shown

  -- Time: nullable, with separate axes.
  event_at_utc_ms     INTEGER,                                   -- the start when it can be computed; NULL for an undated play
  time_anchor         TEXT NOT NULL CHECK (time_anchor IN ('start', 'end', 'none')),   -- what the source reported
  clock_basis         TEXT NOT NULL CHECK (clock_basis IN ('hub_age', 'server_wall', 'device_wall', 'device_offset', 'source_utc', 'none')),   -- which clock the instant rests on
  time_precision_ms   INTEGER,
  zone_name           TEXT,
  zone_basis          TEXT NOT NULL CHECK (zone_basis IN ('captured', 'assumed', 'offset_only', 'none')),
  utc_offset_minutes  INTEGER,
  local_date          INTEGER,                                   -- YYYYMMDD
  month_day           INTEGER,                                   -- MMDD, for "on this day"
  local_hour          INTEGER,
  local_dow           INTEGER,                                   -- 0 is Sunday
  calendar_basis      TEXT NOT NULL CHECK (calendar_basis IN ('zone', 'device_wall', 'offset', 'none')),   -- where the local date came from
  not_before_ms       INTEGER,                                   -- bounds of an event with no time
  not_after_ms        INTEGER,

  evidence_class      TEXT NOT NULL CHECK (evidence_class IN ('observed', 'manual')),
  ms_played           INTEGER,
  track_duration_ms   INTEGER,
  outcome             TEXT NOT NULL CHECK (outcome IN ('listened', 'skipped', 'partial', 'unknown')),
  counts              INTEGER NOT NULL CHECK (counts IN (0, 1)),
  count_basis         TEXT NOT NULL CHECK (count_basis IN ('play_rule', 'source_verdict', 'device_position')),
  play_rule_version   INTEGER NOT NULL,
  grouper_version     INTEGER NOT NULL,
  excluded            INTEGER NOT NULL DEFAULT 0 CHECK (excluded IN (0, 1)),   -- an active 'exclude' targets one of its observations
  context_kind        TEXT CHECK (context_kind IN ('manual', 'album', 'artist', 'playlist', 'query', 'agent')),
  context_ref         BLOB CHECK (length(context_ref) = 16),
  shuffle             INTEGER CHECK (shuffle IN (0, 1)),
  rev                 INTEGER NOT NULL
) STRICT;

INSERT INTO play_event SELECT * FROM play_event_0002;
DROP TABLE play_event_0002;

-- The three indexes history.md "Queries M1 must answer" names. Recent plays
-- are paged by their instant and then their ID, which no two plays share.
CREATE INDEX play_event_recent ON play_event (listener_id, event_at_utc_ms DESC, public_id DESC);
CREATE INDEX play_event_counted ON play_event (listener_id, recording_id, event_at_utc_ms DESC)
  WHERE counts = 1 AND excluded = 0;
CREATE INDEX play_event_on_this_day ON play_event (listener_id, month_day, local_date);

PRAGMA legacy_alter_table = OFF;
