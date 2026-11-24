//! What applying a run of Spotify streams would do, worked out without
//! writing anything: the dry run's report. See "Spotify streams" in
//! docs/internals/history.md.

use std::collections::{BTreeMap, HashMap, HashSet};

use jiff::Timestamp;
use jiff::tz::TimeZone;
use ritornel_contract::{
    ImportProblem, ImportReport, ImportYear, JoinSample, SpotifyStream, StreamTier, TierJoins,
};

use crate::history::{Correction, Outcome, ZonePeriod, assumed_zone};
use crate::streams::{
    Event, Matched, Scrobble, Stream, Tier, Tiers, judge_alone, kept, lengths, match_streams,
    name_key, repeat_order,
};

/// How many joins of tiers B and C a report shows, for judging them.
pub const SAMPLES_PER_TIER: usize = 25;

/// How near the end of its stream an offline time must be to read as one
/// unit or the other.
const OFFLINE_NEAR_MS: i64 = 24 * 60 * 60 * 1000;

/// Everything a dry run reads.
#[derive(Debug, Clone, Copy)]
pub struct DryRunInput<'a> {
    /// The run's streams, with their keys.
    pub streams: &'a [Stream],
    /// The listener's streams in force now, with their keys, which applying
    /// keeps in force beside the run's.
    pub in_force: &'a [Stream],
    /// Every other listener with a stream stored, with their streams in
    /// force and the scrobbles offered to them: a track's length is taken
    /// from their streams too, as derivation takes it from every stream in
    /// force, so a run can move their plays.
    pub others: &'a [OtherListener],
    /// The scrobbles that may be offered to a stream.
    pub scrobbles: &'a [Scrobble],
    /// Scrobbles set aside, which are offered to none.
    pub excluded: u64,
    /// Scrobbles that are this library's own listens come back.
    pub echoes: u64,
    /// The plays now, and of those the ones that count, by the year of
    /// their start in UTC.
    pub plays_by_year: &'a BTreeMap<i32, (u64, u64)>,
    /// Whose streams these are.
    pub listener: i64,
    /// The zones listeners were in.
    pub zone_periods: &'a [ZonePeriod],
    /// Every correction, of which the `join` and `split` corrections that
    /// stand pin a stored stream to a scrobble, as derivation reads them.
    pub corrections: &'a [Correction],
    /// The stored streams' observations, by key, which a correction names
    /// a stream by.
    pub observations: &'a HashMap<Vec<u8>, i64>,
    /// The streams stored already, by key, as they were stored.
    pub stored: &'a HashMap<Vec<u8>, SpotifyStream>,
    /// Whose each stored stream is, by key: a stream stays the listener's
    /// it was stored for.
    pub owners: &'a HashMap<Vec<u8>, i64>,
    /// The keys of the streams a correction that stands sets aside: a play
    /// of one is excluded, with the scrobbles joined to it.
    pub set_aside: &'a HashSet<Vec<u8>>,
    /// When the report is made.
    pub made_at: i64,
}

/// Another listener's streams in force and the scrobbles offered to them.
#[derive(Debug, Clone, Default)]
pub struct OtherListener {
    /// Who they are.
    pub listener: i64,
    /// Their streams in force, with their keys.
    pub streams: Vec<Stream>,
    /// Their scrobbles that may be offered to a stream.
    pub scrobbles: Vec<Scrobble>,
}

/// The row of `year`, made when it is the first of it.
fn row(years: &mut BTreeMap<i32, ImportYear>, year: i32) -> &mut ImportYear {
    years.entry(year).or_insert_with(|| ImportYear {
        year,
        ..ImportYear::default()
    })
}

/// The year an instant falls in, in UTC.
fn year(ms: i64) -> i32 {
    Timestamp::from_millisecond(ms)
        .map(|at| i32::from(at.to_zoned(TimeZone::UTC).year()))
        .unwrap_or(0)
}

fn contract_tier(tier: Tier) -> StreamTier {
    match tier {
        Tier::Start => StreamTier::Start,
        Tier::During => StreamTier::During,
        Tier::Renamed => StreamTier::Renamed,
    }
}

fn first_record<'a>(streams: &'a [Stream], event: &Event) -> Option<&'a SpotifyStream> {
    event
        .streams
        .first()
        .and_then(|at| streams.get(*at))
        .map(|stream| &stream.record)
}

/// The place a stream's key gives it: its last field.
fn place_of(key: &[u8]) -> Option<u32> {
    std::str::from_utf8(key)
        .ok()?
        .rsplit(':')
        .next()?
        .parse()
        .ok()
}

/// The key of a stream at a place: `sp1`, its end, its time played, its
/// track's ID and its place among the streams with the same three.
fn key_at(r: &SpotifyStream, place: u32) -> Vec<u8> {
    let id = r
        .track_uri
        .strip_prefix("spotify:track:")
        .unwrap_or(&r.track_uri);
    format!("sp1:{}:{}:{id}:{place}", r.ended_at, r.ms_played).into_bytes()
}

/// How near a stored record a row of an archive must be to take its place.
#[derive(Clone, Copy)]
enum Pass {
    /// The same record.
    Same,
    /// The same but for its names.
    Respelled,
    /// Any of its group.
    Any,
}

/// A record without its names.
fn unnamed(r: &SpotifyStream) -> SpotifyStream {
    SpotifyStream {
        title: String::new(),
        artist: String::new(),
        album: None,
        ..r.clone()
    }
}

