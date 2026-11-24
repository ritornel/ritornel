//! Who and what each play was of, when there are no files for it: the
//! rows the History lists are read from, `heard_artist` and `heard_album`,
//! and each play's links to them. A play bound to a song is linked to the
//! library's artists and record; any other to its names, and a name is
//! found to be the library's apart from the play, whenever the catalog
//! changes. See "History by artist and album" in docs/internals/history.md.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use ritornel_contract::{
    DEFAULT_HEARD, Heard, HeardAlbum, HeardAlbums, HeardArtist, HeardArtists, HeardListParams,
    HeardOrder, MAX_HEARD,
};
use ritornel_domain::normalize::{album_key, artist_key, artist_spelling};
use rusqlite::{Connection, OptionalExtension};

use crate::list::REACHED_RELEASE_R;
use crate::{Error, Id, LibraryStore, Today};

/// What a play's links are worked out from.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ToLink {
    /// The play, and whose it is.
    pub event: i64,
    pub listener: i64,
    /// Its recording and appearance, when it is bound.
    pub recording: Option<i64>,
    pub appearance: Option<i64>,
    /// Its primary observation, whose names it has.
    pub primary: i64,
    /// Whether it counts and is not excluded.
    pub counted: bool,
    /// Its local date, YYYYMMDD, and its instant in UTC.
    pub local_date: Option<i64>,
    pub at_ms: Option<i64>,
}

/// The library's artists a name can be, as canonical, by their name's key:
/// those the Artists list files, those a bound play's song credits, and
/// those credited on a record a play was heard from. So every artist the
/// lists give as the library's is found again by their key, files or not.
pub(crate) fn library_artists(connection: &Connection) -> Result<HashMap<String, Vec<i64>>, Error> {
    let mut statement = connection.prepare_cached(&format!(
        "SELECT a.id, a.display_name FROM artist a
         WHERE a.redirect_to_id IS NULL AND a.display_name IS NOT NULL
           AND (a.id IN (
                  SELECT ca.canonical_id FROM release r
                  JOIN artist_credit_name acn ON acn.artist_credit_id = r.display_artist_credit_id
                  JOIN artist ca ON ca.id = acn.artist_id
                  WHERE r.redirect_to_id IS NULL AND {REACHED_RELEASE_R})
                OR a.id IN (
                  SELECT x.canonical_id FROM heard_artist h JOIN artist x ON x.id = h.artist_id)
                OR a.id IN (
                  SELECT ca.canonical_id FROM heard_album h
                  JOIN release r ON r.release_group_id = h.release_group_id
                  JOIN artist_credit_name acn ON acn.artist_credit_id = r.display_artist_credit_id
                  JOIN artist ca ON ca.id = acn.artist_id
                  WHERE r.redirect_to_id IS NULL))"
    ))?;
    let mut by_key: HashMap<String, Vec<i64>> = HashMap::new();
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let name: String = row.get(1)?;
        by_key
            .entry(artist_key(&name))
            .or_default()
            .push(row.get(0)?);
    }
    Ok(by_key)
}

/// The one of these, when there is exactly one.
pub(crate) fn only(found: Option<&Vec<i64>>) -> Option<i64> {
    match found.map(Vec::as_slice) {
        Some([one]) => Some(*one),
        _ => None,
    }
}

/// An artist's records with files or that a play was heard from, as each
/// version's title's key and the record.
pub(crate) fn records_of(
    connection: &Connection,
    artist: i64,
) -> Result<Vec<(String, i64)>, Error> {
    let mut statement = connection.prepare_cached(&format!(
        "SELECT DISTINCT r.release_group_id, r.display_title FROM release r
         JOIN artist_credit_name acn ON acn.artist_credit_id = r.display_artist_credit_id
         JOIN artist ca ON ca.id = acn.artist_id
         WHERE ca.canonical_id = ?1 AND r.redirect_to_id IS NULL
           AND r.display_title IS NOT NULL
           AND ({REACHED_RELEASE_R} OR r.release_group_id IN (
                  SELECT release_group_id FROM heard_album WHERE release_group_id IS NOT NULL))"
    ))?;
    let records = statement
        .query_map([artist], |row| {
            Ok((album_key(&row.get::<_, String>(1)?), row.get(0)?))
        })?
        .collect::<Result<_, _>>()?;
    Ok(records)
}

/// The record whose title has this key, when exactly one has it.
pub(crate) fn record_named(records: Option<&Vec<(String, i64)>>, key: &str) -> Option<i64> {
    let matched: BTreeSet<i64> = records
        .into_iter()
        .flatten()
        .filter(|(title, _)| title == key)
        .map(|(_, group)| *group)
        .collect();
    match matched.len() {
        1 => matched.first().copied(),
        _ => None,
    }
}

