//! Resolving a query into the rows of a view, in order, with its sections.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use ritornel_contract::{FILTER_VERSION, Id, ListKind, Predicate, Query, Section, SortKey};
use rusqlite::{Connection, OptionalExtension};

use super::{ListError, View, fold};
use crate::Error;

/// A query, checked against what its kind of list takes.
#[derive(Debug, Clone)]
pub(super) struct Plan {
    kind: ListKind,
    sort: SortKey,
    /// What was typed, folded. `None` when nothing was.
    text: Option<String>,
    /// The artist a filter names.
    artist: Option<Id>,
}

impl Plan {
    pub(super) fn of(kind: ListKind, query: &Query) -> Result<Self, ListError> {
        let sorts = query.sort.as_deref().unwrap_or_default();
        let sort = match sorts {
            [] => kind.sorts().first().copied().unwrap_or(SortKey::Title),
            [one] => one.key,
            _ => {
                return Err(ListError::Invalid(
                    "a list takes one order in this version".to_owned(),
                ));
            }
        };
        if !kind.sorts().contains(&sort) {
            return Err(ListError::Invalid(format!(
                "a list of {kind:?} cannot be ordered by {sort:?}"
            )));
        }
        let artist = match &query.filter {
            None => None,
            Some(filter) if filter.version != FILTER_VERSION => {
                return Err(ListError::Unsupported(format!(
                    "this server reads filters of version {FILTER_VERSION}"
                )));
            }
            Some(filter) => {
                if !kind.predicates().contains(&filter.predicate.name()) {
                    return Err(ListError::Invalid(format!(
                        "a list of {kind:?} takes no {:?} filter",
                        filter.predicate.name()
                    )));
                }
                match filter.predicate {
                    Predicate::ByArtist { artist_id } => Some(artist_id),
                }
            }
        };
        let text = query
            .text
            .as_deref()
            .map(fold::needle)
            .filter(|needle| !needle.is_empty());
        Ok(Self {
            kind,
            sort,
            text,
            artist,
        })
    }
}

/// Resolves a plan, inside a read transaction the caller holds.
pub(super) fn resolve(connection: &Connection, plan: &Plan, now_ms: i64) -> Result<View, Error> {
    let revision: i64 =
        connection.query_row("SELECT rev FROM library_meta WHERE id = 1", [], |row| {
            row.get(0)
        })?;
    let (ids, sections) = match plan.kind {
        ListKind::Artists => artists(connection, plan)?,
        ListKind::Albums => albums(connection, plan, now_ms)?,
        ListKind::Songs => songs(connection, plan)?,
    };
    Ok(View {
        kind: plan.kind,
        sort: plan.sort,
        revision,
        ids,
        sections,
    })
}

/// The canonical key of the artist with this public ID, if there is one.
fn artist_key(connection: &Connection, artist: Id) -> Result<Option<i64>, Error> {
    Ok(connection
        .query_row(
            "SELECT canonical_id FROM artist WHERE public_id = ?1",
            [artist.as_bytes()],
            |row| row.get(0),
        )
        .optional()?)
}

/// Sections from a list in order: a new one wherever the label changes.
fn sections<T>(items: &[T], label: impl Fn(&T) -> String) -> Vec<Section> {
    let mut sections: Vec<Section> = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let label = label(item);
        if sections.last().map(|last| &last.label) != Some(&label) {
            sections.push(Section {
                label,
                index: index as u64,
            });
        }
    }
    sections
}

// ---------------------------------------------------------------- artists

struct ArtistEntry {
    id: i64,
    key: String,
}

