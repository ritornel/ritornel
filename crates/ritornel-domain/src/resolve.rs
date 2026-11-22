//! The resolver: which recording each source item is. See "Recording
//! resolution" in docs/internals/identity.md.
//!
//! One ladder serves every policy; a [`Policy`] is the numbers and switches
//! it decides by. Policy 2 is policy 1 with four
//! values changed after it was tuned on a real library: accents folded,
//! both disc rules on, and a match on tags alone bound across two albums
//! only when the lengths agree almost exactly
//! ([`Policy::tags_bind_across_albums`],
//! [`Policy::across_albums_within_ms`]). Policy 3 weighs eligible runner-ups
//! independently and reads part numbers as version markers only on songs.
//!
//! Items are taken in first-seen order, and each sees only what the items
//! before it said. A recording is made by the first item that binds to
//! nothing, and that item keeps the recording made for it from one run to
//! the next, which is what makes a second run over the same library decide
//! the same thing.

use std::cell::Cell;
use std::collections::{BTreeSet, HashMap, HashSet};

use crate::extract::Facts;
use crate::library::{
    AcrossAlbums, Candidate, Decision, Evidence, Item, Kind, LearnedAlias, Library, Method,
    Penalties, Withheld, WithheldCause,
};
use unicode_normalization::UnicodeNormalization;

use crate::normalize::{core_with_fullwidth, difference, fold_accents, normalize};

/// The version of the policy scans resolve by, [`Policy::current`]: what
/// `identity_decision.resolver_version` records for a decision it makes.
pub const RESOLVER_VERSION: i64 = 3;

/// Words that mark a different version of a song. One that is in a title
/// and not in the other can only stop a match. "Remaster" is not here: a
/// remaster shares the recording.
const VERSION_MARKERS: &[&str] = &[
    "live",
    "remix",
    "remixes",
    "remixed",
    "mix",
    "mixes",
    "edit",
    "edits",
    "demo",
    "demos",
    "mono",
    "acoustic",
    "instrumental",
    "instrumentals",
    "karaoke",
    "unplugged",
    "session",
    "sessions",
    "alternate",
    "take",
    "reprise",
    "extended",
    "dub",
    "rehearsal",
    "outtake",
    "acapella",
    "cappella",
    "orchestral",
    "rerecorded",
    "stripped",
];

/// Spellings of "a cappella". Policy 2's markers read every one as the
/// same marker, so that two spellings never stop a match and one spelling
/// against none does; policy 1 knows only the first two, as two markers.
const CAPPELLA: &[&str] = &["acapella", "cappella", "capella", "acappella"];

/// The words that, followed by a number, name a part: "Part II", "Pt. 2".
const PART: &[&str] = &["part", "pt"];

/// Number words a part may be written in.
const NUMBERS: &[&str] = &[
    "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten",
];

/// The markers that, in an album's title, say every track on it is a
/// different version.
const ALBUM_MARKERS: &[&str] = &[
    "live",
    "remix",
    "remixes",
    "remixed",
    "demo",
    "demos",
    "mono",
    "acoustic",
    "instrumental",
    "instrumentals",
    "karaoke",
    "unplugged",
    "session",
    "sessions",
];

/// The numbers and switches a policy decides by. The thresholds are Beets'
/// defaults applied to a different scoring function: a starting point,
/// tuned on a real library. The dry run takes other values to ask what
/// they would change; only a named policy, [`Policy::current`], is applied
/// by a scan.
#[derive(Debug, Clone, PartialEq)]
pub struct Policy {
    /// The policy's number: `identity_decision.resolver_version`. Values
    /// changed for a dry run keep the number of the policy they vary.
    pub version: i64,
    /// A tags-and-duration match binds at this distance or less.
    pub bind_max: f64,
    /// And only if the runner-up is at least this much further.
    pub min_gap: f64,
    /// A new recording is marked for review if a candidate scored under
    /// this.
    pub review_below: f64,
    /// A difference in duration costs nothing up to this.
    pub duration_grace_ms: i64,
    /// And costs everything this much beyond the grace.
    pub duration_span_ms: i64,
    /// An identifier does not bind across a larger difference in duration.
    pub identifier_duration_max_ms: i64,
    /// The penalty for titles that differ only in a bracketed note.
    pub title_note_penalty: f64,
    /// The weights of the title, artist, duration and album penalties.
    pub weights: [f64; 4],
    /// Whether candidate text has its accents taken off before it is
    /// compared, so that `Élan` finds `Elan`. Stored names never change.
    pub fold_accents: bool,
    /// Whether sibling folders of one album whose track numbers collide,
    /// which share no title and no year, and whose names each give a disc
    /// of their own, are discs of one release. See the `discs` module.
    pub discs_from_folders: bool,
    /// Whether a trailing "Disc 2" or "CD 2" in an album title is taken
    /// as the disc number and left out of the title. See the `discs` module.
    pub disc_from_album_title: bool,
    /// Whether a match on tags and duration alone may bind a file to a
    /// recording whose only near evidence is on a different album. When it
    /// may not, such a match is still scored and listed, the file gets a
    /// recording of its own, and the decision is marked for review with
    /// the match it was held back from in its evidence. The same audio and
    /// a recording MBID bind across albums either way. What "a different
    /// album" is: see `same_album` in this module.
    pub tags_bind_across_albums: bool,
    /// When tags may not bind across albums, the exception: a match on
    /// another album still binds when the two lengths differ by this many
    /// milliseconds or less, and the decision's evidence says so. 0 makes
    /// no exception. Two rips of one master agree to within a CD frame
    /// (13 ms); two different recordings of a song rarely do.
    pub across_albums_within_ms: i64,
    /// Whether two different tracks of one release are never one
    /// recording by tags: a match on tags and length does not bind a file
    /// to a recording that a file at another disc and track of the same
    /// release is bound to. The file gets a recording of its own, marked
    /// for review. "The same release" is the one the catalog forms: by a
    /// release MBID, or else by album artist and title split by folder
    /// where two folders claim one position, so two copies of an album in
    /// two folders are two releases and still bind track to track. Two
    /// positions differ when the track numbers do, or both files have a
    /// disc and the discs do; a file with no track number has no position
    /// and is never held back by this. The same audio still binds.
    pub tracks_of_one_release_differ: bool,
    /// Which list of version markers: 1 is policy 1's; 2 also reads every
    /// spelling of "a cappella" as one marker, and part numbers ("Part
    /// II", "Pt. 2") as markers.
    /// Policy 3 reads a part number as a marker only in a song title.
    pub marker_list: u8,
}

