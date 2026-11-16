-- =============================================================================
-- Ritornel library.db, migration 0001: every table the library has in M1's
-- first slice. Catalog, evidence, files, history from our own player, ratings,
-- and the bookkeeping that says whose library this is and what changed.
-- The hub holds the one library.db and is its one writer. The tables are
-- docs/design/schema/library.sql, whose summary, value lists and flags are in
-- docs/design/schema.md.
--
-- This file is never edited once it has shipped: the runner keeps a checksum
-- of every migration it applied and refuses to start if one has changed. A
-- change to the schema is a new numbered file.
--
-- Conventions
--   * Every table is STRICT with one integer primary key. Integer keys never
--     leave this file; anything addressed from outside has a 16-byte UUIDv7
--     `public_id` (README conventions; review 2.4). `play_event.public_id` is
--     the one 16-byte ID that is computed and is not a UUIDv7.
--   * `rev` is the library revision at which the row last changed, on every
--     table (server.md decision 22).
--   * The tag line above each table is `-- [area|class]`, where class is
--     durable, derived (rebuildable, safe to drop) or bookkeeping. check.sh
--     reads these tags, so they are the single list of what is durable.
--   * Two things are exempt from STRICT, `rev` and the tags' rules: the
--     migration ledger, and the text search index, an FTS5 virtual table
--     (server.md "Testing and CI").
--   * A column named `*_pid` refers to a row in server.db by its public ID, which
--     no foreign key can reach. A reference to a row in this file is an integer
--     `*_id` with a foreign key, except `entity_id` on `identity_decision` and
--     `metadata_edit`, which names a row of the kind beside it.
--   * Columns named `display_*` are the projector's cache of ranked claims and
--     edits and are written by nothing else (identity.md decision 11).
--   * `*_ms` is Unix milliseconds in UTC, or a duration in milliseconds. Flags
--     are INTEGER 0 or 1.
--   * Closed value lists are CHECK constraints, and schema.md "Value lists" is
--     the one place they are written out. A list a later source will extend
--     (source and item kinds, namespaces, fields, job kinds) stays a comment,
--     because SQLite cannot alter a CHECK without rebuilding the table.
-- =============================================================================


-- -----------------------------------------------------------------------------
-- Bookkeeping: whose library this is, who may write it, and what changed
-- -----------------------------------------------------------------------------

-- [meta|bookkeeping] One applied migration, written by the migration runner outside any library revision. server.md decision 19.
CREATE TABLE schema_migration (
  version        INTEGER PRIMARY KEY,
  name           TEXT NOT NULL,
  checksum       BLOB NOT NULL,     -- hash of the migration file; the server refuses to start if a shipped one changed
  applied_at_ms  INTEGER NOT NULL
) STRICT;

-- [meta|durable] One server this library knows: the registry that the hub pointer, roots and player endpoints point at. server.md "Which file holds what"; linking.md decision 1.
CREATE TABLE server (
  id                INTEGER PRIMARY KEY,
  public_id         BLOB NOT NULL UNIQUE CHECK (length(public_id) = 16),   -- the server's own ID from its server.db
  name              TEXT NOT NULL,                                         -- untrusted display text
  created_at_ms     INTEGER NOT NULL,
  rev               INTEGER NOT NULL
) STRICT;

-- [meta|durable] The single row saying which library this is and which server may write it. linking.md decision 2; server.md decision 15.
CREATE TABLE library_meta (
  id                INTEGER PRIMARY KEY CHECK (id = 1),
  library_id        BLOB NOT NULL CHECK (length(library_id) = 16),
  hub_server_id     INTEGER REFERENCES server(id),   -- NULL means released: no server may write
  hub_epoch         INTEGER NOT NULL,                -- raised by every release and adopt
  rev               INTEGER NOT NULL,                -- the revision counter itself: this row changes in every write transaction
  created_at_ms     INTEGER NOT NULL
) STRICT;

-- [meta|durable] One library write transaction: which call made it, for whom, and when. server.md decision 22.
CREATE TABLE library_revision (
  rev                INTEGER PRIMARY KEY,
  command_id         BLOB CHECK (length(command_id) = 16),          -- NULL for writes with no receipt, such as a history ingest
  method             TEXT NOT NULL,                                 -- the contract call or internal writer, for example 'scan.step'
  client_session_pid BLOB CHECK (length(client_session_pid) = 16),  -- session public ID in the handling server's server.db; NULL for internal work
  client_kind        TEXT CHECK (client_kind IN ('person', 'agent', 'server')),   -- the session's subject kind
  committed_at_ms    INTEGER NOT NULL
) STRICT;

-- [meta|durable] The receipt of one library command, committed with its change. Kept at least 24 hours, then prunable. server.md decision 21, flag 2 and "An effect, a job and a lease"; contract.md decision 13.
CREATE TABLE library_command (
  id                   INTEGER PRIMARY KEY,
  command_id           BLOB NOT NULL UNIQUE CHECK (length(command_id) = 16),   -- minted by the client; a job step derives its 16 bytes from the job's ID and the step number
  request_fingerprint  BLOB NOT NULL,                                          -- same ID with another fingerprint is refused
  method               TEXT NOT NULL,
  result_json          TEXT NOT NULL,                                          -- the result to replay on a retry
  rev                  INTEGER NOT NULL REFERENCES library_revision(rev),
  created_at_ms        INTEGER NOT NULL
) STRICT;

-- [meta|durable] One setting that follows the library to a new hub, such as the play rule version or the default presentation. server.md decision 26.
CREATE TABLE library_setting (
  id                INTEGER PRIMARY KEY,
  key               TEXT NOT NULL UNIQUE,
  value_json        TEXT NOT NULL,
  rev               INTEGER NOT NULL
) STRICT;


-- -----------------------------------------------------------------------------
-- Catalog: what the music is
-- -----------------------------------------------------------------------------

