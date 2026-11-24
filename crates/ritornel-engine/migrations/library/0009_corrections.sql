-- The corrections engine: the evidence gathered beside the files and from
-- MusicBrainz, what it proposes each disc is, and what was said to it. See
-- docs/internals/corrections.md.
--
-- Evidence (sidecars, disc layouts, replies and the facts read from them)
-- and proposals are read by no resolver and shown on no wall. Answers are
-- the ledger: append-only, each value or rule withdrawn once by the undo of
-- the answer that wrote it, and read by the projector while they stand.

-- Sidecars and the disc layouts read from them. See "Disc layouts" in
-- docs/internals/corrections.md.
--
-- A sidecar is a rip log, CUE sheet or checksum report found with the
-- audio. A scan keeps its bytes as it read them, and a layout is derived
-- from them and from the files' stored lengths. Nothing here is read by
-- the resolver or shown on a wall.

-- [evidence|durable] One sidecar file of a source, by its path under the
-- root. Like a source item, it is marked missing when a scan of a
-- reachable root no longer finds it, and keeps its revisions.
CREATE TABLE sidecar (
  id                    INTEGER PRIMARY KEY,
  source_instance_id    INTEGER NOT NULL REFERENCES source_instance(id),
  rel_path              TEXT NOT NULL,                  -- relative to the root, with '/' between its parts
  current_revision_id   INTEGER,                        -- one of this sidecar's own revisions
  size_bytes            INTEGER NOT NULL,
  mtime_ms              INTEGER NOT NULL,               -- unchanged size and time let a rescan skip the file
  missing_since_ms      INTEGER,
  first_seen_at_ms      INTEGER NOT NULL,
  rev                   INTEGER NOT NULL,
  UNIQUE (source_instance_id, rel_path),
  FOREIGN KEY (id, current_revision_id) REFERENCES sidecar_revision (sidecar_id, id)
) STRICT;

-- [evidence|durable] The bytes a sidecar held when a scan read it, kept
-- whole and deduplicated by their hash. Immutable.
CREATE TABLE sidecar_revision (
  id                INTEGER PRIMARY KEY,
  sidecar_id        INTEGER NOT NULL REFERENCES sidecar(id),
  content_hash      BLOB NOT NULL CHECK (length(content_hash) = 32),   -- BLAKE3 of content
  content           BLOB NOT NULL,                                     -- the file's bytes, which may be UTF-16 or Windows-1252
  observed_at_ms    INTEGER NOT NULL,
  rev               INTEGER NOT NULL,
  UNIQUE (sidecar_id, content_hash),
  UNIQUE (sidecar_id, id)
) STRICT;

CREATE TRIGGER sidecar_revision_no_update
BEFORE UPDATE ON sidecar_revision
BEGIN
  SELECT RAISE(ABORT, 'sidecar_revision is immutable');
END;

CREATE TRIGGER sidecar_revision_no_delete
BEFORE DELETE ON sidecar_revision
BEGIN
  SELECT RAISE(ABORT, 'sidecar_revision is immutable');
END;

-- An INSERT OR REPLACE would remove the row it replaces, by its ID or by
-- either unique key, without the delete trigger, which does not fire for it.
CREATE TRIGGER sidecar_revision_no_replace
BEFORE INSERT ON sidecar_revision
WHEN EXISTS (SELECT 1 FROM sidecar_revision
             WHERE id = NEW.id OR (sidecar_id = NEW.sidecar_id AND content_hash = NEW.content_hash))
BEGIN
  SELECT RAISE(ABORT, 'sidecar_revision is immutable');
END;