fn artists(connection: &Connection, plan: &Plan) -> Result<(Vec<i64>, Vec<Section>), Error> {
    // Those who have a record: the artists a release's credit names. A
    // name that only appears in a song's credit ("Drake feat. Rihanna",
    // which is never split) is not filed among them.
    let mut statement = connection.prepare(
        "SELECT a.id, a.display_name, a.display_sort_name
         FROM artist a
         WHERE a.redirect_to_id IS NULL
           AND a.id IN (
             SELECT ca.canonical_id
             FROM release r
             CROSS JOIN artist_credit_name acn ON acn.artist_credit_id = r.display_artist_credit_id
             CROSS JOIN artist ca ON ca.id = acn.artist_id
             WHERE r.redirect_to_id IS NULL
           )",
    )?;
    let mut entries = Vec::new();
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let name: Option<String> = row.get(1)?;
        let sort_name: Option<String> = row.get(2)?;
        let name = name.unwrap_or_default();
        if let Some(needle) = &plan.text
            && !fold::matches(&name, needle)
            && !sort_name
                .as_deref()
                .is_some_and(|sort_name| fold::matches(sort_name, needle))
        {
            continue;
        }
        entries.push(ArtistEntry {
            id: row.get(0)?,
            key: fold::sort_text(&name, sort_name.as_deref()),
        });
    }
    entries.sort_by(|a, b| by_name(&a.key, &b.key).then(a.id.cmp(&b.id)));
    let sections = sections(&entries, |entry| fold::letter(&entry.key).to_string());
    Ok((entries.iter().map(|entry| entry.id).collect(), sections))
}

/// Filed names in order: A to Z, then `#`.
fn by_name(a: &str, b: &str) -> Ordering {
    fold::rank(a).cmp(&fold::rank(b)).then_with(|| a.cmp(b))
}

#[cfg(test)]
pub(super) fn by_name_for_tests(a: &str, b: &str) -> Ordering {
    by_name(a, b)
}

// ---------------------------------------------------------------- albums

/// One live release, with what ordering and filing need.
#[derive(Debug, Clone)]
pub(super) struct Release {
    pub(super) id: i64,
    pub(super) group: i64,
    pub(super) expanded: bool,
    pub(super) display_release: Option<i64>,
    pub(super) first_year: Option<i64>,
    pub(super) year: Option<i64>,
    pub(super) title: String,
    pub(super) credit: String,
    /// Whether the credit names the artist a filter asks for.
    by_artist: bool,
    /// What it is filed under by artist.
    pub(super) artist_key: String,
    /// What it is filed under by title.
    pub(super) title_key: String,
}

/// Live releases with their group and what each is filed under. `?1` is
/// the artist a filter asks for, or null.
const RELEASES: &str = "SELECT r.id, r.release_group_id, rg.presentation, rg.display_release_id,
        rg.display_first_year, r.display_year, r.display_title, ac.display_text,
        (SELECT a.display_sort_name
           FROM artist_credit_name acn JOIN artist a ON a.id = acn.artist_id
          WHERE acn.artist_credit_id = r.display_artist_credit_id
          ORDER BY acn.position LIMIT 1),
        (SELECT count(*) FROM artist_credit_name acn
          WHERE acn.artist_credit_id = r.display_artist_credit_id),
        ?1 IS NULL OR EXISTS (
          SELECT 1 FROM artist_credit_name acn JOIN artist a ON a.id = acn.artist_id
           WHERE acn.artist_credit_id = r.display_artist_credit_id
             AND a.canonical_id = ?1)
 FROM release r
 JOIN release_group rg ON rg.id = r.release_group_id
 LEFT JOIN artist_credit ac ON ac.id = r.display_artist_credit_id
 WHERE r.redirect_to_id IS NULL";

/// Every live release with its group and what it is filed under.
pub(super) fn releases(
    connection: &Connection,
    artist: Option<i64>,
) -> Result<Vec<Release>, Error> {
    let mut statement = connection.prepare(RELEASES)?;
    let mut releases = Vec::new();
    let mut rows = statement.query([artist])?;
    while let Some(row) = rows.next()? {
        releases.push(release(row)?);
    }
    Ok(releases)
}

