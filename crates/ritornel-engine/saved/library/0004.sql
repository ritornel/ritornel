-- A library.db as migration 0004 left it: the database of saved/library/0003.sql
-- with migration 0004's file run on it and its ledger row, as the runner does.
-- This is what `sqlite3 library.db .dump` printed. A test loads it and
-- migrates it forward (docs/design/server.md, "Testing and CI"), so migration
-- 0005, which adds artist pictures, is shown to keep what the library holds.
--
-- Nothing here is real. The clock was fixed and the random bytes counted up
-- from 00; every title, name and path is made up. The search index is an FTS5
-- table, which the dump writes through `writable_schema`.
/* WARNING: Script requires that SQLITE_DBCONFIG_DEFENSIVE be disabled */
PRAGMA foreign_keys=OFF;
BEGIN TRANSACTION;
CREATE TABLE schema_migration (
  version        INTEGER PRIMARY KEY,
  name           TEXT NOT NULL,
  checksum       BLOB NOT NULL,     -- hash of the migration file; the server refuses to start if a shipped one changed
  applied_at_ms  INTEGER NOT NULL
) STRICT;
INSERT INTO schema_migration VALUES(1,'0001_library',X'5a72aa0a6f7da030ece148d3b4669da867ca722f4c659f5600b47fd648100b45',1794776400000);
INSERT INTO schema_migration VALUES(2,'0002_list_indexes',X'1789bf26cc0c33c9bb95b6c24293e838237ffe0695bdcc7a1818c0b79212d0cb',1794776400000);
INSERT INTO schema_migration VALUES(3,'0003_listen_detail',X'826a650aaf9ac96b0cbf0365f3a06ad90c5df0af654ee21353fb4bd16220d1e0',1794776400000);
INSERT INTO schema_migration VALUES(4,'0004_lastfm',X'bb5ed63f0f6797c6130d974d315612dfa8f1d4acaba1f32a2154d3af185c4397',1794776400000);
CREATE TABLE server (
  id                INTEGER PRIMARY KEY,
  public_id         BLOB NOT NULL UNIQUE CHECK (length(public_id) = 16),   -- the server's own ID from its server.db
  name              TEXT NOT NULL,                                         -- untrusted display text
  created_at_ms     INTEGER NOT NULL,
  rev               INTEGER NOT NULL
) STRICT;
INSERT INTO server VALUES(1,X'01a1e10280807021a223242526272829','desk',1794776400000,1);
CREATE TABLE library_meta (
  id                INTEGER PRIMARY KEY CHECK (id = 1),
  library_id        BLOB NOT NULL CHECK (length(library_id) = 16),
  hub_server_id     INTEGER REFERENCES server(id),   -- NULL means released: no server may write
  hub_epoch         INTEGER NOT NULL,                -- raised by every release and adopt
  rev               INTEGER NOT NULL,                -- the revision counter itself: this row changes in every write transaction
  created_at_ms     INTEGER NOT NULL
) STRICT;
INSERT INTO library_meta VALUES(1,X'01a1e10280807455965758595a5b5c5d',1,1,11,1794776400000);
CREATE TABLE library_revision (
  rev                INTEGER PRIMARY KEY,
  command_id         BLOB CHECK (length(command_id) = 16),          -- NULL for writes with no receipt, such as a history ingest
  method             TEXT NOT NULL,                                 -- the contract call or internal writer, for example 'scan.step'
  client_session_pid BLOB CHECK (length(client_session_pid) = 16),  -- session public ID in the handling server's server.db; NULL for internal work
  client_kind        TEXT CHECK (client_kind IN ('person', 'agent', 'server')),   -- the session's subject kind
  committed_at_ms    INTEGER NOT NULL
) STRICT;
INSERT INTO library_revision VALUES(1,NULL,'library.create',NULL,NULL,1794776400000);
INSERT INTO library_revision VALUES(2,NULL,'test.catalog',NULL,NULL,1794776400000);
INSERT INTO library_revision VALUES(3,NULL,'history.ingest',NULL,NULL,1794776556000);
INSERT INTO library_revision VALUES(4,NULL,'history.ingest',NULL,NULL,1794776940000);
INSERT INTO library_revision VALUES(5,NULL,'history.ingest',NULL,NULL,1794777215000);
INSERT INTO library_revision VALUES(6,NULL,'history.ingest',NULL,NULL,1794777445000);
INSERT INTO library_revision VALUES(7,NULL,'history.ingest',NULL,NULL,1794777900000);
INSERT INTO library_revision VALUES(8,NULL,'history.ingest',NULL,NULL,1794778200000);
INSERT INTO library_revision VALUES(9,NULL,'history.ingest',NULL,NULL,1794778500000);
INSERT INTO library_revision VALUES(10,NULL,'history.ingest',NULL,NULL,1794778885000);
INSERT INTO library_revision VALUES(11,NULL,'history.ingest',NULL,NULL,1794783260000);
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
INSERT INTO artist_credit VALUES(1,'Moss Arcade',2);
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
INSERT INTO recording VALUES(1,X'01a1e102808074a5a6a7a8a9aaabacad',NULL,1,'Paper Kites',1,2);
INSERT INTO recording VALUES(2,X'01a1e102808070e1a2e3e4e5e6e7e8e9',NULL,2,'Lanternfish',1,2);
INSERT INTO recording VALUES(3,X'01a1e10280807c1d9e1f202122232425',NULL,3,'Interlude',1,2);
INSERT INTO recording VALUES(4,X'01a1e102808078599a5b5c5d5e5f6061',NULL,4,'Salt Road',1,2);
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
INSERT INTO release_group VALUES(1,X'01a1e102808070919293949596979899',NULL,NULL,NULL,NULL,'[]',NULL,NULL,2);
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
INSERT INTO "release" VALUES(1,X'01a1e10280807a9b9c9d9e9fa0a1a2a3',NULL,1,NULL,1,'Tin Weather',NULL,NULL,NULL,NULL,2);
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
INSERT INTO track_appearance VALUES(1,X'01a1e10280807eafb0b1b2b3b4b5b6b7',1,1,NULL,1,'Paper Kites',1,1,1,240000,NULL,2);
INSERT INTO track_appearance VALUES(2,X'01a1e10280807aebacedeeeff0f1f2f3',2,1,NULL,2,'Lanternfish',1,1,2,300000,NULL,2);
INSERT INTO track_appearance VALUES(3,X'01a1e10280807627a8292a2b2c2d2e2f',3,1,NULL,3,'Interlude',1,1,3,25000,NULL,2);
INSERT INTO track_appearance VALUES(4,X'01a1e10280807263a465666768696a6b',4,1,NULL,4,'Salt Road',1,1,4,200000,NULL,2);
CREATE TABLE metadata_source (
  id                INTEGER PRIMARY KEY,
  kind              TEXT NOT NULL UNIQUE,   -- 'user', 'musicbrainz', 'beets', 'file_tags', 'device', 'inferred'
  precedence        INTEGER NOT NULL,
  rev               INTEGER NOT NULL
) STRICT;
INSERT INTO metadata_source VALUES(1,'user',60,1);
INSERT INTO metadata_source VALUES(2,'musicbrainz',50,1);
INSERT INTO metadata_source VALUES(3,'beets',40,1);
INSERT INTO metadata_source VALUES(4,'file_tags',30,1);
INSERT INTO metadata_source VALUES(5,'device',20,1);
INSERT INTO metadata_source VALUES(6,'inferred',10,1);
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
INSERT INTO library_root VALUES(1,X'01a1e10280807c7dbe7f808182838485',1,'music','master',0,0,0,2);
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
INSERT INTO source_instance VALUES(1,X'01a1e1028080768788898a8b8c8d8e8f','folder','music',1,0,2);
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
INSERT INTO source_item VALUES(1,X'01a1e10280807ccd8ecfd0d1d2d3d4d5',1,'file','Moss Arcade/Tin Weather/01.flac',1,1,1000,0,NULL,NULL,0,2);
INSERT INTO source_item VALUES(2,X'01a1e102808078098a0b0c0d0e0f1011',1,'file','Moss Arcade/Tin Weather/02.flac',2,2,1000,0,NULL,NULL,0,2);
INSERT INTO source_item VALUES(3,X'01a1e10280807445864748494a4b4c4d',1,'file','Moss Arcade/Tin Weather/03.flac',3,3,1000,0,NULL,NULL,0,2);
INSERT INTO source_item VALUES(4,X'01a1e102808070818283848586878889',1,'file','Moss Arcade/Tin Weather/04.mp3',4,4,1000,0,NULL,NULL,0,2);
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
INSERT INTO source_item_revision VALUES(1,1,1,X'01a1e102808076d798d9dadbdcdddedf','{}',1,0,2);
INSERT INTO source_item_revision VALUES(2,2,1,X'01a1e102808072139415161718191a1b','{}',2,0,2);
INSERT INTO source_item_revision VALUES(3,3,1,X'01a1e10280807e4f9051525354555657','{}',3,0,2);
INSERT INTO source_item_revision VALUES(4,4,1,X'01a1e10280807a8b8c8d8e8f90919293','{}',4,0,2);
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
INSERT INTO assignment VALUES(1,1,1,1,1,0,NULL,NULL,2);
INSERT INTO assignment VALUES(2,2,2,2,2,0,NULL,NULL,2);
INSERT INTO assignment VALUES(3,3,3,3,3,0,NULL,NULL,2);
INSERT INTO assignment VALUES(4,4,4,4,4,0,NULL,NULL,2);
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
INSERT INTO audio_asset VALUES(1,X'01a1e102808078b9babbbcbdbebfc0c1',1,X'01a1e102808072c384c5c6c7c8c9cacb',NULL,'flac',44100,'stereo',16,NULL,1,240000,2);
INSERT INTO audio_asset VALUES(2,X'01a1e102808074f5b6f7f8f9fafbfcfd',1,X'01a1e10280807eff8001020304050607',NULL,'flac',44100,'stereo',16,NULL,1,300000,2);
INSERT INTO audio_asset VALUES(3,X'01a1e10280807031b233343536373839',1,X'01a1e10280807a3bbc3d3e3f40414243',NULL,'flac',44100,'stereo',16,NULL,1,25000,2);
INSERT INTO audio_asset VALUES(4,X'01a1e10280807c6dae6f707172737475',1,X'01a1e10280807677b8797a7b7c7d7e7f',NULL,'flac',44100,'stereo',16,NULL,1,200000,2);
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
PRAGMA writable_schema=ON;
INSERT INTO sqlite_schema(type,name,tbl_name,rootpage,sql)VALUES('table','search_text','search_text',0,'CREATE VIRTUAL TABLE search_text USING fts5(
  title,
  artist,
  album,
  tokenize = ''trigram remove_diacritics 1''
)');
CREATE TABLE IF NOT EXISTS 'search_text_data'(id INTEGER PRIMARY KEY, block BLOB);
INSERT INTO search_text_data VALUES(1,X'');
INSERT INTO search_text_data VALUES(10,X'00000000000000');
CREATE TABLE IF NOT EXISTS 'search_text_idx'(segid, term, pgno, PRIMARY KEY(segid, term)) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS 'search_text_content'(id INTEGER PRIMARY KEY, c0, c1, c2);
CREATE TABLE IF NOT EXISTS 'search_text_docsize'(id INTEGER PRIMARY KEY, sz BLOB);
CREATE TABLE IF NOT EXISTS 'search_text_config'(k PRIMARY KEY, v) WITHOUT ROWID;
INSERT INTO search_text_config VALUES('version',4);
CREATE TABLE listener (
  id                INTEGER PRIMARY KEY,
  public_id         BLOB NOT NULL UNIQUE CHECK (length(public_id) = 16),
  name              TEXT NOT NULL,
  rev               INTEGER NOT NULL
) STRICT;
INSERT INTO listener VALUES(1,X'01a1e10280807e5fa061626364656667','owner',1);
CREATE TABLE history_endpoint (
  id                INTEGER PRIMARY KEY,
  public_id         BLOB NOT NULL UNIQUE CHECK (length(public_id) = 16),
  kind              TEXT NOT NULL CHECK (kind IN ('player', 'device_log', 'service', 'import', 'manual')),   -- 'player' and 'manual' in M1
  server_id         INTEGER REFERENCES server(id),   -- a player's server
  name              TEXT NOT NULL,
  rev               INTEGER NOT NULL,
  CHECK (kind <> 'player' OR server_id IS NOT NULL)
) STRICT;
INSERT INTO history_endpoint VALUES(1,X'01a1e10280807869aa6b6c6d6e6f7071','player',1,'player',1);
INSERT INTO history_endpoint VALUES(2,X'01a1e10280807273b475767778797a7b','manual',NULL,'corrections',1);
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
INSERT INTO history_observation VALUES(1,1,X'01010101010101010101010101010101','listen',1,1,1794776496000,1794776556000,1794776556000,1156000,X'b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0',0,'America/New_York','captured',-300,20261115,16,'{"v":1,"listenId":"01010101-0101-0101-0101-010101010101","listener":"01a1e102-8080-7e5f-a061-626364656667","playedOnServer":"01a1e102-8080-7021-a223-242526272829","appearance":"01a1e102-8080-7eaf-b0b1-b2b3b4b5b6b7","asset":"01a1e102-8080-78b9-babb-bcbdbebfc0c1","copyKind":"master","sourceItem":"01a1e102-8080-7ccd-8ecf-d0d1d2d3d4d5","sourceRevision":1,"startedAtMs":1794776400000,"zone":"America/New_York","startedMonoMs":1000000,"boot":"b0b0b0b0-b0b0-b0b0-b0b0-b0b0b0b0b0b0","startReason":"user_pick","startPositionMs":0,"trackDurationMs":240000,"msPlayed":96000,"heardRanges":[[0,96000]],"maxPositionMs":96000,"seekCount":0,"pauseCount":0,"endPositionMs":96000,"endReason":"next","endedAtMs":1794776496000,"contextKind":"album","shuffle":false}',3);
INSERT INTO history_observation VALUES(2,1,X'02020202020202020202020202020202','listen',2,1,1794776880000,1794776940000,1794776940000,1540000,X'b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0',0,'America/New_York','captured',-300,20261115,16,'{"v":1,"listenId":"02020202-0202-0202-0202-020202020202","listener":"01a1e102-8080-7e5f-a061-626364656667","playedOnServer":"01a1e102-8080-7021-a223-242526272829","appearance":"01a1e102-8080-7aeb-aced-eeeff0f1f2f3","asset":"01a1e102-8080-74f5-b6f7-f8f9fafbfcfd","copyKind":"master","sourceItem":"01a1e102-8080-7809-8a0b-0c0d0e0f1011","sourceRevision":1,"startedAtMs":1794776700000,"zone":"America/New_York","startedMonoMs":1300000,"boot":"b0b0b0b0-b0b0-b0b0-b0b0-b0b0b0b0b0b0","startReason":"user_pick","startPositionMs":0,"trackDurationMs":300000,"msPlayed":180000,"heardRanges":[[0,180000]],"maxPositionMs":180000,"seekCount":0,"pauseCount":0,"endPositionMs":180000,"endReason":"finished","endedAtMs":1794776880000,"contextKind":"album","shuffle":false}',4);
INSERT INTO history_observation VALUES(3,1,X'03030303030303030303030303030303','listen',2,1,1794777155000,1794777215000,1794777215000,1815000,X'b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0',0,'America/New_York','captured',-300,20261115,16,'{"v":1,"listenId":"03030303-0303-0303-0303-030303030303","listener":"01a1e102-8080-7e5f-a061-626364656667","playedOnServer":"01a1e102-8080-7021-a223-242526272829","appearance":"01a1e102-8080-7aeb-aced-eeeff0f1f2f3","asset":"01a1e102-8080-74f5-b6f7-f8f9fafbfcfd","copyKind":"master","sourceItem":"01a1e102-8080-7809-8a0b-0c0d0e0f1011","sourceRevision":1,"startedAtMs":1794777000000,"zone":"America/New_York","startedMonoMs":1600000,"boot":"b0b0b0b0-b0b0-b0b0-b0b0-b0b0b0b0b0b0","startReason":"user_pick","startPositionMs":0,"trackDurationMs":300000,"msPlayed":155000,"heardRanges":[[0,155000]],"maxPositionMs":155000,"seekCount":0,"pauseCount":0,"endPositionMs":155000,"endReason":"crash","endedAtMs":1794777155000,"contextKind":"album","shuffle":false}',5);
INSERT INTO history_observation VALUES(4,1,X'04040404040404040404040404040404','listen',2,1,1794777385000,1794777445000,1794777445000,205050,X'b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0',0,'America/New_York','captured',-300,20261115,16,'{"v":1,"listenId":"04040404-0404-0404-0404-040404040404","listener":"01a1e102-8080-7e5f-a061-626364656667","playedOnServer":"01a1e102-8080-7021-a223-242526272829","appearance":"01a1e102-8080-7aeb-aced-eeeff0f1f2f3","asset":"01a1e102-8080-74f5-b6f7-f8f9fafbfcfd","copyKind":"master","sourceItem":"01a1e102-8080-7809-8a0b-0c0d0e0f1011","sourceRevision":1,"startedAtMs":1794777240000,"zone":"America/New_York","startedMonoMs":50,"boot":"b1b1b1b1-b1b1-b1b1-b1b1-b1b1b1b1b1b1","startReason":"resume","continues":"03030303-0303-0303-0303-030303030303","startPositionMs":155000,"trackDurationMs":300000,"msPlayed":145000,"heardRanges":[[0,145000]],"maxPositionMs":145000,"seekCount":0,"pauseCount":0,"endPositionMs":145000,"endReason":"finished","endedAtMs":1794777385000,"contextKind":"album","shuffle":false}',6);
INSERT INTO history_observation VALUES(5,1,X'05050505050505050505050505050505','listen',1,1,1794777840000,1794777900000,1794777900000,2500000,X'b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0',0,'America/New_York','captured',-300,20261115,16,'{"v":1,"listenId":"05050505-0505-0505-0505-050505050505","listener":"01a1e102-8080-7e5f-a061-626364656667","playedOnServer":"01a1e102-8080-7021-a223-242526272829","appearance":"01a1e102-8080-7eaf-b0b1-b2b3b4b5b6b7","asset":"01a1e102-8080-78b9-babb-bcbdbebfc0c1","copyKind":"master","sourceItem":"01a1e102-8080-7ccd-8ecf-d0d1d2d3d4d5","sourceRevision":1,"startedAtMs":1794777600000,"zone":"America/New_York","startedMonoMs":2200000,"boot":"b0b0b0b0-b0b0-b0b0-b0b0-b0b0b0b0b0b0","startReason":"user_pick","startPositionMs":0,"trackDurationMs":240000,"msPlayed":240000,"heardRanges":[[0,240000]],"maxPositionMs":240000,"seekCount":0,"pauseCount":0,"endPositionMs":240000,"endReason":"finished","endedAtMs":1794777840000,"contextKind":"album","shuffle":false}',7);
INSERT INTO history_observation VALUES(6,1,X'06060606060606060606060606060606','listen',1,1,1794778140000,1794778200000,1794778200000,2800000,X'b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0',0,'America/New_York','captured',-300,20261115,16,'{"v":1,"listenId":"06060606-0606-0606-0606-060606060606","listener":"01a1e102-8080-7e5f-a061-626364656667","playedOnServer":"01a1e102-8080-7021-a223-242526272829","appearance":"01a1e102-8080-7eaf-b0b1-b2b3b4b5b6b7","asset":"01a1e102-8080-78b9-babb-bcbdbebfc0c1","copyKind":"master","sourceItem":"01a1e102-8080-7ccd-8ecf-d0d1d2d3d4d5","sourceRevision":1,"startedAtMs":1794777900000,"zone":"America/New_York","startedMonoMs":2500000,"boot":"b0b0b0b0-b0b0-b0b0-b0b0-b0b0b0b0b0b0","startReason":"user_pick","startPositionMs":0,"trackDurationMs":240000,"msPlayed":240000,"heardRanges":[[0,240000]],"maxPositionMs":240000,"seekCount":0,"pauseCount":0,"endPositionMs":240000,"endReason":"finished","endedAtMs":1794778140000,"contextKind":"album","shuffle":false}',8);
INSERT INTO history_observation VALUES(7,1,X'07070707070707070707070707070707','listen',1,1,1794778440000,1794778500000,1794778500000,3100000,X'b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0',0,'America/New_York','captured',-300,20261115,16,'{"v":1,"listenId":"07070707-0707-0707-0707-070707070707","listener":"01a1e102-8080-7e5f-a061-626364656667","playedOnServer":"01a1e102-8080-7021-a223-242526272829","appearance":"01a1e102-8080-7eaf-b0b1-b2b3b4b5b6b7","asset":"01a1e102-8080-78b9-babb-bcbdbebfc0c1","copyKind":"master","sourceItem":"01a1e102-8080-7ccd-8ecf-d0d1d2d3d4d5","sourceRevision":1,"startedAtMs":1794778200000,"zone":"America/New_York","startedMonoMs":2800000,"boot":"b0b0b0b0-b0b0-b0b0-b0b0-b0b0b0b0b0b0","startReason":"user_pick","startPositionMs":0,"trackDurationMs":240000,"msPlayed":240000,"heardRanges":[[0,240000]],"maxPositionMs":240000,"seekCount":0,"pauseCount":0,"endPositionMs":240000,"endReason":"finished","endedAtMs":1794778440000,"contextKind":"album","shuffle":false}',9);
INSERT INTO history_observation VALUES(8,1,X'08080808080808080808080808080808','listen',3,1,1794778825000,1794778885000,1794778885000,3485000,X'b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0',0,'America/New_York','captured',-300,20261115,16,'{"v":1,"listenId":"08080808-0808-0808-0808-080808080808","listener":"01a1e102-8080-7e5f-a061-626364656667","playedOnServer":"01a1e102-8080-7021-a223-242526272829","appearance":"01a1e102-8080-7627-a829-2a2b2c2d2e2f","asset":"01a1e102-8080-7031-b233-343536373839","copyKind":"master","sourceItem":"01a1e102-8080-7445-8647-48494a4b4c4d","sourceRevision":1,"startedAtMs":1794778800000,"zone":"America/New_York","startedMonoMs":3400000,"boot":"b0b0b0b0-b0b0-b0b0-b0b0-b0b0b0b0b0b0","startReason":"user_pick","startPositionMs":0,"trackDurationMs":25000,"msPlayed":25000,"heardRanges":[[0,25000]],"maxPositionMs":25000,"seekCount":0,"pauseCount":0,"endPositionMs":25000,"endReason":"finished","endedAtMs":1794778825000,"contextKind":"album","shuffle":false}',10);
INSERT INTO history_observation VALUES(9,1,X'0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a','listen',1,1,1794783200000,1794783260000,1794783260000,2260000,X'b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0',0,'America/New_York','captured',-300,20261115,17,'{"v":1,"listenId":"0a0a0a0a-0a0a-0a0a-0a0a-0a0a0a0a0a0a","listener":"01a1e102-8080-7e5f-a061-626364656667","playedOnServer":"01a1e102-8080-7021-a223-242526272829","appearance":"01a1e102-8080-7eaf-b0b1-b2b3b4b5b6b7","asset":"01a1e102-8080-78b9-babb-bcbdbebfc0c1","copyKind":"master","sourceItem":"01a1e102-8080-7ccd-8ecf-d0d1d2d3d4d5","sourceRevision":1,"startedAtMs":1794783000000,"zone":"America/New_York","startedMonoMs":2000000,"boot":"b0b0b0b0-b0b0-b0b0-b0b0-b0b0b0b0b0b0","startReason":"user_pick","startPositionMs":0,"trackDurationMs":240000,"msPlayed":200000,"heardRanges":[[0,200000]],"maxPositionMs":200000,"seekCount":0,"pauseCount":0,"endPositionMs":200000,"endReason":"finished","endedAtMs":1794783200000,"contextKind":"album","shuffle":false}',11);
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
CREATE TABLE correction_target (
  id                     INTEGER PRIMARY KEY,
  correction_id          INTEGER NOT NULL REFERENCES observation_correction(observation_id),
  target_observation_id  INTEGER NOT NULL REFERENCES history_observation(id),   -- for 'revoke', the correction being undone
  rev                    INTEGER NOT NULL,
  UNIQUE (correction_id, target_observation_id)
) STRICT;
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
CREATE TABLE history_ingest_conflict (
  id                       INTEGER PRIMARY KEY,
  endpoint_id              INTEGER NOT NULL REFERENCES history_endpoint(id),
  source_key               BLOB NOT NULL,
  existing_observation_id  INTEGER NOT NULL REFERENCES history_observation(id),
  payload_json             TEXT NOT NULL,
  received_at_utc_ms       INTEGER NOT NULL,
  rev                      INTEGER NOT NULL
) STRICT;
CREATE TABLE play_event_observation (
  id              INTEGER PRIMARY KEY,
  play_event_id   INTEGER NOT NULL REFERENCES play_event(id),
  observation_id  INTEGER NOT NULL REFERENCES history_observation(id),
  role            TEXT NOT NULL CHECK (role IN ('primary', 'corroborating')),   -- the primary supplies time, played time and outcome
  rev             INTEGER NOT NULL,
  UNIQUE (play_event_id, observation_id)
) STRICT;
INSERT INTO play_event_observation VALUES(1,1,1,'primary',3);
INSERT INTO play_event_observation VALUES(2,2,2,'primary',4);
INSERT INTO play_event_observation VALUES(3,3,3,'primary',6);
INSERT INTO play_event_observation VALUES(4,3,4,'corroborating',6);
INSERT INTO play_event_observation VALUES(5,4,5,'primary',7);
INSERT INTO play_event_observation VALUES(6,5,6,'primary',8);
INSERT INTO play_event_observation VALUES(7,6,7,'primary',9);
INSERT INTO play_event_observation VALUES(8,7,8,'primary',10);
INSERT INTO play_event_observation VALUES(9,8,9,'primary',11);
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
  ended_mono_ms             INTEGER,                                         -- the same clock at the end: for a crash, at the last checkpoint. NULL only for a listen recorded before migration 0003
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
INSERT INTO observation_listen VALUES(1,1,NULL,1,1,1,'master',1794776400000,1794776496000,1000000,NULL,X'b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0','user_pick',NULL,0,96000,96000,240000,96000,'[[0,96000]]',0,0,0,'next','album',NULL,0,3);
INSERT INTO observation_listen VALUES(2,1,NULL,1,2,2,'master',1794776700000,1794776880000,1300000,NULL,X'b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0','user_pick',NULL,0,180000,180000,300000,180000,'[[0,180000]]',0,0,0,'finished','album',NULL,0,4);
INSERT INTO observation_listen VALUES(3,1,NULL,1,2,2,'master',1794777000000,1794777155000,1600000,NULL,X'b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0','user_pick',NULL,0,155000,155000,300000,155000,'[[0,155000]]',0,0,0,'crash','album',NULL,0,5);
INSERT INTO observation_listen VALUES(4,1,NULL,1,2,2,'master',1794777240000,1794777385000,50,NULL,X'b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1','resume',X'03030303030303030303030303030303',155000,145000,145000,300000,145000,'[[0,145000]]',0,0,0,'finished','album',NULL,0,6);
INSERT INTO observation_listen VALUES(5,1,NULL,1,1,1,'master',1794777600000,1794777840000,2200000,NULL,X'b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0','user_pick',NULL,0,240000,240000,240000,240000,'[[0,240000]]',0,0,0,'finished','album',NULL,0,7);
INSERT INTO observation_listen VALUES(6,1,NULL,1,1,1,'master',1794777900000,1794778140000,2500000,NULL,X'b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0','user_pick',NULL,0,240000,240000,240000,240000,'[[0,240000]]',0,0,0,'finished','album',NULL,0,8);
INSERT INTO observation_listen VALUES(7,1,NULL,1,1,1,'master',1794778200000,1794778440000,2800000,NULL,X'b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0','user_pick',NULL,0,240000,240000,240000,240000,'[[0,240000]]',0,0,0,'finished','album',NULL,0,9);
INSERT INTO observation_listen VALUES(8,1,NULL,1,3,3,'master',1794778800000,1794778825000,3400000,NULL,X'b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0','user_pick',NULL,0,25000,25000,25000,25000,'[[0,25000]]',0,0,0,'finished','album',NULL,0,10);
INSERT INTO observation_listen VALUES(9,1,NULL,1,1,1,'master',1794783000000,1794783200000,2000000,NULL,X'b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0','user_pick',NULL,0,200000,200000,240000,200000,'[[0,200000]]',0,0,0,'finished','album',NULL,0,11);
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
INSERT INTO play_event VALUES(1,X'abb3c5b6d6d58ca73e60af5069023630',1,1,1,1,1794776400000,'start','hub_age',1000,'America/New_York','captured',-300,20261115,1115,16,0,'zone',NULL,NULL,'observed',96000,240000,'skipped',0,'play_rule',1,1,0,'album',NULL,0,3);
INSERT INTO play_event VALUES(2,X'23358fd34ab03fe24f7a00ba29e1adf7',1,2,2,2,1794776700000,'start','hub_age',1000,'America/New_York','captured',-300,20261115,1115,16,0,'zone',NULL,NULL,'observed',180000,300000,'listened',1,'play_rule',1,1,0,'album',NULL,0,4);
INSERT INTO play_event VALUES(3,X'531a596aa1a85c0b4155323c017edf8d',1,2,2,2,1794777000000,'start','hub_age',1000,'America/New_York','captured',-300,20261115,1115,16,0,'zone',NULL,NULL,'observed',300000,300000,'listened',1,'play_rule',1,1,0,'album',NULL,0,6);
INSERT INTO play_event VALUES(4,X'148d7d10310ef9ddfd9dbd596b5501be',1,1,1,1,1794777600000,'start','hub_age',1000,'America/New_York','captured',-300,20261115,1115,16,0,'zone',NULL,NULL,'observed',240000,240000,'listened',1,'play_rule',1,1,0,'album',NULL,0,7);
INSERT INTO play_event VALUES(5,X'b5f0e08c21d84e57ef916c79b8c309a2',1,1,1,1,1794777900000,'start','hub_age',1000,'America/New_York','captured',-300,20261115,1115,16,0,'zone',NULL,NULL,'observed',240000,240000,'listened',1,'play_rule',1,1,0,'album',NULL,0,8);
INSERT INTO play_event VALUES(6,X'1c7850ec3a906f3c29976d9781369537',1,1,1,1,1794778200000,'start','hub_age',1000,'America/New_York','captured',-300,20261115,1115,16,0,'zone',NULL,NULL,'observed',240000,240000,'listened',1,'play_rule',1,1,0,'album',NULL,0,9);
INSERT INTO play_event VALUES(7,X'a11b2a5e8e223bca4571ef723ecd5cba',1,3,3,3,1794778800000,'start','hub_age',1000,'America/New_York','captured',-300,20261115,1115,16,0,'zone',NULL,NULL,'observed',25000,25000,'partial',0,'play_rule',1,1,0,'album',NULL,0,10);
INSERT INTO play_event VALUES(8,X'b7f34d98d8055283376cd034c0d1f956',1,1,1,1,1794783000000,'start','hub_age',1000,'America/New_York','captured',-300,20261115,1115,17,0,'zone',NULL,NULL,'observed',200000,240000,'listened',1,'play_rule',1,1,0,'album',NULL,0,11);
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
INSERT INTO identity_decision VALUES(1,'bind',1,NULL,NULL,NULL,'new',1,1,NULL,NULL,NULL,'{}',0,'resolver',NULL,NULL,0,2);
INSERT INTO identity_decision VALUES(2,'bind',2,NULL,NULL,NULL,'new',1,1,NULL,NULL,NULL,'{}',0,'resolver',NULL,NULL,0,2);
INSERT INTO identity_decision VALUES(3,'bind',3,NULL,NULL,NULL,'new',1,1,NULL,NULL,NULL,'{}',0,'resolver',NULL,NULL,0,2);
INSERT INTO identity_decision VALUES(4,'bind',4,NULL,NULL,NULL,'new',1,1,NULL,NULL,NULL,'{}',0,'resolver',NULL,NULL,0,2);
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
  AND sr.audio_asset_id IS NOT NULL
