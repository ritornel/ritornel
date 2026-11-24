//! Corrections: the evidence a library keeps for correcting itself from
//! MusicBrainz. See docs/internals/corrections.md.
//!
//! The calls name files and folders by their path under their root, so
//! every one needs a `full` session.

use serde::{Deserialize, Serialize};

/// The params of `corrections.discs`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct DiscsParams {
    /// One folder, by its path under its root, with `/` between its parts.
    /// Its discs come with the file at each position. Left out, every
    /// disc of the library comes, without its files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub folder: Option<String>,
}

/// How a disc's layout was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum LayoutMethod {
    /// A drive's table of sectors in a rip log.
    Log,
    /// A CUE sheet, with the exact lengths of the files it names.
    Cue,
    /// A CUE sheet whose files were renamed since it was written.
    RenamedCue,
    /// The files' lengths in order, checked against an AccurateRip disc ID
    /// printed beside them.
    #[serde(rename = "accuraterip")]
    AccurateRip,
    /// The files' lengths in order, checked against the CDDB ID a CUE
    /// sheet prints.
    Cddb,
}

/// One file at one position of a disc.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct DiscFile {
    /// The track's position on the disc, from 1, or 0 for a file that holds
    /// the whole disc.
    pub position: u32,
    /// The file, by its path under its root.
    pub path: String,
}

/// One layout of one folder's files: one CD, as its sidecars give it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct Disc {
    /// The root the folder is in, by its name.
    pub root: String,
    /// The folder, by its path under its root: empty for the root.
    pub folder: String,
    /// The MusicBrainz disc ID the layout gives.
    pub disc_id: String,
    /// How many tracks it has.
    pub tracks: u32,
    /// How long it is, in CD sectors.
    pub sectors: u64,
    /// Each way it was found, in the order found.
    pub methods: Vec<LayoutMethod>,
    /// Whether the layout may not be the disc's: a CUE sheet that disagrees
    /// with the CDDB ID it prints.
    pub provisional: bool,
    /// Whether one file holds the whole disc.
    pub whole_file: bool,
    /// Whether every track was matched to one file by its length.
    pub matched: bool,
    /// The file at each position, when one folder was asked for. Empty
    /// otherwise.
    pub files: Vec<DiscFile>,
}

/// The result of `corrections.discs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct DiscList {
    /// The library revision it read.
    pub library_revision: u64,
    /// How many folders hold audio, of those asked about.
    pub folders: u64,
    /// How many of those have at least one layout that is not provisional.
    pub with_layout: u64,
    /// Each layout, by root, folder and disc ID.
    pub discs: Vec<Disc>,
}
