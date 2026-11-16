-- =============================================================================
-- Ritornel server.db, migration 0001: the migration ledger, this server's
-- identity, its settings and its sessions.
-- Every server has one server.db. It describes this machine and never follows
-- the library. The tables are the M0 part of docs/design/schema/server.sql.
--
-- This file is never edited once it has shipped: the runner keeps a checksum
-- of every migration it applied and refuses to start if one has changed. A
-- change to the schema is a new numbered file.
--
-- Conventions
--   * Every table is STRICT with one integer primary key. Anything addressed
--     from outside has a 16-byte UUIDv7 `public_id`.
--   * A column named `*_pid` refers to a library row by its public ID. No foreign
--     key can reach it: the two files cannot commit together (server.md
--     decision 14), and a satellite has no library.db at all. A reference to a
--     row in this file is an integer `*_id` with a foreign key.
--   * The tag line above each table is `-- [area|class]`. Everything here is
--     durable server-local state; nothing is derived.
--   * `*_ms` is Unix milliseconds in UTC, or a duration. Flags are INTEGER 0 or 1.
--   * Closed value lists are CHECK constraints, written out once in schema.md
--     "Value lists".
-- =============================================================================

-- [meta|bookkeeping] One applied migration, written by the migration runner. server.md decision 19.
CREATE TABLE schema_migration (
  version        INTEGER PRIMARY KEY,
  name           TEXT NOT NULL,
  checksum       BLOB NOT NULL,
  applied_at_ms  INTEGER NOT NULL
) STRICT;

-- [identity|durable] The single row that is this server: its ID and signing key, created together on first start and never changed. linking.md decision 1.
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

-- [identity|durable] One setting about this machine, such as the cache size limit. server.md decision 26.
CREATE TABLE server_setting (
  id          INTEGER PRIMARY KEY,
  key         TEXT NOT NULL UNIQUE,
  value_json  TEXT NOT NULL
) STRICT;

-- [identity|durable] One credential the server issued: the local owner's, the desktop's, or an agent's. Only hashes are kept. It lands in M0, with the local bootstrap credential. linking.md decisions 4, 5, 6; README "Sessions".
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
