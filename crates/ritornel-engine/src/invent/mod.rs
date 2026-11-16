//! Invented libraries, for developing and measuring before there is a scan
//! of real music. Developer-only: `ritornel-server invent` and the hidden
//! `--invent-churn` flag of `serve` reach it, and neither is in the help.
//!
//! [`seed`] fills a library with artists, their records and tracks, made up
//! from word lists ([`words`]) and a seed, with covers painted from the same
//! seed ([`art`]) and written as folder images beside where the tracks would
//! be, in a root of their own. A few artists have many records and most
//! have one or two; some records have more than one version, which collapse
//! into one group; some have no cover. The same seed makes the same library:
//! names, IDs, covers and all. Every row goes in through
//! [`LibraryStore::write`], in batches, shaped as the scanner's would be,
//! the text search index included. No audio file is written: a track's file
//! is a path in its source item, and only its folder's cover is on disk.
//!
//! [`Churn`] writes to a library that is being read, a step at a time:
//! records added, titles and years changed, covers replaced. It stands in
//! for a scan running while someone scrolls.

mod art;
mod showcase;
#[cfg(test)]
pub(crate) mod testing;
mod words;

pub use showcase::seed_showcase;

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use ritornel_contract::Id;
use rusqlite::Connection;

use crate::library_store::Write;
use crate::{Error, LibraryStore, ServerStore, WriteRequest};

/// The name of the root invented covers are written in, in `server.db` and
/// `library.db`.
pub const ROOT_NAME: &str = "invented";

/// Why inventing failed.
#[derive(Debug)]
pub struct InventError(String);

impl fmt::Display for InventError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for InventError {}

impl From<Error> for InventError {
    fn from(error: Error) -> Self {
        Self(error.to_string())
    }
}

impl From<std::io::Error> for InventError {
    fn from(error: std::io::Error) -> Self {
        Self(format!("the invented root: {error}"))
    }
}

/// A small, fast generator of numbers that are the same for the same seed:
/// SplitMix64. Nothing here needs more.
#[derive(Debug, Clone)]
pub(crate) struct Rng(u64);

impl Rng {
    pub(crate) fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub(crate) fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// A number from 0 up to `bound`, not including it. 0 when `bound` is.
    pub(crate) fn below(&mut self, bound: u64) -> u64 {
        if bound == 0 { 0 } else { self.next() % bound }
    }

    /// A number from `low` to `high`, both included.
    pub(crate) fn between(&mut self, low: u64, high: u64) -> u64 {
        low + self.below(high.saturating_sub(low) + 1)
    }

    /// A number from 0 to 1.
    pub(crate) fn float(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }

    /// Whether something that happens this often happens.
    pub(crate) fn chance(&mut self, often: f32) -> bool {
        self.float() < often
    }

    pub(crate) fn pick<'a>(&mut self, from: &[&'a str]) -> &'a str {
        let index = usize::try_from(self.below(from.len() as u64)).unwrap_or_default();
        from.get(index).copied().unwrap_or_default()
    }

    fn bytes<const N: usize>(&mut self) -> [u8; N] {
        let mut bytes = [0; N];
        for chunk in bytes.chunks_mut(8) {
            let word = self.next().to_le_bytes();
            chunk.copy_from_slice(word.get(..chunk.len()).unwrap_or_default());
        }
        bytes
    }
}

/// What to invent.
#[derive(Debug, Clone)]
pub struct Plan {
    /// How many records, counting a record with several versions once.
    pub albums: usize,
    /// What everything is made from. The same seed makes the same library.
    pub seed: u64,
    /// Where the covers are written. It is made if it is missing.
    pub root: PathBuf,
    /// The time the library is invented at: when its records came in is
    /// counted back from it.
    pub now_ms: i64,
    /// The smallest and largest side of a cover, in pixels.
    pub cover_sides: (u32, u32),
}

/// What was invented.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Invented {
    /// Artists who have a record.
    pub artists: usize,
    /// Records, counting one with several versions once.
    pub groups: usize,
    /// Releases: every version of every record.
    pub releases: usize,
    /// Tracks, on every version.
    pub tracks: usize,
    /// Covers written.
    pub covers: usize,
    /// The bytes of the covers written.
    pub cover_bytes: u64,
}

/// What a record is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Album,
    Ep,
    Single,
}

/// A format, as an invented record's files are in.
#[derive(Debug, Clone, Copy)]
struct Format {
    codec: &'static str,
    lossless: bool,
    bit_depth: Option<u32>,
    rate: u32,
    bitrate: Option<u32>,
    extension: &'static str,
}

/// The format most records are in.
const CD: Format = Format {
    codec: "flac",
    lossless: true,
    bit_depth: Some(16),
    rate: 44_100,
    bitrate: None,
    extension: "flac",
};

const FORMATS: &[Format] = &[
    CD,
    Format {
        codec: "flac",
        lossless: true,
        bit_depth: Some(24),
        rate: 96_000,
        bitrate: None,
        extension: "flac",
    },
    Format {
        codec: "mp3",
        lossless: false,
        bit_depth: None,
        rate: 44_100,
        bitrate: Some(320),
        extension: "mp3",
    },
    Format {
        codec: "aac",
        lossless: false,
        bit_depth: None,
        rate: 44_100,
        bitrate: Some(256),
        extension: "m4a",
    },
];

/// One version of a record, planned.
#[derive(Debug, Clone)]
struct Version {
    title: String,
    year: i64,
    /// How many of the record's recordings it has, from the first.
    tracks: usize,
}

/// One record, planned.
#[derive(Debug, Clone)]
struct Album {
    /// The artist, by index; `None` for a compilation.
    artist: Option<usize>,
    title: String,
    kind: Kind,
    first_year: i64,
    versions: Vec<Version>,
    /// The recordings' titles, durations and the artists of a
    /// compilation's tracks, in order. A later version may have more.
    recordings: Vec<(String, i64, Option<usize>)>,
    discs: i64,
    format: usize,
    /// One track in a different format, as a record pieced together does.
    mixed: bool,
    added_ms: i64,
    /// The cover: its seed and side. `None` for a record with none.
    cover: Option<(u64, u32)>,
    /// Whether its files say what kind of record it is. Most of the
    /// owner's do not.
    typed: bool,
    /// The marks beside its kind, such as `live` or `compilation`.
    marks: &'static [&'static str],
    /// Whether its files say when it came out.
    dated: bool,
}