impl Default for Policy {
    fn default() -> Self {
        Self::current()
    }
}

impl Policy {
    /// The file policy used by scans.
    pub fn current() -> Self {
        Self::v3()
    }

    /// Policy 3 also reads fullwidth brackets as title-note delimiters.
    pub(crate) fn title_core(&self, text: &str) -> String {
        core_with_fullwidth(text, self.version >= 3)
    }

    /// The named policy of this number, if there is one.
    pub fn of_version(version: i64) -> Option<Self> {
        match version {
            1 => Some(Self::v1()),
            2 => Some(Self::v2()),
            3 => Some(Self::v3()),
            _ => None,
        }
    }

    /// Policy 1, as it was first applied. Kept unchanged so that a dry run
    /// can still compare it with the policy in force.
    pub fn v1() -> Self {
        Self {
            version: 1,
            bind_max: 0.04,
            min_gap: 0.25,
            review_below: 0.25,
            duration_grace_ms: 2_000,
            duration_span_ms: 20_000,
            identifier_duration_max_ms: 10_000,
            title_note_penalty: 0.1,
            weights: [3.0, 3.0, 3.0, 0.3],
            fold_accents: false,
            discs_from_folders: false,
            disc_from_album_title: false,
            tags_bind_across_albums: true,
            across_albums_within_ms: 0,
            tracks_of_one_release_differ: false,
            marker_list: 1,
        }
    }

    /// Policy 2: policy 1 with the four choices its tuning on a real
    /// library recommended. The free duration band stays at 2 s.
    pub fn v2() -> Self {
        Self {
            version: 2,
            fold_accents: true,
            discs_from_folders: true,
            disc_from_album_title: true,
            tags_bind_across_albums: false,
            across_albums_within_ms: 20,
            tracks_of_one_release_differ: true,
            marker_list: 2,
            ..Self::v1()
        }
    }

    /// Policy 3 compares eligible runner-ups and treats an album part
    /// number as part of its title.
    pub fn v3() -> Self {
        Self {
            version: 3,
            ..Self::v2()
        }
    }

    fn uses_eligible_runner_up(&self) -> bool {
        self.version >= 3
    }

    fn album_part_numbers(&self) -> PartNumbers {
        if self.version < 3 {
            PartNumbers::Include
        } else {
            PartNumbers::Ignore
        }
    }
}

/// Which recording of the fold an entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecKey {
    /// A row that exists and is not redirected.
    Row(i64),
    /// A new recording, made for the item at this index of the library.
    Founded(usize),
}

/// What the fold decided for one item.
#[derive(Debug, Clone)]
pub(crate) struct Bound {
    /// The recording, as an index into [`Folded::recs`].
    pub(crate) rec: Option<usize>,
    /// The row the assignment names when it is left as it is.
    pub(crate) keep_row: Option<i64>,
    pub(crate) decision: Option<Decision>,
    /// Whether the recording differs from the current assignment's.
    pub(crate) rec_changed: bool,
    pub(crate) pinned: bool,
    /// The recording, as an index into [`Folded::recs`], that a match on
    /// tags alone was held back from because it is on another album.
    pub(crate) withheld: Option<usize>,
    /// An exclusion took a candidate away from it.
    pub(crate) blocked: bool,
    /// The item's assignment is not the resolver's to change at all.
    pub(crate) frozen: bool,
}

#[derive(Debug)]
pub(crate) struct Folded {
    pub(crate) recs: Vec<RecKey>,
    pub(crate) items: Vec<Bound>,
}

/// One item's claims, as they are compared.
#[derive(Debug, Clone)]
struct Ev {
    item: i64,
    rec: usize,
    raw_title: Option<String>,
    raw_artist: Option<String>,
    norm_title: Option<String>,
    norm_artist: Option<String>,
    full: Option<String>,
    core: Option<String>,
    artist: Option<String>,
    album_full: Option<String>,
    album_core: Option<String>,
    /// The album artist tag, normalised. `None` when there is none.
    album_artist: Option<String>,
    release: Option<String>,
    release_group: Option<String>,
    duration: Option<i64>,
    markers: Vec<String>,
    album_markers: Vec<String>,
    /// The release the catalog puts it on, when the policy tells tracks of
    /// one release apart.
    on_release: Option<String>,
    /// Its disc, where it has one, and its track, where it has a track
    /// number.
    position: Option<(Option<i64>, i64)>,
    mbid: Option<String>,
    ids: Vec<String>,
}