/// The keys of an archive's streams, made from the records alone and from
/// the streams stored already, never from a clock of ours.
///
/// The streams with the same end, time played and track are one group, in
/// the order of their other kept fields. A stream takes the place of a
/// stored stream of its group with the same record as it is kept; then one
/// takes the place of a stored stream of its group that is left, first one
/// that differs in its names alone, as a row a later archive gives
/// otherwise, which keeps the record it was stored with; and the rest take
/// places no stored stream has. So the same archive gives the
/// same keys, and a later one gives the rows it shares with an earlier one
/// their keys, whatever rows it adds and wherever they sort.
pub fn keyed(records: Vec<SpotifyStream>, stored: &HashMap<Vec<u8>, SpotifyStream>) -> Vec<Stream> {
    type Group<'a> = (i64, i64, &'a str);
    let mut held: HashMap<Group<'_>, Vec<(u32, &SpotifyStream)>> = HashMap::new();
    for (key, record) in stored {
        if let Some(place) = place_of(key) {
            held.entry((record.ended_at, record.ms_played, record.track_uri.as_str()))
                .or_default()
                .push((place, record));
        }
    }
    for places in held.values_mut() {
        places.sort_by_key(|(place, _)| *place);
    }
    let mut groups: BTreeMap<Group<'_>, Vec<usize>> = BTreeMap::new();
    for (at, r) in records.iter().enumerate() {
        groups
            .entry((r.ended_at, r.ms_played, r.track_uri.as_str()))
            .or_default()
            .push(at);
    }
    let mut keys: Vec<Vec<u8>> = vec![Vec::new(); records.len()];
    for (group, mut members) in groups {
        members.sort_by(|a, b| match (records.get(*a), records.get(*b)) {
            (Some(a), Some(b)) => repeat_order(a).cmp(&repeat_order(b)),
            _ => std::cmp::Ordering::Equal,
        });
        let stored = held.get(&group).map(Vec::as_slice).unwrap_or_default();
        let mut taken: HashSet<u32> = HashSet::new();
        let mut place: Vec<Option<u32>> = vec![None; members.len()];
        // The same record first, then one spelled otherwise in its names
        // alone, then any left of the group.
        for pass in [Pass::Same, Pass::Respelled, Pass::Any] {
            for (slot, at) in place.iter_mut().zip(&members) {
                if slot.is_some() {
                    continue;
                }
                let Some(record) = records.get(*at) else {
                    continue;
                };
                let record = kept(record);
                let found = stored.iter().find(|(held_at, held)| {
                    !taken.contains(held_at)
                        && match pass {
                            Pass::Same => kept(held) == record,
                            Pass::Respelled => unnamed(&kept(held)) == unnamed(&record),
                            Pass::Any => true,
                        }
                });
                if let Some((held_at, _)) = found {
                    taken.insert(*held_at);
                    *slot = Some(*held_at);
                }
            }
        }
        let mut next = stored.iter().map(|(at, _)| at + 1).max().unwrap_or(0);
        for (slot, at) in place.into_iter().zip(&members) {
            let place = slot.unwrap_or_else(|| {
                next += 1;
                next - 1
            });
            if let (Some(record), Some(key)) = (records.get(*at), keys.get_mut(*at)) {
                *key = key_at(record, place);
            }
        }
    }
    records
        .into_iter()
        .zip(keys)
        .map(|(record, key)| Stream { key, record })
        .collect()
}

/// Up to `count` of `items`, spread evenly from first to last.
fn spread<T: Clone>(items: &[T], count: usize) -> Vec<T> {
    if items.len() <= count {
        return items.to_vec();
    }
    (0..count)
        .filter_map(|at| items.get(at * items.len() / count).cloned())
        .collect()
}

/// What streams add to each year's plays and counted plays, as matched: an
/// event alone is a play of its own, counted or not by the stream rule; a
/// second report's scrobble, a play of its own without them, joins the play
/// of the stream it repeats; and an event joined to a scrobble takes its
/// play's place, dated by the stream, which can be another year.
fn added(
    streams: &[Stream],
    scrobbles: &[Scrobble],
    matched: &Matched,
    lengths: &HashMap<String, i64>,
    set_aside: &HashSet<Vec<u8>>,
) -> BTreeMap<i32, (i64, i64)> {
    // A play a stream set aside speaks for is excluded, and is no play.
    let excluded = |event: usize| {
        matched.events.get(event).is_some_and(|event| {
            event
                .streams
                .iter()
                .filter_map(|at| streams.get(*at))
                .any(|stream| set_aside.contains(&stream.key))
        })
    };
    let mut added: BTreeMap<i32, (i64, i64)> = BTreeMap::new();
    let mut add = |year: i32, plays: i64, counted: i64| {
        let row = added.entry(year).or_default();
        row.0 += plays;
        row.1 += counted;
    };
    let mut joined: HashSet<usize> = HashSet::new();
    for join in &matched.joins {
        let (Some(scrobble), Some(event)) =
            (scrobbles.get(join.scrobble), matched.events.get(join.event))
        else {
            continue;
        };
        let from = year(scrobble.at_ms);
        if join.second {
            add(from, -1, -1);
        } else {
            joined.insert(join.event);
            let to = year(event.start_ms);
            if excluded(join.event) {
                add(from, -1, -1);
            } else if to != from {
                add(from, -1, -1);
                add(to, 1, 1);
            }
        }
    }
    for (index, event) in matched.events.iter().enumerate() {
        if joined.contains(&index) || excluded(index) {
            continue;
        }
        let Some(record) = first_record(streams, event) else {
            continue;
        };
        let (counts, _) = judge_alone(
            record.ms_played,
            lengths.get(&record.track_uri).copied(),
            record.reason_end.as_deref(),
        );
        add(year(event.start_ms), 1, i64::from(counts));
    }
    added
}

