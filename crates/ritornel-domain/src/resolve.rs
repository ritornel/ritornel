//! Resolver policy 1: which recording each source item is. See "Resolver
//! policy 1" and "Merge, split and never-link" in docs/design/identity.md.
//!
//! Items are taken in first-seen order, and each sees only what the items
//! before it said. A recording is made by the first item that binds to
//! nothing, and that item keeps the recording made for it from one run to
//! the next, which is what makes a second run over the same library decide
//! the same thing.

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::extract::Facts;
use crate::library::{Candidate, Decision, Evidence, Item, Kind, Library, Method, Penalties};
use crate::normalize::{core, difference, normalize};

/// The version of this policy: `identity_decision.resolver_version`.
pub const RESOLVER_VERSION: i64 = 1;

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

/// The numbers policy 1 decides by. The thresholds are Beets' defaults
/// applied to a different scoring function: a starting point, tuned on a
/// real library.
#[derive(Debug, Clone, PartialEq)]
pub struct Policy {
    /// [`RESOLVER_VERSION`].
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
}

impl Policy {
    /// Policy 1.
    pub fn v1() -> Self {
        Self {
            version: RESOLVER_VERSION,
            bind_max: 0.04,
            min_gap: 0.25,
            review_below: 0.25,
            duration_grace_ms: 2_000,
            duration_span_ms: 20_000,
            identifier_duration_max_ms: 10_000,
            title_note_penalty: 0.1,
            weights: [3.0, 3.0, 3.0, 0.3],
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
    full: Option<String>,
    core: Option<String>,
    artist: Option<String>,
    album_full: Option<String>,
    album_core: Option<String>,
    duration: Option<i64>,
    markers: Vec<&'static str>,
    album_markers: Vec<&'static str>,
    mbid: Option<String>,
    ids: Vec<String>,
}

fn markers(key: Option<&str>, list: &'static [&'static str]) -> Vec<&'static str> {
    let mut found: Vec<&'static str> = Vec::new();
    for word in key.unwrap_or("").split(' ') {
        if let Some(marker) = list.iter().find(|marker| **marker == word)
            && !found.contains(marker)
        {
            found.push(marker);
        }
    }
    found.sort_unstable();
    found
}