/// What the catalog says names are: the library's artists by key, and each
/// artist's records, read once and kept for every name found.
struct Catalog<'c> {
    connection: &'c Connection,
    library: Option<HashMap<String, Vec<i64>>>,
    records: HashMap<i64, Vec<(String, i64)>>,
}

impl<'c> Catalog<'c> {
    fn new(connection: &'c Connection) -> Self {
        Self {
            connection,
            library: None,
            records: HashMap::new(),
        }
    }

    /// The library's artist an artist's name with this key is.
    fn artist(&mut self, key: &str) -> Result<Option<i64>, Error> {
        if self.library.is_none() {
            self.library = Some(library_artists(self.connection)?);
        }
        Ok(self
            .library
            .as_ref()
            .and_then(|library| only(library.get(key))))
    }

    /// The record of the artist's an album's name with this key is.
    fn record(&mut self, artist: i64, key: &str) -> Result<Option<i64>, Error> {
        if !self.records.contains_key(&artist) {
            self.records
                .insert(artist, records_of(self.connection, artist)?);
        }
        Ok(record_named(self.records.get(&artist), key))
    }
}

/// An artist's name row: its row, its key, and the library's artist it is.
type NameRow = (i64, String, Option<i64>);

/// An album's name row: its row, its artist's name row, its key, and the
/// library's record it is.
type AlbumNameRow = (i64, i64, String, Option<i64>);

/// Finds again what these names are, or every name, by what the catalog
/// holds now, and writes what moved. A name the library's artist has, and
/// an album's name that one of their records has, is theirs. Of names
/// touched, only those and the albums under them are read.
fn find_names(
    connection: &Connection,
    rev: i64,
    catalog: &mut Catalog<'_>,
    only_these: Option<(&BTreeSet<i64>, &BTreeSet<i64>)>,
) -> Result<(), Error> {
    const ARTIST: &str = "SELECT id, name_key, library_artist_id FROM heard_artist
                          WHERE name_key IS NOT NULL";
    const ALBUM: &str = "SELECT id, heard_artist_id, name_key, library_group_id FROM heard_album
                         WHERE name_key IS NOT NULL";
    let artist_row = |row: &rusqlite::Row<'_>| Ok((row.get(0)?, row.get(1)?, row.get(2)?));
    let album_row =
        |row: &rusqlite::Row<'_>| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?));
    // Every name, or those touched and the albums under them, each looked
    // up by its row.
    let (artists, albums): (Vec<NameRow>, Vec<AlbumNameRow>) = match only_these {
        None => (
            connection
                .prepare_cached(ARTIST)?
                .query_map([], artist_row)?
                .collect::<Result<_, _>>()?,
            connection
                .prepare_cached(ALBUM)?
                .query_map([], album_row)?
                .collect::<Result<_, _>>()?,
        ),
        Some((artists, albums)) => {
            let mut artist = connection.prepare_cached(&format!("{ARTIST} AND id = ?1"))?;
            let mut album = connection.prepare_cached(&format!("{ALBUM} AND id = ?1"))?;
            let mut under =
                connection.prepare_cached(&format!("{ALBUM} AND heard_artist_id = ?1"))?;
            let mut found_albums = BTreeMap::new();
            let mut found_artists = Vec::new();
            for id in artists {
                found_artists.extend(artist.query_row([id], artist_row).optional()?);
                for row in under.query_map([id], album_row)? {
                    let row: (i64, i64, String, Option<i64>) = row?;
                    found_albums.insert(row.0, row);
                }
            }
            for id in albums {
                if let Some(row) = album.query_row([id], album_row).optional()? {
                    found_albums.insert(row.0, row);
                }
            }
            (found_artists, found_albums.into_values().collect())
        }
    };
    for (row, key, was) in artists {
        let is = catalog.artist(&key)?;
        if is != was {
            connection
                .prepare_cached(
                    "UPDATE heard_artist SET library_artist_id = ?2, rev = ?3 WHERE id = ?1",
                )?
                .execute((row, is, rev))?;
        }
    }
    for (row, parent, key, was) in albums {
        // The artist's name it is under, as the library's artist it is now.
        let artist: Option<i64> = connection
            .prepare_cached("SELECT library_artist_id FROM heard_artist WHERE id = ?1")?
            .query_row([parent], |row| row.get(0))
            .optional()?
            .flatten();
        let is = match artist {
            Some(artist) => catalog.record(artist, &key)?,
            None => None,
        };
        if is != was {
            connection
                .prepare_cached(
                    "UPDATE heard_album SET library_group_id = ?2, rev = ?3 WHERE id = ?1",
                )?
                .execute((row, is, rev))?;
        }
    }
    Ok(())
}

