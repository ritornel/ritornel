//! Resolving a query into the rows of a view, in order, with its sections.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use ritornel_contract::{
    FILTER_VERSION, GroupBy, Id, ListKind, MAX_RATING, Predicate, Query, Section, Shelf, SortKey,
};
use rusqlite::{Connection, OptionalExtension};

use super::{Find, ListError, View, discography, fold, rows};
use crate::Error;

/// A query, checked against what its kind of list takes.
#[derive(Debug, Clone)]
pub(super) struct Plan {
    kind: ListKind,
    sort: SortKey,
    /// The grouping the query names. `None` when it names none, and the
    /// list takes the sections its order has.
    group: Option<GroupBy>,
    /// What was typed, folded. `None` when nothing was.
    text: Option<String>,
    /// What its filter asks of a row.
    conditions: Conditions,
}

/// A range a number must be in, either end of which may be open.
#[derive(Debug, Clone, Copy, Default)]
struct Bounds<T> {
    from: Option<T>,
    to: Option<T>,
}

impl<T: Ord + Copy> Bounds<T> {
    /// Narrows the range to what is also from `from` to `to`.
    fn narrow(&mut self, from: Option<T>, to: Option<T>) {
        // `None` is below every `Some`, so the higher lower bound stands.
        self.from = self.from.max(from);
        self.to = match (self.to, to) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
    }

    fn is_open(&self) -> bool {
        self.from.is_none() && self.to.is_none()
    }

    fn holds(&self, value: T) -> bool {
        self.from.is_none_or(|from| value >= from) && self.to.is_none_or(|to| value <= to)
    }

    /// As [`Bounds::holds`], of a number a row may not have: one without
    /// it is in no range but the open one.
    fn holds_known(&self, value: Option<T>) -> bool {
        self.is_open() || value.is_some_and(|value| self.holds(value))
    }
}

/// What a filter's conditions come to, gathered from its tree.
#[derive(Debug, Clone, Default)]
struct Conditions {
    /// The artist it names.
    artist: Option<Id>,
    /// Whether the records of others that the artist is credited on a song
    /// of are in it too ([`Predicate::WithArtist`]).
    appears: bool,
    /// Whether it is only songs on no record ([`Predicate::NoRecord`]).
    no_record: bool,
    /// The fewest tracks a record has ([`Predicate::MinTracks`]).
    min_tracks: Option<u64>,
    /// How many times a row has been played ([`Predicate::PlayCount`]).
    plays: Bounds<u64>,
    /// The lowest rating a song has ([`Predicate::MinRating`]).
    min_rating: Option<u32>,
    /// Whether a row's best copy is lossless ([`Predicate::Lossless`]).
    lossless: Option<bool>,
    /// The years a row is from ([`Predicate::YearRange`]).
    years: Bounds<i64>,
}

impl Conditions {
    fn gather(&mut self, kind: ListKind, predicate: &Predicate) -> Result<(), ListError> {
        if !kind.predicates().contains(&predicate.name()) {
            return Err(ListError::Invalid(format!(
                "a list of {kind:?} takes no {:?} filter",
                predicate.name()
            )));
        }
        let mut name = |artist: Id, appears: bool| {
            if self.artist.is_some() {
                return Err(ListError::Invalid(
                    "a filter names at most one artist".to_owned(),
                ));
            }
            self.artist = Some(artist);
            self.appears = appears;
            Ok(())
        };
        match predicate {
            Predicate::ByArtist { artist_id } => name(*artist_id, false)?,
            Predicate::WithArtist { artist_id } => name(*artist_id, true)?,
            Predicate::NoRecord => self.no_record = true,
            Predicate::MinTracks { count } => {
                self.min_tracks = self.min_tracks.max(Some(*count));
            }
            Predicate::PlayCount { min, max } => self.plays.narrow(*min, *max),
            Predicate::MinRating { rating } => {
                if *rating > MAX_RATING {
                    return Err(ListError::Invalid(format!("a rating is 0 to {MAX_RATING}")));
                }
                self.min_rating = self.min_rating.max(Some(*rating));
            }
            Predicate::Lossless { lossless } => {
                if self.lossless.is_some_and(|said| said != *lossless) {
                    return Err(ListError::Invalid(
                        "a filter says once whether a copy is lossless".to_owned(),
                    ));
                }
                self.lossless = Some(*lossless);
            }
            Predicate::YearRange { from, to } => self.years.narrow(*from, *to),
            Predicate::All { of } => {
                for each in of {
                    self.gather(kind, each)?;
                }
            }
        }
        Ok(())
    }
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
        let mut conditions = Conditions::default();
        match &query.filter {
            None => {}
            Some(filter) if filter.version != FILTER_VERSION => {
                return Err(ListError::Unsupported(format!(
                    "this server reads filters of version {FILTER_VERSION}"
                )));
            }
            Some(filter) => conditions.gather(kind, &filter.predicate)?,
        }
        if sort == SortKey::Discography && conditions.artist.is_none() {
            return Err(ListError::Invalid(
                "a discography is of one artist: its filter names them".to_owned(),
            ));
        }
        if let Some(group) = query.group_by
            && !kind.groupings().contains(&group)
        {
            return Err(ListError::Invalid(format!(
                "a list of {kind:?} cannot be grouped by {group:?}"
            )));
        }
        let text = query
            .text
            .as_deref()
            .map(fold::needle)
            .filter(|needle| !needle.is_empty());
        Ok(Self {
            kind,
            sort,
            group: query.group_by,
            text,
            conditions,
        })
    }

    /// Whether plays decide which rows it has or where they come.
    fn by_plays(&self) -> bool {
        matches!(self.sort, SortKey::Plays | SortKey::LastPlayed)
            || !self.conditions.plays.is_open()
    }

    /// Whether ratings decide which rows it has or where they come.
    fn by_ratings(&self) -> bool {
        self.sort == SortKey::Rating || self.conditions.min_rating.is_some()
    }

    /// How a prefix typed is found in what it resolves to, with these
    /// sections: see [`Find`].
    fn find(&self, sections: &[Section]) -> Find {
        let by_name = matches!(
            self.sort,
            SortKey::Name | SortKey::Artist | SortKey::Title | SortKey::Album
        );
        // Letters cut an order by name where it changes letter, so the
        // order still runs from end to end.
        let unbroken = matches!(self.group, None | Some(GroupBy::None | GroupBy::Letter));
        if by_name && unbroken {
            Find::Name
        } else if sections.is_empty() {
            Find::Nothing
        } else {
            Find::Sections { names: by_name }
        }
    }
}

