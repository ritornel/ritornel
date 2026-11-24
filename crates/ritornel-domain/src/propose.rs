//! Album proposals: what MusicBrainz's replies about a library's discs
//! propose each disc is, and the tag values a yes would put on its files.
//! See "Album proposals" in docs/internals/corrections.md.
//!
//! Proposals are derived, as plays are: a pure function of the disc
//! layouts, the current replies and the files' facts, versioned by
//! [`PROPOSER_VERSION`]. A proposal's ID is computed from its kind and its
//! anchor, the first-seen file it is about, so deriving again gives the
//! same proposal.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use sha1::{Digest, Sha1};

use crate::extract::{PRIMARY_TYPES, parse_date};
use crate::library::{Item, Library};
use crate::values::{Origin, Value};
use crate::{Facts, Policy};

/// The version of how proposals are worked out.
pub const PROPOSER_VERSION: u32 = 1;

/// One layout of one folder's files, as the proposer reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscLayout {
    /// Its disc ID.
    pub disc_id: String,
    /// Whether it may not be the disc's.
    pub provisional: bool,
    /// How the layouts of its disc ID were found, every copy of it in the
    /// library included, as a proposal shows them: each method with the
    /// sidecar revision it was read from.
    pub found: String,
    /// The source items it places, in the order of their positions. Empty
    /// for a layout whose positions fit no file, such as a drive's log of
    /// a rip whose files lost their gaps.
    pub items: Vec<i64>,
}

/// One folder's layouts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderDiscs {
    /// The folder, with its root, as [`Item::folder`] names it.
    pub folder: String,
    /// Its layouts.
    pub layouts: Vec<DiscLayout>,
}

/// What the current reply about a disc says of one release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyRelease {
    /// The reply it is read from, by the caller's key.
    pub reply: i64,
    /// The release's MusicBrainz ID.
    pub release: String,
    /// Its album: the release group's MusicBrainz ID.
    pub group: String,
    /// The album's title.
    pub group_title: Option<String>,
    /// Who the album is credited to, as written.
    pub artist: String,
    /// The album's kind, as MusicBrainz writes it.
    pub primary_type: Option<String>,
    /// When the album first came out.
    pub first_date: Option<String>,
    /// Which of the release's media the disc is, from 1.
    pub positions: Vec<u32>,
}

/// What a yes to a candidate would change, worked out cheaply. A preview
/// says exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Change {
    /// Only an ID.
    Knows,
    /// A kind or a first date as well.
    Gains,
    /// Files join or part: an album title, a credit or a disc number.
    Moves,
}

impl Change {
    /// The change as it is stored.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Knows => "knows",
            Self::Gains => "gains",
            Self::Moves => "moves",
        }
    }
}

/// One album a disc may be on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// The release group's MusicBrainz ID.
    pub release_group: String,
    /// Its title.
    pub title: Option<String>,
    /// Who it is credited to.
    pub artist: String,
    /// Its kind, as tags name it: `album`, `single`, `ep`.
    pub primary_type: Option<String>,
    /// When it first came out, as a date tag holds one.
    pub first_date: Option<String>,
    /// How many of the replies' releases are of it.
    pub releases: u32,
    /// What a yes would change.
    pub change: Change,
    /// The values a yes would put on the files.
    pub values: Vec<Value>,
    /// The files of the disc a yes joins this one onto, the album's first,
    /// when it joins it to another: what the yes puts together, and where
    /// it takes the album's title and credit from.
    pub onto: Vec<i64>,
}

/// What a proposal rests on: one layout of a disc and the reply about it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct RestsOn {
    /// The layout's disc ID.
    pub disc_id: String,
    /// The current reply about it, by the caller's key.
    pub reply: i64,
    /// How the layouts of the disc ID were found, as shown.
    pub found: String,
}

/// One proposal: that one disc of the library is one album.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proposal {
    /// Its ID, computed from its kind and its anchor.
    pub id: [u8; 16],
    /// Its kind: `album`.
    pub kind: &'static str,
    /// The first-seen file it is about.
    pub anchor: i64,
    /// The anchor of its group: the first-seen file among the discs whose
    /// likeliest candidate is the same album.
    pub group: i64,
    /// The folder, as [`Item::folder`] names it.
    pub folder: String,
    /// Its subject: the files of the disc.
    pub items: Vec<i64>,
    /// What it rests on: the disc's own layouts and replies, and those of
    /// the other discs its candidates' values were worked out with.
    pub rests_on: Vec<RestsOn>,
    /// Its candidates, the likeliest first.
    pub candidates: Vec<Candidate>,
    /// A digest of all of it: a yes names the fingerprint it was shown.
    pub fingerprint: String,
}