/// Works out what applying a run would do. What it says of listens, joins
/// and streams alone is of every stream in force once it is applied, the
/// run's and those in force now, as applying derives every play from them.
pub fn dry_run(input: &DryRunInput<'_>) -> ImportReport {
    let run = input.streams;
    // A stream in force for any listener stays theirs.
    let held: HashSet<&[u8]> = input
        .in_force
        .iter()
        .chain(input.others.iter().flat_map(|other| &other.streams))
        .map(|s| s.key.as_slice())
        .collect();
    // A stream stored already keeps its record as it was stored, and stays
    // the listener's it was stored for: one of another listener's that the
    // run brings back into force is theirs again.
    let as_stored = |stream: &Stream| Stream {
        key: stream.key.clone(),
        record: input
            .stored
            .get(&stream.key)
            .unwrap_or(&stream.record)
            .clone(),
    };
    let mut back: BTreeMap<i64, Vec<Stream>> = BTreeMap::new();
    let mut union: Vec<Stream> = input.in_force.to_vec();
    for stream in run
        .iter()
        .filter(|stream| !held.contains(stream.key.as_slice()))
    {
        match input.owners.get(&stream.key) {
            Some(owner) if *owner != input.listener => {
                back.entry(*owner).or_default().push(as_stored(stream));
            }
            _ => union.push(as_stored(stream)),
        }
    }
    let input = &DryRunInput {
        streams: &union,
        ..*input
    };
    let of_run: HashSet<&[u8]> = run.iter().map(|stream| stream.key.as_slice()).collect();
    // The pins of the streams matched, as derivation finds them among the
    // streams it is given.
    let pins = |streams: &[Stream]| {
        crate::streams::pins(
            input.corrections,
            streams
                .iter()
                .filter_map(|stream| Some((*input.observations.get(&stream.key)?, stream))),
        )
    };
    let matched = match_streams(
        input.streams,
        input.scrobbles,
        Tiers::ALL,
        &pins(input.streams),
    );
    // A track's length, from these streams and every other listener's in
    // force, and once applied those the run brings back to them.
    let lengths_of = |streams: &[Stream], events: &[Event], applied: bool| {
        let back = back.values().flatten().filter(|_| applied);
        if input.others.is_empty() && back.clone().next().is_none() {
            return lengths(streams, events);
        }
        let every: Vec<Stream> = streams
            .iter()
            .chain(input.others.iter().flat_map(|other| &other.streams))
            .chain(back)
            .cloned()
            .collect();
        lengths(&every, &crate::streams::events(&every))
    };
    let lengths = lengths_of(input.streams, &matched.events, true);

    let counts = crate::streams::counts(&matched);
    let tiers = Tier::ALL
        .iter()
        .map(|tier| {
            let (joined, second) = counts.get(tier).copied().unwrap_or_default();
            TierJoins {
                tier: contract_tier(*tier),
                joined,
                second,
            }
        })
        .collect::<Vec<_>>();

    let mut years: BTreeMap<i32, ImportYear> = BTreeMap::new();
    for (year, (plays, counted)) in input.plays_by_year {
        let row = row(&mut years, *year);
        row.plays_before = *plays;
        row.counted_before = *counted;
    }
    let joined_tier: HashMap<usize, Option<Tier>> = matched
        .joins
        .iter()
        .filter(|join| !join.second)
        .map(|join| (join.event, join.tier))
        .collect();
    let mut alone: Vec<usize> = Vec::new();
    for (index, event) in matched.events.iter().enumerate() {
        let row = row(&mut years, year(event.start_ms));
        row.streams += 1;
        // Stored already when any row of the run's for it is.
        if event
            .streams
            .iter()
            .filter_map(|at| input.streams.get(*at))
            .any(|stream| {
                of_run.contains(stream.key.as_slice()) && input.stored.contains_key(&stream.key)
            })
        {
            row.already_stored += 1;
        }
        if assumed_zone(input.listener, event.start_ms, input.zone_periods).is_none() {
            row.no_zone += 1;
        }
        match joined_tier.get(&index) {
            Some(Some(Tier::Start)) => row.joined_start += 1,
            Some(None) => row.joined_pinned += 1,
            Some(Some(Tier::During)) => row.joined_during += 1,
            Some(Some(Tier::Renamed)) => row.joined_renamed += 1,
            None => {
                alone.push(index);
                let Some(record) = first_record(input.streams, event) else {
                    continue;
                };
                let (counts, outcome) = judge_alone(
                    record.ms_played,
                    lengths.get(&record.track_uri).copied(),
                    record.reason_end.as_deref(),
                );
                if counts {
                    row.alone_counted += 1;
                } else if outcome == Outcome::Unknown {
                    row.alone_unknown += 1;
                } else {
                    row.alone_not_counted += 1;
                }
            }
        }
    }
    let taken: HashSet<usize> = matched.joins.iter().map(|join| join.scrobble).collect();
    for (at, scrobble) in input.scrobbles.iter().enumerate() {
        if !taken.contains(&at) {
            row(&mut years, year(scrobble.at_ms)).scrobbles_alone += 1;
        }
    }
    // The plays now hold what the streams in force add; once applied they
    // hold what every stream in force adds instead.
    let mut after = added(
        input.streams,
        input.scrobbles,
        &matched,
        &lengths,
        input.set_aside,
    );
    // With no stream in force, as before a first archive, nothing is
    // matched now.
    let in_force = if input.in_force.is_empty() {
        Matched::default()
    } else {
        match_streams(
            input.in_force,
            input.scrobbles,
            Tiers::ALL,
            &pins(input.in_force),
        )
    };
    let lengths_now = lengths_of(input.in_force, &in_force.events, false);
    let mut now = added(
        input.in_force,
        input.scrobbles,
        &in_force,
        &lengths_now,
        input.set_aside,
    );
    // Another listener's plays move by a track's length, which the run's
    // streams can change, and by their streams it brings back into force.
    for other in input.others {
        let mut once_applied = other.streams.clone();
        once_applied.extend(back.get(&other.listener).into_iter().flatten().cloned());
        for (into, streams, lengths) in [
            (&mut after, &once_applied, &lengths),
            (&mut now, &other.streams, &lengths_now),
        ] {
            let matched = match_streams(streams, &other.scrobbles, Tiers::ALL, &pins(streams));
            let theirs = added(
                streams,
                &other.scrobbles,
                &matched,
                lengths,
                input.set_aside,
            );
            for (year, (plays, counted)) in theirs {
                let row = into.entry(year).or_default();
                row.0 += plays;
                row.1 += counted;
            }
        }
    }
    for row in years.values_mut() {
        let (after, now) = (
            after.get(&row.year).copied().unwrap_or_default(),
            now.get(&row.year).copied().unwrap_or_default(),
        );
        let moved = |before: u64, after: i64, now: i64| {
            u64::try_from(i128::from(before) + i128::from(after) - i128::from(now)).unwrap_or(0)
        };
        row.plays_after = moved(row.plays_before, after.0, now.0);
        row.counted_after = moved(row.counted_before, after.1, now.1);
    }

    let (offline, offline_timestamped, offline_seconds, offline_milliseconds) =
        offline_facts(input.streams, &matched);
    let paused = paused(input, &matched, &alone, &taken);

    let mut problems = Vec::new();
    if years.values().any(|row| row.no_zone > 0) {
        problems.push(ImportProblem::NoZone);
    }

    ImportReport {
        made_at: input.made_at,
        grouper_version: crate::history::GROUPER_VERSION,
        play_rule_version: crate::history::PlayRule::CURRENT.version,
        streams: run.len() as u64,
        events: crate::streams::events(run).len() as u64,
        respelled: run
            .iter()
            .filter(|stream| {
                input
                    .stored
                    .get(&stream.key)
                    .is_some_and(|stored| *stored != kept(&stream.record))
            })
            .count() as u64,
        scrobbles: input.scrobbles.len() as u64,
        scrobbles_excluded: input.excluded,
        scrobbles_echoes: input.echoes,
        tiers,
        years: years.into_values().collect(),
        offline,
        offline_timestamped,
        offline_seconds,
        offline_milliseconds,
        paused,
        problems,
        samples: samples(input, &matched),
    }
}

