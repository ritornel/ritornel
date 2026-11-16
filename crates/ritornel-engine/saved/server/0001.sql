-- A server.db as migration 0001 left it, with a server's identity and the
-- local credential's session in it: what `sqlite3 server.db .dump` printed for
-- a database the program made at that schema. A test loads it and migrates it
-- forward, so a migration that cannot take an old database on is caught
-- (docs/design/server.md, "Testing and CI").
--
-- Nothing here is real. The clock was fixed and the random bytes counted up
-- from 00, so the signing key is 00 to 1f and the session's token is the hex
-- digits of 2a to 49.
PRAGMA foreign_keys=OFF;
BEGIN TRANSACTION;
CREATE TABLE schema_migration (
  version        INTEGER PRIMARY KEY,
  name           TEXT NOT NULL,
  checksum       BLOB NOT NULL,
  applied_at_ms  INTEGER NOT NULL
) STRICT;
INSERT INTO schema_migration VALUES(1,'0001_identity',X'a9c16ff49737779be6423a76d68a42a3c76bc81d94fc8dc0ed2afcf94cdb5837',1763000000000);
CREATE TABLE server_identity (
  id             INTEGER PRIMARY KEY CHECK (id = 1),
  server_id      BLOB NOT NULL CHECK (length(server_id) = 16),
  signing_key    BLOB NOT NULL CHECK (length(signing_key) = 32),   -- Ed25519 private key; never leaves this file
  public_key     BLOB NOT NULL CHECK (length(public_key) = 32),    -- what clients and peers pin
  name           TEXT NOT NULL,
  role           TEXT NOT NULL DEFAULT 'unassigned' CHECK (role IN ('unassigned', 'hub', 'satellite')),   -- this server's own claim; the library's pointer decides
  library_id     BLOB CHECK (length(library_id) = 16),             -- the library it claims or is linked to
  created_at_ms  INTEGER NOT NULL
) STRICT;
INSERT INTO server_identity VALUES(1,X'019a7afd1e007021a223242526272829',X'000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f',X'03a107bff3ce10be1d70dd18e74bc09967e4d6309ba50d5f1ddc8664125531b8','desk','unassigned',NULL,1763000000000);
CREATE TABLE server_setting (
  id          INTEGER PRIMARY KEY,
  key         TEXT NOT NULL UNIQUE,
  value_json  TEXT NOT NULL
) STRICT;
INSERT INTO server_setting VALUES(1,'local_credential_session_id','1');
CREATE TABLE session (
  id                  INTEGER PRIMARY KEY,
  public_id           BLOB NOT NULL UNIQUE CHECK (length(public_id) = 16),   -- the actor on every command this session sends
  subject_kind        TEXT NOT NULL CHECK (subject_kind IN ('person', 'agent', 'server')),
  label               TEXT NOT NULL,                                         -- display text; never decides access
  listener_pid        BLOB CHECK (length(listener_pid) = 16),                -- public ID of a library listener
  kind                TEXT NOT NULL CHECK (kind IN ('full', 'limited')),     -- 'limited' may read the library and control playback, and may change or delete nothing
  token_hash          BLOB NOT NULL UNIQUE,                                  -- hash of the session token
  media_token_hash    BLOB UNIQUE,                                           -- hash of the media-only token from media.token (contract.md)
  created_at_ms       INTEGER NOT NULL,
  revoked_at_ms       INTEGER                                                -- sessions end by revocation, not by time
) STRICT;
INSERT INTO session VALUES(1,X'019a7afd1e007a4b8c4d4e4f50515253','person','local owner',NULL,'full',X'bdd44d520c6f39dfecdcd2122a900e0c18dc125d688b0f26379d574e8f9c533f',NULL,1763000000000,NULL);
COMMIT;