/// One release of an album, as a disc's replies name it: every position
/// any of them gives the disc on it.
struct Seen<'a> {
    release: &'a ReplyRelease,
    positions: BTreeSet<u32>,
}

/// One disc, as it is being proposed for.
struct Disc<'a> {
    folder: &'a str,
    items: Vec<i64>,
    rests_on: Vec<RestsOn>,
    /// Each album its replies name, with its releases.
    albums: BTreeMap<&'a str, Vec<Seen<'a>>>,
}

impl Disc<'_> {
    /// The medium the disc is of an album: the one position every release
    /// of it that holds the disc gives, if they all give one and the same.
    fn position(&self, group: &str) -> Option<u32> {
        let releases = self.albums.get(group)?;
        let one = |seen: &Seen<'_>| match seen.positions.iter().collect::<Vec<_>>()[..] {
            [only] => Some(*only),
            _ => None,
        };
        let first = one(releases.first()?)?;
        releases
            .iter()
            .all(|seen| one(seen) == Some(first))
            .then_some(first)
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// A kind as tags name it, if it is a primary kind.
fn kind(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    PRIMARY_TYPES.contains(&lower.as_str()).then_some(lower)
}

/// The discs of one folder: each set of files its layouts place, with the
/// layouts that place them. Layouts whose sets overlap are of one disc. A
/// layout that places no file belongs to the folder's one disc, if it has
/// only one; otherwise to none.
fn discs_of<'a>(
    folder: &'a FolderDiscs,
    replies: &'a HashMap<String, Vec<ReplyRelease>>,
    eligible: &HashMap<i64, &Item>,
) -> Vec<Disc<'a>> {
    let usable = folder
        .layouts
        .iter()
        .filter(|layout| !layout.provisional && replies.contains_key(&layout.disc_id));
    let mut sets: Vec<(BTreeSet<i64>, Vec<&DiscLayout>)> = Vec::new();
    let mut loose: Vec<&DiscLayout> = Vec::new();
    for layout in usable {
        let placed: BTreeSet<i64> = layout
            .items
            .iter()
            .copied()
            .filter(|item| eligible.contains_key(item))
            .collect();
        if placed.is_empty() {
            loose.push(layout);
            continue;
        }
        let mut joined = (placed, vec![layout]);
        sets.retain(|(items, layouts)| {
            if items.is_disjoint(&joined.0) {
                return true;
            }
            joined.0.extend(items.iter().copied());
            joined.1.extend(layouts.iter().copied());
            false
        });
        sets.push(joined);
    }
    if let [(_, layouts)] = sets.as_mut_slice() {
        layouts.extend(loose);
    }
    let first_seen = |id: &i64| {
        eligible
            .get(id)
            .map_or((i64::MAX, *id), |item| (item.first_seen_ms, *id))
    };
    sets.into_iter()
        .map(|(items, layouts)| {
            let mut items: Vec<i64> = items.into_iter().collect();
            items.sort_by_key(first_seen);
            let mut albums: BTreeMap<&str, Vec<Seen<'_>>> = BTreeMap::new();
            let mut rests_on = Vec::new();
            for layout in layouts {
                for release in replies.get(&layout.disc_id).into_iter().flatten() {
                    let seen = albums.entry(release.group.as_str()).or_default();
                    match seen
                        .iter_mut()
                        .find(|had| had.release.release == release.release)
                    {
                        Some(had) => had.positions.extend(release.positions.iter().copied()),
                        None => seen.push(Seen {
                            release,
                            positions: release.positions.iter().copied().collect(),
                        }),
                    }
                    let rest = RestsOn {
                        disc_id: layout.disc_id.clone(),
                        reply: release.reply,
                        found: layout.found.clone(),
                    };
                    if !rests_on.contains(&rest) {
                        rests_on.push(rest);
                    }
                }
            }
            Disc {
                folder: &folder.folder,
                items,
                rests_on,
                albums,
            }
        })
        .collect()
}

