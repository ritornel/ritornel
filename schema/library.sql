-- library.db: the schema after every migration, as SQLite keeps it.
-- Generated from crates/ritornel-engine/migrations. Do not edit it: change the
-- schema with a new migration, then run `mise run schema`.
--
-- Migrations, each with the SHA-256 recorded when it is applied. A checksum
-- that changes here means a migration that has shipped was edited.
--   0001_library  62cec2a19a590384d34a42a3b8b3bb1c4cabdcc21f7f7371bcd49f848bebee56
--   0002_fold_search_text  6c46673f3d3da76a0115cc787be0537d9f2ec2660002ca9f579514ff617b498e
--   0003_claim_and_love_indexes  4247daecea906f0640d8aec80e338886d707b933201711d07a6ad210e0b9efb1
--   0004_observation_recording_correction  f500fbf7eb27851be43e1fc97395fa05818b4bd0186480758c6161fafd92059a
--   0005_artist_keys  87f1f257a31ef76053a0f7f6591d42176859dca46c9e4b8e809a5ea30467b36f
--   0006_alias_spellings_and_unbind  5d6f0f953a8e5dab6217e7eeb472fcd0c9f5154ea3a65befc16b174ab20e21d1

CREATE TABLE schema_migration (
  version        INTEGER PRIMARY KEY,
  name           TEXT NOT NULL,
  checksum       BLOB NOT NULL,     -- hash of the migration file; the server refuses to start if a shipped one changed
  applied_at_ms  INTEGER NOT NULL
) STRICT;

CREATE TABLE server (
  id                INTEGER PRIMARY KEY,
  public_id         BLOB NOT NULL UNIQUE CHECK (length(public_id) = 16),   -- the server's own ID from its server.db
  name              TEXT NOT NULL,                                         -- untrusted display text
  created_at_ms     INTEGER NOT NULL,
  rev               INTEGER NOT NULL
) STRICT;

CREATE TABLE library_meta (
  id                INTEGER PRIMARY KEY CHECK (id = 1),
  library_id        BLOB NOT NULL CHECK (length(library_id) = 16),
  hub_server_id     INTEGER REFERENCES server(id),   -- NULL means released: no server may write
  hub_epoch         INTEGER NOT NULL,                -- raised by every release and adopt
  rev               INTEGER NOT NULL,                -- the revision counter itself: this row changes in every write transaction
  created_at_ms     INTEGER NOT NULL
) STRICT;

CREATE TABLE library_revision (
  rev                INTEGER PRIMARY KEY,
  command_id         BLOB CHECK (length(command_id) = 16),          -- NULL for writes with no receipt, such as a history ingest
  method             TEXT NOT NULL,                                 -- the contract call or internal writer, for example 'scan.step'
  client_session_pid BLOB CHECK (length(client_session_pid) = 16),  -- session public ID in the handling server's server.db; NULL for internal work
  client_kind        TEXT CHECK (client_kind IN ('person', 'agent', 'server')),   -- the session's subject kind
  committed_at_ms    INTEGER NOT NULL
) STRICT;

CREATE TABLE library_command (
  id                   INTEGER PRIMARY KEY,
  command_id           BLOB NOT NULL UNIQUE CHECK (length(command_id) = 16),   -- minted by the client; a job step derives its 16 bytes from the job's ID and the step number
  request_fingerprint  BLOB NOT NULL,                                          -- same ID with another fingerprint is refused
  method               TEXT NOT NULL,
  result_json          TEXT NOT NULL,                                          -- the result to replay on a retry
  rev                  INTEGER NOT NULL REFERENCES library_revision(rev),
  created_at_ms        INTEGER NOT NULL
) STRICT;

CREATE TABLE library_setting (
  id                INTEGER PRIMARY KEY,
  key               TEXT NOT NULL UNIQUE,
  value_json        TEXT NOT NULL,
  rev               INTEGER NOT NULL
) STRICT;

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

CREATE TABLE artist_credit (
  id                INTEGER PRIMARY KEY,
  display_text      TEXT NOT NULL,   -- the whole credit as one string
  rev               INTEGER NOT NULL
) STRICT;

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

CREATE INDEX release_release_group ON release (release_group_id);

CREATE INDEX release_display_artist_credit ON release (display_artist_credit_id);

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

CREATE INDEX track_appearance_release ON track_appearance (release_id);

CREATE TABLE picture_folder (
  id                INTEGER PRIMARY KEY,
  public_id         BLOB NOT NULL UNIQUE CHECK (length(public_id) = 16),
  server_id         INTEGER NOT NULL REFERENCES server(id),
  name              TEXT NOT NULL,
  created_at_ms     INTEGER NOT NULL,
  rev               INTEGER NOT NULL,
  UNIQUE (server_id, name)
) STRICT;