/// The number a part is written as: digits, a roman numeral up to
/// thirty-nine, or a word up to ten.
pub(crate) fn part_number(word: &str) -> Option<u32> {
    if let Ok(number) = word.parse::<u32>() {
        return Some(number);
    }
    if let Some(at) = NUMBERS.iter().position(|number| *number == word) {
        return u32::try_from(at).ok().map(|at| at + 1);
    }
    let value = |c: char| match c {
        'i' => Some(1),
        'v' => Some(5),
        'x' => Some(10),
        _ => None,
    };
    let digits: Vec<u32> = word.chars().map(value).collect::<Option<_>>()?;
    let mut total = 0;
    for (at, digit) in digits.iter().enumerate() {
        match digits.get(at + 1) {
            Some(next) if next > digit => total -= i64::from(*digit),
            _ => total += i64::from(*digit),
        }
    }
    u32::try_from(total)
        .ok()
        .filter(|total| (1..40).contains(total))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PartNumbers {
    Include,
    Ignore,
}

/// The version markers in a title as it is written. The words are taken
/// before any punctuation is stripped, so that a marker stuck to the word
/// before it, as in `Lantern(Live)` or `Lantern-Remix`, is still a word,
/// and also as the normaliser joins them, so that `Re-recorded` is one.
///
/// With policy 2's list (`list` 2), every spelling of "a cappella" is one
/// marker, and a part number is one: "Part II", "Pt. 2" and "Part Two"
/// are all `part 2` when part numbers are included. Policy 3 reserves that rule
/// for song titles. Like the others, these markers can only stop a match.
fn markers(
    text: Option<&str>,
    words: &'static [&'static str],
    list: u8,
    parts: PartNumbers,
) -> Vec<String> {
    let text = text.unwrap_or("");
    let lower: String = text.nfkc().flat_map(char::to_lowercase).collect();
    let split: Vec<&str> = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect();
    let key = crate::normalize::joined_words(text);
    let joined = key.split(' ');
    let mut found: Vec<String> = Vec::new();
    let mut add = |marker: String| {
        if !found.contains(&marker) {
            found.push(marker);
        }
    };
    for word in split.iter().copied().chain(joined) {
        if list >= 2 && CAPPELLA.contains(&word) {
            add("a cappella".to_owned());
        } else if let Some(marker) = words.iter().find(|marker| **marker == word) {
            add((*marker).to_owned());
        }
    }
    if parts == PartNumbers::Include && list >= 2 {
        for pair in split.windows(2) {
            if let [word, number] = pair
                && PART.contains(word)
                && let Some(number) = part_number(number)
            {
                add(format!("part {number}"));
            }
        }
    }
    found.sort_unstable();
    found
}

/// Whether two positions on one release are different tracks: their track
/// numbers differ, or both have a disc and those differ. A file with no
/// track number has no position, and one with no disc is taken to be on
/// whichever disc the other is, so that a copy of a disc that lost its
/// disc tag is not parted from one that kept it.
fn apart(a: Option<(Option<i64>, i64)>, b: Option<(Option<i64>, i64)>) -> bool {
    match (a, b) {
        (Some((a_disc, a_track)), Some((b_disc, b_track))) => {
            a_track != b_track || matches!((a_disc, b_disc), (Some(a), Some(b)) if a != b)
        }
        _ => false,
    }
}

/// The version markers of a title by a marker list.
pub(crate) fn markers_of(title: &str, list: u8) -> Vec<String> {
    markers(Some(title), VERSION_MARKERS, list, PartNumbers::Include)
}

/// The version markers of an album title, such as "live" or "acoustic", by
/// policy 2's list, as the scrobble matcher reads them. It also reads
/// "Acoustics" as "acoustic"; the file resolver's policies do not, so that
/// their results stay as they are.
pub(crate) fn album_markers_of(album: &str) -> Vec<String> {
    let mut found = markers(Some(album), ALBUM_MARKERS, 2, PartNumbers::Include);
    // Read as `markers` reads, between any marks and with marks inside a
    // word joining it.
    let lower: String = album.nfkc().flat_map(char::to_lowercase).collect();
    let acoustics = lower
        .split(|c: char| !c.is_alphanumeric())
        .chain(crate::normalize::joined_words(album).split(' '))
        .any(|word| word == "acoustics");
    if acoustics && !found.iter().any(|marker| marker == "acoustic") {
        found.push("acoustic".to_owned());
    }
    found.sort();
    found
}

impl Ev {
    fn of(item: i64, facts: &Facts, policy: &Policy, on_release: Option<&String>) -> Self {
        let key = |key: String| {
            if policy.fold_accents {
                fold_accents(&key)
            } else {
                key
            }
        };
        let raw_title = facts.title.clone();
        let raw_artist = facts.artist_text();
        let norm_title = raw_title.as_deref().map(normalize);
        let norm_artist = raw_artist.as_deref().map(normalize);
        let full = norm_title.clone().map(key);
        let artist = norm_artist.clone().map(key);
        let album_full = facts.album.as_deref().map(normalize).map(key);
        Self {
            item,
            rec: usize::MAX,
            raw_title,
            raw_artist,
            norm_title,
            norm_artist,
            core: facts
                .title
                .as_deref()
                .map(|text| policy.title_core(text))
                .map(key),
            artist,
            album_core: facts
                .album
                .as_deref()
                .map(|text| policy.title_core(text))
                .map(key),
            album_artist: facts.album_artist_text().as_deref().map(normalize).map(key),
            release: facts.release_mbid.clone(),
            release_group: facts.release_group_mbid.clone(),
            duration: facts.duration_ms.filter(|ms| *ms >= 0),
            markers: markers(
                facts.title.as_deref(),
                VERSION_MARKERS,
                policy.marker_list,
                PartNumbers::Include,
            ),
            album_markers: markers(
                facts.album.as_deref(),
                ALBUM_MARKERS,
                policy.marker_list,
                policy.album_part_numbers(),
            ),
            on_release: on_release.cloned(),
            position: facts.track.map(|track| (facts.disc, track)),
            mbid: facts.recording_mbid.clone(),
            ids: facts
                .isrcs
                .iter()
                .map(|id| format!("isrc:{id}"))
                .chain(facts.acoustids.iter().map(|id| format!("acoustid:{id}")))
                .collect(),
            full,
            album_full,
        }
    }
}