/// The live releases of the groups that the releases with these keys are
/// in: every version of each, and nothing else.
fn releases_of_groups(connection: &Connection, keys: &[i64]) -> Result<Vec<Release>, Error> {
    let mut groups = Vec::new();
    super::for_each_in(
        connection,
        "SELECT DISTINCT release_group_id FROM release WHERE id IN ({})",
        keys,
        |row| {
            groups.push(row.get(0)?);
            Ok(())
        },
    )?;
    groups.sort_unstable();
    groups.dedup();
    let mut releases = Vec::new();
    for chunk in groups.chunks(super::CHUNK) {
        let sql = format!(
            "{RELEASES} AND r.release_group_id IN ({})",
            super::placeholders(chunk.len())
        );
        let mut statement = connection.prepare(&sql)?;
        // No artist, then the groups.
        let mut parameters: Vec<Option<i64>> = Vec::with_capacity(chunk.len() + 1);
        parameters.push(None);
        parameters.extend(chunk.iter().map(|group| Some(*group)));
        let mut rows = statement.query(rusqlite::params_from_iter(parameters))?;
        while let Some(row) = rows.next()? {
            releases.push(release(row)?);
        }
    }
    Ok(releases)
}

/// One row of [`RELEASES`].
fn release(row: &rusqlite::Row<'_>) -> Result<Release, Error> {
    let presentation: Option<String> = row.get(2)?;
    let title: Option<String> = row.get(6)?;
    let credit: Option<String> = row.get(7)?;
    let first_sort_name: Option<String> = row.get(8)?;
    let names: i64 = row.get(9)?;
    let (title, credit) = (title.unwrap_or_default(), credit.unwrap_or_default());
    // A sort name speaks for one artist. A credit of several is filed as it
    // is written.
    let sort_name = first_sort_name.filter(|_| names == 1);
    Ok(Release {
        id: row.get(0)?,
        group: row.get(1)?,
        expanded: presentation.as_deref() == Some("expanded"),
        display_release: row.get(3)?,
        first_year: row.get(4)?,
        year: row.get(5)?,
        artist_key: fold::sort_text(&credit, sort_name.as_deref()),
        title_key: fold::sort_text(&title, None),
        by_artist: row.get(10)?,
        title,
        credit,
    })
}

/// One tile of the Albums list: its key, and the release it shows.
#[derive(Debug, Clone, Copy)]
pub(super) struct Tile {
    /// A group shown as one tile is its key; a release of a group shown
    /// expanded is its key negated.
    pub(super) key: i64,
    /// The release it shows, by its index in the releases.
    pub(super) shown: usize,
    /// The year it first came out, for a group shown as one tile, or the
    /// release's own.
    pub(super) year: Option<i64>,
}

/// The tiles the releases make: one for each group shown collapsed, which
/// is the library's default, showing the release the group is set to
/// show, and one for each release of a group shown expanded.
pub(super) fn tiles(releases: &[Release]) -> Vec<Tile> {
    let mut groups: HashMap<i64, Vec<usize>> = HashMap::new();
    for (index, release) in releases.iter().enumerate() {
        groups.entry(release.group).or_default().push(index);
    }
    let mut tiles = Vec::with_capacity(groups.len());
    for (group, members) in groups {
        let Some(first) = members.first().and_then(|index| releases.get(*index)) else {
            continue;
        };
        if first.expanded {
            for index in members {
                if let Some(release) = releases.get(index) {
                    tiles.push(Tile {
                        key: -release.id,
                        shown: index,
                        year: release.year,
                    });
                }
            }
            continue;
        }
        let shown = shown_of(releases, &members, first.display_release);
        let year = releases
            .get(shown)
            .and_then(|release| release.first_year.or(release.year));
        tiles.push(Tile {
            key: group,
            shown,
            year,
        });
    }
    tiles
}

