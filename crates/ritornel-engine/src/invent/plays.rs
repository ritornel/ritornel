//! An invented listening history, for seeing a page's history and the
//! History tab at the size of a real one before a real one is imported.
//! Developer-only, as the rest of this module is: `ritornel-server invent
//! --plays <n>` reaches it.
//!
//! The plays are made up as a person's would be, and go in as a Last.fm
//! import through the one ingest call, so that they are matched to the
//! library as an imported history is. Each artist is found in some month
//! of the years the history runs over, is played most for a while after,
//! and then less, with a few kept for good and a few coming back years
//! later. A fifth of the plays are of strangers the library does not have,
//! so they stay unbound, as many real plays do. Every name is invented,
//! from the library or from the same word lists; none comes from a real
//! catalogue. The same seed makes the same history.

use std::collections::BTreeMap;

use ritornel_source::{Batch, Identifiers, ImportRecord, Span, TrackText};

use super::{InventError, Rng, artist_name, title};
use crate::{IngestOutcome, LibraryStore, Observations, WriteRequest, ZonePeriodRow};

/// The account the invented plays are imported under.
pub const ACCOUNT: &str = "invented";

/// About a month.
const MONTH_MS: i64 = 2_629_746_000;
const DAY_MS: i64 = 86_400_000;
/// How many records go in at once, as a sync stores a span.
const BATCH: usize = 5_000;
/// The share of plays that are of strangers.
const STRANGERS: f32 = 0.2;

/// What history to invent.
#[derive(Debug, Clone)]
pub struct PlaysPlan {
    /// How many plays.
    pub plays: usize,
    /// What they are made from.
    pub seed: u64,
    /// The time they are invented at: the last falls before it.
    pub now_ms: i64,
    /// How many years they run over, back from now.
    pub years: u32,
    /// The zone the listener is taken to have been in, when the library
    /// has none for them yet: an imported play is dated in it.
    pub zone: String,
}

/// An artist and their records, each a title and its songs.
type Discography = (String, Vec<(String, Vec<String>)>);

/// One record as the invented listener knows it.
struct Record {
    title: String,
    songs: Vec<String>,
    /// How much it is liked beside the artist's others.
    liked: f32,
    /// The month it was found in: the first with the artist, the rest later.
    from: f32,
}

impl Record {
    /// How likely it is to be the one played in `month`: not before it was
    /// found, and most in the months after.
    fn pull(&self, month: f32) -> f32 {
        if month < self.from {
            return 0.0;
        }
        self.liked * (1.0 + 2.0 * 0.5_f32.powf((month - self.from) / 8.0))
    }
}

/// One artist as the invented listener knows them: when they were found,
/// how much they were played, and how that faded.
struct Taste {
    artist: String,
    records: Vec<Record>,
    /// The month they were found in, from the start of the history.
    found: f32,
    weight: f32,
    /// How many months it takes for half the liking to go.
    half_life: f32,
    /// What is left of it for good.
    floor: f32,
    /// A month they came back in, and how strongly.
    revival: Option<(f32, f32)>,
}

impl Taste {
    /// How much they are played in `month`.
    fn liking(&self, month: f32) -> f32 {
        let since = month - self.found;
        if since < 0.0 {
            return 0.0;
        }
        let first = (since + 1.0).min(2.0) / 2.0;
        let fading = self.floor + (1.0 - self.floor) * 0.5_f32.powf(since / self.half_life);
        let back = self.revival.map_or(0.0, |(at, strength)| {
            strength * (-((month - at) / 4.0).powi(2)).exp()
        });
        self.weight * (first * fading + back)
    }
}

/// A number near 0, spread as a bell: the sum of four draws, centred.
fn bell(rng: &mut Rng) -> f32 {
    (rng.float() + rng.float() + rng.float() + rng.float() - 2.0) * 1.7
}