/// Resolves a plan, inside a read transaction the caller holds.
pub(super) fn resolve(connection: &Connection, plan: &Plan, now_ms: i64) -> Result<View, Error> {
    let revision: i64 =
        connection.query_row("SELECT rev FROM library_meta WHERE id = 1", [], |row| {
            row.get(0)
        })?;
    let (ids, sections) = match plan.kind {
        ListKind::Artists => artists(connection, plan, now_ms)?,
        ListKind::Albums => albums(connection, plan, now_ms)?,
        ListKind::Songs => songs(connection, plan, now_ms)?,
    };
    Ok(View {
        kind: plan.kind,
        sort: plan.sort,
        revision,
        ids,
        find: plan.find(&sections),
        by_plays: plan.by_plays(),
        by_ratings: plan.by_ratings(),
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
                shelf: None,
            });
        }
    }
    sections
}

// ---------------------------------------------------------------- groups

/// The group a row is in under a grouping a query names.
struct Group {
    /// Where the group comes among the others, lowest first.
    rank: i64,
    /// Its heading.
    label: String,
    /// The kind of record it holds, under [`GroupBy::Shelf`].
    shelf: Option<Shelf>,
}

impl Group {
    fn new(rank: i64, label: impl Into<String>) -> Self {
        Self {
            rank,
            label: label.into(),
            shelf: None,
        }
    }

    /// A to Z, then `#`.
    fn letter(letter: char) -> Self {
        let rank = match letter {
            'A'..='Z' => i64::from(u32::from(letter) - u32::from('A')),
            _ => 26,
        };
        Self::new(rank, letter)
    }

    /// Newest first, and no year last.
    fn year(year: Option<i64>) -> Self {
        match year {
            Some(year) => Self::new(-year, year.to_string()),
            None => Self::new(i64::MAX, "No year"),
        }
    }

    /// Newest first, and no year last.
    fn decade(year: Option<i64>) -> Self {
        match year {
            Some(year) => {
                let decade = year.div_euclid(10) * 10;
                Self::new(-decade, format!("{decade}s"))
            }
            None => Self::new(i64::MAX, "Unknown"),
        }
    }

    /// The stretches of time of [`period`], newest first, and what has no
    /// time under `never`, last.
    fn when(at_ms: Option<i64>, now_ms: i64, never: &str) -> Self {
        let Some(at_ms) = at_ms else {
            return Self::new(i64::MAX, never);
        };
        let label = period(at_ms, now_ms);
        let (year, month) = year_month(at_ms);
        let rank = match label.as_str() {
            "This week" => 0,
            "The last few weeks" => 1,
            // A month of this year, December first.
            _ if year == year_month(now_ms).0 => 2 + (12 - month),
            // A year, after every month of this one.
            _ => 14 + (year_month(now_ms).0 - year),
        };
        Self::new(rank, label)
    }

    fn quality(tier: Tier) -> Self {
        Self::new(tier as i64, tier.label())
    }

