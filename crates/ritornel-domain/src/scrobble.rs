//! Matching a scrobble to the library: which recording, if any, a play that
//! another service reported by its text alone was of. See "Scrobbles" in
//! docs/design/identity.md.
//!
//! A scrobble names an artist, a title and maybe an album, as some other
//! player sent them, and nothing else that can be trusted. So it binds only
//! on a confident match, and otherwise stays unbound and is listed. This is
//! matcher 2, safest first:
//!
//! - **Bind** when the normalised artist and title name recordings, and
//!   either they name one and the album fits one of its appearances or is
//!   absent, or they name several and the album fits exactly one of them.
//!   An album fits an appearance's when they are the same once a bracketed
//!   note, spacing, punctuation, a leading "The" and accents are passed
//!   over.
//! - Then the same, with artist and title compared loosely: spacing,
//!   punctuation, a leading "The", accents and a featured artist passed
//!   over ([`Rule::Loose`]).
//! - **Doubtful** when they name one recording whose appearances the album
//!   fits none of, which may be another version of the song; when they name
//!   several the album cannot tell apart; or when nothing matches but
//!   something does once accents and a bracketed note such as
//!   `(Remastered 2011)` are left out. Each is listed for the owner, with
//!   what it nearly matched.
//! - **Absent** when nothing comes close: music played and not owned.
//!
//! A scrobble never creates an artist, a record or a song. The matcher is
//! pure; the engine reads the library into a [`Catalog`], judges every
//! scrobbled track again at each sync, and binds what it says to bind.

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::discs::disc_in_title;
use crate::normalize::{core, fold_accents, normalize};
use crate::resolve::{album_markers_of, markers_of, part_number};

/// The matcher's version, recorded on each binding it makes.
///
/// 2: the album holds a binding back, artist and title also match
/// loosely, and every track is judged again.
pub const MATCHER_VERSION: i64 = 2;

/// One appearance of a recording: on which record, and credited to whom.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Appearance {
    /// The appearance's row.
    pub appearance: i64,
    /// Its own recording's row, which a binding to it names.
    pub recording: i64,
    /// The title it shows.
    pub title: Option<String>,
    /// The artist credit it shows.
    pub artist: Option<String>,
    /// The title of its record.
    pub album: Option<String>,
}

/// One recording the library has files for, with what it shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recording {
    /// The recording's row: a canonical one.
    pub recording: i64,
    /// The title it shows.
    pub title: Option<String>,
    /// The artist credit it shows.
    pub artist: Option<String>,
    /// Its appearances.
    pub appearances: Vec<Appearance>,
}

/// What a scrobble is of, as its source reported it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scrobbled<'a> {
    /// The artist.
    pub artist: &'a str,
    /// The album, when there is one.
    pub album: Option<&'a str>,
    /// The title.
    pub title: &'a str,
}

/// Why a scrobble was not bound although something was near.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Doubt {
    /// The artist and title name several recordings, and the album does
    /// not tell them apart.
    Several,
    /// The artist and title name one recording, and the album fits none of
    /// its appearances: it may be another version of the song.
    Album,
    /// Nothing matches exactly, and something does once accents and
    /// bracketed notes are left out.
    Near,
}

impl Doubt {
    /// The doubt as a word.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Several => "several",
            Self::Album => "album",
            Self::Near => "near",
        }
    }
}

/// How the artist and title of a binding matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rule {
    /// By their normalised keys.
    Exact,
    /// With spacing, punctuation, a leading "The", accents and a featured
    /// artist passed over.
    Loose,
}

impl Rule {
    /// The rule as a word.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Loose => "loose",
        }
    }
}

/// What the matcher makes of a scrobble.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// It is this recording, and this appearance where one is known.
    Bind {
        /// The recording's row: the appearance's own recording when there
        /// is an appearance, else the canonical one.
        recording: i64,
        /// The appearance, when the album or the recording having only one
        /// says which.
        appearance: Option<i64>,
        /// How many recordings the artist and title named.
        candidates: usize,
        /// How the artist and title matched.
        rule: Rule,
    },
    /// It is not bound, and these canonical recordings were near.
    Doubtful {
        /// Why.
        doubt: Doubt,
        /// What it nearly matched.
        candidates: Vec<i64>,
    },
    /// Nothing in the library comes close.
    Absent,
}

type Index = HashMap<(String, String), BTreeSet<usize>>;

/// The library as the matcher reads it: every recording under the keys of
/// its artist and title.
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    recordings: Vec<Recording>,
    exact: Index,
    loose: Index,
    near: Index,
}

/// The exact key: normalised artist and title.
fn exact_key(artist: &str, title: &str) -> (String, String) {
    (normalize(artist), normalize(title))
}

/// The words that bring in a featured artist, with what is before them.
const FEATURED: &[&str] = &[
    " feat. ",
    " feat ",
    " ft. ",
    " featuring ",
    "(feat. ",
    "(feat ",
    "(ft. ",
    "(featuring ",
    "[feat. ",
    "[feat ",
    "[ft. ",
    "[featuring ",
];