#[derive(Debug)]
struct Rec {
    key: RecKey,
    mbids: Vec<String>,
    first_ev: Option<usize>,
}

type Aliases = HashMap<String, BTreeSet<String>>;

struct Fold<'a> {
    library: &'a Library,
    policy: &'a Policy,
    recs: Vec<Rec>,
    evs: Vec<Ev>,
    by_row: HashMap<i64, usize>,
    /// The rows that an item of this fold has taken as its own recording.
    founded: HashSet<i64>,
    by_audio: HashMap<i64, usize>,
    by_mbid: HashMap<String, Vec<usize>>,
    by_core: HashMap<String, Vec<usize>>,
    by_full: HashMap<String, Vec<usize>>,
    title_alias: Aliases,
    artist_alias: Aliases,
    excluded: HashMap<i64, HashSet<i64>>,
    /// Set when an exclusion takes a candidate away from the item being
    /// decided.
    blocked: Cell<bool>,
    /// The release each item is on, by its row, when the policy tells two
    /// tracks of one release apart.
    releases: HashMap<i64, String>,
}

enum Outcome {
    /// The assignment is left exactly as it is.
    Keep {
        pinned: bool,
    },
    /// No audio and no assignment: nothing to bind.
    Unbound,
    Bind {
        rec: usize,
        decision: Decision,
    },
    New {
        decision: Decision,
        /// The recording a match on tags was held back from.
        withheld: Option<usize>,
    },
}

/// A candidate recording: its distance, what made it up, its index in the
/// fold, the item whose evidence it is, and whether that evidence is on
/// another album.
#[derive(Debug, Clone, Copy)]
struct Weighed {
    distance: f64,
    penalties: Penalties,
    rec: usize,
    item: i64,
    across: bool,
    /// How far apart the two lengths are, when both are known.
    apart_ms: Option<i64>,
    /// An item bound to this recording that is another track of the
    /// file's own release.
    clash: Option<i64>,
}

impl Weighed {
    /// Whether the policy lets tags bind to this candidate at all: one on
    /// the same album, or on another whose length agrees closely enough.
    fn may_bind(&self, policy: &Policy) -> bool {
        self.clash.is_none()
            && (policy.tags_bind_across_albums
                || !self.across
                || (policy.across_albums_within_ms > 0
                    && self
                        .apart_ms
                        .is_some_and(|apart| apart <= policy.across_albums_within_ms)))
    }
}

fn learn(aliases: &mut Aliases, a: &str, b: &str) {
    aliases
        .entry(a.to_owned())
        .or_default()
        .insert(b.to_owned());
    aliases
        .entry(b.to_owned())
        .or_default()
        .insert(a.to_owned());
}

fn aliased(aliases: &Aliases, a: &str, b: &str) -> bool {
    aliases.get(a).is_some_and(|names| names.contains(b))
}

impl<'a> Fold<'a> {
    fn new(library: &'a Library, policy: &'a Policy) -> Self {
        let canonical = |row: i64| {
            library
                .recording_canonical
                .get(&row)
                .copied()
                .unwrap_or(row)
        };
        let mut excluded: HashMap<i64, HashSet<i64>> = HashMap::new();
        for (item, recording) in &library.exclusions {
            excluded
                .entry(*item)
                .or_default()
                .insert(canonical(*recording));
        }
        let (mut title_alias, mut artist_alias) = (Aliases::new(), Aliases::new());
        for (kind, a, b) in &library.aliases {
            let (a, b) = if policy.fold_accents {
                (fold_accents(a), fold_accents(b))
            } else {
                (a.clone(), b.clone())
            };
            match kind {
                Kind::Recording => learn(&mut title_alias, &a, &b),
                Kind::Artist => learn(&mut artist_alias, &a, &b),
                _ => {}
            }
        }
        Self {
            library,
            policy,
            recs: Vec::new(),
            evs: Vec::new(),
            by_row: HashMap::new(),
            founded: HashSet::new(),
            by_audio: HashMap::new(),
            by_mbid: HashMap::new(),
            by_core: HashMap::new(),
            by_full: HashMap::new(),
            title_alias,
            artist_alias,
            excluded,
            blocked: Cell::new(false),
            releases: if policy.tracks_of_one_release_differ {
                crate::catalog::release_keys(library)
            } else {
                HashMap::new()
            },
        }
    }

    fn canonical(&self, row: i64) -> i64 {
        self.library
            .recording_canonical
            .get(&row)
            .copied()
            .unwrap_or(row)
    }

    /// The fold's entry for a row, through its redirects.
    fn rec_for_row(&mut self, row: i64) -> usize {
        let row = self.canonical(row);
        if let Some(rec) = self.by_row.get(&row) {
            return *rec;
        }
        self.recs.push(Rec {
            key: RecKey::Row(row),
            mbids: Vec::new(),
            first_ev: None,
        });
        self.by_row.insert(row, self.recs.len() - 1);
        self.recs.len() - 1
    }

