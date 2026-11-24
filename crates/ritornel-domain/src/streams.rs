//! Spotify's streams beside Last.fm's scrobbles: which stream and which
//! scrobble are one listen, and whether a stream that has no scrobble
//! counts. Pure: the engine reads the records and writes what this says.
//! See "Spotify streams" in docs/internals/history.md.
//!
//! Two records are one listen when their times and their names agree. The
//! library is never asked, so buying a record never changes which old
//! reports are one listen.

use std::collections::{BTreeMap, HashMap, HashSet};

use ritornel_contract::SpotifyStream;
use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

use crate::history::{Correction, Outcome, Verb, active_corrections};

/// How far a scrobble's stamp may be from a stream's worked-out start, or
/// past its end, for the two to be one listen.
pub const WITHIN_MS: i64 = 10_000;

/// How near a record of the same names must be for tier C to leave a pair
/// alone: a scrobbler that stamps each track's end puts every scrobble of
/// an album on the next track's start.
pub const PARTNER_WITHIN_MS: i64 = 60 * 60 * 1000;

/// What a stream must have played for tier C to take it, and what a stream
/// must have played to give its track a length.
pub const THIRTY_SECONDS_MS: i64 = 30_000;

/// A stream of this much or less never counts on its own: no track over
/// thirty seconds is half heard by then.
pub const NEVER_COUNTS_MS: i64 = 15_000;

/// Four minutes played counts whatever the track's length, as in play
/// rule 1.
pub const ENOUGH_MS: i64 = 240_000;

/// The `reason_end` that says a stream played to its end.
pub const TRACK_DONE: &str = "trackdone";

/// Which rule joined a scrobble to a stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Tier {
    /// A: the same names, stamped within ten seconds of the stream's
    /// worked-out start.
    Start,
    /// B: the same names, stamped while the stream played or up to ten
    /// seconds past its end.
    During,
    /// C: stamped as in A, with either the artist or the title the same,
    /// each the other's only candidate.
    Renamed,
}

impl Tier {
    /// Every tier, in the order they run.
    pub const ALL: [Self; 3] = [Self::Start, Self::During, Self::Renamed];

    /// The tier as the schema writes it.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::During => "during",
            Self::Renamed => "renamed",
        }
    }
}

/// Which tiers past A are in force. A always is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Tiers {
    /// Tier B.
    pub during: bool,
    /// Tier C.
    pub renamed: bool,
}

impl Tiers {
    /// The tiers in force: A alone, until B and C have each been judged on
    /// a sample of what they join.
    pub const CURRENT: Self = Self {
        during: false,
        renamed: false,
    };

    /// Every tier, for a dry run that says what each would join.
    pub const ALL: Self = Self {
        during: true,
        renamed: true,
    };
}

/// One stream, as the matcher reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stream {
    /// What it is told apart by, and ordered by where all else is equal:
    /// its source key.
    pub key: Vec<u8>,
    /// The stream as it was kept.
    pub record: SpotifyStream,
}

/// One scrobble that may be offered to a stream: not one excluded, and not
/// the echo of a listen of this library's own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scrobble {
    /// Its observation, which a correction names it by.
    pub observation: i64,
    /// What it is told apart by, and ordered by where all else is equal.
    pub key: Vec<u8>,
    /// When the scrobbler stamped it, in UTC.
    pub at_ms: i64,
    /// The artist, as it was scrobbled.
    pub artist: String,
    /// The title, as it was scrobbled.
    pub title: String,
}

/// Stream and scrobble pairs that a correction has said are one listen or
/// are not: a stream by its key and a scrobble by its observation, so that
/// the same pins hold in any list of streams and scrobbles. A pin whose
/// stream or scrobble is not in what the matcher is given pins nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pins {
    /// Never joined.
    pub apart: HashSet<(Vec<u8>, i64)>,
    /// Joined before any tier runs, each scrobble to the event of its
    /// stream, in order: of two that join one scrobble, the first takes it.
    pub together: Vec<(Vec<u8>, i64)>,
}

/// The pins that the `join` and `split` corrections standing now make,
/// among these stored streams, each given with its observation. A target
/// that is none of the streams is read as a scrobble.
///
/// Of two corrections that pin one listen and one scrobble, by any row of
/// the listen, the newer says it; and of two that join one scrobble, the
/// newer is read first and takes it.
pub fn pins<'a>(
    corrections: &[Correction],
    streams: impl IntoIterator<Item = (i64, &'a Stream)>,
) -> Pins {
    let active = active_corrections(corrections);
    let mut in_order: Vec<&Correction> = corrections
        .iter()
        .filter(|correction| {
            active.contains(&correction.observation)
                && matches!(correction.verb, Verb::Join | Verb::Split)
        })
        .collect();
    if in_order.is_empty() {
        return Pins::default();
    }
    in_order.sort_by_key(|correction| correction.observation);
    let streams: HashMap<i64, &Stream> = streams.into_iter().collect();
    type Listen = (i64, i64, String);
    let mut newest: HashMap<(Listen, i64), (i64, bool, &[u8])> = HashMap::new();
    for correction in in_order {
        for target in &correction.targets {
            let Some(stream) = streams.get(target) else {
                continue;
            };
            let r = &stream.record;
            let listen = (r.ended_at, r.ms_played, r.track_uri.clone());
            for scrobble in correction
                .targets
                .iter()
                .filter(|target| !streams.contains_key(target))
            {
                newest.insert(
                    (listen.clone(), *scrobble),
                    (
                        correction.observation,
                        correction.verb == Verb::Join,
                        stream.key.as_slice(),
                    ),
                );
            }
        }
    }
    let mut pins = Pins::default();
    let mut together: Vec<(i64, &[u8], i64)> = Vec::new();
    for ((_, scrobble), (by, join, stream)) in newest {
        if join {
            together.push((by, stream, scrobble));
        } else {
            pins.apart.insert((stream.to_vec(), scrobble));
        }
    }
    // Newest first, and in one correction by the stream's key and the
    // scrobble's observation.
    together.sort_unstable_by(|a, b| b.0.cmp(&a.0).then((a.1, a.2).cmp(&(b.1, b.2))));
    pins.together = together
        .into_iter()
        .map(|(_, stream, scrobble)| (stream.to_vec(), scrobble))
        .collect();
    pins
}