/// The text without a featured artist: "(feat. Somebody)" taken out, and
/// " feat. Somebody" up to the next bracket, dash or the end.
pub(crate) fn without_featured(text: &str) -> String {
    let mut text = text.to_owned();
    loop {
        // ASCII lower case keeps every byte where it was.
        let lower = text.to_ascii_lowercase();
        // "Feat of" is a word, not a featured artist.
        let Some(at) = FEATURED
            .iter()
            .filter_map(|word| {
                lower.match_indices(word).map(|(at, _)| at).find(|at| {
                    !lower.get(at + word.len()..).is_some_and(|rest| {
                        word.trim().ends_with("feat") && rest.starts_with("of ")
                    })
                })
            })
            .min()
        else {
            return text;
        };
        let end = match lower.as_bytes().get(at) {
            Some(b'(') => lower
                .get(at..)
                .and_then(|rest| rest.find(')'))
                .map(|end| at + end + 1),
            Some(b'[') => lower
                .get(at..)
                .and_then(|rest| rest.find(']'))
                .map(|end| at + end + 1),
            // A bare one runs to the next bracket or dash.
            _ => lower.get(at + 1..).and_then(|rest| {
                [
                    rest.find(['(', '[']),
                    rest.find(" - "),
                    rest.find(" – "),
                    rest.find(" — "),
                ]
                .into_iter()
                .flatten()
                .min()
                .map(|end| at + 1 + end)
            }),
        }
        .unwrap_or(text.len());
        text.replace_range(at..end, " ");
    }
}

/// A key with accents folded, a leading "the" and every "and" passed over,
/// and no spaces: "&", "+" and "/" between two words are then all one.
fn squash(key: &str) -> String {
    let key = fold_accents(key);
    // A symbol a key keeps at either end, as "⇄" or "+", is passed over
    // first, unless the key is nothing else.
    let words: Vec<&str> = key
        .split(' ')
        .filter(|word| word.chars().any(char::is_alphanumeric))
        .collect();
    if words.is_empty() {
        return key.split(' ').collect();
    }
    let words = match words.as_slice() {
        ["the", rest @ ..] if !rest.is_empty() => rest,
        all => all,
    };
    let squashed: String = words
        .iter()
        .filter(|word| **word != "and")
        .copied()
        .collect();
    if squashed.is_empty() {
        words.concat()
    } else {
        squashed
    }
}

/// The loose key: spacing, punctuation, a leading "The", accents and a
/// featured artist passed over. The title's version markers, such as
/// "live", are kept beside it, so that taking a featured artist out never
/// takes a version with it.
fn loose_key(artist: &str, title: &str) -> (String, String) {
    let key = |text: &str| squash(&normalize(&without_featured(text)));
    let mut kept = markers_of(title, 2);
    // And its numbers, as in "Take 2", wherever they are: one in a featured
    // artist's name may keep a play apart, and never binds a wrong one.
    kept.extend(
        normalize(title)
            .split(' ')
            .filter(|word| word.chars().all(|c| c.is_ascii_digit()))
            .map(str::to_owned),
    );
    kept.sort();
    (key(artist), format!("{}|{}", key(title), kept.join(",")))
}

/// The near key: accents off, and the title without what is in brackets.
fn near_key(artist: &str, title: &str) -> (String, String) {
    (fold_accents(&normalize(artist)), fold_accents(&core(title)))
}

/// The words an album is compared by: its title without a disc marker, a
/// bracketed note, a trailing "EP" or a trailing ", The", with accents
/// folded and a leading "The" and every "and" passed over.
fn album_words(album: &str) -> Vec<String> {
    let album = disc_in_title(album).map_or_else(|| album.to_owned(), |(title, _)| title);
    let key = fold_accents(&core(&album));
    // "And" and a leading "The" are passed over only while a word is left
    // beside them: "The +" is not "+".
    let worded = |words: &[&str]| {
        words
            .iter()
            .any(|word| word.chars().any(char::is_alphanumeric))
    };
    let mut words: Vec<&str> = key.split(' ').filter(|word| *word != "and").collect();
    if !worded(&words) {
        words = key.split(' ').collect();
    }
    while words.len() > 1 && matches!(words.last(), Some(&("ep" | "the"))) {
        words.pop();
    }
    // A leading "The", after any symbols before it.
    let first = words
        .iter()
        .position(|word| word.chars().any(char::is_alphanumeric));
    if let Some(at) = first
        && words.get(at) == Some(&"the")
        && worded(words.get(at + 1..).unwrap_or_default())
    {
        words.remove(at);
    }
    words.into_iter().map(str::to_owned).collect()
}

/// The numbers an album title gives, notes and all: a volume or a part
/// ("Vol. 2", "Vol.2"), a roman numeral such as "II", or a number standing
/// on its own ("Low Tide (2)"), which tell those from "Low Tide". A year, a
/// range such as "01 - 09" and "10,000" are not.
fn album_numbers(album: &str) -> Vec<u32> {
    let album = disc_in_title(album).map_or_else(|| album.to_owned(), |(title, _)| title);
    // Each bracketed note is read on its own, so that a year in one does
    // not hide the number beside it: "Low Tide (2) (2011 Remaster)".
    let mut segments: Vec<String> = vec![String::new()];
    for c in album.chars() {
        match c {
            '(' | '[' | ')' | ']' => segments.push(String::new()),
            c => segments
                .last_mut()
                .into_iter()
                .for_each(|segment| segment.push(c)),
        }
    }
    let mut found: Vec<u32> = Vec::new();
    for segment in &segments {
        found.extend(segment_numbers(&normalize(segment)));
    }
    found.sort_unstable();
    found
}