-- [evidence|derived] One layout of the audio files of one folder: where
-- each track of a CD starts, and the MusicBrainz disc ID that gives.
-- Rebuilt from sidecar revisions and the files' stored lengths by the
-- layout version in layout_version.
CREATE TABLE evidence_disc (
  id                    INTEGER PRIMARY KEY,
  source_instance_id    INTEGER NOT NULL REFERENCES source_instance(id),
  folder                TEXT NOT NULL,                  -- relative to the root; '' is the root
  disc_id               TEXT NOT NULL CHECK (length(disc_id) = 28),
  track_offsets_json    TEXT NOT NULL,                  -- the sector each track starts at, from the first track's, without the lead-in
  lead_out              INTEGER NOT NULL,               -- the sector after the last track's audio
  found_json            TEXT NOT NULL,                  -- each way it was found, in order: [{"method":"log","sidecarRevisionId":12}]
  provisional           INTEGER NOT NULL CHECK (provisional IN (0, 1)),   -- a CUE sheet that disagrees with the CDDB ID it prints
  whole_file            INTEGER NOT NULL CHECK (whole_file IN (0, 1)),    -- one file holds the whole disc
  layout_version        INTEGER NOT NULL,
  rev                   INTEGER NOT NULL,
  UNIQUE (source_instance_id, folder, disc_id)
) STRICT;

-- [evidence|derived] The file at each position of a layout: every track
-- for a layout whose positions each fit exactly one file, or the one file
-- of a whole-disc image at position 0.
CREATE TABLE evidence_disc_item (
  id                    INTEGER PRIMARY KEY,
  evidence_disc_id      INTEGER NOT NULL REFERENCES evidence_disc(id),
  position              INTEGER NOT NULL CHECK (position >= 0),
  source_item_id        INTEGER NOT NULL REFERENCES source_item(id),
  rev                   INTEGER NOT NULL,
  UNIQUE (evidence_disc_id, position)
) STRICT;

CREATE INDEX evidence_disc_item_item ON evidence_disc_item (source_item_id);

-- What a catalogue replied when it was asked about a disc, and the facts
-- read from each reply. See "MusicBrainz lookups" in
-- docs/internals/corrections.md.

-- [evidence|durable] One reply a catalogue gave, kept as it came: a reply
-- naming releases, or one saying it knows no such thing. A request is the
-- catalogue, the kind of question, what it was about and what it asked to
-- be included. The newest reply to a request is its current one. A reply
-- the same as the current one is not kept again. Immutable.
CREATE TABLE lookup_response (
  id                INTEGER PRIMARY KEY,
  catalogue         TEXT NOT NULL,                  -- 'musicbrainz'
  request_kind      TEXT NOT NULL,                  -- 'disc'
  request_key       TEXT NOT NULL,                  -- the disc ID
  includes          TEXT NOT NULL,                  -- what the request asked to be included
  status            INTEGER NOT NULL CHECK (status IN (200, 404)),
  body              TEXT NOT NULL,
  body_hash         BLOB NOT NULL CHECK (length(body_hash) = 32),   -- BLAKE3 of body
  received_at_ms    INTEGER NOT NULL,
  rev               INTEGER NOT NULL
) STRICT;

CREATE INDEX lookup_response_request
  ON lookup_response (catalogue, request_kind, request_key, includes, id);

CREATE TRIGGER lookup_response_no_update
BEFORE UPDATE ON lookup_response
BEGIN
  SELECT RAISE(ABORT, 'lookup_response is immutable');
END;

CREATE TRIGGER lookup_response_no_delete
BEFORE DELETE ON lookup_response
BEGIN
  SELECT RAISE(ABORT, 'lookup_response is immutable');
END;

-- An INSERT OR REPLACE would remove the row it replaces without the
-- delete trigger, which does not fire for it.
CREATE TRIGGER lookup_response_no_replace
BEFORE INSERT ON lookup_response
WHEN EXISTS (SELECT 1 FROM lookup_response WHERE id = NEW.id)
BEGIN
  SELECT RAISE(ABORT, 'lookup_response is immutable');
END;

-- [evidence|derived] One fact a reply states about one release, as a
-- file's claims are facts its tags state. Rebuilt from lookup_response by
-- the reader version in reader_version.
CREATE TABLE evidence_claim (
  id                    INTEGER PRIMARY KEY,
  lookup_response_id    INTEGER NOT NULL REFERENCES lookup_response(id),
  release_mbid          TEXT NOT NULL,                -- the release the fact is about
  field                 TEXT NOT NULL,                -- such as 'mb_release_group', 'album_artist', 'disc_number'
  value                 TEXT NOT NULL,
  ordinal               INTEGER NOT NULL,             -- the position among a field's values, from 0
  reader_version        INTEGER NOT NULL,
  rev                   INTEGER NOT NULL
) STRICT;