/// One listen as Spotify reported it: a stream, and every other row of the
/// archive with the same end, time played and track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    /// The stream that speaks for it, and the streams that repeat it, by
    /// index, the first first.
    pub streams: Vec<usize>,
    /// Its worked-out start: its end less the time played.
    pub start_ms: i64,
    /// Its end.
    pub end_ms: i64,
}

/// A scrobble joined to an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Join {
    /// The event, by index into [`Matched::events`].
    pub event: usize,
    /// The scrobble, by index.
    pub scrobble: usize,
    /// The rule that joined them. `None` for a pin.
    pub tier: Option<Tier>,
    /// Whether the event already had its scrobble, so that this one is a
    /// second report of the same listen.
    pub second: bool,
}

/// What the matcher found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Matched {
    /// Every event, in the order of its first stream's key.
    pub events: Vec<Event>,
    /// Every join, in the order made.
    pub joins: Vec<Join>,
}

impl Matched {
    /// The scrobble that is each event's own, by event: the first joined
    /// to it.
    pub fn scrobble_of(&self) -> HashMap<usize, usize> {
        let mut of = HashMap::new();
        for join in &self.joins {
            if !join.second {
                of.entry(join.event).or_insert(join.scrobble);
            }
        }
        of
    }
}

/// The key two names are compared by: Unicode NFKC, case folded, and no
/// white space or punctuation, brackets of every kind among it. Accents,
/// symbols, credits and version words stay, so a live take is never the
/// studio one.
pub fn name_key(text: &str) -> String {
    let mut folded = String::with_capacity(text.len());
    for c in text.nfkc().flat_map(char::to_lowercase) {
        match c {
            // Case folding beyond lower case: a final sigma is a sigma, and
            // a sharp s is two.
            'ς' => folded.push('σ'),
            'ß' => folded.push_str("ss"),
            c if c.is_alphanumeric() || is_combining_mark(c) || is_kept_symbol(c) => {
                folded.push(c);
            }
            _ => {}
        }
    }
    folded.nfc().collect()
}

/// Whether a mark that is neither a letter nor a digit is a symbol that
/// tells two names apart, as `+` or `★` does, and not punctuation.
fn is_kept_symbol(c: char) -> bool {
    crate::normalize::is_symbol(c) || matches!(c, '^' | '`' | '|' | '~')
}

/// What a stream's names are compared by.
fn names(record: &SpotifyStream) -> (String, String) {
    (name_key(&record.artist), name_key(&record.title))
}

/// A stream's record as it is kept: what Spotify said of the listen,
/// without what a reader said it was played on, which is kept apart.
#[must_use]
pub fn kept(record: &SpotifyStream) -> SpotifyStream {
    SpotifyStream {
        platform: None,
        device_kind: None,
        via: None,
        ..record.clone()
    }
}

/// A field that says something before one that says nothing.
fn said<T: Ord>(field: Option<T>) -> (bool, Option<T>) {
    (field.is_none(), field)
}

/// The order of records that repeat one another: their other kept fields,
/// a field that says something before one that does not, and their names
/// only where all of those are the same, so that which speaks for a listen
/// never depends on the order the rows came in. The device it played on is
/// no part of it, since a later reader may say it otherwise.
pub fn repeat_order(r: &SpotifyStream) -> impl Ord + '_ {
    (
        (
            said(r.reason_start.as_deref()),
            said(r.reason_end.as_deref()),
            said(r.shuffle),
            said(r.skipped),
            said(r.offline),
            said(r.offline_timestamp),
            said(r.incognito),
        ),
        (
            r.title.as_str(),
            r.artist.as_str(),
            said(r.album.as_deref()),
        ),
    )
}

/// The events these streams make: each is one listen Spotify may have
/// reported more than once, the rows with the same end, time played and
/// track.
pub fn events(streams: &[Stream]) -> Vec<Event> {
    type Members<'a> = Vec<(usize, &'a Stream)>;
    let mut by: BTreeMap<(i64, i64, &str), Members<'_>> = BTreeMap::new();
    for (at, stream) in streams.iter().enumerate() {
        let r = &stream.record;
        by.entry((r.ended_at, r.ms_played, r.track_uri.as_str()))
            .or_default()
            .push((at, stream));
    }
    let mut events: Vec<(&[u8], Event)> = by
        .into_iter()
        .filter_map(|((end_ms, ms_played, _), mut members)| {
            members.sort_by(|(_, a), (_, b)| {
                repeat_order(&a.record)
                    .cmp(&repeat_order(&b.record))
                    .then_with(|| a.key.cmp(&b.key))
            });
            let first = members.first()?.1.key.as_slice();
            Some((
                first,
                Event {
                    streams: members.iter().map(|(at, _)| *at).collect(),
                    start_ms: end_ms.saturating_sub(ms_played),
                    end_ms,
                },
            ))
        })
        .collect();
    events.sort_by_key(|(key, _)| *key);
    events.into_iter().map(|(_, event)| event).collect()
}

/// One possible pair: how far apart in time, and the keys that break ties.
struct Edge<'a> {
    distance: i64,
    scrobble_key: &'a [u8],
    event_key: &'a [u8],
    scrobble: usize,
    event: usize,
}