/// The numbers of one part of an album title, as [`album_numbers`] reads
/// them.
fn segment_numbers(key: &str) -> Vec<u32> {
    let words: Vec<&str> = key.split(' ').collect();
    let digits = |word: &str| word.starts_with(|c: char| c.is_ascii_digit());
    let roman = |word: &str| !word.is_empty() && word.chars().all(|c| "ivx".contains(c));
    let mut found: Vec<u32> = Vec::new();
    for (at, word) in words.iter().enumerate() {
        let before = at.checked_sub(1).and_then(|before| words.get(before));
        let after = words.get(at + 1);
        let after_volume = before.is_some_and(|before| {
            matches!(
                *before,
                "vol" | "vols" | "volume" | "part" | "pt" | "book" | "chapter"
            )
        });
        // A volume and its number written as one word: "vol2", "volii".
        let joined = ["volume", "vol", "part", "pt"]
            .iter()
            .find_map(|prefix| word.strip_prefix(prefix))
            .filter(|rest| {
                !rest.is_empty() && (rest.chars().all(|c| c.is_ascii_digit()) || roman(rest))
            });
        let alone = word.chars().all(|c| c.is_ascii_digit())
            && !before.is_some_and(|w| digits(w))
            && !after.is_some_and(|w| digits(w));
        // A roman numeral of one letter is a word as often as not.
        let number = if let Some(rest) = joined {
            part_number(rest)
        } else if after_volume || word.len() > 1 && roman(word) {
            part_number(word)
        } else if alone {
            word.parse().ok().filter(|number| *number < 100)
        } else {
            None
        };
        found.extend(number);
    }
    found
}

/// A title's text outside its bracketed notes, as written.
fn core_text(title: &str) -> String {
    let mut outside = String::with_capacity(title.len());
    let mut depth = 0_u32;
    for c in title.chars() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = depth.saturating_sub(1),
            _ if depth == 0 => outside.push(c),
            _ => {}
        }
    }
    outside
}

/// What an album is compared by: [`album_words`] with no spaces.
fn album_key(album: &str) -> String {
    album_words(album).concat()
}

/// The album a scrobble names, as it is compared: its normalised title,
/// which picks an appearance first, its [`album_key`] and words, its
/// version markers and its numbers, which must agree, and the title as
/// written.
struct Album {
    exact: String,
    loose: String,
    words: Vec<String>,
    markers: Vec<String>,
    numbers: Vec<u32>,
    written: String,
    /// The artist's name, as an album's key: a subtitle that is the
    /// artist's name is set off as surely as one after a colon.
    artist: String,
}

/// Words that begin an edition's name after an album's title.
const EDITION: &[&str] = &[
    "deluxe",
    "expanded",
    "edition",
    "version",
    "remastered",
    "remaster",
    "anniversary",
    "special",
    "bonus",
    "reissue",
    "extra",
    "soundtrack",
    "ost",
];

impl Album {
    fn new(album: &str, artist: &str) -> Option<Self> {
        let loose = album_key(album);
        (!loose.is_empty()).then(|| Self {
            artist: album_key(artist),
            exact: normalize(album),
            loose,
            words: album_words(album),
            markers: album_markers_of(album),
            numbers: album_numbers(album),
            written: album.to_owned(),
        })
    }

    /// The albums a scrobble's album may be, in order: as written, then,
    /// for "S/T" or "Self-Titled", the album named after the artist, and,
    /// for "null", no album at all.
    fn read(album: Option<&str>, artist: &str) -> Vec<Option<Self>> {
        let Some(album) = album else {
            return vec![None];
        };
        let mut read: Vec<Option<Self>> = Self::new(album, artist).into_iter().map(Some).collect();
        match normalize(album).as_str() {
            "null" | "undefined" => read.push(None),
            "s t" | "st" | "self titled" | "selftitled" => {
                read.extend(Self::new(artist, artist).map(Some))
            }
            _ => {}
        }
        if read.is_empty() {
            read.push(None);
        }
        read
    }

    /// Whether an appearance's album is this one, loosely: the same, or the
    /// one is the other's first two words or more and a subtitle or an
    /// edition after them, set off by punctuation or begun by a word such
    /// as "Deluxe", as "Low Tide: Rarities" is "Low Tide". What follows is
    /// not a number or a volume: "Low Tide II" is another album. Their
    /// version markers, such as "live", and their numbers, in a note or
    /// not, agree.
    fn fits(&self, album: &str) -> bool {
        if album_markers_of(album) != self.markers || album_numbers(album) != self.numbers {
            return false;
        }
        if album_key(album) == self.loose {
            return true;
        }
        let words = album_words(album);
        let (short, long, written, other) = if words.len() < self.words.len() {
            (&words, &self.words, self.written.as_str(), album)
        } else {
            (&self.words, &words, album, self.written.as_str())
        };
        let Some(next) = long.get(short.len()) else {
            return false;
        };
        let numbered = part_number(next).is_some()
            || matches!(
                next.as_str(),
                "vol" | "volume" | "part" | "pt" | "chapter" | "book"
            );
        let rest = long.get(short.len()..).unwrap_or_default();
        // By its letters: a symbol inside a title is no word of it.
        let letters = |text: &str| -> String {
            text.chars()
                .filter(|c| {
                    c.is_alphanumeric() || unicode_normalization::char::is_combining_mark(*c)
                })
                .collect()
        };
        let rest = album_words(&rest.join(" ")).concat();
        // An artist of symbols alone is matched as written.
        let by_artist = if letters(&self.artist).is_empty() {
            rest == self.artist
        } else {
            letters(&rest) == letters(&self.artist)
        };
        // Set off: the longer title, outside its notes, is the shorter one
        // and then a colon, a dash or a full stop; or both have the same
        // title before such a mark, and one subtitle runs on from the
        // other's.
        let (outside, inside) = (core_text(written), core_text(other));
        let after_mark = [":", " - ", " – ", " — ", ". "].iter().any(|mark| {
            outside.split_once(mark).is_some_and(|(head, _)| {
                let head = album_words(head);
                head == *short
                    || inside
                        .split_once(mark)
                        .is_some_and(|(shared, _)| !head.is_empty() && album_words(shared) == head)
            })
        });
        let set_off = by_artist || EDITION.contains(&next.as_str()) || after_mark;
        short.len() >= 2 && long.starts_with(short) && !numbered && set_off
    }
}

