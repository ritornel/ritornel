-- =============================================================================
-- Ritornel server.db, migration 0002: the link between a hub and a
-- satellite. On the satellite, the one row that says which hub it is linked
-- to and how to reach it. On the hub, which satellite each link session
-- belongs to, and the key it proved. See "The link between the hub and a satellite" in
-- docs/design/linking.md.
--
-- This file is never edited once it has shipped: the runner keeps a checksum
-- of every migration it applied and refuses to start if one has changed. A
-- change to the schema is a new numbered file.
-- =============================================================================

-- [identity|durable] On a satellite, the single row that is its link to its hub: the hub's ID and key, pinned when it joined, the library it holds and its epoch then, the routes to try in order, and the link session's token. A hub has no row. linking.md decision 9 and "The link between the hub and a satellite".
-- The token is a credential the hub issued, which this server presents to it; like the credential file, it is readable by whoever can read the data directory.
CREATE TABLE hub_link (
  id                 INTEGER PRIMARY KEY CHECK (id = 1),
  hub_server_id      BLOB NOT NULL CHECK (length(hub_server_id) = 16),
  hub_public_key     BLOB NOT NULL CHECK (length(hub_public_key) = 32),   -- checked against the descriptor's proof on every connection
  hub_name           TEXT NOT NULL,                                       -- untrusted display text
  library_id         BLOB NOT NULL CHECK (length(library_id) = 16),
  hub_epoch          INTEGER NOT NULL,
  routes_json        TEXT NOT NULL CHECK (json_valid(routes_json)),       -- ["192.168.1.20:4747", ...], tried in order
  token              TEXT NOT NULL,                                       -- the link session's token at the hub: a secret, never logged
  linked_at_ms       INTEGER NOT NULL
) STRICT;

-- On a hub, the satellite a session of subject kind 'server' is the link of, and the public key it proved it holds when it joined. A server ID already linked joins again only with the same key. NULL for every other session.
ALTER TABLE session ADD COLUMN peer_server_pid BLOB CHECK (peer_server_pid IS NULL OR length(peer_server_pid) = 16);
ALTER TABLE session ADD COLUMN peer_public_key BLOB CHECK (peer_public_key IS NULL OR length(peer_public_key) = 32);