    fn is_excluded(&self, item: i64, rec: usize) -> bool {
        let excluded = match self.recs.get(rec).map(|rec| rec.key) {
            Some(RecKey::Row(row)) => self
                .excluded
                .get(&item)
                .is_some_and(|recordings| recordings.contains(&row)),
            _ => false,
        };
        if excluded {
            self.blocked.set(true);
        }
        excluded
    }

    /// Whether two items' claims put them on the same album, by which
    /// [`Policy::tags_bind_across_albums`] decides. `None` when either has
    /// no album tag: an album not known is not a different one, so the
    /// switch leaves such a pair alone.
    ///
    /// - With a release-group MBID on both, the album is the same when the
    ///   IDs are: a remaster or a deluxe edition in the original's group is
    ///   the same album, and two self-titled albums are not.
    /// - Otherwise, one release MBID on both is the same album.
    /// - Otherwise it is the same when the normalised titles are, whole or
    ///   without a bracketed note (`Long Field` and `Long Field (Deluxe)`),
    ///   and the album artists are, when both files have an album artist
    ///   tag. Two copies of one album in different folders are therefore
    ///   one album, and so are two copies of a various-artists compilation
    ///   tagged with no album artist; a compilation and an artist's own
    ///   album, titled differently, are two.
    fn same_album(&self, a: &Ev, b: &Ev) -> Option<bool> {
        let (Some(a_full), Some(b_full)) = (&a.album_full, &b.album_full) else {
            return None;
        };
        if let (Some(a), Some(b)) = (&a.release_group, &b.release_group) {
            return Some(a == b);
        }
        if a.release.is_some() && a.release == b.release {
            return Some(true);
        }
        let titles = a_full == b_full || a.album_core == b.album_core;
        let artists = match (&a.album_artist, &b.album_artist) {
            (Some(a), Some(b)) => a == b || aliased(&self.artist_alias, a, b),
            _ => true,
        };
        Some(titles && artists)
    }

    /// The distance between an item and one item's evidence for a
    /// candidate, or `None` if a version marker rules the pair out.
    fn distance(&self, probe: &Ev, ev: &Ev) -> Option<(f64, Penalties)> {
        if probe.markers != ev.markers || probe.album_markers != ev.album_markers {
            return None;
        }
        let policy = self.policy;
        let same = |aliases: &Aliases, a: &Option<String>, b: &Option<String>| match (a, b) {
            (Some(a), Some(b)) => a == b || aliased(aliases, a, b),
            _ => false,
        };
        let title = if same(&self.title_alias, &probe.full, &ev.full) {
            0.0
        } else {
            policy.title_note_penalty
        };
        let artist = match (&probe.artist, &ev.artist) {
            _ if same(&self.artist_alias, &probe.artist, &ev.artist) => 0.0,
            (Some(a), Some(b)) => difference(a, b),
            _ => 1.0,
        };
        let duration = match (probe.duration, ev.duration) {
            (Some(a), Some(b)) => {
                let beyond = (a - b).abs() - policy.duration_grace_ms;
                (beyond as f64 / policy.duration_span_ms as f64).clamp(0.0, 1.0)
            }
            _ => 1.0,
        };
        let album = match (&probe.album_full, &ev.album_full) {
            (None, None) => 0.0,
            (Some(a), Some(b)) if a == b || probe.album_core == ev.album_core => 0.0,
            (Some(_), Some(_)) => 1.0,
            _ => 0.5,
        };
        let [w_title, w_artist, w_duration, w_album] = policy.weights;
        let mut distance =
            (w_title * title + w_artist * artist + w_duration * duration + w_album * album)
                / (w_title + w_artist + w_duration + w_album);
        // An ISRC or an AcoustID in common lowers the distance, and never
        // binds alone.
        let shared_identifier = probe.ids.iter().any(|id| ev.ids.contains(id));
        if shared_identifier {
            distance *= 0.5;
        }
        Some((
            distance,
            Penalties {
                title,
                artist,
                duration,
                album,
                shared_identifier,
            },
        ))
    }

    /// The names a binding by audio or by identifier showed to be the same
    /// thing, where they differ, carrying both the normalised keys and the raw
    /// spellings they came from.
    fn aliases_from(&mut self, probe: &Ev, rec: usize) -> Vec<LearnedAlias> {
        let Some(first) = self.recs.get(rec).and_then(|rec| rec.first_ev) else {
            return Vec::new();
        };
        let Some(ev) = self.evs.get(first) else {
            return Vec::new();
        };
        let mut pairs = Vec::new();
        let mut learned = Vec::new();
        let candidates = [
            (
                Kind::Recording,
                probe.full.as_deref(),
                ev.full.as_deref(),
                probe.norm_title.as_deref(),
                ev.norm_title.as_deref(),
                probe.raw_title.as_deref(),
                ev.raw_title.as_deref(),
            ),
            (
                Kind::Artist,
                probe.artist.as_deref(),
                ev.artist.as_deref(),
                probe.norm_artist.as_deref(),
                ev.norm_artist.as_deref(),
                probe.raw_artist.as_deref(),
                ev.raw_artist.as_deref(),
            ),
        ];
        for (kind, folded_a, folded_b, norm_a, norm_b, raw_a, raw_b) in candidates {
            if let (Some(fa), Some(fb)) = (folded_a, folded_b)
                && fa != fb
            {
                learned.push((kind, fa.to_owned(), fb.to_owned()));
            }
            // Persisted alias observations store unfolded `normalize()` keys
            // alongside the raw spellings (earlier rows at v2 without spellings
            // stored folded keys); in-memory matching folds per policy on load.
            if let (Some(na), Some(nb), Some(sa), Some(sb)) = (norm_a, norm_b, raw_a, raw_b)
                && !na.is_empty()
                && !nb.is_empty()
                && na != nb
                && !sa.is_empty()
                && !sb.is_empty()
            {
                let (name_a, name_b, spelling_a, spelling_b) = if na < nb {
                    (na.to_owned(), nb.to_owned(), sa.to_owned(), sb.to_owned())
                } else {
                    (nb.to_owned(), na.to_owned(), sb.to_owned(), sa.to_owned())
                };
                pairs.push(LearnedAlias {
                    kind,
                    name_a,
                    name_b,
                    spelling_a,
                    spelling_b,
                });
            }
        }
        for (kind, a, b) in &learned {
            match kind {
                Kind::Recording => learn(&mut self.title_alias, a, b),
                _ => learn(&mut self.artist_alias, a, b),
            }
        }
        pairs
    }