impl Ev {
    fn of(item: i64, facts: &Facts) -> Self {
        let full = facts.title.as_deref().map(normalize);
        let album_full = facts.album.as_deref().map(normalize);
        Self {
            item,
            rec: usize::MAX,
            core: facts.title.as_deref().map(core),
            artist: facts.artist_text().as_deref().map(normalize),
            album_core: facts.album.as_deref().map(core),
            duration: facts.duration_ms,
            markers: markers(full.as_deref(), VERSION_MARKERS),
            album_markers: markers(album_full.as_deref(), ALBUM_MARKERS),
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
    },
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
            match kind {
                Kind::Recording => learn(&mut title_alias, a, b),
                Kind::Artist => learn(&mut artist_alias, a, b),
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
        match self.recs.get(rec).map(|rec| rec.key) {
            Some(RecKey::Row(row)) => self
                .excluded
                .get(&item)
                .is_some_and(|recordings| recordings.contains(&row)),
            _ => false,
        }
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
    /// thing, where they differ.
    fn aliases_from(&mut self, probe: &Ev, rec: usize) -> Vec<(Kind, String, String)> {
        let Some(first) = self.recs.get(rec).and_then(|rec| rec.first_ev) else {
            return Vec::new();
        };
        let Some(ev) = self.evs.get(first) else {
            return Vec::new();
        };
        let mut pairs = Vec::new();
        let (title, artist) = (ev.full.clone(), ev.artist.clone());
        for (kind, mine, theirs) in [
            (Kind::Recording, &probe.full, title),
            (Kind::Artist, &probe.artist, artist),
        ] {
            if let (Some(a), Some(b)) = (mine, theirs)
                && *a != b
            {
                let (a, b) = if *a < b {
                    (a.clone(), b)
                } else {
                    (b, a.clone())
                };
                pairs.push((kind, a, b));
            }
        }
        for (kind, a, b) in &pairs {
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
                if apart > self.policy.identifier_duration_max_ms {
                    contradictions.push(format!(
                        "mb_recording {mbid} is carried by item {} whose duration differs by {} ms",
                        ev.item, apart
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
        let mut claims = Vec::new();
        let mut candidates: Vec<(f64, Penalties, usize, i64)> = Vec::new();
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
            let mut best: HashMap<usize, (f64, Penalties, i64)> = HashMap::new();
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
                if let Some((distance, penalties)) = self.distance(probe, ev) {
                    let entry = best.entry(ev.rec).or_insert((distance, penalties, ev.item));
                    if distance < entry.0 {
                        *entry = (distance, penalties, ev.item);
                    }
                }
            }
            candidates = best
                .into_iter()
                .map(|(rec, (distance, penalties, item))| (distance, penalties, rec, item))
                .collect();
            candidates.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.2.cmp(&b.2)));
        }
        let listed: Vec<Candidate> = candidates
            .iter()
            .take(5)
            .map(|(distance, _, _, item)| Candidate {
                item: *item,
                distance: *distance,
            })
            .collect();
        let gap = match (candidates.first(), candidates.get(1)) {
            (Some(best), Some(next)) => Some(next.0 - best.0),
            _ => None,
        };
        if let Some((distance, penalties, rec, _)) = candidates.first().copied()
            && distance <= self.policy.bind_max
            && gap.is_none_or(|gap| gap >= self.policy.min_gap)
        {
            return Outcome::Bind {
                rec,
                decision: Decision {
                    method: Method::TagsDuration,
                    score: distance,
                    runner_up_gap: gap,
                    penalties: Some(penalties),
                    evidence: Evidence {
                        claims,
                        candidates: listed,
                        contradictions: Vec::new(),
                    },
                    needs_review: false,
                    aliases: Vec::new(),
                },
            };
        }

        // Step 4: nothing bound. Policy 1 never binds on a doubtful match:
        // it makes a recording and marks the decision for review.
        let doubtful = candidates
            .iter()
            .any(|(distance, ..)| *distance < self.policy.review_below);
        Outcome::New {
            decision: Decision {
                method: Method::New,
                score: 0.0,
                runner_up_gap: gap,
                penalties: None,
                needs_review: doubtful || !contradictions.is_empty(),
                evidence: Evidence {
                    claims,
                    candidates: listed,
                    contradictions,
                },
                aliases: Vec::new(),
            },
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
            let probe = Ev::of(item.id, &item.facts);
            let current = item.current;
            let (rec, decision, pinned, frozen) = match self.decide(item, &probe, false) {
                Outcome::Unbound => (None, None, false, true),
                Outcome::Keep { pinned } => {
                    let rec = current.map(|current| self.rec_for_row(current.recording));
                    (rec, None, pinned, true)
                }
                Outcome::Bind { rec, decision } => (Some(rec), Some(decision), false, false),
                Outcome::New { decision } => {
                    // An item whose recording was made for it keeps it.
                    let own = current
                        .filter(|current| current.method == Method::New && !current.by_user)
                        .map(|current| current.recording)
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
/// retired. See "Rescans" in docs/design/identity.md.
pub fn judge_replacements(
    library: &Library,
    replacements: &[Replacement],
    policy: &Policy,
) -> Vec<bool> {
    let mut fold = Fold::new(library, policy);
    let bounds = fold.run();
    replacements
        .iter()
        .map(|replacement| {
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
                facts: replacement.facts.clone(),
                current: None,
            };
            let probe = Ev::of(as_new.id, &as_new.facts);
            matches!(fold.decide(&as_new, &probe, true), Outcome::Bind { rec, .. } if rec == had)
        })
        .collect()
}
