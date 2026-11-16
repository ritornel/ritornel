-- =============================================================================
-- Ritornel library.db, migration 0004: what the Last.fm slice adds. Plays
-- another service reported, where its reading of them has got to, the account
-- they came from, and the record of each play sent there.
--
-- - `observation_import` is the detail of an import record: one play a
--   service reported, with the facts read from it. The record itself is the
--   observation's payload (history.md, "Observation kinds").
-- - `history_cursor` is a reader's place in a paged import, committed in the
--   transaction with the observations it covers.
-- - `service_account` is one account at a service: the history endpoint its
--   plays come from and the source instance whose text-only items name what
--   they were of (identity.md, "Scrobbles").
-- - `history_delivery` is one play sent to one destination, recorded before it
--   is sent, so that its echo is matched exactly when the import brings it
--   back (history.md, "Last.fm").
-- - `identity_decision.method` takes `scrobble_text`: a scrobble's item bound
--   by its artist and title, which is neither a file's tags and length nor an
--   identifier. SQLite cannot change a CHECK in place, so the table is
--   rebuilt as migration 0003 rebuilt its tables, and its triggers made again.
--   The legacy rename leaves `assignment`'s foreign key naming it.
--
-- The conventions are those of migration 0001. This file is never edited once
-- it has shipped.
-- =============================================================================

PRAGMA legacy_alter_table = ON;

ALTER TABLE identity_decision RENAME TO identity_decision_0003;

-- [evidence|durable] Why one binding, merge or undo was made, with the full score shape. Immutable. identity.md decision 8, "Resolver policy 1" and "Scrobbles"; review 5.4, 5.10.
CREATE TABLE identity_decision (
  id                   INTEGER PRIMARY KEY,
  action               TEXT NOT NULL CHECK (action IN ('bind', 'merge', 'unmerge')),
  source_item_id       INTEGER REFERENCES source_item(id),   -- bind: the item decided
  entity_kind          TEXT CHECK (entity_kind IN ('recording', 'artist', 'release', 'track_appearance')),   -- merge and unmerge: what was redirected
  entity_id            INTEGER,                              -- the row whose redirect was set or cleared
  target_entity_id     INTEGER,                              -- merge: where it now points
  method               TEXT NOT NULL CHECK (method IN ('user', 'same_audio', 'identifier', 'tags_duration', 'new', 'scrobble_text')),   -- scrobble_text: a text-only item bound by its artist and title
  resolver_version     INTEGER NOT NULL,
  normalizer_version   INTEGER NOT NULL,
  score                REAL CHECK (score BETWEEN 0 AND 1),   -- a distance; 0 for exact methods, NULL for a user decision
  runner_up_gap        REAL,                                 -- NULL with no runner-up
  penalty_vector_json  TEXT,
  evidence_json        TEXT NOT NULL,                        -- claims used (by namespace or field and value, never by row ID), candidates with distances, contradictions
  needs_review         INTEGER NOT NULL DEFAULT 0 CHECK (needs_review IN (0, 1)),
  actor_kind           TEXT NOT NULL CHECK (actor_kind IN ('resolver', 'person', 'agent')),
  actor_session_pid    BLOB CHECK (length(actor_session_pid) = 16),
  rationale            TEXT,
  decided_at_ms        INTEGER NOT NULL,
  rev                  INTEGER NOT NULL,
  CHECK ((action = 'bind') = (source_item_id IS NOT NULL)),
  CHECK ((action = 'bind') = (entity_kind IS NULL AND entity_id IS NULL)),
  CHECK ((action = 'merge') = (target_entity_id IS NOT NULL))
) STRICT;

INSERT INTO identity_decision SELECT * FROM identity_decision_0003;
DROP TABLE identity_decision_0003;

-- A decision is never changed or removed. Whether it was the owner's decides if the resolver may supersede the binding, so it must not be editable.
CREATE TRIGGER identity_decision_no_update
BEFORE UPDATE ON identity_decision
BEGIN
  SELECT RAISE(ABORT, 'identity_decision is immutable');
END;

CREATE TRIGGER identity_decision_no_delete
BEFORE DELETE ON identity_decision
BEGIN
  SELECT RAISE(ABORT, 'identity_decision is immutable');
END;

PRAGMA legacy_alter_table = OFF;

-- [history|durable] One account at an outside service: where its plays come from and what names what they were of. history.md "Last.fm"; identity.md "Scrobbles".
CREATE TABLE service_account (
  id                  INTEGER PRIMARY KEY,
  public_id           BLOB NOT NULL UNIQUE CHECK (length(public_id) = 16),
  service             TEXT NOT NULL CHECK (service IN ('lastfm')),
  account_name        TEXT NOT NULL,                                          -- as the service spells it; untrusted display text
  listener_id         INTEGER NOT NULL REFERENCES listener(id),               -- whose plays and loves these are
  endpoint_id         INTEGER NOT NULL UNIQUE REFERENCES history_endpoint(id),   -- of kind 'service': the account's plays are its observations
  source_instance_id  INTEGER NOT NULL UNIQUE REFERENCES source_instance(id),    -- of the service's kind: what a play was of is one of its items
  created_at_ms       INTEGER NOT NULL,
  rev                 INTEGER NOT NULL,
  UNIQUE (service, account_name COLLATE NOCASE)
) STRICT;

