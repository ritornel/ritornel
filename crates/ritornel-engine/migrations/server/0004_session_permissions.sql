-- Per-token scoped permissions and named roles on `session`. See "Local
-- credentials, sessions, and tokens" in docs/internals/linking.md. Once
-- shipped, this migration is immutable.

ALTER TABLE session ADD COLUMN role TEXT CHECK (
  role IS NULL OR role IN ('owner', 'personal', 'player', 'guest', 'monitor')
);

ALTER TABLE session ADD COLUMN permissions_json TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(permissions_json));

-- Backfill sessions created before scoped permissions were stored:
--   * Full non-server sessions (such as the local owner credential) become Owner.
--   * Linked satellite sessions become Personal (no hub folders, server, services,
--     or access management).
--   * Limited sessions become Guest (library read and playback control).
UPDATE session
SET role = 'owner',
    permissions_json = '["library:read","library:write","playback:control","status:read","folders:manage","server:manage","services:connect","access:manage"]'
WHERE kind = 'full' AND subject_kind != 'server';

UPDATE session
SET role = 'personal',
    permissions_json = '["library:read","library:write","playback:control","status:read"]'
WHERE kind = 'full' AND subject_kind = 'server';

UPDATE session
SET role = 'guest',
    permissions_json = '["library:read","playback:control"]'
WHERE kind = 'limited';
