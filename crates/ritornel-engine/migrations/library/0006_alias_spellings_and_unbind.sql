-- 1. Allow `identity_decision.action = 'unbind'` so releasing an automatic
--    scrobble binding leaves an explicit, immutable decision row with its
--    matcher version, normaliser version, and evidence.
-- 2. Store the raw spellings (`spelling_a`, `spelling_b`) on
--    `alias_observation` alongside the normalised keys (`name_a`, `name_b`)
--    so owner aliases can be recomputed when the normaliser version changes.

CREATE TABLE identity_decision_new (
  id                   INTEGER PRIMARY KEY,
  action               TEXT NOT NULL CHECK (action IN ('bind', 'unbind', 'merge', 'unmerge')),
  source_item_id       INTEGER REFERENCES source_item(id),   -- bind and unbind: the item decided
  entity_kind          TEXT CHECK (entity_kind IN ('recording', 'artist', 'release', 'track_appearance')),   -- merge and unmerge: what was redirected
  entity_id            INTEGER,                              -- the row whose redirect was set or cleared
  target_entity_id     INTEGER,                              -- merge: where it now points
  method               TEXT NOT NULL CHECK (method IN ('user', 'same_audio', 'identifier', 'tags_duration', 'new', 'scrobble_text')),   -- scrobble_text: a text-only item bound or unbound by its artist and title
  resolver_version     INTEGER NOT NULL,
  normalizer_version   INTEGER NOT NULL,
  score                REAL CHECK (score BETWEEN 0 AND 1),   -- a distance; 0 for exact methods, NULL for a user or unbind decision
  runner_up_gap        REAL,                                 -- NULL with no runner-up
  penalty_vector_json  TEXT,
  evidence_json        TEXT NOT NULL,                        -- claims used (by namespace or field and value, never by row ID), candidates with distances, contradictions
  needs_review         INTEGER NOT NULL DEFAULT 0 CHECK (needs_review IN (0, 1)),
  actor_kind           TEXT NOT NULL CHECK (actor_kind IN ('resolver', 'person', 'agent')),
  actor_session_pid    BLOB CHECK (length(actor_session_pid) = 16),
  rationale            TEXT,
  decided_at_ms        INTEGER NOT NULL,
  rev                  INTEGER NOT NULL,
  CHECK ((action IN ('bind', 'unbind')) = (source_item_id IS NOT NULL)),
  CHECK ((action IN ('bind', 'unbind')) = (entity_kind IS NULL AND entity_id IS NULL)),
  CHECK ((action = 'merge') = (target_entity_id IS NOT NULL)),
  CHECK (action <> 'unbind' OR score IS NULL)
) STRICT;

INSERT INTO identity_decision_new (
  id, action, source_item_id, entity_kind, entity_id, target_entity_id, method,
  resolver_version, normalizer_version, score, runner_up_gap, penalty_vector_json,
  evidence_json, needs_review, actor_kind, actor_session_pid, rationale, decided_at_ms, rev
)
SELECT
  id, action, source_item_id, entity_kind, entity_id, target_entity_id, method,
  resolver_version, normalizer_version, score, runner_up_gap, penalty_vector_json,
  evidence_json, needs_review, actor_kind, actor_session_pid, rationale, decided_at_ms, rev
FROM identity_decision;

DROP TABLE identity_decision;
ALTER TABLE identity_decision_new RENAME TO identity_decision;

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

CREATE TABLE alias_observation_new (
  id                  INTEGER PRIMARY KEY,
  scope               TEXT NOT NULL CHECK (scope IN ('artist', 'title', 'release')),
  name_a              TEXT NOT NULL,   -- normalised; the pair is unordered, stored with name_a < name_b
  name_b              TEXT NOT NULL,
  spelling_a          TEXT,            -- raw spelling that produced name_a; NULL only for rows created before migration 6
  spelling_b          TEXT,            -- raw spelling that produced name_b
  normalizer_version  INTEGER NOT NULL,
  decision_id         INTEGER NOT NULL REFERENCES identity_decision(id),   -- the same-audio, identifier or user decision that linked them
  observed_at_ms      INTEGER NOT NULL,
  rev                 INTEGER NOT NULL,
  CHECK (name_a < name_b),
  CHECK ((spelling_a IS NULL) = (spelling_b IS NULL)),
  CHECK (spelling_a IS NULL OR (octet_length(spelling_a) > 0 AND octet_length(spelling_b) > 0))
) STRICT;

INSERT INTO alias_observation_new (
  id, scope, name_a, name_b, spelling_a, spelling_b, normalizer_version,
  decision_id, observed_at_ms, rev
)
SELECT
  id, scope, name_a, name_b, NULL, NULL, normalizer_version,
  decision_id, observed_at_ms, rev
FROM alias_observation;

DROP TABLE alias_observation;
ALTER TABLE alias_observation_new RENAME TO alias_observation;
