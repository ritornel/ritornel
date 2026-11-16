-- =============================================================================
-- Ritornel library.db, migration 0002: the indexes the library's lists and
-- covers read by. Each is here because a query the walls run was measured
-- reading a whole table without it, on an invented library of 100,000
-- records (1.36 million tracks); the comment above each names the query.
-- docs/design/schema.md left these to be added from measurements.
--
-- This file is never edited once it has shipped.
-- =============================================================================

-- A window of Albums or Artists: the tracks of the records it shows, for how
-- many there are, how long they are and their copies. Songs in album order.
-- (list/rows.rs, `albums` and `track_counts`.)
CREATE INDEX track_appearance_release ON track_appearance (release_id);

-- A window of Albums: every version of the groups it shows, for the version a
-- group shows and how many it has. Songs, searched or filtered: the versions
-- of the groups its songs are on. (list/rows.rs `albums`; list/open.rs
-- `releases_of_groups`.)
CREATE INDEX release_release_group ON release (release_group_id);

-- A window of Artists: the records that name each artist, by their credit.
-- (list/rows.rs, `artists`.) The credit's names are read whole: by artist
-- they were no quicker.
CREATE INDEX release_display_artist_credit ON release (display_artist_credit_id);

-- A window of Albums or Songs: where each track's current copies are, and in
-- what format. (list/rows.rs, `for_each_formats`.)
CREATE INDEX assignment_current_appearance
  ON assignment (appearance_id)
  WHERE superseded_at_ms IS NULL;

-- A cover: the files a picture came with, to read it from one of them.
-- (covers/mod.rs, `picture_sources`.)
CREATE INDEX revision_picture_picture ON revision_picture (picture_id);