/// How far apart two instants are, without overflow.
fn distance(a: i64, b: i64) -> i64 {
    i64::try_from((i128::from(a) - i128::from(b)).unsigned_abs()).unwrap_or(i64::MAX)
}

/// Takes pairs one to one, the closest first and the lower keys on a tie,
/// and returns those taken.
fn one_to_one(
    mut edges: Vec<Edge<'_>>,
    scrobble_free: &mut [bool],
    event_free: &mut [bool],
) -> Vec<(usize, usize)> {
    edges.sort_by(|a, b| {
        (a.distance, a.scrobble_key, a.event_key).cmp(&(b.distance, b.scrobble_key, b.event_key))
    });
    let mut taken = Vec::new();
    for edge in edges {
        let free = |flags: &[bool], at: usize| flags.get(at).copied().unwrap_or(false);
        if free(scrobble_free, edge.scrobble) && free(event_free, edge.event) {
            if let Some(flag) = scrobble_free.get_mut(edge.scrobble) {
                *flag = false;
            }
            if let Some(flag) = event_free.get_mut(edge.event) {
                *flag = false;
            }
            taken.push((edge.scrobble, edge.event));
        }
    }
    taken
}

/// The events of each pair of names, each with its start, by start.
type ByNames<'n> = HashMap<&'n (String, String), Vec<(i64, usize)>>;

/// The events of these names that start between `from` and `to`, with their
/// starts.
fn near<'a>(
    by_names: &'a ByNames<'_>,
    names: &(String, String),
    from: i64,
    to: i64,
) -> &'a [(i64, usize)] {
    let Some(list) = by_names.get(names) else {
        return &[];
    };
    let first = list.partition_point(|(start, _)| *start < from);
    let last = list.partition_point(|(start, _)| *start <= to);
    list.get(first..last).unwrap_or_default()
}