/// Streams played offline, those with an offline time, and of those the
/// ones whose time reads as seconds or as milliseconds near the end.
fn offline_facts(streams: &[Stream], matched: &Matched) -> (u64, u64, u64, u64) {
    let (mut offline, mut stamped, mut seconds, mut millis) = (0, 0, 0, 0);
    for event in &matched.events {
        let Some(r) = first_record(streams, event) else {
            continue;
        };
        if r.offline != Some(true) {
            continue;
        }
        offline += 1;
        let Some(at) = r.offline_timestamp.filter(|at| *at != 0) else {
            continue;
        };
        stamped += 1;
        let near = |at: i128| (at - i128::from(r.ended_at)).abs() <= i128::from(OFFLINE_NEAR_MS);
        if near(i128::from(at) * 1000) {
            seconds += 1;
        } else if near(i128::from(at)) {
            millis += 1;
        }
    }
    (offline, stamped, seconds, millis)
}

/// Scrobbles no stream took that sit between the stream before and the
/// worked-out start of a stream alone with their names: where a pause in
/// that stream would have put its scrobble.
fn paused(
    input: &DryRunInput<'_>,
    matched: &Matched,
    alone: &[usize],
    taken: &HashSet<usize>,
) -> u64 {
    let mut by_start: Vec<(i64, i64)> = matched
        .events
        .iter()
        .map(|event| (event.start_ms, event.end_ms))
        .collect();
    by_start.sort_unstable();
    let mut free: HashMap<(String, String), Vec<(i64, usize)>> = HashMap::new();
    for (at, scrobble) in input.scrobbles.iter().enumerate() {
        if !taken.contains(&at) {
            free.entry((name_key(&scrobble.artist), name_key(&scrobble.title)))
                .or_default()
                .push((scrobble.at_ms, at));
        }
    }
    for stamps in free.values_mut() {
        stamps.sort_unstable();
    }
    // Each scrobble once, however many streams it sits before.
    let mut counted: HashSet<usize> = HashSet::new();
    for index in alone {
        let Some(event) = matched.events.get(*index) else {
            continue;
        };
        let Some(record) = first_record(input.streams, event) else {
            continue;
        };
        // Only between this stream and one that played before it.
        let before = by_start.partition_point(|(start, _)| *start < event.start_ms);
        let Some((_, previous_end)) = before.checked_sub(1).and_then(|at| by_start.get(at)) else {
            continue;
        };
        let names = (name_key(&record.artist), name_key(&record.title));
        if let Some(stamps) = free.get(&names) {
            let from = stamps.partition_point(|(at, _)| at <= previous_end);
            let to = stamps.partition_point(|(at, _)| *at < event.start_ms);
            counted.extend(
                stamps
                    .get(from..to)
                    .unwrap_or_default()
                    .iter()
                    .map(|(_, at)| *at),
            );
        }
    }
    counted.len() as u64
}

