//! Search as a person types it: the few artists, records and songs whose
//! names match what was typed, best first, for the results that drop down
//! under the search field ("Frame and navigation" in
//! docs/internals/desktop.md).
//!
//! - `library.search` takes a [`SearchParams`] and returns
//!   [`SearchResults`].
//!
//! It is one call and holds nothing, so that a client can send it at each
//! pause in typing. A list of everything that matches is `list.open` with
//! [`Query::text`](crate::Query).

use serde::{Deserialize, Serialize};

use crate::{AlbumTile, ArtistRow, Id, ListKind, SongRow};

/// The most of each kind `library.search` answers with.
pub const MAX_SEARCH_RESULTS: u32 = 20;

/// The params of `library.search`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SearchParams {
    /// What was typed. Case and accents are ignored, as `list.open` ignores
    /// them, and so is the punctuation between words.
    pub text: String,
    /// How many of each kind, at most [`MAX_SEARCH_RESULTS`]. Left out, 5.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub limit: Option<u32>,
}

/// The result of `library.search`: the best matches of each kind.
///
/// A row matches when its own name has a word that starts with what was
/// typed: an artist by their name or the name they are filed under, a
/// record or a song by its title. "pine" finds Pinegrove and The Pines and
/// not Happiness. Each kind is ranked the same way: a name that is what was
/// typed, then one that starts with it, then one with a word that does;
/// within each, the most played first, then as the walls file them. A song
/// on several records is listed once.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SearchResults {
    /// Artists who have a record in the library, as the Artists list has
    /// them.
    pub artists: Vec<ArtistRow>,
    /// Records, as the Albums list has them.
    pub albums: Vec<AlbumTile>,
    /// Songs, as the Songs list has them, with their record's cover.
    pub songs: Vec<SongFound>,
    /// The kind whose first row is the best match of all: artists before
    /// records before songs among equals. Return opens it. `null` when
    /// nothing matched.
    pub top: Option<ListKind>,
    /// The library revision this was read at.
    pub library_revision: u64,
}

/// A song that search found: its row, and the cover of the record it is
/// on, which a Songs row does not carry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SongFound {
    /// The song's row.
    #[serde(flatten)]
    pub song: SongRow,
    /// Its record's cover, for `GET /v1/art/{artId}/{size}`. `null` when it
    /// has none, or the song is on no record.
    pub art_id: Option<Id>,
}