CREATE INDEX evidence_claim_response ON evidence_claim (lookup_response_id);
CREATE INDEX evidence_claim_release ON evidence_claim (release_mbid, field);

-- Proposals: what the replies propose each disc is, and the discs of an
-- album as one release of it. See "Album proposals" and "Edition
-- proposals" in docs/internals/corrections.md. All four tables are
-- derived, worked out again from the layouts, the current replies and the
-- files whenever either changes, and under a new proposer version.

-- [evidence|derived] One proposal: that one disc of the library is one
-- album, or that the discs of an album are one release of it. Its public
-- ID is computed from its kind and its anchor, the first-seen file it is
-- about, and for an edition its album, so a proposal derived again keeps
-- its ID.
CREATE TABLE proposal (
  id                    INTEGER PRIMARY KEY,
  public_id             BLOB NOT NULL UNIQUE CHECK (length(public_id) = 16),
  kind                  TEXT NOT NULL CHECK (kind IN ('album', 'edition')),
  anchor_item_id        INTEGER NOT NULL REFERENCES source_item(id),
  group_public_id       BLOB NOT NULL CHECK (length(group_public_id) = 16),   -- the proposal whose anchor is its group's
  source_instance_id    INTEGER NOT NULL REFERENCES source_instance(id),
  folder                TEXT NOT NULL,                                       -- relative to the root: the anchor's
  fingerprint           TEXT NOT NULL,                                       -- a digest of all of it, which a yes names
  proposer_version      INTEGER NOT NULL,
  rev                   INTEGER NOT NULL
) STRICT;

CREATE INDEX proposal_group ON proposal (group_public_id);

-- [evidence|derived] The files a proposal is about.
CREATE TABLE proposal_item (
  id                INTEGER PRIMARY KEY,
  proposal_id       INTEGER NOT NULL REFERENCES proposal(id),
  source_item_id    INTEGER NOT NULL REFERENCES source_item(id),
  rev               INTEGER NOT NULL,
  UNIQUE (proposal_id, source_item_id)
) STRICT;

CREATE INDEX proposal_item_item ON proposal_item (source_item_id);

-- [evidence|derived] One album a disc may be on, or one release the
-- discs of an album may be, and the tag values a yes to it would put on
-- the files.
CREATE TABLE proposal_candidate (
  id                    INTEGER PRIMARY KEY,
  proposal_id           INTEGER NOT NULL REFERENCES proposal(id),
  ordinal               INTEGER NOT NULL,              -- the likeliest first
  release_group_mbid    TEXT NOT NULL,                 -- the album
  release_mbid          TEXT,                          -- for an edition: the release
  title                 TEXT,                          -- the album's, or for an edition the release's
  artist                TEXT NOT NULL,
  primary_type          TEXT,
  first_date            TEXT,                          -- the album's first, or for an edition the release's
  country               TEXT,                          -- for an edition: where the release came out
  media                 INTEGER,                       -- for an edition: how many media the release has
  releases              INTEGER NOT NULL,              -- how many of the replies' releases are of it
  change                TEXT NOT NULL CHECK (change IN ('knows', 'gains', 'moves')),
  values_json           TEXT NOT NULL,                 -- [{"item":12,"field":"disc_number","ordinal":0,"value":"2"}]
  onto_json             TEXT NOT NULL,                 -- the files of the first disc a yes joins these onto: [14,15]
  rev                   INTEGER NOT NULL,
  CHECK (release_mbid IS NOT NULL OR (country IS NULL AND media IS NULL)),
  UNIQUE (proposal_id, ordinal)
) STRICT;

-- [evidence|derived] What a proposal rests on: each disc ID whose layout
-- and current reply it was worked out from.
CREATE TABLE proposal_evidence (
  id                    INTEGER PRIMARY KEY,
  proposal_id           INTEGER NOT NULL REFERENCES proposal(id),
  role                  TEXT NOT NULL CHECK (role IN ('rests_on')),
  disc_id               TEXT NOT NULL,
  lookup_response_id    INTEGER NOT NULL REFERENCES lookup_response(id),   -- the reply, as it was current
  rev                   INTEGER NOT NULL,
  UNIQUE (proposal_id, role, disc_id, lookup_response_id)
) STRICT;

