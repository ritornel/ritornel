//! Reading the rows of a view: what each row says now.
//!
//! Each kind is read in a few statements over the whole window, never one
//! a row, each by an index: a primary key, or one of those migration 0002
//! adds for these reads, named where they are used.

use std::collections::HashMap;

use ritornel_contract::{
    AlbumRef, AlbumTile, ArtistRow, CopyRole, Credit, Format, GoneRow, Id, ListKind, Quality, Row,
    SongRow, TrackCopy,
};
use rusqlite::Connection;

use super::fold;
use super::for_each_in;
use crate::Error;

/// The rows with these keys, in order.
pub(super) fn read(
    connection: &Connection,
    kind: ListKind,
    keys: &[i64],
) -> Result<Vec<Row>, Error> {
    match kind {
        ListKind::Artists => artists(connection, keys),
        ListKind::Albums => albums(connection, keys),
        ListKind::Songs => songs(connection, keys),
    }
}

fn gone() -> Row {
    Row::Gone(GoneRow { gone: true })
}

fn id(bytes: [u8; 16]) -> Id {
    Id::from_bytes(bytes)
}

fn count(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

/// The credits with these keys: their text, and their artists in order, by
/// the public ID of the artist each is now.
fn credits(connection: &Connection, keys: &[i64]) -> Result<HashMap<i64, Credit>, Error> {
    let mut credits: HashMap<i64, Credit> = HashMap::new();
    for_each_in(
        connection,
        "SELECT id, display_text FROM artist_credit WHERE id IN ({})",
        keys,
        |row| {
            credits.insert(
                row.get(0)?,
                Credit {
                    text: row.get(1)?,
                    artist_ids: Vec::new(),
                },
            );
            Ok(())
        },
    )?;
    // By `artist_credit_name (artist_credit_id, position)`, an index.
    for_each_in(
        connection,
        "SELECT acn.artist_credit_id, ca.public_id
         FROM artist_credit_name acn
         JOIN artist a ON a.id = acn.artist_id
         JOIN artist ca ON ca.id = a.canonical_id
         WHERE acn.artist_credit_id IN ({})
         ORDER BY acn.artist_credit_id, acn.position",
        keys,
        |row| {
            if let Some(credit) = credits.get_mut(&row.get::<_, i64>(0)?) {
                credit.artist_ids.push(id(row.get(1)?));
            }
            Ok(())
        },
    )?;
    Ok(credits)
}

// ---------------------------------------------------------------- artists

/// One of an artist's records, as the Albums list shows it.
struct ArtistTile {
    /// The release it shows.
    release: i64,
    /// The year it first came out.
    year: Option<i64>,
    /// Its title, as it is filed.
    title_key: String,
    /// Its cover.
    art: Option<[u8; 16]>,
}

/// What an artist's records add up to.
#[derive(Default)]
struct Records {
    /// The groups shown as one tile, and the releases of groups shown
    /// expanded, that name the artist.
    tiles: HashMap<i64, ArtistTile>,
}

fn artists(connection: &Connection, keys: &[i64]) -> Result<Vec<Row>, Error> {
    let mut found: HashMap<i64, (Id, Option<String>, Option<String>)> = HashMap::new();
    for_each_in(
        connection,
        "SELECT id, public_id, display_name, display_sort_name FROM artist
         WHERE id IN ({}) AND redirect_to_id IS NULL",
        keys,
        |row| {
            found.insert(row.get(0)?, (id(row.get(1)?), row.get(2)?, row.get(3)?));
            Ok(())
        },
    )?;
    let live: Vec<i64> = keys
        .iter()
        .copied()
        .filter(|key| found.contains_key(key))
        .collect();

    // Their records, by `release_display_artist_credit`. The credits'
    // names are a small table, read whole.
    let mut records: HashMap<i64, Records> = HashMap::new();
    for_each_in(
        connection,
        "SELECT ca.canonical_id, r.id, r.release_group_id, rg.presentation,
                rg.display_release_id, coalesce(rg.display_first_year, r.display_year),
                r.display_title, p.public_id
         FROM artist ca
         JOIN artist_credit_name acn ON acn.artist_id = ca.id
         JOIN release r ON r.display_artist_credit_id = acn.artist_credit_id
         JOIN release_group rg ON rg.id = r.release_group_id
         LEFT JOIN picture p ON p.id = r.display_picture_id
         WHERE r.redirect_to_id IS NULL AND ca.canonical_id IN ({})",
        &live,
        |row| {
            let artist: i64 = row.get(0)?;
            let release: i64 = row.get(1)?;
            let group: i64 = row.get(2)?;
            let presentation: Option<String> = row.get(3)?;
            let display: Option<i64> = row.get(4)?;
            let year: Option<i64> = row.get(5)?;
            let title: Option<String> = row.get(6)?;
            let art: Option<[u8; 16]> = row.get(7)?;
            let tile = if presentation.as_deref() == Some("expanded") {
                -release
            } else {
                group
            };
            let entry = records.entry(artist).or_default();
            let candidate = ArtistTile {
                release,
                year,
                title_key: fold::sort_text(title.as_deref().unwrap_or_default(), None),
                art,
            };
            match entry.tiles.get_mut(&tile) {
                // A collapsed group shows the release it is set to show, or
                // else its oldest.
                Some(held)
                    if tile > 0
                        && (display == Some(release)
                            || (display != Some(held.release) && release < held.release)) =>
                {
                    *held = candidate;
                }
                Some(_) => {}
                None => {
                    entry.tiles.insert(tile, candidate);
                }
            }
            Ok(())
        },
    )?;

    // How many tracks the records they show have, by
    // `track_appearance_release`.
    let shown: Vec<i64> = records
        .values()
        .flat_map(|records| records.tiles.values().map(|tile| tile.release))
        .collect();
    let tracks = track_counts(connection, &shown)?;

    keys.iter()
        .map(|key| {
            let Some((artist_id, name, sort_name)) = found.get(key) else {
                return Ok(gone());
            };
            let mine = records.get(key);
            let tiles = mine.map(|records| &records.tiles);
            let album_count = tiles.map_or(0, HashMap::len) as u64;
            let track_count = tiles.map_or(0, |tiles| {
                tiles
                    .values()
                    .map(|tile| tracks.get(&tile.release).map_or(0, |(count, _)| *count))
                    .sum()
            });
            // Their earliest record that has a cover: by the year it first
            // came out, then by title, then by which came into the library
            // first.
            let art_id = tiles.and_then(|tiles| {
                tiles
                    .values()
                    .filter(|tile| tile.art.is_some())
                    .min_by(|a, b| {
                        a.year
                            .unwrap_or(i64::MAX)
                            .cmp(&b.year.unwrap_or(i64::MAX))
                            .then_with(|| a.title_key.cmp(&b.title_key))
                            .then(a.release.cmp(&b.release))
                    })
                    .and_then(|tile| tile.art.map(id))
            });
            Ok(Row::Artist(ArtistRow {
                artist_id: *artist_id,
                name: name.clone(),
                sort_name: sort_name.clone(),
                album_count,
                track_count,
                art_id,
            }))
        })
        .collect()
}

/// How many tracks each of these releases has, and how long they are.
fn track_counts(
    connection: &Connection,
    releases: &[i64],
) -> Result<HashMap<i64, (u64, u64)>, Error> {
    let mut counts = HashMap::new();
    for_each_in(
        connection,
        "SELECT release_id, count(*), total(display_duration_ms) FROM track_appearance
         WHERE redirect_to_id IS NULL AND release_id IN ({})
         GROUP BY release_id",
        releases,
        |row| {
            let duration: f64 = row.get(2)?;
            counts.insert(
                row.get::<_, i64>(0)?,
                (count(row.get(1)?), duration.max(0.0) as u64),
            );
            Ok(())
        },
    )?;
    Ok(counts)
}

// ---------------------------------------------------------------- albums

struct ShownRelease {
    title: Option<String>,
    year: Option<i64>,
    credit: Option<i64>,
    art: Option<Id>,
}

fn albums(connection: &Connection, keys: &[i64]) -> Result<Vec<Row>, Error> {
    let groups: Vec<i64> = keys.iter().copied().filter(|key| *key > 0).collect();
    let expanded: Vec<i64> = keys
        .iter()
        .filter(|key| **key < 0)
        .map(|key| -key)
        .collect();

    // The groups: their public IDs, the release each is set to show, and the
    // year each first came out.
    let mut group_rows: HashMap<i64, (Id, Option<i64>, Option<i64>)> = HashMap::new();
    for_each_in(
        connection,
        "SELECT id, public_id, display_release_id, display_first_year FROM release_group
         WHERE id IN ({})",
        &groups,
        |row| {
            group_rows.insert(row.get(0)?, (id(row.get(1)?), row.get(2)?, row.get(3)?));
            Ok(())
        },
    )?;
    // The releases of groups shown expanded.
    let mut release_rows: HashMap<i64, (Id, i64)> = HashMap::new();
    for_each_in(
        connection,
        "SELECT id, public_id, release_group_id FROM release
         WHERE id IN ({}) AND redirect_to_id IS NULL",
        &expanded,
        |row| {
            release_rows.insert(row.get(0)?, (id(row.get(1)?), row.get(2)?));
            Ok(())
        },
    )?;
    // Every live release of those groups, for the release a group shows and
    // how many versions it has, by `release_release_group`.
    let all_groups: Vec<i64> = groups
        .iter()
        .copied()
        .chain(release_rows.values().map(|(_, group)| *group))
        .collect();
    let mut versions: HashMap<i64, Vec<i64>> = HashMap::new();
    for_each_in(
        connection,
        "SELECT release_group_id, id FROM release
         WHERE redirect_to_id IS NULL AND release_group_id IN ({})",
        &all_groups,
        |row| {
            versions.entry(row.get(0)?).or_default().push(row.get(1)?);
            Ok(())
        },
    )?;

    // The release each tile shows.
    let mut shown_of: HashMap<i64, i64> = HashMap::new();
    for key in keys {
        if *key < 0 {
            if release_rows.contains_key(&-key) {
                shown_of.insert(*key, -key);
            }
        } else if let Some((_, display, _)) = group_rows.get(key) {
            let live = versions.get(key);
            let shown = display
                .filter(|display| live.is_some_and(|live| live.contains(display)))
                .or_else(|| live.and_then(|live| live.iter().min().copied()));
            if let Some(shown) = shown {
                shown_of.insert(*key, shown);
            }
        }
    }
    let shown_keys: Vec<i64> = shown_of.values().copied().collect();
    let mut shown: HashMap<i64, ShownRelease> = HashMap::new();
    for_each_in(
        connection,
        "SELECT r.id, r.display_title, r.display_year, r.display_artist_credit_id, p.public_id
         FROM release r LEFT JOIN picture p ON p.id = r.display_picture_id
         WHERE r.id IN ({})",
        &shown_keys,
        |row| {
            shown.insert(
                row.get(0)?,
                ShownRelease {
                    title: row.get(1)?,
                    year: row.get(2)?,
                    credit: row.get(3)?,
                    art: row.get::<_, Option<[u8; 16]>>(4)?.map(id),
                },
            );
            Ok(())
        },
    )?;
    let credit_keys: Vec<i64> = shown
        .values()
        .filter_map(|release| release.credit)
        .collect();
    let credits = credits(connection, &credit_keys)?;
    // Their tracks, once, for how many and how long, and for their copies,
    // by `track_appearance_release`.
    let mut release_of: HashMap<i64, i64> = HashMap::new();
    let mut tracks: HashMap<i64, (u64, u64)> = HashMap::new();
    for_each_in(
        connection,
        "SELECT id, release_id, display_duration_ms FROM track_appearance
         WHERE redirect_to_id IS NULL AND release_id IN ({})",
        &shown_keys,
        |row| {
            let release: i64 = row.get(1)?;
            let duration: Option<i64> = row.get(2)?;
            release_of.insert(row.get(0)?, release);
            let entry = tracks.entry(release).or_default();
            entry.0 += 1;
            entry.1 += count(duration.unwrap_or(0));
            Ok(())
        },
    )?;
    let qualities = qualities(connection, &release_of)?;

    keys.iter()
        .map(|key| {
            let Some(release_key) = shown_of.get(key) else {
                return Ok(gone());
            };
            let Some(release) = shown.get(release_key) else {
                return Ok(gone());
            };
            let (album, year, group) = if *key < 0 {
                let Some((release_id, group)) = release_rows.get(&-key) else {
                    return Ok(gone());
                };
                (AlbumRef::Release { id: *release_id }, release.year, *group)
            } else {
                let Some((group_id, _, first_year)) = group_rows.get(key) else {
                    return Ok(gone());
                };
                (
                    AlbumRef::ReleaseGroup { id: *group_id },
                    first_year.or(release.year),
                    *key,
                )
            };
            let (track_count, duration_ms) = tracks.get(release_key).copied().unwrap_or((0, 0));
            Ok(Row::Album(AlbumTile {
                album,
                title: release.title.clone(),
                artist_credit: release
                    .credit
                    .and_then(|credit| credits.get(&credit).cloned()),
                year,
                track_count,
                duration_ms,
                version_count: versions
                    .get(&group)
                    .map_or(1, |live| live.len().max(1) as u64),
                art_id: release.art,
                quality: qualities.get(release_key).cloned(),
            }))
        })
        .collect()
}

/// How good a format is, best last: lossless before lossy, then more bits,
/// a higher rate, a higher bit rate.
fn grade(format: &Format) -> (bool, u32, u32, u32) {
    (
        format.lossless,
        format.bit_depth.unwrap_or(0),
        format.sample_rate_hz,
        format.bitrate_kbps.unwrap_or(0),
    )
}

/// The best copy across each release's tracks, and whether they differ,
/// from its appearances, given as the release each is on.
fn qualities(
    connection: &Connection,
    release_of: &HashMap<i64, i64>,
) -> Result<HashMap<i64, Quality>, Error> {
    let appearances: Vec<i64> = release_of.keys().copied().collect();
    let mut formats: HashMap<i64, Vec<Format>> = HashMap::new();
    for_each_formats(connection, &appearances, |appearance, _, _, format| {
        if let Some(release) = release_of.get(&appearance) {
            formats.entry(*release).or_default().push(format);
        }
    })?;
    Ok(formats
        .into_iter()
        .filter_map(|(release, formats)| {
            let varies = formats.windows(2).any(|pair| match pair {
                [a, b] => a != b,
                _ => false,
            });
            let best = formats.into_iter().max_by_key(grade)?;
            Some((release, Quality { best, varies }))
        })
        .collect())
}

/// Hands `each` the current copy of each of these appearances that the
/// library has read: the appearance, the server and kind of root it is in,
/// and its format, by `assignment_current_appearance`.
fn for_each_formats(
    connection: &Connection,
    appearances: &[i64],
    mut each: impl FnMut(i64, Id, CopyRole, Format),
) -> Result<(), Error> {
    for_each_in(
        connection,
        "SELECT asg.appearance_id, srv.public_id, lr.kind, aa.codec, aa.lossless,
                aa.bit_depth, aa.sample_rate_hz, aa.bitrate_kbps
         FROM assignment asg
         JOIN source_item si ON si.id = asg.source_item_id
         JOIN source_item_revision sr ON sr.id = si.current_revision_id
         JOIN audio_asset aa ON aa.id = sr.audio_asset_id
         JOIN source_instance src ON src.id = si.source_instance_id
         JOIN library_root lr ON lr.id = src.root_id
         JOIN server srv ON srv.id = lr.server_id
         WHERE asg.superseded_at_ms IS NULL
           AND si.retired_at_ms IS NULL
           AND si.missing_since_ms IS NULL
           AND asg.appearance_id IN ({})",
        appearances,
        |row| {
            let kind: String = row.get(2)?;
            let format = Format {
                codec: row.get(3)?,
                lossless: row.get(4)?,
                bit_depth: row.get(5)?,
                sample_rate_hz: row.get(6)?,
                bitrate_kbps: row.get(7)?,
            };
            let role = if kind == "cache" {
                CopyRole::Derived
            } else {
                CopyRole::Master
            };
            each(row.get(0)?, id(row.get(1)?), role, format);
            Ok(())
        },
    )
}

// ---------------------------------------------------------------- songs

struct Appearance {
    public_id: Id,
    recording: i64,
    recording_id: Id,
    release_id: Option<Id>,
    title: Option<String>,
    credit: Option<i64>,
    album_title: Option<String>,
    disc: Option<i64>,
    track: Option<i64>,
    year: Option<i64>,
    duration_ms: Option<i64>,
    added_at: Option<i64>,
}

fn songs(connection: &Connection, keys: &[i64]) -> Result<Vec<Row>, Error> {
    let mut found: HashMap<i64, Appearance> = HashMap::new();
    for_each_in(
        connection,
        "SELECT ta.id, ta.public_id, crc.id, crc.public_id, r.public_id, ta.display_title,
                ta.display_artist_credit_id, r.display_title, ta.display_disc_number,
                ta.display_track_number, r.display_year, ta.display_duration_ms,
                ta.display_added_at_ms
         FROM track_appearance ta
         JOIN recording rc ON rc.id = ta.recording_id
         JOIN recording crc ON crc.id = rc.canonical_id
         LEFT JOIN release r ON r.id = ta.release_id
         WHERE ta.id IN ({}) AND ta.redirect_to_id IS NULL",
        keys,
        |row| {
            found.insert(
                row.get(0)?,
                Appearance {
                    public_id: id(row.get(1)?),
                    recording: row.get(2)?,
                    recording_id: id(row.get(3)?),
                    release_id: row.get::<_, Option<[u8; 16]>>(4)?.map(id),
                    title: row.get(5)?,
                    credit: row.get(6)?,
                    album_title: row.get(7)?,
                    disc: row.get(8)?,
                    track: row.get(9)?,
                    year: row.get(10)?,
                    duration_ms: row.get(11)?,
                    added_at: row.get(12)?,
                },
            );
            Ok(())
        },
    )?;
    let credit_keys: Vec<i64> = found.values().filter_map(|song| song.credit).collect();
    let credits = credits(connection, &credit_keys)?;

    // Plays, by the index history.md names for them.
    let recordings: Vec<i64> = found.values().map(|song| song.recording).collect();
    let mut plays: HashMap<i64, (u64, Option<i64>)> = HashMap::new();
    let listener: Option<i64> =
        connection.query_row("SELECT min(id) FROM listener", [], |row| row.get(0))?;
    if let Some(listener) = listener {
        for chunk in recordings.chunks(super::CHUNK) {
            let sql = format!(
                "SELECT recording_id, count(*), max(event_at_utc_ms) FROM play_event
                 WHERE listener_id = ? AND counts = 1 AND excluded = 0 AND recording_id IN ({})
                 GROUP BY recording_id",
                super::placeholders(chunk.len())
            );
            let mut statement = connection.prepare(&sql)?;
            let parameters = std::iter::once(listener).chain(chunk.iter().copied());
            let mut rows = statement.query(rusqlite::params_from_iter(parameters))?;
            while let Some(row) = rows.next()? {
                plays.insert(row.get(0)?, (count(row.get(1)?), row.get(2)?));
            }
        }
    }

    let live: Vec<i64> = keys
        .iter()
        .copied()
        .filter(|key| found.contains_key(key))
        .collect();
    let mut copies: HashMap<i64, Vec<TrackCopy>> = HashMap::new();
    for_each_formats(connection, &live, |appearance, server, role, format| {
        copies.entry(appearance).or_default().push(TrackCopy {
            server,
            role,
            format,
        });
    })?;

    keys.iter()
        .map(|key| {
            let Some(song) = found.get(key) else {
                return Ok(gone());
            };
            let (play_count, last_played_at) =
                plays.get(&song.recording).copied().unwrap_or((0, None));
            Ok(Row::Song(SongRow {
                appearance_id: song.public_id,
                recording_id: song.recording_id,
                release_id: song.release_id,
                title: song.title.clone(),
                artist_credit: song.credit.and_then(|credit| credits.get(&credit).cloned()),
                album_title: song.album_title.clone(),
                disc: song.disc,
                track: song.track,
                year: song.year,
                duration_ms: song.duration_ms,
                play_count,
                last_played_at,
                added_at: song.added_at,
                copies: copies.remove(key).unwrap_or_default(),
            }))
        })
        .collect()
}
