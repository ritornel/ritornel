-- =============================================================================
-- Ritornel library.db, migration 0008: Spotify's streams, kept beside the
-- scrobbles and never in place of them.
--
-- A stream is one row of Spotify's extended streaming history. It is stored
-- as an import observation of the library's Spotify endpoint, with its
-- observation_import row holding its worked-out start and its names, and a
-- row of its own here holding what only a stream knows. What it was of is a
-- 'spotify_track' source item of the Spotify source instance, one for each
-- track URI. Neither the endpoint nor the instance holds the account's name.
--
-- An import run is one archive applied: the streams it held, and a ledger
-- of whether it is in force. A stream is in force while any run that held it
-- is. Undoing a run is a row of its ledger, and nothing is deleted.
--
-- This file is never edited once it has shipped: the runner keeps a checksum
-- of every migration it applied and refuses to start if one has changed. A
-- change to the schema is a new numbered file.
-- =============================================================================

CREATE TABLE observation_import_stream (
  observation_id     INTEGER PRIMARY KEY REFERENCES observation_import(observation_id),
  ended_at_utc_ms    INTEGER NOT NULL,                           -- when the stream ended, as the archive says; observation_import.played_at_utc_ms is this less ms_played
  ms_played          INTEGER NOT NULL CHECK (ms_played >= 0),
  reason_start       TEXT,                                       -- as Spotify says it, such as 'clickrow'
  reason_end         TEXT,                                       -- such as 'trackdone'
  shuffle            INTEGER CHECK (shuffle IN (0, 1)),
  skipped            INTEGER CHECK (skipped IN (0, 1)),          -- Spotify's own mark
  offline            INTEGER CHECK (offline IN (0, 1)),
  offline_timestamp  INTEGER,                                    -- as the archive gives it: in seconds in some rows and milliseconds in others
  incognito          INTEGER CHECK (incognito IN (0, 1)),        -- a private session
  rev                INTEGER NOT NULL
) STRICT;

CREATE TRIGGER observation_import_stream_kind
BEFORE INSERT ON observation_import_stream
WHEN (SELECT kind FROM history_observation WHERE id = NEW.observation_id) IS NOT 'import'
BEGIN
  SELECT RAISE(ABORT, 'observation_import_stream holds the detail of an import record and of nothing else');
END;

CREATE TRIGGER observation_import_stream_no_update
BEFORE UPDATE ON observation_import_stream
BEGIN
  SELECT RAISE(ABORT, 'observation_import_stream is immutable: record a correction instead');
END;

CREATE TRIGGER observation_import_stream_no_delete
BEFORE DELETE ON observation_import_stream
BEGIN
  SELECT RAISE(ABORT, 'observation_import_stream is immutable: nothing is deleted');
END;

CREATE TABLE import_run (
  id                    INTEGER PRIMARY KEY,
  public_id             BLOB NOT NULL UNIQUE CHECK (length(public_id) = 16),   -- the run as the client staged it
  endpoint_id           INTEGER NOT NULL REFERENCES history_endpoint(id),
  applied_by_server_id  INTEGER NOT NULL REFERENCES server(id),
  session_pid           BLOB CHECK (length(session_pid) = 16),                -- the session that asked, when one did
  reader                TEXT NOT NULL,                                       -- the reader's name and version, such as 'ritornelctl 0.1.0'
  records_hash          BLOB NOT NULL CHECK (length(records_hash) = 32),     -- SHA-256 of the kept records in key order: the same archive has the same hash
  streams               INTEGER NOT NULL CHECK (streams >= 0),               -- the streams the archive held
  new_streams           INTEGER NOT NULL CHECK (new_streams BETWEEN 0 AND streams),   -- of those, the ones it stored
  created_at_ms         INTEGER NOT NULL,
  rev                   INTEGER NOT NULL
) STRICT;

CREATE TRIGGER import_run_no_update
BEFORE UPDATE ON import_run
BEGIN
  SELECT RAISE(ABORT, 'import_run is immutable: its ledger says whether it is in force');
END;

CREATE TRIGGER import_run_no_delete
BEFORE DELETE ON import_run
BEGIN
  SELECT RAISE(ABORT, 'import_run is immutable: nothing is deleted');
END;

CREATE TABLE import_run_event (
  id       INTEGER PRIMARY KEY,
  run_id   INTEGER NOT NULL REFERENCES import_run(id),
  state    TEXT NOT NULL CHECK (state IN ('applied', 'undone')),   -- the newest row of a run says whether it is in force
  at_ms    INTEGER NOT NULL,
  rev      INTEGER NOT NULL
) STRICT;

CREATE INDEX import_run_event_run ON import_run_event (run_id, id);

CREATE TRIGGER import_run_event_no_update
BEFORE UPDATE ON import_run_event
BEGIN
  SELECT RAISE(ABORT, 'import_run_event is append-only');
END;

CREATE TRIGGER import_run_event_no_delete
BEFORE DELETE ON import_run_event
BEGIN
  SELECT RAISE(ABORT, 'import_run_event is append-only: nothing is deleted');
END;

CREATE TABLE import_run_stream (
  run_id          INTEGER NOT NULL REFERENCES import_run(id),
  observation_id  INTEGER NOT NULL REFERENCES observation_import_stream(observation_id),   -- every stream the run's archive held, new or stored before
  rev             INTEGER NOT NULL,
  PRIMARY KEY (run_id, observation_id)
) STRICT;

CREATE INDEX import_run_stream_observation ON import_run_stream (observation_id, run_id);

CREATE TRIGGER import_run_stream_no_update
BEFORE UPDATE ON import_run_stream
BEGIN
  SELECT RAISE(ABORT, 'import_run_stream is immutable');
END;

CREATE TRIGGER import_run_stream_no_delete
BEFORE DELETE ON import_run_stream
BEGIN
  SELECT RAISE(ABORT, 'import_run_stream is immutable: nothing is deleted');
END;

-- What a stream was played on, as words a reader chose from the platform's
-- text, which is never kept: the device that made the sound, never one
-- that controlled it. They are apart from the stream, since a later reader
-- may say them better: every run says them again for each stream it holds,
-- new or stored before, and the row names that run, whose reader it names.
-- A word is NULL when the text did not say, and 'other' when it named
-- something outside these. Nothing derived reads them. A word added later
-- is this table made again with its checks, and touches no stream.
CREATE TABLE stream_device (
  observation_id  INTEGER PRIMARY KEY REFERENCES observation_import_stream(observation_id),
  platform        TEXT CHECK (platform IN ('ios', 'android', 'mac', 'windows', 'linux', 'other')),   -- the operating system only
  device_kind     TEXT CHECK (device_kind IN ('computer', 'phone', 'tablet', 'watch', 'speaker', 'tv', 'car', 'console', 'group', 'other')),   -- the hardware the sound came out of
  via             TEXT CHECK (via IN ('app', 'web', 'cast', 'connect', 'airplay', 'other')),   -- how that device played it
  run_id          INTEGER NOT NULL REFERENCES import_run(id),   -- the run whose reader said them
  rev             INTEGER NOT NULL
) STRICT;
