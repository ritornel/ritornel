//! `library.search`: the few artists, records and songs whose names match
//! what was typed, ranked as a person would want them under the search
//! field. See `ritornel_contract::SearchResults` for the rule.
//!
//! Every kind is matched by its own name only: an artist's records are one
//! click from the artist, and a record that matched by its artist would
//! crowd out one that matched by its title.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use ritornel_contract::{Id, ListKind, Row, SearchResults, SongFound};
use rusqlite::Connection;

use super::{CHUNK, fold, for_each_in, open, placeholders, rows};
use crate::{Error, LibraryStore};

/// What was typed, read for matching.
struct Typed {
    /// Its words, folded, one space apart: "ac dc".
    spaced: String,
    /// The same after a space, to find it at the start of a later word.
    later: String,
    /// The same with no spaces, so that "acdc" finds "AC/DC".
    squashed: String,
    /// Folded as it is, for a name with no letter or digit, such as "!!!".
    bare: String,
}

/// How well a name matches: lower is better.
type Tier = u8;
const IS: Tier = 0;
const STARTS: Tier = 1;
const WORD: Tier = 2;

impl Typed {
    fn of(text: &str) -> Self {
        let spaced = fold::spaced(text);
        Self {
            later: format!(" {spaced}"),
            squashed: spaced.replace(' ', ""),
            spaced,
            bare: fold::needle(text),
        }
    }

    fn is_empty(&self) -> bool {
        self.bare.is_empty()
    }

    /// The longest word typed, which the text index can look up when it is
    /// three characters or more.
    fn longest_word(&self) -> Option<&str> {
        self.spaced
            .split(' ')
            .max_by_key(|word| word.chars().count())
            .filter(|word| word.chars().count() >= 3)
    }

    /// How well `name` matches: it is what was typed, it starts with it, or
    /// it has a word that starts with it. `None` when it does none of these.
    fn tier(&self, name: &str) -> Option<Tier> {
        let whole = |rest: &str| if rest.is_empty() { IS } else { STARTS };
        if name.is_ascii() && !self.spaced.is_empty() {
            return self.ascii_tier(name.as_bytes());
        }
        if self.spaced.is_empty() {
            return fold::needle(name)
                .strip_prefix(self.bare.as_str())
                .map(whole);
        }
        let spaced = fold::spaced(name);
        if let Some(rest) = spaced.strip_prefix(self.spaced.as_str()) {
            return Some(whole(rest));
        }
        // Squashed only when the first letters agree: this runs over every
        // title in the library.
        if spaced.as_bytes().first() == self.squashed.as_bytes().first() {
            let squashed = spaced.replace(' ', "");
            if let Some(rest) = squashed.strip_prefix(self.squashed.as_str()) {
                return Some(whole(rest));
            }
        }
        spaced.contains(self.later.as_str()).then_some(WORD)
    }

    /// [`Typed::tier`] for a name in ASCII, which most are, without making
    /// a string: it runs over every title in the library.
    fn ascii_tier(&self, name: &[u8]) -> Option<Tier> {
        let words = || {
            name.split(|byte| !byte.is_ascii_alphanumeric())
                .filter(|word| !word.is_empty())
        };
        let whole = |rest: bool| if rest { STARTS } else { IS };
        let spaced = self.spaced.as_bytes();
        if let Some(rest) = after(spaced_bytes(words()), spaced) {
            return Some(whole(rest));
        }
        let squashed = words().flat_map(|word| word.iter().map(u8::to_ascii_lowercase));
        if let Some(rest) = after(squashed, self.squashed.as_bytes()) {
            return Some(whole(rest));
        }
        (1..words().count())
            .any(|skip| after(spaced_bytes(words().skip(skip)), spaced).is_some())
            .then_some(WORD)
    }
}

/// Words lower-cased, one space between.
fn spaced_bytes<'a>(words: impl Iterator<Item = &'a [u8]>) -> impl Iterator<Item = u8> {
    words.enumerate().flat_map(|(at, word)| {
        (at > 0)
            .then_some(b' ')
            .into_iter()
            .chain(word.iter().map(u8::to_ascii_lowercase))
    })
}

/// Whether `bytes` start with `prefix`: `None` if not, or whether more
/// follows.
fn after(mut bytes: impl Iterator<Item = u8>, prefix: &[u8]) -> Option<bool> {
    for expected in prefix {
        if bytes.next()? != *expected {
            return None;
        }
    }
    Some(bytes.next().is_some())
}