/// An artist, planned.
#[derive(Debug, Clone)]
struct Artist {
    name: String,
    sort_name: Option<String>,
}

/// Makes an artist's name.
fn artist_name(rng: &mut Rng) -> (String, Option<String>) {
    match rng.below(10) {
        0..=2 => {
            let (given, family) = (rng.pick(words::GIVEN), rng.pick(words::FAMILY));
            (
                format!("{given} {family}"),
                Some(format!("{family}, {given}")),
            )
        }
        3 => (
            format!("{} {}", rng.pick(words::GIVEN), rng.pick(words::BAND_TAILS)),
            None,
        ),
        4..=5 => (
            format!(
                "The {} {}",
                rng.pick(words::ADJECTIVES),
                rng.pick(words::NOUNS)
            ),
            None,
        ),
        6 => (rng.pick(words::SINGLE_WORDS).to_owned(), None),
        7 => (
            format!("{} {}", rng.pick(words::SMALL), rng.pick(words::NOUNS)),
            None,
        ),
        // Names that begin with no letter, or with an accent, so that `#`
        // and folding are exercised.
        8 if rng.chance(0.3) => (
            format!("{} {}", rng.between(2, 99), rng.pick(words::NOUNS)),
            None,
        ),
        _ => (
            format!("{} {}", rng.pick(words::ADJECTIVES), rng.pick(words::NOUNS)),
            None,
        ),
    }
}

/// Makes a record's or a song's title.
fn title(rng: &mut Rng) -> String {
    match rng.below(9) {
        0 => rng.pick(words::SINGLE_WORDS).to_owned(),
        1 => format!("{} {}", rng.pick(words::ADJECTIVES), rng.pick(words::NOUNS)),
        2 => format!(
            "The {} {}",
            rng.pick(words::ADJECTIVES),
            rng.pick(words::NOUNS)
        ),
        3 => format!("{} of {}", rng.pick(words::NOUNS), rng.pick(words::SMALL)),
        4 => format!("{} in {}", rng.pick(words::VERBS), rng.pick(words::PLACES)),
        5 => format!("Songs for {}", rng.pick(words::PLACES)),
        6 => format!(
            "{} {}",
            rng.pick(words::SMALL),
            rng.pick(words::SINGLE_WORDS)
        ),
        7 => rng.pick(words::VERBS).to_owned(),
        _ => format!("{} & {}", rng.pick(words::SMALL), rng.pick(words::SMALL)),
    }
}

/// How many records an artist has: most one or two, a few a great many.
fn records_for(rng: &mut Rng) -> usize {
    let roll = rng.float();
    let count = if roll < 0.55 {
        1
    } else if roll < 0.75 {
        2
    } else if roll < 0.85 {
        3
    } else if roll < 0.92 {
        rng.between(4, 6)
    } else if roll < 0.97 {
        rng.between(7, 12)
    } else {
        rng.between(13, 40)
    };
    usize::try_from(count).unwrap_or(1)
}

/// Plans the artists and the records, from the seed.
fn plan_library(plan: &Plan) -> (Vec<Artist>, Vec<Album>) {
    let mut rng = Rng::new(plan.seed);
    let mut artists = Vec::new();
    let mut albums = Vec::new();
    let year_ms: i64 = 365 * 86_400_000;
    let mut taken = std::collections::HashSet::new();
    while albums.len() < plan.albums {
        // Two made-up acts of one name would be one artist, so a name that
        // is taken is drawn again, and then told apart.
        let (mut name, mut sort_name) = artist_name(&mut rng);
        for _ in 0..8 {
            if !taken.contains(&name) {
                break;
            }
            (name, sort_name) = artist_name(&mut rng);
        }
        let mut tried = 0;
        while taken.contains(&name) {
            let tail = rng.pick(words::ACT_TAILS);
            name = if tried < 4 {
                format!("{name} {tail}")
            } else {
                format!("{name} {}", rng.between(2, 9))
            };
            sort_name = None;
            tried += 1;
        }
        taken.insert(name.clone());
        let artist = artists.len();
        artists.push(Artist { name, sort_name });
        let debut = 1960 + i64::try_from(rng.below(62)).unwrap_or_default();
        for _ in 0..records_for(&mut rng) {
            if albums.len() >= plan.albums {
                break;
            }
            albums.push(plan_album(&mut rng, Some(artist), debut, plan, year_ms));
        }
        // Now and then, a compilation of several artists.
        if rng.chance(0.03) && albums.len() < plan.albums && artists.len() > 3 {
            let mut album = plan_album(&mut rng, None, 1975, plan, year_ms);
            let count = artists.len() as u64;
            for recording in &mut album.recordings {
                recording.2 = usize::try_from(rng.below(count)).ok();
            }
            albums.push(album);
        }
    }
    (artists, albums)
}

