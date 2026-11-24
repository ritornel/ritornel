-- Answers: what was said to a proposal, and the tag values a yes puts on
-- files. See "Answers" in docs/internals/corrections.md.
--
-- The ledger is append-only. An undo is an answer that names the one it
-- undoes, and the only change a value ever takes is being withdrawn, once.
-- A verb, a kind, a field and an origin are names the code gives, left
-- open here so that later kinds of answer add to them without rewriting a
-- row.

-- [corrections|durable] One answer: a yes to a proposal's candidate, or
-- the undo of an earlier answer. Immutable.
CREATE TABLE answer (
  id                      INTEGER PRIMARY KEY,
  public_id               BLOB NOT NULL UNIQUE CHECK (length(public_id) = 16),
  verb                    TEXT NOT NULL CHECK (length(verb) > 0),               -- 'confirm' or 'undo'
  kind                    TEXT CHECK (length(kind) > 0),                        -- what a confirm was about: 'album'; NULL for an undo
  proposal_public_id      BLOB CHECK (proposal_public_id IS NULL OR length(proposal_public_id) = 16),
  proposal_fingerprint    TEXT,                                                 -- the proposal as it was shown
  candidate               TEXT,                                                 -- the release group a confirm said yes to
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
