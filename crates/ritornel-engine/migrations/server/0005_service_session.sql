-- =============================================================================
-- Ritornel server.db, migration 0005: where this server keeps its session at
-- an outside service, Last.fm first, and whether it scrobbles there.
--
-- The session key is a secret that does not expire and can change the
-- account, so it lives here, in the file that never leaves this machine, and
-- nowhere in library.db. The account it belongs to is a `service_account` in
-- library.db, named by its public ID. The API key and secret are not here:
-- they are the server's own, from its environment or a file in its data
-- directory (docs/design/history.md, "Last.fm").
--
-- The conventions are those of migration 0001. This file is never edited once
-- it has shipped.
-- =============================================================================

-- [service|durable] This server's session at an outside service: one per service. A row is written when the account is signed in and removed when it is signed out.
CREATE TABLE service_session (
  id                  INTEGER PRIMARY KEY,
  service             TEXT NOT NULL UNIQUE CHECK (service IN ('lastfm')),
  account_pid         BLOB NOT NULL CHECK (length(account_pid) = 16),   -- the service_account in library.db
  account_name        TEXT NOT NULL,                                    -- as the service spells it
  session_key         TEXT NOT NULL,                                    -- a secret: never logged, never sent to a client
  subscriber          INTEGER NOT NULL CHECK (subscriber IN (0, 1)),
  signed_in_at_ms     INTEGER NOT NULL,
  scrobble_from_ms    INTEGER,                                          -- plays from this time on are sent; NULL sends none
  sign_in_again       INTEGER NOT NULL DEFAULT 0 CHECK (sign_in_again IN (0, 1))   -- the service refused the session; nothing is sent until a new sign-in
) STRICT;