    fn shelf(shelf: Shelf) -> Self {
        Self {
            rank: shelf as i64,
            label: shelf.label().to_owned(),
            shelf: Some(shelf),
        }
    }
}

/// `items`, which are in order, moved into their groups, each keeping its
/// order within its group, and where each group begins.
fn grouped<T>(items: Vec<T>, group: impl Fn(&T) -> Group) -> (Vec<T>, Vec<Section>) {
    let mut keyed: Vec<(Group, T)> = items.into_iter().map(|item| (group(&item), item)).collect();
    // A stable sort, so the order within a group is the one they came in.
    keyed.sort_by_key(|(group, _)| group.rank);
    let mut sections: Vec<Section> = Vec::new();
    let mut last = None;
    let mut items = Vec::with_capacity(keyed.len());
    for (index, (group, item)) in keyed.into_iter().enumerate() {
        if last != Some(group.rank) {
            last = Some(group.rank);
            sections.push(Section {
                label: group.label,
                index: index as u64,
                shelf: group.shelf,
            });
        }
        items.push(item);
    }
    (items, sections)
}

/// What an order by when a row was last played puts a row under: the
/// stretches of time of [`period`], and what has no dated play last.
const NEVER_PLAYED: &str = "Never played";

// ---------------------------------------------------------------- plays

/// Counted plays: how many, and when the last dated one was.
#[derive(Debug, Clone, Copy, Default)]
struct Played {
    count: u64,
    last: Option<i64>,
}

impl Played {
    fn add(&mut self, other: Self) {
        self.count += other.count;
        self.last = self.last.max(other.last);
    }
}

/// The counted plays of each recording that has any, by its canonical
/// key, through the counted-plays index ("Queries" in
/// docs/internals/history.md), as a song's row counts them.
fn plays_by_recording(connection: &Connection) -> Result<HashMap<i64, Played>, Error> {
    let mut statement = connection.prepare(
        "SELECT recording_id, count(*), max(event_at_utc_ms) FROM play_event
         WHERE listener_id = (SELECT min(id) FROM listener) AND counts = 1 AND excluded = 0
           AND recording_id IS NOT NULL
         GROUP BY recording_id",
    )?;
    let mut plays = HashMap::new();
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let count: i64 = row.get(1)?;
        plays.insert(
            row.get(0)?,
            Played {
                count: u64::try_from(count).unwrap_or(0),
                last: row.get(2)?,
            },
        );
    }
    Ok(plays)
}

/// The plays of what `sql` pairs each recording with: it gives a key and a
/// recording's canonical key a row, each pair once.
fn plays_of(
    connection: &Connection,
    sql: &str,
    plays: &HashMap<i64, Played>,
) -> Result<HashMap<i64, Played>, Error> {
    let mut of = HashMap::new();
    // With no plays at all, nothing has any.
    if plays.is_empty() {
        return Ok(of);
    }
    let mut statement = connection.prepare(sql)?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        if let Some(played) = plays.get(&row.get::<_, i64>(1)?) {
            of.entry(row.get(0)?)
                .or_insert_with(Played::default)
                .add(*played);
        }
    }
    Ok(of)
}

/// The plays of each release: those of the recordings on it.
fn plays_by_release(
    connection: &Connection,
    plays: &HashMap<i64, Played>,
) -> Result<HashMap<i64, Played>, Error> {
    plays_of(
        connection,
        concat!(
            "SELECT DISTINCT ta.release_id, rc.canonical_id
             FROM track_appearance ta JOIN recording rc ON rc.id = ta.recording_id
             WHERE ta.redirect_to_id IS NULL AND ta.release_id IS NOT NULL AND ",
            reached!("ta")
        ),
        plays,
    )
}

/// The plays of each artist: those of the recordings of the songs whose
/// credit names them.
fn plays_by_artist(
    connection: &Connection,
    plays: &HashMap<i64, Played>,
) -> Result<HashMap<i64, Played>, Error> {
    plays_of(
        connection,
        concat!(
            "SELECT DISTINCT a.canonical_id, rc.canonical_id
             FROM track_appearance ta
             JOIN recording rc ON rc.id = ta.recording_id
             JOIN artist_credit_name acn ON acn.artist_credit_id = ta.display_artist_credit_id
             JOIN artist a ON a.id = acn.artist_id
             WHERE ta.redirect_to_id IS NULL AND ",
            reached!("ta")
        ),
        plays,
    )
}

// ---------------------------------------------------------------- quality

/// How good a row's best copy is, best first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tier {
    HiRes,
    Lossless,
    Lossy,
    Unknown,
}

impl Tier {
    /// The tier of a row's best copy, or of a row with none the library
    /// has read.
    fn of(best: Option<rows::Grade>) -> Self {
        match best {
            Some((true, bits, rate, _)) if bits > 16 || rate > 48_000 => Self::HiRes,
            Some((true, ..)) => Self::Lossless,
            Some((false, ..)) => Self::Lossy,
            None => Self::Unknown,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::HiRes => "Hi-Res Lossless",
            Self::Lossless => "Lossless",
            Self::Lossy => "Lossy",
            Self::Unknown => "Unknown",
        }
    }
}

