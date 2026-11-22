//! From bound items to a catalog: releases, track appearances, release
//! groups, artists and credits, and what each of them shows. See "Resolver
//! policy 1", "Displayed metadata" and "Release groups" in
//! docs/design/identity.md.
//!
//! Everything here is recomputed from the claims on every run and mapped
//! back onto the rows that exist, so that a run over an unchanged library
//! asks for no change.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::hash::Hash;

/// The version of how claims become releases, discs and credits, beside
/// the policy's. A library last resolved by another is resolved again.
///
/// 1: a name with no ID written as one artist's sort name is that artist.
/// 2: a disc marker before an album title's bracketed notes is a disc.
/// 3: an album none of whose files names an artist is shown as Unknown
///    Artist's, not Various Artists', and is formed and grouped as before.
pub const CATALOG_VERSION: i64 = 3;

use crate::extract::{Facts, parse_date};
use crate::library::{
    AppearanceOut, ArtistKeyKind, ArtistOut, CreditOut, GroupOut, Item, ItemOut, Kind, Library,
    RecordingOut, Ref, ReleaseOut, Resolution,
};
use crate::normalize::{NORMALIZER_VERSION, core, normalize, normalize_by};
use crate::resolve::{Policy, RecKey, fold};

/// The name of the album artist of an album whose tracks have different
/// artists and no album artist tag.
const VARIOUS: &str = "Various Artists";

/// The name an album none of whose files names an artist is shown under, as
/// other players show it. It is only shown: the album is formed and grouped
/// as one of Various Artists, as it was before it had a name of its own. A
/// file tagged with this name is the same artist.
const UNKNOWN: &str = "Unknown Artist";

/// Whether a file names no artist at all, neither its track's nor its
/// album's, by name or by ID.
fn names_no_artist(facts: &Facts) -> bool {
    facts.artist.is_empty()
        && facts.artists.is_empty()
        && facts.artist_mbids.is_empty()
        && facts.album_artist.is_empty()
        && facts.album_artist_mbids.is_empty()
}

/// The most common of some values, and among equals the one seen first.
/// The values come in first-seen order of their items.
fn pick<T: Clone + Eq + Hash>(values: impl IntoIterator<Item = T>) -> Option<T> {
    let mut counts: Vec<(T, usize)> = Vec::new();
    let mut index: HashMap<T, usize> = HashMap::new();
    for value in values {
        match index.get(&value).and_then(|at| counts.get_mut(*at)) {
            Some(entry) => entry.1 += 1,
            None => {
                index.insert(value.clone(), counts.len());
                counts.push((value, 1));
            }
        }
    }
    let most = counts.iter().map(|(_, count)| *count).max()?;
    counts
        .into_iter()
        .find(|(_, count)| *count == most)
        .map(|(value, _)| value)
}

/// Who an artist is: a MusicBrainz ID, or else a normalised name.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum ArtistKey {
    Mbid(String),
    Name(String),
}

/// A credit as the tags give it, before its artists are rows.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CreditSpec {
    text: String,
    names: Vec<(ArtistKey, String, String)>,
    sort: Option<String>,
}

/// The credit a tag's values and its MusicBrainz IDs make. Artist strings
/// are not parsed: one string is one artist, a multi-valued tag gives one
/// artist per value, and with IDs each ID is one artist.
fn credit(
    values: &[String],
    list: &[String],
    mbids: &[String],
    sort: Option<&String>,
) -> Option<CreditSpec> {
    let text = match values {
        [] => return None,
        [one] => one.clone(),
        several => several.join(", "),
    };
    // The names the IDs belong to: a list of the same length, or the one
    // string for one ID.
    let named: Option<&[String]> = if mbids.is_empty() {
        None
    } else if list.len() == mbids.len() {
        Some(list)
    } else if values.len() == mbids.len() {
        Some(values)
    } else {
        None
    };
    let names: Vec<(ArtistKey, String, String)> = match named {
        Some(names) => {
            // The join phrases are what is written between the names.
            let mut rest = text.as_str();
            let mut joins: Vec<String> = Vec::new();
            let mut found = values.len() == 1;
            for (index, name) in names.iter().enumerate() {
                match rest.find(name.as_str()) {
                    Some(at) if found && (index > 0 || at == 0) => {
                        if index > 0 {
                            joins.push(rest.get(..at).unwrap_or("").to_owned());
                        }
                        rest = rest.get(at + name.len()..).unwrap_or("");
                    }
                    _ => found = false,
                }
            }
            if found && !rest.is_empty() {
                // Something follows the last name, so the string is not
                // these names and their joins.
                found = false;
            }
            names
                .iter()
                .zip(mbids)
                .enumerate()
                .map(|(index, (name, mbid))| {
                    let join = if index + 1 == names.len() {
                        String::new()
                    } else if found {
                        joins.get(index).cloned().unwrap_or_default()
                    } else {
                        ", ".to_owned()
                    };
                    (ArtistKey::Mbid(mbid.clone()), name.clone(), join)
                })
                .collect()
        }
        None => values
            .iter()
            .enumerate()
            .map(|(index, name)| {
                let join = if index + 1 == values.len() { "" } else { ", " };
                (
                    ArtistKey::Name(normalize(name)),
                    name.clone(),
                    join.to_owned(),
                )
            })
            .collect(),
    };
    let text = if named.is_some() && values.len() != 1 {
        names
            .iter()
            .map(|(_, name, join)| format!("{name}{join}"))
            .collect()
    } else {
        text
    };
    Some(CreditSpec {
        text,
        sort: sort.filter(|_| names.len() == 1).cloned(),
        names,
    })
}

/// One item the resolver may place: bound, and not held where it is.
struct Active<'a> {
    seq: usize,
    item: &'a Item,
    /// The index of its recording in the resolution.
    rec: usize,
    /// The recording its assignment names.
    assign: Ref,
    track_credit: Option<CreditSpec>,
    /// The album artist credit its claims give, which forms and groups
    /// releases.
    release_credit: Option<CreditSpec>,
    /// The album artist credit its release shows: `release_credit`, or
    /// Unknown Artist when no file of the release names an artist (and no
    /// other version of the record does, which is settled with the groups).
    shown_credit: Option<CreditSpec>,
    /// The index of its release in `releases`.
    release: Option<usize>,
    appearance: Option<usize>,
    /// The owner's assignment holds it where it is. Its claims are still
    /// projected onto what it is bound to.
    pinned: bool,
}

/// A release being formed.
struct Forming {
    key: String,
    members: Vec<usize>,
    r: Ref,
}

fn common_parts(a: &str, b: &str) -> usize {
    a.split('/')
        .zip(b.split('/'))
        .take_while(|(a, b)| a == b)
        .count()
}