CREATE TABLE artist_picture (
  id                INTEGER PRIMARY KEY,
  artist_id         INTEGER NOT NULL REFERENCES artist(id),
  kind              TEXT NOT NULL CHECK (kind IN ('portrait')),   -- the square picture for the circle and the banner
  picture_id        INTEGER NOT NULL REFERENCES picture(id),
  folder_id         INTEGER NOT NULL REFERENCES picture_folder(id),
  path              TEXT NOT NULL,                                -- relative to the folder, such as '<mbid>/portrait.deezer.0a1b2c.jpg'
  matched_by        TEXT NOT NULL CHECK (matched_by IN ('mbid', 'name')),   -- the artist's MusicBrainz ID, or their name where they have none
  source            TEXT,                                         -- who supplied it, as the manifest says: 'theaudiodb', 'deezer', 'owner'
  chosen_by         TEXT,                                         -- who chose it, as the manifest says: 'rule:v1', 'owner'
  rev               INTEGER NOT NULL,
  UNIQUE (artist_id, kind)
) STRICT;

CREATE INDEX artist_picture_picture ON artist_picture (picture_id);

CREATE TABLE metadata_source (
  id                INTEGER PRIMARY KEY,
  kind              TEXT NOT NULL UNIQUE,   -- 'user', 'musicbrainz', 'beets', 'file_tags', 'device', 'inferred'
  precedence        INTEGER NOT NULL,
  rev               INTEGER NOT NULL
) STRICT;

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

CREATE TABLE source_instance (
  id                INTEGER PRIMARY KEY,
  public_id         BLOB NOT NULL UNIQUE CHECK (length(public_id) = 16),   -- the contract's sourceId
  kind              TEXT NOT NULL,                                         -- 'folder' and 'lastfm' in M1; later 'beets', 'device', 'musicbrainz'
  name              TEXT NOT NULL,
  root_id           INTEGER UNIQUE REFERENCES library_root(id),            -- a folder's root, of kind 'master'; one folder per root
  created_at_ms     INTEGER NOT NULL,
  rev               INTEGER NOT NULL,
  CHECK ((kind = 'folder') = (root_id IS NOT NULL))
) STRICT;