/// The best copy the library has read of each song, by its key, or with
/// `of_releases` of any track of each release, by the release's: as a row
/// says its copies, files that are missing left out.
fn best_copies(
    connection: &Connection,
    of_releases: bool,
) -> Result<HashMap<i64, rows::Grade>, Error> {
    let mut statement = connection.prepare(
        "SELECT ta.id, ta.release_id, aa.lossless, aa.bit_depth, aa.sample_rate_hz,
                aa.bitrate_kbps
         FROM assignment asg
         JOIN source_item si ON si.id = asg.source_item_id
         JOIN source_item_revision sr ON sr.id = si.current_revision_id
         JOIN audio_asset aa ON aa.id = sr.audio_asset_id
         JOIN track_appearance ta ON ta.id = asg.appearance_id
         WHERE asg.superseded_at_ms IS NULL AND si.item_kind = 'file'
           AND si.retired_at_ms IS NULL AND si.missing_since_ms IS NULL
           AND ta.redirect_to_id IS NULL",
    )?;
    let mut best: HashMap<i64, rows::Grade> = HashMap::new();
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let key: Option<i64> = row.get(usize::from(of_releases))?;
        let Some(key) = key else { continue };
        let bits: Option<u32> = row.get(3)?;
        let bitrate: Option<u32> = row.get(5)?;
        let grade = (
            row.get(2)?,
            bits.unwrap_or(0),
            row.get(4)?,
            bitrate.unwrap_or(0),
        );
        let held = best.entry(key).or_insert(grade);
        *held = (*held).max(grade);
    }
    Ok(best)
}

// ---------------------------------------------------------------- artists

struct ArtistEntry {
    id: i64,
    key: String,
}

fn artists(
    connection: &Connection,
    plan: &Plan,
    now_ms: i64,
) -> Result<(Vec<i64>, Vec<Section>), Error> {
    // Those who have a record: the artists a release's credit names. A
    // name that only appears in a song's credit ("Drake feat. Rihanna",
    // which is never split) is not filed among them.
    let mut statement = connection.prepare(concat!(
        "SELECT a.id, a.display_name, a.display_sort_name
         FROM artist a
         WHERE a.redirect_to_id IS NULL
           AND a.id IN (
             SELECT ca.canonical_id
             FROM release r
             CROSS JOIN artist_credit_name acn ON acn.artist_credit_id = r.display_artist_credit_id
             CROSS JOIN artist ca ON ca.id = acn.artist_id
             WHERE r.redirect_to_id IS NULL AND ",
        reached_release!("r"),
        ")"
    ))?;
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
    let plays = if plan.by_plays() {
        plays_by_artist(connection, &plays_by_recording(connection)?)?
    } else {
        HashMap::new()
    };
    let played = |entry: &ArtistEntry| plays.get(&entry.id).copied().unwrap_or_default();
    entries.retain(|entry| plan.conditions.plays.holds(played(entry).count));
    entries.sort_by(|a, b| {
        let order = match plan.sort {
            SortKey::Plays => played(b).count.cmp(&played(a).count),
            SortKey::LastPlayed => most_first(played(a).last, played(b).last),
            _ => Ordering::Equal,
        };
        order
            .then_with(|| by_name(&a.key, &b.key))
            .then(a.id.cmp(&b.id))
    });
    let (entries, sections) = match plan.group {
        Some(GroupBy::None) => (entries, Vec::new()),
        Some(_) => grouped(entries, |entry| Group::letter(fold::letter(&entry.key))),
        None => {
            let sections = match plan.sort {
                SortKey::Plays => Vec::new(),
                SortKey::LastPlayed => sections(&entries, |entry| {
                    Group::when(played(entry).last, now_ms, NEVER_PLAYED).label
                }),
                _ => sections(&entries, |entry| fold::letter(&entry.key).to_string()),
            };
            (entries, sections)
        }
    };
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

/// Filed names in order, for the discography's order of titles.
pub(super) fn by_name_for_order(a: &str, b: &str) -> Ordering {
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
    /// Its group's primary type, as stored.
    pub(super) primary: Option<String>,
    /// Its group's marks, as stored: a JSON array.
    pub(super) marks: Option<String>,
}

/// Live releases a file reaches, with their group and what each is filed
/// under. `?1` is the artist a filter asks for, or null.
const RELEASES: &str = concat!(
    "SELECT r.id, r.release_group_id, rg.presentation, rg.display_release_id,
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
             AND a.canonical_id = ?1),
        rg.display_primary_type, rg.display_secondary_types_json
 FROM release r
 JOIN release_group rg ON rg.id = r.release_group_id
 LEFT JOIN artist_credit ac ON ac.id = r.display_artist_credit_id
 WHERE r.redirect_to_id IS NULL AND ",
    reached_release!("r")
);

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
pub(super) fn releases_of_groups(
    connection: &Connection,
    keys: &[i64],
) -> Result<Vec<Release>, Error> {
    releases_of_groups_by(connection, keys, None)
}

/// As [`releases_of_groups`], each saying whether its credit names the
/// artist with the key `artist`.
fn releases_of_groups_by(
    connection: &Connection,
    keys: &[i64],
    artist: Option<i64>,
) -> Result<Vec<Release>, Error> {
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
        // The artist, if any, then the groups.
        let mut parameters: Vec<Option<i64>> = Vec::with_capacity(chunk.len() + 1);
        parameters.push(artist);
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
        primary: row.get(11)?,
        marks: row.get(12)?,
        title,
        credit,
    })
}

