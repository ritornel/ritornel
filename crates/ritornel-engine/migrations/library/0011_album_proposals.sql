-- Album proposals: what MusicBrainz's replies propose each disc of the
-- library is. See "Album proposals" in docs/internals/corrections.md.
--
-- All four tables are derived. They are worked out again from the disc
-- layouts, the current replies and the files whenever either changes, by
-- the proposer version in proposer_version, and nothing reads them but the
-- corrections calls.

-- [evidence|derived] One proposal: that one disc of the library is one
-- album. Its public ID is computed from its kind and its anchor, the
-- first-seen file it is about, so a proposal derived again keeps its ID.
CREATE TABLE proposal (
  id                    INTEGER PRIMARY KEY,
  public_id             BLOB NOT NULL UNIQUE CHECK (length(public_id) = 16),
  kind                  TEXT NOT NULL CHECK (kind IN ('album')),
  anchor_item_id        INTEGER NOT NULL REFERENCES source_item(id),
  group_public_id       BLOB NOT NULL CHECK (length(group_public_id) = 16),   -- the proposal whose anchor is its group's
  source_instance_id    INTEGER NOT NULL REFERENCES source_instance(id),
  folder                TEXT NOT NULL,                                       -- relative to the root
  fingerprint           TEXT NOT NULL,                                       -- a digest of all of it, which a yes names
  proposer_version      INTEGER NOT NULL,
  rev                   INTEGER NOT NULL
) STRICT;

CREATE INDEX proposal_group ON proposal (group_public_id);

-- [evidence|derived] The files a proposal is about.
CREATE TABLE proposal_item (
  id                INTEGER PRIMARY KEY,
  proposal_id       INTEGER NOT NULL REFERENCES proposal(id),
  source_item_id    INTEGER NOT NULL REFERENCES source_item(id),
  rev               INTEGER NOT NULL,
  UNIQUE (proposal_id, source_item_id)
) STRICT;

CREATE INDEX proposal_item_item ON proposal_item (source_item_id);

-- [evidence|derived] One album a proposal's disc may be on, and the tag
-- values a yes to it would put on the files.
CREATE TABLE proposal_candidate (
  id                    INTEGER PRIMARY KEY,
  proposal_id           INTEGER NOT NULL REFERENCES proposal(id),
  ordinal               INTEGER NOT NULL,              -- the likeliest first
  release_group_mbid    TEXT NOT NULL,
  title                 TEXT,
  artist                TEXT NOT NULL,
  primary_type          TEXT,
  first_date            TEXT,
  releases              INTEGER NOT NULL,              -- how many of the replies' releases are of it
  change                TEXT NOT NULL CHECK (change IN ('knows', 'gains', 'moves')),
  values_json           TEXT NOT NULL,                 -- [{"item":12,"field":"disc_number","ordinal":0,"value":"2"}]
  onto_json             TEXT NOT NULL,                 -- the files of the first disc a yes joins these onto: [14,15]
  rev                   INTEGER NOT NULL,
  UNIQUE (proposal_id, ordinal)
) STRICT;

-- [evidence|derived] What a proposal rests on: each disc ID whose layout
-- and current reply it was worked out from, its own disc's and those of the
-- other discs a join was worked out with.
CREATE TABLE proposal_evidence (
  id                    INTEGER PRIMARY KEY,
  proposal_id           INTEGER NOT NULL REFERENCES proposal(id),
  role                  TEXT NOT NULL CHECK (role IN ('rests_on')),
  disc_id               TEXT NOT NULL,
  lookup_response_id    INTEGER NOT NULL REFERENCES lookup_response(id),   -- the reply, as it was current
  rev                   INTEGER NOT NULL,
  UNIQUE (proposal_id, role, disc_id, lookup_response_id)
) STRICT;
