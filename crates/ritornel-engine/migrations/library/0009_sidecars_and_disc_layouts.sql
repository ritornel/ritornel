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