/// A sample of what tiers B and C join, which ask less of the time or the
/// names than A does.
fn samples(input: &DryRunInput<'_>, matched: &Matched) -> Vec<JoinSample> {
    let mut samples = Vec::new();
    for tier in [Tier::During, Tier::Renamed] {
        let mut joins: Vec<_> = matched
            .joins
            .iter()
            .filter(|join| join.tier == Some(tier))
            .collect();
        joins.sort_by_key(|join| {
            matched
                .events
                .get(join.event)
                .map_or(0, |event| event.start_ms)
        });
        for join in spread(&joins, SAMPLES_PER_TIER) {
            let (Some(event), Some(scrobble)) = (
                matched.events.get(join.event),
                input.scrobbles.get(join.scrobble),
            ) else {
                continue;
            };
            let Some(record) = first_record(input.streams, event) else {
                continue;
            };
            samples.push(JoinSample {
                tier: contract_tier(tier),
                second: join.second,
                stream_artist: record.artist.clone(),
                stream_title: record.title.clone(),
                scrobble_artist: scrobble.artist.clone(),
                scrobble_title: scrobble.title.clone(),
                from_start_ms: scrobble.at_ms - event.start_ms,
                ms_played: record.ms_played,
            });
        }
    }
    samples
}

#[cfg(test)]
mod tests {

    use super::*;

    /// 2019-03-15 21:00:00 UTC.
    const EVENING: i64 = 1_552_683_600_000;

    fn record(end_s: i64, played_s: i64, title: &str, reason_end: &str) -> SpotifyStream {
        SpotifyStream {
            ended_at: EVENING + end_s * 1000,
            ms_played: played_s * 1000,
            track_uri: format!("spotify:track:{}", title.replace(' ', "")),
            title: title.to_owned(),
            artist: "Calla Brandt".to_owned(),
            album: Some("Glasshouse".to_owned()),
            reason_start: Some("clickrow".to_owned()),
            reason_end: Some(reason_end.to_owned()),
            shuffle: Some(false),
            skipped: Some(false),
            offline: Some(false),
            offline_timestamp: None,
            incognito: Some(false),
            platform: None,
            device_kind: None,
            via: None,
        }
    }

    fn scrobble(at_s: i64, title: &str) -> Scrobble {
        Scrobble {
            observation: at_s,
            key: format!("{at_s}:{title}").into_bytes(),
            at_ms: EVENING + at_s * 1000,
            artist: "Calla Brandt".to_owned(),
            title: title.to_owned(),
        }
    }

    #[test]
    fn a_streams_key_is_made_from_the_records_alone() {
        let low = record(221, 221, "Low Tide", "trackdone");
        let mut other = low.clone();
        other.reason_end = Some("endplay".to_owned());
        let keys = |records: Vec<SpotifyStream>| -> Vec<String> {
            let mut keys: Vec<String> = fresh(records)
                .into_iter()
                .map(|stream| String::from_utf8(stream.key).unwrap())
                .collect();
            keys.sort();
            keys
        };
        let first = keys(vec![low.clone(), other.clone(), low.clone()]);
        assert_eq!(
            first,
            [
                "sp1:1552683821000:221000:LowTide:0",
                "sp1:1552683821000:221000:LowTide:1",
                "sp1:1552683821000:221000:LowTide:2",
            ]
        );
        // In any order the same records get the same keys: "endplay"
        // sorts first.
        let keyed_other = fresh(vec![low.clone(), low.clone(), other.clone()]);
        let place_of_other = keyed_other
            .iter()
            .find(|stream| stream.record == other)
            .map(|stream| String::from_utf8(stream.key.clone()).unwrap());
        assert_eq!(
            place_of_other.as_deref(),
            Some("sp1:1552683821000:221000:LowTide:0")
        );
    }

    #[test]
    fn a_later_archive_gives_the_rows_it_shares_their_keys_wherever_new_rows_sort() {
        // The first archive has a row that ended with "trackdone"; the
        // next adds a repeat of it that ended with "endplay", which sorts
        // first, and spells the first's album otherwise.
        let first = record(221, 221, "Low Tide", "trackdone");
        let stored: HashMap<Vec<u8>, SpotifyStream> = fresh(vec![first.clone()])
            .into_iter()
            .map(|stream| (stream.key, stream.record))
            .collect();
        let mut added = first.clone();
        added.reason_end = Some("endplay".to_owned());
        let mut respelled = first.clone();
        respelled.album = Some("Glasshouse (Deluxe)".to_owned());
        let key_of = |streams: &[Stream], record: &SpotifyStream| {
            streams
                .iter()
                .find(|stream| stream.record == *record)
                .map(|stream| String::from_utf8(stream.key.clone()).unwrap())
                .unwrap()
        };
        let next = keyed(vec![added.clone(), first.clone()], &stored);
        assert_eq!(key_of(&next, &first), "sp1:1552683821000:221000:LowTide:0");
        assert_eq!(key_of(&next, &added), "sp1:1552683821000:221000:LowTide:1");
        // A row spelled otherwise takes the stored row's key, and keeps it.
        let next = keyed(vec![added.clone(), respelled.clone()], &stored);
        assert_eq!(
            key_of(&next, &respelled),
            "sp1:1552683821000:221000:LowTide:0"
        );
        assert_eq!(key_of(&next, &added), "sp1:1552683821000:221000:LowTide:1");
    }

