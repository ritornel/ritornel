-- =============================================================================
-- Ritornel library.db, migration 0007: which server sent each scrobble.
--
-- Today only the hub sends scrobbles. A satellite may later send one itself
-- when the hub has been away too long for the service to take it, and the
-- hub must then honour what the satellite sent. The row says who sent it,
-- so nothing stored changes when that comes.
--
-- This file is never edited once it has shipped: the runner keeps a checksum
-- of every migration it applied and refuses to start if one has changed. A
-- change to the schema is a new numbered file.
-- =============================================================================

-- The server that sent the scrobble to the service, set when it is taken to be sent. NULL for one not sent yet, and for one sent before this was kept, which the hub sent.
ALTER TABLE history_delivery ADD COLUMN sent_by_server_id INTEGER REFERENCES server(id);
