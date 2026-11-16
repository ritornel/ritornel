-- A library.db as migration 0001 left it, with a library in it and a scan of
-- three of the audio fixtures: one song in a FLAC and an MP3 of one record,
-- with the FLAC's embedded cover, and the same audio retagged as a loose song
-- by someone else. It is what `sqlite3 library.db .dump` printed for a
-- database the program made at that schema. A test loads it and migrates it
-- forward, so a migration that cannot take an old database on is caught
-- (docs/design/server.md, "Testing and CI").
--
-- Nothing here is real. The tags are the fixtures' invented ones, the clock
-- was fixed and the random bytes counted up from 00. The search index is an
-- FTS5 table, which the dump writes through `writable_schema`.
/* WARNING: Script requires that SQLITE_DBCONFIG_DEFENSIVE be disabled */
PRAGMA foreign_keys=OFF;
BEGIN TRANSACTION;
CREATE TABLE schema_migration (
  version        INTEGER PRIMARY KEY,
  name           TEXT NOT NULL,
  checksum       BLOB NOT NULL,     -- hash of the migration file; the server refuses to start if a shipped one changed
  applied_at_ms  INTEGER NOT NULL
) STRICT;
INSERT INTO schema_migration VALUES(1,'0001_library',X'5a72aa0a6f7da030ece148d3b4669da867ca722f4c659f5600b47fd648100b45',1763000000000);
CREATE TABLE server (
  id                INTEGER PRIMARY KEY,
  public_id         BLOB NOT NULL UNIQUE CHECK (length(public_id) = 16),   -- the server's own ID from its server.db
  name              TEXT NOT NULL,                                         -- untrusted display text
  created_at_ms     INTEGER NOT NULL,
  rev               INTEGER NOT NULL
) STRICT;
INSERT INTO server VALUES(1,X'11111111111111111111111111111111','desk',1763000000000,1);
CREATE TABLE library_meta (
  id                INTEGER PRIMARY KEY CHECK (id = 1),
  library_id        BLOB NOT NULL CHECK (length(library_id) = 16),
  hub_server_id     INTEGER REFERENCES server(id),   -- NULL means released: no server may write
  hub_epoch         INTEGER NOT NULL,                -- raised by every release and adopt
  rev               INTEGER NOT NULL,                -- the revision counter itself: this row changes in every write transaction
  created_at_ms     INTEGER NOT NULL
) STRICT;
INSERT INTO library_meta VALUES(1,X'019a7afd1e0070018203040506070809',1,1,4,1763000000000);
CREATE TABLE library_revision (
  rev                INTEGER PRIMARY KEY,
  command_id         BLOB CHECK (length(command_id) = 16),          -- NULL for writes with no receipt, such as a history ingest
  method             TEXT NOT NULL,                                 -- the contract call or internal writer, for example 'scan.step'
  client_session_pid BLOB CHECK (length(client_session_pid) = 16),  -- session public ID in the handling server's server.db; NULL for internal work
  client_kind        TEXT CHECK (client_kind IN ('person', 'agent', 'server')),   -- the session's subject kind
  committed_at_ms    INTEGER NOT NULL
) STRICT;
INSERT INTO library_revision VALUES(1,NULL,'library.create',NULL,NULL,1763000000000);
INSERT INTO library_revision VALUES(2,X'01010101010101010101010101010101','sources.add',NULL,NULL,1763000000000);
INSERT INTO library_revision VALUES(3,X'eb4b1a51547549b7f45674e65ec7d2cf','scan.step',NULL,NULL,1763000001000);
INSERT INTO library_revision VALUES(4,X'e0ae0ca249479f95e6ea9f8146af3261','scan.step',NULL,NULL,1763000001000);
CREATE TABLE library_command (
  id                   INTEGER PRIMARY KEY,
  command_id           BLOB NOT NULL UNIQUE CHECK (length(command_id) = 16),   -- minted by the client; a job step derives its 16 bytes from the job's ID and the step number
  request_fingerprint  BLOB NOT NULL,                                          -- same ID with another fingerprint is refused
  method               TEXT NOT NULL,
  result_json          TEXT NOT NULL,                                          -- the result to replay on a retry
  rev                  INTEGER NOT NULL REFERENCES library_revision(rev),
  created_at_ms        INTEGER NOT NULL
) STRICT;
INSERT INTO library_command VALUES(1,X'01010101010101010101010101010101',X'2b2e60338d8bd8a6199840ea8047b422749e2cb18b19716b637125adae24187a','sources.add','019a7afd-1e00-7233-b435-363738393a3b',2,1763000000000);
INSERT INTO library_command VALUES(2,X'eb4b1a51547549b7f45674e65ec7d2cf',X'c07c242575b831468790da2ee39ae1940ae5c21cda0e84c5886a7adadef057ab','scan.step','3',3,1763000001000);
INSERT INTO library_command VALUES(3,X'e0ae0ca249479f95e6ea9f8146af3261',X'25c7b7bf86b933c2d57712b8f7e7ce3db50278186cd21d008a777fc441da9063','scan.step','{"assignments":3,"recordings":1,"releases":1,"displayed":0}',4,1763000001000);
CREATE TABLE library_setting (
  id                INTEGER PRIMARY KEY,
  key               TEXT NOT NULL UNIQUE,
  value_json        TEXT NOT NULL,
  rev               INTEGER NOT NULL
) STRICT;
INSERT INTO library_setting VALUES(1,'identity.versions','{"extractor":2,"resolver":1,"normalizer":1}',4);
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
INSERT INTO artist VALUES(1,X'019a7afd21e872838485868788898a8b',NULL,1,'Somebody Else','Somebody Else','somebody else',4);
INSERT INTO artist VALUES(2,X'019a7afd21e87c8d8e8f909192939495',NULL,2,'Moss Arcade','Moss Arcade','moss arcade',4);
CREATE TABLE artist_credit (
  id                INTEGER PRIMARY KEY,
  display_text      TEXT NOT NULL,   -- the whole credit as one string
  rev               INTEGER NOT NULL
) STRICT;
INSERT INTO artist_credit VALUES(1,'Somebody Else',4);
INSERT INTO artist_credit VALUES(2,'Moss Arcade',4);
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
INSERT INTO artist_credit_name VALUES(1,1,0,1,'Somebody Else','',4);
INSERT INTO artist_credit_name VALUES(2,2,0,2,'Moss Arcade','',4);
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
INSERT INTO recording VALUES(1,X'019a7afd21e8769798999a9b9c9d9e9f',NULL,1,'Paper Kites',1,4);
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
INSERT INTO release_group VALUES(1,X'019a7afd21e870a1a2a3a4a5a6a7a8a9',NULL,NULL,1,'ep','[]','2004-10-11',2004,4);
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
INSERT INTO "release" VALUES(1,X'019a7afd21e87aabacadaeafb0b1b2b3',NULL,1,NULL,1,'Tin Weather',2,'2019-03-08',2019,1,4);
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
INSERT INTO track_appearance VALUES(1,X'019a7afd21e874b5b6b7b8b9babbbcbd',1,NULL,NULL,1,'Another Name Entirely',1,NULL,NULL,500,1763000001000,4);
INSERT INTO track_appearance VALUES(2,X'019a7afd21e87ebf80c1c2c3c4c5c6c7',1,1,NULL,2,'Paper Kites',2,1,2,500,1763000001000,4);
INSERT INTO track_appearance VALUES(3,X'019a7afd21e878c98acbcccdcecfd0d1',1,1,NULL,3,'Paper Kites',2,NULL,2,500,1763000001000,4);
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
INSERT INTO library_root VALUES(1,X'019a7afd1e007829aa2b2c2d2e2f3031',1,'main','master',0,0,1763000000000,2);
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
INSERT INTO source_instance VALUES(1,X'019a7afd1e007233b435363738393a3b','folder','main',1,1763000000000,2);
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
INSERT INTO source_item VALUES(1,X'019a7afd21e8764788494a4b4c4d4e4f',1,'file','Elsewhere/03.flac',1,1,11373,1700000240000,NULL,NULL,1763000001000,4);
INSERT INTO source_item VALUES(2,X'019a7afd21e87a5b9c5d5e5f60616263',1,'file','Moss Arcade/Tin Weather/01.flac',2,1,11916,1700000120000,NULL,NULL,1763000001000,4);
INSERT INTO source_item VALUES(3,X'019a7afd21e87e6fb071727374757677',1,'file','Moss Arcade/Tin Weather/02.mp3',3,1,2763,1700000180000,NULL,NULL,1763000001000,4);
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
INSERT INTO source_item_revision VALUES(1,1,1,X'4f59d9443388b662601857faca7c6b2fb3744d654a1862f632b76097dbcf856e','{"v":1,"path":"Elsewhere/03.flac","size":11373,"mtime_ms":1700000240000,"container":"flac","audio":{"hash_scheme":1,"hash":"9bc10212a894e26f8a4861671767910516f571472def328d116d24a0290c10da","codec":"flac","sample_rate":16000,"channels":1,"bits":16,"duration_ms":500,"packets":8,"payload_bytes":3008,"flac_md5":"9ec1a6a231f8395ec74e464a05dc8a9c"},"tags":[{"key":"title","block":"flac","values":["Another Name Entirely"]},{"key":"artist","block":"flac","values":["Somebody Else"]},{"key":"DESCRIPTION","block":"flac","values":["retagged"]},{"key":"encoder","block":"flac","values":["Lavf63.1.102"]}],"pictures":[],"folder_images":[]}',1,1763000001000,3);
INSERT INTO source_item_revision VALUES(2,2,1,X'846050c9075248fbb95795da4f3b4f1162850d10189a8ecd82c0f3f4cf84ac3d','{"v":1,"path":"Moss Arcade/Tin Weather/01.flac","size":11916,"mtime_ms":1700000120000,"container":"flac","audio":{"hash_scheme":1,"hash":"9bc10212a894e26f8a4861671767910516f571472def328d116d24a0290c10da","codec":"flac","sample_rate":16000,"channels":1,"bits":16,"duration_ms":500,"packets":8,"payload_bytes":3008,"flac_md5":"9ec1a6a231f8395ec74e464a05dc8a9c"},"tags":[{"key":"X_INVENTED","block":"flac","values":["not understood, and kept"]},{"key":"title","block":"flac","values":["Paper Kites"]},{"key":"artist","block":"flac","values":["Moss Arcade"]},{"key":"album","block":"flac","values":["Tin Weather"]},{"key":"ALBUMARTIST","block":"flac","values":["Moss Arcade"]},{"key":"TRACKNUMBER","block":"flac","values":["2/9"]},{"key":"TRACKNUMBER","block":"flac","values":["2/9"]},{"key":"DISCNUMBER","block":"flac","values":["1/1"]},{"key":"DISCNUMBER","block":"flac","values":["1/1"]},{"key":"date","block":"flac","values":["2019-03-08"]},{"key":"ORIGINALDATE","block":"flac","values":["2004-10-11"]},{"key":"RELEASETYPE","block":"flac","values":["ep"]},{"key":"MUSICBRAINZ_TRACKID","block":"flac","values":["11111111-2222-4333-8444-555555555555"]},{"key":"MUSICBRAINZ_ALBUMID","block":"flac","values":["aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"]},{"key":"MUSICBRAINZ_RELEASEGROUPID","block":"flac","values":["99999999-8888-4777-8666-555555555555"]},{"key":"MUSICBRAINZ_ARTISTID","block":"flac","values":["12121212-3434-4565-8787-909090909090"]},{"key":"encoder","block":"flac","values":["Lavf63.1.102"]}],"pictures":[{"hash":"0ebc8ace303749fdba9171f4656e34ca5145e30c04899d16af582a192cfb64be","mime":"image/png","width":8,"height":8,"size":98,"kind":0}],"folder_images":[]}',1,1763000001000,3);
INSERT INTO source_item_revision VALUES(3,3,1,X'e05a939fa4f5a6662ee6e88721e2be9dba1b9952f610c2e66bc64b648b731c69','{"v":1,"path":"Moss Arcade/Tin Weather/02.mp3","size":2763,"mtime_ms":1700000180000,"container":"mp3","audio":{"hash_scheme":1,"hash":"1459d102a13cf68edec2de615d73cb254511c468e6f365f49ebb3c7c54d586c2","codec":"mp3","sample_rate":16000,"channels":1,"bits":null,"duration_ms":500,"packets":16,"payload_bytes":2304},"tags":[{"key":"TXXX","block":"id3v2","sub":"MusicBrainz Album Id","values":["aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"]},{"key":"TIT2","block":"id3v2","values":["Paper Kites"]},{"key":"TPE1","block":"id3v2","values":["Moss Arcade"]},{"key":"TALB","block":"id3v2","values":["Tin Weather"]},{"key":"TRCK","block":"id3v2","values":["2"]},{"key":"TPE2","block":"id3v2","values":["Moss Arcade"]},{"key":"TDOR","block":"id3v2","values":["2004"]},{"key":"TXXX","block":"id3v2","sub":"MusicBrainz Album Type","values":["album/live"]},{"key":"TSSE","block":"id3v2","values":["Lavf63.1.102"]}],"pictures":[],"folder_images":[]}',2,1763000001000,3);
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
INSERT INTO identifier_claim VALUES(1,2,2,2,'recording','mb_recording','11111111-2222-4333-8444-555555555555',0,4,3);
INSERT INTO identifier_claim VALUES(2,2,2,2,'release','mb_release','aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee',0,4,3);
INSERT INTO identifier_claim VALUES(3,2,2,2,'release_group','mb_release_group','99999999-8888-4777-8666-555555555555',0,4,3);
INSERT INTO identifier_claim VALUES(4,2,2,2,'artist','mb_artist','12121212-3434-4565-8787-909090909090',0,4,3);
INSERT INTO identifier_claim VALUES(5,3,3,2,'release','mb_release','aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee',0,4,3);
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
INSERT INTO metadata_claim VALUES(1,1,1,2,'track_appearance','title','Another Name Entirely',0,4,3);
INSERT INTO metadata_claim VALUES(2,1,1,2,'track_appearance','artist','Somebody Else',0,4,3);
INSERT INTO metadata_claim VALUES(3,1,1,2,'track_appearance','duration_ms','500',0,4,3);
INSERT INTO metadata_claim VALUES(4,1,1,2,'track_appearance','title','03',0,6,3);
INSERT INTO metadata_claim VALUES(5,2,2,2,'track_appearance','title','Paper Kites',0,4,3);
INSERT INTO metadata_claim VALUES(6,2,2,2,'track_appearance','artist','Moss Arcade',0,4,3);
INSERT INTO metadata_claim VALUES(7,2,2,2,'track_appearance','track_number','2',0,4,3);
INSERT INTO metadata_claim VALUES(8,2,2,2,'track_appearance','track_total','9',0,4,3);
INSERT INTO metadata_claim VALUES(9,2,2,2,'track_appearance','disc_number','1',0,4,3);
INSERT INTO metadata_claim VALUES(10,2,2,2,'track_appearance','disc_total','1',0,4,3);
INSERT INTO metadata_claim VALUES(11,2,2,2,'track_appearance','duration_ms','500',0,4,3);
INSERT INTO metadata_claim VALUES(12,2,2,2,'release','album','Tin Weather',0,4,3);
INSERT INTO metadata_claim VALUES(13,2,2,2,'release','album_artist','Moss Arcade',0,4,3);
INSERT INTO metadata_claim VALUES(14,2,2,2,'release','date','2019-03-08',0,4,3);
INSERT INTO metadata_claim VALUES(15,2,2,2,'release_group','primary_type','ep',0,4,3);
INSERT INTO metadata_claim VALUES(16,2,2,2,'release_group','original_date','2004-10-11',0,4,3);
INSERT INTO metadata_claim VALUES(17,2,2,2,'track_appearance','title','01',0,6,3);
INSERT INTO metadata_claim VALUES(18,3,3,2,'track_appearance','title','Paper Kites',0,4,3);
INSERT INTO metadata_claim VALUES(19,3,3,2,'track_appearance','artist','Moss Arcade',0,4,3);
INSERT INTO metadata_claim VALUES(20,3,3,2,'track_appearance','track_number','2',0,4,3);
INSERT INTO metadata_claim VALUES(21,3,3,2,'track_appearance','duration_ms','500',0,4,3);
INSERT INTO metadata_claim VALUES(22,3,3,2,'release','album','Tin Weather',0,4,3);
INSERT INTO metadata_claim VALUES(23,3,3,2,'release','album_artist','Moss Arcade',0,4,3);
INSERT INTO metadata_claim VALUES(24,3,3,2,'release_group','primary_type','album',0,4,3);
INSERT INTO metadata_claim VALUES(25,3,3,2,'release_group','secondary_type','live',0,4,3);
INSERT INTO metadata_claim VALUES(26,3,3,2,'release_group','original_date','2004',0,4,3);
INSERT INTO metadata_claim VALUES(27,3,3,2,'track_appearance','title','02',0,6,3);
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
INSERT INTO identity_decision VALUES(1,'bind',1,NULL,NULL,NULL,'new',1,1,0.0,NULL,NULL,'{"claims":[["title","another name entirely"],["artist","somebody else"],["duration_ms","500"]],"candidates":[],"contradictions":[]}',0,'resolver',NULL,NULL,1763000001000,4);
INSERT INTO identity_decision VALUES(2,'bind',2,NULL,NULL,NULL,'same_audio',1,1,0.0,NULL,NULL,'{"claims":[["audio","1"]],"candidates":[],"contradictions":[]}',0,'resolver',NULL,NULL,1763000001000,4);
INSERT INTO identity_decision VALUES(3,'bind',3,NULL,NULL,NULL,'tags_duration',1,1,0.0,NULL,'{"title":0.0,"artist":0.0,"duration":0.0,"album":0.0,"shared_identifier":false}','{"claims":[["title","paper kites"],["artist","moss arcade"],["album","tin weather"],["duration_ms","500"]],"candidates":[{"item":2,"distance":0.0}],"contradictions":[]}',0,'resolver',NULL,NULL,1763000001000,4);
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
INSERT INTO assignment VALUES(1,1,1,1,1,1763000001000,NULL,NULL,4);
INSERT INTO assignment VALUES(2,2,1,2,2,1763000001000,NULL,NULL,4);
INSERT INTO assignment VALUES(3,3,1,3,3,1763000001000,NULL,NULL,4);
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
INSERT INTO alias_observation VALUES(1,'title','another name entirely','paper kites',1,2,1763000001000,4);
INSERT INTO alias_observation VALUES(2,'artist','moss arcade','somebody else',1,2,1763000001000,4);
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
INSERT INTO audio_asset VALUES(1,X'019a7afd21e870519253545556575859',1,X'9bc10212a894e26f8a4861671767910516f571472def328d116d24a0290c10da',X'9ec1a6a231f8395ec74e464a05dc8a9c','flac',16000,'mono',16,NULL,1,500,3);
INSERT INTO audio_asset VALUES(2,X'019a7afd21e87879ba7b7c7d7e7f8081',1,X'1459d102a13cf68edec2de615d73cb254511c468e6f365f49ebb3c7c54d586c2',NULL,'mp3',16000,'mono',NULL,36,0,500,3);
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
INSERT INTO picture VALUES(1,X'019a7afd21e87465a66768696a6b6c6d',X'0ebc8ace303749fdba9171f4656e34ca5145e30c04899d16af582a192cfb64be','image/png',8,8,98,3);
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
INSERT INTO revision_picture VALUES(1,2,1,0,'embedded',NULL,0,3);
PRAGMA writable_schema=ON;
INSERT INTO sqlite_schema(type,name,tbl_name,rootpage,sql)VALUES('table','search_text','search_text',0,'CREATE VIRTUAL TABLE search_text USING fts5(
  title,
  artist,
  album,
  tokenize = ''trigram remove_diacritics 1''
)');
CREATE TABLE IF NOT EXISTS 'search_text_data'(id INTEGER PRIMARY KEY, block BLOB);
INSERT INTO search_text_data VALUES(1,X'03251d12');
INSERT INTO search_text_data VALUES(10,X'000000000101010001010101');
INSERT INTO search_text_data VALUES(137438953473,X'000002440430206172020601010601060101060202656c010601010a03016e01020e02026b6902020701020702026e6101020902027765020601020501060102050103616465020601010a010601010a02026d6501020b02026e6f01020202027065020203010203020272630206010107010601010702027468020601020801060102080103626f64010601010601036361640206010109010601010901036479200106010108010365206501020d02026174020601020701060102070202626f010601010502026c73010601010b03017901021402026e7401020f020272200102070102050102050103686572010206010601020a010601020a0103696e2002060102030106010203020272650102120202746502020901020901036b697402020801020801036c7365010601010c01036d652001020c030162010601010402026f730206010102010601010201036e2077020601020401060102040202616d01020a02026f740102030202746901021001036f6479010601010702026d650106010103020273730206010103010601010302027468010204010370617002020201020202026572020204010204010372206b02020601020603016e01020802026361020601010801060101080202656c01021301037320610206010105010601010502026f6d01060101020202732002060101040106010104010374657302020a01020a02026865010205010601020901060102090202696e0206010202010601020203017201021101037765610206010206010601020601037920650106010109040f09060a070e0f07070a0e0e0a0f0a080e090906070d120f070a0b0a08080e0f0707070a090e070b0a0b060e070f090e0b110e060f');
CREATE TABLE IF NOT EXISTS 'search_text_idx'(segid, term, pgno, PRIMARY KEY(segid, term)) WITHOUT ROWID;
INSERT INTO search_text_idx VALUES(1,X'',2);
CREATE TABLE IF NOT EXISTS 'search_text_content'(id INTEGER PRIMARY KEY, c0, c1, c2);
INSERT INTO search_text_content VALUES(1,'Another Name Entirely','Somebody Else','');
INSERT INTO search_text_content VALUES(2,'Paper Kites','Moss Arcade','Tin Weather');
INSERT INTO search_text_content VALUES(3,'Paper Kites','Moss Arcade','Tin Weather');
CREATE TABLE IF NOT EXISTS 'search_text_docsize'(id INTEGER PRIMARY KEY, sz BLOB);
INSERT INTO search_text_docsize VALUES(1,X'130b00');
INSERT INTO search_text_docsize VALUES(2,X'090909');
INSERT INTO search_text_docsize VALUES(3,X'090909');
CREATE TABLE IF NOT EXISTS 'search_text_config'(k PRIMARY KEY, v) WITHOUT ROWID;
INSERT INTO search_text_config VALUES('version',4);
CREATE TABLE listener (
  id                INTEGER PRIMARY KEY,
  public_id         BLOB NOT NULL UNIQUE CHECK (length(public_id) = 16),
  name              TEXT NOT NULL,
  rev               INTEGER NOT NULL
) STRICT;
INSERT INTO listener VALUES(1,X'019a7afd1e007a0b8c0d0e0f10111213','owner',1);
CREATE TABLE history_endpoint (
  id                INTEGER PRIMARY KEY,
  public_id         BLOB NOT NULL UNIQUE CHECK (length(public_id) = 16),
  kind              TEXT NOT NULL CHECK (kind IN ('player', 'device_log', 'service', 'import', 'manual')),   -- 'player' and 'manual' in M1
  server_id         INTEGER REFERENCES server(id),   -- a player's server
  name              TEXT NOT NULL,
  rev               INTEGER NOT NULL,
  CHECK (kind <> 'player' OR server_id IS NOT NULL)
) STRICT;
INSERT INTO history_endpoint VALUES(1,X'019a7afd1e007415961718191a1b1c1d','player',1,'player',1);
INSERT INTO history_endpoint VALUES(2,X'019a7afd1e007e1fa021222324252627','manual',NULL,'corrections',1);
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
CREATE TABLE play_event_observation (
  id              INTEGER PRIMARY KEY,
  play_event_id   INTEGER NOT NULL REFERENCES play_event(id),
  observation_id  INTEGER NOT NULL REFERENCES history_observation(id),
  role            TEXT NOT NULL CHECK (role IN ('primary', 'corroborating')),   -- the primary supplies time, played time and outcome
  rev             INTEGER NOT NULL,
  UNIQUE (play_event_id, observation_id)
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
CREATE INDEX play_event_recent ON play_event (listener_id, event_at_utc_ms DESC);
CREATE INDEX play_event_counted ON play_event (listener_id, recording_id, event_at_utc_ms DESC)
  WHERE counts = 1 AND excluded = 0;
CREATE INDEX play_event_on_this_day ON play_event (listener_id, month_day, local_date);
CREATE INDEX play_event_observation_observation ON play_event_observation (observation_id);
PRAGMA writable_schema=OFF;
COMMIT;
