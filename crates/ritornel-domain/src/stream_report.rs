//! What applying a run of Spotify streams would do, worked out without
//! writing anything: the dry run's report. See "Spotify streams" in
//! docs/internals/history.md.

use std::collections::{BTreeMap, HashMap, HashSet};

use jiff::Timestamp;
use jiff::tz::TimeZone;
use ritornel_contract::{
    ImportProblem, ImportReport, ImportYear, JoinSample, SpotifyStream, StreamTier, TierJoins,
};

use crate::history::{Outcome, ZonePeriod, assumed_zone};
use crate::streams::{
    Event, Matched, Pins, STREAM_MATCHER_VERSION, Scrobble, Stream, Tier, Tiers, judge_alone,
    lengths, match_streams, name_key,
};

/// How many joins of each tier not in force a report shows for judging it.
pub const SAMPLES_PER_TIER: usize = 25;

/// How near the end of its stream an offline time must be to read as one
/// unit or the other.
const OFFLINE_NEAR_MS: i64 = 24 * 60 * 60 * 1000;

/// Everything a dry run reads.
#[derive(Debug, Clone, Copy)]
pub struct DryRunInput<'a> {
    /// The run's streams, with their keys.
    pub streams: &'a [Stream],
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
    /// Pairs a correction has pinned.
    pub pins: &'a Pins,
    /// When the report is made.
    pub made_at: i64,
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