fn plan_album(
    rng: &mut Rng,
    artist: Option<usize>,
    debut: i64,
    plan: &Plan,
    year_ms: i64,
) -> Album {
    let kind = match rng.below(100) {
        0..=77 => Kind::Album,
        78..=89 => Kind::Ep,
        _ => Kind::Single,
    };
    let tracks = match kind {
        Kind::Album if rng.chance(0.08) => rng.between(18, 30),
        Kind::Album => rng.between(10, 18),
        Kind::Ep => rng.between(4, 6),
        Kind::Single => rng.between(1, 3),
    };
    let first_year = (debut + i64::try_from(rng.below(25)).unwrap_or_default()).min(2026);
    let title = match kind {
        Kind::Album if rng.chance(0.04) => {
            format!("{} {}", title(rng), rng.pick(words::ROMAN))
        }
        _ => title(rng),
    };
    let mut recordings = Vec::new();
    for index in 0..tracks {
        // The single is named for its first song.
        let name = if index == 0 && kind == Kind::Single {
            title.clone()
        } else {
            self::title(rng)
        };
        let duration = i64::try_from(rng.between(95_000, 420_000)).unwrap_or(200_000);
        recordings.push((name, duration, None));
    }
    let mut versions = vec![Version {
        title: title.clone(),
        year: first_year,
        tracks: recordings.len(),
    }];
    // Some records come in more than one version: a later remaster, or a
    // deluxe edition with songs the first did not have.
    if kind == Kind::Album && rng.chance(0.06) {
        for _ in 0..rng.between(1, 2) {
            let edition = rng.pick(words::EDITIONS);
            let year = (first_year + i64::try_from(rng.between(5, 30)).unwrap_or(10)).min(2026);
            let mut count = recordings.len();
            if edition != "Remastered" {
                for _ in 0..rng.between(2, 6) {
                    let duration = i64::try_from(rng.between(120_000, 360_000)).unwrap_or(200_000);
                    recordings.push((self::title(rng), duration, None));
                }
                count = recordings.len();
            }
            versions.push(Version {
                title: format!("{title} ({edition})"),
                year,
                tracks: count,
            });
        }
    }
    let discs = if recordings.len() > 20 { 2 } else { 1 };
    let format = match rng.below(100) {
        0..=54 => 0,
        55..=69 => 1,
        70..=89 => 2,
        _ => 3,
    };
    // When it came in: over the last ten years, more of them lately, and a
    // few this week.
    let back = if rng.chance(0.01) {
        i64::try_from(rng.below(6 * 86_400_000)).unwrap_or_default()
    } else {
        let spread = rng.float().powi(2) * 10.0;
        (spread * year_ms as f32) as i64
    };
    let (low, high) = plan.cover_sides;
    let cover = (!rng.chance(0.06)).then(|| {
        let side = u32::try_from(rng.between(u64::from(low), u64::from(high))).unwrap_or(low);
        (rng.next(), side)
    });
    Album {
        artist,
        title,
        kind,
        first_year,
        versions,
        recordings,
        discs,
        format,
        mixed: rng.chance(0.03),
        added_ms: plan.now_ms - back,
        cover,
        typed: true,
        marks: &[],
        dated: true,
    }
}