/// Matches scrobbles to the events of these streams: pins first, then tier
/// A, then B and C where they are in force, each over what the last left.
/// The result depends on the records alone, never on the order they are
/// given in.
pub fn match_streams(
    streams: &[Stream],
    scrobbles: &[Scrobble],
    tiers: Tiers,
    pins: &Pins,
) -> Matched {
    let events = events(streams);
    let event_key = |event: usize| -> &[u8] {
        events
            .get(event)
            .and_then(|e| e.streams.first())
            .and_then(|at| streams.get(*at))
            .map_or(&[], |stream| stream.key.as_slice())
    };
    let event_names: Vec<(String, String)> = events
        .iter()
        .map(|e| {
            e.streams
                .first()
                .and_then(|at| streams.get(*at))
                .map(|stream| names(&stream.record))
                .unwrap_or_default()
        })
        .collect();
    let event_played: Vec<i64> = events
        .iter()
        .map(|e| e.end_ms.saturating_sub(e.start_ms))
        .collect();
    let scrobble_names: Vec<(String, String)> = scrobbles
        .iter()
        .map(|s| (name_key(&s.artist), name_key(&s.title)))
        .collect();
    // A pin names a stream; it stands for the stream's event. Streams of
    // one key are one listen, so any of them stands for it. With no pins,
    // nothing is looked up.
    let (event_by_key, scrobble_at): (HashMap<&[u8], usize>, HashMap<i64, usize>) =
        if pins.apart.is_empty() && pins.together.is_empty() {
            (HashMap::new(), HashMap::new())
        } else {
            (
                events
                    .iter()
                    .enumerate()
                    .flat_map(|(event, e)| {
                        e.streams.iter().filter_map(move |at| {
                            streams
                                .get(*at)
                                .map(|stream| (stream.key.as_slice(), event))
                        })
                    })
                    .collect(),
                scrobbles
                    .iter()
                    .enumerate()
                    .map(|(at, scrobble)| (scrobble.observation, at))
                    .collect(),
            )
        };
    let pinned = |stream: &Vec<u8>, scrobble: &i64| {
        Some((
            *event_by_key.get(stream.as_slice())?,
            *scrobble_at.get(scrobble)?,
        ))
    };
    let apart: HashSet<(usize, usize)> = pins
        .apart
        .iter()
        .filter_map(|(stream, scrobble)| pinned(stream, scrobble))
        .collect();

    let mut scrobble_free = vec![true; scrobbles.len()];
    let mut event_free = vec![true; events.len()];
    let mut joins = Vec::new();

    for (stream, scrobble) in &pins.together {
        let Some((event, scrobble)) = pinned(stream, scrobble) else {
            continue;
        };
        if !scrobble_free.get(scrobble).copied().unwrap_or(false) {
            continue;
        }
        let second = !event_free.get(event).copied().unwrap_or(false);
        if let Some(flag) = scrobble_free.get_mut(scrobble) {
            *flag = false;
        }
        if let Some(flag) = event_free.get_mut(event) {
            *flag = false;
        }
        joins.push(Join {
            event,
            scrobble,
            tier: None,
            second,
        });
    }

    // The events of each pair of names, with their starts, by start, for
    // finding those near a scrobble.
    let mut by_names: ByNames<'_> = HashMap::new();
    for (event, names) in event_names.iter().enumerate() {
        let start = events.get(event).map_or(0, |e| e.start_ms);
        by_names.entry(names).or_default().push((start, event));
    }
    for list in by_names.values_mut() {
        list.sort_by_key(|(start, _)| *start);
    }
    let allowed = |event: usize, scrobble: usize| !apart.contains(&(event, scrobble));

    // Tier A.
    let mut edges = Vec::new();
    for (scrobble, s) in scrobbles.iter().enumerate() {
        if !scrobble_free.get(scrobble).copied().unwrap_or(false) {
            continue;
        }
        let Some(names) = scrobble_names.get(scrobble) else {
            continue;
        };
        for &(start, event) in near(
            &by_names,
            names,
            s.at_ms.saturating_sub(WITHIN_MS),
            s.at_ms.saturating_add(WITHIN_MS),
        ) {
            if !allowed(event, scrobble) {
                continue;
            }
            edges.push(Edge {
                distance: distance(s.at_ms, start),
                scrobble_key: &s.key,
                event_key: event_key(event),
                scrobble,
                event,
            });
        }
    }
    for (scrobble, event) in one_to_one(edges, &mut scrobble_free, &mut event_free) {
        joins.push(Join {
            event,
            scrobble,
            tier: Some(Tier::Start),
            second: false,
        });
    }

    // Tier B: a scrobble stamped while the stream played. An event with no
    // scrobble takes it as its own, stamped late, when it played longer than
    // any play that never counts, so a skip takes no scrobble from the play
    // after it; then an event that has one takes it as a second report,
    // stamped from as early as tier A takes one.
    if tiers.during {
        let longest = event_played.iter().copied().max().unwrap_or(0);
        for second in [false, true] {
            let mut edges = Vec::new();
            for (scrobble, s) in scrobbles.iter().enumerate() {
                if !scrobble_free.get(scrobble).copied().unwrap_or(false) {
                    continue;
                }
                let Some(names) = scrobble_names.get(scrobble) else {
                    continue;
                };
                let from = s.at_ms.saturating_sub(longest).saturating_sub(WITHIN_MS);
                let to = if second {
                    s.at_ms.saturating_add(WITHIN_MS)
                } else {
                    s.at_ms
                };
                for &(_, event) in near(&by_names, names, from, to) {
                    let Some(e) = events.get(event) else {
                        continue;
                    };
                    let has_own = !event_free.get(event).copied().unwrap_or(false);
                    let earliest = if second {
                        e.start_ms.saturating_sub(WITHIN_MS)
                    } else {
                        e.start_ms
                    };
                    if has_own != second
                        || !allowed(event, scrobble)
                        || s.at_ms < earliest
                        || s.at_ms > e.end_ms.saturating_add(WITHIN_MS)
                        || (!second && e.end_ms.saturating_sub(e.start_ms) <= NEVER_COUNTS_MS)
                    {
                        continue;
                    }
                    edges.push(Edge {
                        distance: distance(s.at_ms, e.end_ms),
                        scrobble_key: &s.key,
                        event_key: event_key(event),
                        scrobble,
                        event,
                    });
                }
            }
            // A second report is one more for an event that has its own:
            // each takes one at most.
            let mut free: Vec<bool> = if second {
                vec![true; events.len()]
            } else {
                event_free.clone()
            };
            for (scrobble, event) in one_to_one(edges, &mut scrobble_free, &mut free) {
                if let Some(flag) = event_free.get_mut(event) {
                    *flag = false;
                }
                joins.push(Join {
                    event,
                    scrobble,
                    tier: Some(Tier::During),
                    second,
                });
            }
        }
    }

    // Tier C: renamed. Either the artist or the title is the same, the
    // stamps are as close as in A, each is the other's only candidate, and
    // neither has a record of its own names within the hour.
    if tiers.renamed {
        // Every event by its start, for finding those near a stamp.
        let mut by_start: Vec<(i64, usize)> = events
            .iter()
            .enumerate()
            .map(|(event, e)| (e.start_ms, event))
            .collect();
        by_start.sort_unstable();
        let mut scrobbles_by_names: HashMap<&(String, String), Vec<i64>> = HashMap::new();
        for (scrobble, names) in scrobble_names.iter().enumerate() {
            if let Some(s) = scrobbles.get(scrobble) {
                scrobbles_by_names.entry(names).or_default().push(s.at_ms);
            }
        }
        for stamps in scrobbles_by_names.values_mut() {
            stamps.sort_unstable();
        }
        let scrobble_partner = |names: &(String, String), at: i64| {
            !near(
                &by_names,
                names,
                at.saturating_sub(PARTNER_WITHIN_MS),
                at.saturating_add(PARTNER_WITHIN_MS),
            )
            .is_empty()
        };
        let event_partner = |names: &(String, String), start: i64| {
            scrobbles_by_names.get(names).is_some_and(|stamps| {
                let first =
                    stamps.partition_point(|at| *at < start.saturating_sub(PARTNER_WITHIN_MS));
                stamps
                    .get(first)
                    .is_some_and(|at| *at <= start.saturating_add(PARTNER_WITHIN_MS))
            })
        };
        let mut candidates: Vec<(usize, usize, i64)> = Vec::new();
        for (scrobble, s) in scrobbles.iter().enumerate() {
            if !scrobble_free.get(scrobble).copied().unwrap_or(false) {
                continue;
            }
            let Some(names) = scrobble_names.get(scrobble) else {
                continue;
            };
            let first =
                by_start.partition_point(|(start, _)| *start < s.at_ms.saturating_sub(WITHIN_MS));
            let last =
                by_start.partition_point(|(start, _)| *start <= s.at_ms.saturating_add(WITHIN_MS));
            for (start, event) in by_start.get(first..last).unwrap_or_default() {
                let (Some(e), Some(own)) = (events.get(*event), event_names.get(*event)) else {
                    continue;
                };
                // One of the two agrees, and a name that folds to nothing,
                // as one of punctuation alone does, agrees with none.
                let artist = !names.0.is_empty() && own.0 == names.0;
                let title = !names.1.is_empty() && own.1 == names.1;
                let renamed = artist != title && own != names;
                if renamed
                    && e.end_ms.saturating_sub(e.start_ms) >= THIRTY_SECONDS_MS
                    && event_free.get(*event).copied().unwrap_or(false)
                    && allowed(*event, scrobble)
                {
                    candidates.push((scrobble, *event, distance(s.at_ms, *start)));
                }
            }
        }
        let mut per_scrobble: HashMap<usize, usize> = HashMap::new();
        let mut per_event: HashMap<usize, usize> = HashMap::new();
        for (scrobble, event, _) in &candidates {
            *per_scrobble.entry(*scrobble).or_default() += 1;
            *per_event.entry(*event).or_default() += 1;
        }
        let mut edges = Vec::new();
        for (scrobble, event, distance) in candidates {
            let only = per_scrobble.get(&scrobble) == Some(&1) && per_event.get(&event) == Some(&1);
            let (Some(s), Some(e), Some(names), Some(own)) = (
                scrobbles.get(scrobble),
                events.get(event),
                scrobble_names.get(scrobble),
                event_names.get(event),
            ) else {
                continue;
            };
            if only && !scrobble_partner(names, s.at_ms) && !event_partner(own, e.start_ms) {
                edges.push(Edge {
                    distance,
                    scrobble_key: &s.key,
                    event_key: event_key(event),
                    scrobble,
                    event,
                });
            }
        }
        for (scrobble, event) in one_to_one(edges, &mut scrobble_free, &mut event_free) {
            joins.push(Join {
                event,
                scrobble,
                tier: Some(Tier::Renamed),
                second: false,
            });
        }
    }

    Matched { events, joins }
}