CREATE TABLE source_item (
  id                    INTEGER PRIMARY KEY,
  public_id             BLOB NOT NULL UNIQUE CHECK (length(public_id) = 16),
  source_instance_id    INTEGER NOT NULL REFERENCES source_instance(id),   -- for a file, the folder, which names the root
  item_kind             TEXT NOT NULL,                              -- 'file' and 'scrobble_subject' in M1; later 'device_object'
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

CREATE UNIQUE INDEX source_item_live_key
  ON source_item (source_instance_id, item_kind, provider_key)
  WHERE retired_at_ms IS NULL;

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

CREATE INDEX identifier_claim_item ON identifier_claim (source_item_id, namespace);

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

CREATE UNIQUE INDEX metadata_edit_in_force
  ON metadata_edit (entity_kind, entity_id, field, ordinal)
  WHERE withdrawn_at_ms IS NULL;

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

CREATE UNIQUE INDEX assignment_current
  ON assignment (source_item_id)
  WHERE superseded_at_ms IS NULL;

CREATE INDEX assignment_current_appearance
  ON assignment (appearance_id)
  WHERE superseded_at_ms IS NULL;

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

CREATE INDEX revision_picture_picture ON revision_picture (picture_id);

CREATE VIRTUAL TABLE search_text USING fts5(
  title,
  artist,
  album,
  tokenize = 'trigram remove_diacritics 1'
);

CREATE TABLE 'search_text_data'(id INTEGER PRIMARY KEY, block BLOB);

CREATE TABLE 'search_text_idx'(segid, term, pgno, PRIMARY KEY(segid, term)) WITHOUT ROWID;

CREATE TABLE 'search_text_content'(id INTEGER PRIMARY KEY, c0, c1, c2);

CREATE TABLE 'search_text_docsize'(id INTEGER PRIMARY KEY, sz BLOB);

CREATE TABLE 'search_text_config'(k PRIMARY KEY, v) WITHOUT ROWID;

CREATE TABLE listener (
  id                INTEGER PRIMARY KEY,
  public_id         BLOB NOT NULL UNIQUE CHECK (length(public_id) = 16),
  name              TEXT NOT NULL,
  rev               INTEGER NOT NULL
) STRICT;

CREATE TABLE history_endpoint (
  id                INTEGER PRIMARY KEY,
  public_id         BLOB NOT NULL UNIQUE CHECK (length(public_id) = 16),
  kind              TEXT NOT NULL CHECK (kind IN ('player', 'device_log', 'service', 'import', 'manual')),   -- 'player', 'manual' and 'service' in M1
  server_id         INTEGER REFERENCES server(id),   -- a player's server
  name              TEXT NOT NULL,
  rev               INTEGER NOT NULL,
  CHECK (kind <> 'player' OR server_id IS NOT NULL)
) STRICT;

CREATE UNIQUE INDEX history_endpoint_player ON history_endpoint (server_id) WHERE kind = 'player';

CREATE UNIQUE INDEX history_endpoint_manual ON history_endpoint (kind) WHERE kind = 'manual';

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

CREATE INDEX history_observation_source_item ON history_observation (source_item_id);

CREATE TABLE observation_listen (
  observation_id            INTEGER PRIMARY KEY REFERENCES history_observation(id),
  played_on_server_id       INTEGER NOT NULL REFERENCES server(id),          -- the server whose player made the sound
  actor_session_pid         BLOB CHECK (length(actor_session_pid) = 16),     -- the client session that queued the track
  source_revision_no        INTEGER NOT NULL CHECK (source_revision_no > 0),   -- which payload of the envelope's source item was heard
  queued_appearance_id      INTEGER REFERENCES track_appearance(id),         -- a hint for display only; the recording always comes from the source item
  audio_asset_id            INTEGER REFERENCES audio_asset(id),              -- the asset that played
  copy_kind                 TEXT NOT NULL CHECK (copy_kind IN ('master', 'derived', 'stream_master', 'stream_transcode')),
  started_at_utc_ms         INTEGER NOT NULL,                                -- the playing server's wall clock; its zone is on the envelope
  ended_at_utc_ms           INTEGER NOT NULL,                                -- the same clock, which can have been set back since the start
  started_mono_ms           INTEGER NOT NULL,                                -- the playing server's monotonic clock when the track started
  ended_mono_ms             INTEGER,                                         -- the same clock at the end: for a crash, at the last checkpoint. NULL only for a listen recorded before the clock was kept
  boot_id                   BLOB NOT NULL CHECK (length(boot_id) = 16),      -- the boot that clock counts from: the machine's, as its kernel names it, or the playing server's run where the machine's could not be read
  start_reason              TEXT NOT NULL CHECK (start_reason IN ('user_pick', 'auto_advance', 'repeat', 'resume')),
  continues_source_key      BLOB CHECK (length(continues_source_key) = 16),   -- the earlier listen on this endpoint that this one resumes
  start_position_ms         INTEGER NOT NULL CHECK (start_position_ms >= 0),
  end_position_ms           INTEGER NOT NULL CHECK (end_position_ms >= 0),
  max_position_ms           INTEGER NOT NULL CHECK (max_position_ms >= 0),
  track_duration_ms         INTEGER NOT NULL CHECK (track_duration_ms >= 0),
  ms_played                 INTEGER NOT NULL CHECK (ms_played >= 0),         -- audio actually rendered
  heard_ranges_json         TEXT NOT NULL CHECK (json_valid(heard_ranges_json)),   -- merged ranges of the track that were heard, with no cap on how many
  heard_ranges_approximate  INTEGER NOT NULL CHECK (heard_ranges_approximate IN (0, 1)),   -- neighbouring ranges were joined to keep them under the player's cap
  seek_count                INTEGER NOT NULL CHECK (seek_count >= 0),
  pause_count               INTEGER NOT NULL CHECK (pause_count >= 0),
  end_reason                TEXT NOT NULL CHECK (end_reason IN ('finished', 'next', 'previous', 'jump', 'replaced', 'removed', 'stopped', 'error', 'output_lost', 'shutdown', 'crash')),
  context_kind              TEXT NOT NULL CHECK (context_kind IN ('manual', 'album', 'artist', 'playlist', 'query', 'agent')),
  context_ref               BLOB CHECK (length(context_ref) = 16),           -- public ID of the release, playlist, definition or agent session
  shuffle                   INTEGER NOT NULL CHECK (shuffle IN (0, 1)),
  rev                       INTEGER NOT NULL
) STRICT;

CREATE INDEX observation_listen_continues ON observation_listen (continues_source_key)
  WHERE continues_source_key IS NOT NULL;

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

CREATE TABLE correction_target (
  id                     INTEGER PRIMARY KEY,
  correction_id          INTEGER NOT NULL REFERENCES observation_correction(observation_id),
  target_observation_id  INTEGER NOT NULL REFERENCES history_observation(id),   -- for 'revoke', the correction being undone
  rev                    INTEGER NOT NULL,
  UNIQUE (correction_id, target_observation_id)
) STRICT;

CREATE INDEX correction_target_target ON correction_target (target_observation_id);

CREATE TRIGGER observation_listen_kind
BEFORE INSERT ON observation_listen
WHEN (SELECT kind FROM history_observation WHERE id = NEW.observation_id) IS NOT 'listen'
BEGIN
  SELECT RAISE(ABORT, 'observation_listen holds the detail of a listen and of nothing else');
END;

CREATE TRIGGER observation_import_kind
BEFORE INSERT ON observation_import
WHEN (SELECT kind FROM history_observation WHERE id = NEW.observation_id) IS NOT 'import'
BEGIN
  SELECT RAISE(ABORT, 'observation_import holds the detail of an import record and of nothing else');
END;

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

CREATE TABLE listener_zone_period (
  id                INTEGER PRIMARY KEY,
  listener_id       INTEGER NOT NULL REFERENCES listener(id),
  zone_name         TEXT NOT NULL,   -- IANA name
  valid_from_ms     INTEGER,         -- NULL is open
  valid_to_ms       INTEGER,
  rev               INTEGER NOT NULL
) STRICT;

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

CREATE TABLE history_ingest_conflict (
  id                       INTEGER PRIMARY KEY,
  endpoint_id              INTEGER NOT NULL REFERENCES history_endpoint(id),
  source_key               BLOB NOT NULL,
  existing_observation_id  INTEGER NOT NULL REFERENCES history_observation(id),
  payload_json             TEXT NOT NULL,
  received_at_utc_ms       INTEGER NOT NULL,
  rev                      INTEGER NOT NULL
) STRICT;

CREATE TABLE history_cursor (
  id             INTEGER PRIMARY KEY,
  endpoint_id    INTEGER NOT NULL UNIQUE REFERENCES history_endpoint(id),
  cursor_json    TEXT NOT NULL CHECK (json_valid(cursor_json)),
  updated_at_ms  INTEGER NOT NULL,
  rev            INTEGER NOT NULL
) STRICT;

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
  context_kind        TEXT CHECK (context_kind IN ('manual', 'album', 'artist', 'playlist', 'query', 'agent')),
  context_ref         BLOB CHECK (length(context_ref) = 16),
  shuffle             INTEGER CHECK (shuffle IN (0, 1)),
  rev                 INTEGER NOT NULL
) STRICT;