/// The release a collapsed group shows: the one the group is set to show
/// if it is live, or else the oldest.
fn shown_of(releases: &[Release], members: &[usize], display: Option<i64>) -> usize {
    let display = display.and_then(|display| {
        members.iter().copied().find(|index| {
            releases
                .get(*index)
                .is_some_and(|release| release.id == display)
        })
    });
    display
        .or_else(|| {
            members
                .iter()
                .copied()
                .min_by_key(|index| releases.get(*index).map(|release| release.id))
        })
        .unwrap_or_default()
}

/// When each release came into the library: the oldest of its tracks.
fn added(connection: &Connection) -> Result<HashMap<i64, i64>, Error> {
    let mut statement = connection.prepare(
        "SELECT release_id, min(display_added_at_ms) FROM track_appearance
         WHERE redirect_to_id IS NULL AND release_id IS NOT NULL
         GROUP BY release_id",
    )?;
    let mut added = HashMap::new();
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        if let Some(at) = row.get::<_, Option<i64>>(1)? {
            added.insert(row.get(0)?, at);
        }
    }
    Ok(added)
}

/// Years newest first, and no year last.
fn newest_first(a: Option<i64>, b: Option<i64>) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) => b.cmp(&a),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

/// Years oldest first, and no year last.
fn oldest_first(a: Option<i64>, b: Option<i64>) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) => a.cmp(&b),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

/// The order of records by artist: the artist as a name is filed, then the
/// year it first came out, oldest first, then the title.
fn by_artist(a: &Release, a_year: Option<i64>, b: &Release, b_year: Option<i64>) -> Ordering {
    by_name(&a.artist_key, &b.artist_key)
        .then_with(|| oldest_first(a_year, b_year))
        .then_with(|| a.title_key.cmp(&b.title_key))
}

fn albums(
    connection: &Connection,
    plan: &Plan,
    now_ms: i64,
) -> Result<(Vec<i64>, Vec<Section>), Error> {
    let artist = match plan.artist {
        Some(artist) => match artist_key(connection, artist)? {
            Some(key) => Some(key),
            // An artist the library does not have has no records.
            None => return Ok((Vec::new(), Vec::new())),
        },
        None => None,
    };
    let releases = releases(connection, artist)?;
    let mut tiles: Vec<Tile> = tiles(&releases)
        .into_iter()
        .filter(|tile| {
            let Some(release) = releases.get(tile.shown) else {
                return false;
            };
            release.by_artist
                && plan.text.as_ref().is_none_or(|needle| {
                    fold::matches(&release.title, needle) || fold::matches(&release.credit, needle)
                })
        })
        .collect();
    let shown = |tile: &Tile| releases.get(tile.shown);
    let added = if plan.sort == SortKey::Added {
        added(connection)?
    } else {
        HashMap::new()
    };
    // A group's tracks came in when its oldest version did.
    let mut group_added: HashMap<i64, i64> = HashMap::new();
    for release in &releases {
        if let Some(at) = added.get(&release.id) {
            let key = if release.expanded {
                -release.id
            } else {
                release.group
            };
            let entry = group_added.entry(key).or_insert(*at);
            *entry = (*entry).min(*at);
        }
    }
    tiles.sort_by(|a, b| {
        let (Some(ra), Some(rb)) = (shown(a), shown(b)) else {
            return a.key.cmp(&b.key);
        };
        let order = match plan.sort {
            SortKey::Title => by_name(&ra.title_key, &rb.title_key)
                .then_with(|| by_name(&ra.artist_key, &rb.artist_key)),
            SortKey::Year => {
                newest_first(a.year, b.year).then_with(|| by_artist(ra, a.year, rb, b.year))
            }
            SortKey::Added => newest_first(
                group_added.get(&a.key).copied(),
                group_added.get(&b.key).copied(),
            )
            .then_with(|| by_artist(ra, a.year, rb, b.year)),
            _ => by_artist(ra, a.year, rb, b.year),
        };
        order.then(a.key.cmp(&b.key))
    });
    let sections = match plan.sort {
        SortKey::Title => sections(&tiles, |tile| {
            shown(tile)
                .map_or('#', |release| fold::letter(&release.title_key))
                .to_string()
        }),
        SortKey::Year => sections(&tiles, |tile| {
            tile.year
                .map_or_else(|| "No year".to_owned(), |year| year.to_string())
        }),
        SortKey::Added => sections(&tiles, |tile| {
            group_added
                .get(&tile.key)
                .map_or_else(|| "Earlier".to_owned(), |at| period(*at, now_ms))
        }),
        _ => sections(&tiles, |tile| {
            shown(tile)
                .map_or('#', |release| fold::letter(&release.artist_key))
                .to_string()
        }),
    };
    Ok((tiles.iter().map(|tile| tile.key).collect(), sections))
}

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// The stretch of time an instant falls in, as a heading puts it, from
/// `now_ms`: this week, the last few weeks, a month of this year, or a
/// year. Months and years are counted in UTC.
pub(super) fn period(at_ms: i64, now_ms: i64) -> String {
    const DAY_MS: i64 = 86_400_000;
    let days = (now_ms - at_ms) / DAY_MS;
    if days < 7 {
        return "This week".to_owned();
    }
    if days < 31 {
        return "The last few weeks".to_owned();
    }
    let (year, month) = year_month(at_ms);
    if year == year_month(now_ms).0 {
        let month = usize::try_from(month - 1).unwrap_or_default();
        return MONTHS.get(month).copied().unwrap_or("Earlier").to_owned();
    }
    year.to_string()
}

