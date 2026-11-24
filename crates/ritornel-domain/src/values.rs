//! Tag values a correction puts on files: what the files would say had
//! they been tagged right. A value replaces the file's own tag for its
//! field before anything is formed, and no rule rewrites it. See "Album
//! proposals" in docs/internals/corrections.md.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::library::{Item, Join, Library, Ref, Resolution};

/// The fields a value may set, named as claims are.
pub const FIELDS: [&str; 9] = [
    "album",
    "album_artist",
    "mb_album_artist",
    "disc_number",
    "mb_release_group",
    "primary_type",
    "original_date",
    "mb_release",
    "track_number",
];

/// The field of this name, if a value may set it.
pub fn field(name: &str) -> Option<&'static str> {
    FIELDS.iter().copied().find(|field| *field == name)
}

/// Where a value comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Origin {
    /// What the catalogue said about the album.
    Catalogue,
    /// What another of the library's files says: the first disc's title or
    /// credit, taken by the discs joined to it.
    Library,
}

impl Origin {
    /// Its name, as it is stored.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Catalogue => "catalogue",
            Self::Library => "library",
        }
    }

    /// The origin of this name.
    #[must_use]
    pub fn named(name: &str) -> Option<Self> {
        match name {
            "catalogue" => Some(Self::Catalogue),
            "library" => Some(Self::Library),
            _ => None,
        }
    }
}

/// One value for one field of one file. A field of several values, such as
/// a credit of two artists, has one for each, in order. A value of `None`
/// clears the field: the file says nothing for it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Value {
    /// The source item.
    pub item: i64,
    /// The field, one of [`FIELDS`].
    pub field: &'static str,
    /// Its position among the field's values, from 0.
    pub ordinal: u32,
    /// The value, or `None` to clear the field.
    pub value: Option<String>,
    /// Where it comes from.
    pub origin: Origin,
}

/// Puts values in place of the files' own tags. The values of a field
/// replace all the file's own of it, and the field is held: no rule
/// rewrites it.
pub fn apply(library: &mut Library, values: &[Value]) {
    /// Each field's values, with their positions.
    type Fields<'a> = BTreeMap<&'static str, Vec<(u32, Option<&'a str>)>>;
    let mut by_item: BTreeMap<i64, Fields<'_>> = BTreeMap::new();
    for value in values {
        by_item
            .entry(value.item)
            .or_default()
            .entry(value.field)
            .or_default()
            .push((value.ordinal, value.value.as_deref()));
    }
    for item in &mut library.items {
        let Some(fields) = by_item.get_mut(&item.id) else {
            continue;
        };
        for (field, given) in fields.iter_mut() {
            given.sort_unstable();
            let all: Vec<String> = given
                .iter()
                .filter_map(|(_, value)| value.map(str::to_owned))
                .collect();
            let first = all.first().cloned();
            let facts = &mut item.facts;
            match *field {
                "album" => facts.album = first,
                "album_artist" => facts.album_artist = all,
                "mb_album_artist" => facts.album_artist_mbids = all,
                // A disc number is a number from 1, or none.
                "disc_number" => {
                    facts.disc = first
                        .and_then(|disc| disc.parse().ok())
                        .filter(|disc: &i64| *disc > 0);
                }
                "mb_release_group" => facts.release_group_mbid = first,
                "primary_type" => facts.primary_type = first,
                "original_date" => facts.original_date = first,
                "mb_release" => facts.release_mbid = first,
                // A track number is a number from 1, or none.
                "track_number" => {
                    facts.track = first
                        .and_then(|track| track.parse().ok())
                        .filter(|track: &i64| *track > 0);
                }
                _ => continue,
            }
            if !item.held.contains(field) {
                item.held.push(field);
            }
        }
    }
}

/// Puts an item's held values on the facts of a file that replaced its
/// audio, so the file is judged the same recording or not as the item
/// answered is, not by its new tags alone.
pub fn hold(item: &crate::library::Item, facts: &mut crate::Facts) {
    for field in &item.held {
        let from = &item.facts;
        match *field {
            "album" => facts.album.clone_from(&from.album),
            "album_artist" => facts.album_artist.clone_from(&from.album_artist),
            "mb_album_artist" => facts
                .album_artist_mbids
                .clone_from(&from.album_artist_mbids),
            "disc_number" => facts.disc = from.disc,
            "mb_release_group" => facts
                .release_group_mbid
                .clone_from(&from.release_group_mbid),
            "primary_type" => facts.primary_type.clone_from(&from.primary_type),
            "original_date" => facts.original_date.clone_from(&from.original_date),
            "mb_release" => facts.release_mbid.clone_from(&from.release_mbid),
            "track_number" => facts.track = from.track,
            _ => {}
        }
    }
}

/// One album as a preview shows it: a release group and what is on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlbumShape {
    /// The title its tile shows.
    pub title: Option<String>,
    /// The credit its tile shows.
    pub artist: Option<String>,
    /// Its kind.
    pub primary_type: Option<String>,
    /// The year it first came out.
    pub first_year: Option<i64>,
    /// How many releases it holds.
    pub releases: u32,
    /// How many discs its fullest release has.
    pub discs: u32,
    /// How many songs are on it, on all its releases.
    pub songs: u32,
}