/// Links plays to what they were heard as, and tidies the rows it touched
/// when it is done.
pub(crate) struct Linker<'c> {
    connection: &'c Connection,
    rev: i64,
    catalog: Catalog<'c>,
    /// Each credit's artists, as canonical, in order.
    credits: HashMap<i64, Vec<i64>>,
    /// Each appearance's credit and record.
    appearances: HashMap<i64, (Option<i64>, Option<i64>)>,
    /// Each recording's credit.
    recordings: HashMap<i64, Option<i64>>,
    /// The rows made or found, by the library's artist or record, or by
    /// key.
    artist_rows: HashMap<i64, i64>,
    name_rows: HashMap<String, i64>,
    record_rows: HashMap<i64, i64>,
    album_rows: HashMap<(i64, String), i64>,
    artists_touched: BTreeSet<i64>,
    albums_touched: BTreeSet<i64>,
    /// Whether a row of the library's artist or record was made or went,
    /// which can change what any name is.
    library_moved: bool,
}

impl<'c> Linker<'c> {
    pub(crate) fn new(connection: &'c Connection, rev: i64) -> Self {
        Self {
            connection,
            rev,
            catalog: Catalog::new(connection),
            credits: HashMap::new(),
            appearances: HashMap::new(),
            recordings: HashMap::new(),
            artist_rows: HashMap::new(),
            name_rows: HashMap::new(),
            record_rows: HashMap::new(),
            album_rows: HashMap::new(),
            artists_touched: BTreeSet::new(),
            albums_touched: BTreeSet::new(),
            library_moved: false,
        }
    }

    /// The artists a credit names, as canonical, in order.
    fn credited(&mut self, credit: i64) -> Result<Vec<i64>, Error> {
        if let Some(artists) = self.credits.get(&credit) {
            return Ok(artists.clone());
        }
        let artists: Vec<i64> = self
            .connection
            .prepare_cached(
                "SELECT ca.canonical_id FROM artist_credit_name acn
                 JOIN artist ca ON ca.id = acn.artist_id
                 WHERE acn.artist_credit_id = ?1
                 GROUP BY ca.canonical_id ORDER BY min(acn.position)",
            )?
            .query_map([credit], |row| row.get(0))?
            .collect::<Result<_, _>>()?;
        self.credits.insert(credit, artists.clone());
        Ok(artists)
    }

    /// The artists a bound play's song is credited to: as the appearance
    /// it was heard from credits it, or else as the recording is.
    fn artists_of(&mut self, recording: i64, appearance: Option<i64>) -> Result<Vec<i64>, Error> {
        let credit = match appearance.map(|appearance| self.appearance(appearance)) {
            Some(found) => found?.0,
            None => None,
        };
        let credit = match credit {
            Some(credit) => Some(credit),
            None => self.recording_credit(recording)?,
        };
        match credit {
            Some(credit) => self.credited(credit),
            None => Ok(Vec::new()),
        }
    }

    /// An appearance's credit and the record it is on.
    fn appearance(&mut self, appearance: i64) -> Result<(Option<i64>, Option<i64>), Error> {
        if let Some(found) = self.appearances.get(&appearance) {
            return Ok(*found);
        }
        let found = self
            .connection
            .prepare_cached(
                "SELECT ta.display_artist_credit_id, rc.release_group_id FROM track_appearance ta
                 LEFT JOIN release r ON r.id = ta.release_id
                 LEFT JOIN release rc ON rc.id = r.canonical_id
                 WHERE ta.id = ?1",
            )?
            .query_row([appearance], |row| Ok((row.get(0)?, row.get(1)?)))
            .optional()?
            .unwrap_or((None, None));
        self.appearances.insert(appearance, found);
        Ok(found)
    }

    /// A recording's credit.
    fn recording_credit(&mut self, recording: i64) -> Result<Option<i64>, Error> {
        if let Some(credit) = self.recordings.get(&recording) {
            return Ok(*credit);
        }
        let credit: Option<i64> = self
            .connection
            .prepare_cached("SELECT display_artist_credit_id FROM recording WHERE id = ?1")?
            .query_row([recording], |row| row.get(0))
            .optional()?
            .flatten();
        self.recordings.insert(recording, credit);
        Ok(credit)
    }

    /// The row `select` finds `by` these, or else the one `insert` makes of
    /// `values`, and whether it was made.
    fn row(
        &self,
        select: &str,
        by: impl rusqlite::Params,
        insert: &str,
        values: impl rusqlite::Params,
    ) -> Result<(i64, bool), Error> {
        let found: Option<i64> = self
            .connection
            .prepare_cached(select)?
            .query_row(by, |row| row.get(0))
            .optional()?;
        match found {
            Some(id) => Ok((id, false)),
            None => {
                self.connection.prepare_cached(insert)?.execute(values)?;
                Ok((self.connection.last_insert_rowid(), true))
            }
        }
    }