-- Answers: what was said to a proposal, and the tag values a yes puts on
-- files. See "Answers" in docs/internals/corrections.md.
--
-- The ledger is append-only. An undo is an answer that names the one it
-- undoes, and the only change a value ever takes is being withdrawn, once.
-- A verb, a kind, a field and an origin are names the code gives, left
-- open here so that later kinds of answer add to them without rewriting a
-- row.

-- [corrections|durable] One answer: a yes or a no to a proposal, two
-- albums kept apart, or the undo of an earlier answer. Immutable.
CREATE TABLE answer (
  id                      INTEGER PRIMARY KEY,
  public_id               BLOB NOT NULL UNIQUE CHECK (length(public_id) = 16),
  verb                    TEXT NOT NULL CHECK (length(verb) > 0),               -- 'confirm', 'decline', 'stop_asking', 'apart' or 'undo'
  kind                    TEXT CHECK (length(kind) > 0),                        -- what a yes or a no was about: 'album' or 'edition'; NULL otherwise
  proposal_public_id      BLOB CHECK (proposal_public_id IS NULL OR length(proposal_public_id) = 16),
  proposal_fingerprint    TEXT,                                                 -- the proposal as it was shown
  candidate               TEXT,                                                 -- what a yes or a no named: a release group, or for an edition a release
  undoes_answer_id        INTEGER REFERENCES answer(id),
  actor_kind              TEXT NOT NULL CHECK (actor_kind IN ('person', 'agent', 'rule')),
  actor_session_pid       BLOB CHECK (actor_session_pid IS NULL OR length(actor_session_pid) = 16),
  answered_at_ms          INTEGER NOT NULL,
  layout_version          INTEGER NOT NULL,
  reader_version          INTEGER NOT NULL,
  proposer_version        INTEGER NOT NULL,
  evidence_json           TEXT NOT NULL,                                        -- what it rested on, by value
  before_json             TEXT NOT NULL,                                        -- each file's assignment before it
  rev                     INTEGER NOT NULL,
  CHECK (verb <> 'confirm' OR (kind IS NOT NULL AND proposal_public_id IS NOT NULL
                                AND proposal_fingerprint IS NOT NULL AND candidate IS NOT NULL
                                AND undoes_answer_id IS NULL)),
  CHECK ((verb = 'undo') = (undoes_answer_id IS NOT NULL)),
  CHECK (verb <> 'undo' OR (kind IS NULL AND proposal_public_id IS NULL
                             AND proposal_fingerprint IS NULL AND candidate IS NULL))
) STRICT;

CREATE UNIQUE INDEX answer_undone ON answer (undoes_answer_id) WHERE undoes_answer_id IS NOT NULL;

CREATE TRIGGER answer_no_update
BEFORE UPDATE ON answer
BEGIN
  SELECT RAISE(ABORT, 'answer is immutable');
END;

CREATE TRIGGER answer_no_delete
BEFORE DELETE ON answer
BEGIN
  SELECT RAISE(ABORT, 'answer is immutable');
END;

-- An undo names an answer that is there, and not another undo.
CREATE TRIGGER answer_undoes_an_answer
BEFORE INSERT ON answer
WHEN NEW.undoes_answer_id IS NOT NULL
 AND NOT EXISTS (SELECT 1 FROM answer WHERE id = NEW.undoes_answer_id AND verb <> 'undo')
BEGIN
  SELECT RAISE(ABORT, 'an undo names an answer that is not an undo');
END;

-- An INSERT OR REPLACE would remove the row it replaces, by its ID or by
-- a unique key, without the delete trigger, which does not fire for it.
CREATE TRIGGER answer_no_replace
BEFORE INSERT ON answer
WHEN EXISTS (SELECT 1 FROM answer
             WHERE id = NEW.id OR public_id = NEW.public_id OR undoes_answer_id = NEW.undoes_answer_id)
BEGIN
  SELECT RAISE(ABORT, 'answer is immutable');
END;