/// One match, before it is ranked.
struct Found {
    key: i64,
    tier: Tier,
    /// What it is filed under, for the order within a tier.
    filed: String,
    plays: u64,
}

/// The matches in the tiers that fill `limit`: the best tier, and the next
/// while there are fewer than `limit`. Only these are worth counting plays
/// for.
fn needed(mut found: Vec<Found>, limit: usize) -> Vec<Found> {
    found.sort_by_key(|each| each.tier);
    if let Some(last) = found.get(limit.saturating_sub(1)) {
        let tier = last.tier;
        let cut = found.partition_point(|each| each.tier <= tier);
        found.truncate(cut);
    }
    found
}

/// The keys of a kind's best matches, best first, and how well the best
/// of them matched.
struct Ranked {
    keys: Vec<i64>,
    best: Option<Tier>,
}

/// Best first: by tier, the most played, then as the walls file them.
fn rank(mut found: Vec<Found>, limit: usize) -> Ranked {
    found.sort_by(|a, b| {
        a.tier
            .cmp(&b.tier)
            .then(b.plays.cmp(&a.plays))
            .then_with(|| by_name(&a.filed, &b.filed))
            .then(a.key.cmp(&b.key))
    });
    Ranked {
        best: found.first().map(|each| each.tier),
        keys: found.into_iter().take(limit).map(|each| each.key).collect(),
    }
}

fn by_name(a: &str, b: &str) -> Ordering {
    fold::rank(a).cmp(&fold::rank(b)).then_with(|| a.cmp(b))
}

/// The listener's plays that count, by recording.
fn plays_of(connection: &Connection, recordings: &[i64]) -> Result<HashMap<i64, u64>, Error> {
    let mut plays = HashMap::new();
    let listener: Option<i64> =
        connection.query_row("SELECT min(id) FROM listener", [], |row| row.get(0))?;
    let Some(listener) = listener else {
        return Ok(plays);
    };
    let mut each = |row: &rusqlite::Row<'_>| -> Result<(), rusqlite::Error> {
        let count: i64 = row.get(1)?;
        plays.insert(row.get(0)?, u64::try_from(count).unwrap_or(0));
        Ok(())
    };
    let counted = "SELECT recording_id, count(*) FROM play_event
                   WHERE listener_id = ?1 AND counts = 1 AND excluded = 0
                     AND recording_id IS NOT NULL";
    if recordings.len() > 4 * CHUNK {
        // Most of the history: one pass over its index.
        let mut statement = connection.prepare(&format!("{counted} GROUP BY recording_id"))?;
        let mut rows = statement.query([listener])?;
        while let Some(row) = rows.next()? {
            each(row)?;
        }
        return Ok(plays);
    }
    for chunk in recordings.chunks(CHUNK) {
        let sql = format!(
            "{counted} AND recording_id IN ({}) GROUP BY recording_id",
            placeholders(chunk.len())
        );
        let mut statement = connection.prepare(&sql)?;
        let parameters = std::iter::once(listener).chain(chunk.iter().copied());
        let mut rows = statement.query(rusqlite::params_from_iter(parameters))?;
        while let Some(row) = rows.next()? {
            each(row)?;
        }
    }
    Ok(plays)
}

/// Adds to each match the plays of the recordings `pairs` gives it, as
/// (match key, recording).
fn add_plays(
    connection: &Connection,
    found: &mut [Found],
    pairs: &[(i64, i64)],
) -> Result<(), Error> {
    let mut recordings: Vec<i64> = pairs.iter().map(|(_, recording)| *recording).collect();
    recordings.sort_unstable();
    recordings.dedup();
    let plays = plays_of(connection, &recordings)?;
    let mut sums: HashMap<i64, u64> = HashMap::new();
    for (key, recording) in pairs {
        *sums.entry(*key).or_default() += plays.get(recording).copied().unwrap_or(0);
    }
    for each in found {
        each.plays = sums.get(&each.key).copied().unwrap_or(0);
    }
    Ok(())
}

