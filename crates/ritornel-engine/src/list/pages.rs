//! The pages about one thing: an album with its parts, and an artist's
//! row. Their lists (an artist's records and songs) are views like any
//! other; these are the reads that are one entity each.

use ritornel_contract::{AlbumPage, AlbumRef, AlbumVersion, ArtistRow, Id, Row};
use rusqlite::{Connection, OptionalExtension};

use super::{discography, for_each_in, rows};
use crate::Error;

fn revision(connection: &Connection) -> Result<u64, Error> {
    let revision: i64 =
        connection.query_row("SELECT rev FROM library_meta WHERE id = 1", [], |row| {
            row.get(0)
        })?;
    Ok(u64::try_from(revision).unwrap_or(0))
}

/// The artist with this public ID, followed to the one they were merged
/// into, as the Artists list has them.
pub(super) fn artist(
    connection: &Connection,
    artist: Id,
) -> Result<Option<(ArtistRow, u64)>, Error> {
    let key: Option<i64> = connection
        .query_row(
            "SELECT canonical_id FROM artist WHERE public_id = ?1",
            [artist.as_bytes()],
            |row| row.get(0),
        )
        .optional()?;
    let Some(key) = key else {
        return Ok(None);
    };
    let revision = revision(connection)?;
    Ok(
        match rows::read(connection, ritornel_contract::ListKind::Artists, &[key])?.pop() {
            Some(Row::Artist(row)) => Some((row, revision)),
            _ => None,
        },
    )
}

/// What a release group is, as an album's page needs it.
struct Group {
    key: i64,
    public_id: Id,
    display: Option<i64>,
    primary: Option<String>,
    marks: Option<String>,
}

fn group(connection: &Connection, key: i64) -> Result<Option<Group>, Error> {
    Ok(connection
        .query_row(
            "SELECT id, display_release_id, display_primary_type, display_secondary_types_json,
                    public_id
             FROM release_group WHERE id = ?1",
            [key],
            |row| {
                Ok(Group {
                    key: row.get(0)?,
                    display: row.get(1)?,
                    primary: row.get(2)?,
                    marks: row.get(3)?,
                    public_id: Id::from_bytes(row.get(4)?),
                })
            },
        )
        .optional()?)
}

/// A release of a group: its key, its ID, its title and its year.
type Version = (i64, Id, Option<String>, Option<i64>);