-- [catalog|durable] One artist entity. Merged by redirect. identity.md decisions 10 and 13.
CREATE TABLE artist (
  id                 INTEGER PRIMARY KEY,
  public_id          BLOB NOT NULL UNIQUE CHECK (length(public_id) = 16),
  redirect_to_id     INTEGER REFERENCES artist(id) CHECK (redirect_to_id <> id),   -- set by a merge, cleared by its undo
  canonical_id       INTEGER NOT NULL REFERENCES artist(id),                       -- cached end of the redirect chain; equals id when not merged
  display_name       TEXT,
  display_sort_name  TEXT,
  match_name         TEXT,                                                         -- mechanically normalised name, for exact binding; never unique
  rev                INTEGER NOT NULL,
  CHECK ((redirect_to_id IS NULL) = (canonical_id = id))
) STRICT;

-- [catalog|durable] One credit: an ordered list of artists with join phrases, as written. An immutable value that display columns point at. plan.md "Catalog"; identity.md "Displayed metadata".
CREATE TABLE artist_credit (
  id                INTEGER PRIMARY KEY,
  display_text      TEXT NOT NULL,   -- the whole credit as one string
  rev               INTEGER NOT NULL
) STRICT;

-- [catalog|durable] One artist at one position in a credit. review 16 step 1 (the plan folds it into artist_credit).
CREATE TABLE artist_credit_name (
  id                INTEGER PRIMARY KEY,
  artist_credit_id  INTEGER NOT NULL REFERENCES artist_credit(id),
  position          INTEGER NOT NULL,
  artist_id         INTEGER NOT NULL REFERENCES artist(id),
  credited_name     TEXT NOT NULL,              -- the name as written on this credit
  join_phrase       TEXT NOT NULL DEFAULT '',   -- text between this name and the next, such as ' & '
  rev               INTEGER NOT NULL,
  UNIQUE (artist_credit_id, position)
) STRICT;

-- [catalog|durable] One performance or mix. Merged by redirect, never by rewriting what points at it. identity.md decision 10; review 5.5.
CREATE TABLE recording (
  id                        INTEGER PRIMARY KEY,
  public_id                 BLOB NOT NULL UNIQUE CHECK (length(public_id) = 16),
  redirect_to_id            INTEGER REFERENCES recording(id) CHECK (redirect_to_id <> id),
  canonical_id              INTEGER NOT NULL REFERENCES recording(id),   -- a redirected recording is never a resolver candidate
  display_title             TEXT,
  display_artist_credit_id  INTEGER REFERENCES artist_credit(id),
  rev                       INTEGER NOT NULL,
  CHECK ((redirect_to_id IS NULL) = (canonical_id = id))
) STRICT;

-- [catalog|durable] The versions of one album, with the owner's choice of how to show them, and the two facts an artist's page files and orders its records by: what kind of record it is and when it first came out. Grouping is presentation only. identity.md decisions 14 and 15, "Release groups".
CREATE TABLE release_group (
  id                            INTEGER PRIMARY KEY,
  public_id                     BLOB NOT NULL UNIQUE CHECK (length(public_id) = 16),
  presentation                  TEXT CHECK (presentation IN ('collapsed', 'expanded')),   -- durable, the owner's; NULL follows the library default
  preferred_release_id          INTEGER REFERENCES release(id),                           -- durable, the owner's; NULL means automatic
  display_release_id            INTEGER REFERENCES release(id),                           -- derived cache: the release a collapsed tile shows, after the automatic rule
  display_primary_type          TEXT CHECK (display_primary_type IN ('album', 'single', 'ep', 'broadcast', 'other')),   -- what kind of record it is, as MusicBrainz's primary type and as a release-type tag writes it; NULL is unknown, which the interface files with the albums
  display_secondary_types_json  TEXT NOT NULL DEFAULT '[]',                               -- the marks beside the kind: a sorted JSON array of none or more of MusicBrainz's secondary types, 'compilation', 'live', 'soundtrack', 'remix', 'dj-mix', 'mixtape/street', 'demo', 'spokenword', 'interview', 'audiobook', 'audio drama', 'field recording'
  display_first_date            TEXT,                                                     -- when the record first came out, whichever version that was: 'YYYY', 'YYYY-MM' or 'YYYY-MM-DD', as precise as it is known. An original-date claim or the owner's edit, else the earliest date among the group's releases
  display_first_year            INTEGER,                                                  -- the year of display_first_date; NULL when no version has a date
  rev                           INTEGER NOT NULL
) STRICT;

-- [catalog|durable] One album as published. Merged by redirect. identity.md decisions 10, 14 and 15.
CREATE TABLE release (
  id                        INTEGER PRIMARY KEY,
  public_id                 BLOB NOT NULL UNIQUE CHECK (length(public_id) = 16),
  chosen_release_group_id   INTEGER REFERENCES release_group(id),            -- durable, the owner's choice of group; NULL means automatic
  release_group_id          INTEGER NOT NULL REFERENCES release_group(id),   -- derived cache: the group in effect. The choice if set, else a shared release-group MBID, else shared recordings
  redirect_to_id            INTEGER REFERENCES release(id) CHECK (redirect_to_id <> id),
  canonical_id              INTEGER NOT NULL REFERENCES release(id),
  display_title             TEXT,
  display_artist_credit_id  INTEGER REFERENCES artist_credit(id),
  display_date              TEXT,                                            -- 'YYYY', 'YYYY-MM' or 'YYYY-MM-DD', as precise as the claims are
  display_year              INTEGER,
  display_picture_id        INTEGER REFERENCES picture(id),                  -- the cover; its public ID is the contract's artId
  rev                       INTEGER NOT NULL,
  CHECK ((redirect_to_id IS NULL) = (canonical_id = id))
) STRICT;

-- [catalog|durable] One recording at one position on a release; a loose file has no release. The unit of Songs rows, queue entries and sync membership. Redirected when its release is merged, so queue entries follow. identity.md decision 1 and "Merge, split and never-link"; playback.md "The queue".
CREATE TABLE track_appearance (
  id                        INTEGER PRIMARY KEY,
  public_id                 BLOB NOT NULL UNIQUE CHECK (length(public_id) = 16),
  recording_id              INTEGER NOT NULL REFERENCES recording(id),   -- fixed at creation; read through recording.canonical_id
  release_id                INTEGER REFERENCES release(id),
  redirect_to_id            INTEGER REFERENCES track_appearance(id) CHECK (redirect_to_id <> id),   -- playback.md follows this at prepare time
  canonical_id              INTEGER NOT NULL REFERENCES track_appearance(id),
  display_title             TEXT,
  display_artist_credit_id  INTEGER REFERENCES artist_credit(id),
  display_disc_number       INTEGER,
  display_track_number      INTEGER,
  display_duration_ms       INTEGER,
  display_added_at_ms       INTEGER,                                     -- first seen time of its oldest source item
  rev                       INTEGER NOT NULL,
  UNIQUE (id, recording_id),                                             -- the target of assignment's composite foreign key
  CHECK ((redirect_to_id IS NULL) = (canonical_id = id))
) STRICT;