/// Splits the files that share an album artist and an album title into
/// releases. They are one release unless two of them in different folders
/// claim the same disc and track: then the folders that collide part, and a
/// folder that collides with nothing stays with the nearest one it fits.
fn split_by_folder(members: &[(usize, &Item)]) -> Vec<Vec<usize>> {
    type Position = (i64, i64);
    /// The folders of one release, the positions taken and the files.
    type Cluster<'a> = (Vec<&'a str>, HashSet<Position>, Vec<usize>);
    let mut folders: BTreeMap<&str, Vec<(usize, Position)>> = BTreeMap::new();
    for (index, item) in members {
        let slot = (item.facts.disc.unwrap_or(1), item.facts.track.unwrap_or(-1));
        folders
            .entry(item.folder.as_str())
            .or_default()
            .push((*index, slot));
    }
    let mut clusters: Vec<Cluster<'_>> = Vec::new();
    for (folder, files) in folders {
        let slots: HashSet<(i64, i64)> = files
            .iter()
            .map(|(_, slot)| *slot)
            .filter(|(_, track)| *track >= 0)
            .collect();
        let fits = clusters
            .iter()
            .enumerate()
            .filter(|(_, (_, taken, _))| taken.is_disjoint(&slots))
            .max_by_key(|(at, (names, _, _))| {
                let near = names
                    .iter()
                    .map(|name| common_parts(name, folder))
                    .max()
                    .unwrap_or(0);
                (near, std::cmp::Reverse(*at))
            })
            .map(|(at, _)| at);
        let indexes = files.iter().map(|(index, _)| *index);
        match fits.and_then(|at| clusters.get_mut(at)) {
            Some((names, taken, cluster)) => {
                names.push(folder);
                taken.extend(slots);
                cluster.extend(indexes);
            }
            None => clusters.push((vec![folder], slots, indexes.collect())),
        }
    }
    clusters
        .into_iter()
        .map(|(_, _, members)| members)
        .collect()
}

/// Gives each thing being formed the row most of its members are on now,
/// each row to at most one. Rows in `owner_rows` (rows carrying owner edits,
/// merges, or settings) always win over unedited rows so automatic regrouping
/// never discards owner input. `preferred` gives `(row, index)` pairs that
/// break ties in vote count when a group splits.
fn keep_rows(
    votes: Vec<(String, Vec<i64>)>,
    owner_rows: &HashSet<i64>,
    preferred: &HashSet<(i64, usize)>,
) -> Vec<Option<i64>> {
    let mut ballots: Vec<(bool, usize, bool, i64, usize)> = Vec::new();
    for (index, (_, rows)) in votes.iter().enumerate() {
        let mut counts: BTreeMap<i64, usize> = BTreeMap::new();
        for row in rows {
            *counts.entry(*row).or_default() += 1;
        }
        ballots.extend(counts.into_iter().map(|(row, count)| {
            (
                owner_rows.contains(&row),
                count,
                preferred.contains(&(row, index)),
                row,
                index,
            )
        }));
    }
    ballots.sort_by(|a, b| {
        let key = |index: usize| votes.get(index).map(|(key, _)| key.as_str());
        b.0.cmp(&a.0)
            .then(b.1.cmp(&a.1))
            .then(b.2.cmp(&a.2))
            .then_with(|| key(a.4).cmp(&key(b.4)))
            .then(a.3.cmp(&b.3))
    });
    let mut kept: Vec<Option<i64>> = vec![None; votes.len()];
    let mut taken: HashSet<i64> = HashSet::new();
    for (_, _, _, row, index) in ballots {
        if let Some(slot) = kept.get_mut(index)
            && slot.is_none()
            && taken.insert(row)
        {
            *slot = Some(row);
        }
    }
    kept
}

fn find(parents: &mut [usize], mut node: usize) -> usize {
    while let Some(parent) = parents.get(node).copied()
        && parent != node
    {
        node = parent;
    }
    node
}

fn union(parents: &mut [usize], a: usize, b: usize) {
    let (a, b) = (find(parents, a), find(parents, b));
    if let Some(slot) = parents.get_mut(a.max(b)) {
        *slot = a.min(b);
    }
}

/// The album artist credit of each item that has an album: its album
/// artist tag, or else the artist its folder's files of that album share,
/// or else various.
fn release_credits(items: &[&Item]) -> Vec<Option<CreditSpec>> {
    let mut shared: HashMap<(&str, String), Option<String>> = HashMap::new();
    for item in items {
        let facts = &item.facts;
        if let (Some(album), true) = (&facts.album, facts.album_artist.is_empty()) {
            let artist = facts.artist_text().unwrap_or_default();
            shared
                .entry((item.folder.as_str(), normalize(album)))
                .and_modify(|one| {
                    if one
                        .as_ref()
                        .is_some_and(|one| normalize(one) != normalize(&artist))
                    {
                        *one = None;
                    }
                })
                .or_insert(Some(artist));
        }
    }
    items
        .iter()
        .map(|item| {
            let facts = &item.facts;
            let album = facts.album.as_ref()?;
            credit(
                &facts.album_artist,
                &[],
                &facts.album_artist_mbids,
                facts.album_artist_sort.as_ref(),
            )
            .or_else(|| {
                let artist = shared
                    .get(&(item.folder.as_str(), normalize(album)))
                    .cloned()
                    .flatten()
                    .filter(|artist| !artist.is_empty())
                    .unwrap_or_else(|| VARIOUS.to_owned());
                credit(&[artist], &[], &[], None)
            })
        })
        .collect()
}

/// Forms releases from items and their album artist credits: a release
/// MBID picks the release, and otherwise album artist and album title do,
/// split by folder where two folders claim the same disc and track. Gives
/// each release's key and the indexes of its members. An item with no
/// album is on none.
fn form_releases(items: &[(usize, &Item, Option<&CreditSpec>)]) -> Vec<(String, Vec<usize>)> {
    let mut keyed: BTreeMap<String, Vec<(usize, &Item)>> = BTreeMap::new();
    for (index, item, spec) in items {
        let facts: &Facts = &item.facts;
        let Some(album) = &facts.album else { continue };
        let key = match &facts.release_mbid {
            Some(mbid) => format!("m\u{1f}{mbid}"),
            None => format!(
                "t\u{1f}{}\u{1f}{}",
                spec.map(|credit| normalize(&credit.text))
                    .unwrap_or_default(),
                normalize(album)
            ),
        };
        keyed.entry(key).or_default().push((*index, item));
    }
    let mut formed = Vec::new();
    for (key, members) in keyed {
        if key.starts_with('m') {
            formed.push((key, members.into_iter().map(|(index, _)| index).collect()));
            continue;
        }
        for (part, members) in split_by_folder(&members).into_iter().enumerate() {
            formed.push((format!("{key}\u{1f}{part}"), members));
        }
    }
    formed
}

/// The release each item's claims put it on, as the catalog forms releases
/// for files the resolver places, by the source item's row. The resolver
/// reads it to tell two tracks of one release apart before anything is
/// bound. An item with no album, or no audio, is on none.
pub(crate) fn release_keys(library: &Library) -> HashMap<i64, String> {
    let items: Vec<&Item> = library
        .items
        .iter()
        .filter(|item| item.audio.is_some() && !item.retired)
        .collect();
    let credits = release_credits(&items);
    let listed: Vec<(usize, &Item, Option<&CreditSpec>)> = items
        .iter()
        .zip(&credits)
        .enumerate()
        .map(|(index, (item, spec))| (index, *item, spec.as_ref()))
        .collect();
    let mut keys = HashMap::new();
    for (key, members) in form_releases(&listed) {
        for index in members {
            if let Some(item) = items.get(index) {
                keys.insert(item.id, key.clone());
            }
        }
    }
    keys
}