/// Picks an index by weight, from the running totals `sums`.
fn weighted(rng: &mut Rng, sums: &[f32]) -> usize {
    let total = sums.last().copied().unwrap_or(0.0);
    let at = rng.float() * total;
    sums.partition_point(|&sum| sum <= at)
        .min(sums.len().saturating_sub(1))
}

fn running(weights: impl Iterator<Item = f32>) -> Vec<f32> {
    let mut sum = 0.0;
    weights
        .map(|weight| {
            sum += weight;
            sum
        })
        .collect()
}

/// The library's records by artist, as a scrobble would name them: each
/// record once, by the release its group shows.
fn library_records(
    library: &LibraryStore,
) -> Result<BTreeMap<String, BTreeMap<String, Vec<String>>>, InventError> {
    let rows: Vec<(String, String, String)> = library.read(|connection| {
        let mut statement = connection.prepare(
            "SELECT ac.display_text, rel.display_title, ta.display_title
             FROM release_group g
             JOIN release rel ON rel.id = coalesce(g.display_release_id,
               (SELECT min(id) FROM release WHERE release_group_id = g.id))
             JOIN track_appearance ta ON ta.release_id = rel.id AND ta.redirect_to_id IS NULL
             JOIN artist_credit ac ON ac.id = coalesce(ta.display_artist_credit_id,
                                                       rel.display_artist_credit_id)
             WHERE rel.display_title IS NOT NULL AND ta.display_title IS NOT NULL
             ORDER BY g.id, ta.display_disc_number, ta.display_track_number, ta.id",
        )?;
        let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    })?;
    let mut artists: BTreeMap<String, BTreeMap<String, Vec<String>>> = BTreeMap::new();
    for (artist, album, title) in rows {
        artists
            .entry(artist)
            .or_default()
            .entry(album)
            .or_default()
            .push(title);
    }
    Ok(artists)
}

/// Artists the library does not have, each with a record or two: a name
/// the library has is drawn again.
fn strangers(rng: &mut Rng, count: usize, owned: &[Discography]) -> Vec<Discography> {
    let taken: std::collections::HashSet<String> =
        owned.iter().map(|(name, _)| name.to_lowercase()).collect();
    (0..count)
        .filter_map(|_| {
            let name = artist_name(rng).0;
            let records = (0..rng.between(1, 3))
                .map(|_| {
                    let songs = (0..rng.between(3, 11)).map(|_| title(rng)).collect();
                    (title(rng), songs)
                })
                .collect();
            (!taken.contains(&name.to_lowercase())).then_some((name, records))
        })
        .collect()
}

fn tastes(rng: &mut Rng, artists: Vec<Discography>, months: f32) -> Vec<Taste> {
    artists
        .into_iter()
        .filter(|(_, records)| !records.is_empty())
        .map(|(artist, records)| {
            // Some known already when the history starts; the rest found
            // more often early, when the whole of a collection was new,
            // and steadily after.
            let found = if rng.chance(0.12) {
                -24.0 * rng.float()
            } else {
                months * rng.float().powf(1.25)
            };
            let revival = rng.chance(0.15).then(|| {
                let at = found + 24.0 + rng.float() * 96.0;
                (at, 0.4 + rng.float())
            });
            Taste {
                artist,
                records: records
                    .into_iter()
                    .enumerate()
                    .map(|(index, (title, songs))| Record {
                        title,
                        songs,
                        liked: (bell(rng) * 0.8).exp(),
                        from: if index == 0 {
                            found
                        } else {
                            found + 72.0 * rng.float()
                        },
                    })
                    .collect(),
                found,
                weight: (bell(rng) * 1.1).exp(),
                half_life: 2.0 + 40.0 * rng.float().powi(2),
                floor: 0.01 + 0.25 * rng.float().powi(3),
                revival,
            }
        })
        .collect()
}

