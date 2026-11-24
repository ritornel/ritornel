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