/// A date and its year, when the claim holds one.
fn dated(value: Option<String>) -> (Option<String>, Option<i64>) {
    match value.as_deref().and_then(parse_date) {
        Some((date, year)) => (Some(date), Some(year)),
        None => (None, None),
    }
}

/// Resolves a library: binds every item to a recording by the policy, forms
/// the catalog its claims describe, and projects what each entity shows.
///
/// It is pure, and it reads the current assignments only to keep what need
/// not change. Run on a library it has just resolved, it reports no change:
/// that is what `re-resolve --dry-run` checks.
pub fn resolve(library: &Library, policy: &Policy) -> Resolution {
    // The disc numbers the policy's release rules imply, when one is on.
    let implied = crate::discs::apply(library, policy);
    let library = implied.as_ref().unwrap_or(library);
    let folded = fold(library, policy);
    let edit = |kind: Kind, r: Ref, field: &str| -> Option<String> {
        match r {
            Ref::Row(row) => library.edits.get(&(kind, row, field.to_owned())).cloned(),
            Ref::New(_) => None,
        }
    };

    // Recordings: one for each the fold bound an item to.
    let mut recordings: Vec<RecordingOut> = folded
        .recs
        .iter()
        .enumerate()
        .map(|(index, key)| RecordingOut {
            r: match key {
                RecKey::Row(row) => Ref::Row(*row),
                RecKey::Founded(_) => Ref::New(index),
            },
            title: None,
            credit: None,
            projected: false,
        })
        .collect();

    let mut actives: Vec<Active<'_>> = Vec::new();
    for (seq, (item, bound)) in library.items.iter().zip(&folded.items).enumerate() {
        let Some(rec) = bound.rec.filter(|_| !bound.frozen || bound.pinned) else {
            continue;
        };
        let Some(recording) = recordings.get(rec) else {
            continue;
        };
        let facts = &item.facts;
        actives.push(Active {
            seq,
            item,
            rec,
            assign: bound.keep_row.map_or(recording.r, Ref::Row),
            track_credit: credit(
                &facts.artist,
                &facts.artists,
                &facts.artist_mbids,
                facts.artist_sort.as_ref(),
            ),
            release_credit: None,
            shown_credit: None,
            release: None,
            appearance: None,
            pinned: bound.pinned,
        });
    }

    let credits = release_credits(&actives.iter().map(|active| active.item).collect::<Vec<_>>());
    for (active, spec) in actives.iter_mut().zip(credits) {
        active.release_credit = spec;
    }
    // A pinned file stays on the release it is on, which is found once the
    // others have their rows.
    let unpinned: Vec<(usize, &Item, Option<&CreditSpec>)> = actives
        .iter()
        .enumerate()
        .filter(|(_, active)| !active.pinned)
        .map(|(index, active)| (index, active.item, active.release_credit.as_ref()))
        .collect();
    let mut forming: Vec<Forming> = form_releases(&unpinned)
        .into_iter()
        .map(|(key, members)| Forming {
            key,
            members,
            r: Ref::New(0),
        })
        .collect();
    let release_of = |item: &Item| -> Option<i64> {
        let appearance = item.current?.appearance?;
        let release = library.appearances.get(&appearance)?.1?;
        Some(
            library
                .releases
                .get(&release)
                .map_or(release, |release| release.canonical),
        )
    };
    // A merge of releases is the owner's and lasts: files on a release that
    // a merge was made into stay one release, whatever their tags say. The
    // formations that hold files on such a release are joined before rows
    // are given out.
    let merged: HashSet<i64> = library
        .releases
        .iter()
        .filter(|(row, release)| **row != release.canonical)
        .map(|(_, release)| release.canonical)
        .collect();
    if !merged.is_empty() {
        let mut parents: Vec<usize> = (0..forming.len()).collect();
        let mut holder: HashMap<i64, usize> = HashMap::new();
        for (at, release) in forming.iter().enumerate() {
            for member in &release.members {
                let Some(canonical) = actives
                    .get(*member)
                    .and_then(|active| release_of(active.item))
                else {
                    continue;
                };
                if merged.contains(&canonical) {
                    let first = *holder.entry(canonical).or_insert(at);
                    union(&mut parents, first, at);
                }
            }
        }
        let mut joined: BTreeMap<usize, Forming> = BTreeMap::new();
        for (at, release) in std::mem::take(&mut forming).into_iter().enumerate() {
            match joined.entry(find(&mut parents, at)) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(release);
                }
                std::collections::btree_map::Entry::Occupied(mut entry) => {
                    let into = entry.get_mut();
                    into.key = into.key.clone().min(release.key);
                    into.members.extend(release.members);
                    into.members.sort_unstable();
                }
            }
        }
        forming = joined.into_values().collect();
    }
    let mut owner_groups: HashSet<i64> = library
        .groups
        .iter()
        .filter(|(_, group)| group.has_presentation || group.preferred_release.is_some())
        .map(|(row, _)| *row)
        .collect();
    for (kind, row, _) in library.edits.keys() {
        if *kind == Kind::ReleaseGroup {
            owner_groups.insert(*row);
        }
    }
    let mut owner_releases: HashSet<i64> = merged.clone();
    for (kind, row, _) in library.edits.keys() {
        if *kind == Kind::Release {
            owner_releases.insert(*row);
        }
    }
    for existing in library.releases.values() {
        if existing.chosen_group.is_some() || owner_groups.contains(&existing.group) {
            owner_releases.insert(existing.canonical);
        }
    }
    // Automatic release formation never silently collapses two distinct
    // canonical releases that BOTH carry owner input (edits, merges, a chosen
    // group, or membership in an owner-edited group).
    let mut split_forming: Vec<Forming> = Vec::with_capacity(forming.len());
    for release in forming {
        let mut distinct_owners: BTreeSet<i64> = BTreeSet::new();
        for &member in &release.members {
            if let Some(rel) = actives
                .get(member)
                .and_then(|active| release_of(active.item))
                && owner_releases.contains(&rel)
            {
                distinct_owners.insert(rel);
            }
        }
        if distinct_owners.len() <= 1 {
            split_forming.push(release);
            continue;
        }
        let first_owner = *distinct_owners.iter().next().unwrap_or(&0);
        let mut buckets: BTreeMap<i64, Vec<usize>> = BTreeMap::new();
        for member in release.members {
            let bucket = actives
                .get(member)
                .and_then(|active| release_of(active.item))
                .filter(|rel| distinct_owners.contains(rel))
                .unwrap_or(first_owner);
            buckets.entry(bucket).or_default().push(member);
        }
        for (owner_rel, members) in buckets {
            split_forming.push(Forming {
                key: format!("{}\u{1f}o{owner_rel}", release.key),
                members,
                r: Ref::New(0),
            });
        }
    }
    forming = split_forming;

    let no_preferred: HashSet<(i64, usize)> = HashSet::new();
    let votes = forming
        .iter()
        .map(|release| {
            let rows = release
                .members
                .iter()
                .filter_map(|index| actives.get(*index))
                .filter_map(|active| release_of(active.item))
                .collect();
            (release.key.clone(), rows)
        })
        .collect();
    for (release, row) in forming
        .iter_mut()
        .zip(keep_rows(votes, &owner_releases, &no_preferred))
    {
        release.r = row.map_or(Ref::New(usize::MAX), Ref::Row);
    }
    // Each pinned file joins the release it is on, or holds it alone.
    for (index, active) in actives.iter().enumerate() {
        if !active.pinned {
            continue;
        }
        let Some(row) = release_of(active.item) else {
            continue;
        };
        match forming
            .iter_mut()
            .find(|release| release.r == Ref::Row(row))
        {
            Some(release) => release.members.push(index),
            None => forming.push(Forming {
                key: format!("p\u{1f}{row}"),
                members: vec![index],
                r: Ref::Row(row),
            }),
        }
    }
    let unknown = credit(&[UNKNOWN.to_owned()], &[], &[], None);
    for (index, release) in forming.iter_mut().enumerate() {
        if release.r == Ref::New(usize::MAX) {
            release.r = Ref::New(index);
        }
        let nameless = release.members.iter().all(|member| {
            actives
                .get(*member)
                .is_some_and(|active| names_no_artist(&active.item.facts))
        });
        for member in &release.members {
            if let Some(active) = actives.get_mut(*member) {
                active.release = Some(index);
                active.shown_credit = if nameless {
                    unknown.clone()
                } else {
                    active.release_credit.clone()
                };
            }
        }
    }

    // Appearances. An item keeps the one it has while that one's recording
    // and release are still its own. Otherwise it shares one that has its
    // recording, release, disc and track, or gets a new one.
    type Slot = (Ref, Option<usize>, Option<i64>, Option<i64>);
    let mut appearance_rows: Vec<(Ref, Vec<usize>)> = Vec::new();
    let mut by_slot: HashMap<Slot, usize> = HashMap::new();
    let mut by_row: HashMap<i64, usize> = HashMap::new();
    let slot_of = |active: &Active<'_>| -> Slot {
        (
            active.assign,
            active.release,
            active.item.facts.disc,
            active.item.facts.track,
        )
    };
    for pass in [true, false] {
        for (index, active) in actives.iter_mut().enumerate() {
            if active.appearance.is_some() {
                continue;
            }
            let slot = slot_of(active);
            let wanted_release = active
                .release
                .and_then(|at| forming.get(at))
                .map(|release| release.r);
            let held = active
                .item
                .current
                .and_then(|current| current.appearance)
                .filter(|row| {
                    library
                        .appearances
                        .get(row)
                        .is_some_and(|(recording, release)| {
                            let release = release
                                .map(|row| {
                                    library
                                        .releases
                                        .get(&row)
                                        .map_or(row, |release| release.canonical)
                                })
                                .map(Ref::Row);
                            Ref::Row(*recording) == active.assign && release == wanted_release
                        })
                });
            // A pinned file keeps its appearance, whatever its tags say.
            let held = if active.pinned {
                active.item.current.and_then(|current| current.appearance)
            } else {
                held
            };
            let at = match (pass, held) {
                (true, Some(row)) => *by_row.entry(row).or_insert_with(|| {
                    appearance_rows.push((Ref::Row(row), Vec::new()));
                    appearance_rows.len() - 1
                }),
                (true, None) => continue,
                (false, _) => match by_slot.get(&slot) {
                    Some(at) => *at,
                    None => {
                        appearance_rows.push((Ref::New(appearance_rows.len()), Vec::new()));
                        appearance_rows.len() - 1
                    }
                },
            };
            by_slot.entry(slot).or_insert(at);
            if let Some((_, members)) = appearance_rows.get_mut(at) {
                members.push(index);
            }
            active.appearance = Some(at);
        }
    }

    // Artists: each MusicBrainz ID is one, and a name with no ID is the
    // artist of that exact normalised name.
    struct Seen {
        names: Vec<String>,
        sorts: Vec<String>,
        first: usize,
    }
    let mut seen: HashMap<ArtistKey, Seen> = HashMap::new();
    // The keys a track's own credit names, as opposed to an album credit
    // made of its tracks' artists.
    let mut on_tracks: HashSet<ArtistKey> = HashSet::new();
    for active in &actives {
        on_tracks.extend(
            active
                .track_credit
                .iter()
                .flat_map(|spec| spec.names.iter().map(|(key, _, _)| key.clone())),
        );
        // Every artist the claims name, as before there was Unknown Artist,
        // and Unknown Artist where a release shows it instead.
        let shown = Some(&active.shown_credit).filter(|shown| **shown != active.release_credit);
        for spec in [
            Some(&active.track_credit),
            Some(&active.release_credit),
            shown,
        ]
        .into_iter()
        .flatten()
        .flatten()
        {
            for (key, name, _) in &spec.names {
                let entry = seen.entry(key.clone()).or_insert_with(|| Seen {
                    names: Vec::new(),
                    sorts: Vec::new(),
                    first: active.seq,
                });
                entry.names.push(name.clone());
                entry.sorts.extend(spec.sort.clone());
            }
        }
    }
    // Build lookups from durable `artist_key` ownership rows, including rows
    // whose artist currently redirects to another after a merge.
    let mut mbid_owner: HashMap<&str, i64> = HashMap::new();
    let mut name_owner_current: HashMap<&str, i64> = HashMap::new();
    let mut old_name_owners: BTreeMap<i64, HashMap<&str, VecDeque<i64>>> = BTreeMap::new();
    for entry in &library.artist_keys {
        match entry.kind {
            ArtistKeyKind::Mbid => {
                mbid_owner
                    .entry(entry.value.as_str())
                    .or_insert(entry.artist);
            }
            ArtistKeyKind::Name => {
                let stale_version = entry
                    .normalizer_version
                    .filter(|&version| version != NORMALIZER_VERSION)
                    .or(library.artist_keys_by);
                match stale_version {
                    Some(version) => {
                        old_name_owners
                            .entry(version)
                            .or_default()
                            .entry(entry.value.as_str())
                            .or_default()
                            .push_back(entry.artist);
                    }
                    None => {
                        name_owner_current
                            .entry(entry.value.as_str())
                            .or_insert(entry.artist);
                    }
                }
            }
        }
    }

    // A name that already has a durable owner in `artist_key` keeps that owner
    // across rescans, merges, unmerges, and retags rather than being folded
    // into another artist by `credited_under` or `by_sort` (unless
    // `credited_under` maps to an MBID owned by that exact same artist row).
    let mut protected_owners: HashMap<&ArtistKey, i64> = HashMap::new();
    for (key, entry) in &seen {
        let ArtistKey::Name(name) = key else {
            continue;
        };
        if let Some(&row) = name_owner_current.get(name.as_str()) {
            protected_owners.insert(key, row);
            continue;
        }
        if !old_name_owners.is_empty() {
            'outer: for (&ver, map) in &old_name_owners {
                for spelling in &entry.names {
                    if let Some(old) = normalize_by(ver, spelling)
                        && let Some(rows) = map.get(old.as_str())
                        && let Some(&row) = rows.front()
                    {
                        protected_owners.insert(key, row);
                        break 'outer;
                    }
                }
            }
        }
    }
    let protected_owner = |key: &ArtistKey| -> Option<i64> { protected_owners.get(key).copied() };

    // A name with no ID is the artist an ID is credited under that name:
    // the most credited of them, when there are several.
    let mut under: HashMap<String, Vec<(usize, &str)>> = HashMap::new();
    for (key, entry) in &seen {
        if let ArtistKey::Mbid(mbid) = key {
            let names: HashSet<String> = entry.names.iter().map(|name| normalize(name)).collect();
            for name in names {
                under
                    .entry(name)
                    .or_default()
                    .push((entry.names.len(), mbid.as_str()));
            }
        }
    }
    let credited_under = |key: &ArtistKey| -> Option<ArtistKey> {
        if let ArtistKey::Name(name) = key
            && let Some(ids) = under.get(name)
            && let Some((_, mbid)) = ids.iter().max_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(a.1)))
        {
            if let Some(protected) = protected_owner(key)
                && mbid_owner.get(*mbid) != Some(&protected)
            {
                return None;
            }
            return Some(ArtistKey::Mbid((*mbid).to_owned()));
        }
        None
    };
    // A name with no ID that is written as one artist's sort name, and as
    // no other's, is that artist: "Vane, Iris" where Iris Vane's files claim
    // that sort name. A sort name that is the artist's own name, as for most
    // bands, says nothing, and so does one that is not the name's own words
    // reordered, such as "Dream" for The Dream. The artists that claim it
    // are counted once each, whether their files name them by ID or by name
    // alone.
    let words = |key: &str| -> Vec<String> {
        let mut words: Vec<String> = key.split(' ').map(str::to_owned).collect();
        words.sort();
        words
    };
    let mut sorted_as: HashMap<String, BTreeSet<ArtistKey>> = HashMap::new();
    for (key, entry) in &seen {
        let names: HashSet<String> = entry.names.iter().map(|name| normalize(name)).collect();
        let claimant = credited_under(key).unwrap_or_else(|| key.clone());
        for sort in &entry.sorts {
            let sort = normalize(sort);
            let reordered = names.iter().any(|name| words(name) == words(&sort));
            if !names.contains(&sort) && reordered {
                sorted_as.entry(sort).or_default().insert(claimant.clone());
            }
        }
    }
    // Only a name a track is credited to, whose own files claim no sort
    // name, is read as another artist's sort name: one whose files say how
    // it sorts speaks for itself.
    let by_sort = |key: &ArtistKey| -> Option<ArtistKey> {
        let ArtistKey::Name(name) = key else {
            return None;
        };
        if protected_owner(key).is_some()
            || !on_tracks.contains(key)
            || seen.get(key).is_some_and(|entry| !entry.sorts.is_empty())
        {
            return None;
        }
        match sorted_as.get(name)?.iter().collect::<Vec<_>>().as_slice() {
            [one] => Some((*one).clone()),
            _ => None,
        }
    };
    let stands_for = |key: &ArtistKey| -> ArtistKey {
        credited_under(key)
            .or_else(|| by_sort(key))
            .unwrap_or_else(|| key.clone())
    };
    let mut merged: HashMap<ArtistKey, Seen> = HashMap::new();
    let mut ordered: Vec<(&ArtistKey, &Seen)> = seen.iter().collect();
    ordered.sort_by(|a, b| a.1.first.cmp(&b.1.first).then(a.0.cmp(b.0)));
    for (key, entry) in ordered {
        let merged = merged.entry(stands_for(key)).or_insert_with(|| Seen {
            names: Vec::new(),
            sorts: Vec::new(),
            first: entry.first,
        });
        merged.first = merged.first.min(entry.first);
        // The names an ID is credited under come first: they are how its
        // own tags spell it. A sort name written as a name is credited as
        // written, and is not a name the artist is shown by.
        let sort_written = credited_under(key).is_none() && by_sort(key).is_some();
        if matches!(key, ArtistKey::Mbid(_)) {
            merged.names.splice(0..0, entry.names.iter().cloned());
        } else if !sort_written {
            merged.names.extend(entry.names.iter().cloned());
        }
        merged.sorts.extend(entry.sorts.iter().cloned());
    }
    let mut artist_keys: Vec<(&ArtistKey, &Seen)> = merged.iter().collect();
    artist_keys.sort_by(|a, b| a.1.first.cmp(&b.1.first).then(a.0.cmp(b.0)));

    // After the normaliser changed, stored name keys hold what the earlier
    // version made of their files' names, and an artist with no ID is found
    // by those alone: today's key of one artist may be another's old one.
    // First each takes the row of a spelling whose key did not change, then
    // the others take rows by the old key most of their files give.
    let mut migrated: HashMap<&ArtistKey, i64> = HashMap::new();
    if !old_name_owners.is_empty() {
        let unnamed: Vec<(&ArtistKey, &Seen)> = artist_keys
            .iter()
            .copied()
            .filter(|(key, _)| {
                matches!(key, ArtistKey::Name(own) if !name_owner_current.contains_key(own.as_str()))
            })
            .collect();
        for (&version, old_rows) in &mut old_name_owners {
            for (key, entry) in &unnamed {
                if migrated.contains_key(key) {
                    continue;
                }
                let ArtistKey::Name(own) = key else { continue };
                let unchanged = entry.names.iter().any(|spelling| {
                    normalize_by(version, spelling).as_deref() == Some(own.as_str())
                });
                if let Some(row) = unchanged
                    .then(|| old_rows.get_mut(own.as_str()).and_then(VecDeque::pop_front))
                    .flatten()
                {
                    migrated.insert(key, row);
                }
            }
            for (key, entry) in &unnamed {
                if migrated.contains_key(key) {
                    continue;
                }
                let mut votes: Vec<(String, usize, usize)> = Vec::new();
                for (at, spelling) in entry.names.iter().enumerate() {
                    let Some(old) = normalize_by(version, spelling) else {
                        continue;
                    };
                    match votes.iter_mut().find(|(key, _, _)| *key == old) {
                        Some(vote) => vote.1 += 1,
                        None => votes.push((old, 1, at)),
                    }
                }
                votes.sort_by(|a, b| b.1.cmp(&a.1).then(a.2.cmp(&b.2)));
                if let Some(row) = votes.iter().find_map(|(old, _, _)| {
                    old_rows.get_mut(old.as_str()).and_then(VecDeque::pop_front)
                }) {
                    migrated.insert(key, row);
                }
            }
        }
    }

    let mut artists: Vec<ArtistOut> = Vec::new();
    let mut artist_index: HashMap<ArtistKey, usize> = HashMap::new();
    let mut row_to_index: HashMap<i64, usize> = HashMap::new();
    let mut redirected_claims: BTreeMap<i64, (Vec<String>, Vec<String>)> = BTreeMap::new();
    for (key, entry) in artist_keys {
        let Some(name) = pick(entry.names.iter().cloned()) else {
            continue;
        };
        // An artist with no ID is found by its own key, whichever of its
        // names is shown.
        let match_name = match key {
            ArtistKey::Name(own) => own.clone(),
            ArtistKey::Mbid(_) => normalize(&name),
        };
        let (row, mbid, durable_key) = match key {
            ArtistKey::Mbid(mbid) => (
                mbid_owner.get(mbid.as_str()).copied(),
                Some(mbid.clone()),
                (ArtistKeyKind::Mbid, mbid.clone()),
            ),
            ArtistKey::Name(own) => {
                let row = name_owner_current
                    .get(own.as_str())
                    .copied()
                    .or_else(|| migrated.get(key).copied());
                (row, None, (ArtistKeyKind::Name, own.clone()))
            }
        };
        if let Some(row_id) = row
            && let Some(&canon) = library.artist_canonical.get(&row_id)
        {
            let claims = redirected_claims.entry(canon).or_default();
            claims.0.extend(entry.names.iter().cloned());
            claims.1.extend(entry.sorts.iter().cloned());
        }
        if let Some(row_id) = row
            && let Some(&existing_at) = row_to_index.get(&row_id)
        {
            artist_index.insert(key.clone(), existing_at);
            continue;
        }
        let r = row.map_or(Ref::New(artists.len()), Ref::Row);
        let name = edit(Kind::Artist, r, "name").unwrap_or(name);
        let at = artists.len();
        artist_index.insert(key.clone(), at);
        if let Some(row_id) = row {
            row_to_index.insert(row_id, at);
        }
        artists.push(ArtistOut {
            r,
            sort_name: pick(entry.sorts.iter().cloned()).unwrap_or_else(|| name.clone()),
            name,
            match_name,
            mbid,
            key: Some(durable_key),
        });
    }
    // If a canonical merge target has no direct files of its own, still
    // project its display from its edit or the claims of rows redirecting to
    // it so a display rebuild never leaves it blank.
    for (canon, (names, sorts)) in redirected_claims {
        if row_to_index.contains_key(&canon) {
            continue;
        }
        let r = Ref::Row(canon);
        let Some(name) = edit(Kind::Artist, r, "name").or_else(|| pick(names)) else {
            continue;
        };
        let sort_name = pick(sorts).unwrap_or_else(|| name.clone());
        let match_name = normalize(&name);
        artists.push(ArtistOut {
            r,
            name,
            sort_name,
            match_name,
            mbid: None,
            key: None,
        });
    }

    // Credits: values, the same wherever the same artists are credited the
    // same way.
    type Written<'a> = (&'a str, Vec<(i64, &'a str, &'a str)>);
    let mut existing: HashMap<Written<'_>, i64> = HashMap::new();
    for credit in library.credits.iter().rev() {
        let names = credit
            .names
            .iter()
            .map(|(row, name, join)| (*row, name.as_str(), join.as_str()));
        existing.insert((credit.text.as_str(), names.collect()), credit.id);
    }
    let mut credits: Vec<CreditOut> = Vec::new();
    let mut credit_index: HashMap<CreditSpec, Option<usize>> = HashMap::new();
    let mut credit_of = |spec: &CreditSpec| -> Option<usize> {
        if let Some(at) = credit_index.get(spec) {
            return *at;
        }
        let names: Option<Vec<(usize, String, String)>> = spec
            .names
            .iter()
            .map(|(key, name, join)| {
                let at = artist_index.get(&stands_for(key))?;
                Some((*at, name.clone(), join.clone()))
            })
            .collect();
        let at = names.map(|names| {
            let rows: Option<Vec<(i64, &str, &str)>> = names
                .iter()
                .map(|(at, name, join)| match artists.get(*at)?.r {
                    Ref::Row(row) => Some((row, name.as_str(), join.as_str())),
                    Ref::New(_) => None,
                })
                .collect();
            let row = rows.and_then(|rows| existing.get(&(spec.text.as_str(), rows)).copied());
            // The sort name is the artist's, so two specs that differ only
            // in it are one credit.
            let same = credits
                .iter()
                .position(|credit: &CreditOut| credit.text == spec.text && credit.names == names);
            same.unwrap_or_else(|| {
                credits.push(CreditOut {
                    r: row.map_or(Ref::New(credits.len()), Ref::Row),
                    text: spec.text.clone(),
                    names,
                });
                credits.len() - 1
            })
        });
        credit_index.insert(spec.clone(), at);
        at
    };

    // What each entity shows: the most common value among its items'
    // highest-ranked claims, then the one from the oldest item.
    let title_of = |facts: &Facts| facts.title.clone().or_else(|| facts.title_inferred.clone());
    let from_tags = |members: &[&Active<'_>]| -> Vec<String> {
        let tagged: Vec<String> = members
            .iter()
            .filter_map(|active| active.item.facts.title.clone())
            .collect();
        if tagged.is_empty() {
            members
                .iter()
                .filter_map(|active| title_of(&active.item.facts))
                .collect()
        } else {
            tagged
        }
    };
    let mut by_recording: BTreeMap<usize, Vec<&Active<'_>>> = BTreeMap::new();
    for active in &actives {
        by_recording.entry(active.rec).or_default().push(active);
    }
    for (rec, members) in &by_recording {
        let credit = pick(
            members
                .iter()
                .filter_map(|active| active.track_credit.clone()),
        )
        .and_then(|spec| credit_of(&spec));
        if let Some(recording) = recordings.get_mut(*rec) {
            recording.title =
                edit(Kind::Recording, recording.r, "title").or_else(|| pick(from_tags(members)));
            recording.credit = credit;
            recording.projected = true;
        }
    }

    let mut appearances: Vec<AppearanceOut> = Vec::new();
    let mut emitted_appearances: HashSet<i64> = HashSet::new();
    let mut redirected_appearances: BTreeMap<i64, Vec<&Active<'_>>> = BTreeMap::new();
    for (r, members) in &appearance_rows {
        let members: Vec<&Active<'_>> = members
            .iter()
            .filter_map(|index| actives.get(*index))
            .collect();
        let Some(first) = members.first() else {
            continue;
        };
        if let Ref::Row(row) = *r {
            emitted_appearances.insert(row);
            if let Some(&canon) = library.appearance_canonical.get(&row) {
                redirected_appearances
                    .entry(canon)
                    .or_default()
                    .extend(members.iter().copied());
            }
        }
        let credit = pick(
            members
                .iter()
                .filter_map(|active| active.track_credit.clone()),
        )
        .and_then(|spec| credit_of(&spec));
        appearances.push(AppearanceOut {
            r: *r,
            recording: first.assign,
            release: first.release,
            title: edit(Kind::TrackAppearance, *r, "title").or_else(|| pick(from_tags(&members))),
            credit,
            disc: pick(members.iter().filter_map(|active| active.item.facts.disc)),
            track: pick(members.iter().filter_map(|active| active.item.facts.track)),
            duration_ms: pick(
                members
                    .iter()
                    .filter_map(|active| active.item.facts.duration_ms),
            ),
            added_at_ms: members.iter().map(|active| active.item.first_seen_ms).min(),
        });
    }
    // If a canonical merged track appearance has no direct live assignment of
    // its own, still project its display from its edits and the claims of
    // appearances redirecting into it so playback and lists stay up to date.
    for (canon, members) in redirected_appearances {
        if emitted_appearances.contains(&canon) {
            continue;
        }
        let Some(first) = members.first() else {
            continue;
        };
        let credit = pick(
            members
                .iter()
                .filter_map(|active| active.track_credit.clone()),
        )
        .and_then(|spec| credit_of(&spec));
        let r = Ref::Row(canon);
        appearances.push(AppearanceOut {
            r,
            recording: first.assign,
            release: first.release,
            title: edit(Kind::TrackAppearance, r, "title").or_else(|| pick(from_tags(&members))),
            credit,
            disc: pick(members.iter().filter_map(|active| active.item.facts.disc)),
            track: pick(members.iter().filter_map(|active| active.item.facts.track)),
            duration_ms: pick(
                members
                    .iter()
                    .filter_map(|active| active.item.facts.duration_ms),
            ),
            added_at_ms: members.iter().map(|active| active.item.first_seen_ms).min(),
        });
    }

    struct Formed<'a> {
        members: Vec<&'a Active<'a>>,
        recordings: HashSet<usize>,
        group_mbid: Option<String>,
        family: (String, String),
        /// The credit its claims give, while it shows Unknown Artist.
        claimed: Option<CreditSpec>,
        lossless: bool,
        tracks: usize,
        first: usize,
    }
    let mut releases: Vec<ReleaseOut> = Vec::new();
    let mut formed: Vec<Formed<'_>> = Vec::new();
    for release in &forming {
        let members: Vec<&Active<'_>> = release
            .members
            .iter()
            .filter_map(|index| actives.get(*index))
            .collect();
        let spec = pick(
            members
                .iter()
                .filter_map(|active| active.release_credit.clone()),
        );
        let shown = pick(
            members
                .iter()
                .filter_map(|active| active.shown_credit.clone()),
        );
        let title = edit(Kind::Release, release.r, "album").or_else(|| {
            pick(
                members
                    .iter()
                    .filter_map(|active| active.item.facts.album.clone()),
            )
        });
        let (date, year) = dated(edit(Kind::Release, release.r, "date").or_else(|| {
            pick(members.iter().filter_map(|active| {
                active
                    .item
                    .facts
                    .date
                    .as_deref()
                    .and_then(parse_date)
                    .map(|(date, _)| date)
            }))
        }));
        formed.push(Formed {
            recordings: members.iter().map(|active| active.rec).collect(),
            group_mbid: pick(
                members
                    .iter()
                    .filter_map(|active| active.item.facts.release_group_mbid.clone()),
            ),
            family: (
                spec.as_ref()
                    .map(|spec| normalize(&spec.text))
                    .unwrap_or_default(),
                title.as_deref().map(core).unwrap_or_default(),
            ),
            claimed: spec.clone().filter(|spec| Some(spec) != shown.as_ref()),
            lossless: members.iter().all(|active| active.item.lossless),
            tracks: members
                .iter()
                .filter_map(|active| active.appearance)
                .collect::<HashSet<_>>()
                .len(),
            first: members
                .iter()
                .map(|active| active.seq)
                .min()
                .unwrap_or(usize::MAX),
            members: members.clone(),
        });
        releases.push(ReleaseOut {
            r: release.r,
            group: 0,
            title,
            credit: shown.and_then(|spec| credit_of(&spec)),
            date,
            year,
            picture: pick(members.iter().filter_map(|active| active.item.cover)),
        });
    }

    // Release groups: the owner's choice, then a shared release-group
    // MBID, then the same album artist and title with at least half of the
    // smaller release's recordings shared. Automatic regrouping never
    // collapses two distinct groups that both carry owner input (an active
    // edit, an explicit presentation, or a preferred release).
    let chosen = |release: &ReleaseOut| match release.r {
        Ref::Row(row) => library
            .releases
            .get(&row)
            .and_then(|release| release.chosen_group),
        Ref::New(_) => None,
    };
    let mut parents: Vec<usize> = (0..releases.len()).collect();
    let mut comp_owner_group: Vec<Option<i64>> = releases
        .iter()
        .map(|release| match release.r {
            Ref::Row(row) => library
                .releases
                .get(&row)
                .map(|existing| existing.group)
                .filter(|group| owner_groups.contains(group)),
            Ref::New(_) => None,
        })
        .collect();
    let safe_union = |parents: &mut [usize], comp_owner: &mut [Option<i64>], a: usize, b: usize| {
        let (ra, rb) = (find(parents, a), find(parents, b));
        if ra == rb {
            return;
        }
        let (oa, ob) = (
            comp_owner.get(ra).copied().flatten(),
            comp_owner.get(rb).copied().flatten(),
        );
        if let (Some(g1), Some(g2)) = (oa, ob)
            && g1 != g2
        {
            return;
        }
        let root = ra.min(rb);
        union(parents, ra, rb);
        if let Some(slot) = comp_owner.get_mut(root) {
            *slot = oa.or(ob);
        }
    };
    let mut by_mbid: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    let mut families: BTreeMap<&(String, String), Vec<usize>> = BTreeMap::new();
    for (index, (release, formed)) in releases.iter().zip(&formed).enumerate() {
        if chosen(release).is_some() {
            continue;
        }
        if let Some(mbid) = &formed.group_mbid {
            by_mbid.entry(mbid.as_str()).or_default().push(index);
        }
        families.entry(&formed.family).or_default().push(index);
    }
    // First unite releases sharing an MBID that already belong to the same
    // owner-edited group, then unite remaining pairs in deterministic order.
    for members in by_mbid.values() {
        for (at, a) in members.iter().enumerate() {
            for b in members.iter().skip(at + 1) {
                let oa = comp_owner_group
                    .get(find(&mut parents, *a))
                    .copied()
                    .flatten();
                let ob = comp_owner_group
                    .get(find(&mut parents, *b))
                    .copied()
                    .flatten();
                if oa.is_some() && oa == ob {
                    safe_union(&mut parents, &mut comp_owner_group, *a, *b);
                }
            }
        }
        for (at, a) in members.iter().enumerate() {
            for b in members.iter().skip(at + 1) {
                safe_union(&mut parents, &mut comp_owner_group, *a, *b);
            }
        }
    }
    for family in families.values() {
        for pass in [true, false] {
            for (at, a) in family.iter().enumerate() {
                for b in family.iter().skip(at + 1) {
                    let (Some(mine), Some(theirs)) = (formed.get(*a), formed.get(*b)) else {
                        continue;
                    };
                    let both = mine.recordings.intersection(&theirs.recordings).count();
                    let smaller = mine.recordings.len().min(theirs.recordings.len());
                    if smaller > 0 && both * 2 >= smaller {
                        if pass {
                            let oa = comp_owner_group
                                .get(find(&mut parents, *a))
                                .copied()
                                .flatten();
                            let ob = comp_owner_group
                                .get(find(&mut parents, *b))
                                .copied()
                                .flatten();
                            if oa.is_some() && oa == ob {
                                safe_union(&mut parents, &mut comp_owner_group, *a, *b);
                            }
                        } else {
                            safe_union(&mut parents, &mut comp_owner_group, *a, *b);
                        }
                    }
                }
            }
        }
    }
    let mut components: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    let mut fixed: BTreeMap<i64, Vec<usize>> = BTreeMap::new();
    for (index, release) in releases.iter().enumerate() {
        match chosen(release) {
            Some(group) => fixed.entry(group).or_default().push(index),
            None => components
                .entry(find(&mut parents, index))
                .or_default()
                .push(index),
        }
    }
    let components: Vec<Vec<usize>> = components.into_values().collect();
    let mut preferred_group_hits: HashSet<(i64, usize)> = HashSet::new();
    let votes = components
        .iter()
        .enumerate()
        .map(|(comp_idx, members)| {
            let key = members
                .iter()
                .filter_map(|index| forming.get(*index))
                .map(|release| release.key.as_str())
                .min()
                .unwrap_or("")
                .to_owned();
            let mut rows = Vec::new();
            for index in members {
                if let Some(Ref::Row(rel_row)) = releases.get(*index).map(|release| release.r)
                    && let Some(existing) = library.releases.get(&rel_row)
                {
                    let group = existing.group;
                    if library.groups.get(&group).and_then(|g| g.preferred_release) == Some(rel_row)
                    {
                        preferred_group_hits.insert((group, comp_idx));
                    }
                    rows.push(group);
                }
            }
            (key, rows)
        })
        .collect();
    let mut grouped: Vec<(Ref, Vec<usize>)> = Vec::new();
    let mut group_at: HashMap<i64, usize> = HashMap::new();
    for (members, row) in
        components
            .into_iter()
            .zip(keep_rows(votes, &owner_groups, &preferred_group_hits))
    {
        match row {
            Some(row) => {
                group_at.insert(row, grouped.len());
                grouped.push((Ref::Row(row), members));
            }
            None => grouped.push((Ref::New(grouped.len()), members)),
        }
    }
    for (row, members) in fixed {
        match group_at.get(&row).and_then(|at| grouped.get_mut(*at)) {
            Some((_, group)) => group.extend(members),
            None => grouped.push((Ref::Row(row), members)),
        }
    }
    let mut groups: Vec<GroupOut> = Vec::new();
    for (at, (r, members)) in grouped.iter().enumerate() {
        let mut ordered: Vec<usize> = members.clone();
        ordered.sort_unstable();
        let files = || {
            ordered
                .iter()
                .filter_map(|index| formed.get(*index))
                .flat_map(|formed| formed.members.iter())
                .map(|active| &active.item.facts)
        };
        let typed = || {
            files()
                .filter(|facts| facts.primary_type.is_some() || !facts.secondary_types.is_empty())
        };
        let primary_type = edit(Kind::ReleaseGroup, *r, "primary_type")
            .or_else(|| pick(files().filter_map(|facts| facts.primary_type.clone())));
        let secondary_types =
            pick(typed().map(|facts| facts.secondary_types.clone())).unwrap_or_default();
        // The year it first came out: an original-date claim, or else the
        // earliest date among its releases.
        let original = edit(Kind::ReleaseGroup, *r, "original_date").or_else(|| {
            pick(files().filter_map(|facts| {
                facts
                    .original_date
                    .as_deref()
                    .and_then(parse_date)
                    .map(|(date, _)| date)
            }))
        });
        let earliest = ordered
            .iter()
            .filter_map(|index| releases.get(*index)?.date.clone())
            .min();
        let (first_date, first_year) = dated(original.or(earliest));
        // The release a collapsed tile shows: the owner's, or else lossless
        // before lossy, then the most tracks, then the earliest date.
        let preferred = match r {
            Ref::Row(row) => library.groups.get(row).and_then(|g| g.preferred_release),
            Ref::New(_) => None,
        };
        let display_release = ordered
            .iter()
            .find(|index| {
                preferred.is_some()
                    && releases.get(**index).map(|release| release.r) == preferred.map(Ref::Row)
            })
            .or_else(|| {
                ordered.iter().min_by_key(|index| {
                    let formed = formed.get(**index);
                    let date = releases
                        .get(**index)
                        .and_then(|release| release.date.clone());
                    (
                        !formed.is_some_and(|formed| formed.lossless),
                        std::cmp::Reverse(formed.map_or(0, |formed| formed.tracks)),
                        date.is_none(),
                        date,
                        formed.map_or(usize::MAX, |formed| formed.first),
                    )
                })
            })
            .copied();
        for index in members {
            if let Some(release) = releases.get_mut(*index) {
                release.group = at;
            }
        }
        groups.push(GroupOut {
            r: *r,
            display_release,
            primary_type,
            secondary_types,
            first_date,
            first_year,
        });
    }

    // A record shows one artist on every version of it: a version that
    // names no artist shows Unknown Artist only when no other version of
    // its group names one, and otherwise what its claims give.
    let named: HashSet<usize> = releases
        .iter()
        .zip(&formed)
        .filter(|(_, formed)| formed.claimed.is_none())
        .map(|(release, _)| release.group)
        .collect();
    for (release, formed) in releases.iter_mut().zip(&formed) {
        if let Some(claimed) = &formed.claimed
            && named.contains(&release.group)
        {
            release.credit = credit_of(claimed);
        }
    }

    // Each item's outcome, beside what it has now.
    let mut placed: HashMap<usize, &Active<'_>> = HashMap::new();
    for active in &actives {
        placed.insert(active.seq, active);
    }
    let mut items = Vec::with_capacity(library.items.len());
    for (seq, (item, bound)) in library.items.iter().zip(folded.items).enumerate() {
        let current = item.current;
        let (recording, appearance) = match placed.get(&seq) {
            Some(active) => {
                let appearance = active
                    .appearance
                    .and_then(|at| appearance_rows.get(at))
                    .map(|(r, _)| *r);
                (Some(active.assign), appearance)
            }
            // Held where it is, whatever that is.
            None => (
                current.map(|current| Ref::Row(current.recording)),
                current.and_then(|current| current.appearance).map(Ref::Row),
            ),
        };
        // A decision that would mark the item for review otherwise than
        // the one in force is written, even where the binding stays.
        // So is one whose binding across albums by an all but equal length
        // the one in force does not mark, or marks wrongly.
        let review_changed = match (&bound.decision, current) {
            (Some(decision), Some(current)) => {
                decision.needs_review != current.needs_review
                    || decision.evidence.across_albums.is_some() != current.across_albums
            }
            _ => false,
        };
        let changed = !bound.frozen
            && (bound.rec_changed
                || review_changed
                || appearance != current.and_then(|current| current.appearance).map(Ref::Row));
        let mut decision = bound.decision;
        if let (Some(decision), Some(rec)) = (decision.as_mut(), bound.withheld)
            && let Some(withheld) = decision.evidence.withheld.as_mut()
        {
            withheld.recording = recordings.get(rec).map(|out| out.r);
        }
        items.push(ItemOut {
            item: item.id,
            recording,
            appearance,
            decision,
            changed,
            pinned: bound.pinned,
            blocked: bound.blocked,
        });
    }
    Resolution {
        items,
        artists,
        credits,
        recordings,
        groups,
        releases,
        appearances,
    }
}