/// The live releases whose own credit names the artist with this key: the
/// credits that name them, from `artist_credit_name` (a small table, read
/// whole), and the releases of those credits by
/// `release_display_artist_credit`.
fn releases_by(connection: &Connection, artist: i64) -> Result<Vec<i64>, Error> {
    let mut statement = connection.prepare(concat!(
        "SELECT r.id FROM release r
         WHERE r.redirect_to_id IS NULL
           AND r.display_artist_credit_id IN (
             SELECT acn.artist_credit_id
             FROM artist_credit_name acn JOIN artist a ON a.id = acn.artist_id
             WHERE a.canonical_id = ?1)
           AND ",
        reached_release!("r")
    ))?;
    let mut found = Vec::new();
    let mut rows = statement.query([artist])?;
    while let Some(row) = rows.next()? {
        found.push(row.get(0)?);
    }
    Ok(found)
}

/// The live releases that have a song whose credit names the artist with
/// this key. There is no index on a song's credit, so this reads every
/// song: measured at 100,000 records in the report of the pages' work.
fn releases_crediting(connection: &Connection, artist: i64) -> Result<HashSet<i64>, Error> {
    let mut statement = connection.prepare(concat!(
        "SELECT DISTINCT ta.release_id FROM track_appearance ta
         WHERE ta.redirect_to_id IS NULL AND ta.release_id IS NOT NULL
           AND ta.display_artist_credit_id IN (
             SELECT acn.artist_credit_id
             FROM artist_credit_name acn JOIN artist a ON a.id = acn.artist_id
             WHERE a.canonical_id = ?1)
           AND ",
        reached!("ta")
    ))?;
    let mut found = HashSet::new();
    let mut rows = statement.query([artist])?;
    while let Some(row) = rows.next()? {
        found.insert(row.get(0)?);
    }
    Ok(found)
}

/// The shelf a record goes on in the discography of the artist a list is
/// filtered by: see [`discography::shelf`].
pub(super) fn shelf_of(release: &Release) -> Shelf {
    discography::shelf(
        discography::record_type(release.primary.as_deref()),
        &discography::marks(release.marks.as_deref()),
        release.by_artist,
    )
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
    let mut statement = connection.prepare(concat!(
        "SELECT release_id, min(display_added_at_ms) FROM track_appearance
         WHERE redirect_to_id IS NULL AND release_id IS NOT NULL AND ",
        reached!("track_appearance"),
        " GROUP BY release_id"
    ))?;
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
    most_first(a, b)
}