/// How much was listened to in each month: a life's slow ups and downs,
/// a little more in winter, and a first year still getting going.
fn months_of(rng: &mut Rng, months: usize, plays: usize) -> Vec<usize> {
    let mut level = 0.0_f32;
    let shape: Vec<f32> = (0..months)
        .map(|month| {
            level = 0.92 * level + 0.25 * bell(rng);
            let season = 1.0 + 0.12 * (month as f32 / 12.0 * std::f32::consts::TAU).cos();
            let start = (0.3 + month as f32 / 12.0).min(1.0);
            (0.6 * level).exp() * season * start
        })
        .collect();
    let total: f32 = shape.iter().sum();
    shape
        .iter()
        .map(|share| (share / total * plays as f32).round() as usize)
        .collect()
}

/// An hour of the day, as people listen: little at night, most in the
/// evening.
fn hour(rng: &mut Rng) -> i64 {
    const WEIGHTS: [f32; 24] = [
        2.0, 1.0, 0.5, 0.3, 0.2, 0.3, 1.0, 2.5, 4.0, 4.5, 4.5, 4.5, 5.0, 5.0, 4.5, 4.5, 5.0, 5.5,
        6.0, 6.5, 7.0, 6.5, 5.0, 3.5,
    ];
    let sums = running(WEIGHTS.iter().copied());
    i64::try_from(weighted(rng, &sums)).unwrap_or(20)
}

/// The hour a sitting with an artist starts at, from the listener's usual
/// `hour`: a few of the library's artists are mostly kept to a time of day
/// of their own, mornings or late nights.
fn hours_of(who: usize, stranger: bool, hour: i64) -> i64 {
    match (stranger, who % 9, hour % 3) {
        (_, _, 0) | (true, ..) => hour,
        (false, 2, _) => 6 + hour % 5,
        (false, 5, _) => hour % 4,
        _ => hour,
    }
}