fn artists(connection: &Connection, typed: &Typed, limit: usize) -> Result<Ranked, Error> {
    // Those the Artists list has: whom a record's credit names.
    let mut statement = connection.prepare(concat!(
        "SELECT a.id, a.display_name, a.display_sort_name
         FROM artist a
         WHERE a.redirect_to_id IS NULL
           AND a.id IN (
             SELECT ca.canonical_id
             FROM release r
             CROSS JOIN artist_credit_name acn ON acn.artist_credit_id = r.display_artist_credit_id
             CROSS JOIN artist ca ON ca.id = acn.artist_id
             WHERE r.redirect_to_id IS NULL AND ",
        reached_release!("r"),
        ")"
    ))?;
    let mut found = Vec::new();
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let name: Option<String> = row.get(1)?;
        let sort_name: Option<String> = row.get(2)?;
        let name = name.unwrap_or_default();
        let filed = fold::sort_text(&name, sort_name.as_deref());
        // "beatles" is The Beatles exactly, and "marchetti" starts the name
        // Ilse Marchetti is filed under.
        let tier = [
            typed.tier(&name),
            typed.tier(&fold::sort_text(&name, None)),
            typed.tier(&filed),
        ]
        .into_iter()
        .flatten()
        .min();
        if let Some(tier) = tier {
            found.push(Found {
                key: row.get(0)?,
                tier,
                filed,
                plays: 0,
            });
        }
    }
    let mut found = needed(found, limit);
    let keys: Vec<i64> = found.iter().map(|each| each.key).collect();
    let mut pairs = Vec::new();
    for_each_in(
        connection,
        // In this order, by `release_display_artist_credit` and
        // `track_appearance_release`: left to itself SQLite reads every song.
        concat!(
            "SELECT a.canonical_id, ta.recording_id
             FROM artist_credit_name acn
             CROSS JOIN artist a ON a.id = acn.artist_id
             CROSS JOIN release r ON r.display_artist_credit_id = acn.artist_credit_id
             CROSS JOIN track_appearance ta ON ta.release_id = r.id
             WHERE a.canonical_id IN ({})
               AND r.redirect_to_id IS NULL AND ta.redirect_to_id IS NULL AND ",
            reached!("ta")
        ),
        &keys,
        |row| {
            pairs.push((row.get(0)?, row.get(1)?));
            Ok(())
        },
    )?;
    add_plays(connection, &mut found, &pairs)?;
    Ok(rank(found, limit))
}

fn albums(connection: &Connection, typed: &Typed, limit: usize) -> Result<Ranked, Error> {
    let releases = open::releases(connection, None)?;
    let mut found = Vec::new();
    // The release each tile shows, by the tile's key.
    let mut shown: HashMap<i64, i64> = HashMap::new();
    for tile in open::tiles(&releases) {
        let Some(release) = releases.get(tile.shown) else {
            continue;
        };
        if let Some(tier) = typed.tier(&release.title) {
            shown.insert(tile.key, release.id);
            found.push(Found {
                key: tile.key,
                tier,
                filed: release.title_key.clone(),
                plays: 0,
            });
        }
    }
    let mut found = needed(found, limit);
    let by_release: HashMap<i64, i64> = found
        .iter()
        .filter_map(|each| Some((*shown.get(&each.key)?, each.key)))
        .collect();
    let release_keys: Vec<i64> = by_release.keys().copied().collect();
    let mut pairs = Vec::new();
    for_each_in(
        connection,
        concat!(
            "SELECT release_id, recording_id FROM track_appearance
             WHERE release_id IN ({}) AND redirect_to_id IS NULL AND ",
            reached!("track_appearance")
        ),
        &release_keys,
        |row| {
            let release: i64 = row.get(0)?;
            if let Some(key) = by_release.get(&release) {
                pairs.push((*key, row.get(1)?));
            }
            Ok(())
        },
    )?;
    add_plays(connection, &mut found, &pairs)?;
    Ok(rank(found, limit))
}

/// A song whose title matched: its key, how well, its title, and its
/// recording and whether it is loose, when they were read with it.
type Titled = (i64, Tier, String, Option<(i64, bool)>);

/// A song kept for its recording.
struct Kept {
    tier: Tier,
    loose: bool,
    key: i64,
    title: String,
}

/// A column's text where it is read, without copying it. Empty for a null.
fn text(value: rusqlite::types::ValueRef<'_>) -> &str {
    match value {
        rusqlite::types::ValueRef::Text(bytes) => std::str::from_utf8(bytes).unwrap_or_default(),
        _ => "",
    }
}