/// The most first, and what has none last: years, times and ratings.
fn most_first(a: Option<i64>, b: Option<i64>) -> Ordering {
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
    let conditions = &plan.conditions;
    let artist = match conditions.artist {
        Some(artist) => match artist_key(connection, artist)? {
            Some(key) => Some(key),
            // An artist the library does not have has no records.
            None => return Ok((Vec::new(), Vec::new())),
        },
        None => None,
    };
    // An artist's records are found by their credits, and then every
    // version of each, rather than by reading every release there is;
    // with the records of others they are credited on a song of, the
    // groups of the releases that credit them so.
    let mut appears_in: HashSet<i64> = HashSet::new();
    let releases = match artist {
        Some(artist) => {
            let mut keys = releases_by(connection, artist)?;
            let crediting = if conditions.appears {
                releases_crediting(connection, artist)?
            } else {
                HashSet::new()
            };
            keys.extend(crediting.iter().copied());
            let releases = releases_of_groups_by(connection, &keys, Some(artist))?;
            for release in &releases {
                if crediting.contains(&release.id) {
                    appears_in.insert(release.group);
                }
            }
            releases
        }
        None => releases(connection, None)?,
    };
    let mut tiles: Vec<Tile> = tiles(&releases)
        .into_iter()
        .filter(|tile| {
            let Some(release) = releases.get(tile.shown) else {
                return false;
            };
            (release.by_artist || appears_in.contains(&release.group))
                && plan.text.as_ref().is_none_or(|needle| {
                    fold::matches(&release.title, needle) || fold::matches(&release.credit, needle)
                })
        })
        .collect();
    let shown = |tile: &Tile| releases.get(tile.shown);
    // What only some queries ask of a record is read only for them: how
    // many tracks the release it shows has, its plays, and its best copy.
    let tracks = if conditions.min_tracks.is_some() {
        let shown: Vec<i64> = tiles
            .iter()
            .filter_map(|tile| Some(shown(tile)?.id))
            .collect();
        rows::track_counts(connection, &shown)?
    } else {
        HashMap::new()
    };
    let plays = if plan.by_plays() {
        plays_by_release(connection, &plays_by_recording(connection)?)?
    } else {
        HashMap::new()
    };
    let played = |tile: &Tile| {
        shown(tile)
            .and_then(|release| plays.get(&release.id).copied())
            .unwrap_or_default()
    };
    let copies = if conditions.lossless.is_some() || plan.group == Some(GroupBy::Quality) {
        best_copies(connection, true)?
    } else {
        HashMap::new()
    };
    let best = |tile: &Tile| shown(tile).and_then(|release| copies.get(&release.id).copied());
    tiles.retain(|tile| {
        let Some(release) = shown(tile) else {
            return false;
        };
        conditions
            .min_tracks
            .is_none_or(|fewest| tracks.get(&release.id).map_or(0, |(count, _)| *count) >= fewest)
            && conditions.plays.holds(played(tile).count)
            && conditions
                .lossless
                .is_none_or(|lossless| best(tile).is_some_and(|best| best.0 == lossless))
            && conditions.years.holds_known(tile.year)
    });
    let added = if plan.sort == SortKey::Added || plan.group == Some(GroupBy::Added) {
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
    // The shelf of each record, for a discography.
    let shelves: HashMap<i64, Shelf> = if plan.sort == SortKey::Discography {
        tiles
            .iter()
            .filter_map(|tile| Some((tile.key, shelf_of(shown(tile)?))))
            .collect()
    } else {
        HashMap::new()
    };
    let shelf = |tile: &Tile| shelves.get(&tile.key).copied().unwrap_or(Shelf::Albums);
    tiles.sort_by(|a, b| {
        let (Some(ra), Some(rb)) = (shown(a), shown(b)) else {
            return a.key.cmp(&b.key);
        };
        let order = match plan.sort {
            SortKey::Discography => shelf(a).cmp(&shelf(b)).then_with(|| {
                discography::newest_first(a.year, &ra.title_key, b.year, &rb.title_key)
            }),
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
            SortKey::Plays => played(b)
                .count
                .cmp(&played(a).count)
                .then_with(|| by_artist(ra, a.year, rb, b.year)),
            SortKey::LastPlayed => most_first(played(a).last, played(b).last)
                .then_with(|| by_artist(ra, a.year, rb, b.year)),
            _ => by_artist(ra, a.year, rb, b.year),
        };
        order.then(a.key.cmp(&b.key))
    });
    if let Some(group) = plan.group {
        if group == GroupBy::None {
            return Ok((tiles.iter().map(|tile| tile.key).collect(), Vec::new()));
        }
        let (tiles, sections) = grouped(tiles, |tile| {
            let release = shown(tile);
            match group {
                GroupBy::Year => Group::year(tile.year),
                GroupBy::Decade => Group::decade(tile.year),
                GroupBy::Added => {
                    Group::when(group_added.get(&tile.key).copied(), now_ms, "Earlier")
                }
                GroupBy::Quality => Group::quality(Tier::of(best(tile))),
                GroupBy::Shelf => Group::shelf(release.map_or(Shelf::Albums, shelf_of)),
                GroupBy::Letter | GroupBy::None => Group::letter(release.map_or('#', |release| {
                    fold::letter(if plan.sort == SortKey::Title {
                        &release.title_key
                    } else {
                        &release.artist_key
                    })
                })),
            }
        });
        return Ok((tiles.iter().map(|tile| tile.key).collect(), sections));
    }
    let sections = match plan.sort {
        SortKey::Discography => {
            let mut sections = sections(&tiles, |tile| shelf(tile).label().to_owned());
            for section in &mut sections {
                section.shelf = Shelf::ALL
                    .iter()
                    .copied()
                    .find(|shelf| shelf.label() == section.label);
            }
            sections
        }
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
        SortKey::Plays => Vec::new(),
        SortKey::LastPlayed => sections(&tiles, |tile| {
            Group::when(played(tile).last, now_ms, NEVER_PLAYED).label
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
    /// When it came into the library.
    added: Option<i64>,
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

fn songs(
    connection: &Connection,
    plan: &Plan,
    now_ms: i64,
) -> Result<(Vec<i64>, Vec<Section>), Error> {
    let conditions = &plan.conditions;
    let artist = match conditions.artist {
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
    let all = concat!(
        "SELECT ta.id, ta.release_id, rc.canonical_id, ta.display_title,
                      ta.display_disc_number, ta.display_track_number,
                      ?1 IS NULL OR EXISTS (
                        SELECT 1 FROM artist_credit_name acn JOIN artist a ON a.id = acn.artist_id
                         WHERE acn.artist_credit_id = ta.display_artist_credit_id
                           AND a.canonical_id = ?1),
                      ta.display_added_at_ms
               FROM track_appearance ta JOIN recording rc ON rc.id = ta.recording_id
               WHERE ta.redirect_to_id IS NULL AND ",
        reached!("ta")
    );
    // Songs on no record are found by `track_appearance_release`.
    let loose = format!("{all} AND ta.release_id IS NULL");
    let columns = if conditions.no_record {
        loose.as_str()
    } else {
        all
    };
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
            added: row.get(7)?,
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

    // What only some queries ask of a song is read only for them: its
    // plays, its stars and its best copy.
    let plays = if plan.by_plays() {
        plays_by_recording(connection)?
    } else {
        HashMap::new()
    };
    let played = |song: &Song| plays.get(&song.recording).copied().unwrap_or_default();
    let ratings: HashMap<i64, i64> = if plan.by_ratings() {
        let recordings: Vec<i64> = songs.iter().map(|song| song.recording).collect();
        crate::ratings::said(connection, &recordings)?
            .into_iter()
            .filter_map(|(recording, said)| Some((recording, said.rating?)))
            .collect()
    } else {
        HashMap::new()
    };
    let rating = |song: &Song| ratings.get(&song.recording).copied();
    let copies = if conditions.lossless.is_some() || plan.group == Some(GroupBy::Quality) {
        best_copies(connection, false)?
    } else {
        HashMap::new()
    };
    let best = |song: &Song| copies.get(&song.id).copied();
    let narrowed = !conditions.plays.is_open()
        || conditions.min_rating.is_some()
        || conditions.lossless.is_some();
    if narrowed {
        songs.retain(|song| {
            conditions.plays.holds(played(song).count)
                && conditions
                    .min_rating
                    .is_none_or(|least| rating(song).is_some_and(|stars| stars >= i64::from(least)))
                && conditions
                    .lossless
                    .is_none_or(|lossless| best(song).is_some_and(|best| best.0 == lossless))
        });
    }

    // In a group shown as one tile, a song on another version is hidden
    // where the version the group shows has the same recording. That needs
    // every version of the groups the songs are on: for a search or a
    // filter, only those groups are read.
    let mut on: Vec<i64> = songs.iter().filter_map(|song| song.release).collect();
    on.sort_unstable();
    on.dedup();
    let releases = if (matched.is_some() || artist.is_some() || conditions.no_record || narrowed)
        && on.len() <= LOOK_UP_AT_MOST
    {
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
    // A song is of the year of its release, as its row says.
    let year = |song: &Song| {
        song.release
            .and_then(|id| release_index.get(&id))
            .and_then(|index| releases.get(*index))
            .and_then(|release| release.year)
    };
    if !conditions.years.is_open() {
        songs.retain(|song| conditions.years.holds_known(year(song)));
    }
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
        _ => songs.sort_by(|a, b| {
            let order = match plan.sort {
                SortKey::Plays => played(b).count.cmp(&played(a).count),
                SortKey::LastPlayed => most_first(played(a).last, played(b).last),
                SortKey::Rating => most_first(rating(a), rating(b)),
                _ => Ordering::Equal,
            };
            order
                .then_with(|| by_name(&a.title_key, &b.title_key))
                .then(a.id.cmp(&b.id))
        }),
    }
    // The letter a song is under: its record's artist's in an order by
    // record, a song on none going after them all, and else its title's.
    let letter = |song: &Song| match plan.sort {
        SortKey::Album => album_of(song).map_or('#', |release| fold::letter(&release.artist_key)),
        _ => fold::letter(&song.title_key),
    };
    if let Some(group) = plan.group {
        if group == GroupBy::None {
            return Ok((songs.iter().map(|song| song.id).collect(), Vec::new()));
        }
        let (songs, sections) = grouped(songs, |song| match group {
            GroupBy::Year => Group::year(year(song)),
            GroupBy::Decade => Group::decade(year(song)),
            GroupBy::Added => Group::when(song.added, now_ms, "Earlier"),
            GroupBy::Quality => Group::quality(Tier::of(best(song))),
            GroupBy::Letter | GroupBy::Shelf | GroupBy::None => Group::letter(letter(song)),
        });
        return Ok((songs.iter().map(|song| song.id).collect(), sections));
    }
    let sections = match plan.sort {
        SortKey::Plays | SortKey::Rating => Vec::new(),
        SortKey::LastPlayed => sections(&songs, |song| {
            Group::when(played(song).last, now_ms, NEVER_PLAYED).label
        }),
        _ => sections(&songs, |song| letter(song).to_string()),
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

enum Filed {
    Name(String),
    /// A song without an album sorts after all named albums.
    Loose,
    /// The row was removed after the view opened.
    Gone,
}

/// What one row of a view is filed under in its order, for type-to-find.
fn filed_under(connection: &Connection, resolved: &View, key: i64) -> Result<Filed, Error> {
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
                .optional()?
                .map_or(Filed::Gone, Filed::Name));
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
                .optional()?
                .map_or(Filed::Gone, Filed::Name));
        }
        (ListKind::Songs, _) => "SELECT release_id FROM track_appearance WHERE id = ?1",
        (ListKind::Albums, _) if key < 0 => "SELECT ?1 * -1",
        // The release the tile shows, as `tiles` chooses it.
        (ListKind::Albums, _) => concat!(
            "SELECT coalesce(
               (SELECT r.id FROM release r JOIN release_group rg ON rg.id = r.release_group_id
                 WHERE rg.id = ?1 AND r.id = rg.display_release_id AND r.redirect_to_id IS NULL
                   AND ",
            reached_release!("r"),
            "),
               (SELECT min(r.id) FROM release r
                 WHERE r.release_group_id = ?1 AND r.redirect_to_id IS NULL AND ",
            reached_release!("r"),
            "))"
        ),
    };
    let release: Option<Option<i64>> = connection
        .query_row(sql, [key], |row| row.get(0))
        .optional()?;
    let release = match release {
        Some(Some(key)) => key,
        Some(None) if resolved.kind == ListKind::Songs => return Ok(Filed::Loose),
        _ => return Ok(Filed::Gone),
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
        .optional()?
        .map_or(Filed::Gone, Filed::Name))
}

/// Where a prefix typed is in a view, by how the view is found in
/// ([`Find`]).
pub(super) fn locate_prefix(
    connection: &Connection,
    resolved: &View,
    prefix: &str,
) -> Result<Option<u64>, Error> {
    let names = match resolved.find {
        Find::Nothing => return Ok(None),
        Find::Name => {
            let found = filed_at_or_after(connection, resolved, prefix, 0..resolved.ids.len())?;
            return Ok(found.map(|(at, _)| at));
        }
        Find::Sections { names } => names,
    };
    // Each section starts the order again, so a name is looked for in each,
    // and only one that begins with what was typed is a find: the row after
    // where it would be is some other name, in the first section.
    let target = fold::sort_text(prefix, None);
    if names && !target.is_empty() {
        let starts = resolved.sections.iter().map(|section| section.index);
        let ends = starts
            .clone()
            .skip(1)
            .chain(std::iter::once(resolved.ids.len() as u64));
        for (start, end) in starts.zip(ends) {
            let rows = usize::try_from(start).unwrap_or(usize::MAX)
                ..usize::try_from(end).unwrap_or(usize::MAX);
            if let Some((at, name)) = filed_at_or_after(connection, resolved, prefix, rows)?
                && name.starts_with(&target)
            {
                return Ok(Some(at));
            }
        }
    }
    // Or a section by its label: a year, a decade, "Lossless".
    let typed = fold::needle(prefix);
    if typed.is_empty() {
        return Ok(None);
    }
    Ok(resolved
        .sections
        .iter()
        .find(|section| fold::fold(&section.label).starts_with(&typed))
        .map(|section| section.index))
}

/// The first row of `rows` filed at or after `prefix`, and what it is
/// filed under, found by halving: `rows` are in order by name.
fn filed_at_or_after(
    connection: &Connection,
    resolved: &View,
    prefix: &str,
    rows: std::ops::Range<usize>,
) -> Result<Option<(u64, String)>, Error> {
    let target = fold::sort_text(prefix, None);
    if target.is_empty() {
        return Ok(None);
    }
    let (mut low, mut high) = (rows.start, rows.end.min(resolved.ids.len()));
    let mut found = None;
    while low < high {
        let middle = low + (high - low) / 2;
        let mut next_name = None;
        for (at, &key) in resolved.ids.iter().enumerate().take(high).skip(middle) {
            match filed_under(connection, resolved, key)? {
                Filed::Gone => {}
                Filed::Name(name) => {
                    next_name = Some((at, name));
                    break;
                }
                Filed::Loose => break,
            }
        }

        if let Some((at, name)) = next_name {
            if by_name(&name, &target) == Ordering::Less {
                low = at + 1;
                continue;
            }
            found = Some((at as u64, name));
        }
        high = middle;
    }
    Ok(found)
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