    fn artist_row(&mut self, artist: i64) -> Result<i64, Error> {
        let id = match self.artist_rows.get(&artist) {
            Some(id) => *id,
            None => {
                let (id, made) = self.row(
                    "SELECT id FROM heard_artist WHERE artist_id = ?1",
                    [artist],
                    "INSERT INTO heard_artist (artist_id, rev) VALUES (?1, ?2)",
                    (artist, self.rev),
                )?;
                self.library_moved |= made;
                self.artist_rows.insert(artist, id);
                id
            }
        };
        self.artists_touched.insert(id);
        Ok(id)
    }

    fn name_row(&mut self, key: &str, spelling: &str) -> Result<i64, Error> {
        let id = match self.name_rows.get(key) {
            Some(id) => *id,
            None => {
                let (id, _) = self.row(
                    "SELECT id FROM heard_artist WHERE name_key = ?1",
                    [key],
                    "INSERT INTO heard_artist (name_key, name, rev) VALUES (?1, ?2, ?3)",
                    (key, spelling, self.rev),
                )?;
                self.name_rows.insert(key.to_owned(), id);
                id
            }
        };
        self.artists_touched.insert(id);
        Ok(id)
    }

    fn record_row(&mut self, group: i64) -> Result<i64, Error> {
        let id = match self.record_rows.get(&group) {
            Some(id) => *id,
            None => {
                let (id, made) = self.row(
                    "SELECT id FROM heard_album WHERE release_group_id = ?1",
                    [group],
                    "INSERT INTO heard_album (release_group_id, rev) VALUES (?1, ?2)",
                    (group, self.rev),
                )?;
                self.library_moved |= made;
                self.record_rows.insert(group, id);
                id
            }
        };
        self.albums_touched.insert(id);
        Ok(id)
    }

    fn album_row(&mut self, artist: i64, key: &str, spelling: &str) -> Result<i64, Error> {
        let id = match self.album_rows.get(&(artist, key.to_owned())) {
            Some(id) => *id,
            None => {
                let (id, _) = self.row(
                    "SELECT id FROM heard_album WHERE heard_artist_id = ?1 AND name_key = ?2",
                    (artist, key),
                    "INSERT INTO heard_album (heard_artist_id, name_key, name, rev)
                     VALUES (?1, ?2, ?3, ?4)",
                    (artist, key, spelling, self.rev),
                )?;
                self.album_rows.insert((artist, key.to_owned()), id);
                id
            }
        };
        self.albums_touched.insert(id);
        Ok(id)
    }

    /// Links one play to the artists and the album it was heard as.
    pub(crate) fn link(&mut self, play: &ToLink) -> Result<(), Error> {
        let mut artists: Vec<(i64, Option<String>)> = Vec::new();
        let mut album: Option<(i64, Option<String>)> = None;
        if let Some(recording) = play.recording {
            for artist in self.artists_of(recording, play.appearance)? {
                artists.push((self.artist_row(artist)?, None));
            }
            if let Some(appearance) = play.appearance
                && let Some(group) = self.appearance(appearance)?.1
            {
                album = Some((self.record_row(group)?, None));
            }
        }

        // What it was not bound to, it has by its names: its artist's when
        // its song credits no one, and its album's, under its artist's
        // name, when it was heard from no record.
        if artists.is_empty() || album.is_none() {
            let names: Option<(String, Option<String>)> = self
                .connection
                .prepare_cached(
                    "SELECT artist, album FROM observation_import WHERE observation_id = ?1",
                )?
                .query_row([play.primary], |row| Ok((row.get(0)?, row.get(1)?)))
                .optional()?;
            if let Some((artist_name, album_name)) = names {
                let key = artist_key(&artist_name);
                let artist_name = artist_spelling(&artist_name);
                let album_name = album_name
                    .filter(|_| album.is_none())
                    .map(|name| (album_key(&name), name))
                    .filter(|(key, _)| !key.is_empty());
                if !key.is_empty() && (artists.is_empty() || album_name.is_some()) {
                    let named = self.name_row(&key, &artist_name)?;
                    if let Some((album_key, album_name)) = album_name {
                        album = Some((
                            self.album_row(named, &album_key, &album_name)?,
                            Some(album_name),
                        ));
                    }
                    if artists.is_empty() {
                        artists.push((named, Some(artist_name)));
                    }
                }
            }
        }

        for (row, spelling) in artists {
            self.connection
                .prepare_cached(
                    "INSERT OR IGNORE INTO play_event_artist
                       (heard_artist_id, play_event_id, listener_id, counted, local_date,
                        event_at_utc_ms, spelling, rev)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                )?
                .execute((
                    row,
                    play.event,
                    play.listener,
                    play.counted,
                    play.local_date,
                    play.at_ms,
                    spelling,
                    self.rev,
                ))?;
        }
        if let Some((row, spelling)) = album {
            self.connection
                .prepare_cached(
                    "INSERT INTO play_event_album
                       (heard_album_id, play_event_id, listener_id, counted, local_date,
                        event_at_utc_ms, spelling, rev)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                )?
                .execute((
                    row,
                    play.event,
                    play.listener,
                    play.counted,
                    play.local_date,
                    play.at_ms,
                    spelling,
                    self.rev,
                ))?;
        }
        Ok(())
    }