/// A name made safe to be a folder's.
fn folder(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|character| match character {
            '/' | '\\' | ':' | '\0' => '_',
            other => other,
        })
        .collect();
    let trimmed =
        cleaned.trim_matches(|character: char| character == '.' || character.is_whitespace());
    if trimmed.is_empty() {
        "_".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// A cover painted and written to disk, waiting for its rows.
#[derive(Debug, Clone)]
struct Painted {
    hash: [u8; 32],
    side: u32,
    bytes: u64,
}

/// Paints the covers of these records and writes each to its folder, on
/// as many threads as there are cores.
fn paint(
    root: &Path,
    albums: &[(usize, Album, String)],
) -> Result<Vec<Option<Painted>>, InventError> {
    let threads = std::thread::available_parallelism().map_or(4, usize::from);
    let chunk = albums.len().div_ceil(threads).max(1);
    let mut painted = vec![None; albums.len()];
    std::thread::scope(|scope| {
        let mut running = Vec::new();
        for (part, slots) in albums.chunks(chunk).zip(painted.chunks_mut(chunk)) {
            running.push(scope.spawn(move || -> Result<(), InventError> {
                for ((_, album, dir), slot) in part.iter().zip(slots.iter_mut()) {
                    let Some((seed, side)) = album.cover else {
                        continue;
                    };
                    let jpeg = art::cover(seed, side).map_err(InventError)?;
                    let folder = root.join(dir);
                    std::fs::create_dir_all(&folder)?;
                    std::fs::write(folder.join("cover.jpg"), &jpeg)?;
                    *slot = Some(Painted {
                        hash: *blake3::hash(&jpeg).as_bytes(),
                        side,
                        bytes: jpeg.len() as u64,
                    });
                }
                Ok(())
            }));
        }
        for thread in running {
            thread
                .join()
                .map_err(|_| InventError("a thread painting covers failed".to_owned()))??;
        }
        Ok::<_, InventError>(())
    })?;
    Ok(painted)
}

/// What the rows of an invented library need to know across writes: the
/// next key of each table, the artists and credits made so far, and where
/// the files go.
#[derive(Debug)]
struct Rows {
    rng: Rng,
    /// IDs count up from here, a millisecond apart.
    id_ms: i64,
    next: HashMap<&'static str, i64>,
    instance: i64,
    /// Artists by name: their key and the key of the credit that is them
    /// alone.
    credits: HashMap<String, (i64, i64)>,
}

const TABLES: &[&str] = &[
    "artist",
    "artist_credit",
    "recording",
    "release_group",
    "release",
    "track_appearance",
    "audio_asset",
    "source_item",
    "source_item_revision",
    "identity_decision",
    "assignment",
    "picture",
];

impl Rows {
    /// Reads where each table's keys are up to, and makes the invented
    /// root's rows if it has none.
    fn start(write: &Write<'_>, seed: u64, now_ms: i64) -> Result<Self, Error> {
        let connection = write.connection();
        let mut next = HashMap::new();
        for table in TABLES {
            let max: i64 = connection.query_row(
                &format!("SELECT coalesce(max(id), 0) FROM {table}"),
                [],
                |row| row.get(0),
            )?;
            next.insert(*table, max + 1);
        }
        // The root and its source are added by `seed` before any write
        // here, as `sources.add` adds them.
        let instance: i64 = connection.query_row(
            "SELECT src.id FROM source_instance src JOIN library_root lr ON lr.id = src.root_id
             WHERE lr.name = ?1",
            [ROOT_NAME],
            |row| row.get(0),
        )?;
        let rng = Rng::new(seed ^ 0x5eed);
        // Artists made before, so a second run adds to them.
        let mut credits = HashMap::new();
        let mut statement = connection.prepare(
            "SELECT a.display_name, a.id, acn.artist_credit_id
             FROM artist_credit_name acn JOIN artist a ON a.id = acn.artist_id
             WHERE acn.position = 0
               AND (SELECT count(*) FROM artist_credit_name x WHERE x.artist_credit_id = acn.artist_credit_id) = 1",
        )?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            if let Some(name) = row.get::<_, Option<String>>(0)? {
                credits.insert(name, (row.get(1)?, row.get(2)?));
            }
        }
        Ok(Self {
            rng,
            id_ms: now_ms - 10 * 365 * 86_400_000,
            next,
            instance,
            credits,
        })
    }

    fn key(&mut self, table: &'static str) -> i64 {
        let next = self.next.entry(table).or_insert(1);
        let key = *next;
        *next += 1;
        key
    }

    fn public_id(&mut self) -> [u8; 16] {
        self.id_ms += 1;
        *Id::new_v7(self.id_ms, self.rng.bytes()).as_bytes()
    }

    /// The artist named `name` and the credit that is them alone, made if
    /// they are new.
    fn artist(
        &mut self,
        write: &Write<'_>,
        statements: &mut Statements<'_>,
        name: &str,
        sort_name: Option<&str>,
    ) -> Result<(i64, i64), Error> {
        if let Some(found) = self.credits.get(name) {
            return Ok(*found);
        }
        let (artist, credit) = (self.key("artist"), self.key("artist_credit"));
        let public_id = self.public_id();
        statements.artist.execute((
            artist,
            public_id,
            name,
            sort_name,
            name.to_lowercase(),
            write.rev(),
        ))?;
        statements
            .artist_credit
            .execute((credit, name, write.rev()))?;
        statements
            .artist_credit_name
            .execute((credit, artist, name, write.rev()))?;
        self.credits.insert(name.to_owned(), (artist, credit));
        Ok((artist, credit))
    }

    /// A picture row for a painted cover, and the key it has.
    fn picture(
        &mut self,
        write: &Write<'_>,
        statements: &mut Statements<'_>,
        painted: &Painted,
    ) -> Result<i64, Error> {
        let key = self.key("picture");
        let public_id = self.public_id();
        statements.picture.execute((
            key,
            public_id,
            painted.hash,
            painted.side,
            i64::try_from(painted.bytes).unwrap_or(i64::MAX),
            write.rev(),
        ))?;
        Ok(key)
    }

    /// Writes one record: its group, each version, the recordings, and a
    /// file for every track with its revision, audio, binding, cover and
    /// search text.
    fn album(
        &mut self,
        write: &Write<'_>,
        statements: &mut Statements<'_>,
        artists: &[Artist],
        album: &Album,
        dir: &str,
        painted: Option<&Painted>,
    ) -> Result<usize, Error> {
        let rev = write.rev();
        let various = Artist {
            name: "Various Artists".to_owned(),
            sort_name: None,
        };
        let owner = album
            .artist
            .and_then(|index| artists.get(index))
            .unwrap_or(&various);
        let (_, credit) =
            self.artist(write, statements, &owner.name, owner.sort_name.as_deref())?;
        let picture = match painted {
            Some(painted) => Some(self.picture(write, statements, painted)?),
            None => None,
        };
        let group = self.key("release_group");
        let group_id = self.public_id();
        let primary = album.typed.then_some(match album.kind {
            Kind::Album => "album",
            Kind::Ep => "ep",
            Kind::Single => "single",
        });
        let marks = format!(
            "[{}]",
            album
                .marks
                .iter()
                .map(|mark| json_text(mark))
                .collect::<Vec<_>>()
                .join(",")
        );
        let first_year = album.dated.then_some(album.first_year);
        let first_release = self.next.get("release").copied().unwrap_or(1);
        // Parents go in before what points at them, so that no foreign key
        // is ever outstanding: SQLite would otherwise search every child
        // table on each insert until it was resolved.
        statements.release_group.execute((
            group,
            group_id,
            None::<i64>,
            primary,
            first_year.map(|year| year.to_string()),
            first_year,
            rev,
            marks,
        ))?;

        // The recordings, which every version shares.
        let mut recordings = Vec::with_capacity(album.recordings.len());
        for (title, _, by) in &album.recordings {
            let key = self.key("recording");
            let public_id = self.public_id();
            let track_credit = match by.and_then(|index| artists.get(index)) {
                Some(by) => {
                    self.artist(write, statements, &by.name, by.sort_name.as_deref())?
                        .1
                }
                None => credit,
            };
            statements
                .recording
                .execute((key, public_id, title, track_credit, rev))?;
            recordings.push((key, track_credit));
        }

        let mut tracks = 0;
        for (number, version) in album.versions.iter().enumerate() {
            let release = self.key("release");
            let public_id = self.public_id();
            let year = album.dated.then_some(version.year);
            statements.release.execute((
                release,
                public_id,
                group,
                &version.title,
                credit,
                year.map(|year| year.to_string()),
                year,
                picture,
                rev,
            ))?;
            let per_disc = album
                .recordings
                .len()
                .div_ceil(usize::try_from(album.discs).unwrap_or(1).max(1));
            let version_dir = if number == 0 {
                dir.to_owned()
            } else {
                format!("{dir} [{number}]")
            };
            for (index, ((title, duration, by), (recording, track_credit))) in album
                .recordings
                .iter()
                .zip(&recordings)
                .take(version.tracks)
                .enumerate()
            {
                let disc = i64::try_from(index / per_disc.max(1)).unwrap_or_default() + 1;
                let track = i64::try_from(index % per_disc.max(1)).unwrap_or_default() + 1;
                let format = if album.mixed && index == 0 {
                    FORMATS.get((album.format + 2) % FORMATS.len())
                } else {
                    FORMATS.get(album.format)
                }
                .copied()
                .unwrap_or(CD);
                let path = format!(
                    "{version_dir}/{disc}-{track:02} {}.{}",
                    folder(title),
                    format.extension
                );
                let added = album.added_ms + i64::try_from(number).unwrap_or_default() * 86_400_000;
                self.track(
                    write,
                    statements,
                    Track {
                        release: Some(release),
                        recording: *recording,
                        credit: *track_credit,
                        title,
                        disc,
                        track,
                        duration: *duration,
                        added,
                        path: &path,
                        format,
                        picture,
                        artist: by
                            .and_then(|index| artists.get(index))
                            .map_or(owner.name.as_str(), |artist| artist.name.as_str()),
                        album: &version.title,
                    },
                )?;
                tracks += 1;
            }
        }
        statements.shows.execute((first_release, group))?;
        Ok(tracks)
    }

    fn track(
        &mut self,
        write: &Write<'_>,
        statements: &mut Statements<'_>,
        track: Track<'_>,
    ) -> Result<(), Error> {
        let rev = write.rev();
        let appearance = self.key("track_appearance");
        let appearance_id = self.public_id();
        statements.track_appearance.execute((
            appearance,
            appearance_id,
            track.recording,
            track.release,
            track.title,
            track.credit,
            track.disc,
            track.track,
            track.duration,
            track.added,
            rev,
        ))?;
        let asset = self.key("audio_asset");
        let asset_id = self.public_id();
        let audio_hash: [u8; 32] = self.rng.bytes();
        statements.audio_asset.execute((
            asset,
            asset_id,
            audio_hash,
            track.format.codec,
            track.format.rate,
            track.format.bit_depth,
            track.format.bitrate,
            track.format.lossless,
            track.duration,
            rev,
        ))?;
        let item = self.key("source_item");
        let revision = self.key("source_item_revision");
        let item_id = self.public_id();
        let size = track.duration * 110;
        statements.source_item.execute((
            item,
            item_id,
            self.instance,
            track.path,
            None::<i64>,
            track.recording,
            size,
            track.added,
            rev,
        ))?;
        let payload = format!(
            "{{\"invented\":true,\"path\":{},\"tags\":{{\"title\":{},\"artist\":{},\"album\":{}}}}}",
            json_text(track.path),
            json_text(track.title),
            json_text(track.artist),
            json_text(track.album),
        );
        let payload_hash: [u8; 32] = *blake3::hash(payload.as_bytes()).as_bytes();
        statements.source_item_revision.execute((
            revision,
            item,
            payload_hash,
            payload,
            asset,
            track.added,
            rev,
        ))?;
        statements.current_revision.execute((revision, item))?;
        let decision = self.key("identity_decision");
        statements
            .identity_decision
            .execute((decision, item, track.added, rev))?;
        let assignment = self.key("assignment");
        statements.assignment.execute((
            assignment,
            item,
            track.recording,
            appearance,
            decision,
            track.added,
            rev,
        ))?;
        if let Some(picture) = track.picture {
            statements
                .revision_picture
                .execute((revision, picture, rev))?;
        }
        statements
            .search_text
            .execute((appearance, track.title, track.artist, track.album))?;
        Ok(())
    }
}

