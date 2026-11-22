-- library.db migration 0002: observation-level recording corrections.
--
-- A correction can say which recording (and optional appearance) specific
-- observations — or observations in a date range on an endpoint, optionally
-- narrowed to one source item — were of, applied above the source item's
-- assignment. A NULL recording_id explicitly unbinds the targeted
-- observations. See docs/design/history.md, "Corrections", and
-- docs/design/identity.md, "Before merge is built".

CREATE TABLE observation_correction_new (
  observation_id      INTEGER PRIMARY KEY REFERENCES history_observation(id),
  verb                TEXT NOT NULL CHECK (verb IN ('exclude', 'set_listener', 'split', 'join', 'set_clock_offset', 'set_recording', 'revoke')),
  set_listener_id     INTEGER REFERENCES listener(id),           -- set_listener: who the plays move to
  target_endpoint_id  INTEGER REFERENCES history_endpoint(id),   -- a target by window: this endpoint's observations in the range. NULL when the targets are listed
  target_from_utc_ms  INTEGER,                                   -- NULL is open
  target_to_utc_ms    INTEGER,
  clock_offset_ms     INTEGER,                                   -- set_clock_offset: added to times dated by that endpoint's wall clock
  note                TEXT,
  actor_session_pid   BLOB CHECK (length(actor_session_pid) = 16),   -- the client session that made the correction
  rev                 INTEGER NOT NULL,
  CHECK ((verb = 'set_listener') = (set_listener_id IS NOT NULL)),
  CHECK ((verb = 'set_clock_offset') = (clock_offset_ms IS NOT NULL)),
  CHECK (verb <> 'set_clock_offset' OR target_endpoint_id IS NOT NULL),
  CHECK (verb NOT IN ('split', 'join', 'revoke') OR target_endpoint_id IS NULL),
  CHECK (target_endpoint_id IS NOT NULL OR (target_from_utc_ms IS NULL AND target_to_utc_ms IS NULL)),
  CHECK (verb <> 'set_recording' OR target_from_utc_ms IS NULL OR target_to_utc_ms IS NULL OR target_from_utc_ms < target_to_utc_ms)
) STRICT;

INSERT INTO observation_correction_new
SELECT observation_id, verb, set_listener_id, target_endpoint_id, target_from_utc_ms,
       target_to_utc_ms, clock_offset_ms, note, actor_session_pid, rev
FROM observation_correction;

DROP TABLE observation_correction;
ALTER TABLE observation_correction_new RENAME TO observation_correction;

CREATE TRIGGER observation_correction_kind
BEFORE INSERT ON observation_correction
WHEN (SELECT kind FROM history_observation WHERE id = NEW.observation_id) IS NOT 'correction'
BEGIN
  SELECT RAISE(ABORT, 'observation_correction holds the detail of a correction and of nothing else');
END;

CREATE TRIGGER observation_correction_no_update
BEFORE UPDATE ON observation_correction
BEGIN
  SELECT RAISE(ABORT, 'observation_correction is immutable: revoke it instead');
END;

CREATE TRIGGER observation_correction_no_delete
BEFORE DELETE ON observation_correction
BEGIN
  SELECT RAISE(ABORT, 'observation_correction is immutable: nothing is deleted');
END;

-- [history|durable] The recording an observation-level `set_recording`
-- correction names for its target observations, read through redirects.
-- NULL recording_id leaves the targeted observations unbound.
CREATE TABLE correction_recording (
  correction_id   INTEGER PRIMARY KEY REFERENCES observation_correction(observation_id),
  recording_id    INTEGER REFERENCES recording(id),             -- stored as named; read through recording.canonical_id. NULL means unbound
  appearance_id   INTEGER,                                      -- optional appearance on recording_id; read through track_appearance.canonical_id
  source_item_id  INTEGER REFERENCES source_item(id),           -- optional filter for a window target: only observations of this source item
  rev             INTEGER NOT NULL,
  FOREIGN KEY (appearance_id, recording_id) REFERENCES track_appearance (id, recording_id),
  CHECK (appearance_id IS NULL OR recording_id IS NOT NULL)
) STRICT;

CREATE INDEX correction_recording_recording
  ON correction_recording (recording_id)
  WHERE recording_id IS NOT NULL;

CREATE TRIGGER correction_recording_verb
BEFORE INSERT ON correction_recording
WHEN (SELECT verb FROM observation_correction WHERE observation_id = NEW.correction_id) IS NOT 'set_recording'
BEGIN
  SELECT RAISE(ABORT, 'correction_recording holds the detail of a set_recording correction and of nothing else');
END;

CREATE TRIGGER correction_recording_source_item_window
BEFORE INSERT ON correction_recording
WHEN NEW.source_item_id IS NOT NULL
 AND (SELECT target_endpoint_id FROM observation_correction WHERE observation_id = NEW.correction_id) IS NULL
BEGIN
  SELECT RAISE(ABORT, 'correction_recording.source_item_id requires a window target');
END;

CREATE TRIGGER correction_recording_no_update
BEFORE UPDATE ON correction_recording
BEGIN
  SELECT RAISE(ABORT, 'correction_recording is immutable: revoke the correction instead');
END;

CREATE TRIGGER correction_recording_no_delete
BEFORE DELETE ON correction_recording
BEGIN
  SELECT RAISE(ABORT, 'correction_recording is immutable: nothing is deleted');
END;