    /// Drops the links of these plays, before the plays are dropped.
    pub(crate) fn unlink(&mut self, plays: &BTreeSet<i64>) -> Result<(), Error> {
        for play in plays {
            let artists: Vec<i64> = self
                .connection
                .prepare_cached(
                    "DELETE FROM play_event_artist WHERE play_event_id = ?1
                     RETURNING heard_artist_id",
                )?
                .query_map([play], |row| row.get(0))?
                .collect::<Result<_, _>>()?;
            self.artists_touched.extend(artists);
            let albums: Vec<i64> = self
                .connection
                .prepare_cached(
                    "DELETE FROM play_event_album WHERE play_event_id = ?1 RETURNING heard_album_id",
                )?
                .query_map([play], |row| row.get(0))?
                .collect::<Result<_, _>>()?;
            self.albums_touched.extend(albums);
        }
        Ok(())
    }

    /// Tidies the rows it touched: a row nothing links to any more goes; a
    /// name is spelled as its counted plays spell it most, or its others
    /// when none counts; and a name made is found to be the library's or
    /// not, as is every name when a row of the library's came or went.
    pub(crate) fn finish(mut self) -> Result<(), Error> {
        let mut parents = Vec::new();
        for album in &self.albums_touched {
            // An album's name that goes leaves its artist's name to be
            // tidied too, which may have held only it.
            let gone: Option<Option<i64>> = self
                .connection
                .prepare_cached(
                    "DELETE FROM heard_album WHERE id = ?1
                       AND NOT EXISTS (SELECT 1 FROM play_event_album WHERE heard_album_id = ?1)
                     RETURNING heard_artist_id",
                )?
                .query_row([album], |row| row.get(0))
                .optional()?;
            if let Some(parent) = gone {
                // A record's row has no artist's name above it.
                self.library_moved |= parent.is_none();
                parents.extend(parent);
            } else if self.is_name("heard_album", *album)? {
                self.connection
                    .prepare_cached(
                        "UPDATE heard_album SET name = spelled.spelling, rev = ?2
                         FROM (SELECT spelling FROM play_event_album
                               WHERE heard_album_id = ?1 AND spelling IS NOT NULL
                               GROUP BY spelling
                               ORDER BY sum(counted) DESC, count(*) DESC, spelling
                               LIMIT 1) spelled
                         WHERE id = ?1 AND name IS NOT spelled.spelling",
                    )?
                    .execute((album, self.rev))?;
            }
        }
        self.artists_touched.extend(parents);
        for artist in &self.artists_touched {
            let gone: Option<bool> = self
                .connection
                .prepare_cached(
                    "DELETE FROM heard_artist WHERE id = ?1
                       AND NOT EXISTS (SELECT 1 FROM play_event_artist WHERE heard_artist_id = ?1)
                       AND NOT EXISTS (SELECT 1 FROM heard_album WHERE heard_artist_id = ?1)
                     RETURNING artist_id IS NOT NULL",
                )?
                .query_row([artist], |row| row.get(0))
                .optional()?;
            if let Some(library) = gone {
                self.library_moved |= library;
            } else if self.is_name("heard_artist", *artist)? {
                self.connection
                    .prepare_cached(
                        "UPDATE heard_artist SET name = spelled.spelling, rev = ?2
                         FROM (SELECT spelling FROM play_event_artist
                               WHERE heard_artist_id = ?1 AND spelling IS NOT NULL
                               GROUP BY spelling
                               ORDER BY sum(counted) DESC, count(*) DESC, spelling
                               LIMIT 1) spelled
                         WHERE id = ?1 AND name IS NOT spelled.spelling",
                    )?
                    .execute((artist, self.rev))?;
            }
        }
        let these = (&self.artists_touched, &self.albums_touched);
        let only_these = (!self.library_moved).then_some(these);
        find_names(self.connection, self.rev, &mut self.catalog, only_these)
    }

    /// Whether a row of `table` is a name's, which is spelled, rather than
    /// the library's, which is not.
    fn is_name(&self, table: &str, id: i64) -> Result<bool, Error> {
        Ok(self
            .connection
            .prepare_cached(&format!(
                "SELECT name_key IS NOT NULL FROM {table} WHERE id = ?1"
            ))?
            .query_row([id], |row| row.get(0))
            .optional()?
            .unwrap_or(false))
    }
}