/// The statements an invented library is written with, prepared once a
/// write.
struct Statements<'a> {
    artist: rusqlite::Statement<'a>,
    artist_credit: rusqlite::Statement<'a>,
    artist_credit_name: rusqlite::Statement<'a>,
    picture: rusqlite::Statement<'a>,
    release_group: rusqlite::Statement<'a>,
    recording: rusqlite::Statement<'a>,
    release: rusqlite::Statement<'a>,
    track_appearance: rusqlite::Statement<'a>,
    audio_asset: rusqlite::Statement<'a>,
    source_item: rusqlite::Statement<'a>,
    source_item_revision: rusqlite::Statement<'a>,
    identity_decision: rusqlite::Statement<'a>,
    assignment: rusqlite::Statement<'a>,
    revision_picture: rusqlite::Statement<'a>,
    search_text: rusqlite::Statement<'a>,
    shows: rusqlite::Statement<'a>,
    current_revision: rusqlite::Statement<'a>,
}

impl<'a> Statements<'a> {
    fn new(connection: &'a Connection) -> Result<Self, Error> {
        Ok(Self {
            shows: connection.prepare(
                "UPDATE release_group SET display_release_id = ?1 WHERE id = ?2",
            )?,
            current_revision: connection.prepare(
                "UPDATE source_item SET current_revision_id = ?1 WHERE id = ?2",
            )?,
            artist: connection.prepare("INSERT INTO artist (id, public_id, canonical_id, display_name, display_sort_name, match_name, rev)
                 VALUES (?1, ?2, ?1, ?3, ?4, ?5, ?6)")?,
            artist_credit: connection.prepare("INSERT INTO artist_credit (id, display_text, rev) VALUES (?1, ?2, ?3)")?,
            artist_credit_name: connection.prepare("INSERT INTO artist_credit_name (artist_credit_id, position, artist_id, credited_name, rev)
                 VALUES (?1, 0, ?2, ?3, ?4)")?,
            picture: connection.prepare("INSERT INTO picture (id, public_id, content_hash, mime_type, width, height, size_bytes, rev)
                 VALUES (?1, ?2, ?3, 'image/jpeg', ?4, ?4, ?5, ?6)")?,
            release_group: connection.prepare("INSERT INTO release_group (id, public_id, display_release_id, display_primary_type,
                                            display_first_date, display_first_year, rev,
                                            display_secondary_types_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)")?,
            recording: connection.prepare("INSERT INTO recording (id, public_id, canonical_id, display_title, display_artist_credit_id, rev)
                     VALUES (?1, ?2, ?1, ?3, ?4, ?5)")?,
            release: connection.prepare("INSERT INTO release (id, public_id, release_group_id, canonical_id, display_title,
                                          display_artist_credit_id, display_date, display_year,
                                          display_picture_id, rev)
                     VALUES (?1, ?2, ?3, ?1, ?4, ?5, ?6, ?7, ?8, ?9)")?,
            track_appearance: connection.prepare("INSERT INTO track_appearance (id, public_id, recording_id, release_id, canonical_id,
                                               display_title, display_artist_credit_id,
                                               display_disc_number, display_track_number,
                                               display_duration_ms, display_added_at_ms, rev)
                 VALUES (?1, ?2, ?3, ?4, ?1, ?5, ?6, ?7, ?8, ?9, ?10, ?11)")?,
            audio_asset: connection.prepare("INSERT INTO audio_asset (id, public_id, hash_scheme, audio_hash, codec, sample_rate_hz,
                                          channel_layout, bit_depth, bitrate_kbps, lossless, duration_ms, rev)
                 VALUES (?1, ?2, 1, ?3, ?4, ?5, 'stereo', ?6, ?7, ?8, ?9, ?10)")?,
            source_item: connection.prepare("INSERT INTO source_item (id, public_id, source_instance_id, item_kind, provider_key,
                                          current_revision_id, current_recording_id, size_bytes,
                                          mtime_ms, first_seen_at_ms, rev)
                 VALUES (?1, ?2, ?3, 'file', ?4, ?5, ?6, ?7, ?8, ?8, ?9)")?,
            source_item_revision: connection.prepare("INSERT INTO source_item_revision (id, source_item_id, revision_no, payload_hash,
                                                   payload_json, audio_asset_id, observed_at_ms, rev)
                 VALUES (?1, ?2, 1, ?3, ?4, ?5, ?6, ?7)")?,
            identity_decision: connection.prepare("INSERT INTO identity_decision (id, action, source_item_id, method, resolver_version,
                                                normalizer_version, score, evidence_json, actor_kind,
                                                decided_at_ms, rev)
                 VALUES (?1, 'bind', ?2, 'new', 1, 1, 0, '{\"invented\":true}', 'resolver', ?3, ?4)")?,
            assignment: connection.prepare("INSERT INTO assignment (id, source_item_id, recording_id, appearance_id, decision_id,
                                         effective_from_ms, rev)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)")?,
            revision_picture: connection.prepare("INSERT INTO revision_picture (revision_id, picture_id, position, source_kind, file_name, rev)
                     VALUES (?1, ?2, 0, 'folder_image', 'cover.jpg', ?3)")?,
            search_text: connection.prepare("INSERT INTO search_text (rowid, title, artist, album) VALUES (?1, ?2, ?3, ?4)")?,
        })
    }
}

/// One track's file, as [`Rows::track`] writes it.
struct Track<'a> {
    /// Its record, or `None` for a file that names none.
    release: Option<i64>,
    recording: i64,
    credit: i64,
    title: &'a str,
    disc: i64,
    track: i64,
    duration: i64,
    added: i64,
    path: &'a str,
    format: Format,
    picture: Option<i64>,
    artist: &'a str,
    album: &'a str,
}

/// Text as a JSON string.
fn json_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            control if u32::from(control) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", u32::from(control)));
            }
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

/// The folder of one record, under its artist's.
fn album_dir(artists: &[Artist], album: &Album, index: usize) -> String {
    let artist = album
        .artist
        .and_then(|artist| artists.get(artist))
        .map_or("Various Artists", |artist| artist.name.as_str());
    format!("{}/{} ({index})", folder(artist), folder(&album.title))
}

/// How many records go in one write.
const BATCH: usize = 250;

fn request(method: &'static str) -> WriteRequest {
    WriteRequest {
        method,
        command: None,
        client: None,
    }
}

/// Adds the invented root as `sources.add` adds a root: its path in
/// `server.db`, then its name and its source in `library.db`. A root that
/// is there already, at the same path, is kept.
fn add_root(library: &LibraryStore, servers: &ServerStore, root: &Path) -> Result<(), InventError> {
    let path = root
        .to_str()
        .ok_or_else(|| InventError(format!("{} is not UTF-8", root.display())))?;
    servers.add_root(ROOT_NAME, path)?;
    match library.add_source(request("invent.source"), ROOT_NAME, |_, _| String::new()) {
        Ok(_) | Err(Error::SourceExists { .. }) => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// Fills `library` with an invented library as `plan` says, writing its
/// covers under `plan.root`, which `servers` records as the root named
/// [`ROOT_NAME`]. `progress` is told how many records are written as it
/// goes.
pub fn seed(
    library: &LibraryStore,
    servers: &ServerStore,
    plan: &Plan,
    mut progress: impl FnMut(usize, usize),
) -> Result<Invented, InventError> {
    std::fs::create_dir_all(&plan.root)?;
    let root = std::path::absolute(&plan.root)?;
    add_root(library, servers, &root)?;
    let (artists, albums) = plan_library(plan);
    let mut invented = Invented {
        artists: albums
            .iter()
            .filter_map(|album| album.artist)
            .collect::<std::collections::HashSet<_>>()
            .len(),
        ..Invented::default()
    };
    let artists = Arc::new(artists);
    let rows: Arc<Mutex<Option<Rows>>> = Arc::new(Mutex::new(None));
    let (seed, now_ms) = (plan.seed, plan.now_ms);
    let batches: Vec<Vec<(usize, Album, String)>> = albums
        .chunks(BATCH)
        .enumerate()
        .map(|(batch_index, batch)| {
            batch
                .iter()
                .enumerate()
                .map(|(offset, album)| {
                    let index = batch_index * BATCH + offset;
                    (index, album.clone(), album_dir(&artists, album, index))
                })
                .collect()
        })
        .collect();
    // The next batch's covers are painted while this batch is written.
    std::thread::scope(|scope| -> Result<(), InventError> {
        let paint_on = |batch: &Vec<(usize, Album, String)>| {
            let (root, batch) = (root.clone(), batch.clone());
            scope.spawn(move || paint(&root, &batch))
        };
        let mut next = batches.first().map(paint_on);
        for (batch_index, batch) in batches.iter().enumerate() {
            let painted = match next.take() {
                Some(painting) => painting
                    .join()
                    .map_err(|_| InventError("painting covers failed".to_owned()))??,
                None => Vec::new(),
            };
            next = batches.get(batch_index + 1).map(paint_on);
            let batch = batch.clone();
            for painted in painted.iter().flatten() {
                invented.covers += 1;
                invented.cover_bytes += painted.bytes;
            }
            invented.groups += batch.len();
            invented.releases += batch
                .iter()
                .map(|(_, album, _)| album.versions.len())
                .sum::<usize>();
            let (artists, rows) = (Arc::clone(&artists), Arc::clone(&rows));
            let written = library.write(request("invent.seed"), move |write| {
                let mut held = rows.lock().unwrap_or_else(PoisonError::into_inner);
                if held.is_none() {
                    *held = Some(Rows::start(write, seed, now_ms)?);
                }
                let Some(rows) = held.as_mut() else {
                    return Ok("0".to_owned());
                };
                let mut statements = Statements::new(write.connection())?;
                let mut tracks = 0;
                for ((_, album, dir), painted) in batch.iter().zip(&painted) {
                    tracks += rows.album(
                        write,
                        &mut statements,
                        &artists,
                        album,
                        dir,
                        painted.as_ref(),
                    )?;
                }
                Ok(tracks.to_string())
            })?;
            invented.tracks += written.result_json.parse::<usize>().unwrap_or_default();
            progress(invented.groups, plan.albums);
        }
        Ok(())
    })?;
    Ok(invented)
}

/// Writes to a library a step at a time, as a scan would while someone
/// reads it: records added, titles and years changed, covers replaced.
#[derive(Debug)]
pub struct Churn {
    rng: Rng,
    root: PathBuf,
    rows: Arc<Mutex<Option<Rows>>>,
    artists: Arc<Vec<Artist>>,
    /// The releases there are, by key, and their appearances' keys.
    releases: Vec<(i64, Vec<i64>)>,
    made: usize,
    now_ms: i64,
}

/// What one step of [`Churn`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Churned {
    /// Records were added.
    Added(usize),
    /// Releases had their title and year changed.
    Changed(usize),
    /// A release was given a new cover.
    Recovered,
    /// A release was merged into another, with its tracks: it has gone.
    Merged,
}

/// Which kind of step to take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChurnKind {
    /// Add records.
    Add,
    /// Change titles and years.
    Change,
    /// Give a release a new cover.
    Recover,
    /// Merge a release into another.
    Merge,
}

impl Churn {
    /// Gets ready to write to `library`, whose invented root `servers`
    /// knows. It reads which releases there are, once.
    pub fn start(
        library: &LibraryStore,
        servers: &ServerStore,
        seed: u64,
        now_ms: i64,
    ) -> Result<Self, InventError> {
        let root = servers
            .roots()?
            .into_iter()
            .find_map(|(name, path)| (name == ROOT_NAME).then(|| PathBuf::from(path)))
            .ok_or_else(|| {
                InventError("there is no invented root: run `invent` first".to_owned())
            })?;
        let releases = library.read(|connection: &Connection| {
            let mut statement = connection.prepare(
                "SELECT release_id, id FROM track_appearance
                 WHERE release_id IS NOT NULL ORDER BY release_id, id",
            )?;
            let mut releases: Vec<(i64, Vec<i64>)> = Vec::new();
            let mut rows = statement.query([])?;
            while let Some(row) = rows.next()? {
                let (release, appearance): (i64, i64) = (row.get(0)?, row.get(1)?);
                match releases.last_mut() {
                    Some((last, appearances)) if *last == release => appearances.push(appearance),
                    _ => releases.push((release, vec![appearance])),
                }
            }
            Ok(releases)
        })?;
        let mut rng = Rng::new(seed);
        let artists: Vec<Artist> = (0..200)
            .map(|_| {
                let (name, sort_name) = artist_name(&mut rng);
                Artist { name, sort_name }
            })
            .collect();
        Ok(Self {
            rng,
            root,
            rows: Arc::new(Mutex::new(None)),
            artists: Arc::new(artists),
            releases,
            made: 0,
            now_ms,
        })
    }

    /// One write.
    pub fn step(&mut self, library: &LibraryStore) -> Result<Churned, InventError> {
        let roll = self.rng.float();
        let kind = if roll < 0.55 {
            ChurnKind::Add
        } else if roll < 0.85 {
            ChurnKind::Change
        } else if roll < 0.95 {
            ChurnKind::Recover
        } else {
            ChurnKind::Merge
        };
        self.take(library, kind)
    }

    /// One write of this kind.
    pub fn take(
        &mut self,
        library: &LibraryStore,
        kind: ChurnKind,
    ) -> Result<Churned, InventError> {
        let (seed, now_ms) = (self.rng.next(), self.now_ms);
        let rows = Arc::clone(&self.rows);
        if kind == ChurnKind::Merge && self.releases.len() > 1 {
            return self.merge(library);
        }
        if kind == ChurnKind::Add || self.releases.is_empty() {
            let count = usize::try_from(self.rng.between(1, 4)).unwrap_or(1);
            let plan = Plan {
                albums: count,
                seed,
                root: self.root.clone(),
                now_ms,
                cover_sides: (600, 1400),
            };
            let mut rng = Rng::new(seed);
            let batch: Vec<(usize, Album, String)> = (0..count)
                .map(|_| {
                    let artist = usize::try_from(rng.below(self.artists.len() as u64)).ok();
                    let mut album = plan_album(&mut rng, artist, 1990, &plan, 365 * 86_400_000);
                    album.added_ms = now_ms;
                    let index = 1_000_000_000 + self.made;
                    self.made += 1;
                    let dir = format!("churn/{}", album_dir(&self.artists, &album, index));
                    (index, album, dir)
                })
                .collect();
            let painted = paint(&self.root, &batch)?;
            let artists = Arc::clone(&self.artists);
            library.write(request("invent.churn"), move |write| {
                let mut held = rows.lock().unwrap_or_else(PoisonError::into_inner);
                if held.is_none() {
                    *held = Some(Rows::start(write, seed, now_ms)?);
                }
                if let Some(rows) = held.as_mut() {
                    let mut statements = Statements::new(write.connection())?;
                    for ((_, album, dir), painted) in batch.iter().zip(&painted) {
                        rows.album(
                            write,
                            &mut statements,
                            &artists,
                            album,
                            dir,
                            painted.as_ref(),
                        )?;
                    }
                }
                Ok(String::new())
            })?;
            return Ok(Churned::Added(count));
        }
        if kind != ChurnKind::Recover {
            let count = usize::try_from(self.rng.between(1, 5)).unwrap_or(1);
            let mut changes = Vec::new();
            for _ in 0..count {
                let index =
                    usize::try_from(self.rng.below(self.releases.len() as u64)).unwrap_or_default();
                if let Some((release, appearances)) = self.releases.get(index) {
                    let title = title(&mut self.rng);
                    let year = 1960 + i64::try_from(self.rng.below(66)).unwrap_or_default();
                    changes.push((*release, appearances.clone(), title, year));
                }
            }
            let changed = changes.len();
            library.write(request("invent.churn"), move |write| {
                let connection = write.connection();
                for (release, appearances, title, year) in &changes {
                    connection.execute(
                        "UPDATE release SET display_title = ?1, display_year = ?2, rev = ?3 WHERE id = ?4",
                        (title, year, write.rev(), release),
                    )?;
                    connection.execute(
                        "UPDATE release_group SET display_first_year = ?1, rev = ?2
                         WHERE id = (SELECT release_group_id FROM release WHERE id = ?3)",
                        (year, write.rev(), release),
                    )?;
                    for appearance in appearances {
                        connection.execute(
                            "UPDATE search_text SET album = ?1 WHERE rowid = ?2",
                            (title, appearance),
                        )?;
                    }
                }
                Ok(String::new())
            })?;
            return Ok(Churned::Changed(changed));
        }
        // A new cover for one release: a new picture row, so a new art ID.
        let index = usize::try_from(self.rng.below(self.releases.len() as u64)).unwrap_or_default();
        let Some((release, _)) = self.releases.get(index).cloned() else {
            return Ok(Churned::Changed(0));
        };
        let side = u32::try_from(self.rng.between(600, 1400)).unwrap_or(800);
        let jpeg = art::cover(seed, side).map_err(InventError)?;
        let dir = format!("churn/covers/{release}-{}", self.made);
        self.made += 1;
        std::fs::create_dir_all(self.root.join(&dir))?;
        std::fs::write(self.root.join(&dir).join("cover.jpg"), &jpeg)?;
        let painted = Painted {
            hash: *blake3::hash(&jpeg).as_bytes(),
            side,
            bytes: jpeg.len() as u64,
        };
        let path = format!("{dir}/1-01 Cover.flac");
        library.write(request("invent.churn"), move |write| {
            let mut held = rows.lock().unwrap_or_else(PoisonError::into_inner);
            if held.is_none() {
                *held = Some(Rows::start(write, seed, now_ms)?);
            }
            let Some(rows) = held.as_mut() else {
                return Ok(String::new());
            };
            let picture = rows.picture(write, &mut Statements::new(write.connection())?, &painted)?;
            // The picture needs a file beside it to be read from: a source
            // item that holds it, as a scan would have found.
            let item = rows.key("source_item");
            let revision = rows.key("source_item_revision");
            let item_id = rows.public_id();
            let connection = write.connection();
            connection.execute(
                "INSERT INTO source_item (id, public_id, source_instance_id, item_kind, provider_key,
                                          current_revision_id, first_seen_at_ms, rev)
                 VALUES (?1, ?2, ?3, 'file', ?4, NULL, ?5, ?6)",
                (item, item_id, rows.instance, &path, now_ms, write.rev()),
            )?;
            connection.execute(
                "INSERT INTO source_item_revision (id, source_item_id, revision_no, payload_hash,
                                                   payload_json, observed_at_ms, rev)
                 VALUES (?1, ?2, 1, ?3, '{\"invented\":true}', ?4, ?5)",
                (revision, item, painted.hash, now_ms, write.rev()),
            )?;
            connection.execute(
                "UPDATE source_item SET current_revision_id = ?1 WHERE id = ?2",
                (revision, item),
            )?;
            connection.execute(
                "INSERT INTO revision_picture (revision_id, picture_id, position, source_kind, file_name, rev)
                 VALUES (?1, ?2, 0, 'folder_image', 'cover.jpg', ?3)",
                (revision, picture, write.rev()),
            )?;
            connection.execute(
                "UPDATE release SET display_picture_id = ?1, rev = ?2 WHERE id = ?3",
                (picture, write.rev(), release),
            )?;
            Ok(String::new())
        })?;
        Ok(Churned::Recovered)
    }