/// The UTC year and month (1 to 12) of an instant.
fn year_month(at_ms: i64) -> (i64, i64) {
    // Howard Hinnant's days_from_civil, inverted.
    let days = at_ms.div_euclid(86_400_000) + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month)
}

// ---------------------------------------------------------------- songs

struct Song {
    id: i64,
    release: Option<i64>,
    recording: i64,
    title_key: String,
    disc: Option<i64>,
    track: Option<i64>,
    by_artist: bool,
}

/// Appearances whose title, artist or album matches what was typed, by
/// their keys: through the text index for three characters or more, and
/// for fewer by reading the index's text and matching the start of a word.
fn matching(connection: &Connection, needle: &str) -> Result<HashSet<i64>, Error> {
    let mut found = HashSet::new();
    if needle.chars().count() >= 3 {
        let phrase = format!("\"{}\"", needle.replace('"', "\"\""));
        let mut statement =
            connection.prepare("SELECT rowid FROM search_text WHERE search_text MATCH ?1")?;
        let mut rows = statement.query([phrase])?;
        while let Some(row) = rows.next()? {
            found.insert(row.get(0)?);
        }
    } else {
        let mut statement =
            connection.prepare("SELECT rowid, title, artist, album FROM search_text")?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let mut any = false;
            for column in 1..=3 {
                let text: Option<String> = row.get(column)?;
                if text.is_some_and(|text| fold::matches(&text, needle)) {
                    any = true;
                    break;
                }
            }
            if any {
                found.insert(row.get(0)?);
            }
        }
    }
    Ok(found)
}

/// Above this many matches, every appearance is read and the matches kept,
/// which is quicker than looking each one up; the same for the records the
/// matches are on. On 1.36 million tracks, looking 40,000 songs up by key
/// was about twice as quick as reading every one.
const LOOK_UP_AT_MOST: usize = 200_000;