-- -----------------------------------------------------------------------------
-- Evidence: why we believe it
-- -----------------------------------------------------------------------------

-- [evidence|durable] One class of metadata source and its rank. Higher precedence wins. identity.md "Displayed metadata"; review 5.7.
CREATE TABLE metadata_source (
  id                INTEGER PRIMARY KEY,
  kind              TEXT NOT NULL UNIQUE,   -- 'user', 'musicbrainz', 'beets', 'file_tags', 'device', 'inferred'
  precedence        INTEGER NOT NULL,
  rev               INTEGER NOT NULL
) STRICT;

-- [files|durable] One named root: a folder of masters, or a server's built-in cache. The root names its server, so a hub move re-points this one row; the absolute path is in that server's server.db. identity.md decision 16; server.md decisions 12 and 13.
-- A master root is hands-off unless the owner turns on one of the two manage flags, and a cache root never has them. Nothing reads the flags in M1. file-management.md decisions 1 and 3.
CREATE TABLE library_root (
  id                INTEGER PRIMARY KEY,
  public_id         BLOB NOT NULL UNIQUE CHECK (length(public_id) = 16),
  server_id         INTEGER NOT NULL REFERENCES server(id),             -- the server the root is on
  name              TEXT NOT NULL,                                      -- that server's server.db `root` maps this name to an absolute path
  kind              TEXT NOT NULL CHECK (kind IN ('master', 'cache')),  -- a cache root is never scanned; its first rows arrive with M3
  manage_tags       INTEGER NOT NULL DEFAULT 0 CHECK (manage_tags IN (0, 1)),     -- 1: the owner lets the server write tags into the files here
  manage_layout     INTEGER NOT NULL DEFAULT 0 CHECK (manage_layout IN (0, 1)),   -- 1: the owner lets the server rename and move the files here
  created_at_ms     INTEGER NOT NULL,
  rev               INTEGER NOT NULL,
  UNIQUE (server_id, name),
  CHECK (kind = 'master' OR (manage_tags = 0 AND manage_layout = 0))
) STRICT;

-- [evidence|durable] One place items come from. A scanned folder is the whole of one master root and references it. identity.md decision 16 and "Servers".
CREATE TABLE source_instance (
  id                INTEGER PRIMARY KEY,
  public_id         BLOB NOT NULL UNIQUE CHECK (length(public_id) = 16),   -- the contract's sourceId
  kind              TEXT NOT NULL,                                         -- 'folder' in M1; later 'beets', 'device', 'lastfm', 'musicbrainz'
  name              TEXT NOT NULL,
  root_id           INTEGER UNIQUE REFERENCES library_root(id),            -- a folder's root, of kind 'master'; one folder per root
  created_at_ms     INTEGER NOT NULL,
  rev               INTEGER NOT NULL,
  CHECK ((kind = 'folder') = (root_id IS NOT NULL))
) STRICT;

-- [evidence|durable] One thing a source reported: for a scan, one file. It holds revisions, claims, assignments, exclusions and raw history, and for a file it is also the one place its path is stored. identity.md decision 4 and "Rescans"; review 5.4.
-- When the audio at a path is replaced by a different song, the old item is retired with its assignments and history, and a new item takes the path.
-- An item never turns into another song. A re-rip that lands on the same recording keeps its item and gets a new revision.
CREATE TABLE source_item (
  id                    INTEGER PRIMARY KEY,
  public_id             BLOB NOT NULL UNIQUE CHECK (length(public_id) = 16),
  source_instance_id    INTEGER NOT NULL REFERENCES source_instance(id),   -- for a file, the folder, which names the root
  item_kind             TEXT NOT NULL,                              -- 'file' in M1; later 'device_object', 'scrobble_subject'
  provider_key          TEXT NOT NULL,                              -- for a file, the path relative to the root; re-keyed when the file moves
  current_revision_id   INTEGER,                                    -- one of this item's own revisions, by the composite key below
  current_recording_id  INTEGER REFERENCES recording(id),           -- derived cache of the current assignment, for reads
  size_bytes            INTEGER,                                    -- files: the whole file, tags included
  mtime_ms              INTEGER,                                    -- files: unchanged size and time let a rescan skip the file
  missing_since_ms      INTEGER,                                    -- files: set when a scan of a reachable root no longer finds the file, cleared if it returns
  retired_at_ms         INTEGER,                                    -- set when a different song took the path; the item keeps its rows
  first_seen_at_ms      INTEGER NOT NULL,                           -- the resolver processes items in this order
  rev                   INTEGER NOT NULL,
  FOREIGN KEY (id, current_revision_id) REFERENCES source_item_revision (source_item_id, id),
  CHECK (item_kind = 'file' OR (size_bytes IS NULL AND mtime_ms IS NULL AND missing_since_ms IS NULL))
) STRICT;

-- A key is unique only among items that are not retired: a retired item keeps its key while a new item takes the path. identity.md decision 4.
CREATE UNIQUE INDEX source_item_live_key
  ON source_item (source_instance_id, item_kind, provider_key)
  WHERE retired_at_ms IS NULL;

-- [evidence|durable] One raw payload an item has had, kept verbatim inline and deduplicated by hash. Immutable. identity.md "Scanning one file" step 4; review 5.4.
CREATE TABLE source_item_revision (
  id                INTEGER PRIMARY KEY,
  source_item_id    INTEGER NOT NULL REFERENCES source_item(id),
  revision_no       INTEGER NOT NULL,                    -- counts from 1 per item; a listen names (item, revision_no)
  payload_hash      BLOB NOT NULL,
  payload_json      TEXT NOT NULL,                       -- tags as read, path, size, times, hash and stream properties, or the read error
  audio_asset_id    INTEGER REFERENCES audio_asset(id),  -- the audio this revision held; NULL if the file could not be read as audio
  observed_at_ms    INTEGER NOT NULL,
  rev               INTEGER NOT NULL,
  UNIQUE (source_item_id, revision_no),
  UNIQUE (source_item_id, payload_hash),
  UNIQUE (source_item_id, id)                          -- lets an item and its claims name a revision only together with its item
) STRICT;