/// The keys of one stream: `sp1`, its end, its time played, its track's ID,
/// and its place among the streams with the same three, which are ordered
/// by their other kept fields. Made from the records alone, so the same
/// archive gives the same keys, and a later one gives the rows it shares
/// with an earlier one the same keys too.
pub fn keyed(records: Vec<SpotifyStream>) -> Vec<Stream> {
    let mut order: Vec<usize> = (0..records.len()).collect();
    let fields = |at: &usize| {
        records.get(*at).map(|r| {
            (
                r.ended_at,
                r.ms_played,
                r.track_uri.clone(),
                r.reason_start.clone(),
                r.reason_end.clone(),
                r.shuffle,
                r.skipped,
                r.offline,
                r.offline_timestamp,
                r.incognito,
                r.platform,
                r.album.clone(),
            )
        })
    };
    order.sort_by_cached_key(fields);
    let mut places: HashMap<(i64, i64, &str), u32> = HashMap::new();
    let mut keys: Vec<Vec<u8>> = vec![Vec::new(); records.len()];
    for at in order {
        let Some(r) = records.get(at) else {
            continue;
        };
        let place = places
            .entry((r.ended_at, r.ms_played, r.track_uri.as_str()))
            .or_default();
        let id = r.track_uri.trim_start_matches("spotify:track:");
        if let Some(key) = keys.get_mut(at) {
            *key = format!("sp1:{}:{}:{id}:{place}", r.ended_at, r.ms_played).into_bytes();
        }
        *place += 1;
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

/// Works out what applying a run would do.
pub fn dry_run(input: &DryRunInput<'_>) -> ImportReport {
    let in_force = match_streams(input.streams, input.scrobbles, Tiers::CURRENT, input.pins);
    let every = match_streams(input.streams, input.scrobbles, Tiers::ALL, input.pins);
    let lengths = lengths(input.streams, &in_force.events);

    let tiers = Tier::ALL
        .iter()
        .map(|tier| {
            let on = match tier {
                Tier::Start => true,
                Tier::During => Tiers::CURRENT.during,
                Tier::Renamed => Tiers::CURRENT.renamed,
            };
            let source = if on { &in_force } else { &every };
            let (joined, second) = crate::streams::counts(source)
                .get(tier)
                .copied()
                .unwrap_or_default();
            TierJoins {
                tier: contract_tier(*tier),
                in_force: on,
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
    let joined_tier: HashMap<usize, Option<Tier>> = in_force
        .joins
        .iter()
        .filter(|join| !join.second)
        .map(|join| (join.event, join.tier))
        .collect();
    let mut alone: Vec<usize> = Vec::new();
    for (index, event) in in_force.events.iter().enumerate() {
        let row = row(&mut years, year(event.start_ms));
        row.streams += 1;
        if assumed_zone(input.listener, event.start_ms, input.zone_periods).is_none() {
            row.no_zone += 1;
        }
        match joined_tier.get(&index) {
            Some(Some(Tier::Start) | None) => row.joined_start += 1,
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
    let taken: HashSet<usize> = in_force.joins.iter().map(|join| join.scrobble).collect();
    for (at, scrobble) in input.scrobbles.iter().enumerate() {
        if !taken.contains(&at) {
            row(&mut years, year(scrobble.at_ms)).scrobbles_alone += 1;
        }
    }
    for row in years.values_mut() {
        let alone = row.alone_counted + row.alone_not_counted + row.alone_unknown;
        row.plays_after = row.plays_before + alone;
        row.counted_after = row.counted_before + row.alone_counted;
    }

    let (offline, offline_timestamped, offline_seconds, offline_milliseconds) =
        offline_facts(input.streams, &in_force);
    let paused = paused(input, &in_force, &alone, &taken);

    let mut problems = Vec::new();
    if years.values().any(|row| row.no_zone > 0) {
        problems.push(ImportProblem::NoZone);
    }
    if tiers
        .iter()
        .any(|tier| !tier.in_force && tier.joined + tier.second > 0)
    {
        problems.push(ImportProblem::TierNotInForce);
    }

    ImportReport {
        made_at: input.made_at,
        matcher_version: STREAM_MATCHER_VERSION,
        streams: input.streams.len() as u64,
        events: in_force.events.len() as u64,
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
        samples: samples(input, &every),
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
        if (at.saturating_mul(1000) - r.ended_at).abs() <= OFFLINE_NEAR_MS {
            seconds += 1;
        } else if (at - r.ended_at).abs() <= OFFLINE_NEAR_MS {
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
    let mut free: HashMap<(String, String), Vec<i64>> = HashMap::new();
    for (at, scrobble) in input.scrobbles.iter().enumerate() {
        if !taken.contains(&at) {
            free.entry((name_key(&scrobble.artist), name_key(&scrobble.title)))
                .or_default()
                .push(scrobble.at_ms);
        }
    }
    for stamps in free.values_mut() {
        stamps.sort_unstable();
    }
    let mut count = 0;
    for index in alone {
        let Some(event) = matched.events.get(*index) else {
            continue;
        };
        let Some(record) = first_record(input.streams, event) else {
            continue;
        };
        let before = by_start.partition_point(|(start, _)| *start < event.start_ms);
        let previous_end = before
            .checked_sub(1)
            .and_then(|at| by_start.get(at))
            .map_or(i64::MIN, |(_, end)| *end);
        let names = (name_key(&record.artist), name_key(&record.title));
        if let Some(stamps) = free.get(&names) {
            let from = stamps.partition_point(|at| *at <= previous_end);
            let to = stamps.partition_point(|at| *at < event.start_ms);
            count += to.saturating_sub(from) as u64;
        }
    }
    count
}

/// A sample of what each tier not in force would join.
fn samples(input: &DryRunInput<'_>, every: &Matched) -> Vec<JoinSample> {
    let mut samples = Vec::new();
    for tier in [Tier::During, Tier::Renamed] {
        let on = match tier {
            Tier::During => Tiers::CURRENT.during,
            _ => Tiers::CURRENT.renamed,
        };
        if on {
            continue;
        }
        let mut joins: Vec<_> = every
            .joins
            .iter()
            .filter(|join| join.tier == Some(tier))
            .collect();
        joins.sort_by_key(|join| {
            every
                .events
                .get(join.event)
                .map_or(0, |event| event.start_ms)
        });
        for join in spread(&joins, SAMPLES_PER_TIER) {
            let (Some(event), Some(scrobble)) = (
                every.events.get(join.event),
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
    use ritornel_contract::SpotifyPlatform;

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
            platform: SpotifyPlatform::Mac,
        }
    }

    fn scrobble(at_s: i64, title: &str) -> Scrobble {
        Scrobble {
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
            let mut keys: Vec<String> = keyed(records)
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
        let keyed_other = keyed(vec![low.clone(), low.clone(), other.clone()]);
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
    fn a_dry_run_counts_by_year_and_says_what_stands_in_the_way() {
        let streams = keyed(vec![
            // Joined by tier A.
            record(221, 221, "Low Tide", "trackdone"),
            // Alone, and counts: its track's length is 3:41.
            record(1000, 221, "Low Tide", "trackdone"),
            // Alone, twelve seconds: a skip.
            record(1100, 12, "Paper Lanterns", "fwdbtn"),
            // Alone, no length to judge it by.
            record(1300, 100, "Night Ferry", "endplay"),
        ]);
        let scrobbles = vec![
            scrobble(1, "Low Tide"),
            // Stamped as the stream played: tier B, not in force.
            scrobble(1250, "Night Ferry"),
        ];
        let plays = BTreeMap::from([(2019, (10, 8))]);
        let report = dry_run(&DryRunInput {
            streams: &streams,
            scrobbles: &scrobbles,
            excluded: 3,
            echoes: 2,
            plays_by_year: &plays,
            listener: 1,
            zone_periods: &[],
            pins: &Pins::default(),
            made_at: 7,
        });
        assert_eq!((report.streams, report.events, report.scrobbles), (4, 4, 2));
        assert_eq!((report.scrobbles_excluded, report.scrobbles_echoes), (3, 2));
        let year = &report.years[0];
        assert_eq!(year.year, 2019);
        assert_eq!(
            (
                year.joined_start,
                year.alone_counted,
                year.alone_not_counted,
                year.alone_unknown
            ),
            (1, 1, 1, 1)
        );
        assert_eq!(year.scrobbles_alone, 1);
        assert_eq!((year.plays_after, year.counted_after), (13, 9));
        assert_eq!(year.no_zone, 4);
        assert_eq!(
            report.problems,
            [ImportProblem::NoZone, ImportProblem::TierNotInForce]
        );
        let during = report
            .tiers
            .iter()
            .find(|tier| tier.tier == StreamTier::During)
            .unwrap();
        assert_eq!((during.joined, during.in_force), (1, false));
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
            scrobbles: &scrobbles,
            excluded: 0,
            echoes: 0,
            plays_by_year: &plays,
            listener: 1,
            zone_periods: &zones,
            pins: &Pins::default(),
            made_at: 7,
        });
        assert_eq!(report.years[0].no_zone, 0);
        assert_eq!(report.problems, [ImportProblem::TierNotInForce]);
    }
}