/// A track's length, as its streams say it: the median time played among
/// its events that ran to their end and played over thirty seconds, the
/// lower of two middles. By track URI.
pub fn lengths(streams: &[Stream], events: &[Event]) -> HashMap<String, i64> {
    let mut played: HashMap<&str, Vec<i64>> = HashMap::new();
    for event in events {
        let Some(stream) = event.streams.first().and_then(|at| streams.get(*at)) else {
            continue;
        };
        let r = &stream.record;
        if r.reason_end.as_deref() == Some(TRACK_DONE) && r.ms_played > THIRTY_SECONDS_MS {
            played
                .entry(r.track_uri.as_str())
                .or_default()
                .push(r.ms_played);
        }
    }
    played
        .into_iter()
        .filter_map(|(track, mut ms)| {
            ms.sort_unstable();
            let middle = ms.len().checked_sub(1)? / 2;
            Some((track.to_owned(), *ms.get(middle)?))
        })
        .collect()
}

/// Whether a stream's `reason_end` says the listener moved on to something
/// else, so that a stream that does not count is a skip.
pub fn moved_on(reason_end: Option<&str>) -> bool {
    matches!(
        reason_end,
        Some("fwdbtn" | "backbtn" | "clickrow" | "playbtn" | "uriopen" | "popup" | "endplay")
    )
}

/// Judges a stream that has no scrobble: whether it counts, and what
/// became of it. `length` is its track's length from [`lengths`].
pub fn judge_alone(
    ms_played: i64,
    length: Option<i64>,
    reason_end: Option<&str>,
) -> (bool, Outcome) {
    let not_counted = if moved_on(reason_end) {
        Outcome::Skipped
    } else {
        Outcome::Partial
    };
    if ms_played >= ENOUGH_MS {
        return (true, Outcome::Listened);
    }
    if ms_played <= NEVER_COUNTS_MS {
        return (false, not_counted);
    }
    match length {
        Some(length) if i128::from(ms_played) * 2 >= i128::from(length) => {
            (true, Outcome::Listened)
        }
        Some(_) => (false, not_counted),
        None => (false, Outcome::Unknown),
    }
}

/// How many scrobbles each tier joined, and how many of them were second
/// reports, by tier, for a report.
pub fn counts(matched: &Matched) -> BTreeMap<Tier, (u64, u64)> {
    let mut counts: BTreeMap<Tier, (u64, u64)> = BTreeMap::new();
    for join in &matched.joins {
        if let Some(tier) = join.tier {
            let entry = counts.entry(tier).or_default();
            if join.second {
                entry.1 += 1;
            } else {
                entry.0 += 1;
            }
        }
    }
    counts
}

#[cfg(test)]
mod tests {

    use super::*;

    /// An invented stream of `title` by Calla Brandt that ended at `end_s`
    /// seconds after an arbitrary evening and played `played_s` seconds.
    fn stream(title: &str, end_s: i64, played_s: i64) -> Stream {
        let ended_at = 1_552_680_000_000 + end_s * 1000;
        let ms_played = played_s * 1000;
        let track_uri = format!(
            "spotify:track:{:0>22}",
            title.len() * 7 + title.chars().count()
        );
        let track_uri = format!("{track_uri}{}", title.replace(' ', ""));
        Stream {
            key: format!("sp1:{ended_at}:{ms_played}:{track_uri}:0").into_bytes(),
            record: SpotifyStream {
                ended_at,
                ms_played,
                track_uri,
                title: title.to_owned(),
                artist: "Calla Brandt".to_owned(),
                album: Some("Glasshouse".to_owned()),
                reason_start: Some("trackdone".to_owned()),
                reason_end: Some(TRACK_DONE.to_owned()),
                shuffle: Some(false),
                skipped: Some(false),
                offline: Some(false),
                offline_timestamp: None,
                incognito: Some(false),
                platform: None,
                device_kind: None,
                via: None,
            },
        }
    }

    fn scrobble(title: &str, at_s: i64) -> Scrobble {
        Scrobble {
            observation: at_s,
            key: format!("{at_s:012}:{title}").into_bytes(),
            at_ms: 1_552_680_000_000 + at_s * 1000,
            artist: "Calla Brandt".to_owned(),
            title: title.to_owned(),
        }
    }

    /// The joins as (stream title, scrobble index, tier, second), for
    /// comparing.
    fn joins(streams: &[Stream], matched: &Matched) -> Vec<(String, usize, Option<Tier>, bool)> {
        let mut joins: Vec<_> = matched
            .joins
            .iter()
            .map(|join| {
                let event = &matched.events[join.event];
                let title = streams[event.streams[0]].record.title.clone();
                (title, join.scrobble, join.tier, join.second)
            })
            .collect();
        joins.sort();
        joins
    }