fn songs(connection: &Connection, typed: &Typed, limit: usize) -> Result<Ranked, Error> {
    // The titles that match, through the text index when a word typed is
    // long enough for it to look up, and otherwise from every live song,
    // with its recording and whether it is loose, on no record.
    let mut titled: Vec<Titled> = Vec::new();
    match typed.longest_word() {
        Some(word) => {
            let phrase = format!("{{title}} : \"{}\"", word.replace('"', "\"\""));
            let mut statement = connection.prepare(
                "SELECT ta.id, ta.display_title FROM search_text
                          JOIN track_appearance ta ON ta.id = search_text.rowid
                          WHERE search_text MATCH ?1",
            )?;
            let mut rows = statement.query([phrase])?;
            while let Some(row) = rows.next()? {
                let title = text(row.get_ref(1)?);
                if let Some(tier) = typed.tier(title) {
                    titled.push((row.get(0)?, tier, title.to_owned(), None));
                }
            }
        }
        None => {
            let mut statement = connection.prepare(concat!(
                "SELECT id, display_title, recording_id, release_id IS NULL
                 FROM track_appearance WHERE redirect_to_id IS NULL AND ",
                reached!("track_appearance")
            ))?;
            let mut rows = statement.query([])?;
            while let Some(row) = rows.next()? {
                let title = text(row.get_ref(1)?);
                if let Some(tier) = typed.tier(title) {
                    let live = (row.get(2)?, row.get(3)?);
                    titled.push((row.get(0)?, tier, title.to_owned(), Some(live)));
                }
            }
        }
    }
    titled.sort_by_key(|(_, tier, _, _)| *tier);

    // A tier at a time, until there are enough: the live appearances, by
    // their recording. A song on several records is listed once: on a
    // record rather than loose, and then the first that came into the
    // library.
    let mut by_recording: HashMap<i64, Kept> = HashMap::new();
    let mut rest = titled.as_slice();
    while let Some((_, tier, _, _)) = rest.first() {
        let end = rest.partition_point(|(_, each, _, _)| each == tier);
        let (these, after) = rest.split_at(end);
        rest = after;
        let unknown: Vec<i64> = these
            .iter()
            .filter(|(_, _, _, live)| live.is_none())
            .map(|(key, _, _, _)| *key)
            .collect();
        let looked_up = live_songs(connection, &unknown)?;
        for (key, tier, title, live) in these {
            let Some((recording, loose)) = live.or_else(|| looked_up.get(key).copied()) else {
                continue;
            };
            let better = by_recording
                .get(&recording)
                .is_none_or(|kept| (*tier, loose, *key) < (kept.tier, kept.loose, kept.key));
            if better {
                by_recording.insert(
                    recording,
                    Kept {
                        tier: *tier,
                        loose,
                        key: *key,
                        title: title.clone(),
                    },
                );
            }
        }
        if by_recording.len() >= limit {
            break;
        }
    }
    let mut found: Vec<Found> = by_recording
        .values()
        .map(|kept| Found {
            key: kept.key,
            tier: kept.tier,
            filed: fold::sort_text(&kept.title, None),
            plays: 0,
        })
        .collect();
    let pairs: Vec<(i64, i64)> = by_recording
        .iter()
        .map(|(recording, kept)| (kept.key, *recording))
        .collect();
    add_plays(connection, &mut found, &pairs)?;
    Ok(rank(found, limit))
}

/// The live appearances among these, with their recording and whether
/// each is loose, on no record.
fn live_songs(connection: &Connection, keys: &[i64]) -> Result<HashMap<i64, (i64, bool)>, Error> {
    let mut live: HashMap<i64, (i64, bool)> = HashMap::with_capacity(keys.len());
    let mut each = |row: &rusqlite::Row<'_>| -> Result<(), rusqlite::Error> {
        live.insert(row.get(0)?, (row.get(1)?, row.get(2)?));
        Ok(())
    };
    let columns = concat!(
        "SELECT id, recording_id, release_id IS NULL FROM track_appearance
         WHERE redirect_to_id IS NULL AND ",
        reached!("track_appearance")
    );
    // A long list is most of the table, which one pass reads faster.
    if keys.len() > 4 * CHUNK {
        let wanted: HashSet<i64> = keys.iter().copied().collect();
        let mut statement = connection.prepare(columns)?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            if wanted.contains(&row.get(0)?) {
                each(row)?;
            }
        }
    } else {
        for_each_in(
            connection,
            &format!("{columns} AND id IN ({{}})"),
            keys,
            &mut each,
        )?;
    }
    Ok(live)
}

impl LibraryStore {
    /// The best `limit` artists, records and songs of each kind whose names
    /// match `text`, read in one transaction.
    pub fn search(&self, text: &str, limit: usize) -> Result<SearchResults, Error> {
        let typed = Typed::of(text);
        self.read(move |connection| {
            connection.execute_batch("BEGIN")?;
            let found = search(connection, &typed, limit);
            let _ = connection.execute_batch("COMMIT");
            found
        })
    }
}

