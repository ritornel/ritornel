//! Lists read through views: the engine's half. See "Large lists" in
//! docs/design/contract.md.
//!
//! [`LibraryStore::list_open`] resolves a query once, in one short read
//! transaction, into the ordered row IDs a view holds, the sections that
//! jump to a letter or a year, and the library revision it was resolved at.
//! The IDs are the tables' own integer keys, one `i64` a row, and never
//! leave the server. An Albums tile is a release group shown as one tile,
//! held as its key, or a release of a group shown expanded, held as its key
//! negated.
//!
//! [`LibraryStore::list_rows`] reads the rows of a stretch of those IDs as
//! the library has them now. A row the library no longer has is
//! [`Row::Gone`].
//!
//! The order is worked out here and not in SQL, because it is the order a
//! person files things in (`fold`), which SQLite's collations do not know:
//! a leading article set aside, accents and case ignored, names that start
//! with no letter after Z.

mod discography;
mod fold;
mod open;
mod pages;
mod rows;
mod search;

use std::fmt;

use ritornel_contract::{
    AlbumPage, AlbumRef, ArtistRow, Id, ListKind, Query, Row, Section, SortKey,
};
use rusqlite::Connection;

use crate::{Error, LibraryStore};

/// A query resolved into the rows a view holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct View {
    /// What the rows are.
    pub kind: ListKind,
    /// The order they are in.
    pub sort: SortKey,
    /// The library revision the query was resolved at.
    pub revision: i64,
    /// The rows, in order, by their keys in `library.db`.
    pub ids: Vec<i64>,
    /// Where each letter, year or stretch of time begins.
    pub sections: Vec<Section>,
}

impl View {
    /// Whether its rows show plays: a song's play count and when it was
    /// last played, which a play taken in changes. No view is sorted or
    /// filtered by plays.
    pub fn shows_plays(&self) -> bool {
        matches!(self.kind, ListKind::Songs)
    }

    /// What the view costs to hold, in bytes, near enough.
    pub fn bytes(&self) -> usize {
        self.ids.capacity() * std::mem::size_of::<i64>()
            + self
                .sections
                .iter()
                .map(|section| std::mem::size_of::<Section>() + section.label.capacity())
                .sum::<usize>()
    }
}

/// Why a list could not be opened.
#[derive(Debug)]
pub enum ListError {
    /// The query is wrong for the list: an order or a predicate it does not
    /// take, or more than one order.
    Invalid(String),
    /// The query is written in a version of the filter language this
    /// server does not read.
    Unsupported(String),
    /// Reading the library failed.
    Engine(Error),
}

impl fmt::Display for ListError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(why) | Self::Unsupported(why) => f.write_str(why),
            Self::Engine(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for ListError {}

impl From<Error> for ListError {
    fn from(error: Error) -> Self {
        Self::Engine(error)
    }
}

impl From<rusqlite::Error> for ListError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Engine(Error::Sqlite(error))
    }
}

impl LibraryStore {
    /// Resolves `query` on a list of `kind` into the rows a view holds.
    pub fn list_open(&self, kind: ListKind, query: &Query) -> Result<View, ListError> {
        let plan = open::Plan::of(kind, query)?;
        let now_ms = self.now_ms();
        self.read(move |connection| {
            // One read transaction, so the revision is the one the rows
            // were read at.
            connection.execute_batch("BEGIN")?;
            let resolved = open::resolve(connection, &plan, now_ms);
            // A read changes nothing, so ending it cannot lose anything.
            let _ = connection.execute_batch("COMMIT");
            resolved
        })
        .map_err(ListError::Engine)
    }