    fn all(streams: &[Stream], scrobbles: &[Scrobble]) -> Vec<(String, usize, Option<Tier>, bool)> {
        joins(
            streams,
            &match_streams(streams, scrobbles, Tiers::ALL, &Pins::default()),
        )
    }

    #[test]
    fn names_are_compared_without_case_spacing_or_punctuation() {
        assert_eq!(name_key("Low Tide"), name_key("low-tide"));
        assert_eq!(name_key("Paper Lanterns!"), name_key("PAPER  LANTERNS"));
        assert_eq!(name_key("ＬＯＷ ＴＩＤＥ"), name_key("low tide"));
        // Accents, credits, version words and symbols stay.
        assert_ne!(name_key("Café"), name_key("Cafe"));
        assert_ne!(name_key("Low Tide - Live"), name_key("Low Tide"));
        assert_ne!(name_key("Night Drives +"), name_key("Night Drives"));
        // Brackets of every kind are punctuation, and case is folded.
        assert_eq!(name_key("Low Tide ❨Live❩"), name_key("Low Tide (Live)"));
        assert_eq!(name_key("ΟΔΟΣ"), name_key("οδος"));
        assert_eq!(name_key("Straße"), name_key("STRASSE"));
    }

    #[test]
    fn a_scrobble_joins_the_stream_it_was_stamped_at_the_start_of() {
        // The stream ended at 21:14:09 after 3:41: its start was 21:10:28,
        // and the scrobble was stamped two seconds before.
        let streams = [stream("Low Tide", 221, 221)];
        let scrobbles = [scrobble("Low Tide", -2)];
        assert_eq!(
            all(&streams, &scrobbles),
            [("Low Tide".to_owned(), 0, Some(Tier::Start), false)]
        );
        // Eleven seconds is too far, and another title is not it.
        let scrobbles = [scrobble("Low Tide", -11), scrobble("Paper Lanterns", 0)];
        let matched = match_streams(&streams, &scrobbles, Tiers::CURRENT, &Pins::default());
        assert!(matched.joins.is_empty());
    }

    #[test]
    fn back_to_back_plays_of_one_track_each_take_their_own_scrobble() {
        let streams = [stream("Low Tide", 200, 200), stream("Low Tide", 400, 200)];
        let scrobbles = [scrobble("Low Tide", 201), scrobble("Low Tide", 1)];
        let matched = match_streams(&streams, &scrobbles, Tiers::CURRENT, &Pins::default());
        let mut pairs: Vec<(i64, usize)> = matched
            .joins
            .iter()
            .map(|join| (matched.events[join.event].start_ms, join.scrobble))
            .collect();
        pairs.sort_unstable();
        assert_eq!(pairs, [(1_552_680_000_000, 1), (1_552_680_200_000, 0)]);
    }

    #[test]
    fn a_repeated_row_is_one_event_and_the_first_by_its_other_fields_speaks() {
        let first = stream("Low Tide", 221, 221);
        let mut again = first.clone();
        again.key = b"sp1:again".to_vec();
        let mut other = first.clone();
        other.key = b"sp1:other".to_vec();
        other.record.reason_end = Some("endplay".to_owned());
        let streams = [other, again, first];
        let events = events(&streams);
        assert_eq!(events.len(), 1);
        // "endplay" sorts before "trackdone", then the keys: the first's
        // key, of digits, before "sp1:again".
        assert_eq!(events[0].streams, [0, 2, 1]);
        let matched = match_streams(
            &streams,
            &[scrobble("Low Tide", 0)],
            Tiers::ALL,
            &Pins::default(),
        );
        assert_eq!(matched.joins.len(), 1);
    }

    #[test]
    fn a_pause_moves_the_worked_out_start_past_what_tier_a_takes() {
        // Played 3:41 of a track with a pause of a minute: the worked-out
        // start is a minute after the scrobble's stamp, which no tier takes
        // and the dry run counts as where a pause would put it. A scrobble
        // stamped while the stream played is tier B's.
        let streams = [stream("Low Tide", 281, 221)];
        let scrobbles = [scrobble("Low Tide", 0)];
        assert!(
            match_streams(&streams, &scrobbles, Tiers::CURRENT, &Pins::default())
                .joins
                .is_empty()
        );
        // Stamped before the worked-out start: B does not take it either.
        assert!(all(&streams, &scrobbles).is_empty());
        let scrobbles = [scrobble("Low Tide", 150)];
        assert_eq!(
            all(&streams, &scrobbles),
            [("Low Tide".to_owned(), 0, Some(Tier::During), false)]
        );
    }

    #[test]
    fn a_second_report_at_the_end_corroborates_the_play_it_repeats() {
        let streams = [stream("Low Tide", 221, 221)];
        let scrobbles = [scrobble("Low Tide", 1), scrobble("Low Tide", 225)];
        assert_eq!(
            all(&streams, &scrobbles),
            [
                ("Low Tide".to_owned(), 0, Some(Tier::Start), false),
                ("Low Tide".to_owned(), 1, Some(Tier::During), true),
            ]
        );
        let matched = match_streams(&streams, &scrobbles, Tiers::ALL, &Pins::default());
        assert_eq!(matched.scrobble_of().get(&0), Some(&0));
    }

