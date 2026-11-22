-- Durable ownership of artist keys: each MusicBrainz artist ID or
-- mechanically normalised name belongs to one artist row, even while that
-- row redirects to another after a merge. A rescan or a display rebuild
-- finds the same row by its key, never creates a duplicate for a merged
-- artist, and never swaps an artist row to a different MBID after a retag.

CREATE TABLE artist_key (
  id                  INTEGER PRIMARY KEY,
  key_kind            TEXT NOT NULL CHECK (key_kind IN ('mbid', 'name')),
  key_value           TEXT NOT NULL CHECK (length(key_value) > 0),
  artist_id           INTEGER NOT NULL REFERENCES artist(id),
  normalizer_version  INTEGER,                                             -- NULL for 'mbid'; the normaliser version that made key_value for 'name'
  decision_id         INTEGER REFERENCES identity_decision(id),            -- set when an owner decision pinned this key
  rev                 INTEGER NOT NULL,
  UNIQUE (key_kind, key_value),
  CHECK ((key_kind = 'mbid') = (normalizer_version IS NULL))
) STRICT;

CREATE INDEX artist_key_artist ON artist_key (artist_id);

-- Backfill MBID keys from the claims of files currently placed on each
-- artist's credits. Count an MBID only where the file's own artist or
-- album_artist tag count matches its ID count and the credit's name count
-- (mirroring how credits are built from IDs), breaking ties by first-seen
-- order to agree with the projector.
WITH placed AS (
  SELECT a.source_item_id AS item, ta.display_artist_credit_id AS credit, 'mb_artist' AS namespace
  FROM assignment a
  JOIN track_appearance ta ON ta.id = a.appearance_id
  WHERE a.superseded_at_ms IS NULL AND ta.display_artist_credit_id IS NOT NULL
  UNION ALL
  SELECT a.source_item_id, rel.display_artist_credit_id, 'mb_album_artist'
  FROM assignment a
  JOIN track_appearance ta ON ta.id = a.appearance_id
  JOIN release rel ON rel.id = ta.release_id
  WHERE a.superseded_at_ms IS NULL AND rel.display_artist_credit_id IS NOT NULL
),
ic_counts AS (
  SELECT source_item_id, namespace, count(*) AS cnt
  FROM identifier_claim
  WHERE namespace IN ('mb_artist', 'mb_album_artist')
  GROUP BY source_item_id, namespace
),
mc_counts AS (
  SELECT source_item_id, field, count(*) AS cnt
  FROM metadata_claim
  WHERE field IN ('artist', 'artists', 'album_artist')
  GROUP BY source_item_id, field
),
acn_counts AS (
  SELECT artist_credit_id, count(*) AS cnt
  FROM artist_credit_name
  GROUP BY artist_credit_id
),
votes AS (
  SELECT n.artist_id, c.value AS mbid, count(*) AS cnt,
         min(si.first_seen_at_ms) AS first_seen,
         min(si.id) AS first_item
  FROM placed p
  JOIN source_item si ON si.id = p.item
  JOIN acn_counts ac ON ac.artist_credit_id = p.credit
  JOIN ic_counts ic ON ic.source_item_id = p.item
                   AND ic.namespace = p.namespace
                   AND ic.cnt = ac.cnt
  LEFT JOIN mc_counts mc_aa ON mc_aa.source_item_id = p.item AND mc_aa.field = 'album_artist'
  LEFT JOIN mc_counts mc_as ON mc_as.source_item_id = p.item AND mc_as.field = 'artists'
  LEFT JOIN mc_counts mc_a  ON mc_a.source_item_id = p.item  AND mc_a.field = 'artist'
  JOIN artist_credit_name n ON n.artist_credit_id = p.credit
  JOIN identifier_claim c ON c.source_item_id = p.item
                         AND c.namespace = p.namespace
                         AND c.ordinal = n.position
                         AND length(c.value) > 0
  WHERE (
      (p.namespace = 'mb_album_artist' AND coalesce(mc_aa.cnt, 0) = ac.cnt)
      OR
      (p.namespace = 'mb_artist' AND (coalesce(mc_as.cnt, 0) = ac.cnt OR coalesce(mc_a.cnt, 0) = ac.cnt))
    )
  GROUP BY n.artist_id, c.value
),
best_per_artist AS (
  SELECT artist_id, mbid, cnt,
         ROW_NUMBER() OVER (
           PARTITION BY artist_id
           ORDER BY cnt DESC, first_seen ASC, first_item ASC, mbid ASC
         ) AS rn
  FROM votes
),
dedup_mbid AS (
  SELECT b.artist_id, b.mbid,
         ROW_NUMBER() OVER (
           PARTITION BY b.mbid
           ORDER BY (a.redirect_to_id IS NULL) DESC, b.cnt DESC, b.artist_id ASC
         ) AS rn
  FROM best_per_artist b
  JOIN artist a ON a.id = b.artist_id
  WHERE b.rn = 1
)
INSERT INTO artist_key (key_kind, key_value, artist_id, normalizer_version, decision_id, rev)
SELECT 'mbid', mbid, artist_id, NULL, NULL,
       coalesce((SELECT rev FROM library_meta WHERE id = 1), 0)
FROM dedup_mbid
WHERE rn = 1;

-- Backfill normalised name keys for artists that have no MBID key, keeping
-- redirected artists' keys too so merges survive rescans and display rebuilds.
WITH norm AS (
  SELECT coalesce(
    CAST(json_extract(
      (SELECT value_json FROM library_setting WHERE key = 'identity.versions'),
      '$.normalizer'
    ) AS INTEGER),
    3
  ) AS ver
),
name_candidates AS (
  SELECT a.id AS artist_id, a.match_name,
         ROW_NUMBER() OVER (
           PARTITION BY a.match_name
           ORDER BY (a.redirect_to_id IS NULL) DESC, a.id ASC
         ) AS rn
  FROM artist a
  WHERE a.match_name IS NOT NULL
    AND length(a.match_name) > 0
    AND NOT EXISTS (
      SELECT 1 FROM artist_key ak
      WHERE ak.artist_id = a.id AND ak.key_kind = 'mbid'
    )
)
INSERT INTO artist_key (key_kind, key_value, artist_id, normalizer_version, decision_id, rev)
SELECT 'name', nc.match_name, nc.artist_id, norm.ver, NULL,
       coalesce((SELECT rev FROM library_meta WHERE id = 1), 0)
FROM name_candidates nc, norm
WHERE nc.rn = 1;