/// The live releases of a group, oldest first: by year, those with none
/// last, then by when they came into the catalogue.
fn versions(connection: &Connection, group: i64) -> Result<Vec<Version>, Error> {
    let mut statement = connection.prepare(concat!(
        "SELECT id, public_id, display_title, display_year FROM release
         WHERE release_group_id = ?1 AND redirect_to_id IS NULL AND id IN (",
        reached_releases!(),
        ") ORDER BY display_year IS NULL, display_year, id"
    ))?;
    let mut found = Vec::new();
    let mut rows = statement.query([group])?;
    while let Some(row) = rows.next()? {
        found.push((
            row.get(0)?,
            Id::from_bytes(row.get(1)?),
            row.get(2)?,
            row.get(3)?,
        ));
    }
    Ok(found)
}

/// One album's page: the release `album` names, or the one its group is set
/// to show, with its tracks and the group's other versions. `None` if the
/// library has no such record, or no live release of it.
pub(super) fn album(connection: &Connection, album: AlbumRef) -> Result<Option<AlbumPage>, Error> {
    // The group, and the release shown.
    let (group_key, release_key) = match album {
        AlbumRef::ReleaseGroup { id } => {
            let key: Option<i64> = connection
                .query_row(
                    "SELECT id FROM release_group WHERE public_id = ?1",
                    [id.as_bytes()],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(key) = key else { return Ok(None) };
            let Some(group) = group(connection, key)? else {
                return Ok(None);
            };
            let live = versions(connection, key)?;
            // The release the group is set to show if it is live, or else
            // its oldest, as the Albums list chooses.
            let shown = group
                .display
                .filter(|display| live.iter().any(|(id, ..)| id == display))
                .or_else(|| live.iter().map(|(id, ..)| *id).min());
            let Some(shown) = shown else { return Ok(None) };
            (key, shown)
        }
        AlbumRef::Release { id } => {
            // An old ID still resolves, to the release it was merged into.
            let found: Option<(i64, i64)> = connection
                .query_row(
                    "SELECT c.id, c.release_group_id FROM release r
                     JOIN release c ON c.id = r.canonical_id
                     WHERE r.public_id = ?1",
                    [id.as_bytes()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            let Some((release, group)) = found else {
                return Ok(None);
            };
            (group, release)
        }
    };
    let Some(group) = group(connection, group_key)? else {
        return Ok(None);
    };
    // The tile, as the Albums list would draw it.
    let tile_key = match album {
        AlbumRef::ReleaseGroup { .. } => group.key,
        AlbumRef::Release { .. } => -release_key,
    };
    let Some(Row::Album(mut tile)) =
        rows::read(connection, ritornel_contract::ListKind::Albums, &[tile_key])?.pop()
    else {
        return Ok(None);
    };
    // A release asked for by an ID it was merged from is named by the one
    // it is now.
    let release_id: Id = connection.query_row(
        "SELECT public_id FROM release WHERE id = ?1",
        [release_key],
        |row| Ok(Id::from_bytes(row.get(0)?)),
    )?;
    if let AlbumRef::Release { .. } = album {
        tile.album = AlbumRef::Release { id: release_id };
    }

    // Its tracks, in disc and track order; a track with no number after
    // those with one.
    let mut statement = connection.prepare(concat!(
        "SELECT id FROM track_appearance
         WHERE release_id = ?1 AND redirect_to_id IS NULL AND id IN (",
        reached!(),
        ") ORDER BY display_disc_number IS NULL, display_disc_number,
                    display_track_number IS NULL, display_track_number, id"
    ))?;
    let mut keys = Vec::new();
    let mut found = statement.query([release_key])?;
    while let Some(row) = found.next()? {
        keys.push(row.get::<_, i64>(0)?);
    }
    let tracks = rows::read(connection, ritornel_contract::ListKind::Songs, &keys)?
        .into_iter()
        .filter_map(|row| match row {
            Row::Song(song) => Some(song),
            _ => None,
        })
        .collect();

    // The other versions, each with how many tracks and its best copy.
    let live = versions(connection, group.key)?;
    let others: Vec<i64> = live
        .iter()
        .map(|(key, ..)| *key)
        .filter(|key| *key != release_key)
        .collect();
    let counts = rows::track_counts(connection, &others)?;
    let mut release_of = std::collections::HashMap::new();
    for_each_in(
        connection,
        concat!(
            "SELECT id, release_id FROM track_appearance
             WHERE redirect_to_id IS NULL AND release_id IN ({}) AND id IN (",
            reached!(),
            ")"
        ),
        &others,
        |row| {
            release_of.insert(row.get::<_, i64>(0)?, row.get::<_, i64>(1)?);
            Ok(())
        },
    )?;
    let qualities = rows::qualities(connection, &release_of)?;
    let shown_by_group = group
        .display
        .filter(|display| live.iter().any(|(id, ..)| id == display))
        .or_else(|| live.iter().map(|(id, ..)| *id).min());
    let versions = live
        .into_iter()
        .filter(|(key, ..)| *key != release_key)
        .map(|(key, id, title, year)| AlbumVersion {
            release_id: id,
            title,
            year,
            track_count: counts.get(&key).map_or(0, |(count, _)| *count),
            quality: qualities.get(&key).cloned(),
            shown: shown_by_group == Some(key),
        })
        .collect();

    let record_type = discography::record_type(group.primary.as_deref());
    let marks = discography::marks(group.marks.as_deref());
    Ok(Some(AlbumPage {
        tile,
        release_id,
        release_group_id: group.public_id,
        record_type,
        shelf: discography::shelf(record_type, &marks, true),
        tracks,
        versions,
        art_color: None,
        library_revision: revision(connection)?,
    }))
}