fn songs(connection: &Connection, plan: &Plan) -> Result<(Vec<i64>, Vec<Section>), Error> {
    let artist = match plan.artist {
        Some(artist) => match artist_key(connection, artist)? {
            Some(key) => Some(key),
            None => return Ok((Vec::new(), Vec::new())),
        },
        None => None,
    };
    let matched = match &plan.text {
        Some(needle) => Some(matching(connection, needle)?),
        None => None,
    };
    let columns = "SELECT ta.id, ta.release_id, rc.canonical_id, ta.display_title,
                          ta.display_disc_number, ta.display_track_number,
                          ?1 IS NULL OR EXISTS (
                            SELECT 1 FROM artist_credit_name acn JOIN artist a ON a.id = acn.artist_id
                             WHERE acn.artist_credit_id = ta.display_artist_credit_id
                               AND a.canonical_id = ?1)
                   FROM track_appearance ta JOIN recording rc ON rc.id = ta.recording_id
                   WHERE ta.redirect_to_id IS NULL";
    let read = |row: &rusqlite::Row<'_>| -> Result<Song, rusqlite::Error> {
        let title: Option<String> = row.get(3)?;
        Ok(Song {
            id: row.get(0)?,
            release: row.get(1)?,
            recording: row.get(2)?,
            title_key: fold::sort_text(title.as_deref().unwrap_or_default(), None),
            disc: row.get(4)?,
            track: row.get(5)?,
            by_artist: row.get(6)?,
        })
    };
    let mut songs = Vec::new();
    match &matched {
        Some(matched) if matched.len() <= LOOK_UP_AT_MOST => {
            let keys: Vec<i64> = matched.iter().copied().collect();
            // The artist goes in as the first parameter of each chunk.
            for chunk in keys.chunks(super::CHUNK) {
                let sql = format!(
                    "{columns} AND ta.id IN ({})",
                    super::placeholders(chunk.len())
                );
                let mut statement = connection.prepare(&sql)?;
                let mut parameters: Vec<Option<i64>> = Vec::with_capacity(chunk.len() + 1);
                parameters.push(artist);
                parameters.extend(chunk.iter().map(|key| Some(*key)));
                let mut rows = statement.query(rusqlite::params_from_iter(parameters))?;
                while let Some(row) = rows.next()? {
                    songs.push(read(row)?);
                }
            }
        }
        _ => {
            let mut statement = connection.prepare(columns)?;
            let mut rows = statement.query([artist])?;
            while let Some(row) = rows.next()? {
                // A row that does not match is passed over before its title
                // is folded.
                let id: i64 = row.get(0)?;
                if matched.as_ref().is_none_or(|matched| matched.contains(&id)) {
                    songs.push(read(row)?);
                }
            }
        }
    }
    songs.retain(|song| song.by_artist);

    // In a group shown as one tile, a song on another version is hidden
    // where the version the group shows has the same recording. That needs
    // every version of the groups the songs are on: for a search or a
    // filter, only those groups are read.
    let mut on: Vec<i64> = songs.iter().filter_map(|song| song.release).collect();
    on.sort_unstable();
    on.dedup();
    let releases = if (matched.is_some() || artist.is_some()) && on.len() <= LOOK_UP_AT_MOST {
        releases_of_groups(connection, &on)?
    } else {
        releases(connection, None)?
    };
    let tiles = tiles(&releases);
    let mut shown_by_group: HashMap<i64, i64> = HashMap::new();
    for tile in &tiles {
        if tile.key > 0
            && let Some(release) = releases.get(tile.shown)
        {
            shown_by_group.insert(tile.key, release.id);
        }
    }
    let release_index: HashMap<i64, usize> = releases
        .iter()
        .enumerate()
        .map(|(index, release)| (release.id, index))
        .collect();
    let mut on_shown: HashSet<(i64, i64)> = HashSet::new();
    for song in &songs {
        if let Some(release) = song.release.and_then(|id| release_index.get(&id))
            && let Some(release) = releases.get(*release)
            && shown_by_group.get(&release.group) == Some(&release.id)
        {
            on_shown.insert((release.group, song.recording));
        }
    }
    // A hidden song is only hidden if the shown version's copy of it is in
    // the list: with a filter or a search it may not be, and then this one
    // stands in for it.
    songs.retain(|song| {
        let Some(release) = song
            .release
            .and_then(|id| release_index.get(&id))
            .and_then(|index| releases.get(*index))
        else {
            return true;
        };
        if release.expanded || shown_by_group.get(&release.group) == Some(&release.id) {
            return true;
        }
        !on_shown.contains(&(release.group, song.recording))
    });

    let album_of = |song: &Song| {
        song.release
            .and_then(|id| release_index.get(&id))
            .and_then(|index| releases.get(*index))
    };
    let year_of = |release: &Release| release.first_year.or(release.year);
    match plan.sort {
        SortKey::Album => songs.sort_by(|a, b| {
            let order = match (album_of(a), album_of(b)) {
                (Some(ra), Some(rb)) => by_artist(ra, year_of(ra), rb, year_of(rb))
                    .then(ra.id.cmp(&rb.id))
                    .then_with(|| oldest_first(a.disc, b.disc))
                    .then_with(|| oldest_first(a.track, b.track)),
                // A song on no record comes after every record.
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => by_name(&a.title_key, &b.title_key),
            };
            order.then(a.id.cmp(&b.id))
        }),
        _ => songs.sort_by(|a, b| by_name(&a.title_key, &b.title_key).then(a.id.cmp(&b.id))),
    }
    let sections = match plan.sort {
        SortKey::Album => sections(&songs, |song| {
            album_of(song)
                .map_or('#', |release| fold::letter(&release.artist_key))
                .to_string()
        }),
        _ => sections(&songs, |song| fold::letter(&song.title_key).to_string()),
    };
    Ok((songs.iter().map(|song| song.id).collect(), sections))
}