-- A stored payload is never changed or removed: it is the only copy of what the source said.
CREATE TRIGGER source_item_revision_no_update
BEFORE UPDATE ON source_item_revision
BEGIN
  SELECT RAISE(ABORT, 'source_item_revision is immutable');
END;

CREATE TRIGGER source_item_revision_no_delete
BEFORE DELETE ON source_item_revision
BEGIN
  SELECT RAISE(ABORT, 'source_item_revision is immutable');
END;

-- [evidence|derived] One external identifier a source item stated, extracted from one of its revisions. Rebuilt by the extractor from stored payloads, and it reaches an entity only through the item's current assignment. No uniqueness on (namespace, value): one ISRC or AcoustID maps to several recordings. identity.md decisions 3 and 9.
CREATE TABLE identifier_claim (
  id                  INTEGER PRIMARY KEY,
  source_item_id      INTEGER NOT NULL REFERENCES source_item(id),            -- the item that made the claim
  revision_id         INTEGER NOT NULL,                                       -- the payload it was extracted from, one of that item's own
  extractor_version   INTEGER NOT NULL,
  scope               TEXT NOT NULL CHECK (scope IN ('recording', 'track_appearance', 'release', 'release_group', 'artist')),   -- the kind of entity described, never a pointer to one
  namespace           TEXT NOT NULL,                                          -- 'mb_recording', 'mb_release', 'mb_release_group', 'mb_artist', 'isrc', 'acoustid', ...
  value               TEXT NOT NULL,
  ordinal             INTEGER NOT NULL DEFAULT 0,                             -- position among the values of a multi-valued tag
  metadata_source_id  INTEGER NOT NULL REFERENCES metadata_source(id),
  rev                 INTEGER NOT NULL,
  FOREIGN KEY (source_item_id, revision_id) REFERENCES source_item_revision (source_item_id, id)
) STRICT;

-- [evidence|derived] One tag value a source item stated, extracted from one of its revisions. Rebuilt by the extractor. Displayed metadata is the projection of these and of metadata_edit, by rank. identity.md decisions 3 and 11.
CREATE TABLE metadata_claim (
  id                  INTEGER PRIMARY KEY,
  source_item_id      INTEGER NOT NULL REFERENCES source_item(id),
  revision_id         INTEGER NOT NULL,
  extractor_version   INTEGER NOT NULL,
  scope               TEXT NOT NULL CHECK (scope IN ('recording', 'track_appearance', 'release', 'release_group', 'artist')),
  field               TEXT NOT NULL,                                          -- 'title', 'artist', 'album', 'disc_number', 'track_number', 'duration_ms', 'date', ...; on a release group 'primary_type', 'secondary_type' and 'original_date'
  value               TEXT NOT NULL,                                          -- as claimed, unparsed
  ordinal             INTEGER NOT NULL DEFAULT 0,
  metadata_source_id  INTEGER NOT NULL REFERENCES metadata_source(id),        -- 'file_tags', or 'inferred' for file and folder names
  rev                 INTEGER NOT NULL,
  FOREIGN KEY (source_item_id, revision_id) REFERENCES source_item_revision (source_item_id, id)
) STRICT;

-- [evidence|durable] One edit the owner made to one field of one entity. Ranked above every claim, and the only copy: no extractor can rebuild it. Removing an edit stamps it withdrawn, so the next source shows through. identity.md decisions 3 and 11, "Displayed metadata".
CREATE TABLE metadata_edit (
  id                 INTEGER PRIMARY KEY,
  entity_kind        TEXT NOT NULL CHECK (entity_kind IN ('recording', 'track_appearance', 'release', 'release_group', 'artist')),   -- a release group's kind and first year are edited on the group
  entity_id          INTEGER NOT NULL,                              -- the row of that kind; read through its canonical_id after a merge, where the kind has one
  field              TEXT NOT NULL,                                 -- the same names as metadata_claim.field
  value              TEXT NOT NULL,
  ordinal            INTEGER NOT NULL DEFAULT 0,
  actor_kind         TEXT NOT NULL CHECK (actor_kind IN ('person', 'agent')),
  actor_session_pid  BLOB CHECK (length(actor_session_pid) = 16),
  edited_at_ms       INTEGER NOT NULL,
  withdrawn_at_ms    INTEGER,                                       -- NULL while the edit is in force
  rev                INTEGER NOT NULL
) STRICT;

-- One edit in force per entity and field value. A changed edit withdraws the old row and inserts a new one.
CREATE UNIQUE INDEX metadata_edit_in_force
  ON metadata_edit (entity_kind, entity_id, field, ordinal)
  WHERE withdrawn_at_ms IS NULL;

