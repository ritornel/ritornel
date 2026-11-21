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

use std::collections::{BTreeSet, HashMap};

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
/// " feat. Somebody" up to the next bracket or the end.
pub(crate) fn without_featured(text: &str) -> String {
    let mut text = text.to_owned();
    loop {
        // ASCII lower case keeps every byte where it was.
        let lower = text.to_ascii_lowercase();
        let Some(at) = FEATURED.iter().filter_map(|word| lower.find(word)).min() else {
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
            _ => lower
                .get(at..)
                .and_then(|rest| rest.find(['(', '[']))
                .map(|end| at + end),
        }
        .unwrap_or(text.len());
        text.replace_range(at..end, " ");
    }
}

/// A key with accents folded, a leading "the" and every "and" passed over,
/// and no spaces: "&", "+" and "/" between two words are then all one.
fn squash(key: &str) -> String {
    let key = fold_accents(key);
    let key = match key.strip_prefix("the ") {
        Some(rest) if !rest.is_empty() => rest,
        _ => key.as_str(),
    };
    let squashed: String = key.split(' ').filter(|word| *word != "and").collect();
    if squashed.is_empty() {
        key.split(' ').collect()
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
    let mut markers = markers_of(title, 2);
    markers.sort();
    (key(artist), format!("{}|{}", key(title), markers.join(",")))
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
    let mut words: Vec<&str> = key.split(' ').filter(|word| *word != "and").collect();
    if words.is_empty() {
        words = key.split(' ').collect();
    }
    while words.len() > 1 && matches!(words.last(), Some(&("ep" | "the"))) {
        words.pop();
    }
    if words.len() > 1 && words.first() == Some(&"the") {
        words.remove(0);
    }
    words.into_iter().map(str::to_owned).collect()
}

/// The numbers an album title gives, notes and all: a volume or a part,
/// or a roman numeral such as "II", which tell "Low Tide (Vol. 2)" from
/// "Low Tide". Other digits, such as a year or "10,000", are not.
fn album_numbers(album: &str) -> Vec<u32> {
    let album = disc_in_title(album).map_or_else(|| album.to_owned(), |(title, _)| title);
    let key = normalize(&album);
    let words: Vec<&str> = key.split(' ').collect();
    let mut found: Vec<u32> = Vec::new();
    for (at, word) in words.iter().enumerate() {
        let after_volume = at
            .checked_sub(1)
            .and_then(|before| words.get(before))
            .is_some_and(|before| {
                matches!(
                    *before,
                    "vol" | "vols" | "volume" | "part" | "pt" | "book" | "chapter"
                )
            });
        // A roman numeral of one letter is a word as often as not.
        let roman = word.len() > 1 && word.chars().all(|c| "ivx".contains(c));
        let number = if after_volume || roman {
            part_number(word)
        } else {
            None
        };
        found.extend(number);
    }
    found.sort_unstable();
    found
}

/// What an album is compared by: [`album_words`] with no spaces.
fn album_key(album: &str) -> String {
    album_words(album).concat()
}

/// The album a scrobble names, as it is compared: its normalised title,
/// which picks an appearance first, its [`album_key`], and its version
/// markers, which must agree.
struct Album {
    exact: String,
    loose: String,
    words: Vec<String>,
    markers: Vec<String>,
    numbers: Vec<u32>,
}

impl Album {
    /// The album as it is compared. "S/T" or "Self-Titled" is the album
    /// named after the artist, and "null" is no album.
    fn of(album: &str, artist: &str) -> Option<Self> {
        let album = match normalize(album).as_str() {
            "null" | "undefined" => return None,
            "s t" | "st" | "self titled" | "selftitled" => artist,
            _ => album,
        };
        let loose = album_key(album);
        (!loose.is_empty()).then(|| Self {
            exact: normalize(album),
            loose,
            words: album_words(album),
            markers: album_markers_of(album),
            numbers: album_numbers(album),
        })
    }

    /// Whether an appearance's album is this one, loosely: the same, or the
    /// one is the other's first two words or more and an unbracketed
    /// subtitle or edition after them, as "Low Tide: Rarities" is "Low
    /// Tide". What follows is not a number or a volume: "Low Tide II" is
    /// another album. Their version markers, such as "live", and their
    /// numbers, in a note or not, agree.
    fn fits(&self, album: &str) -> bool {
        if album_markers_of(album) != self.markers || album_numbers(album) != self.numbers {
            return false;
        }
        if album_key(album) == self.loose {
            return true;
        }
        let words = album_words(album);
        let (short, long) = if words.len() < self.words.len() {
            (&words, &self.words)
        } else {
            (&self.words, &words)
        };
        let numbered = long.get(short.len()).is_some_and(|next| {
            part_number(next).is_some()
                || matches!(
                    next.as_str(),
                    "vol" | "volume" | "part" | "pt" | "chapter" | "book"
                )
        });
        short.len() >= 2 && long.starts_with(short) && !numbered
    }
}

/// The appearances of a recording on the album: those whose album is the
/// same by its normalised title, or else those that fit it by its
/// [`album_key`], so that of an album and its deluxe edition the one named
/// is picked.
fn on_album<'r>(recording: &'r Recording, album: &Album) -> Vec<&'r Appearance> {
    let by = |fits: &dyn Fn(&str) -> bool| -> Vec<&'r Appearance> {
        recording
            .appearances
            .iter()
            .filter(|a| a.album.as_deref().is_some_and(fits))
            .collect()
    };
    let exact = by(&|other| normalize(other) == album.exact);
    if exact.is_empty() {
        by(&|other| album.fits(other))
    } else {
        exact
    }
}

/// Whether two artist and title pairs name the same track, by their
/// normalised keys: what a scrobble this library sent is matched to its
/// echo by, beside its exact time.
pub fn same_track(artist: &str, title: &str, other_artist: &str, other_title: &str) -> bool {
    exact_key(artist, title) == exact_key(other_artist, other_title)
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
        let (artist, title) = (scrobbled.artist, scrobbled.title);
        let album = scrobbled.album.and_then(|album| Album::of(album, artist));
        for (index, key, rule) in [
            (&self.exact, exact_key(artist, title), Rule::Exact),
            (&self.loose, loose_key(artist, title), Rule::Loose),
        ] {
            if let Some(found) = index.get(&key) {
                return self.choose(found, album.as_ref(), rule);
            }
        }
        match self.near.get(&near_key(artist, title)) {
            Some(near) => Verdict::Doubtful {
                doubt: Doubt::Near,
                candidates: self.rows(near),
            },
            None => Verdict::Absent,
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
                let fit: Vec<(&Recording, Vec<&Appearance>)> = candidates
                    .iter()
                    .map(|recording| (*recording, on_album(recording, album)))
                    .filter(|(_, on)| !on.is_empty())
                    .collect();
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
        for album in ["Low Tide (Live)", "Low Tide: Acoustic"] {
            assert_eq!(
                judge("The Quiet Harbour", Some(album), "Lanterns"),
                Verdict::Doubtful {
                    doubt: Doubt::Album,
                    candidates: vec![1]
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