    /// Merges one release into another, as the owner merging two copies of
    /// a record would: it and its tracks are redirected, and its tracks
    /// leave the search index.
    fn merge(&mut self, library: &LibraryStore) -> Result<Churned, InventError> {
        let index = usize::try_from(self.rng.below(self.releases.len() as u64)).unwrap_or_default();
        let (release, appearances) = self.releases.swap_remove(index);
        let target =
            usize::try_from(self.rng.below(self.releases.len() as u64)).unwrap_or_default();
        let Some((into, into_appearances)) = self.releases.get(target).cloned() else {
            return Ok(Churned::Merged);
        };
        let Some(into_appearance) = into_appearances.first().copied() else {
            return Ok(Churned::Merged);
        };
        library.write(request("invent.churn"), move |write| {
            let connection = write.connection();
            connection.execute(
                "UPDATE release SET redirect_to_id = ?1, canonical_id = ?1, rev = ?2
                 WHERE id = ?3 AND redirect_to_id IS NULL",
                (into, write.rev(), release),
            )?;
            for appearance in &appearances {
                connection.execute(
                    "UPDATE track_appearance SET redirect_to_id = ?1, canonical_id = ?1, rev = ?2
                     WHERE id = ?3 AND redirect_to_id IS NULL",
                    (into_appearance, write.rev(), appearance),
                )?;
                connection.execute("DELETE FROM search_text WHERE rowid = ?1", [appearance])?;
            }
            Ok(String::new())
        })?;
        Ok(Churned::Merged)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_seed_plans_the_same_library_and_another_seed_another() {
        let plan = Plan {
            albums: 300,
            seed: 7,
            root: PathBuf::from("/nowhere"),
            now_ms: 1_794_744_000_000,
            cover_sides: (600, 1500),
        };
        let (artists, albums) = plan_library(&plan);
        let (again_artists, again) = plan_library(&plan);
        assert_eq!(albums.len(), 300);
        // No two acts share a name, or they would be one artist.
        let names: std::collections::HashSet<_> = artists.iter().map(|a| &a.name).collect();
        assert_eq!(names.len(), artists.len());
        assert_eq!(
            artists.iter().map(|a| &a.name).collect::<Vec<_>>(),
            again_artists.iter().map(|a| &a.name).collect::<Vec<_>>()
        );
        assert_eq!(
            albums.iter().map(|a| &a.title).collect::<Vec<_>>(),
            again.iter().map(|a| &a.title).collect::<Vec<_>>()
        );
        let (_, other) = plan_library(&Plan {
            seed: 8,
            ..plan.clone()
        });
        assert_ne!(
            albums.iter().map(|a| &a.title).collect::<Vec<_>>(),
            other.iter().map(|a| &a.title).collect::<Vec<_>>()
        );
        // A realistic spread: most artists have one or two records, a few
        // have many; some records have versions and some have no cover.
        let mut per_artist: HashMap<usize, usize> = HashMap::new();
        for album in &albums {
            if let Some(artist) = album.artist {
                *per_artist.entry(artist).or_default() += 1;
            }
        }
        let few = per_artist.values().filter(|count| **count <= 2).count();
        assert!(few * 2 > per_artist.len(), "{per_artist:?}");
        assert!(per_artist.values().any(|count| *count >= 7));
        assert!(albums.iter().any(|album| album.versions.len() > 1));
        assert!(albums.iter().any(|album| album.cover.is_none()));
        let tracks: usize = albums.iter().map(|album| album.recordings.len()).sum();
        assert!((3_000..5_500).contains(&tracks), "{tracks}");
    }

    #[test]
    fn a_cover_is_a_jpeg_of_the_size_asked_for_and_the_same_for_the_same_seed() {
        let jpeg = art::cover(42, 640).unwrap();
        assert_eq!(jpeg.get(..3), Some(&[0xff, 0xd8, 0xff][..]));
        assert_eq!(jpeg, art::cover(42, 640).unwrap());
        assert_ne!(jpeg, art::cover(43, 640).unwrap());
        let decoded = image::load_from_memory(&jpeg).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (640, 640));
    }
}