-- [evidence|durable] Why one binding, merge or undo was made, with the full score shape. Immutable. identity.md decision 8 and "Resolver policy 1"; review 5.4, 5.10.
CREATE TABLE identity_decision (
  id                   INTEGER PRIMARY KEY,
  action               TEXT NOT NULL CHECK (action IN ('bind', 'merge', 'unmerge')),
  source_item_id       INTEGER REFERENCES source_item(id),   -- bind: the item decided
  entity_kind          TEXT CHECK (entity_kind IN ('recording', 'artist', 'release', 'track_appearance')),   -- merge and unmerge: what was redirected
  entity_id            INTEGER,                              -- the row whose redirect was set or cleared
  target_entity_id     INTEGER,                              -- merge: where it now points
  method               TEXT NOT NULL CHECK (method IN ('user', 'same_audio', 'identifier', 'tags_duration', 'new')),
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

-- [evidence|durable] One binding of a source item to a recording and, when known, a track appearance on it. One table holds both halves, so they can never disagree. Insert-only apart from the supersession stamp. identity.md decision 2; review 5.10.
CREATE TABLE assignment (
  id                INTEGER PRIMARY KEY,
  source_item_id    INTEGER NOT NULL REFERENCES source_item(id),
  recording_id      INTEGER NOT NULL REFERENCES recording(id),   -- with an appearance, that appearance's own recording; read through recording.canonical_id
  appearance_id     INTEGER,                                     -- NULL when no appearance is known
  decision_id       INTEGER NOT NULL REFERENCES identity_decision(id),
  effective_from_ms INTEGER NOT NULL,
  superseded_at_ms  INTEGER,                                     -- NULL marks the current row
  superseded_by_id  INTEGER REFERENCES assignment(id),
  rev               INTEGER NOT NULL,
  FOREIGN KEY (appearance_id, recording_id) REFERENCES track_appearance (id, recording_id)
) STRICT;

-- One current row per source item. A retired item keeps its current row.
CREATE UNIQUE INDEX assignment_current
  ON assignment (source_item_id)
  WHERE superseded_at_ms IS NULL;

-- The binding columns can never change, a supersession is set once, and no row is deleted.
CREATE TRIGGER assignment_no_rebind
BEFORE UPDATE ON assignment
WHEN NEW.source_item_id IS NOT OLD.source_item_id
  OR NEW.recording_id IS NOT OLD.recording_id
  OR NEW.appearance_id IS NOT OLD.appearance_id
  OR NEW.decision_id IS NOT OLD.decision_id
  OR NEW.effective_from_ms IS NOT OLD.effective_from_ms
  OR (OLD.superseded_at_ms IS NOT NULL AND NEW.superseded_at_ms IS NOT OLD.superseded_at_ms)
  OR (OLD.superseded_by_id IS NOT NULL AND NEW.superseded_by_id IS NOT OLD.superseded_by_id)
BEGIN
  SELECT RAISE(ABORT, 'assignment is insert-only: supersede the row, never update the binding');
END;

CREATE TRIGGER assignment_no_delete
BEFORE DELETE ON assignment
BEGIN
  SELECT RAISE(ABORT, 'assignment is insert-only: rows are never deleted');
END;

-- [evidence|durable] One "this item is not that recording" assertion, read by every resolver run. identity.md decision 10 and "Merge, split and never-link"; review 5.4.
CREATE TABLE identity_exclusion (
  id                INTEGER PRIMARY KEY,
  source_item_id    INTEGER NOT NULL REFERENCES source_item(id),
  recording_id      INTEGER NOT NULL REFERENCES recording(id),
  actor_kind        TEXT NOT NULL CHECK (actor_kind IN ('person', 'agent')),
  actor_session_pid BLOB CHECK (length(actor_session_pid) = 16),
  reason            TEXT,
  created_at_ms     INTEGER NOT NULL,
  rev               INTEGER NOT NULL
) STRICT;

-- [evidence|durable] One pair of differing normalised names that a trusted link showed to be the same thing. A resolver input. identity.md decision 12 and "The proof"; review 5.9.
CREATE TABLE alias_observation (
  id                  INTEGER PRIMARY KEY,
  scope               TEXT NOT NULL CHECK (scope IN ('artist', 'title', 'release')),
  name_a              TEXT NOT NULL,   -- normalised; the pair is unordered, stored with name_a < name_b
  name_b              TEXT NOT NULL,
  normalizer_version  INTEGER NOT NULL,
  decision_id         INTEGER NOT NULL REFERENCES identity_decision(id),   -- the same-audio, identifier or user decision that linked them
  observed_at_ms      INTEGER NOT NULL,
  rev                 INTEGER NOT NULL,
  CHECK (name_a < name_b)
) STRICT;


-- -----------------------------------------------------------------------------
-- Files: where the audio is
-- -----------------------------------------------------------------------------

-- [files|durable] One distinct piece of encoded audio, identified by a hash of the audio alone. identity.md decision 5.
CREATE TABLE audio_asset (
  id                 INTEGER PRIMARY KEY,
  public_id          BLOB NOT NULL UNIQUE CHECK (length(public_id) = 16),   -- a listen records which asset played
  hash_scheme        INTEGER NOT NULL,                                      -- versions the hash so it can be replaced; scheme 1 is the audio-hash spike's
  audio_hash         BLOB NOT NULL,                                         -- BLAKE3 over the scheme, codec, codec parameters and every audio packet's payload
  flac_md5           BLOB,                                                  -- STREAMINFO MD5 of the decoded audio when not zero; evidence only, never a key
  codec              TEXT NOT NULL,
  sample_rate_hz     INTEGER NOT NULL,
  channel_layout     TEXT NOT NULL,
  bit_depth          INTEGER,
  bitrate_kbps       INTEGER,
  lossless           INTEGER NOT NULL CHECK (lossless IN (0, 1)),
  duration_ms        INTEGER NOT NULL,                                      -- from the packets read
  rev                INTEGER NOT NULL,
  UNIQUE (hash_scheme, audio_hash)
) STRICT;

-- Where an asset's bytes are: an asset, a root and a relative path. A view, not a table, so a file's root and path are stored once, on its source item.
-- It covers scanned masters, which is every location M1 has. Derived copies in a cache root (M3) are not source items and will join it from their own table.
-- identity.md decision 16 and "Servers"; server.md decision 12.
CREATE VIEW asset_location AS
SELECT
  si.id                AS source_item_id,
  sr.audio_asset_id    AS audio_asset_id,
  src.root_id          AS root_id,
  si.provider_key      AS rel_path,
  si.size_bytes        AS size_bytes,
  si.mtime_ms          AS mtime_ms,
  si.missing_since_ms  AS missing_since_ms    -- the row stays while the file is missing; readers filter on it
FROM source_item si
JOIN source_instance src ON src.id = si.source_instance_id
JOIN source_item_revision sr ON sr.id = si.current_revision_id
WHERE si.item_kind = 'file'
  AND si.retired_at_ms IS NULL
  AND src.root_id IS NOT NULL
  AND sr.audio_asset_id IS NOT NULL;

-- [files|durable] One distinct picture, known by the hash of its bytes: embedded in a file, or a folder image such as cover.jpg. The bytes stay in the files. identity.md "Scanning one file" step 4; contract.md decision 18.
CREATE TABLE picture (
  id                INTEGER PRIMARY KEY,
  public_id         BLOB NOT NULL UNIQUE CHECK (length(public_id) = 16),   -- the contract's artId; a changed image is a new row
  content_hash      BLOB NOT NULL UNIQUE,
  mime_type         TEXT NOT NULL,
  width             INTEGER,
  height            INTEGER,
  size_bytes        INTEGER NOT NULL,
  rev               INTEGER NOT NULL
) STRICT;

-- [files|durable] One picture that came with one revision of a file: which files carry a cover, and where to read it from. identity.md "Scanning one file" step 4.
CREATE TABLE revision_picture (
  id                INTEGER PRIMARY KEY,
  revision_id       INTEGER NOT NULL REFERENCES source_item_revision(id),
  picture_id        INTEGER NOT NULL REFERENCES picture(id),
  position          INTEGER NOT NULL,   -- order within the file; folder images follow the embedded ones
  source_kind       TEXT NOT NULL CHECK (source_kind IN ('embedded', 'folder_image')),
  file_name         TEXT,               -- folder image: its name beside the audio file, such as 'cover.jpg'
  picture_type      INTEGER,            -- embedded: the tag format's own type number; 3 is a front cover
  rev               INTEGER NOT NULL,
  UNIQUE (revision_id, position),
  CHECK ((source_kind = 'folder_image') = (file_name IS NOT NULL))
) STRICT;


-- -----------------------------------------------------------------------------
-- Search: derived
-- -----------------------------------------------------------------------------

-- [search|derived] The text search index over displayed title, artist and album: one row per track appearance, with the appearance's id as rowid. The projector writes it and `rebuild-display` rebuilds it. A virtual table, so it is exempt from STRICT and `rev`. identity.md "Displayed metadata"; contract.md "Large lists" step 1; server.md "Testing and CI".
-- The trigram tokenizer gives substring search that ignores case, and `remove_diacritics` makes it ignore accents (SQLite 3.45 or later). It cannot answer a query shorter than three characters; see schema.md flag 3.
CREATE VIRTUAL TABLE search_text USING fts5(
  title,
  artist,
  album,
  tokenize = 'trigram remove_diacritics 1'
);


-- -----------------------------------------------------------------------------
-- History, durable: what sources told us
-- -----------------------------------------------------------------------------

-- [history|durable] One person whose listening is recorded. history.md decision 2; review 6.3.
CREATE TABLE listener (
  id                INTEGER PRIMARY KEY,
  public_id         BLOB NOT NULL UNIQUE CHECK (length(public_id) = 16),
  name              TEXT NOT NULL,
  rev               INTEGER NOT NULL
) STRICT;

-- [history|durable] One place reports come from: a server's player, the library's manual endpoint, later a device log, an account at a service or an import. history.md "Observation kinds".
CREATE TABLE history_endpoint (
  id                INTEGER PRIMARY KEY,
  public_id         BLOB NOT NULL UNIQUE CHECK (length(public_id) = 16),
  kind              TEXT NOT NULL CHECK (kind IN ('player', 'device_log', 'service', 'import', 'manual')),   -- 'player' and 'manual' in M1
  server_id         INTEGER REFERENCES server(id),   -- a player's server
  name              TEXT NOT NULL,
  rev               INTEGER NOT NULL,
  CHECK (kind <> 'player' OR server_id IS NOT NULL)
) STRICT;

-- Each server's player is one endpoint, and the library has one manual endpoint for corrections.
CREATE UNIQUE INDEX history_endpoint_player ON history_endpoint (server_id) WHERE kind = 'player';
CREATE UNIQUE INDEX history_endpoint_manual ON history_endpoint (kind) WHERE kind = 'manual';

-- [history|durable] The envelope every raw record shares, whatever its kind: who reported it, about which source item, for which listener, and every clock that touched it. Immutable. It names the source item and never the recording. history.md decisions 1, 2, 5, 18, 19, 20 and "Observation kinds".
-- Each row has one row in the detail table of its kind. M1 creates two: observation_listen and observation_correction.
-- Device log lines and import records get no detail table here. Each arrives as a new table with its milestone (M3, the Last.fm side track), so no row recorded earlier is reshaped.
CREATE TABLE history_observation (
  id                  INTEGER PRIMARY KEY,
  endpoint_id         INTEGER NOT NULL REFERENCES history_endpoint(id),
  source_key          BLOB NOT NULL,                        -- made by the reporter and never from a clock of ours; a listen's is its 16-byte ID
  kind                TEXT NOT NULL CHECK (kind IN ('listen', 'device_log', 'import', 'correction')),
  source_item_id      INTEGER REFERENCES source_item(id),   -- the master behind the copy played, or the device file; NULL on a correction
  listener_id         INTEGER NOT NULL REFERENCES listener(id),   -- as claimed at ingest; on a correction, who made it

  -- Clocks. history.md decision 20.
  captured_at_utc_ms  INTEGER NOT NULL,                     -- the reporter's wall clock when it made or read the record
  received_at_utc_ms  INTEGER NOT NULL,                     -- the hub's clock at ingest
  sent_at_utc_ms      INTEGER,                              -- the sender's wall clock at this send; every send, retries included, is stamped afresh
  sent_mono_ms        INTEGER,                              -- the sender's monotonic clock at this send: one that keeps counting while the machine sleeps
  sent_boot_id        BLOB CHECK (length(sent_boot_id) = 16),   -- the sender's boot at this send; equal to a listen's own boot ID means its age is exact
  clock_skew_ms       INTEGER,                              -- sender minus hub, measured by the hub at the send

  -- Zone and local date, only what the source knew. Frozen. Assumed zones are worked out at derive time. history.md decisions 18 and 19.
  zone_name           TEXT,                                 -- IANA name
  zone_basis          TEXT NOT NULL CHECK (zone_basis IN ('captured', 'offset_only', 'none')),
  utc_offset_minutes  INTEGER,
  local_date          INTEGER,                              -- YYYYMMDD
  local_hour          INTEGER,

  payload_json        TEXT NOT NULL,                        -- the record verbatim
  rev                 INTEGER NOT NULL,
  UNIQUE (endpoint_id, source_key),
  CHECK (kind <> 'listen' OR source_item_id IS NOT NULL),
  CHECK (kind <> 'correction' OR source_item_id IS NULL),
  CHECK ((zone_basis = 'captured') = (zone_name IS NOT NULL)),
  CHECK (zone_basis <> 'offset_only' OR utc_offset_minutes IS NOT NULL)
) STRICT;

-- "Which plays does this source item touch?" history.md "Queries M1 must answer".
CREATE INDEX history_observation_source_item ON history_observation (source_item_id);

-- [history|durable] The detail of one listen: every field the player's staged row carries. Immutable. None of it can be backfilled. playback.md "Recording a listen"; history.md "What a listen contains".
CREATE TABLE observation_listen (
  observation_id        INTEGER PRIMARY KEY REFERENCES history_observation(id),
  played_on_server_id   INTEGER NOT NULL REFERENCES server(id),          -- the server whose player made the sound
  actor_session_pid     BLOB CHECK (length(actor_session_pid) = 16),     -- the client session that queued the track
  source_revision_no    INTEGER NOT NULL,                                -- which payload of the envelope's source item was heard
  queued_appearance_id  INTEGER REFERENCES track_appearance(id),         -- a hint for display only; the recording always comes from the source item
  audio_asset_id        INTEGER REFERENCES audio_asset(id),              -- the asset that played
  copy_kind             TEXT NOT NULL CHECK (copy_kind IN ('master', 'derived', 'stream_master', 'stream_transcode')),
  started_at_utc_ms     INTEGER NOT NULL,                                -- the playing server's wall clock; its zone is on the envelope
  ended_at_utc_ms       INTEGER NOT NULL,
  started_mono_ms       INTEGER NOT NULL,                                -- the playing server's monotonic clock when the track started
  boot_id               BLOB NOT NULL CHECK (length(boot_id) = 16),      -- the boot it started in
  start_reason          TEXT NOT NULL CHECK (start_reason IN ('user_pick', 'auto_advance', 'repeat', 'resume')),
  continues_source_key  BLOB,                                            -- the earlier listen on this endpoint that this one resumes
  start_position_ms     INTEGER NOT NULL,
  end_position_ms       INTEGER NOT NULL,
  max_position_ms       INTEGER NOT NULL,
  track_duration_ms     INTEGER NOT NULL,
  ms_played             INTEGER NOT NULL,                                -- audio actually rendered
  heard_ranges_json     TEXT NOT NULL,                                   -- merged ranges of the track that were heard, with no cap on how many
  seek_count            INTEGER NOT NULL,
  pause_count           INTEGER NOT NULL,
  end_reason            TEXT NOT NULL CHECK (end_reason IN ('finished', 'next', 'previous', 'jump', 'replaced', 'removed', 'stopped', 'error', 'shutdown', 'crash')),
  context_kind          TEXT NOT NULL CHECK (context_kind IN ('manual', 'album', 'playlist', 'query', 'agent')),
  context_ref           BLOB CHECK (length(context_ref) = 16),           -- public ID of the release, playlist, definition or agent session
  shuffle               INTEGER NOT NULL CHECK (shuffle IN (0, 1)),
  rev                   INTEGER NOT NULL
) STRICT;

-- [history|durable] The detail of one correction: its verb, and what it targets. A target is either a list of observations, in correction_target, or an endpoint with a time range, here. Immutable. history.md decision 16 and "Corrections".
CREATE TABLE observation_correction (
  observation_id      INTEGER PRIMARY KEY REFERENCES history_observation(id),
  verb                TEXT NOT NULL CHECK (verb IN ('exclude', 'set_listener', 'split', 'join', 'set_clock_offset', 'revoke')),
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
  CHECK (target_endpoint_id IS NOT NULL OR (target_from_utc_ms IS NULL AND target_to_utc_ms IS NULL))
) STRICT;

-- [history|durable] One observation that a correction lists as a target. Immutable. Corrections point at observations, never at play events. history.md decisions 3 and 16.
CREATE TABLE correction_target (
  id                     INTEGER PRIMARY KEY,
  correction_id          INTEGER NOT NULL REFERENCES observation_correction(observation_id),
  target_observation_id  INTEGER NOT NULL REFERENCES history_observation(id),   -- for 'revoke', the correction being undone
  rev                    INTEGER NOT NULL,
  UNIQUE (correction_id, target_observation_id)
) STRICT;

CREATE INDEX correction_target_target ON correction_target (target_observation_id);

-- A detail row belongs to an envelope of its own kind.
CREATE TRIGGER observation_listen_kind
BEFORE INSERT ON observation_listen
WHEN (SELECT kind FROM history_observation WHERE id = NEW.observation_id) IS NOT 'listen'
BEGIN
  SELECT RAISE(ABORT, 'observation_listen holds the detail of a listen and of nothing else');
END;

CREATE TRIGGER observation_correction_kind
BEFORE INSERT ON observation_correction
WHEN (SELECT kind FROM history_observation WHERE id = NEW.observation_id) IS NOT 'correction'
BEGIN
  SELECT RAISE(ABORT, 'observation_correction holds the detail of a correction and of nothing else');
END;

-- An observation is immutable in all its parts: the envelope, its detail row and a correction's targets. Undo is a 'revoke'.
CREATE TRIGGER history_observation_no_update
BEFORE UPDATE ON history_observation
BEGIN
  SELECT RAISE(ABORT, 'history_observation is immutable: record a correction instead');
END;

CREATE TRIGGER history_observation_no_delete
BEFORE DELETE ON history_observation
BEGIN
  SELECT RAISE(ABORT, 'history_observation is immutable: nothing is deleted');
END;

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

CREATE TRIGGER correction_target_no_update
BEFORE UPDATE ON correction_target
BEGIN
  SELECT RAISE(ABORT, 'correction_target is immutable: revoke the correction instead');
END;

CREATE TRIGGER correction_target_no_delete
BEFORE DELETE ON correction_target
BEGIN
  SELECT RAISE(ABORT, 'correction_target is immutable: nothing is deleted');
END;

-- [history|durable] The timezone a listener was in over a range of dates, used wherever a zone has to be assumed. history.md decision 18 and "Time".
CREATE TABLE listener_zone_period (
  id                INTEGER PRIMARY KEY,
  listener_id       INTEGER NOT NULL REFERENCES listener(id),
  zone_name         TEXT NOT NULL,   -- IANA name
  valid_from_ms     INTEGER,         -- NULL is open
  valid_to_ms       INTEGER,
  rev               INTEGER NOT NULL
) STRICT;

-- [history|durable] One thing said about how much a listener likes a song: a rating, a loved mark, or the taking away of either. Append-only. What a source reported names the source item it is about and never a recording; what was said in Ritornel names the recording. plan.md "History: what was listened to" and "Open questions"; history.md "Last.fm".
-- A report from an endpoint is evidence, and follows the rule raw history follows: it names the source item, and reaches a recording through that item's current assignment. So it follows a re-match, and it is kept while nothing matches. A rating made in Ritornel is a statement about the song: it names the recording, as a playlist entry will, and is read through recording.canonical_id after a merge.
-- The rating in force for a recording: take the rows that reach it, those that name a recording whose canonical_id it is and those that name a source item whose current assignment leads to it. Among them the newest row of each kind from each source, an endpoint or Ritornel itself, is that source's answer. Sources are not merged: a love from Last.fm and the owner's stars stand side by side.
CREATE TABLE listener_rating (
  id                 INTEGER PRIMARY KEY,
  listener_id        INTEGER NOT NULL REFERENCES listener(id),             -- whose taste this is
  recording_id       INTEGER REFERENCES recording(id),                     -- said in Ritornel: the recording it was said of
  source_item_id     INTEGER REFERENCES source_item(id),                   -- reported from outside: the source item it is about, whether or not that item is assigned to a recording
  kind               TEXT NOT NULL CHECK (kind IN ('rating', 'love')),     -- a love is never turned into stars, nor stars into a love
  value              INTEGER CHECK (value BETWEEN 0 AND 100),              -- a rating: ten to the half star, so 100 is five stars. NULL on a love and on a row that clears
  cleared            INTEGER NOT NULL DEFAULT 0 CHECK (cleared IN (0, 1)), -- 1: this row takes the rating or the love away again
  endpoint_id        INTEGER REFERENCES history_endpoint(id),              -- what said it, when it came from outside: a Last.fm account, later a device. NULL when it was said in Ritornel
  actor_kind         TEXT CHECK (actor_kind IN ('person', 'agent')),       -- who said it, when it was said in Ritornel
  actor_session_pid  BLOB CHECK (length(actor_session_pid) = 16),          -- the client session it was said through
  rated_at_ms        INTEGER NOT NULL,                                     -- when it was said, by whoever said it: for a love from Last.fm, the time Last.fm gives
  rev                INTEGER NOT NULL,
  CHECK ((kind = 'rating' AND cleared = 0) = (value IS NOT NULL)),
  CHECK ((recording_id IS NULL) <> (source_item_id IS NULL)),              -- it is about a recording or about a source item: one or the other
  CHECK ((endpoint_id IS NULL) = (actor_kind IS NOT NULL)),                -- said here by someone, or reported by something: one or the other
  CHECK ((endpoint_id IS NULL) = (source_item_id IS NULL)),                -- and the two go together: a report names a source item, and what is said here names a recording
  CHECK (actor_session_pid IS NULL OR actor_kind IS NOT NULL)
) STRICT;

-- A rating is never changed or removed. Changing your mind is a new row, and so is clearing.
CREATE TRIGGER listener_rating_no_update
BEFORE UPDATE ON listener_rating
BEGIN
  SELECT RAISE(ABORT, 'listener_rating is append-only: a changed rating is a new row');
END;

CREATE TRIGGER listener_rating_no_delete
BEFORE DELETE ON listener_rating
BEGIN
  SELECT RAISE(ABORT, 'listener_rating is append-only: clearing a rating is a new row');
END;

-- [history|durable] One report that reused a stored source key with a different payload. The first is kept; this is the second. history.md "Observation kinds".
CREATE TABLE history_ingest_conflict (
  id                       INTEGER PRIMARY KEY,
  endpoint_id              INTEGER NOT NULL REFERENCES history_endpoint(id),
  source_key               BLOB NOT NULL,
  existing_observation_id  INTEGER NOT NULL REFERENCES history_observation(id),
  payload_json             TEXT NOT NULL,
  received_at_utc_ms       INTEGER NOT NULL,
  rev                      INTEGER NOT NULL
) STRICT;


-- -----------------------------------------------------------------------------
-- History, derived: dropped and rebuilt by `ritornelctl history rebuild`
-- -----------------------------------------------------------------------------

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
  context_kind        TEXT CHECK (context_kind IN ('manual', 'album', 'playlist', 'query', 'agent')),
  context_ref         BLOB CHECK (length(context_ref) = 16),
  shuffle             INTEGER CHECK (shuffle IN (0, 1)),
  rev                 INTEGER NOT NULL
) STRICT;