    /// The ladder, steps 0 to 4, for one item. A trial skips step 0 and
    /// learns nothing: it asks where the audio would land as a new file.
    fn decide(&mut self, item: &Item, probe: &Ev, trial: bool) -> Outcome {
        if !trial {
            if item.retired {
                return Outcome::Keep { pinned: false };
            }
            // Step 0: an assignment the owner made is kept.
            if item.current.is_some_and(|current| current.by_user) {
                return Outcome::Keep { pinned: true };
            }
        }
        let Some(audio) = item.audio else {
            return match item.current {
                Some(_) if !trial => Outcome::Keep { pinned: false },
                _ => Outcome::Unbound,
            };
        };
        let exact = |method, claims: Vec<(String, String)>| Decision {
            method,
            score: 0.0,
            runner_up_gap: None,
            penalties: None,
            evidence: Evidence {
                claims,
                ..Evidence::default()
            },
            needs_review: false,
            aliases: Vec::new(),
        };

        // Step 1: the same audio is bound through another item.
        if let Some(rec) = self.by_audio.get(&audio).copied()
            && !self.is_excluded(item.id, rec)
        {
            let mut decision = exact(
                Method::SameAudio,
                vec![("audio".to_owned(), audio.to_string())],
            );
            if !trial {
                decision.aliases = self.aliases_from(probe, rec);
            }
            return Outcome::Bind { rec, decision };
        }

        // Step 2: a recording MBID that an existing recording carries.
        let mut contradictions = Vec::new();
        if let Some(mbid) = &probe.mbid {
            let carriers = self.by_mbid.get(mbid).cloned().unwrap_or_default();
            let mut found = None;
            for index in carriers {
                let Some(ev) = self.evs.get(index) else {
                    continue;
                };
                if self.is_excluded(item.id, ev.rec) {
                    continue;
                }
                let apart = match (probe.duration, ev.duration) {
                    (Some(a), Some(b)) => (a - b).abs(),
                    _ => 0,
                };
                let titles_disagree = match (&probe.core, &ev.core) {
                    (Some(a), Some(b)) => difference(a, b) > 0.5,
                    _ => false,
                };
                // A version marker on one side and not the other is a clear
                // disagreement: a live take does not share a studio
                // recording's ID, whatever the tags claim.
                let versions_disagree = probe.markers != ev.markers;
                if apart > self.policy.identifier_duration_max_ms {
                    contradictions.push(format!(
                        "mb_recording {mbid} is carried by item {} whose duration differs by {} ms",
                        ev.item, apart
                    ));
                } else if versions_disagree {
                    contradictions.push(format!(
                        "mb_recording {mbid} is carried by item {} under another version: {:?} and {:?}",
                        ev.item, probe.markers, ev.markers
                    ));
                } else if titles_disagree {
                    contradictions.push(format!(
                        "mb_recording {mbid} is carried by item {} under another title",
                        ev.item
                    ));
                } else {
                    found = Some(ev.rec);
                    break;
                }
            }
            if let Some(rec) = found {
                let mut decision = exact(
                    Method::Identifier,
                    vec![("mb_recording".to_owned(), mbid.clone())],
                );
                if !trial {
                    decision.aliases = self.aliases_from(probe, rec);
                }
                return Outcome::Bind { rec, decision };
            }
        }

        // Step 3: tags plus duration. It needs a title and an artist from
        // the tags, and is skipped after a contradiction.
        let policy = self.policy;
        let mut claims = Vec::new();
        let mut candidates: Vec<Weighed> = Vec::new();
        let mut bindable: Vec<Weighed> = Vec::new();
        if contradictions.is_empty()
            && let (Some(full), Some(core), Some(artist)) =
                (&probe.full, &probe.core, &probe.artist)
        {
            claims.push(("title".to_owned(), full.clone()));
            claims.push(("artist".to_owned(), artist.clone()));
            if let Some(album) = &probe.album_full {
                claims.push(("album".to_owned(), album.clone()));
            }
            if let Some(duration) = probe.duration {
                claims.push(("duration_ms".to_owned(), duration.to_string()));
            }
            let mut indexes: BTreeSet<usize> = self
                .by_core
                .get(core)
                .into_iter()
                .flatten()
                .copied()
                .collect();
            for alias in self.title_alias.get(full).into_iter().flatten() {
                indexes.extend(self.by_full.get(alias).into_iter().flatten().copied());
            }
            // Keep the closest evidence overall and the closest eligible to
            // bind. A runner-up can narrow the gap even above bind_max.
            let mut best: HashMap<usize, (Weighed, Option<Weighed>)> = HashMap::new();
            let mut clashes: HashMap<usize, i64> = HashMap::new();
            for index in indexes {
                let Some(ev) = self.evs.get(index) else {
                    continue;
                };
                if self.is_excluded(item.id, ev.rec) {
                    continue;
                }
                // Two different recording MBIDs veto a candidate.
                let other_mbid = self.recs.get(ev.rec).is_some_and(|rec| {
                    probe
                        .mbid
                        .as_ref()
                        .is_some_and(|mbid| !rec.mbids.is_empty() && !rec.mbids.contains(mbid))
                });
                if other_mbid {
                    continue;
                }
                // Another track of the file's own release: whatever the
                // tags say, two positions on one release are two
                // recordings, so tags never bind to its recording.
                if policy.tracks_of_one_release_differ
                    && probe.on_release.is_some()
                    && probe.on_release == ev.on_release
                    && apart(probe.position, ev.position)
                {
                    clashes.entry(ev.rec).or_insert(ev.item);
                }
                if let Some((distance, penalties)) = self.distance(probe, ev) {
                    let weighed = Weighed {
                        distance,
                        penalties,
                        rec: ev.rec,
                        item: ev.item,
                        across: self.same_album(probe, ev) == Some(false),
                        apart_ms: probe.duration.zip(ev.duration).map(|(a, b)| (a - b).abs()),
                        clash: None,
                    };
                    let (nearest, eligible) = best.entry(ev.rec).or_insert((weighed, None));
                    if distance < nearest.distance {
                        *nearest = weighed;
                    }
                    if weighed.may_bind(policy) && eligible.is_none_or(|e| distance < e.distance) {
                        *eligible = Some(weighed);
                    }
                }
            }
            for (mut nearest, eligible) in best.into_values() {
                if let Some(close) = eligible.filter(|e| e.distance <= policy.bind_max) {
                    nearest = close;
                }
                nearest.clash = clashes.get(&nearest.rec).copied();
                // A clash rules out every copy of the recording.
                if nearest.clash.is_none() {
                    if policy.uses_eligible_runner_up() {
                        bindable.extend(eligible);
                    } else if nearest.may_bind(policy) {
                        // Earlier policies chose one copy to represent each recording.
                        bindable.push(nearest);
                    }
                }
                candidates.push(nearest);
            }
            let order = |a: &Weighed, b: &Weighed| {
                a.distance.total_cmp(&b.distance).then(a.rec.cmp(&b.rec))
            };
            candidates.sort_by(order);
            bindable.sort_by(order);
        }
        let gap_of = |list: &[Weighed]| match (list.first(), list.get(1)) {
            (Some(best), Some(next)) => Some(next.distance - best.distance),
            _ => None,
        };
        let close = bindable
            .first()
            .copied()
            .filter(|best| best.distance <= policy.bind_max);
        let eligible_gap = gap_of(&bindable);
        let binding = close.filter(|_| eligible_gap.is_none_or(|gap| gap >= policy.min_gap));
        // Show the copies whose gap decided the outcome. Without a
        // runner-up, the other nearby copies still give useful context.
        let compared =
            if policy.uses_eligible_runner_up() && close.is_some() && eligible_gap.is_some() {
                &bindable
            } else {
                &candidates
            };
        let listed: Vec<Candidate> = compared
            .iter()
            .take(5)
            .map(|weighed| Candidate {
                item: weighed.item,
                distance: weighed.distance,
                penalties: Some(weighed.penalties),
            })
            .collect();
        if let Some(best) = binding {
            return Outcome::Bind {
                rec: best.rec,
                decision: Decision {
                    method: Method::TagsDuration,
                    score: best.distance,
                    runner_up_gap: eligible_gap,
                    penalties: Some(best.penalties),
                    evidence: Evidence {
                        claims,
                        candidates: listed,
                        contradictions: Vec::new(),
                        withheld: None,
                        // Bound across albums only by the exception for an
                        // all but equal length, which says so.
                        across_albums: (best.across && !policy.tags_bind_across_albums).then(
                            || AcrossAlbums {
                                length_difference_ms: best.apart_ms.unwrap_or_default(),
                            },
                        ),
                    },
                    needs_review: false,
                    aliases: Vec::new(),
                },
            };
        }
        // The nearest match was near enough to bind, and the policy does
        // not let tags bind to it: it is another track of the file's own
        // release, or on another album at another length. It is held back
        // whatever the runner-up: when several albums carry the song, a
        // policy that never binds across them finds each of them close.
        let withheld = compared
            .first()
            .copied()
            .filter(|best| !best.may_bind(policy) && best.distance <= policy.bind_max);

        // Step 4: nothing bound. A doubtful match never binds: the item
        // gets a recording of its own, and the decision is marked for
        // review.
        let doubtful = candidates
            .iter()
            .any(|weighed| weighed.distance < policy.review_below);
        Outcome::New {
            decision: Decision {
                method: Method::New,
                score: 0.0,
                runner_up_gap: gap_of(compared),
                penalties: None,
                needs_review: doubtful || !contradictions.is_empty() || withheld.is_some(),
                evidence: Evidence {
                    claims,
                    candidates: listed,
                    contradictions,
                    across_albums: None,
                    withheld: withheld.map(|best| match best.clash {
                        Some(item) => Withheld {
                            cause: WithheldCause::SameRelease,
                            item,
                            recording: None,
                        },
                        None => Withheld {
                            cause: WithheldCause::OtherAlbum,
                            item: best.item,
                            recording: None,
                        },
                    }),
                },
                aliases: Vec::new(),
            },
            withheld: withheld.map(|best| best.rec),
        }
    }