/// Drops every link and row, before every play is derived again.
pub(crate) fn clear(connection: &Connection) -> Result<(), Error> {
    connection.execute_batch(
        "DELETE FROM play_event_album; DELETE FROM play_event_artist;
         DELETE FROM heard_album; DELETE FROM heard_artist;",
    )?;
    Ok(())
}

/// What a change to the catalog moves of what plays were heard as, in its
/// write: a bound play whose song's credit or record is no longer what it
/// is linked to is linked again, and every name is found again. A play
/// whose binding moved is derived again before this, and linked with it.
pub(crate) fn catalog_changed(connection: &Connection, rev: i64) -> Result<(), Error> {
    let mut linker = Linker::new(connection, rev);
    // Each bound play, with the library's artists and record it is linked
    // to.
    let mut statement = connection.prepare(
        "SELECT pe.id, pe.listener_id, pe.recording_id, pe.appearance_id, peo.observation_id,
                pe.counts = 1 AND pe.excluded = 0, pe.local_date, pe.event_at_utc_ms,
                (SELECT group_concat(h.artist_id) FROM play_event_artist pa
                 JOIN heard_artist h ON h.id = pa.heard_artist_id
                 WHERE pa.play_event_id = pe.id AND h.artist_id IS NOT NULL),
                (SELECT h.release_group_id FROM play_event_album pa
                 JOIN heard_album h ON h.id = pa.heard_album_id
                 WHERE pa.play_event_id = pe.id)
         FROM play_event pe
         JOIN play_event_observation peo ON peo.play_event_id = pe.id AND peo.role = 'primary'
         WHERE pe.recording_id IS NOT NULL",
    )?;
    let bound: Vec<(ToLink, Option<String>, Option<i64>)> = statement
        .query_map([], |row| {
            Ok((
                ToLink {
                    event: row.get(0)?,
                    listener: row.get(1)?,
                    recording: row.get(2)?,
                    appearance: row.get(3)?,
                    primary: row.get(4)?,
                    counted: row.get(5)?,
                    local_date: row.get(6)?,
                    at_ms: row.get(7)?,
                },
                row.get(8)?,
                row.get(9)?,
            ))
        })?
        .collect::<Result<_, _>>()?;
    let mut stale = BTreeSet::new();
    let mut moved = Vec::new();
    for (play, artists, record) in bound {
        let Some(recording) = play.recording else {
            continue;
        };
        let linked: BTreeSet<i64> = artists
            .iter()
            .flat_map(|artists| artists.split(','))
            .filter_map(|artist| artist.parse().ok())
            .collect();
        let credited: BTreeSet<i64> = linker
            .artists_of(recording, play.appearance)?
            .into_iter()
            .collect();
        let heard_from = match play.appearance {
            Some(appearance) => linker.appearance(appearance)?.1,
            None => None,
        };
        if linked != credited || record != heard_from {
            stale.insert(play.event);
            moved.push(play);
        }
    }
    linker.unlink(&stale)?;
    for play in &moved {
        linker.link(play)?;
    }
    // The catalog changed, so every name is found again.
    linker.library_moved = true;
    linker.finish()
}

/// Finds every name again by what the catalog holds now: after a file is
/// retired, which can leave an artist or a record of the library without
/// one.
pub(crate) fn find_names_again(connection: &Connection, rev: i64) -> Result<(), Error> {
    find_names(connection, rev, &mut Catalog::new(connection), None)
}

/// How often a row was heard, and its first and last local dates.
type Tally = (u64, Option<i64>, Option<i64>);