    #[test]
    fn a_skip_takes_no_scrobble_as_its_own_and_a_second_report_may_come_early() {
        // Skipped after two seconds; the scrobble stamped eleven seconds in
        // is not the skip's.
        let skip = [stream("Low Tide", 2, 2)];
        assert!(all(&skip, &[scrobble("Low Tide", 11)]).is_empty());
        // A second copy stamped two seconds before the start is one more
        // report of the play that took the first.
        let streams = [stream("Low Tide", 221, 221)];
        let mut copy = scrobble("Low Tide", -2);
        copy.key.push(b'2');
        assert_eq!(
            all(&streams, &[scrobble("Low Tide", -2), copy])
                .into_iter()
                .map(|(_, _, tier, second)| (tier, second))
                .collect::<Vec<_>>(),
            [(Some(Tier::Start), false), (Some(Tier::During), true)]
        );
    }

    #[test]
    fn a_renamed_scrobble_joins_only_as_the_only_candidate_with_no_partner_near() {
        let streams = [stream("Low Tide - Live", 221, 221)];
        let scrobbles = [scrobble("Low Tide", 0)];
        assert_eq!(
            all(&streams, &scrobbles),
            [("Low Tide - Live".to_owned(), 0, Some(Tier::Renamed), false)]
        );
        // Not in force, it joins nothing.
        assert!(
            match_streams(&streams, &scrobbles, Tiers::CURRENT, &Pins::default())
                .joins
                .is_empty()
        );
        // A stream of under thirty seconds is never taken.
        assert!(all(&[stream("Low Tide - Live", 20, 20)], &scrobbles).is_empty());
        // A scrobble with a stream of its own names within the hour is left
        // alone.
        let streams = [
            stream("Low Tide - Live", 221, 221),
            stream("Low Tide", 3000, 200),
        ];
        assert!(all(&streams, &scrobbles).is_empty());
    }

    #[test]
    fn an_album_scrobbled_at_each_tracks_end_is_not_moved_down_a_track() {
        // Three tracks back to back; the scrobbler stamped each at its end,
        // which is the next track's start.
        let titles = ["Low Tide", "Paper Lanterns", "Night Ferry"];
        let streams: Vec<Stream> = titles
            .iter()
            .enumerate()
            .map(|(at, title)| {
                let at = i64::try_from(at).unwrap();
                stream(title, (at + 1) * 200, 200)
            })
            .collect();
        let scrobbles: Vec<Scrobble> = titles
            .iter()
            .enumerate()
            .map(|(at, title)| scrobble(title, (i64::try_from(at).unwrap() + 1) * 200))
            .collect();
        // Each is during its own stream, at its end: B takes each to its
        // own track, and C finds nothing left that it may move.
        let joined = all(&streams, &scrobbles);
        assert_eq!(joined.len(), 3);
        for (title, scrobble, tier, second) in joined {
            assert_eq!(title, titles[scrobble]);
            assert_eq!((tier, second), (Some(Tier::During), false));
        }
        // With B left out, each scrobble sits on the next track's start
        // with the same artist, and C still moves none of them: each has a
        // stream of its own names within the hour.
        let c_alone = Tiers {
            during: false,
            renamed: true,
        };
        let matched = match_streams(&streams, &scrobbles, c_alone, &Pins::default());
        assert!(matched.joins.is_empty(), "{:?}", matched.joins);
    }

    #[test]
    fn tier_c_takes_only_a_lone_candidate_within_ten_seconds_with_no_partner() {
        let c_alone = Tiers {
            during: false,
            renamed: true,
        };
        let joined = |streams: &[Stream], scrobbles: &[Scrobble]| {
            match_streams(streams, scrobbles, c_alone, &Pins::default())
                .joins
                .len()
        };
        let live = || stream("Low Tide - Live", 221, 221);
        // At ten seconds it is taken; at eleven it is not.
        assert_eq!(joined(&[live()], &[scrobble("Low Tide", 10)]), 1);
        assert_eq!(joined(&[live()], &[scrobble("Low Tide", 11)]), 0);
        // Thirty seconds played is enough; less is not.
        assert_eq!(
            joined(
                &[stream("Low Tide - Live", 30, 30)],
                &[scrobble("Low Tide", 0)]
            ),
            1
        );
        assert_eq!(
            joined(
                &[stream("Low Tide - Live", 29, 29)],
                &[scrobble("Low Tide", 0)]
            ),
            0
        );
        // A renamed artist with the same title is a candidate too.
        let mut credited = scrobble("Low Tide", 0);
        credited.artist = "Calla Brandt feat. Odile Castellan".to_owned();
        assert_eq!(
            joined(&[live()], &[credited.clone()]),
            0,
            "two names differ"
        );
        assert_eq!(joined(&[stream("Low Tide", 221, 221)], &[credited]), 1);
        // Two candidates for one scrobble: neither is taken.
        let mut other = stream("Low Tide - Demo", 222, 222);
        other.key = b"sp1:other".to_vec();
        assert_eq!(joined(&[live(), other], &[scrobble("Low Tide", 0)]), 0);
        // The stream has a scrobble of its own names within the hour.
        assert_eq!(
            joined(
                &[live()],
                &[scrobble("Low Tide", 0), scrobble("Low Tide - Live", 3000)]
            ),
            0
        );
        // Names that fold to nothing agree with nothing.
        let mut blank = stream("...", 221, 221);
        blank.record.artist = "Odile Castellan".to_owned();
        let mut marks = scrobble("???", 0);
        marks.artist = "Søren Dunmore Ensemble".to_owned();
        assert_eq!(joined(&[blank], &[marks]), 0);
    }

    #[test]
    fn a_pin_apart_is_never_joined_and_a_pin_together_is_joined_first() {
        let streams = [stream("Low Tide", 221, 221)];
        let scrobbles = [scrobble("Low Tide", 0)];
        let key = streams[0].key.clone();
        let apart = Pins {
            apart: HashSet::from([(key.clone(), 0)]),
            together: Vec::new(),
        };
        assert!(
            match_streams(&streams, &scrobbles, Tiers::ALL, &apart)
                .joins
                .is_empty()
        );
        let together = Pins {
            apart: HashSet::new(),
            together: vec![(key, 5000)],
        };
        let far = [scrobble("Paper Lanterns", 5000)];
        let matched = match_streams(&streams, &far, Tiers::ALL, &together);
        assert_eq!(matched.joins.len(), 1);
        assert_eq!(matched.joins[0].tier, None);
    }