CREATE INDEX play_event_recent ON play_event (listener_id, event_at_utc_ms DESC, public_id DESC);

CREATE INDEX play_event_counted ON play_event (listener_id, recording_id, event_at_utc_ms DESC)
  WHERE counts = 1 AND excluded = 0;

CREATE INDEX play_event_on_this_day ON play_event (listener_id, month_day, local_date);

CREATE TABLE play_event_observation (
  id              INTEGER PRIMARY KEY,
  play_event_id   INTEGER NOT NULL REFERENCES play_event(id),
  observation_id  INTEGER NOT NULL REFERENCES history_observation(id),
  role            TEXT NOT NULL CHECK (role IN ('primary', 'corroborating')),   -- the primary supplies time, played time and outcome
  rev             INTEGER NOT NULL,
  UNIQUE (play_event_id, observation_id)
) STRICT;

CREATE INDEX play_event_observation_observation ON play_event_observation (observation_id);

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

CREATE INDEX metadata_claim_item ON metadata_claim (source_item_id, field);

CREATE INDEX listener_rating_reported ON listener_rating (endpoint_id, kind, source_item_id);

CREATE TABLE "observation_correction" (
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

CREATE TABLE artist_key (
  id                  INTEGER PRIMARY KEY,
  key_kind            TEXT NOT NULL CHECK (key_kind IN ('mbid', 'name')),
  key_value           TEXT NOT NULL CHECK (length(key_value) > 0),
  artist_id           INTEGER NOT NULL REFERENCES artist(id),
  normalizer_version  INTEGER,                                             -- NULL for 'mbid'; the normaliser version that made key_value for 'name'
  decision_id         INTEGER REFERENCES identity_decision(id),            -- set when an owner decision pinned this key
  rev                 INTEGER NOT NULL,
  UNIQUE (key_kind, key_value),
  CHECK ((key_kind = 'mbid') = (normalizer_version IS NULL))
) STRICT;

CREATE INDEX artist_key_artist ON artist_key (artist_id);

CREATE TABLE "identity_decision" (
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

CREATE TABLE "alias_observation" (
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
