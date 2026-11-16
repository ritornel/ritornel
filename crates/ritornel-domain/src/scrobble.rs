//! Matching a scrobble to the library: which recording, if any, a play that
//! another service reported by its text alone was of. See "Scrobbles" in
//! docs/design/identity.md.
//!
//! A scrobble names an artist, a title and maybe an album, as some other
//! player sent them, and nothing else that can be trusted. So it binds only
//! on a confident match, and otherwise stays unbound and is listed:
//!
//! - **Bind** when the normalised artist and title name exactly one
//!   recording, or name several and the album names exactly one of them.
//! - **Doubtful** when they name several the album cannot tell apart, or
//!   when nothing matches them exactly but something does once accents and a
//!   bracketed note such as `(Remastered 2011)` are left out. Each is listed
//!   for the owner, with what it nearly matched.
//! - **Absent** when nothing comes close: music played and not owned.
//!
//! A scrobble never creates an artist, a record or a song. The matcher is
//! pure; the engine reads the library into a [`Catalog`] and binds what it
//! says to bind.

use std::collections::{BTreeSet, HashMap};

use crate::normalize::{core, fold_accents, normalize};

/// The matcher's version, recorded on each binding it makes.
pub const MATCHER_VERSION: i64 = 1;

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
    /// Nothing matches exactly, and something does once accents and
    /// bracketed notes are left out.
    Near,
}

impl Doubt {
    /// The doubt as a word.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Several => "several",
            Self::Near => "near",
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

/// The library as the matcher reads it: every recording under the keys of
/// its artist and title.
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    recordings: Vec<Recording>,
    exact: HashMap<(String, String), BTreeSet<usize>>,
    loose: HashMap<(String, String), BTreeSet<usize>>,
}

/// The exact key: normalised artist and title.
fn exact_key(artist: &str, title: &str) -> (String, String) {
    (normalize(artist), normalize(title))
}

/// The loose key: accents off, and the title without what is in brackets.
fn loose_key(artist: &str, title: &str) -> (String, String) {
    (fold_accents(&normalize(artist)), fold_accents(&core(title)))
}

/// The appearances of a recording on the album whose normalised title is
/// `album`.
fn on_album<'r>(recording: &'r Recording, album: Option<&str>) -> Vec<&'r Appearance> {
    let Some(album) = album else {
        return Vec::new();
    };
    recording
        .appearances
        .iter()
        .filter(|a| a.album.as_deref().map(normalize).as_deref() == Some(album))
        .collect()
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
                catalog
                    .exact
                    .entry(exact_key(artist, title))
                    .or_default()
                    .insert(at);
                catalog
                    .loose
                    .entry(loose_key(artist, title))
                    .or_default()
                    .insert(at);
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
        let Some(found) = self
            .exact
            .get(&exact_key(scrobbled.artist, scrobbled.title))
        else {
            return match self
                .loose
                .get(&loose_key(scrobbled.artist, scrobbled.title))
            {
                Some(near) => Verdict::Doubtful {
                    doubt: Doubt::Near,
                    candidates: self.rows(near),
                },
                None => Verdict::Absent,
            };
        };
        let album = scrobbled
            .album
            .map(normalize)
            .filter(|album| !album.is_empty());
        let on_album = |recording| on_album(recording, album.as_deref());
        let candidates: Vec<&Recording> = found
            .iter()
            .filter_map(|at| self.recordings.get(*at))
            .collect();
        let chosen = match candidates.as_slice() {
            [one] => Some(*one),
            several => {
                let on: Vec<&Recording> = several
                    .iter()
                    .copied()
                    .filter(|recording| !on_album(recording).is_empty())
                    .collect();
                match on.as_slice() {
                    [one] => Some(*one),
                    _ => None,
                }
            }
        };
        let Some(recording) = chosen else {
            return Verdict::Doubtful {
                doubt: Doubt::Several,
                candidates: self.rows(found),
            };
        };
        let named = on_album(recording);
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
            judge("the quiet harbour", Some("Something Else"), "LANTERNS!"),
            Verdict::Bind {
                recording: 1,
                appearance: Some(11),
                candidates: 1
            }
        );
        // On two records, the album picks the appearance, and with none
        // there is no appearance.
        assert_eq!(
            judge("The Quiet Harbour", Some("best of"), "Paper Boats"),
            Verdict::Bind {
                recording: 2,
                appearance: Some(22),
                candidates: 1
            }
        );
        assert_eq!(
            judge("The Quiet Harbour", None, "Paper Boats"),
            Verdict::Bind {
                recording: 2,
                appearance: None,
                candidates: 1
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
                candidates: 2
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
