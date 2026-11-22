-- The index and the words typed into it use the same spelling. Display
-- names are kept separately on the catalog rows.
UPDATE search_text
SET title = search_fold(title), artist = search_fold(artist), album = search_fold(album);