/// What a set of values would change: the albums the files they touch are
/// on now, with everything else on those albums, and the albums all of
/// those files are on once the values are in place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preview {
    /// The albums now.
    pub now: Vec<AlbumShape>,
    /// The albums after.
    pub after: Vec<AlbumShape>,
    /// How many songs would be joined into another: two editions' copies of
    /// one song that the library holds as two, and would hold as one.
    pub songs_joined: u32,
    /// How many songs would be parted from one they are now: copies the
    /// library holds as one song, and would hold as two.
    pub songs_parted: u32,
}

/// Previews values: the projector run on the library as it is and with the
/// values in place, read-only. See "Album proposals" in
/// docs/internals/corrections.md.
pub fn preview(
    library: &Library,
    values: &[Value],
    joins: &[Join],
    policy: &crate::Policy,
) -> Preview {
    let mut changed = library.clone();
    apply(&mut changed, values);
    changed.answer_joins.extend_from_slice(joins);
    let touched: BTreeSet<i64> = values.iter().map(|value| value.item).collect();
    preview_between(library, &changed, &touched, policy)
}

/// Previews a change made otherwise, such as an undo: the projector run on
/// the library as it is (`now`) and as it would be (`after`), read-only,
/// for the albums the `touched` files are on, now or after.
pub fn preview_between(
    now: &Library,
    after: &Library,
    touched: &BTreeSet<i64>,
    policy: &crate::Policy,
) -> Preview {
    let before = crate::resolve(now, policy);
    let after = crate::resolve(after, policy);
    let touched = touched.clone();
    // Every file on an album a touched file is on, now or after.
    let mut files = touched.clone();
    for resolution in [&before, &after] {
        let placed = placed(resolution);
        let groups = groups_of(resolution, &placed, &touched);
        files.extend(
            resolution
                .items
                .iter()
                .filter(|out| {
                    out.appearance
                        .and_then(|at| placed.get(&at))
                        .is_some_and(|(_, group)| groups.contains(group))
                })
                .map(|out| out.item),
        );
    }
    Preview {
        now: shapes(&before, &files),
        after: shapes(&after, &files),
        songs_joined: songs_merged(&before, &after),
        songs_parted: songs_merged(&after, &before),
    }
}

/// How many songs of `from` are one with another in `to`: for each song of
/// `to`, the songs of `from` its files were on, but one.
fn songs_merged(from: &Resolution, to: &Resolution) -> u32 {
    let song_of = |resolution: &Resolution| -> HashMap<i64, Ref> {
        resolution
            .items
            .iter()
            .filter_map(|out| Some((out.item, out.recording?)))
            .collect()
    };
    let was = song_of(from);
    let mut sources: HashMap<Ref, HashSet<Ref>> = HashMap::new();
    for (item, is) in song_of(to) {
        if let Some(had) = was.get(&item) {
            sources.entry(is).or_default().insert(*had);
        }
    }
    let merged: usize = sources
        .values()
        .map(|had| had.len().saturating_sub(1))
        .sum();
    u32::try_from(merged).unwrap_or(u32::MAX)
}

/// The release each file's claims put it on, as the catalog forms releases
/// for files the resolver places, by the file's row.
#[must_use]
pub fn release_keys(library: &Library) -> HashMap<i64, String> {
    crate::catalog::release_keys(library)
}

