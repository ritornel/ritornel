//! Who and what each play was of, when there are no files for it: the
//! rows the History lists are read from, `heard_artist` and `heard_album`,
//! and each play's links to them. They are derived, and written with the
//! plays and by nothing else. See "History by artist and album" in
//! docs/internals/history.md.

use std::collections::{BTreeSet, HashMap};

use ritornel_contract::{
    DEFAULT_HEARD, Heard, HeardAlbum, HeardAlbums, HeardArtist, HeardArtists, HeardListParams,
    HeardOrder, MAX_HEARD,
};
use ritornel_domain::normalize::{album_key, artist_key};
use rusqlite::{Connection, OptionalExtension};

use crate::list::REACHED_RELEASE_R;
use crate::{Error, Id, LibraryStore};

/// What a play's links are worked out from.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ToLink {
    /// The play.
    pub event: i64,
    /// Its recording and appearance, when it is bound.
    pub recording: Option<i64>,
    pub appearance: Option<i64>,
    /// Its primary observation, whose names it has when it is not bound.
    pub primary: i64,
    /// Whether it counts and is not excluded.
    pub counted: bool,
    /// Its local date, YYYYMMDD.
    pub local_date: Option<i64>,
}

/// The library's artists that have a record, as canonical, by their name's
/// key: those the Artists list files.
pub(crate) fn library_artists(connection: &Connection) -> Result<HashMap<String, Vec<i64>>, Error> {
    let mut statement = connection.prepare_cached(&format!(
        "SELECT a.id, a.display_name FROM artist a
         WHERE a.redirect_to_id IS NULL AND a.display_name IS NOT NULL
           AND a.id IN (
             SELECT ca.canonical_id FROM release r
             JOIN artist_credit_name acn ON acn.artist_credit_id = r.display_artist_credit_id
             JOIN artist ca ON ca.id = acn.artist_id
             WHERE r.redirect_to_id IS NULL AND {REACHED_RELEASE_R})"
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

/// An artist's records with files, as each version's title's key and the
/// record.
pub(crate) fn records_of(
    connection: &Connection,
    artist: i64,
) -> Result<Vec<(String, i64)>, Error> {
    let mut statement = connection.prepare_cached(&format!(
        "SELECT DISTINCT r.release_group_id, r.display_title FROM release r
         JOIN artist_credit_name acn ON acn.artist_credit_id = r.display_artist_credit_id
         JOIN artist ca ON ca.id = acn.artist_id
         WHERE ca.canonical_id = ?1 AND r.redirect_to_id IS NULL
           AND r.display_title IS NOT NULL AND {REACHED_RELEASE_R}"
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

/// Links plays to what they were heard as, reading the library's artists
/// and records once for every play it links, and tidies the rows it
/// touched when it is done.
pub(crate) struct Linker<'c> {
    connection: &'c Connection,
    rev: i64,
    /// The library's artists that have a record, by key.
    library: Option<HashMap<String, Vec<i64>>>,
    /// Each artist's records, as each version's title's key and the group.
    records: HashMap<i64, Vec<(String, i64)>>,
    /// Each recording's credited artists, as canonical.
    credits: HashMap<i64, Vec<i64>>,
    /// Each appearance's record.
    groups: HashMap<i64, Option<i64>>,
    artists_touched: BTreeSet<i64>,
    albums_touched: BTreeSet<i64>,
}

impl<'c> Linker<'c> {
    pub(crate) fn new(connection: &'c Connection, rev: i64) -> Self {
        Self {
            connection,
            rev,
            library: None,
            records: HashMap::new(),
            credits: HashMap::new(),
            groups: HashMap::new(),
            artists_touched: BTreeSet::new(),
            albums_touched: BTreeSet::new(),
        }
    }

    /// The library's artist whose name has this key, when exactly one
    /// artist with a record has it.
    fn library_artist(&mut self, key: &str) -> Result<Option<i64>, Error> {
        if self.library.is_none() {
            self.library = Some(library_artists(self.connection)?);
        }
        Ok(self
            .library
            .as_ref()
            .and_then(|library| only(library.get(key))))
    }

    /// The artist's record whose title has this key, when exactly one of
    /// theirs has it, in any of its versions.
    fn library_record(&mut self, artist: i64, key: &str) -> Result<Option<i64>, Error> {
        if !self.records.contains_key(&artist) {
            self.records
                .insert(artist, records_of(self.connection, artist)?);
        }
        Ok(record_named(self.records.get(&artist), key))
    }

    /// The artists a recording's credit names, as canonical, in order.
    fn credited(&mut self, recording: i64) -> Result<Vec<i64>, Error> {
        if let Some(artists) = self.credits.get(&recording) {
            return Ok(artists.clone());
        }
        let artists: Vec<i64> = self
            .connection
            .prepare_cached(
                "SELECT ca.canonical_id FROM recording r
                 JOIN artist_credit_name acn ON acn.artist_credit_id = r.display_artist_credit_id
                 JOIN artist ca ON ca.id = acn.artist_id
                 WHERE r.id = ?1 GROUP BY ca.canonical_id ORDER BY min(acn.position)",
            )?
            .query_map([recording], |row| row.get(0))?
            .collect::<Result<_, _>>()?;
        self.credits.insert(recording, artists.clone());
        Ok(artists)
    }

    /// The record an appearance is on.
    fn record_of(&mut self, appearance: i64) -> Result<Option<i64>, Error> {
        if let Some(group) = self.groups.get(&appearance) {
            return Ok(*group);
        }
        let group: Option<i64> = self
            .connection
            .prepare_cached(
                "SELECT rc.release_group_id FROM track_appearance ta
                 JOIN release r ON r.id = ta.release_id
                 JOIN release rc ON rc.id = r.canonical_id
                 WHERE ta.id = ?1",
            )?
            .query_row([appearance], |row| row.get(0))
            .optional()?;
        self.groups.insert(appearance, group);
        Ok(group)
    }

    /// The row of the library's artist, made if there is none.
    fn artist_row(&mut self, artist: i64) -> Result<i64, Error> {
        let found: Option<i64> = self
            .connection
            .prepare_cached("SELECT id FROM heard_artist WHERE artist_id = ?1")?
            .query_row([artist], |row| row.get(0))
            .optional()?;
        let id = match found {
            Some(id) => id,
            None => {
                self.connection
                    .prepare_cached("INSERT INTO heard_artist (artist_id, rev) VALUES (?1, ?2)")?
                    .execute((artist, self.rev))?;
                self.connection.last_insert_rowid()
            }
        };
        self.artists_touched.insert(id);
        Ok(id)
    }

    /// The row of an artist's name, made if there is none.
    fn name_row(&mut self, key: &str, spelling: &str) -> Result<i64, Error> {
        let found: Option<i64> = self
            .connection
            .prepare_cached("SELECT id FROM heard_artist WHERE name_key = ?1")?
            .query_row([key], |row| row.get(0))
            .optional()?;
        let id = match found {
            Some(id) => id,
            None => {
                self.connection
                    .prepare_cached(
                        "INSERT INTO heard_artist (name_key, name, rev) VALUES (?1, ?2, ?3)",
                    )?
                    .execute((key, spelling, self.rev))?;
                self.connection.last_insert_rowid()
            }
        };
        self.artists_touched.insert(id);
        Ok(id)
    }

    /// The row of the library's record, made if there is none.
    fn record_row(&mut self, group: i64) -> Result<i64, Error> {
        let found: Option<i64> = self
            .connection
            .prepare_cached("SELECT id FROM heard_album WHERE release_group_id = ?1")?
            .query_row([group], |row| row.get(0))
            .optional()?;
        let id = match found {
            Some(id) => id,
            None => {
                self.connection
                    .prepare_cached(
                        "INSERT INTO heard_album (release_group_id, rev) VALUES (?1, ?2)",
                    )?
                    .execute((group, self.rev))?;
                self.connection.last_insert_rowid()
            }
        };
        self.albums_touched.insert(id);
        Ok(id)
    }

    /// The row of an album's name under an artist's row, made if there is
    /// none.
    fn album_row(&mut self, artist: i64, key: &str, spelling: &str) -> Result<i64, Error> {
        let found: Option<i64> = self
            .connection
            .prepare_cached(
                "SELECT id FROM heard_album WHERE heard_artist_id = ?1 AND name_key = ?2",
            )?
            .query_row((artist, key), |row| row.get(0))
            .optional()?;
        let id = match found {
            Some(id) => id,
            None => {
                self.connection
                    .prepare_cached(
                        "INSERT INTO heard_album (heard_artist_id, name_key, name, rev)
                         VALUES (?1, ?2, ?3, ?4)",
                    )?
                    .execute((artist, key, spelling, self.rev))?;
                self.connection.last_insert_rowid()
            }
        };
        self.albums_touched.insert(id);
        Ok(id)
    }

    /// Links one play to the artists and the album it was heard as.
    pub(crate) fn link(&mut self, heard: &ToLink) -> Result<(), Error> {
        let names: Option<(String, Option<String>)> = self
            .connection
            .prepare_cached(
                "SELECT artist, album FROM observation_import WHERE observation_id = ?1",
            )?
            .query_row([heard.primary], |row| Ok((row.get(0)?, row.get(1)?)))
            .optional()?;
        let (artist_name, album_name) = match names {
            Some((artist, album)) => (Some(artist), album),
            None => (None, None),
        };

        // A bound play is of its credit's artists; any other, of its name's.
        let mut library = match heard.recording {
            Some(recording) => self.credited(recording)?,
            None => Vec::new(),
        };
        let mut artists: Vec<(i64, Option<&str>)> = Vec::new();
        for artist in &library {
            artists.push((self.artist_row(*artist)?, None));
        }
        if artists.is_empty()
            && let Some(name) = artist_name.as_deref()
        {
            let key = artist_key(name);
            if !key.is_empty() {
                match self.library_artist(&key)? {
                    Some(artist) => {
                        library.push(artist);
                        artists.push((self.artist_row(artist)?, Some(name)));
                    }
                    None => artists.push((self.name_row(&key, name)?, Some(name))),
                }
            }
        }

        // The record it was heard from; or the one of its artist's whose
        // title is the album's name, when exactly one is; or the name.
        let mut album: Option<(i64, Option<&str>)> = None;
        if let Some(appearance) = heard.appearance
            && let Some(group) = self.record_of(appearance)?
        {
            album = Some((self.record_row(group)?, None));
        }
        if album.is_none()
            && let Some(name) = album_name.as_deref()
            && let Some(&(first, _)) = artists.first()
        {
            let key = album_key(name);
            if !key.is_empty() {
                let mut group = None;
                for artist in &library {
                    group = self.library_record(*artist, &key)?;
                    if group.is_some() {
                        break;
                    }
                }
                album = Some(match group {
                    Some(group) => (self.record_row(group)?, Some(name)),
                    None => (self.album_row(first, &key, name)?, Some(name)),
                });
            }
        }

        for (row, spelling) in artists {
            self.connection
                .prepare_cached(
                    "INSERT OR IGNORE INTO play_event_artist
                       (heard_artist_id, play_event_id, counted, local_date, spelling, rev)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                )?
                .execute((
                    row,
                    heard.event,
                    heard.counted,
                    heard.local_date,
                    spelling,
                    self.rev,
                ))?;
        }
        if let Some((row, spelling)) = album {
            self.connection
                .prepare_cached(
                    "INSERT INTO play_event_album
                       (heard_album_id, play_event_id, counted, local_date, spelling, rev)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                )?
                .execute((
                    row,
                    heard.event,
                    heard.counted,
                    heard.local_date,
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
                    "SELECT heard_artist_id FROM play_event_artist WHERE play_event_id = ?1",
                )?
                .query_map([play], |row| row.get(0))?
                .collect::<Result<_, _>>()?;
            self.artists_touched.extend(artists);
            let albums: Vec<i64> = self
                .connection
                .prepare_cached(
                    "SELECT heard_album_id FROM play_event_album WHERE play_event_id = ?1",
                )?
                .query_map([play], |row| row.get(0))?
                .collect::<Result<_, _>>()?;
            self.albums_touched.extend(albums);
            self.connection
                .prepare_cached("DELETE FROM play_event_artist WHERE play_event_id = ?1")?
                .execute([play])?;
            self.connection
                .prepare_cached("DELETE FROM play_event_album WHERE play_event_id = ?1")?
                .execute([play])?;
        }
        Ok(())
    }

    /// Tidies the rows it touched: a row no play links to any more goes, and
    /// a name is spelled as its plays spell it most.
    pub(crate) fn finish(self) -> Result<(), Error> {
        for album in &self.albums_touched {
            let deleted = self
                .connection
                .prepare_cached(
                    "DELETE FROM heard_album WHERE id = ?1
                       AND NOT EXISTS (SELECT 1 FROM play_event_album WHERE heard_album_id = ?1)",
                )?
                .execute([album])?;
            if deleted == 0 {
                self.connection
                    .prepare_cached(
                        "UPDATE heard_album SET name = spelled.spelling, rev = ?2
                         FROM (SELECT spelling FROM play_event_album
                               WHERE heard_album_id = ?1 AND spelling IS NOT NULL
                               GROUP BY spelling ORDER BY count(*) DESC, spelling LIMIT 1) spelled
                         WHERE id = ?1 AND name_key IS NOT NULL AND name IS NOT spelled.spelling",
                    )?
                    .execute((album, self.rev))?;
            }
        }
        for artist in &self.artists_touched {
            let deleted = self
                .connection
                .prepare_cached(
                    "DELETE FROM heard_artist WHERE id = ?1
                       AND NOT EXISTS (SELECT 1 FROM play_event_artist WHERE heard_artist_id = ?1)
                       AND NOT EXISTS (SELECT 1 FROM heard_album WHERE heard_artist_id = ?1)",
                )?
                .execute([artist])?;
            if deleted == 0 {
                self.connection
                    .prepare_cached(
                        "UPDATE heard_artist SET name = spelled.spelling, rev = ?2
                         FROM (SELECT spelling FROM play_event_artist
                               WHERE heard_artist_id = ?1 AND spelling IS NOT NULL
                               GROUP BY spelling ORDER BY count(*) DESC, spelling LIMIT 1) spelled
                         WHERE id = ?1 AND name_key IS NOT NULL AND name IS NOT spelled.spelling",
                    )?
                    .execute((artist, self.rev))?;
            }
        }
        Ok(())
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

/// Links every play again, as the library holds it now: after a change to
/// the catalog, which can give a name an artist or a record of the library,
/// or take one away, without moving the play.
pub(crate) fn relink(connection: &Connection, rev: i64) -> Result<(), Error> {
    clear(connection)?;
    let mut linker = Linker::new(connection, rev);
    let mut statement = connection.prepare(
        "SELECT pe.id, pe.recording_id, pe.appearance_id, peo.observation_id,
                pe.counts = 1 AND pe.excluded = 0, pe.local_date
         FROM play_event pe
         JOIN play_event_observation peo ON peo.play_event_id = pe.id AND peo.role = 'primary'",
    )?;
    let plays: Vec<ToLink> = statement
        .query_map([], |row| {
            Ok(ToLink {
                event: row.get(0)?,
                recording: row.get(1)?,
                appearance: row.get(2)?,
                primary: row.get(3)?,
                counted: row.get(4)?,
                local_date: row.get(5)?,
            })
        })?
        .collect::<Result<_, _>>()?;
    for play in &plays {
        linker.link(play)?;
    }
    linker.finish()
}

/// How often a row was heard, and its first and last local dates.
type Tally = (u64, Option<i64>, Option<i64>);

/// How often and when each row was heard: its plays that count, and their
/// first and last local dates. Read from the index on its links alone.
fn tallies(
    connection: &Connection,
    table: &str,
    column: &str,
) -> Result<HashMap<i64, Tally>, Error> {
    let mut statement = connection.prepare(&format!(
        "SELECT {column}, count(*), min(local_date), max(local_date) FROM {table}
         WHERE counted = 1 GROUP BY {column}"
    ))?;
    let rows = statement.query_map([], |row| {
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

/// A row of a History list as it is sorted.
struct Listed<T> {
    item: T,
    sort_key: String,
    plays: u64,
    first: Option<i64>,
    last: Option<i64>,
}

impl<T> Listed<T> {
    fn heard(&self) -> Heard {
        Heard {
            plays: self.plays,
            first_played: self.first.map(crate::history_pages::date),
            last_played: self.last.map(crate::history_pages::date),
        }
    }
}

/// Adds one tally to another, as when two rows are one artist since a
/// merge.
fn add(into: &mut Tally, (plays, first, last): Tally) {
    into.0 += plays;
    into.1 = [into.1, first].into_iter().flatten().min();
    into.2 = [into.2, last].into_iter().flatten().max();
}

/// Sorts the rows as asked, and keeps the page asked for.
fn page<T>(mut rows: Vec<Listed<T>>, params: &HeardListParams) -> (Vec<Listed<T>>, u64) {
    rows.retain(|row| row.plays > 0);
    let total = rows.len() as u64;
    rows.sort_by(|a, b| {
        let order = match params.order {
            HeardOrder::Plays => b.plays.cmp(&a.plays),
            // A row with no date goes last either way.
            HeardOrder::First => a
                .first
                .is_none()
                .cmp(&b.first.is_none())
                .then(a.first.cmp(&b.first)),
            HeardOrder::Last => a
                .last
                .is_none()
                .cmp(&b.last.is_none())
                .then(b.last.cmp(&a.last)),
        };
        order.then_with(|| a.sort_key.cmp(&b.sort_key))
    });
    let limit = params.limit.unwrap_or(DEFAULT_HEARD).min(MAX_HEARD) as usize;
    let rows = rows
        .into_iter()
        .skip(params.offset as usize)
        .take(limit)
        .collect();
    (rows, total)
}

/// The library revision.
fn revision(connection: &Connection) -> Result<u64, Error> {
    let rev: i64 =
        connection.query_row("SELECT rev FROM library_meta WHERE id = 1", [], |row| {
            row.get(0)
        })?;
    Ok(u64::try_from(rev).unwrap_or(0))
}

/// Every artist heard, as asked.
fn artists(connection: &Connection, params: &HeardListParams) -> Result<HeardArtists, Error> {
    let tallies = tallies(connection, "play_event_artist", "heard_artist_id")?;
    let mut statement = connection.prepare(
        "SELECT h.id, ca.id, ca.public_id, ca.display_name, h.name_key, h.name
         FROM heard_artist h
         LEFT JOIN artist a ON a.id = h.artist_id
         LEFT JOIN artist ca ON ca.id = a.canonical_id",
    )?;
    let mut rows = statement.query([])?;
    // The library's artists by canonical ID, since two rows are one artist
    // after a merge; names by their row.
    let mut library: HashMap<i64, (HeardArtist, Tally)> = HashMap::new();
    let mut named = Vec::new();
    while let Some(row) = rows.next()? {
        let tally = tallies
            .get(&row.get::<_, i64>(0)?)
            .copied()
            .unwrap_or_default();
        match row.get::<_, Option<i64>>(1)? {
            Some(canonical) => {
                let name: String = row.get::<_, Option<String>>(3)?.unwrap_or_default();
                let id = Id::from_bytes(row.get(2)?);
                let entry = library.entry(canonical).or_insert_with(|| {
                    (
                        HeardArtist {
                            key: artist_key(&name),
                            name,
                            artist_id: Some(id),
                            heard: Heard::default(),
                        },
                        (0, None, None),
                    )
                });
                add(&mut entry.1, tally);
            }
            None => {
                let (key, name): (String, String) = (row.get(4)?, row.get(5)?);
                named.push((
                    HeardArtist {
                        key,
                        name,
                        artist_id: None,
                        heard: Heard::default(),
                    },
                    tally,
                ));
            }
        }
    }
    let listed: Vec<Listed<HeardArtist>> = library
        .into_values()
        .chain(named)
        .map(|(item, (plays, first, last))| Listed {
            sort_key: format!("{}\u{0}{}", item.key, item.name),
            item,
            plays,
            first,
            last,
        })
        .collect();
    let (listed, total) = page(listed, params);
    Ok(HeardArtists {
        artists: listed
            .into_iter()
            .map(|row| HeardArtist {
                heard: row.heard(),
                ..row.item
            })
            .collect(),
        total,
        library_revision: revision(connection)?,
    })
}

/// Every album heard, as asked.
fn albums(connection: &Connection, params: &HeardListParams) -> Result<HeardAlbums, Error> {
    let tallies = tallies(connection, "play_event_album", "heard_album_id")?;
    let mut statement = connection.prepare(
        "SELECT h.id, rg.public_id,
                coalesce(shown.display_title,
                         (SELECT r.display_title FROM release r
                          WHERE r.release_group_id = rg.id AND r.redirect_to_id IS NULL
                            AND r.display_title IS NOT NULL ORDER BY r.id LIMIT 1)),
                coalesce(credit.display_text,
                         (SELECT c.display_text FROM release r
                          JOIN artist_credit c ON c.id = r.display_artist_credit_id
                          WHERE r.release_group_id = rg.id AND r.redirect_to_id IS NULL
                          ORDER BY r.id LIMIT 1)),
                h.name_key, h.name, ca.display_name, ha.name_key, ha.name
         FROM heard_album h
         LEFT JOIN release_group rg ON rg.id = h.release_group_id
         LEFT JOIN release shown ON shown.id = rg.display_release_id
         LEFT JOIN artist_credit credit ON credit.id = shown.display_artist_credit_id
         LEFT JOIN heard_artist ha ON ha.id = h.heard_artist_id
         LEFT JOIN artist a ON a.id = ha.artist_id
         LEFT JOIN artist ca ON ca.id = a.canonical_id",
    )?;
    let mut rows = statement.query([])?;
    let mut listed = Vec::new();
    while let Some(row) = rows.next()? {
        let (plays, first, last) = tallies
            .get(&row.get::<_, i64>(0)?)
            .copied()
            .unwrap_or_default();
        let item = match row.get::<_, Option<[u8; 16]>>(1)? {
            Some(group) => {
                let name: String = row.get::<_, Option<String>>(2)?.unwrap_or_default();
                let artist_name: String = row.get::<_, Option<String>>(3)?.unwrap_or_default();
                HeardAlbum {
                    artist_key: artist_key(&artist_name),
                    key: album_key(&name),
                    artist_name,
                    name,
                    album_id: Some(Id::from_bytes(group)),
                    heard: Heard::default(),
                }
            }
            None => {
                let (artist_key, artist_name) = match row.get::<_, Option<String>>(6)? {
                    Some(library) => (artist_key(&library), library),
                    None => (
                        row.get::<_, Option<String>>(7)?.unwrap_or_default(),
                        row.get::<_, Option<String>>(8)?.unwrap_or_default(),
                    ),
                };
                HeardAlbum {
                    artist_key,
                    artist_name,
                    key: row.get(4)?,
                    name: row.get(5)?,
                    album_id: None,
                    heard: Heard::default(),
                }
            }
        };
        listed.push(Listed {
            sort_key: format!("{}\u{0}{}\u{0}{}", item.key, item.artist_key, item.name),
            item,
            plays,
            first,
            last,
        });
    }
    let (listed, total) = page(listed, params);
    Ok(HeardAlbums {
        albums: listed
            .into_iter()
            .map(|row| HeardAlbum {
                heard: row.heard(),
                ..row.item
            })
            .collect(),
        total,
        library_revision: revision(connection)?,
    })
}

impl LibraryStore {
    /// `history.artists`: every artist heard, with files or not.
    pub fn heard_artists(&self, params: HeardListParams) -> Result<HeardArtists, Error> {
        self.read_snapshot(move |connection| artists(connection, &params))
    }

    /// `history.albums`: every album heard, with files or not.
    pub fn heard_albums(&self, params: HeardListParams) -> Result<HeardAlbums, Error> {
        self.read_snapshot(move |connection| albums(connection, &params))
    }
}

#[cfg(test)]
mod tests;