    /// Adds what an item says to the recording it is bound to, for the
    /// items after it.
    fn register(&mut self, item: &Item, mut ev: Ev, rec: usize) {
        ev.rec = rec;
        let index = self.evs.len();
        if let Some(audio) = item.audio {
            self.by_audio.entry(audio).or_insert(rec);
        }
        if let Some(mbid) = &ev.mbid {
            self.by_mbid.entry(mbid.clone()).or_default().push(index);
        }
        if let Some(entry) = self.recs.get_mut(rec) {
            if let Some(mbid) = &ev.mbid
                && !entry.mbids.contains(mbid)
            {
                entry.mbids.push(mbid.clone());
            }
            entry.first_ev.get_or_insert(index);
        }
        if let (Some(full), Some(core)) = (&ev.full, &ev.core) {
            self.by_core.entry(core.clone()).or_default().push(index);
            self.by_full.entry(full.clone()).or_default().push(index);
        }
        self.evs.push(ev);
    }

    fn run(&mut self) -> Vec<Bound> {
        let library = self.library;
        let mut bounds = Vec::with_capacity(library.items.len());
        for (index, item) in library.items.iter().enumerate() {
            let probe = Ev::of(
                item.id,
                &item.facts,
                self.policy,
                self.releases.get(&item.id),
            );
            let current = item.current;
            self.blocked.set(false);
            let outcome = self.decide(item, &probe, false);
            let blocked = self.blocked.get();
            let mut withheld = None;
            let (rec, decision, pinned, frozen) = match outcome {
                Outcome::Unbound => (None, None, false, true),
                Outcome::Keep { pinned } => {
                    let rec = current.map(|current| self.rec_for_row(current.recording));
                    (rec, None, pinned, true)
                }
                Outcome::Bind { rec, decision } => (Some(rec), Some(decision), false, false),
                Outcome::New {
                    decision,
                    withheld: held,
                } => {
                    withheld = held;
                    // An item whose recording was made for it keeps it,
                    // unless the owner has said the item is not that
                    // recording, wherever its redirects lead.
                    let own = current
                        .filter(|current| current.method == Method::New && !current.by_user)
                        .map(|current| current.recording)
                        .filter(|row| {
                            !self.excluded.get(&item.id).is_some_and(|recordings| {
                                recordings.contains(&self.canonical(*row))
                            })
                        })
                        .filter(|row| self.founded.insert(*row));
                    let rec = match own {
                        Some(row) => self.rec_for_row(row),
                        None => {
                            self.recs.push(Rec {
                                key: RecKey::Founded(index),
                                mbids: Vec::new(),
                                first_ev: None,
                            });
                            self.recs.len() - 1
                        }
                    };
                    (Some(rec), Some(decision), false, false)
                }
            };
            if let Some(rec) = rec {
                self.register(item, probe, rec);
            }
            // A recording that only a redirect tells from the current one
            // is the same recording, and the assignment is left alone.
            let same = match (rec.and_then(|rec| self.recs.get(rec)), current) {
                (Some(rec), Some(current)) => {
                    rec.key == RecKey::Row(self.canonical(current.recording))
                }
                (None, None) => true,
                _ => false,
            };
            bounds.push(Bound {
                rec,
                keep_row: current.filter(|_| same).map(|current| current.recording),
                decision,
                rec_changed: !same,
                pinned,
                withheld,
                blocked,
                frozen,
            });
        }
        bounds
    }
}