/// The proposals the layouts, the replies and the files give. `public_ids`
/// gives each source item's public ID, which a proposal's ID is computed
/// from. A file whose album was answered is in none.
pub fn propose(
    library: &Library,
    folders: &[FolderDiscs],
    replies: &HashMap<String, Vec<ReplyRelease>>,
    public_ids: &HashMap<i64, [u8; 16]>,
    policy: &Policy,
) -> Vec<Proposal> {
    // The files' facts as the catalog reads them, after the disc rules.
    let projected = crate::discs::apply(library, policy);
    let library = projected.as_ref().unwrap_or(library);
    let eligible: HashMap<i64, &Item> = library
        .items
        .iter()
        .filter(|item| !item.retired)
        .map(|item| (item.id, item))
        .collect();
    // The album an answer gave a file, if one did. Such a file is in no
    // album proposal: until it is undone, the answer is what it is. Its disc
    // is still one of that album's discs, which the others are joined to.
    let answered = |id: &i64| {
        eligible
            .get(id)
            .filter(|item| item.held.contains(&"mb_release_group"))
            .and_then(|item| item.facts.release_group_mbid.as_deref())
    };
    let first_seen = |id: &i64| {
        eligible
            .get(id)
            .map_or((i64::MAX, *id), |item| (item.first_seen_ms, *id))
    };
    // A file that names neither its album nor any artist would take the
    // album's title from a yes and no credit, and show under no artist
    // with songs named for their files: a disc with such a file is not
    // proposed for until its songs and credit can be answered too.
    let unnamed = |id: &i64| {
        eligible.get(id).is_some_and(|item| {
            item.facts.album.is_none()
                && item.facts.artist.is_empty()
                && item.facts.album_artist.is_empty()
        })
    };
    let discs: Vec<Disc<'_>> = folders
        .iter()
        .flat_map(|folder| discs_of(folder, replies, &eligible))
        .filter(|disc| !disc.albums.is_empty() && !disc.items.iter().any(unnamed))
        .collect();
    let mut proposals = Vec::new();
    for disc in &discs {
        let subject: Vec<i64> = disc
            .items
            .iter()
            .copied()
            .filter(|id| answered(id).is_none())
            .collect();
        let Some(anchor) = subject.first().copied() else {
            continue;
        };
        let mut rests_on = disc.rests_on.clone();
        let mut candidates: Vec<Candidate> = Vec::new();
        for (group, seen) in &disc.albums {
            let pick = |value: fn(&ReplyRelease) -> Option<&String>| {
                seen.iter().find_map(|seen| value(seen.release)).cloned()
            };
            let mut candidate = Candidate {
                release_group: (*group).to_owned(),
                title: pick(|release| release.group_title.as_ref()),
                artist: seen
                    .first()
                    .map(|seen| seen.release.artist.clone())
                    .unwrap_or_default(),
                primary_type: pick(|release| release.primary_type.as_ref())
                    .and_then(|text| kind(&text)),
                first_date: pick(|release| release.first_date.as_ref())
                    .and_then(|date| parse_date(&date).map(|(date, _)| date)),
                releases: u32::try_from(seen.len()).unwrap_or(u32::MAX),
                change: Change::Knows,
                values: Vec::new(),
                onto: Vec::new(),
            };
            // Another disc an answer gave an album is of that album alone.
            let cluster: Vec<&Disc<'_>> = discs
                .iter()
                .filter(|other| {
                    other.albums.contains_key(group)
                        && (std::ptr::eq(*other, disc)
                            || other
                                .items
                                .iter()
                                .filter_map(answered)
                                .all(|held| held == *group))
                })
                .collect();
            let (values, change, joined, onto) =
                values_for(disc, &subject, &cluster, &candidate, &eligible);
            candidate.onto = onto;
            for other in joined {
                for rest in &other.rests_on {
                    if !rests_on.contains(rest) {
                        rests_on.push(rest.clone());
                    }
                }
            }
            candidate.values = values;
            candidate.change = change;
            candidates.push(candidate);
        }
        candidates.sort_by(|a, b| {
            b.releases
                .cmp(&a.releases)
                .then_with(|| a.release_group.cmp(&b.release_group))
        });
        rests_on.sort();
        proposals.push(Proposal {
            id: proposal_id("album", public_ids.get(&anchor)),
            kind: "album",
            anchor,
            group: anchor,
            folder: disc.folder.to_owned(),
            items: subject,
            rests_on,
            candidates,
            fingerprint: String::new(),
        });
    }
    // A group is every proposal whose likeliest candidate is the same
    // album, named by the first-seen anchor among them.
    let mut groups: HashMap<String, i64> = HashMap::new();
    for proposal in &proposals {
        if let Some(top) = proposal.candidates.first() {
            let group = groups
                .entry(top.release_group.clone())
                .or_insert(proposal.anchor);
            if first_seen(&proposal.anchor) < first_seen(group) {
                *group = proposal.anchor;
            }
        }
    }
    for proposal in &mut proposals {
        if let Some(top) = proposal.candidates.first()
            && let Some(group) = groups.get(&top.release_group)
        {
            proposal.group = *group;
        }
        proposal.fingerprint = fingerprint(proposal, public_ids);
    }
    proposals
}

