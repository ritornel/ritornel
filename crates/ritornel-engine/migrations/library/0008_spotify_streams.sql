-- =============================================================================
-- Ritornel library.db, migration 0008: Spotify's streams, kept beside the
-- scrobbles and never in place of them, and who and what each play was
-- heard as, with files for it or not.
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
  records_hash          BLOB NOT NULL CHECK (length(records_hash) = 32),     -- SHA-256 of the kept records in their own order: the same archive has the same hash, whatever is stored
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

-- How an observation that is not a play's primary joined it: derived, and
-- rewritten with the plays. NULL for a primary.
ALTER TABLE play_event_observation ADD COLUMN joined_by TEXT
  CHECK (joined_by IN ('chain', 'echo', 'repeat', 'start', 'during', 'renamed', 'pin'));

-- =============================================================================
-- Who and what each play was of, when there are no files for it: derived,
-- and rewritten with the plays and by nothing else. A play bound to a song
-- has its credit's artists and the record it was heard from. Any other
-- has its names, by their keys: an artist's name, and an album's under it.
-- A name is the library's artist when exactly one artist it can be has
-- that key: one with a record, one a bound play's song credits, or one
-- credited on a record a play was heard from. An album's name is that
-- artist's record when exactly one of theirs, with files or heard from,
-- has its title; which, the catalog decides, and it is found
-- again whenever the catalog changes, without touching a play. The History
-- lists read these, so that no query folds every play's names as it runs.
-- =============================================================================

CREATE TABLE heard_artist (
  id                 INTEGER PRIMARY KEY,
  artist_id          INTEGER UNIQUE REFERENCES artist(id),   -- the library's artist a bound play's credit names, as canonical when linked; NULL for a name
  name_key           TEXT,                                   -- a name's key (accents folded, no leading 'The' or featured credit); NULL for the library's
  name               TEXT,                                   -- a name's spelling played most; NULL for the library's
  library_artist_id  INTEGER REFERENCES artist(id),          -- for a name, the library's artist it is, when exactly one it can be has its key
  rev                INTEGER NOT NULL,
  CHECK ((artist_id IS NULL) = (name_key IS NOT NULL)),
  CHECK ((name_key IS NULL) = (name IS NULL)),
  CHECK (name_key IS NOT NULL OR library_artist_id IS NULL)
) STRICT;

CREATE UNIQUE INDEX heard_artist_name ON heard_artist (name_key) WHERE name_key IS NOT NULL;

CREATE TABLE heard_album (
  id                INTEGER PRIMARY KEY,
  release_group_id  INTEGER UNIQUE REFERENCES release_group(id),   -- the record a bound play was heard from; NULL for a name
  heard_artist_id   INTEGER REFERENCES heard_artist(id),           -- the artist's name it is under; NULL for the library's
  name_key          TEXT,                                          -- a name's key under its artist; NULL for the library's
  name              TEXT,                                          -- a name's spelling played most; NULL for the library's
  library_group_id  INTEGER REFERENCES release_group(id),          -- for a name, the library's record it is, when its artist is the library's and exactly one of theirs has its title
  rev               INTEGER NOT NULL,
  CHECK ((release_group_id IS NULL) = (heard_artist_id IS NOT NULL)),
  CHECK ((heard_artist_id IS NULL) = (name_key IS NULL)),
  CHECK ((name_key IS NULL) = (name IS NULL)),
  CHECK (name_key IS NOT NULL OR library_group_id IS NULL)
) STRICT;

CREATE UNIQUE INDEX heard_album_name ON heard_album (heard_artist_id, name_key)
  WHERE heard_artist_id IS NOT NULL;

-- A play's artists: one for each of its credit's artists, or its name's.
-- Whose it is, whether it counts and its local date are copied from the
-- play, so that a list is read from the index on them alone.
CREATE TABLE play_event_artist (
  id               INTEGER PRIMARY KEY,
  heard_artist_id  INTEGER NOT NULL REFERENCES heard_artist(id),
  play_event_id    INTEGER NOT NULL REFERENCES play_event(id),
  listener_id      INTEGER NOT NULL REFERENCES listener(id),     -- the play's
  counted          INTEGER NOT NULL CHECK (counted IN (0, 1)),   -- counts and not excluded
  local_date       INTEGER,                                      -- YYYYMMDD
  event_at_utc_ms  INTEGER,                                      -- the play's, so that one stamped after now is never a list's first or last
  spelling         TEXT,                                         -- the name as the play gave it, for a name
  rev              INTEGER NOT NULL,
  UNIQUE (play_event_id, heard_artist_id)
) STRICT;

CREATE INDEX play_event_artist_heard ON play_event_artist (heard_artist_id, counted);
CREATE INDEX play_event_artist_listed ON play_event_artist (listener_id, counted, heard_artist_id, local_date, event_at_utc_ms);

-- A play's album, when it has one.
CREATE TABLE play_event_album (
  id              INTEGER PRIMARY KEY,
  heard_album_id  INTEGER NOT NULL REFERENCES heard_album(id),
  play_event_id   INTEGER NOT NULL UNIQUE REFERENCES play_event(id),
  listener_id     INTEGER NOT NULL REFERENCES listener(id),
  counted         INTEGER NOT NULL CHECK (counted IN (0, 1)),
  local_date      INTEGER,
  event_at_utc_ms INTEGER,
  spelling        TEXT,
  rev             INTEGER NOT NULL
) STRICT;

CREATE INDEX play_event_album_heard ON play_event_album (heard_album_id, counted);
CREATE INDEX play_event_album_listed ON play_event_album (listener_id, counted, heard_album_id, local_date, event_at_utc_ms);

-- An artist's page counts a song as the appearance it was heard from
-- credits it, as the lists link it: the appearances whose credit names
-- them are found by it.
CREATE INDEX track_appearance_display_artist_credit ON track_appearance (display_artist_credit_id);