-- The three indexes history.md "Queries M1 must answer" names.
CREATE INDEX play_event_recent ON play_event (listener_id, event_at_utc_ms DESC);
CREATE INDEX play_event_counted ON play_event (listener_id, recording_id, event_at_utc_ms DESC)
  WHERE counts = 1 AND excluded = 0;
CREATE INDEX play_event_on_this_day ON play_event (listener_id, month_day, local_date);

-- [history|derived] One observation supporting one play event. history.md decision 9; review 6.5.
CREATE TABLE play_event_observation (
  id              INTEGER PRIMARY KEY,
  play_event_id   INTEGER NOT NULL REFERENCES play_event(id),
  observation_id  INTEGER NOT NULL REFERENCES history_observation(id),
  role            TEXT NOT NULL CHECK (role IN ('primary', 'corroborating')),   -- the primary supplies time, played time and outcome
  rev             INTEGER NOT NULL,
  UNIQUE (play_event_id, observation_id)
) STRICT;

CREATE INDEX play_event_observation_observation ON play_event_observation (observation_id);

-- Plays that count and are not excluded. M1 has the dated half; the undated half joins in M3 with the same columns. history.md "Effective plays".
CREATE VIEW play_effective AS
SELECT
  listener_id,
  recording_id,
  event_at_utc_ms,
  NULL AS not_before_ms,
  NULL AS not_after_ms,
  id   AS play_event_id
FROM play_event
WHERE counts = 1 AND excluded = 0 AND event_at_utc_ms IS NOT NULL;