/// A proposal's ID: a digest of its kind and its anchor's public ID.
pub fn proposal_id(kind: &str, anchor: Option<&[u8; 16]>) -> [u8; 16] {
    let mut hash = Sha1::new();
    hash.update(b"ritornel proposal\0");
    hash.update(kind.as_bytes());
    hash.update([0]);
    hash.update(anchor.copied().unwrap_or_default());
    let digest = hash.finalize();
    let mut id = [0u8; 16];
    for (to, from) in id.iter_mut().zip(digest.iter()) {
        *to = *from;
    }
    id
}

/// A digest of everything a proposal shows: its folder, its group, its
/// files, what it rests on, and every candidate with its values.
fn fingerprint(proposal: &Proposal, public_ids: &HashMap<i64, [u8; 16]>) -> String {
    let mut hash = Sha1::new();
    hash.update(proposal.id);
    hash.update(proposal.folder.as_bytes());
    hash.update([0]);
    hash.update(public_ids.get(&proposal.group).copied().unwrap_or_default());
    for item in &proposal.items {
        hash.update(public_ids.get(item).copied().unwrap_or_default());
    }
    for rest in &proposal.rests_on {
        hash.update(rest.disc_id.as_bytes());
        hash.update(rest.reply.to_be_bytes());
        hash.update(rest.found.as_bytes());
        hash.update([0]);
    }
    // Each field by itself, a text with its length, so that no two
    // candidates hash alike and a change to how one prints changes nothing.
    let text = |hash: &mut Sha1, text: Option<&str>| match text {
        Some(text) => {
            hash.update([1]);
            hash.update((text.len() as u64).to_be_bytes());
            hash.update(text.as_bytes());
        }
        None => hash.update([0]),
    };
    let file = |item: &i64| public_ids.get(item).copied().unwrap_or_default();
    for candidate in &proposal.candidates {
        text(&mut hash, Some(&candidate.release_group));
        text(&mut hash, candidate.title.as_deref());
        text(&mut hash, Some(&candidate.artist));
        text(&mut hash, candidate.primary_type.as_deref());
        text(&mut hash, candidate.first_date.as_deref());
        hash.update(candidate.releases.to_be_bytes());
        text(&mut hash, Some(candidate.change.as_str()));
        hash.update((candidate.values.len() as u64).to_be_bytes());
        for value in &candidate.values {
            hash.update(file(&value.item));
            text(&mut hash, Some(value.field));
            hash.update(value.ordinal.to_be_bytes());
            text(&mut hash, value.value.as_deref());
            text(&mut hash, Some(value.origin.name()));
        }
        hash.update((candidate.onto.len() as u64).to_be_bytes());
        for item in &candidate.onto {
            hash.update(file(item));
        }
    }
    hex(&hash.finalize())
}

/// An album title and a credit: names and IDs.
type Credit<'a> = (&'a String, &'a Vec<String>, &'a Vec<String>);

fn credit_of(facts: &Facts) -> Option<Credit<'_>> {
    Some((
        facts.album.as_ref()?,
        &facts.album_artist,
        &facts.album_artist_mbids,
    ))
}