    #[test]
    fn which_repeat_speaks_never_depends_on_the_order_rows_came_in() {
        // Two rows of one listen that differ only in their title: the same
        // record speaks for it, and has the same key, in either order.
        let low = record(221, 221, "Low Tide", "trackdone");
        let mut renamed = low.clone();
        renamed.title = "Low Tide - Remastered".to_owned();
        let speaks = |records: Vec<SpotifyStream>| {
            let streams = fresh(records);
            let events = crate::streams::events(&streams);
            let first = &streams[events[0].streams[0]];
            (first.record.title.clone(), first.key.clone())
        };
        assert_eq!(
            speaks(vec![low.clone(), renamed.clone()]),
            speaks(vec![renamed, low])
        );
    }

    /// The keys of an archive when nothing is stored yet.
    fn fresh(records: Vec<SpotifyStream>) -> Vec<Stream> {
        keyed(records, &EMPTY)
    }

    /// No stream set aside.
    static NONE: std::sync::LazyLock<HashSet<Vec<u8>>> = std::sync::LazyLock::new(HashSet::new);

    /// No stream stored yet.
    static EMPTY: std::sync::LazyLock<HashMap<Vec<u8>, SpotifyStream>> =
        std::sync::LazyLock::new(HashMap::new);

    /// No stream with an observation.
    static UNOBSERVED: std::sync::LazyLock<HashMap<Vec<u8>, i64>> =
        std::sync::LazyLock::new(HashMap::new);