    #[test]
    fn a_correction_pins_its_stream_by_key_in_any_list_and_the_newer_says_it() {
        let low = stream("Low Tide", 221, 221);
        let lanterns = stream("Paper Lanterns", 900, 210);
        let (near, far) = (scrobble("Low Tide", 0), scrobble("Paper Lanterns", 5000));
        let correction = |observation, verb, targets: Vec<i64>| Correction {
            observation,
            verb,
            targets,
            window: None,
            recording: None,
        };
        // The streams' observations are 101 and 102; the scrobbles' are
        // their times, 0 and 5000.
        let mut corrections = vec![
            correction(200, Verb::Split, vec![101, 0]),
            correction(201, Verb::Join, vec![102, 5000]),
        ];
        let pinned = pins(&corrections, [(101, &low), (102, &lanterns)]);
        for streams in [
            vec![low.clone(), lanterns.clone()],
            vec![
                lanterns.clone(),
                stream("Quiet Room", 400, 200),
                low.clone(),
            ],
        ] {
            let scrobbles = [far.clone(), near.clone()];
            let matched = match_streams(&streams, &scrobbles, Tiers::ALL, &pinned);
            assert_eq!(
                joins(&streams, &matched),
                [("Paper Lanterns".to_owned(), 0, None, false)]
            );
        }
        // A newer split of the same pair undoes the join, and the older
        // split still keeps its own pair apart, which joins without it.
        corrections.push(correction(202, Verb::Split, vec![102, 5000]));
        let pinned = pins(&corrections, [(101, &low), (102, &lanterns)]);
        assert!(
            match_streams(&[lanterns], &[far], Tiers::ALL, &pinned)
                .joins
                .is_empty()
        );
        let (low, near) = ([low], [near]);
        assert!(
            match_streams(&low, &near, Tiers::ALL, &Pins::default())
                .joins
                .len()
                == 1
                && match_streams(&low, &near, Tiers::ALL, &pinned)
                    .joins
                    .is_empty()
        );
    }

    /// A join by the keys of its stream and scrobble, its tier, and whether
    /// it is a second report.
    type Pair = (Vec<u8>, Vec<u8>, Option<Tier>, bool);

    #[test]
    fn any_arrival_order_gives_the_same_joins() {
        let mut streams = vec![
            stream("Low Tide", 200, 200),
            stream("Low Tide", 205, 200),
            stream("Paper Lanterns", 420, 210),
            stream("Low Tide - Live", 700, 250),
        ];
        let mut again = streams[0].clone();
        again.key = b"sp1:again".to_vec();
        streams.push(again);
        let scrobbles = vec![
            scrobble("Low Tide", 2),
            scrobble("Low Tide", 4),
            scrobble("Paper Lanterns", 209),
            scrobble("Paper Lanterns", 425),
            scrobble("Low Tide", 451),
        ];
        let canonical = |streams: &[Stream], scrobbles: &[Scrobble]| {
            let matched = match_streams(streams, scrobbles, Tiers::ALL, &Pins::default());
            let mut pairs: Vec<Pair> = matched
                .joins
                .iter()
                .map(|join| {
                    let event = &matched.events[join.event];
                    (
                        streams[event.streams[0]].key.clone(),
                        scrobbles[join.scrobble].key.clone(),
                        join.tier,
                        join.second,
                    )
                })
                .collect();
            pairs.sort();
            pairs
        };
        let expected = canonical(&streams, &scrobbles);
        assert!(!expected.is_empty());
        for turn in 1..streams.len() {
            let mut streams = streams.clone();
            streams.rotate_left(turn);
            let mut scrobbles = scrobbles.clone();
            scrobbles.reverse();
            let count = scrobbles.len();
            scrobbles.rotate_left(turn % count);
            assert_eq!(canonical(&streams, &scrobbles), expected, "turn {turn}");
        }
    }

    #[test]
    fn a_tracks_length_is_the_median_of_its_streams_that_ran_to_the_end() {
        let mut streams = vec![
            stream("Low Tide", 100, 200),
            stream("Low Tide", 1000, 210),
            stream("Low Tide", 2000, 220),
            stream("Low Tide", 3000, 230),
        ];
        // Left early, or under thirty seconds, it says nothing of the length.
        streams.push(stream("Low Tide", 4000, 20));
        let mut left = stream("Low Tide", 5000, 100);
        left.record.reason_end = Some("fwdbtn".to_owned());
        streams.push(left);
        let events = events(&streams);
        let lengths = lengths(&streams, &events);
        // The lower of the two middles: 210 seconds.
        assert_eq!(lengths.get(&streams[0].record.track_uri), Some(&210_000));
    }

    #[test]
    fn a_stream_alone_counts_by_the_length_its_track_was_given() {
        assert_eq!(judge_alone(240_000, None, None), (true, Outcome::Listened));
        assert_eq!(
            judge_alone(15_000, Some(20_000), Some("fwdbtn")),
            (false, Outcome::Skipped)
        );
        assert_eq!(
            judge_alone(15_000, Some(20_000), Some("trackdone")),
            (false, Outcome::Partial)
        );
        assert_eq!(
            judge_alone(105_000, Some(210_000), None),
            (true, Outcome::Listened)
        );
        assert_eq!(
            judge_alone(104_999, Some(210_000), Some("fwdbtn")),
            (false, Outcome::Skipped)
        );
        assert_eq!(
            judge_alone(60_000, None, Some("fwdbtn")),
            (false, Outcome::Unknown)
        );
    }
}