/// Whether two artist and title pairs name the same track, by their
/// normalised keys: what a scrobble this library sent is matched to its
/// echo by, beside its exact time.
pub fn same_track(artist: &str, title: &str, other_artist: &str, other_title: &str) -> bool {
    // A symbol at either end may come back or not.
    // Nor whether it is drawn as an emoji.
    let key = |text: &str| crate::normalize::words_key(&text.replace(['\u{fe0e}', '\u{fe0f}'], ""));
    (key(artist), key(title)) == (key(other_artist), key(other_title))
}

impl Catalog {
    /// Indexes these recordings under every artist and title they show,
    /// their own and each appearance's.
    pub fn new(recordings: Vec<Recording>) -> Self {
        let mut catalog = Self::default();
        for (at, recording) in recordings.iter().enumerate() {
            let mut names: Vec<(&str, &str)> = Vec::new();
            if let (Some(artist), Some(title)) = (&recording.artist, &recording.title) {
                names.push((artist, title));
            }
            for appearance in &recording.appearances {
                let artist = appearance.artist.as_ref().or(recording.artist.as_ref());
                let title = appearance.title.as_ref().or(recording.title.as_ref());
                if let (Some(artist), Some(title)) = (artist, title) {
                    names.push((artist, title));
                }
            }
            for (artist, title) in names {
                for (index, key) in [
                    (&mut catalog.exact, exact_key(artist, title)),
                    (&mut catalog.loose, loose_key(artist, title)),
                    (&mut catalog.near, near_key(artist, title)),
                ] {
                    index.entry(key).or_default().insert(at);
                }
            }
        }
        catalog.recordings = recordings;
        catalog
    }

    /// How many recordings it holds.
    pub fn len(&self) -> usize {
        self.recordings.len()
    }

    /// Whether it holds none.
    pub fn is_empty(&self) -> bool {
        self.recordings.is_empty()
    }

    fn rows(&self, found: &BTreeSet<usize>) -> Vec<i64> {
        found
            .iter()
            .filter_map(|at| self.recordings.get(*at))
            .map(|recording| recording.recording)
            .collect()
    }

    /// What the scrobble is of, if the library can say with confidence.
    pub fn judge(&self, scrobbled: Scrobbled<'_>) -> Verdict {
        self.judge_except(scrobbled, &HashSet::new())
    }

    /// The same, with these canonical recordings no candidates: those the
    /// owner said the scrobble is not.
    pub fn judge_except(&self, scrobbled: Scrobbled<'_>, except: &HashSet<i64>) -> Verdict {
        let (artist, title) = (scrobbled.artist, scrobbled.title);
        let albums = Album::read(scrobbled.album, artist);
        let allowed = |found: &BTreeSet<usize>| -> BTreeSet<usize> {
            found
                .iter()
                .copied()
                .filter(|at| {
                    self.recordings
                        .get(*at)
                        .is_some_and(|recording| !except.contains(&recording.recording))
                })
                .collect()
        };
        // The exact keys first, then the loose ones; the first binding
        // wins, and otherwise the first doubt is the verdict.
        let mut doubt = None;
        for (index, key, rule) in [
            (&self.exact, exact_key(artist, title), Rule::Exact),
            (&self.loose, loose_key(artist, title), Rule::Loose),
        ] {
            let Some(found) = index
                .get(&key)
                .map(allowed)
                .filter(|found| !found.is_empty())
            else {
                continue;
            };
            for album in &albums {
                let verdict = self.choose(&found, album.as_ref(), rule);
                if matches!(verdict, Verdict::Bind { .. }) {
                    return verdict;
                }
                doubt.get_or_insert(verdict);
            }
        }
        if let Some(doubt) = doubt {
            return doubt;
        }
        match self.near.get(&near_key(artist, title)).map(allowed) {
            Some(near) if !near.is_empty() => Verdict::Doubtful {
                doubt: Doubt::Near,
                candidates: self.rows(&near),
            },
            _ => Verdict::Absent,
        }
    }