/// How often and when each row was heard by the listener: their plays
/// that count, and their first and last local dates. A play stamped after
/// now, by a wrong clock, or with no instant counts and is never first or
/// last, as on a page. Read from the index on the links alone.
fn tallies(
    connection: &Connection,
    listener: i64,
    table: &str,
    column: &str,
    today: Today,
) -> Result<HashMap<i64, Tally>, Error> {
    let mut statement = connection.prepare(&format!(
        "SELECT {column}, count(*),
                min(CASE WHEN event_at_utc_ms <= ?2 THEN local_date END),
                max(CASE WHEN event_at_utc_ms <= ?2 THEN local_date END)
         FROM {table}
         WHERE listener_id = ?1 AND counted = 1 GROUP BY {column}"
    ))?;
    let rows = statement.query_map((listener, today.now_ms), |row| {
        Ok((
            row.get::<_, i64>(0)?,
            (
                u64::try_from(row.get::<_, i64>(1)?).unwrap_or(0),
                row.get(2)?,
                row.get(3)?,
            ),
        ))
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Adds one tally to another, as rows that are one artist or one record.
fn add(into: &mut Tally, (plays, first, last): Tally) {
    into.0 += plays;
    into.1 = [into.1, first].into_iter().flatten().min();
    into.2 = [into.2, last].into_iter().flatten().max();
}

/// What a History list holds of one entry as it is ordered: its tally, the
/// key it is ordered by after that, and a number that tells two apart.
struct Listed<T> {
    item: T,
    key: String,
    tie: i64,
    tally: Tally,
}

/// Orders the entries as asked, keeps the page asked for, and says how
/// many entries there are with a play that counts.
fn page<T>(mut rows: Vec<Listed<T>>, params: &HeardListParams) -> (Vec<Listed<T>>, u64) {
    rows.retain(|row| row.tally.0 > 0);
    let total = rows.len() as u64;
    rows.sort_by(|a, b| {
        let ((plays_a, first_a, last_a), (plays_b, first_b, last_b)) = (a.tally, b.tally);
        let order = match params.order.unwrap_or_default() {
            HeardOrder::Plays => plays_b.cmp(&plays_a),
            // An entry with no date goes last either way.
            HeardOrder::First => first_a
                .is_none()
                .cmp(&first_b.is_none())
                .then(first_a.cmp(&first_b)),
            HeardOrder::Last => last_a
                .is_none()
                .cmp(&last_b.is_none())
                .then(last_b.cmp(&last_a)),
        };
        order
            .then_with(|| a.key.cmp(&b.key))
            .then(a.tie.cmp(&b.tie))
    });
    let limit = params.limit.unwrap_or(DEFAULT_HEARD).min(MAX_HEARD) as usize;
    let rows = rows
        .into_iter()
        .skip(params.offset.unwrap_or(0) as usize)
        .take(limit)
        .collect();
    (rows, total)
}

fn heard((plays, first, last): Tally) -> Heard {
    Heard {
        plays,
        first_played: first.map(crate::history_pages::date),
        last_played: last.map(crate::history_pages::date),
    }
}

/// The library revision.
fn revision(connection: &Connection) -> Result<u64, Error> {
    let rev: i64 =
        connection.query_row("SELECT rev FROM library_meta WHERE id = 1", [], |row| {
            row.get(0)
        })?;
    Ok(u64::try_from(rev).unwrap_or(0))
}

/// Every artist heard, as asked. The library's artist gathers the rows of
/// their credit and of the name that is theirs, as canonical.
fn artists(
    connection: &Connection,
    listener: i64,
    params: &HeardListParams,
    today: Today,
) -> Result<HeardArtists, Error> {
    let tallies = tallies(
        connection,
        listener,
        "play_event_artist",
        "heard_artist_id",
        today,
    )?;
    let mut statement = connection.prepare(
        "SELECT h.id, ca.id, ca.public_id, ca.display_name, h.name_key, h.name
         FROM heard_artist h
         LEFT JOIN artist a ON a.id = coalesce(h.artist_id, h.library_artist_id)
         LEFT JOIN artist ca ON ca.id = a.canonical_id",
    )?;
    let mut rows = statement.query([])?;
    let mut library: BTreeMap<i64, Listed<HeardArtist>> = BTreeMap::new();
    let mut named = Vec::new();
    while let Some(row) = rows.next()? {
        let (id, canonical): (i64, Option<i64>) = (row.get(0)?, row.get(1)?);
        let Some(&tally) = tallies.get(&id) else {
            continue;
        };
        match canonical {
            Some(canonical) => {
                let entry = match library.get_mut(&canonical) {
                    Some(entry) => entry,
                    None => {
                        let name: String = row.get::<_, Option<String>>(3)?.unwrap_or_default();
                        let key = artist_key(&name);
                        library.entry(canonical).or_insert(Listed {
                            item: HeardArtist {
                                key: key.clone(),
                                name,
                                artist_id: Some(Id::from_bytes(row.get(2)?)),
                                heard: Heard::default(),
                            },
                            key,
                            tie: canonical,
                            tally: (0, None, None),
                        })
                    }
                };
                add(&mut entry.tally, tally);
            }
            None => {
                let (key, name): (String, String) = (row.get(4)?, row.get(5)?);
                named.push(Listed {
                    item: HeardArtist {
                        key: key.clone(),
                        name,
                        artist_id: None,
                        heard: Heard::default(),
                    },
                    key,
                    tie: -id,
                    tally,
                });
            }
        }
    }
    let (listed, total) = page(library.into_values().chain(named).collect(), params);
    Ok(HeardArtists {
        artists: listed
            .into_iter()
            .map(|entry| HeardArtist {
                heard: heard(entry.tally),
                ..entry.item
            })
            .collect(),
        total,
        library_revision: revision(connection)?,
    })
}

/// Every album heard, as asked. The library's record gathers the rows of
/// plays heard from it and of the names that are it.
fn albums(
    connection: &Connection,
    listener: i64,
    params: &HeardListParams,
    today: Today,
) -> Result<HeardAlbums, Error> {
    let tallies = tallies(
        connection,
        listener,
        "play_event_album",
        "heard_album_id",
        today,
    )?;
    let mut statement = connection.prepare(
        "SELECT h.id, coalesce(h.release_group_id, h.library_group_id), h.name_key, h.name,
                ha.name_key, ha.name, ca.display_name
         FROM heard_album h
         LEFT JOIN heard_artist ha ON ha.id = h.heard_artist_id
         LEFT JOIN artist a ON a.id = ha.library_artist_id
         LEFT JOIN artist ca ON ca.id = a.canonical_id",
    )?;
    let mut rows = statement.query([])?;
    let mut records: BTreeMap<i64, Tally> = BTreeMap::new();
    let mut listed = Vec::new();
    while let Some(row) = rows.next()? {
        let id: i64 = row.get(0)?;
        let Some(&tally) = tallies.get(&id) else {
            continue;
        };
        if let Some(group) = row.get::<_, Option<i64>>(1)? {
            add(records.entry(group).or_default(), tally);
            continue;
        }
        // A name under its artist's: the library's artist's name when it is
        // theirs.
        let (artist_key, spelled): (String, String) = (row.get(4)?, row.get(5)?);
        let artist_name = row.get::<_, Option<String>>(6)?.unwrap_or(spelled);
        let (key, name): (String, String) = (row.get(2)?, row.get(3)?);
        listed.push(Listed {
            key: format!("{key}\u{0}{artist_key}"),
            item: HeardAlbum {
                artist_key,
                key,
                artist_name,
                name,
                album_id: None,
                heard: Heard::default(),
            },
            tie: -id,
            tally,
        });
    }
    // The library's records, named as the Albums wall names them, under
    // the first artist of their credit.
    let mut record = connection.prepare_cached(
        "SELECT rg.public_id, shown.display_title, credit.display_text, first.display_name
         FROM release_group rg
         LEFT JOIN release shown ON shown.id = coalesce(rg.display_release_id,
           (SELECT r.id FROM release r WHERE r.release_group_id = rg.id
              AND r.redirect_to_id IS NULL ORDER BY r.id LIMIT 1))
         LEFT JOIN artist_credit credit ON credit.id = shown.display_artist_credit_id
         LEFT JOIN artist first ON first.id = (
           SELECT a.canonical_id FROM artist_credit_name acn JOIN artist a ON a.id = acn.artist_id
           WHERE acn.artist_credit_id = shown.display_artist_credit_id
           ORDER BY acn.position LIMIT 1)
         WHERE rg.id = ?1",
    )?;
    for (group, tally) in records {
        let Some((id, name, artist_name, first)) = record
            .query_row([group], |row| {
                Ok((
                    Id::from_bytes(row.get(0)?),
                    row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    row.get::<_, Option<String>>(3)?,
                ))
            })
            .optional()?
        else {
            continue;
        };
        let artist_key = artist_key(first.as_deref().unwrap_or(&artist_name));
        let key = album_key(&name);
        listed.push(Listed {
            key: format!("{key}\u{0}{artist_key}"),
            item: HeardAlbum {
                artist_key,
                key,
                artist_name,
                name,
                album_id: Some(id),
                heard: Heard::default(),
            },
            tie: group,
            tally,
        });
    }
    let (listed, total) = page(listed, params);
    Ok(HeardAlbums {
        albums: listed
            .into_iter()
            .map(|entry| HeardAlbum {
                heard: heard(entry.tally),
                ..entry.item
            })
            .collect(),
        total,
        library_revision: revision(connection)?,
    })
}

impl LibraryStore {
    /// `history.artists`: every artist `listener` heard, with files or not,
    /// read `today`.
    pub fn heard_artists(
        &self,
        listener: Id,
        params: HeardListParams,
        today: Today,
    ) -> Result<HeardArtists, Error> {
        self.read_snapshot(move |connection| {
            let listener = crate::history_pages::listener_key(connection, listener)?;
            artists(connection, listener, &params, today)
        })
    }

    /// `history.albums`: every album `listener` heard, with files or not,
    /// read `today`.
    pub fn heard_albums(
        &self,
        listener: Id,
        params: HeardListParams,
        today: Today,
    ) -> Result<HeardAlbums, Error> {
        self.read_snapshot(move |connection| {
            let listener = crate::history_pages::listener_key(connection, listener)?;
            albums(connection, listener, &params, today)
        })
    }
}

#[cfg(test)]
mod tests;