/// What a yes to an album puts together (see [`Join`]): the releases its
/// own files (`items`) and the files it joins them with (`joins`) are on
/// now, their groups, and the group of every release that is that album
/// (`release_group`) now by its files' own tags, every file of it saying
/// so. A release another answer made that album is not counted: its yes
/// joins the album while it stands. `library` holds the values in force.
pub fn join_of(library: &Library, items: &[i64], joins: &[i64], release_group: &str) -> Join {
    let release_of = |item: &Item| -> Option<i64> {
        let appearance = item.current.as_ref()?.appearance?;
        let appearance = library
            .appearance_canonical
            .get(&appearance)
            .copied()
            .unwrap_or(appearance);
        let release = library.appearances.get(&appearance)?.1?;
        library
            .releases
            .get(&release)
            .map(|existing| existing.canonical)
    };
    let wanted: BTreeSet<i64> = items.iter().chain(joins).copied().collect();
    let mut releases = BTreeSet::new();
    // Each release's files, and how many of them say they are the album.
    let mut of_album: HashMap<i64, (usize, usize)> = HashMap::new();
    for item in library.items.iter().filter(|item| !item.retired) {
        let Some(release) = release_of(item) else {
            continue;
        };
        if wanted.contains(&item.id) {
            releases.insert(release);
        }
        let (files, saying) = of_album.entry(release).or_default();
        *files += 1;
        if item.facts.release_group_mbid.as_deref() == Some(release_group)
            && !item.held.contains(&"mb_release_group")
        {
            *saying += 1;
        }
    }
    let groups: BTreeSet<i64> = of_album
        .iter()
        .filter(|(release, (files, saying))| releases.contains(*release) || files == saying)
        .filter_map(|(release, _)| library.releases.get(release).map(|existing| existing.group))
        .collect();
    Join {
        album: release_group.to_owned(),
        releases: releases.into_iter().collect(),
        groups: groups.into_iter().collect(),
    }
}

/// Where each appearance is, by its row or new index: its release and
/// that release's group, as indexes into the resolution.
type Placed = HashMap<Ref, (usize, usize)>;

fn placed(resolution: &Resolution) -> Placed {
    resolution
        .appearances
        .iter()
        .filter_map(|appearance| {
            let release = appearance.release?;
            let group = resolution.releases.get(release)?.group;
            Some((appearance.r, (release, group)))
        })
        .collect()
}

/// The release groups the files are on.
fn groups_of(resolution: &Resolution, placed: &Placed, items: &BTreeSet<i64>) -> BTreeSet<usize> {
    resolution
        .items
        .iter()
        .filter(|out| items.contains(&out.item))
        .filter_map(|out| placed.get(&out.appearance?).map(|(_, group)| *group))
        .collect()
}

/// Each album the files are on, in the order of their groups: its title,
/// credit, kind and year, and its releases, discs and songs, counted in one
/// pass over the appearances.
fn shapes(resolution: &Resolution, files: &BTreeSet<i64>) -> Vec<AlbumShape> {
    let placed = placed(resolution);
    let wanted = groups_of(resolution, &placed, files);
    // Each release of a wanted group: its discs and songs.
    let mut releases: BTreeMap<usize, (BTreeSet<i64>, u32)> = BTreeMap::new();
    for appearance in &resolution.appearances {
        let Some((release, group)) = placed.get(&appearance.r) else {
            continue;
        };
        if wanted.contains(group) {
            let (discs, songs) = releases.entry(*release).or_default();
            discs.insert(appearance.disc.unwrap_or(1));
            *songs += 1;
        }
    }
    wanted
        .into_iter()
        .filter_map(|index| {
            let group = resolution.groups.get(index)?;
            let of_group: Vec<(&usize, &(BTreeSet<i64>, u32))> = releases
                .iter()
                .filter(|(release, _)| {
                    resolution
                        .releases
                        .get(**release)
                        .is_some_and(|r| r.group == index)
                })
                .collect();
            let shown = group
                .display_release
                .or_else(|| of_group.first().map(|(release, _)| **release))
                .and_then(|release| resolution.releases.get(release));
            Some(AlbumShape {
                title: shown.and_then(|release| release.title.clone()),
                artist: shown
                    .and_then(|release| release.credit)
                    .and_then(|credit| resolution.credits.get(credit))
                    .map(|credit| credit.text.clone()),
                primary_type: group.primary_type.clone(),
                first_year: group.first_year,
                releases: u32::try_from(of_group.len()).unwrap_or(u32::MAX),
                discs: of_group
                    .iter()
                    .map(|(_, (discs, _))| discs.len())
                    .max()
                    .map_or(0, |n| u32::try_from(n).unwrap_or(u32::MAX)),
                songs: of_group.iter().map(|(_, (_, songs))| *songs).sum(),
            })
        })
        .collect()
}
