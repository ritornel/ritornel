-- Data from an invented library at schema 1, loaded after its shipped migration.
INSERT INTO server (id, public_id, name, created_at_ms, rev)
VALUES (1, X'11111111111111111111111111111111', 'desk', 1, 1);
INSERT INTO library_meta (id, library_id, hub_server_id, hub_epoch, rev, created_at_ms)
VALUES (1, X'22222222222222222222222222222222', 1, 1, 1, 1);
INSERT INTO search_text (rowid, title, artist, album) VALUES (7, 'Quæł', 'Sølen', 'Błæ');