;
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
CREATE TRIGGER observation_correction_kind
BEFORE INSERT ON observation_correction
WHEN (SELECT kind FROM history_observation WHERE id = NEW.observation_id) IS NOT 'correction'
BEGIN
  SELECT RAISE(ABORT, 'observation_correction holds the detail of a correction and of nothing else');
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
CREATE TRIGGER observation_listen_kind
BEFORE INSERT ON observation_listen
WHEN (SELECT kind FROM history_observation WHERE id = NEW.observation_id) IS NOT 'listen'
BEGIN
  SELECT RAISE(ABORT, 'observation_listen holds the detail of a listen and of nothing else');
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
CREATE UNIQUE INDEX source_item_live_key
  ON source_item (source_instance_id, item_kind, provider_key)
  WHERE retired_at_ms IS NULL;
CREATE UNIQUE INDEX metadata_edit_in_force
  ON metadata_edit (entity_kind, entity_id, field, ordinal)
  WHERE withdrawn_at_ms IS NULL;
CREATE UNIQUE INDEX assignment_current
  ON assignment (source_item_id)
  WHERE superseded_at_ms IS NULL;
CREATE UNIQUE INDEX history_endpoint_player ON history_endpoint (server_id) WHERE kind = 'player';
CREATE UNIQUE INDEX history_endpoint_manual ON history_endpoint (kind) WHERE kind = 'manual';
CREATE INDEX history_observation_source_item ON history_observation (source_item_id);
CREATE INDEX correction_target_target ON correction_target (target_observation_id);
CREATE INDEX play_event_observation_observation ON play_event_observation (observation_id);
CREATE INDEX track_appearance_release ON track_appearance (release_id);
CREATE INDEX release_release_group ON release (release_group_id);
CREATE INDEX release_display_artist_credit ON release (display_artist_credit_id);
CREATE INDEX assignment_current_appearance
  ON assignment (appearance_id)
  WHERE superseded_at_ms IS NULL;
CREATE INDEX revision_picture_picture ON revision_picture (picture_id);
CREATE INDEX observation_listen_continues ON observation_listen (continues_source_key)
  WHERE continues_source_key IS NOT NULL;
CREATE INDEX play_event_recent ON play_event (listener_id, event_at_utc_ms DESC, public_id DESC);
CREATE INDEX play_event_counted ON play_event (listener_id, recording_id, event_at_utc_ms DESC)
  WHERE counts = 1 AND excluded = 0;
CREATE INDEX play_event_on_this_day ON play_event (listener_id, month_day, local_date);
CREATE INDEX observation_import_played_at ON observation_import (played_at_utc_ms);
CREATE INDEX history_delivery_echo ON history_delivery (endpoint_id, sent_timestamp_s);
CREATE INDEX history_delivery_due ON history_delivery (endpoint_id, next_attempt_at_ms)
  WHERE state = 'pending';
PRAGMA writable_schema=OFF;
COMMIT;