// ---------------------------------------------------------------- locating

/// The key a row is held by in a list of `kind`, from its public ID.
pub(super) fn key_of(
    connection: &Connection,
    kind: ListKind,
    id: Id,
) -> Result<Option<i64>, Error> {
    let bytes = id.as_bytes();
    let found = match kind {
        ListKind::Artists => connection
            .query_row(
                "SELECT canonical_id FROM artist WHERE public_id = ?1",
                [bytes],
                |row| row.get(0),
            )
            .optional()?,
        ListKind::Albums => {
            let group: Option<i64> = connection
                .query_row(
                    "SELECT id FROM release_group WHERE public_id = ?1",
                    [bytes],
                    |row| row.get(0),
                )
                .optional()?;
            match group {
                Some(group) => Some(group),
                None => connection
                    .query_row(
                        "SELECT -id FROM release WHERE public_id = ?1",
                        [bytes],
                        |row| row.get(0),
                    )
                    .optional()?,
            }
        }
        ListKind::Songs => connection
            .query_row(
                "SELECT id FROM track_appearance WHERE public_id = ?1",
                [bytes],
                |row| row.get(0),
            )
            .optional()?,
    };
    Ok(found)
}

/// What one row of a view is filed under in its order, for type-to-find.
fn filed_under(
    connection: &Connection,
    resolved: &View,
    key: i64,
) -> Result<Option<String>, Error> {
    let sql = match (resolved.kind, resolved.sort) {
        (ListKind::Artists, _) => {
            return Ok(connection
                .query_row(
                    "SELECT display_name, display_sort_name FROM artist WHERE id = ?1",
                    [key],
                    |row| {
                        let name: Option<String> = row.get(0)?;
                        let sort: Option<String> = row.get(1)?;
                        Ok(fold::sort_text(
                            name.as_deref().unwrap_or_default(),
                            sort.as_deref(),
                        ))
                    },
                )
                .optional()?);
        }
        (ListKind::Songs, SortKey::Title) => {
            return Ok(connection
                .query_row(
                    "SELECT display_title FROM track_appearance WHERE id = ?1",
                    [key],
                    |row| {
                        let title: Option<String> = row.get(0)?;
                        Ok(fold::sort_text(title.as_deref().unwrap_or_default(), None))
                    },
                )
                .optional()?);
        }
        (ListKind::Songs, _) => "SELECT release_id FROM track_appearance WHERE id = ?1",
        (ListKind::Albums, _) if key < 0 => "SELECT ?1 * -1",
        (ListKind::Albums, _) => {
            "SELECT coalesce(
               (SELECT r.id FROM release r JOIN release_group rg ON rg.id = r.release_group_id
                 WHERE rg.id = ?1 AND r.id = rg.display_release_id AND r.redirect_to_id IS NULL),
               (SELECT min(r.id) FROM release r WHERE r.release_group_id = ?1 AND r.redirect_to_id IS NULL))"
        }
    };
    let release: Option<i64> = connection
        .query_row(sql, [key], |row| row.get(0))
        .optional()?
        .flatten();
    let Some(release) = release else {
        return Ok(None);
    };
    Ok(connection
        .query_row(
            "SELECT r.display_title, ac.display_text,
                    (SELECT a.display_sort_name
                       FROM artist_credit_name acn JOIN artist a ON a.id = acn.artist_id
                      WHERE acn.artist_credit_id = r.display_artist_credit_id
                      ORDER BY acn.position LIMIT 1),
                    (SELECT count(*) FROM artist_credit_name acn
                      WHERE acn.artist_credit_id = r.display_artist_credit_id)
             FROM release r LEFT JOIN artist_credit ac ON ac.id = r.display_artist_credit_id
             WHERE r.id = ?1",
            [release],
            |row| {
                let title: Option<String> = row.get(0)?;
                let credit: Option<String> = row.get(1)?;
                let sort_name: Option<String> = row.get(2)?;
                let names: i64 = row.get(3)?;
                Ok(if resolved.sort == SortKey::Title {
                    fold::sort_text(title.as_deref().unwrap_or_default(), None)
                } else {
                    fold::sort_text(
                        credit.as_deref().unwrap_or_default(),
                        sort_name.filter(|_| names == 1).as_deref(),
                    )
                })
            },
        )
        .optional()?)
}