-- [history|durable] The detail of one import record: a play another service reported, and the facts read from it. The record verbatim is the observation's payload. Immutable. history.md "Observation kinds".
CREATE TABLE observation_import (
  observation_id    INTEGER PRIMARY KEY REFERENCES history_observation(id),
  played_at_utc_ms  INTEGER NOT NULL,   -- when the service dates the play; for Last.fm its start, to the second
  artist            TEXT NOT NULL,      -- what it was of, as the service spells it; the envelope's source item stands for the three
  album             TEXT,
  title             TEXT NOT NULL,
  artist_mbid       TEXT,               -- the identifiers the service attached. They support a match and never make one alone
  album_mbid        TEXT,
  track_mbid        TEXT,
  rev               INTEGER NOT NULL
) STRICT;

CREATE INDEX observation_import_played_at ON observation_import (played_at_utc_ms);

CREATE TRIGGER observation_import_kind
BEFORE INSERT ON observation_import
WHEN (SELECT kind FROM history_observation WHERE id = NEW.observation_id) IS NOT 'import'
BEGIN
  SELECT RAISE(ABORT, 'observation_import holds the detail of an import record and of nothing else');
END;

CREATE TRIGGER observation_import_no_update
BEFORE UPDATE ON observation_import
BEGIN
  SELECT RAISE(ABORT, 'observation_import is immutable: record a correction instead');
END;

CREATE TRIGGER observation_import_no_delete
BEFORE DELETE ON observation_import
BEGIN
  SELECT RAISE(ABORT, 'observation_import is immutable: nothing is deleted');
END;

-- [history|durable] How far a reader has got in a paged import or a log, in a form only that reader reads. Committed with the observations it covers. history.md "Observation kinds".
CREATE TABLE history_cursor (
  id             INTEGER PRIMARY KEY,
  endpoint_id    INTEGER NOT NULL UNIQUE REFERENCES history_endpoint(id),
  cursor_json    TEXT NOT NULL CHECK (json_valid(cursor_json)),
  updated_at_ms  INTEGER NOT NULL,
  rev            INTEGER NOT NULL
) STRICT;

-- [history|durable] One play sent to one destination, recorded before it is sent: what was sent, and what became of it. It names the play's primary observation, never the derived play. history.md decision 3 and "Last.fm".
CREATE TABLE history_delivery (
  id                  INTEGER PRIMARY KEY,
  observation_id      INTEGER NOT NULL REFERENCES history_observation(id),   -- the play's primary observation: a listen
  endpoint_id         INTEGER NOT NULL REFERENCES history_endpoint(id),      -- the destination, a service account's endpoint
  state               TEXT NOT NULL CHECK (state IN ('pending', 'sending', 'accepted', 'ignored', 'refused', 'unknown', 'withdrawn')),   -- 'sending' is written before the request leaves; 'accepted' and 'ignored' are the service's answers, final and apart; 'withdrawn' was never sent, because its play was no longer eligible when its turn came
  sent_timestamp_s    INTEGER NOT NULL,   -- the time it is sent with, in seconds: what its echo is matched on
  artist              TEXT NOT NULL,      -- what it is sent as
  title               TEXT NOT NULL,
  album               TEXT,
  album_artist        TEXT,
  track_number        INTEGER,
  duration_ms         INTEGER,
  accepted_artist     TEXT,               -- the names the service took it under, which it may have corrected
  accepted_title      TEXT,
  ignored_code        INTEGER,            -- the service's reason, as it sent it
  detail              TEXT,               -- the service's message, or why it was not sent or was withdrawn
  attempts            INTEGER NOT NULL DEFAULT 0,
  next_attempt_at_ms  INTEGER,            -- when a pending row is next due
  created_at_ms       INTEGER NOT NULL,
  updated_at_ms       INTEGER NOT NULL,
  rev                 INTEGER NOT NULL,
  UNIQUE (observation_id, endpoint_id),
  CHECK ((state = 'accepted') = (accepted_artist IS NOT NULL AND accepted_title IS NOT NULL)),
  CHECK ((state = 'ignored') = (ignored_code IS NOT NULL))
) STRICT;

CREATE INDEX history_delivery_echo ON history_delivery (endpoint_id, sent_timestamp_s);

CREATE INDEX history_delivery_due ON history_delivery (endpoint_id, next_attempt_at_ms)
  WHERE state = 'pending';