/// Invents `plan.plays` plays of `library`'s music and of strangers', and
/// imports them under the account [`ACCOUNT`]. `progress` is told how many
/// are stored as it goes. Run again with the same plan, it stores nothing
/// new. Returns how many it stored.
pub fn invent_plays(
    library: &LibraryStore,
    plan: &PlaysPlan,
    mut progress: impl FnMut(usize, usize),
) -> Result<usize, InventError> {
    let mut rng = Rng::new(plan.seed ^ 0x706c_6179);
    let owned = library_records(library)?;
    if owned.is_empty() {
        return Err(InventError("the library has no records to play".to_owned()));
    }
    let months = usize::try_from(plan.years.max(1) * 12).unwrap_or(12);
    let owned: Vec<_> = owned
        .into_iter()
        .map(|(artist, records)| (artist, records.into_iter().collect()))
        .collect();
    let stranger_count = (owned.len() / 3).max(20);
    let others = strangers(&mut rng, stranger_count, &owned);
    let mine = tastes(&mut rng, owned, months as f32);
    let theirs = tastes(&mut rng, others, months as f32);
    let start_ms = plan.now_ms - i64::try_from(months).unwrap_or(0) * MONTH_MS;
    let mut plays: Vec<(i64, bool, usize, usize, usize)> = Vec::with_capacity(plan.plays);
    for (month, count) in months_of(&mut rng, months, plan.plays)
        .into_iter()
        .enumerate()
    {
        let at = month as f32 + 0.5;
        let pools = [
            running(mine.iter().map(|taste| taste.liking(at))),
            running(theirs.iter().map(|taste| taste.liking(at))),
        ];
        let some = |sums: &Vec<f32>| sums.last().is_some_and(|&total| total > 0.0);
        let (any_mine, any_theirs) = (some(&pools[0]), some(&pools[1]));
        let mut left = count;
        while left > 0 && (any_mine || any_theirs) {
            let stranger = any_theirs && (!any_mine || rng.chance(STRANGERS));
            let (pool, sums) = if stranger {
                (&theirs, &pools[1])
            } else {
                (&mine, &pools[0])
            };
            let who = weighted(&mut rng, sums);
            let Some(taste) = pool.get(who) else { continue };
            let record = weighted(
                &mut rng,
                &running(taste.records.iter().map(|record| record.pull(at))),
            );
            let songs = taste
                .records
                .get(record)
                .map_or(1, |record| record.songs.len().max(1));
            // Songs come in sittings: a song alone, or a run of a record
            // from where it was started, the first songs started from most.
            let first = ((songs as f32) * rng.float().powf(1.4)) as usize;
            let run = if rng.chance(0.45) {
                1
            } else {
                usize::try_from(rng.between(3, 12)).unwrap_or(3)
            };
            let run = run.min(songs - first.min(songs - 1)).min(left);
            // From a midnight: a month is not a whole number of days, and
            // counted from anywhere else the hours drift through the day.
            let month_ms = start_ms + i64::try_from(month).unwrap_or(0) * MONTH_MS;
            let mut when = month_ms - month_ms.rem_euclid(DAY_MS)
                + i64::try_from(rng.below(30)).unwrap_or(0) * DAY_MS
                + hours_of(who, stranger, hour(&mut rng)) * 3_600_000
                + i64::try_from(rng.below(3_600)).unwrap_or(0) * 1_000;
            // The hour is the listener's, where they were.
            when -= crate::local_time(when, &plan.zone)
                .map_or(0, |local| local.offset_minutes * 60_000);
            for song in first..first + run {
                if when < plan.now_ms - 3_600_000 {
                    plays.push((when / 1_000 * 1_000, stranger, who, record, song));
                }
                when += i64::try_from(rng.between(150, 330)).unwrap_or(240) * 1_000;
            }
            left -= run;
        }
    }
    plays.sort_unstable_by_key(|play| play.0);

    let account = library.ensure_service_account("lastfm", ACCOUNT)?;
    let listener = library
        .default_listener()?
        .ok_or_else(|| InventError("the library has no listener".to_owned()))?;
    if library.zone_periods_of(listener)?.is_empty() {
        library.set_zone_periods(
            WriteRequest {
                method: "invent.zones",
                command: None,
                client: None,
            },
            listener,
            vec![ZonePeriodRow {
                zone: plan.zone.clone(),
                from_ms: None,
                to_ms: None,
            }],
            |_| String::new(),
        )?;
    }
    let mut stored = 0;
    for (chunk_index, chunk) in plays.chunks(BATCH).enumerate() {
        let records: Vec<ImportRecord> = chunk
            .iter()
            .enumerate()
            .filter_map(|(offset, &(at, stranger, who, record, song))| {
                let n = chunk_index * BATCH + offset;
                let taste = if stranger { &theirs } else { &mine }.get(who)?;
                let record = taste.records.get(record)?;
                let mut key = b"inv1".to_vec();
                key.extend_from_slice(&(n as u64).to_be_bytes());
                Some(ImportRecord {
                    source_key: key,
                    played_at_ms: at,
                    track: TrackText {
                        artist: taste.artist.clone(),
                        album: Some(record.title.clone()),
                        title: record.songs.get(song)?.clone(),
                    },
                    ids: Identifiers::default(),
                    payload_json: format!("{{\"invented\":{n}}}"),
                })
            })
            .collect();
        let (Some(first), Some(last)) = (records.first(), records.last()) else {
            continue;
        };
        let span = Span {
            from_ms: first.played_at_ms,
            to_ms: last.played_at_ms + 1_000,
        };
        let cursor = format!("{{\"readBefore\":{}}}", span.to_ms / 1_000);
        let outcome = library.ingest(Observations::Imports {
            account,
            batch: &Batch {
                records,
                span,
                cursor,
            },
            full: false,
        })?;
        if let IngestOutcome::Imports(batch) = outcome {
            stored += usize::try_from(batch.new).unwrap_or(0);
        }
        progress((chunk_index + 1) * BATCH, plays.len());
    }
    Ok(stored)
}