/// The first row filed at or after `prefix`, found by halving.
pub(super) fn locate_prefix(
    connection: &Connection,
    resolved: &View,
    prefix: &str,
) -> Result<Option<u64>, Error> {
    if resolved.sort == SortKey::Year {
        let typed = prefix.trim();
        return Ok(resolved
            .sections
            .iter()
            .find(|section| section.label.starts_with(typed))
            .map(|section| section.index));
    }
    let target = fold::sort_text(prefix, None);
    if target.is_empty() {
        return Ok(None);
    }
    let (mut low, mut high) = (0, resolved.ids.len());
    while low < high {
        let middle = low + (high - low) / 2;
        let Some(key) = resolved.ids.get(middle) else {
            break;
        };
        // A row that has gone since sorts nowhere: step past it.
        match filed_under(connection, resolved, *key)? {
            Some(filed) if by_name(&filed, &target) == Ordering::Less => low = middle + 1,
            Some(_) => high = middle,
            None => low = middle + 1,
        }
    }
    Ok((low < resolved.ids.len()).then_some(low as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_period_is_this_week_the_last_few_weeks_a_month_or_a_year() {
        // 15 November 2026, at noon UTC.
        let now = 1_794_744_000_000;
        let day = 86_400_000;
        assert_eq!(period(now - 2 * day, now), "This week");
        assert_eq!(period(now - 20 * day, now), "The last few weeks");
        assert_eq!(period(now - 60 * day, now), "September");
        assert_eq!(period(now - 400 * day, now), "2025");
        assert_eq!(year_month(0), (1970, 1));
        assert_eq!(year_month(951_782_400_000), (2000, 2));
        assert_eq!(year_month(951_868_800_000), (2000, 3));
    }
}
