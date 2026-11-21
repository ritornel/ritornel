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
pub const CATALOG_VERSION: i64 = 1;

use crate::extract::{Facts, parse_date};
use crate::library::{
    AppearanceOut, ArtistOut, CreditOut, GroupOut, Item, ItemOut, Kind, Library, RecordingOut, Ref,
    ReleaseOut, Resolution,
};
use crate::normalize::{core, normalize, normalize_by};
use crate::resolve::{Policy, RecKey, fold};

/// The name of the album artist of an album whose tracks have different
/// artists and no album artist tag.
const VARIOUS: &str = "Various Artists";

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
    release_credit: Option<CreditSpec>,
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
/// each row to at most one. `votes` is, for each thing, the rows its
/// members name. The rest are new.
fn keep_rows(votes: Vec<(String, Vec<i64>)>) -> Vec<Option<i64>> {
    let mut ballots: Vec<(usize, i64, usize)> = Vec::new();
    for (index, (_, rows)) in votes.iter().enumerate() {
        let mut counts: BTreeMap<i64, usize> = BTreeMap::new();
        for row in rows {
            *counts.entry(*row).or_default() += 1;
        }
        ballots.extend(counts.into_iter().map(|(row, count)| (count, row, index)));
    }
    ballots.sort_by(|a, b| {
        let key = |index: usize| votes.get(index).map(|(key, _)| key.as_str());
        b.0.cmp(&a.0)
            .then_with(|| key(a.2).cmp(&key(b.2)))
            .then(a.1.cmp(&b.1))
    });
    let mut kept: Vec<Option<i64>> = vec![None; votes.len()];
    let mut taken: HashSet<i64> = HashSet::new();
    for (_, row, index) in ballots {
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
    for (release, row) in forming.iter_mut().zip(keep_rows(votes)) {
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
    for (index, release) in forming.iter_mut().enumerate() {
        if release.r == Ref::New(usize::MAX) {
            release.r = Ref::New(index);
        }
        for member in &release.members {
            if let Some(active) = actives.get_mut(*member) {
                active.release = Some(index);
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
        for spec in [&active.track_credit, &active.release_credit]
            .into_iter()
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
            return Some(ArtistKey::Mbid((*mbid).to_owned()));
        }
        None
    };
    // A name with no ID that is written as one artist's sort name, and as
    // no other's, is that artist: "Vane, Iris" where Iris Vane's files claim
    // that sort name. A sort name that is the artist's own name, as for most
    // bands, says nothing. The artists that claim it are counted once each,
    // whether their files name them by ID or by name alone.
    let mut sorted_as: HashMap<String, BTreeSet<ArtistKey>> = HashMap::new();
    for (key, entry) in &seen {
        let names: HashSet<String> = entry.names.iter().map(|name| normalize(name)).collect();
        let claimant = credited_under(key).unwrap_or_else(|| key.clone());
        for sort in &entry.sorts {
            let sort = normalize(sort);
            if !names.contains(&sort) {
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
        if !on_tracks.contains(key) || seen.get(key).is_some_and(|entry| !entry.sorts.is_empty()) {
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
    // An artist with an ID keeps the row that has its ID. One with no ID
    // takes a row by its normalised name, among the rows with no ID: so
    // two namesakes never swap rows, and a renamed artist keeps its own.
    let mut id_rows: HashMap<&str, VecDeque<i64>> = HashMap::new();
    let mut free_rows: HashMap<&str, VecDeque<i64>> = HashMap::new();
    for (row, match_name) in &library.artists {
        match library.artist_mbids.get(row) {
            Some(mbid) => id_rows.entry(mbid.as_str()).or_default().push_back(*row),
            // A row given no key is found by no name.
            None if match_name.is_empty() => {}
            None => free_rows
                .entry(match_name.as_str())
                .or_default()
                .push_back(*row),
        }
    }
    // After the normaliser changed, the rows hold the keys the earlier
    // version made of their files' names, and an artist with no ID is
    // found by those alone: today's key of one artist may be another's old
    // one. First each takes the row of a spelling whose key did not
    // change, then the others take rows by the old key most of their files
    // give.
    let mut migrated: HashMap<&ArtistKey, i64> = HashMap::new();
    if let Some(version) = library.artist_keys_by {
        let unnamed: Vec<(&ArtistKey, &Seen)> = artist_keys
            .iter()
            .copied()
            .filter(|(key, _)| matches!(key, ArtistKey::Name(_)))
            .collect();
        for (key, entry) in &unnamed {
            let ArtistKey::Name(own) = key else { continue };
            let unchanged = entry
                .names
                .iter()
                .any(|spelling| normalize_by(version, spelling).as_deref() == Some(own.as_str()));
            if let Some(row) = unchanged
                .then(|| {
                    free_rows
                        .get_mut(own.as_str())
                        .and_then(VecDeque::pop_front)
                })
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
                free_rows
                    .get_mut(old.as_str())
                    .and_then(VecDeque::pop_front)
            }) {
                migrated.insert(key, row);
            }
        }
    }
    let mut artists: Vec<ArtistOut> = Vec::new();
    let mut artist_index: HashMap<ArtistKey, usize> = HashMap::new();
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
        let (row, mbid) = match key {
            ArtistKey::Mbid(mbid) => (
                id_rows.get_mut(mbid.as_str()).and_then(VecDeque::pop_front),
                Some(mbid.clone()),
            ),
            ArtistKey::Name(_) => {
                let row = match library.artist_keys_by {
                    Some(_) => migrated.get(key).copied(),
                    None => free_rows
                        .get_mut(match_name.as_str())
                        .and_then(VecDeque::pop_front),
                };
                (row, None)
            }
        };
        let r = row.map_or(Ref::New(artists.len()), Ref::Row);
        let name = edit(Kind::Artist, r, "name").unwrap_or(name);
        artist_index.insert(key.clone(), artists.len());
        artists.push(ArtistOut {
            r,
            sort_name: pick(entry.sorts.iter().cloned()).unwrap_or_else(|| name.clone()),
            name,
            match_name,
            mbid,
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
    for (r, members) in &appearance_rows {
        let members: Vec<&Active<'_>> = members
            .iter()
            .filter_map(|index| actives.get(*index))
            .collect();
        let Some(first) = members.first() else {
            continue;
        };
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

    struct Formed<'a> {
        members: Vec<&'a Active<'a>>,
        recordings: HashSet<usize>,
        group_mbid: Option<String>,
        family: (String, String),
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
            credit: spec.and_then(|spec| credit_of(&spec)),
            date,
            year,
            picture: pick(members.iter().filter_map(|active| active.item.cover)),
        });
    }

    // Release groups: the owner's choice, then a shared release-group
    // MBID, then the same album artist and title with at least half of the
    // smaller release's recordings shared.
    let chosen = |release: &ReleaseOut| match release.r {
        Ref::Row(row) => library
            .releases
            .get(&row)
            .and_then(|release| release.chosen_group),
        Ref::New(_) => None,
    };
    let mut parents: Vec<usize> = (0..releases.len()).collect();
    let mut by_mbid: HashMap<&str, usize> = HashMap::new();
    let mut families: HashMap<&(String, String), Vec<usize>> = HashMap::new();
    for (index, (release, formed)) in releases.iter().zip(&formed).enumerate() {
        if chosen(release).is_some() {
            continue;
        }
        if let Some(mbid) = &formed.group_mbid {
            let other = *by_mbid.entry(mbid.as_str()).or_insert(index);
            union(&mut parents, other, index);
        }
        families.entry(&formed.family).or_default().push(index);
    }
    for family in families.values() {
        for (at, a) in family.iter().enumerate() {
            for b in family.iter().skip(at + 1) {
                let (Some(mine), Some(theirs)) = (formed.get(*a), formed.get(*b)) else {
                    continue;
                };
                let both = mine.recordings.intersection(&theirs.recordings).count();
                let smaller = mine.recordings.len().min(theirs.recordings.len());
                if smaller > 0 && both * 2 >= smaller {
                    union(&mut parents, *a, *b);
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
    let votes = components
        .iter()
        .map(|members| {
            let key = members
                .iter()
                .filter_map(|index| forming.get(*index))
                .map(|release| release.key.as_str())
                .min()
                .unwrap_or("")
                .to_owned();
            let rows = members
                .iter()
                .filter_map(|index| match releases.get(*index)?.r {
                    Ref::Row(row) => library.releases.get(&row).map(|release| release.group),
                    Ref::New(_) => None,
                })
                .collect();
            (key, rows)
        })
        .collect();
    let mut grouped: Vec<(Ref, Vec<usize>)> = Vec::new();
    let mut group_at: HashMap<i64, usize> = HashMap::new();
    for (members, row) in components.into_iter().zip(keep_rows(votes)) {
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
            Ref::Row(row) => library.groups.get(row).copied().flatten(),
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