    fn input<'a>(
        streams: &'a [Stream],
        scrobbles: &'a [Scrobble],
        plays: &'a BTreeMap<i32, (u64, u64)>,
    ) -> DryRunInput<'a> {
        DryRunInput {
            streams,
            in_force: &[],
            others: &[],
            scrobbles,
            excluded: 0,
            echoes: 0,
            plays_by_year: plays,
            listener: 1,
            zone_periods: &[],
            corrections: &[],
            observations: &UNOBSERVED,
            stored: &EMPTY,
            owners: &UNOBSERVED,
            set_aside: &NONE,
            made_at: 7,
        }
    }

    #[test]
    fn a_pause_is_counted_after_a_stream_before_and_each_scrobble_once() {
        let streams = fresh(vec![
            record(100, 100, "Paper Lanterns", "trackdone"),
            // Two streams alone of one track, with the same worked-out
            // start, a minute after the stream before ended.
            record(400, 240, "Low Tide", "trackdone"),
            record(400, 240, "Low Tide", "endplay"),
        ]);
        let scrobbles = vec![
            // Long before any stream: not where a pause would put it.
            scrobble(-90_000, "Low Tide"),
            // Between the stream before and the worked-out start.
            scrobble(130, "Low Tide"),
        ];
        let plays = BTreeMap::new();
        let report = dry_run(&input(&streams, &scrobbles, &plays));
        assert_eq!(report.paused, 1);
    }

    #[test]
    fn an_offline_time_of_any_size_reads_as_neither_unit_without_overflow() {
        let mut far = record(221, 221, "Low Tide", "trackdone");
        far.offline = Some(true);
        far.offline_timestamp = Some(i64::MIN);
        let mut seconds = record(500, 221, "Night Ferry", "trackdone");
        seconds.offline = Some(true);
        seconds.offline_timestamp = Some((EVENING + 500_000) / 1000);
        let mut millis = record(900, 221, "Paper Lanterns", "trackdone");
        millis.offline = Some(true);
        millis.offline_timestamp = Some(EVENING + 899_000);
        let streams = fresh(vec![far, seconds, millis]);
        let plays = BTreeMap::new();
        let report = dry_run(&input(&streams, &[], &plays));
        assert_eq!(
            (
                report.offline,
                report.offline_timestamped,
                report.offline_seconds,
                report.offline_milliseconds
            ),
            (3, 3, 1, 1)
        );
    }

    #[test]
    fn a_join_a_correction_pins_is_counted_as_pinned_and_in_no_tier() {
        let streams = fresh(vec![record(221, 221, "Low Tide", "trackdone")]);
        // An hour on: no tier would join it.
        let scrobbles = [scrobble(3600, "Low Tide")];
        // The stream is stored, as observation 10.
        let observations = HashMap::from([(streams[0].key.clone(), 10)]);
        let corrections = [Correction {
            observation: 20,
            verb: crate::history::Verb::Join,
            targets: vec![10, 3600],
            window: None,
            recording: None,
        }];
        let plays = BTreeMap::new();
        let report = dry_run(&DryRunInput {
            corrections: &corrections,
            observations: &observations,
            ..input(&streams, &scrobbles, &plays)
        });
        let year = &report.years[0];
        assert_eq!((year.joined_start, year.joined_pinned), (0, 1));
        assert!(report.tiers.iter().all(|tier| tier.joined == 0));
    }

    #[test]
    fn an_archive_in_force_already_adds_no_play_and_a_later_one_only_its_new_streams() {
        let older = fresh(vec![
            record(221, 221, "Low Tide", "trackdone"),
            record(1000, 221, "Low Tide", "trackdone"),
        ]);
        let scrobbles = vec![scrobble(1, "Low Tide")];
        // The plays now: the scrobble's, joined to the first stream, and
        // the second stream alone, which counts.
        let plays = BTreeMap::from([(2019, (2, 2))]);
        let again = DryRunInput {
            in_force: &older,
            ..input(&older, &scrobbles, &plays)
        };
        let year = dry_run(&again).years[0];
        assert_eq!((year.streams, year.already_stored), (2, 0));
        assert_eq!((year.plays_after, year.counted_after), (2, 2));
        // A later archive holds those and one stream more, a skip.
        let mut later = older.clone();
        later.extend(fresh(vec![record(2000, 12, "Paper Lanterns", "fwdbtn")]));
        let next = DryRunInput {
            in_force: &older,
            ..input(&later, &scrobbles, &plays)
        };
        let report = dry_run(&next);
        assert_eq!((report.streams, report.events), (3, 3));
        let year = &report.years[0];
        assert_eq!(year.streams, 3);
        assert_eq!((year.plays_after, year.counted_after), (3, 2));
    }

    #[test]
    fn a_play_joined_across_the_new_year_moves_to_its_streams_year() {
        // 2020-01-01 00:00:00 UTC, from the evening the records begin.
        const NEW_YEAR: i64 = 25_153_200;
        let streams = fresh(vec![record(NEW_YEAR + 180, 210, "Low Tide", "trackdone")]);
        // Scrobbled ten seconds into 2020, while the stream played: its play
        // is dated by the stream, which began in 2019.
        let scrobbles = [scrobble(NEW_YEAR + 10, "Low Tide")];
        let plays = BTreeMap::from([(2020, (1, 1))]);
        let report = dry_run(&input(&streams, &scrobbles, &plays));
        let by_year: Vec<(i32, u64, u64)> = report
            .years
            .iter()
            .map(|year| (year.year, year.plays_after, year.counted_after))
            .collect();
        assert_eq!(by_year, [(2019, 1, 1), (2020, 0, 0)]);
    }

    #[test]
    fn a_stream_set_aside_takes_its_scrobbles_play_with_it() {
        let streams = fresh(vec![record(221, 221, "Low Tide", "trackdone")]);
        let scrobbles = [scrobble(1, "Low Tide")];
        let plays = BTreeMap::from([(2019, (1, 1))]);
        let set_aside: HashSet<Vec<u8>> = HashSet::from([streams[0].key.clone()]);
        let report = dry_run(&DryRunInput {
            set_aside: &set_aside,
            ..input(&streams, &scrobbles, &plays)
        });
        let year = &report.years[0];
        assert_eq!((year.plays_after, year.counted_after), (0, 0));
    }

    #[test]
    fn a_correction_pins_a_stored_stream_in_the_dry_run_as_in_derivation() {
        // A run undone and applied again: its streams are stored, as
        // observations 10 and 11, and none is in force.
        let streams = fresh(vec![
            record(221, 221, "Low Tide", "trackdone"),
            record(1300, 12, "Night Ferry", "fwdbtn"),
        ]);
        let stored: HashMap<Vec<u8>, SpotifyStream> = streams
            .iter()
            .map(|stream| (stream.key.clone(), stream.record.clone()))
            .collect();
        let observations =
            HashMap::from([(streams[0].key.clone(), 10), (streams[1].key.clone(), 11)]);
        // Tier A would join the first; the second's scrobble is an hour on.
        let scrobbles = [scrobble(1, "Low Tide"), scrobble(5000, "Night Ferry")];
        let correction = |observation, verb, targets: Vec<i64>| Correction {
            observation,
            verb,
            targets,
            window: None,
            recording: None,
        };
        let corrections = [
            correction(20, crate::history::Verb::Split, vec![10, 1]),
            correction(21, crate::history::Verb::Join, vec![11, 5000]),
        ];
        // The plays now: the two scrobbles, each counted.
        let plays = BTreeMap::from([(2019, (2, 2))]);
        let report = dry_run(&DryRunInput {
            stored: &stored,
            corrections: &corrections,
            observations: &observations,
            ..input(&streams, &scrobbles, &plays)
        });
        let start = report
            .tiers
            .iter()
            .find(|tier| tier.tier == StreamTier::Start)
            .unwrap();
        assert_eq!(start.joined, 0);
        // Once applied: the first stream and its scrobble apart, each
        // counted, and the skip joined to its scrobble, which counts it.
        let year = &report.years[0];
        assert_eq!((year.plays_after, year.counted_after), (3, 3));
    }

    #[test]
    fn a_tracks_length_is_taken_from_other_listeners_streams_too() {
        // Another listener played Low Tide to its end, in 2:40.
        let theirs = fresh(vec![record(5000, 160, "Low Tide", "trackdone")]);
        // This run's stream of it played a minute and a half and stopped.
        let streams = fresh(vec![record(221, 90, "Low Tide", "endplay")]);
        let plays = BTreeMap::new();
        let alone = |others: &[OtherListener]| {
            let year = dry_run(&DryRunInput {
                others,
                ..input(&streams, &[], &plays)
            })
            .years[0];
            (year.alone_counted, year.alone_unknown)
        };
        assert_eq!(alone(&[]), (0, 1));
        // Over half its length: it counts, as derivation counts it.
        let others = [OtherListener {
            listener: 2,
            streams: theirs,
            scrobbles: Vec::new(),
        }];
        assert_eq!(alone(&others), (1, 0));
    }

    #[test]
    fn a_run_moves_another_listeners_plays_by_a_tracks_length_and_leaves_theirs_theirs() {
        // Another listener's stream of Low Tide played a minute and a half
        // and stopped: with no length for it, it does not count.
        let theirs = fresh(vec![record(221, 90, "Low Tide", "endplay")]);
        let others = [OtherListener {
            listener: 2,
            streams: theirs.clone(),
            scrobbles: Vec::new(),
        }];
        let plays = BTreeMap::from([(2019, (1, 0))]);
        // This run plays it to its end, in 2:40, which gives it a length:
        // their stream counts too once it is applied.
        let streams = fresh(vec![record(5000, 160, "Low Tide", "trackdone")]);
        let year = dry_run(&DryRunInput {
            others: &others,
            ..input(&streams, &[], &plays)
        })
        .years[0];
        assert_eq!((year.plays_after, year.counted_after), (2, 2));
        // A run that holds their stream adds nothing: it stays theirs.
        let year = dry_run(&DryRunInput {
            others: &others,
            ..input(&theirs, &[], &plays)
        })
        .years[0];
        assert_eq!((year.plays_after, year.counted_after), (1, 0));
    }

    #[test]
    fn a_stream_brought_back_into_force_stays_the_listeners_it_was_stored_for() {
        // Another listener's stream, stored for them and out of force since
        // its run was undone, and their scrobble of it as it began.
        let streams = fresh(vec![record(221, 221, "Low Tide", "trackdone")]);
        let stored: HashMap<Vec<u8>, SpotifyStream> = streams
            .iter()
            .map(|stream| (stream.key.clone(), stream.record.clone()))
            .collect();
        let owners = HashMap::from([(streams[0].key.clone(), 2)]);
        let others = [OtherListener {
            listener: 2,
            streams: Vec::new(),
            scrobbles: vec![scrobble(1, "Low Tide")],
        }];
        // The plays now: their scrobble.
        let plays = BTreeMap::from([(2019, (1, 1))]);
        // A run brings it back: it joins their scrobble, and adds no play.
        let year = dry_run(&DryRunInput {
            others: &others,
            stored: &stored,
            owners: &owners,
            ..input(&streams, &[], &plays)
        })
        .years[0];
        assert_eq!((year.plays_after, year.counted_after), (1, 1));
    }

    #[test]
    fn a_dry_run_counts_by_year_and_says_what_stands_in_the_way() {
        let streams = fresh(vec![
            // Joined by tier A.
            record(221, 221, "Low Tide", "trackdone"),
            // Alone, and counts: its track's length is 3:41.
            record(1000, 221, "Low Tide", "trackdone"),
            // Alone, twelve seconds: a skip.
            record(1100, 12, "Paper Lanterns", "fwdbtn"),
            // Joined by tier B.
            record(1300, 100, "Night Ferry", "endplay"),
            // Alone, no length to judge it by.
            record(1500, 100, "Harbour Lights", "endplay"),
        ]);
        let scrobbles = vec![
            scrobble(1, "Low Tide"),
            // Stamped as the stream played.
            scrobble(1250, "Night Ferry"),
        ];
        let plays = BTreeMap::from([(2019, (10, 8))]);
        let report = dry_run(&DryRunInput {
            streams: &streams,
            in_force: &[],
            others: &[],
            scrobbles: &scrobbles,
            excluded: 3,
            echoes: 2,
            plays_by_year: &plays,
            listener: 1,
            zone_periods: &[],
            corrections: &[],
            observations: &UNOBSERVED,
            stored: &HashMap::new(),
            owners: &HashMap::new(),
            set_aside: &NONE,
            made_at: 7,
        });
        assert_eq!((report.streams, report.events, report.scrobbles), (5, 5, 2));
        assert_eq!((report.scrobbles_excluded, report.scrobbles_echoes), (3, 2));
        let year = &report.years[0];
        assert_eq!(year.year, 2019);
        assert_eq!(
            (
                year.joined_start,
                year.joined_during,
                year.alone_counted,
                year.alone_not_counted,
                year.alone_unknown
            ),
            (1, 1, 1, 1, 1)
        );
        assert_eq!(year.scrobbles_alone, 0);
        assert_eq!((year.plays_after, year.counted_after), (13, 9));
        assert_eq!(year.no_zone, 5);
        assert_eq!(report.problems, [ImportProblem::NoZone]);
        let during = report
            .tiers
            .iter()
            .find(|tier| tier.tier == StreamTier::During)
            .unwrap();
        assert_eq!(during.joined, 1);
        assert_eq!(report.samples.len(), 1);
        assert_eq!(report.samples[0].stream_title, "Night Ferry");

        // With a zone for the listener, nothing is without a date.
        let zones = [ZonePeriod {
            listener: 1,
            zone: "Europe/London".to_owned(),
            from_ms: None,
            to_ms: None,
        }];
        let report = dry_run(&DryRunInput {
            streams: &streams,
            in_force: &[],
            others: &[],
            scrobbles: &scrobbles,
            excluded: 0,
            echoes: 0,
            plays_by_year: &plays,
            listener: 1,
            zone_periods: &zones,
            corrections: &[],
            observations: &UNOBSERVED,
            stored: &HashMap::new(),
            owners: &HashMap::new(),
            set_aside: &NONE,
            made_at: 7,
        });
        assert_eq!(report.years[0].no_zone, 0);
        assert!(report.problems.is_empty());
    }
}