/// The values a yes to `candidate` would put on `subject`, the disc's
/// files no answer gave an album, what that would change, and the other discs it was worked out with. Every file
/// gets the album's ID, and its kind and first date where the catalogue
/// knows them. Where other discs of the library are on the same album, each
/// as one medium of it and no two as the same, the yes joins them: each
/// file gets its disc's number, and the files of every disc but the first
/// take the first disc's album title and credit, IDs included, where their
/// own differ. A file with no album title gets the album's.
fn values_for<'a, 'b>(
    disc: &Disc<'_>,
    subject: &[i64],
    cluster: &[&'b Disc<'a>],
    candidate: &Candidate,
    items: &HashMap<i64, &Item>,
) -> (Vec<Value>, Change, Vec<&'b Disc<'a>>, Vec<i64>) {
    let group = candidate.release_group.as_str();
    let mut values = Vec::new();
    let mut change = Change::Knows;
    let mut say = |item: i64, field: &'static str, ordinal: u32, value: Option<&str>, origin| {
        values.push(Value {
            item,
            field,
            ordinal,
            value: value.map(str::to_owned),
            origin,
        });
    };
    let positions: Vec<Option<u32>> = cluster.iter().map(|other| other.position(group)).collect();
    let distinct: BTreeSet<u32> = positions.iter().flatten().copied().collect();
    let joins = cluster.len() > 1 && distinct.len() == cluster.len();
    let first = cluster
        .iter()
        .zip(&positions)
        .min_by_key(|(_, at)| **at)
        .map(|(other, _)| *other);
    // The first disc's album title and credit, as the catalog reads them:
    // its files' most usual.
    let kept: Option<Credit<'_>> = first.and_then(|first| {
        let mut counts: BTreeMap<Credit<'_>, usize> = BTreeMap::new();
        for credit in first
            .items
            .iter()
            .filter_map(|id| items.get(id))
            .filter_map(|item| credit_of(&item.facts))
        {
            *counts.entry(credit).or_default() += 1;
        }
        counts
            .into_iter()
            .max_by_key(|(_, count)| *count)
            .map(|(kept, _)| kept)
    });
    let year = |date: Option<&str>| date.and_then(parse_date).map(|(_, year)| year);
    for id in subject {
        let Some(item) = items.get(id) else {
            continue;
        };
        let facts = &item.facts;
        // Another album ID, or one that would put this disc's tile with
        // another's without joining them, moves files.
        if facts
            .release_group_mbid
            .as_deref()
            .is_some_and(|had| had != group)
            || (cluster.len() > 1 && !joins)
        {
            change = Change::Moves;
        }
        say(*id, "mb_release_group", 0, Some(group), Origin::Catalogue);
        if let Some(kind) = &candidate.primary_type {
            if facts.primary_type.as_ref() != Some(kind) {
                change = change.max(Change::Gains);
            }
            say(*id, "primary_type", 0, Some(kind), Origin::Catalogue);
        }
        if let Some(date) = &candidate.first_date {
            if year(facts.original_date.as_deref()) != year(Some(date)) {
                change = change.max(Change::Gains);
            }
            say(*id, "original_date", 0, Some(date), Origin::Catalogue);
        }
        if joins && let Some(number) = disc.position(group) {
            if facts.disc != Some(i64::from(number)) {
                change = Change::Moves;
            }
            say(
                *id,
                "disc_number",
                0,
                Some(&number.to_string()),
                Origin::Catalogue,
            );
            if let Some(kept) = kept
                && credit_of(facts) != Some(kept)
            {
                change = Change::Moves;
                let (album, artists, mbids) = kept;
                say(*id, "album", 0, Some(album), Origin::Library);
                for (ordinal, artist) in (0..).zip(artists) {
                    say(*id, "album_artist", ordinal, Some(artist), Origin::Library);
                }
                if artists.is_empty() && !facts.album_artist.is_empty() {
                    say(*id, "album_artist", 0, None, Origin::Library);
                }
                for (ordinal, mbid) in (0..).zip(mbids) {
                    say(*id, "mb_album_artist", ordinal, Some(mbid), Origin::Library);
                }
                if mbids.is_empty() && !facts.album_artist_mbids.is_empty() {
                    say(*id, "mb_album_artist", 0, None, Origin::Library);
                }
                continue;
            }
        }
        if facts.album.is_none()
            && let Some(title) = &candidate.title
        {
            change = Change::Moves;
            say(*id, "album", 0, Some(title), Origin::Catalogue);
        }
    }
    values.sort();
    let joined = if joins { cluster.to_vec() } else { Vec::new() };
    let onto = match first {
        Some(first) if joins && !std::ptr::eq(first, disc) => {
            let mut items = first.items.clone();
            items.sort_unstable();
            items
        }
        _ => Vec::new(),
    };
    (values, change, joined, onto)
}

#[cfg(test)]
#[path = "propose_tests.rs"]
mod tests;