-- [corrections|durable] A tag value an answer put on a file. In force for
-- a file and a field are all the values of the newest answer that wrote
-- that field and whose values are not withdrawn. The only change a row
-- takes is withdrawn_by_answer_id, set once by the answer that undid it.
CREATE TABLE answer_value (
  id                        INTEGER PRIMARY KEY,
  answer_id                 INTEGER NOT NULL REFERENCES answer(id),
  source_item_id            INTEGER NOT NULL REFERENCES source_item(id),
  field                     TEXT NOT NULL CHECK (length(field) > 0),            -- a field values::FIELDS names
  ordinal                   INTEGER NOT NULL CHECK (ordinal >= 0),
  value                     TEXT,                                               -- NULL clears the field
  origin                    TEXT NOT NULL CHECK (length(origin) > 0),           -- 'catalogue', MusicBrainz's reply, or 'library', another of the library's files
  withdrawn_by_answer_id    INTEGER REFERENCES answer(id),
  rev                       INTEGER NOT NULL,
  UNIQUE (answer_id, source_item_id, field, ordinal)
) STRICT;

CREATE INDEX answer_value_item ON answer_value (source_item_id, field);

-- Withdrawn once, and only by the undo of the answer that wrote it.
CREATE TRIGGER answer_value_withdrawn_once
BEFORE UPDATE ON answer_value
WHEN OLD.withdrawn_by_answer_id IS NOT NULL
  OR NEW.withdrawn_by_answer_id IS NULL
  OR NOT EXISTS (SELECT 1 FROM answer u
                 WHERE u.id = NEW.withdrawn_by_answer_id AND u.undoes_answer_id = OLD.answer_id)
  OR NEW.id IS NOT OLD.id
  OR NEW.answer_id IS NOT OLD.answer_id
  OR NEW.source_item_id IS NOT OLD.source_item_id
  OR NEW.field IS NOT OLD.field
  OR NEW.ordinal IS NOT OLD.ordinal
  OR NEW.value IS NOT OLD.value
  OR NEW.origin IS NOT OLD.origin
BEGIN
  SELECT RAISE(ABORT, 'answer_value changes only by being withdrawn, once');
END;

-- A value is written in force, by a yes that is not undone. A later verb
-- that writes values is let in here when it comes.
CREATE TRIGGER answer_value_written_in_force
BEFORE INSERT ON answer_value
WHEN NEW.withdrawn_by_answer_id IS NOT NULL
  OR NOT EXISTS (SELECT 1 FROM answer a
                 WHERE a.id = NEW.answer_id AND a.verb = 'confirm'
                   AND NOT EXISTS (SELECT 1 FROM answer u WHERE u.undoes_answer_id = a.id))
BEGIN
  SELECT RAISE(ABORT, 'answer_value is written in force, by an answer standing');
END;

CREATE TRIGGER answer_value_no_delete
BEFORE DELETE ON answer_value
BEGIN
  SELECT RAISE(ABORT, 'answer_value is never removed');
END;

CREATE TRIGGER answer_value_no_replace
BEFORE INSERT ON answer_value
WHEN EXISTS (SELECT 1 FROM answer_value
             WHERE id = NEW.id
                OR (answer_id = NEW.answer_id AND source_item_id = NEW.source_item_id
                    AND field = NEW.field AND ordinal = NEW.ordinal))
BEGIN
  SELECT RAISE(ABORT, 'answer_value is never removed');
END;

-- [corrections|durable] A redirect an answer set: a row no file was on any
-- more, led to the row its files went to. Immutable. The undo of a yes
-- clears the yes's where they still lead there; an undo's stay. A release
-- an answer led on is the answer's, not a merge made by hand.
CREATE TABLE answer_redirect (
  id           INTEGER PRIMARY KEY,
  answer_id    INTEGER NOT NULL REFERENCES answer(id),
  entity       TEXT NOT NULL CHECK (length(entity) > 0),   -- 'recording', 'track_appearance' or 'release'
  row_id       INTEGER NOT NULL,
  to_id        INTEGER NOT NULL,
  rev          INTEGER NOT NULL,
  UNIQUE (answer_id, entity, row_id)
) STRICT;

CREATE INDEX answer_redirect_row ON answer_redirect (entity, row_id);

CREATE TRIGGER answer_redirect_no_update
BEFORE UPDATE ON answer_redirect
BEGIN
  SELECT RAISE(ABORT, 'answer_redirect is immutable');