pub(crate) fn fold(library: &Library, policy: &Policy) -> Folded {
    let mut fold = Fold::new(library, policy);
    let items = fold.run();
    Folded {
        recs: fold.recs.iter().map(|rec| rec.key).collect(),
        items,
    }
}

/// Audio that has taken the path of an item the library already has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Replacement {
    /// The index, in the library's items, of the item at that path.
    pub index: usize,
    /// The `audio_asset` row of the new audio, when the library has that
    /// audio already.
    pub audio: Option<i64>,
    /// What the new file's claims say.
    pub facts: Facts,
}

/// Says, for each file whose audio was replaced, whether the item is kept.
/// The new audio is resolved as if it were a new file, before the item's
/// own assignment is consulted. If it lands on the recording the item
/// already has, as a re-rip or a re-encode does, the item is kept. If not,
/// a different song was saved over the old name, and the item is to be
/// retired. See "Rescans and file moves" in docs/internals/identity.md.
pub fn judge_replacements(
    library: &Library,
    replacements: &[Replacement],
    policy: &Policy,
) -> Vec<bool> {
    let mut probes: Vec<Facts> = replacements.iter().map(|r| r.facts.clone()).collect();
    let implied = crate::discs::apply_with_probes(library, policy, &mut probes);
    let mut fold = Fold::new(implied.as_ref().unwrap_or(library), policy);
    let bounds = fold.run();
    replacements
        .iter()
        .zip(probes)
        .map(|(replacement, facts)| {
            let Some(had) = bounds.get(replacement.index).and_then(|bound| bound.rec) else {
                // There was no song at the path before.
                return true;
            };
            let as_new = Item {
                // No row of its own: no exclusion and no assignment is its.
                id: i64::MIN,
                folder: String::new(),
                // Audio the library has never seen matches no asset.
                audio: Some(replacement.audio.unwrap_or(i64::MIN)),
                lossless: false,
                retired: false,
                first_seen_ms: i64::MAX,
                cover: None,
                facts,
                current: None,
            };
            let probe = Ev::of(as_new.id, &as_new.facts, policy, None);
            matches!(fold.decide(&as_new, &probe, true), Outcome::Bind { rec, .. } if rec == had)
        })
        .collect()
}
