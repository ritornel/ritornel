-- =============================================================================
-- Ritornel library.db, migration 0005: pictures of artists, from a picture
-- folder kept apart from the music (plan.md, open question "Artist
-- pictures").
--
-- - `picture_folder` is one such folder on one server, by name: that server's
--   server.db `root` maps the name to a path, as it does a music root's. The
--   server only ever reads it.
-- - `artist_picture` is the picture the folder's manifest chose for an artist,
--   as a `picture` row, and where in the folder its file is. The file is read
--   in place, as a folder image beside the music is. Each import replaces a
--   folder's rows, so a picture taken out of the folder goes on the next one.
-- - An index on a file's identifier claims, which matching an artist by their
--   MusicBrainz ID reads, as the resolver does.
--
-- The conventions are those of migration 0001. This file is never edited once
-- it has shipped.
-- =============================================================================

-- [catalog|durable] A folder of artist pictures on one server, by the name its server.db `root` maps to a path.
CREATE TABLE picture_folder (
  id                INTEGER PRIMARY KEY,
  public_id         BLOB NOT NULL UNIQUE CHECK (length(public_id) = 16),
  server_id         INTEGER NOT NULL REFERENCES server(id),
  name              TEXT NOT NULL,
  created_at_ms     INTEGER NOT NULL,
  rev               INTEGER NOT NULL,
  UNIQUE (server_id, name)
) STRICT;

-- [catalog|derived] An artist's picture of one kind, as a picture folder's manifest chose it. Rebuilt by importing the folder again.
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

-- A picture served: where in a folder its file is. (covers/mod.rs,
-- `picture_sources`.)
CREATE INDEX artist_picture_picture ON artist_picture (picture_id);

-- The MusicBrainz ID each artist is known by: a file's claims of a namespace,
-- counted for every file. (resolve_store.rs, `artist_mbids`, which an import
-- and every resolution run.) On a real library of 13,000 files it took 30 s
-- without this and 54 ms with it.
CREATE INDEX identifier_claim_item ON identifier_claim (source_item_id, namespace);