END;

CREATE TRIGGER answer_redirect_no_delete
BEFORE DELETE ON answer_redirect
BEGIN
  SELECT RAISE(ABORT, 'answer_redirect is immutable');
END;

CREATE TRIGGER answer_redirect_no_replace
BEFORE INSERT ON answer_redirect
WHEN EXISTS (SELECT 1 FROM answer_redirect
             WHERE id = NEW.id
                OR (answer_id = NEW.answer_id AND entity = NEW.entity AND row_id = NEW.row_id))
BEGIN
  SELECT RAISE(ABORT, 'answer_redirect is immutable');
END;

-- [corrections|durable] What a yes puts together: the releases and the
-- release groups of its own files and of the files it joins them with, as
-- they were when it was said. Immutable. While the yes stands, the
-- projector counts rows joined by it as one, so a join a person
-- said yes to passes the guard on rows that carry their own input, and a
-- later retag of another such row does not.
CREATE TABLE answer_join (
  id           INTEGER PRIMARY KEY,
  answer_id    INTEGER NOT NULL REFERENCES answer(id),
  entity       TEXT NOT NULL CHECK (length(entity) > 0),   -- 'release' or 'release_group'
  row_id       INTEGER NOT NULL,
  album_mbid   TEXT NOT NULL,                     -- the album the yes said its files are, by its release group
  rev          INTEGER NOT NULL,
  UNIQUE (answer_id, entity, row_id)
) STRICT;

CREATE TRIGGER answer_join_no_update
BEFORE UPDATE ON answer_join
BEGIN
  SELECT RAISE(ABORT, 'answer_join is immutable');
END;

CREATE TRIGGER answer_join_no_delete
BEFORE DELETE ON answer_join
BEGIN
  SELECT RAISE(ABORT, 'answer_join is immutable');
END;

CREATE TRIGGER answer_join_no_replace
BEFORE INSERT ON answer_join
WHEN EXISTS (SELECT 1 FROM answer_join
             WHERE id = NEW.id
                OR (answer_id = NEW.answer_id AND entity = NEW.entity AND row_id = NEW.row_id))
BEGIN
  SELECT RAISE(ABORT, 'answer_join is immutable');
END;

-- [corrections|durable] That a yes rests on an earlier one: it took its
-- album's title or credit from files the earlier one gave them. Immutable.
-- Undoing the earlier undoes it too.
CREATE TABLE answer_dependency (
  id                      INTEGER PRIMARY KEY,
  answer_id               INTEGER NOT NULL REFERENCES answer(id),
  depends_on_answer_id    INTEGER NOT NULL REFERENCES answer(id),
  rev                     INTEGER NOT NULL,
  CHECK (answer_id <> depends_on_answer_id),
  UNIQUE (answer_id, depends_on_answer_id)
) STRICT;

CREATE INDEX answer_dependency_on ON answer_dependency (depends_on_answer_id);

CREATE TRIGGER answer_dependency_no_update
BEFORE UPDATE ON answer_dependency
BEGIN
  SELECT RAISE(ABORT, 'answer_dependency is immutable');
END;

CREATE TRIGGER answer_dependency_no_delete
BEFORE DELETE ON answer_dependency
BEGIN
  SELECT RAISE(ABORT, 'answer_dependency is immutable');
END;

CREATE TRIGGER answer_dependency_no_replace
BEFORE INSERT ON answer_dependency
WHEN EXISTS (SELECT 1 FROM answer_dependency
             WHERE id = NEW.id
                OR (answer_id = NEW.answer_id AND depends_on_answer_id = NEW.depends_on_answer_id))
BEGIN
  SELECT RAISE(ABORT, 'answer_dependency is immutable');
END;

-- Saying no: what an answer says no to, which outlives a rescan, a
-- re-resolve and a rebuild as a value does. See "Saying no" in
-- docs/internals/corrections.md.