    /// The rows of a list of `kind` with these keys, in the same order, as
    /// the library has them now.
    pub fn list_rows(&self, kind: ListKind, ids: &[i64]) -> Result<Vec<Row>, Error> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let ids = ids.to_vec();
        self.read(move |connection| {
            connection.execute_batch("BEGIN")?;
            let rows = rows::read(connection, kind, &ids);
            let _ = connection.execute_batch("COMMIT");
            rows
        })
    }

    /// The key a row of a list of `kind` is held by, from the ID a client
    /// knows it by. `None` if no row of that kind has that ID.
    pub fn list_key(&self, kind: ListKind, id: &str) -> Result<Option<i64>, Error> {
        let Ok(id) = id.parse::<crate::Id>() else {
            return Ok(None);
        };
        self.read(move |connection| open::key_of(connection, kind, id))
    }

    /// The index in `resolved` of the first row whose sort text starts with
    /// `prefix`, or of the first one after where it would be. `None` for an
    /// order by when or by kind of record, or a view with no rows.
    pub fn list_locate_prefix(&self, resolved: &View, prefix: &str) -> Result<Option<u64>, Error> {
        if matches!(resolved.sort, SortKey::Added | SortKey::Discography) || resolved.ids.is_empty()
        {
            return Ok(None);
        }
        self.read(|connection| open::locate_prefix(connection, resolved, prefix))
    }

    fn now_ms(&self) -> i64 {
        self.clock().now_ms()
    }

    /// The public IDs of the songs a view of songs holds, by their keys,
    /// in the order given: `None` for one the library no longer has.
    pub fn song_ids(&self, keys: &[i64]) -> Result<Vec<Option<Id>>, Error> {
        self.read(|connection| {
            let mut found: std::collections::HashMap<i64, Id> =
                std::collections::HashMap::with_capacity(keys.len());
            let mut each = |row: &rusqlite::Row<'_>| {
                found.insert(row.get(0)?, Id::from_bytes(row.get(1)?));
                Ok(())
            };
            // A long view is most of the table, which one scan reads faster
            // than a lookup a row.
            if keys.len() > 4 * CHUNK {
                let mut statement = connection.prepare(
                    "SELECT id, public_id FROM track_appearance WHERE redirect_to_id IS NULL",
                )?;
                let mut rows = statement.query([])?;
                while let Some(row) = rows.next()? {
                    each(row)?;
                }
            } else {
                for_each_in(
                    connection,
                    "SELECT id, public_id FROM track_appearance
                     WHERE id IN ({}) AND redirect_to_id IS NULL",
                    keys,
                    each,
                )?;
            }
            Ok(keys.iter().map(|key| found.get(key).copied()).collect())
        })
    }

    /// An album's page: the release `album` names, or the one its group is
    /// set to show, with its tracks in disc and track order and the other
    /// versions of its group. `None` if the library has no such record.
    /// Its colour is the cover cache's to say, and is left `None`.
    pub fn album_page(&self, album: AlbumRef) -> Result<Option<AlbumPage>, Error> {
        self.read(move |connection| {
            connection.execute_batch("BEGIN")?;
            let page = pages::album(connection, album);
            let _ = connection.execute_batch("COMMIT");
            page
        })
    }

    /// The artist with this ID, as the Artists list has them, and the
    /// library revision they were read at. `None` if the library has no
    /// such artist.
    pub fn artist_row(&self, artist: Id) -> Result<Option<(ArtistRow, u64)>, Error> {
        self.read(move |connection| {
            connection.execute_batch("BEGIN")?;
            let found = pages::artist(connection, artist);
            let _ = connection.execute_batch("COMMIT");
            found
        })
    }
}

/// The Albums tiles of these release groups, in the same order, read on a
/// connection already in a read transaction.
pub(crate) fn album_tiles(connection: &Connection, groups: &[i64]) -> Result<Vec<Row>, Error> {
    if groups.is_empty() {
        return Ok(Vec::new());
    }
    rows::read(connection, ListKind::Albums, groups)
}

/// `?, ?, ?` for `count` parameters.
fn placeholders(count: usize) -> String {
    let mut text = String::with_capacity(count * 3);
    for index in 0..count {
        if index > 0 {
            text.push_str(", ");
        }
        text.push('?');
    }
    text
}

/// The most parameters one statement is given. SQLite allows more.
const CHUNK: usize = 5_000;

/// Runs `sql`, which has one `{}` where an `IN` list of keys goes, over
/// `keys` a chunk at a time, and hands `each` every row.
fn for_each_in<F>(
    connection: &Connection,
    sql: &str,
    keys: &[i64],
    mut each: F,
) -> Result<(), Error>
where
    F: FnMut(&rusqlite::Row<'_>) -> Result<(), rusqlite::Error>,
{
    for chunk in keys.chunks(CHUNK) {
        let sql = sql.replacen("{}", &placeholders(chunk.len()), 1);
        let mut statement = connection.prepare(&sql)?;
        let mut rows = statement.query(rusqlite::params_from_iter(chunk))?;
        while let Some(row) = rows.next()? {
            each(row)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