fn search(connection: &Connection, typed: &Typed, limit: usize) -> Result<SearchResults, Error> {
    let revision: i64 =
        connection.query_row("SELECT rev FROM library_meta WHERE id = 1", [], |row| {
            row.get(0)
        })?;
    let mut results = SearchResults {
        artists: Vec::new(),
        albums: Vec::new(),
        songs: Vec::new(),
        top: None,
        library_revision: u64::try_from(revision).unwrap_or(0),
    };
    if typed.is_empty() || limit == 0 {
        return Ok(results);
    }
    let artists = artists(connection, typed, limit)?;
    let albums = albums(connection, typed, limit)?;
    let songs = songs(connection, typed, limit)?;
    // The kind with the best match, artists first among equals.
    results.top = [
        (artists.best, ListKind::Artists),
        (albums.best, ListKind::Albums),
        (songs.best, ListKind::Songs),
    ]
    .into_iter()
    .filter_map(|(best, kind)| Some((best?, kind)))
    .min_by_key(|(best, _)| *best)
    .map(|(_, kind)| kind);
    for row in rows::read(connection, ListKind::Artists, &artists.keys)? {
        if let Row::Artist(artist) = row {
            results.artists.push(artist);
        }
    }
    for row in rows::read(connection, ListKind::Albums, &albums.keys)? {
        if let Row::Album(album) = row {
            results.albums.push(album);
        }
    }
    let mut covers: HashMap<i64, Id> = HashMap::new();
    for_each_in(
        connection,
        "SELECT ta.id, p.public_id FROM track_appearance ta
         JOIN release r ON r.id = ta.release_id
         JOIN picture p ON p.id = r.display_picture_id
         WHERE ta.id IN ({})",
        &songs.keys,
        |row| {
            covers.insert(row.get(0)?, Id::from_bytes(row.get(1)?));
            Ok(())
        },
    )?;
    for (key, row) in songs
        .keys
        .iter()
        .zip(rows::read(connection, ListKind::Songs, &songs.keys)?)
    {
        if let Row::Song(song) = row {
            results.songs.push(SongFound {
                song,
                art_id: covers.get(key).copied(),
            });
        }
    }
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_is_what_was_typed_starts_with_it_or_has_a_word_that_does() {
        let typed = Typed::of("Pine");
        assert_eq!(typed.tier("Pine"), Some(IS));
        assert_eq!(typed.tier("Pinegrove"), Some(STARTS));
        assert_eq!(typed.tier("The Pines"), Some(WORD));
        assert_eq!(typed.tier("Happiness"), None);
        // Case, accents and the punctuation between words do not count.
        assert_eq!(Typed::of("boa").tier("Bôa"), Some(IS));
        assert_eq!(
            Typed::of("soren dun").tier("Søren Dunmore Ensemble"),
            Some(STARTS)
        );
        assert_eq!(Typed::of("acdc").tier("AC/DC"), Some(IS));
        assert_eq!(Typed::of("ac/dc").tier("AC/DC"), Some(IS));
        assert_eq!(
            Typed::of("dark side").tier("The Dark Side of the Moon"),
            Some(WORD)
        );
        // A name of no letter or digit is found as it is written.
        assert_eq!(Typed::of("!!!").tier("!!!"), Some(IS));
        assert!(Typed::of("  ").is_empty());
    }

    #[test]
    fn only_the_tiers_that_fill_the_limit_are_kept() {
        let found = |tiers: &[Tier]| -> Vec<Found> {
            tiers
                .iter()
                .enumerate()
                .map(|(key, tier)| Found {
                    key: i64::try_from(key).unwrap(),
                    tier: *tier,
                    filed: String::new(),
                    plays: 0,
                })
                .collect()
        };
        let tiers = |kept: Vec<Found>| kept.iter().map(|each| each.tier).collect::<Vec<_>>();
        assert_eq!(tiers(needed(found(&[2, 1, 2, 0]), 2)), [0, 1]);
        assert_eq!(tiers(needed(found(&[2, 1, 2, 0]), 3)), [0, 1, 2, 2]);
        assert_eq!(tiers(needed(found(&[1, 1, 1]), 2)), [1, 1, 1]);
        assert_eq!(tiers(needed(found(&[2]), 5)), [2]);
    }
}