    /// Which of the recordings the artist and title found the album says
    /// the play was of.
    fn choose(&self, found: &BTreeSet<usize>, album: Option<&Album>, rule: Rule) -> Verdict {
        let candidates: Vec<&Recording> = found
            .iter()
            .filter_map(|at| self.recordings.get(*at))
            .collect();
        let doubtful = |doubt| Verdict::Doubtful {
            doubt,
            candidates: self.rows(found),
        };
        let (recording, named) = match album {
            None => match candidates.as_slice() {
                [one] => (*one, Vec::new()),
                _ => return doubtful(Doubt::Several),
            },
            Some(album) => {
                // The recordings on the album as named, or else on one that
                // fits it.
                let on = |fits: &dyn Fn(&str) -> bool| -> Vec<(&Recording, Vec<&Appearance>)> {
                    candidates
                        .iter()
                        .map(|recording| {
                            let on: Vec<&Appearance> = recording
                                .appearances
                                .iter()
                                .filter(|a| a.album.as_deref().is_some_and(fits))
                                .collect();
                            (*recording, on)
                        })
                        .filter(|(_, on)| !on.is_empty())
                        .collect()
                };
                let exact = on(&|other| normalize(other) == album.exact);
                let fit = if exact.is_empty() {
                    on(&|other| album.fits(other))
                } else {
                    exact
                };
                match (fit.as_slice(), candidates.as_slice()) {
                    ([(one, on)], _) => (*one, on.clone()),
                    ([], [_]) => return doubtful(Doubt::Album),
                    _ => return doubtful(Doubt::Several),
                }
            }
        };
        let appearance: Option<&Appearance> =
            match (named.as_slice(), recording.appearances.as_slice()) {
                ([one], _) => Some(*one),
                ([], [one]) => Some(one),
                _ => None,
            };
        Verdict::Bind {
            recording: appearance.map_or(recording.recording, |a| a.recording),
            appearance: appearance.map(|a| a.appearance),
            candidates: candidates.len(),
            rule,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recording(row: i64, artist: &str, title: &str, albums: &[(i64, &str)]) -> Recording {
        Recording {
            recording: row,
            title: Some(title.to_owned()),
            artist: Some(artist.to_owned()),
            appearances: albums
                .iter()
                .map(|(appearance, album)| Appearance {
                    appearance: *appearance,
                    recording: row,
                    title: None,
                    artist: None,
                    album: Some((*album).to_owned()),
                })
                .collect(),
        }
    }

    fn catalog() -> Catalog {
        Catalog::new(vec![
            recording(1, "The Quiet Harbour", "Lanterns", &[(11, "Low Tide")]),
            recording(
                2,
                "The Quiet Harbour",
                "Paper Boats",
                &[(21, "Low Tide"), (22, "Best Of")],
            ),
            recording(3, "Mira Vale", "Echo", &[(31, "Glass")]),
            recording(4, "Mira Vale", "Echo", &[(41, "Live at the Hall")]),
            recording(5, "Éléonore", "Été", &[(51, "Saisons")]),
            recording(6, "Mira Vale", "Mira Vale", &[(61, "Mira Vale")]),
        ])
    }

    fn judge(artist: &str, album: Option<&str>, title: &str) -> Verdict {
        catalog().judge(Scrobbled {
            artist,
            album,
            title,
        })
    }

    #[test]
    fn one_recording_by_artist_and_title_binds_whatever_the_case_and_punctuation() {
        assert_eq!(
            judge("the quiet harbour", Some("LOW TIDE"), "LANTERNS!"),
            Verdict::Bind {
                recording: 1,
                appearance: Some(11),
                candidates: 1,
                rule: Rule::Exact,
            }
        );
        // On two records, the album picks the appearance, and with none
        // there is no appearance.
        assert_eq!(
            judge("The Quiet Harbour", Some("best of"), "Paper Boats"),
            Verdict::Bind {
                recording: 2,
                appearance: Some(22),
                candidates: 1,
                rule: Rule::Exact,
            }
        );
        assert_eq!(
            judge("The Quiet Harbour", None, "Paper Boats"),
            Verdict::Bind {
                recording: 2,
                appearance: None,
                candidates: 1,
                rule: Rule::Exact,
            }
        );
    }

    #[test]
    fn the_album_breaks_a_tie_and_without_it_the_play_is_doubtful() {
        assert_eq!(
            judge("Mira Vale", Some("Glass"), "Echo"),
            Verdict::Bind {
                recording: 3,
                appearance: Some(31),
                candidates: 2,
                rule: Rule::Exact,
            }
        );
        assert_eq!(
            judge("Mira Vale", Some("Another"), "Echo"),
            Verdict::Doubtful {
                doubt: Doubt::Several,
                candidates: vec![3, 4]
            }
        );
    }

    #[test]
    fn an_album_that_fits_none_of_the_recordings_appearances_holds_the_binding_back() {
        // The library has the studio recording only; the play was of the
        // acoustic record.
        assert_eq!(
            judge("The Quiet Harbour", Some("Acoustic Sessions"), "Lanterns"),
            Verdict::Doubtful {
                doubt: Doubt::Album,
                candidates: vec![1]
            }
        );
        // An edition note, spacing or punctuation is still the same album.
        for album in [
            "Low Tide (Deluxe Edition)",
            "LowTide",
            "Low-Tide",
            "Low Tide [Remastered]",
        ] {
            assert_eq!(
                judge("The Quiet Harbour", Some(album), "Lanterns"),
                Verdict::Bind {
                    recording: 1,
                    appearance: Some(11),
                    candidates: 1,
                    rule: Rule::Exact,
                },
                "{album}"
            );
        }
        // Nor is a disc, a trailing "EP" or ", The", and "S/T" is the album
        // named after the artist.
        for album in [
            "Low Tide CD 1",
            "Low Tide EP",
            "Low Tide - EP",
            "Low Tide, The",
            "null",
        ] {
            assert!(
                matches!(
                    judge("The Quiet Harbour", Some(album), "Lanterns"),
                    Verdict::Bind { recording: 1, .. }
                ),
                "{album}"
            );
        }
        assert!(matches!(
            judge("Mira Vale", Some("S/T"), "Mira Vale"),
            Verdict::Bind { recording: 6, .. }
        ));
        // Nor is a subtitle or an edition after two words or more.
        for album in [
            "Low Tide: Rarities 2001-2009",
            "Low Tide Deluxe Version",
            "Low Tide (Remastered 2011)",
            "Low Tide [Rarities 01 - 09]",
        ] {
            assert!(
                matches!(
                    judge("The Quiet Harbour", Some(album), "Lanterns"),
                    Verdict::Bind { recording: 1, .. }
                ),
                "{album}"
            );
        }
        // A version marker in a note or a subtitle is not passed over, nor
        // a number or a volume after the shared words, and one word is not
        // enough.
        for album in [
            "Low Tide II",
            "Low Tide 2",
            "Low Tide Vol. 3",
            "Low Tide Part Two",
            "Low Tide (Vol. 2)",
            "Low Tide [II]",
            "And",
        ] {
            assert!(
                matches!(
                    judge("The Quiet Harbour", Some(album), "Lanterns"),
                    Verdict::Doubtful {
                        doubt: Doubt::Album,
                        ..
                    }
                ),
                "{album}"
            );
        }
        assert!(matches!(
            judge(
                "Mira Vale",
                Some("Mira Vale: Live in the Hall"),
                "Mira Vale"
            ),
            Verdict::Doubtful {
                doubt: Doubt::Album,
                ..
            }
        ));
        assert!(matches!(
            judge("The Quiet Harbour", Some("Low"), "Lanterns"),
            Verdict::Doubtful {
                doubt: Doubt::Album,
                ..
            }
        ));
        for album in [
            "Low Tide (Live)",
            "Low Tide: Acoustic",
            "Low Tide (Acoustics)",
        ] {
            assert_eq!(
                judge("The Quiet Harbour", Some(album), "Lanterns"),
                Verdict::Doubtful {
                    doubt: Doubt::Album,
                    candidates: vec![1]
                },
                "{album}"
            );
        }
        // "Acoustics" is the one marker "Acoustic" is: a play from it fits
        // the acoustic record.
        assert_eq!(
            album_markers_of("Low Tide (Acoustics)"),
            album_markers_of("Low Tide: Acoustic")
        );
        let acoustic = Catalog::new(vec![recording(
            7,
            "Mira Vale",
            "Undertow",
            &[(71, "Undertow"), (72, "Undertow (Acoustic)")],
        )]);
        for album in [
            "Undertow (Acoustics)",
            "Undertow(Acoustics)",
            "Undertow (ACOUSTICS)",
        ] {
            assert_eq!(
                acoustic.judge(Scrobbled {
                    artist: "Mira Vale",
                    album: Some(album),
                    title: "Undertow",
                }),
                Verdict::Bind {
                    recording: 7,
                    appearance: Some(72),
                    candidates: 1,
                    rule: Rule::Exact,
                },
                "{album}"
            );
        }
        // With no album, one recording binds as before.
        assert!(matches!(
            judge("The Quiet Harbour", None, "Lanterns"),
            Verdict::Bind { recording: 1, .. }
        ));
    }

    #[test]
    fn a_symbol_in_brackets_before_a_note_names_the_song_with_the_symbol() {
        let catalog = Catalog::new(vec![
            recording(13, "Mira Vale", "Night Drives (Deluxe)", &[(131, "Glass")]),
            recording(
                14,
                "Mira Vale",
                "Night Drives + (Deluxe)",
                &[(141, "Glass")],
            ),
        ]);
        assert!(matches!(
            catalog.judge(Scrobbled {
                artist: "Mira Vale",
                album: Some("Glass"),
                title: "Night Drives (+) (Deluxe)",
            }),
            Verdict::Bind { recording: 14, .. }
        ));
    }

    #[test]
    fn a_leading_symbol_hides_neither_the_artist_nor_the_article_of_an_album() {
        let catalog = Catalog::new(vec![
            recording(
                15,
                "\u{21c4} Mira Vale",
                "Undertow",
                &[(151, "Blue Water \u{21c4} Mira Vale")],
            ),
            recording(
                16,
                "Mira Vale",
                "Lanterns",
                &[(161, "\u{21c4} The Blue Water")],
            ),
        ]);
        assert!(matches!(
            catalog.judge(Scrobbled {
                artist: "\u{21c4} Mira Vale",
                album: Some("Blue Water"),
                title: "Undertow",
            }),
            Verdict::Bind { recording: 15, .. }
        ));
        assert!(matches!(
            catalog.judge(Scrobbled {
                artist: "Mira Vale",
                album: Some("\u{21c4} Blue Water"),
                title: "Lanterns",
            }),
            Verdict::Bind { recording: 16, .. }
        ));
    }

    #[test]
    fn an_album_of_a_symbol_is_not_another_of_the_symbol_and_a_word() {
        let catalog = Catalog::new(vec![
            recording(17, "Mira Vale", "Undertow", &[(171, "The +")]),
            recording(18, "=", "Lanterns", &[(181, "Blue Water")]),
        ]);
        let judge = |artist, album, title| {
            catalog.judge(Scrobbled {
                artist,
                album: Some(album),
                title,
            })
        };
        assert!(matches!(
            judge("Mira Vale", "+", "Undertow"),
            Verdict::Doubtful { .. }
        ));
        // An artist of symbols alone is no subtitle of symbols.
        assert!(matches!(
            judge("=", "Blue Water +", "Lanterns"),
            Verdict::Doubtful { .. }
        ));
    }

    #[test]
    fn an_album_whose_subtitle_is_an_artist_named_with_a_symbol_fits() {
        let catalog = Catalog::new(vec![recording(
            12,
            "Mira Vale +",
            "Undertow",
            &[(121, "Blue Water Mira Vale +")],
        )]);
        assert!(matches!(
            catalog.judge(Scrobbled {
                artist: "Mira Vale +",
                album: Some("Blue Water"),
                title: "Undertow",
            }),
            Verdict::Bind { recording: 12, .. }
        ));
    }

    #[test]
    fn a_symbol_at_either_end_of_a_title_is_passed_over_by_the_loose_rule() {
        let catalog = Catalog::new(vec![
            recording(8, "Mira Vale", "\u{21c4} (Lock and Key)", &[(81, "Glass")]),
            recording(9, "Mira Vale", "--->", &[(91, "Glass")]),
        ]);
        let bound = |title| match catalog.judge(Scrobbled {
            artist: "Mira Vale",
            album: Some("Glass"),
            title,
        }) {
            Verdict::Bind {
                recording, rule, ..
            } => Some((recording, rule)),
            _ => None,
        };
        assert_eq!(bound("\u{21c4} (Lock and Key)"), Some((8, Rule::Exact)));
        // The symbol written another way, or left out, is passed over.
        assert_eq!(bound("<===> (Lock and Key)"), Some((8, Rule::Loose)));
        assert_eq!(bound("(Lock and Key)"), Some((8, Rule::Loose)));
        // A leading "The" behind a symbol is passed over too, and a title
        // of words never becomes one of symbols alone.
        let the = Catalog::new(vec![
            recording(
                10,
                "Mira Vale",
                "\u{21c4} The Lock and Key",
                &[(101, "Glass")],
            ),
            recording(11, "Mira Vale", "The +", &[(111, "Glass")]),
        ]);
        let bound_the = |title| match the.judge(Scrobbled {
            artist: "Mira Vale",
            album: Some("Glass"),
            title,
        }) {
            Verdict::Bind { recording, .. } => Some(recording),
            _ => None,
        };
        assert_eq!(bound_the("The Lock and Key"), Some(10));
        assert_eq!(bound_the("+"), None);
        // An echo of a play this library sent is the same track whether
        // the symbol came back or not.
        assert!(same_track("Mira Vale", "Lock +", "Mira Vale", "Lock"));
        assert!(same_track(
            "Mira Vale",
            "Lock\u{2764}\u{fe0f}",
            "Mira Vale",
            "Lock"
        ));
        // A title of symbols alone is still its symbols.
        assert_eq!(bound("--->"), Some((9, Rule::Exact)));
        assert_eq!(bound("<---"), None);
    }

    #[test]
    fn spacing_a_leading_the_feat_and_accents_are_passed_over() {
        let bound = |artist, album, title| match judge(artist, album, title) {
            Verdict::Bind {
                recording, rule, ..
            } => Some((recording, rule)),
            _ => None,
        };
        assert_eq!(
            bound("Quiet Harbour", Some("Low Tide"), "Lanterns"),
            Some((1, Rule::Loose))
        );
        assert_eq!(
            bound("The QuietHarbour", None, "Paper Boats"),
            Some((2, Rule::Loose))
        );
        assert_eq!(
            bound(
                "Mira Vale feat. Ninefold",
                Some("Glass"),
                "Echo (feat. Ninefold)"
            ),
            Some((3, Rule::Loose))
        );
        assert_eq!(
            bound("Mira Vale", Some("Glass"), "Echo feat. Ninefold"),
            Some((3, Rule::Loose))
        );
        assert_eq!(
            bound("Eleonore", Some("Saisons"), "Ete"),
            Some((5, Rule::Loose))
        );
        assert_eq!(
            bound("The Quiet Harbour", None, "Paper Boats"),
            Some((2, Rule::Exact))
        );
        assert_eq!(
            bound("Quiet Harbour", None, "Paper + Boats"),
            Some((2, Rule::Loose))
        );
        assert_eq!(
            bound("Quiet Harbour", None, "Paper & Boats"),
            Some((2, Rule::Loose))
        );
        // A version marker after the featured artist still tells them apart,
        // and the album still holds a loose match back.
        assert_eq!(
            bound("Mira Vale", Some("Glass"), "Echo (feat. Ninefold) [Live]"),
            None
        );
        assert_eq!(
            bound("Quiet Harbour", Some("Acoustic Sessions"), "Lanterns"),
            None
        );
        // Nor does a version said after a bare featured artist, or inside
        // its brackets.
        assert_eq!(
            bound("Mira Vale", None, "Mira Vale feat. Ninefold"),
            Some((6, Rule::Loose))
        );
        assert_eq!(
            bound("Mira Vale", None, "Mira Vale feat. Ninefold - Live"),
            None
        );
        assert_eq!(
            bound("Mira Vale", None, "Mira Vale (feat. Ninefold - Live)"),
            None
        );
        // "Feat" that begins a title is the title.
        assert_eq!(super::without_featured("Feat of Clay"), "Feat of Clay");
    }

    #[test]
    fn of_an_album_and_its_edition_the_one_named_is_the_appearance() {
        let catalog = Catalog::new(vec![recording(
            7,
            "Mira Vale",
            "Ember",
            &[(71, "Glass (Deluxe)"), (72, "Glass")],
        )]);
        for (album, appearance) in [("Glass", 72), ("Glass (Deluxe)", 71)] {
            assert!(
                matches!(
                    catalog.judge(Scrobbled { artist: "Mira Vale", album: Some(album), title: "Ember" }),
                    Verdict::Bind { appearance: Some(found), .. } if found == appearance
                ),
                "{album}"
            );
        }
    }

    #[test]
    fn the_album_breaks_a_tie_as_loosely_as_it_fits() {
        assert!(matches!(
            judge("Mira Vale", Some("Live at the Hall (Expanded)"), "Echo"),
            Verdict::Bind {
                recording: 4,
                appearance: Some(41),
                ..
            }
        ));
    }

    #[test]
    fn a_near_spelling_is_doubtful_and_a_stranger_is_absent() {
        assert_eq!(
            judge("Eleonore", None, "Ete (Remastered 2011)"),
            Verdict::Doubtful {
                doubt: Doubt::Near,
                candidates: vec![5]
            }
        );
        assert_eq!(judge("Nobody We Have", None, "Lanterns"), Verdict::Absent);
    }
}

#[cfg(test)]
mod review_cases {
    use super::*;

    fn on(row: i64, artist: &str, title: &str, album: &str) -> Recording {
        Recording {
            recording: row,
            title: Some(title.to_owned()),
            artist: Some(artist.to_owned()),
            appearances: vec![Appearance {
                appearance: row * 10,
                recording: row,
                title: None,
                artist: None,
                album: Some(album.to_owned()),
            }],
        }
    }

    fn bound(catalog: &Catalog, album: Option<&str>, title: &str) -> Option<i64> {
        let except = HashSet::new();
        match catalog.judge_except(
            Scrobbled {
                artist: "Mira Vale",
                album,
                title,
            },
            &except,
        ) {
            Verdict::Bind { recording, .. } => Some(recording),
            _ => None,
        }
    }

    #[test]
    fn a_take_number_after_a_featured_artist_is_kept() {
        let catalog = Catalog::new(vec![on(1, "Mira Vale", "Echo feat. Nia - Take 1", "Glass")]);
        assert_eq!(
            bound(&catalog, Some("Glass"), "Echo feat. Nia - Take 1"),
            Some(1)
        );
        assert_eq!(
            bound(&catalog, Some("Glass"), "Echo feat. Nia - Take 2"),
            None
        );
        // Nor inside the featured artist's part.
        let catalog = Catalog::new(vec![on(
            2,
            "Mira Vale",
            "Echo (feat. Nia - Take 1)",
            "Glass",
        )]);
        assert_eq!(
            bound(&catalog, Some("Glass"), "Echo (feat. Nia - Take 2)"),
            None
        );
        let catalog = Catalog::new(vec![on(3, "Mira Vale", "Echo feat. Nia, Take 1", "Glass")]);
        assert_eq!(
            bound(&catalog, Some("Glass"), "Echo feat. Nia, Take 2"),
            None
        );
        // "Feat of" is words, not a featured artist.
        assert_eq!(
            super::without_featured("A Feat of Strength"),
            "A Feat of Strength"
        );
    }

    #[test]
    fn a_numbered_or_merely_longer_album_is_another() {
        let catalog = Catalog::new(vec![on(1, "Mira Vale", "Echo", "Low Tide")]);
        for album in [
            "Low Tide (2)",
            "Low Tide Vol.2",
            "Low Tide Strikes Back",
            "Low Tide (2) (2011 Remaster)",
            "Low Tide (Vol.II)",
            "Low Tide Strikes Back (Bonus, 2020)",
            "Low Tide Strikes Back, The",
            "Low Tide Rising: The Beginning",
        ] {
            assert_eq!(bound(&catalog, Some(album), "Echo"), None, "{album}");
        }
        for album in [
            "Low Tide: Rarities",
            "Low Tide - Deluxe",
            "Low Tide Expanded Edition",
            "Low Tide Soundtrack",
            "Low Tide Mira Vale",
        ] {
            assert_eq!(bound(&catalog, Some(album), "Echo"), Some(1), "{album}");
        }
    }

    #[test]
    fn a_numbered_album_fits_its_remaster_and_punctuation_in_a_title_sets_nothing_off() {
        let catalog = Catalog::new(vec![on(1, "Mira Vale", "Echo", "Low Tide 2")]);
        assert_eq!(
            bound(&catalog, Some("Low Tide 2 (2011 Remaster)"), "Echo"),
            Some(1)
        );
        let catalog = Catalog::new(vec![on(2, "Mira Vale", "Echo", "Hello, World")]);
        assert_eq!(
            bound(&catalog, Some("Hello, World Strikes Back"), "Echo"),
            None
        );
        assert_eq!(
            bound(&catalog, Some("Hello, World: Live Sessions"), "Echo"),
            None
        );
        assert_eq!(
            bound(&catalog, Some("Hello, World: Rarities"), "Echo"),
            Some(2)
        );
        // A subtitle that runs on from the other's, and one after a full
        // stop.
        let catalog = Catalog::new(vec![
            on(3, "Mira Vale", "Salt", "Low Tide: A Benefit for the Coast"),
            on(4, "Mira Vale", "Road", "Lantern Light. And Then the Dark"),
        ]);
        assert_eq!(
            bound(
                &catalog,
                Some("Low Tide: A Benefit for the Coast Fund"),
                "Salt"
            ),
            Some(3)
        );
        assert_eq!(bound(&catalog, Some("Lantern Light"), "Road"), Some(4));
    }

    #[test]
    fn an_album_named_literally_comes_before_what_it_may_stand_for() {
        let catalog = Catalog::new(vec![
            on(1, "Mira Vale", "Echo", "ST"),
            on(2, "Mira Vale", "Echo", "Mira Vale"),
            on(3, "Mira Vale", "Salt", "Null"),
            on(4, "Mira Vale", "Salt", "Glass"),
        ]);
        assert_eq!(bound(&catalog, Some("ST"), "Echo"), Some(1));
        assert_eq!(bound(&catalog, Some("Self-Titled"), "Echo"), Some(2));
        assert_eq!(bound(&catalog, Some("Null"), "Salt"), Some(3));
    }

    #[test]
    fn an_exact_album_beats_a_loose_one_across_recordings() {
        let catalog = Catalog::new(vec![
            on(1, "Mira Vale", "Echo", "Glass"),
            on(2, "Mira Vale", "Echo", "Glass (Deluxe)"),
        ]);
        assert_eq!(bound(&catalog, Some("Glass"), "Echo"), Some(1));
        assert_eq!(bound(&catalog, Some("Glass (Deluxe)"), "Echo"), Some(2));
    }

    #[test]
    fn a_loose_match_on_the_album_named_is_not_hidden_by_an_exact_one_elsewhere() {
        let catalog = Catalog::new(vec![
            on(1, "Mira Vale", "Echo", "Slate"),
            on(2, "Mira Vale", "Echo feat. Nia", "Glass"),
        ]);
        assert_eq!(bound(&catalog, Some("Glass"), "Echo"), Some(2));
    }

    #[test]
    fn an_excluded_recording_is_no_candidate() {
        let catalog = Catalog::new(vec![
            on(1, "Mira Vale", "Echo", "Glass"),
            on(2, "Mira Vale", "Echo", "Slate"),
        ]);
        let scrobbled = Scrobbled {
            artist: "Mira Vale",
            album: None,
            title: "Echo",
        };
        assert!(matches!(
            catalog.judge(scrobbled),
            Verdict::Doubtful {
                doubt: Doubt::Several,
                ..
            }
        ));
        assert!(matches!(
            catalog.judge_except(scrobbled, &HashSet::from([2])),
            Verdict::Bind { recording: 1, .. }
        ));
        assert!(matches!(
            catalog.judge_except(scrobbled, &HashSet::from([1, 2])),
            Verdict::Absent
        ));
    }
}