-- [corrections|durable] One no an answer said: a candidate not to offer
-- some files again, files not to ask about, or two sets of files never to
-- put on one album. The only change a row takes is withdrawn_by_answer_id,
-- set once by the undo of the answer that said it.
CREATE TABLE answer_rule (
  id                        INTEGER PRIMARY KEY,
  answer_id                 INTEGER NOT NULL UNIQUE REFERENCES answer(id),
  kind                      TEXT NOT NULL CHECK (kind IN ('not_candidate', 'stop_asking', 'apart')),
  candidate                 TEXT,                     -- for not_candidate: the release group or release
  about                     TEXT,                     -- stop_asking: the kind of proposal, 'album' or 'edition' (NULL for any); apart: what the sides are kept apart as, 'album'
  withdrawn_by_answer_id    INTEGER REFERENCES answer(id),
  rev                       INTEGER NOT NULL,
  CHECK ((kind = 'not_candidate') = (candidate IS NOT NULL)),
  CHECK (kind <> 'not_candidate' OR about IS NULL)
) STRICT;

CREATE TRIGGER answer_rule_withdrawn_once
BEFORE UPDATE ON answer_rule
WHEN OLD.withdrawn_by_answer_id IS NOT NULL
  OR NEW.withdrawn_by_answer_id IS NULL
  OR NOT EXISTS (SELECT 1 FROM answer u
                 WHERE u.id = NEW.withdrawn_by_answer_id AND u.undoes_answer_id = OLD.answer_id)
  OR NEW.id IS NOT OLD.id
  OR NEW.answer_id IS NOT OLD.answer_id
  OR NEW.kind IS NOT OLD.kind
  OR NEW.candidate IS NOT OLD.candidate
  OR NEW.about IS NOT OLD.about
BEGIN
  SELECT RAISE(ABORT, 'answer_rule changes only by being withdrawn, once');
END;

-- A no is written in force, by an answer that says no and is not undone.
CREATE TRIGGER answer_rule_written_in_force
BEFORE INSERT ON answer_rule
WHEN NEW.withdrawn_by_answer_id IS NOT NULL
  OR NOT EXISTS (SELECT 1 FROM answer a
                 WHERE a.id = NEW.answer_id AND a.verb IN ('decline', 'stop_asking', 'apart')
                   AND NOT EXISTS (SELECT 1 FROM answer u WHERE u.undoes_answer_id = a.id))
BEGIN
  SELECT RAISE(ABORT, 'answer_rule is written in force, by a no standing');
END;

CREATE TRIGGER answer_rule_no_delete
BEFORE DELETE ON answer_rule
BEGIN
  SELECT RAISE(ABORT, 'answer_rule is never removed');
END;

CREATE TRIGGER answer_rule_no_replace
BEFORE INSERT ON answer_rule
WHEN EXISTS (SELECT 1 FROM answer_rule WHERE id = NEW.id OR answer_id = NEW.answer_id)
BEGIN
  SELECT RAISE(ABORT, 'answer_rule is never removed');
END;

-- [corrections|durable] The files a no is about, by source item. For
-- `apart`, each on side 0 or 1. Immutable.
CREATE TABLE answer_rule_item (
  id                INTEGER PRIMARY KEY,
  rule_id           INTEGER NOT NULL REFERENCES answer_rule(id),
  source_item_id    INTEGER NOT NULL REFERENCES source_item(id),
  side              INTEGER NOT NULL CHECK (side IN (0, 1)),
  rev               INTEGER NOT NULL,
  UNIQUE (rule_id, source_item_id)
) STRICT;

CREATE INDEX answer_rule_item_item ON answer_rule_item (source_item_id);

CREATE TRIGGER answer_rule_item_no_update
BEFORE UPDATE ON answer_rule_item
BEGIN
  SELECT RAISE(ABORT, 'answer_rule_item is immutable');
END;

CREATE TRIGGER answer_rule_item_no_delete
BEFORE DELETE ON answer_rule_item
BEGIN
  SELECT RAISE(ABORT, 'answer_rule_item is immutable');
END;

CREATE TRIGGER answer_rule_item_no_replace
BEFORE INSERT ON answer_rule_item
WHEN EXISTS (SELECT 1 FROM answer_rule_item
             WHERE id = NEW.id OR (rule_id = NEW.rule_id AND source_item_id = NEW.source_item_id))
BEGIN
  SELECT RAISE(ABORT, 'answer_rule_item is immutable');
END;
