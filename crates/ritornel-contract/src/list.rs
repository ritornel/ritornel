//! Large lists, read through views. See decisions 7 to 10 and "Large lists"
//! in docs/design/contract.md.
//!
//! `list.open` resolves a query once into an ordered list of row IDs held
//! by the server, and returns a view ID and an exact total. Membership and
//! order are frozen in the view; what a row says is read fresh each time
//! it is asked for. A view is replaced, never changed: when the library
//! changes the server sends a [`ListEvent`], and a client that must take a
//! new order opens a successor with [`Replaces`], naming the row it wants
//! to keep in place.
//!
//! - `list.open` takes a [`ListOpenParams`] and returns a [`ListOpened`].
//! - `list.window` takes a [`ListWindowParams`] and returns a [`ListWindow`].
//! - `list.sections` takes a [`ViewParams`] and returns [`ListSections`]:
//!   where each letter, or each stretch of time, begins.
//! - `list.locate` takes a [`ListLocateParams`] and returns a
//!   [`ListLocated`]: the index of a row, by ID or by the start of its name.
//! - `list.close` takes a [`ViewParams`] and returns a [`ListClosed`].
//!
//! A row the library no longer has comes back as a [`Row::Gone`] at its
//! index. A view that has ended fails every call with `view_expired`, and
//! the client opens it again.

use serde::{Deserialize, Serialize};

use crate::Id;

/// What a list holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum ListKind {
    /// The artists who have a record in the library: one [`ArtistRow`]
    /// each.
    Artists,
    /// The library's records: one [`AlbumTile`] for each release group
    /// shown as one tile, and one for each release of a group shown
    /// expanded.
    Albums,
    /// Track appearances: one [`SongRow`] each.
    Songs,
}

impl ListKind {
    /// Every kind.
    pub const ALL: &[Self] = &[Self::Artists, Self::Albums, Self::Songs];

    /// The sort keys a list of this kind takes, the first being the one it
    /// takes when none is named.
    pub const fn sorts(self) -> &'static [SortKey] {
        match self {
            Self::Artists => &[SortKey::Name],
            Self::Albums => &[
                SortKey::Artist,
                SortKey::Title,
                SortKey::Year,
                SortKey::Added,
            ],
            Self::Songs => &[SortKey::Title, SortKey::Album],
        }
    }

    /// The predicates a filter on a list of this kind may use.
    pub const fn predicates(self) -> &'static [PredicateName] {
        match self {
            Self::Artists => &[],
            Self::Albums | Self::Songs => &[PredicateName::ByArtist],
        }
    }
}

/// An order a list can be read in. Every order ends in a fixed tiebreak,
/// so that it is total and a view of the same query twice has the same
/// order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum SortKey {
    /// Artists: by name, A to Z, as a person files them: a sort name where
    /// there is one, a leading "The", "A" or "An" set aside, case and
    /// accents ignored, and names that begin with no letter after Z.
    Name,
    /// Records: by artist, as [`SortKey::Name`] orders them, then by the
    /// year each first came out, oldest first, then by title.
    Artist,
    /// Records and songs: by title, A to Z, by the same rule as names.
    Title,
    /// Records: by the year each first came out, newest first, then by
    /// artist and title. Records with no year come last.
    Year,
    /// Records: by when they came into the library, newest first.
    Added,
    /// Songs: by record, as [`SortKey::Artist`] orders records, then by
    /// disc and track number.
    Album,
}

/// One key of a query's order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct Sort {
    /// The order.
    pub key: SortKey,
}

/// The version of the filter language this server reads. A filter carries
/// the version it was written in, so a saved one can be read later.
pub const FILTER_VERSION: u32 = 1;

/// A filter: the versioned query tree of decision 10 in
/// docs/design/contract.md. Version 1 has one predicate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct Filter {
    /// The version of the filter language it is written in. A server
    /// refuses a version it does not read with `unsupported`.
    pub version: u32,
    /// What a row must satisfy.
    #[serde(rename = "where")]
    pub predicate: Predicate,
}

/// A condition on a row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(tag = "is", rename_all = "snake_case")]
pub enum Predicate {
    /// Records whose artist credit names this artist, or songs whose
    /// credit does: an artist's records and songs.
    ByArtist {
        /// The artist.
        #[serde(rename = "artistId")]
        artist_id: Id,
    },
}

impl Predicate {
    /// The predicate's name, as the descriptor lists it.
    pub const fn name(&self) -> PredicateName {
        match self {
            Self::ByArtist { .. } => PredicateName::ByArtist,
        }
    }
}

/// The name of a predicate, as the descriptor lists what a list supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum PredicateName {
    /// [`Predicate::ByArtist`].
    ByArtist,
}

/// What a list is of, and in what order.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct Query {
    /// A substring that the row's title, artist or album must contain,
    /// ignoring case and accents. A query of one or two characters matches
    /// the start of a word instead, since nearly every row contains a
    /// letter or two.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub text: Option<String>,
    /// A condition every row must satisfy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub filter: Option<Filter>,
    /// The order. At most one key in this version: each key carries its
    /// own tiebreak. Left out, a list takes its kind's first order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub sort: Option<Vec<Sort>>,
}

/// A stretch of a view's rows: from `offset`, at most `limit` of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct Window {
    /// The index of the first row.
    pub offset: u64,
    /// How many rows, at most the descriptor's `maxWindowRows`.
    pub limit: u64,
}

/// The view a new one replaces, and the row to keep in place.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct Replaces {
    /// The view. It is closed when the new one opens, and calls on it fail
    /// with `view_expired` from then on.
    pub view: String,
    /// The ID of the row the client wants to keep where it is on screen,
    /// such as the first one visible: an `artistId`, an album's `ref` ID
    /// or an `appearanceId`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub anchor: Option<String>,
}

/// The params of `list.open`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ListOpenParams {
    /// What the list holds.
    pub kind: ListKind,
    /// Which rows, and in what order.
    #[serde(default)]
    pub query: Query,
    /// The rows to send with the answer, so that a screen can draw at once.
    /// Left out, none are sent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub window: Option<Window>,
    /// The view this one replaces.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub replaces: Option<Replaces>,
}

/// The result of `list.open`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ListOpened {
    /// The view's ID, for the calls that read it.
    pub view: String,
    /// How many rows the view has. It never changes: a scrollbar can be
    /// exact.
    pub total: u64,
    /// The library revision the view was resolved at.
    pub library_revision: u64,
    /// The index of the first row in `rows`.
    pub offset: u64,
    /// The rows of the window asked for, in order. Empty when none was.
    pub rows: Vec<Row>,
    /// Where the anchor row of the view this replaces is in this one. It is
    /// left out when nothing was replaced, no anchor was named, or the
    /// anchor is not in this view.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub anchor_index: Option<u64>,
}

/// The params of `list.window`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ListWindowParams {
    /// The view.
    pub view: String,
    /// The index of the first row.
    pub offset: u64,
    /// How many rows, at most the descriptor's `maxWindowRows`.
    pub limit: u64,
}

/// The result of `list.window`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ListWindow {
    /// The view.
    pub view: String,
    /// The index of the first row in `rows`.
    pub offset: u64,
    /// The rows, in order. Fewer than asked for at the end of the view.
    pub rows: Vec<Row>,
}

/// The params of a call that names only a view: `list.sections` and
/// `list.close`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ViewParams {
    /// The view.
    pub view: String,
}

/// Where one section of a view begins.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct Section {
    /// What the section is: a letter, `#` for names that begin with no
    /// letter, a year, or a stretch of time such as "This week" or
    /// "October".
    pub label: String,
    /// The index of its first row.
    pub index: u64,
}

/// The result of `list.sections`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ListSections {
    /// The sections, in the view's order. Letters for an order by name or
    /// title, years for an order by year, stretches of time for an order
    /// by when. Empty for an order that has none.
    pub sections: Vec<Section>,
}

/// The params of `list.locate`: a view, and a row's ID or the start of its
/// name. Exactly one of the two is given.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ListLocateParams {
    /// The view.
    pub view: String,
    /// The row's ID: an `artistId`, an album's `ref` ID or an
    /// `appearanceId`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub id: Option<String>,
    /// The start of what the view is ordered by, as typed, for
    /// type-to-find. It finds the first row whose sort text begins with it,
    /// or else the first row after where it would be.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub prefix: Option<String>,
}

/// The result of `list.locate`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ListLocated {
    /// The row's index, or `null` when the view does not hold it.
    pub index: Option<u64>,
}

/// The result of `list.close`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ListClosed {
    /// Whether a view by that ID was open. Closing one that is not changes
    /// nothing.
    pub closed: bool,
}

/// One row of a view.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Row {
    /// A row of a list of artists.
    Artist(ArtistRow),
    /// A row of a list of records.
    Album(AlbumTile),
    /// A row of a list of songs.
    Song(SongRow),
    /// A row the library no longer has, at the index it had.
    Gone(GoneRow),
}

/// A row the library no longer has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct GoneRow {
    /// Always `true`.
    pub gone: bool,
}

/// An artist credit as it is written, and the artists it names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct Credit {
    /// The whole credit as one string, such as "Simon & Garfunkel".
    pub text: String,
    /// The artists it names, in order.
    pub artist_ids: Vec<Id>,
}

/// One artist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ArtistRow {
    /// The artist.
    pub artist_id: Id,
    /// Their name, as displayed. `null` until the library has one.
    pub name: Option<String>,
    /// The name they are filed under, where it differs from how it is
    /// written, such as "Beatles, The".
    pub sort_name: Option<String>,
    /// How many records name them in their credit, counted as the Albums
    /// list shows them.
    pub album_count: u64,
    /// How many songs on those records there are, and loose songs of
    /// theirs.
    pub track_count: u64,
    /// A picture to draw them with until there are photographs: the cover
    /// of their earliest record that has one. `null` when none has.
    pub art_id: Option<Id>,
}

/// What an Albums tile points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AlbumRef {
    /// A release group shown as one tile.
    ReleaseGroup {
        /// The release group.
        id: Id,
    },
    /// One release of a group shown expanded.
    Release {
        /// The release.
        id: Id,
    },
}

impl AlbumRef {
    /// The ID, whichever kind it is.
    pub const fn id(&self) -> Id {
        match self {
            Self::ReleaseGroup { id } | Self::Release { id } => *id,
        }
    }
}

/// A format, as a copy of a track has it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct Format {
    /// The codec, such as `flac` or `mp3`.
    pub codec: String,
    /// Whether it is lossless.
    pub lossless: bool,
    /// Bits a sample, for a format that has them.
    pub bit_depth: Option<u32>,
    /// Samples a second.
    pub sample_rate_hz: u32,
    /// The bit rate, for a lossy format.
    pub bitrate_kbps: Option<u32>,
}

/// The quality of a record: the best copy across its tracks, and whether
/// the tracks differ.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct Quality {
    /// The best format any of its tracks has: lossless before lossy, then
    /// more bits, a higher rate, a higher bit rate.
    pub best: Format,
    /// Whether its tracks are in more than one format.
    pub varies: bool,
}

/// One tile of the Albums list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct AlbumTile {
    /// What the tile is.
    #[serde(rename = "ref")]
    pub album: AlbumRef,
    /// The title of the release it shows. `null` until the library has one.
    pub title: Option<String>,
    /// Who it is by.
    pub artist_credit: Option<Credit>,
    /// The year it first came out, for a group shown as one tile, or the
    /// release's own year.
    pub year: Option<i64>,
    /// How many tracks the release it shows has.
    pub track_count: u64,
    /// How long they are together.
    pub duration_ms: u64,
    /// How many versions its group has. 1 for a record with one.
    pub version_count: u64,
    /// Its cover, for `GET /v1/art/{artId}/{size}`. `null` when it has none.
    pub art_id: Option<Id>,
    /// The best copy across its tracks. `null` when none of its tracks has
    /// a copy the library has read.
    pub quality: Option<Quality>,
}

/// Which copy of a track a copy is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum CopyRole {
    /// The file the owner keeps.
    Master,
    /// A copy made from it for a smaller device.
    Derived,
}

/// One copy of a track: which server has it, and its format.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct TrackCopy {
    /// The server that has it. `servers.list` says whether it can be
    /// reached.
    pub server: Id,
    /// Which copy it is.
    pub role: CopyRole,
    /// Its format.
    #[serde(flatten)]
    pub format: Format,
}

/// One row of the Songs list: a track appearance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SongRow {
    /// The track appearance.
    pub appearance_id: Id,
    /// The recording it is of.
    pub recording_id: Id,
    /// The release it is on. `null` for a loose file.
    pub release_id: Option<Id>,
    /// Its title.
    pub title: Option<String>,
    /// Who it is by.
    pub artist_credit: Option<Credit>,
    /// The title of its release.
    pub album_title: Option<String>,
    /// Its disc number.
    pub disc: Option<i64>,
    /// Its track number.
    pub track: Option<i64>,
    /// The year of its release.
    pub year: Option<i64>,
    /// How long it is.
    pub duration_ms: Option<i64>,
    /// How many times its recording has been played, by the current rule.
    pub play_count: u64,
    /// When its recording was last played, or `null` when no dated play
    /// exists.
    pub last_played_at: Option<i64>,
    /// When it came into the library.
    pub added_at: Option<i64>,
    /// The copies of it that exist, and where.
    pub copies: Vec<TrackCopy>,
}

/// What a list of each kind supports, as the descriptor says.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ListSupport {
    /// The kind.
    pub kind: ListKind,
    /// The orders it can be read in, the first being the one it takes
    /// when none is named.
    pub sorts: Vec<SortKey>,
    /// The predicates a filter on it may use.
    pub predicates: Vec<PredicateName>,
}

impl ListSupport {
    /// What this build supports for every kind.
    pub fn all() -> Vec<Self> {
        ListKind::ALL
            .iter()
            .map(|kind| Self {
                kind: *kind,
                sorts: kind.sorts().to_vec(),
                predicates: kind.predicates().to_vec(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn id(byte: u8) -> Id {
        Id::from_bytes([byte; 16])
    }

    #[test]
    fn a_query_is_text_a_versioned_filter_and_an_order() {
        let wire = json!({
            "kind": "albums",
            "query": {
                "text": "night",
                "filter": { "version": 1, "where": { "is": "by_artist", "artistId": id(0xa1) } },
                "sort": [{ "key": "year" }],
            },
            "window": { "offset": 0, "limit": 100 },
            "replaces": { "view": "v1", "anchor": id(0xb2).to_string() },
        });
        let params: ListOpenParams = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(params.kind, ListKind::Albums);
        assert_eq!(
            params.query.filter.as_ref().unwrap().predicate,
            Predicate::ByArtist {
                artist_id: id(0xa1)
            }
        );
        assert_eq!(serde_json::to_value(&params).unwrap(), wire);

        // Everything but the kind may be left out.
        let bare: ListOpenParams = serde_json::from_value(json!({ "kind": "artists" })).unwrap();
        assert_eq!(bare.query, Query::default());
        assert_eq!(
            serde_json::to_value(&bare).unwrap(),
            json!({ "kind": "artists", "query": {} })
        );
        assert!(serde_json::from_value::<ListOpenParams>(json!({ "kind": "playlists" })).is_err());
    }

    #[test]
    fn a_row_says_what_kind_it_is_and_a_gone_row_says_only_that() {
        let gone = serde_json::to_value(Row::Gone(GoneRow { gone: true })).unwrap();
        assert_eq!(gone, json!({ "type": "gone", "gone": true }));
        let tile = Row::Album(AlbumTile {
            album: AlbumRef::ReleaseGroup { id: id(1) },
            title: Some("Night".to_owned()),
            artist_credit: None,
            year: Some(1997),
            track_count: 12,
            duration_ms: 2_700_000,
            version_count: 2,
            art_id: None,
            quality: None,
        });
        let wire = serde_json::to_value(&tile).unwrap();
        assert_eq!(wire["type"], "album");
        assert_eq!(wire["ref"], json!({ "type": "release_group", "id": id(1) }));
        assert_eq!(wire["artId"], json!(null));
        assert_eq!(serde_json::from_value::<Row>(wire).unwrap(), tile);
    }

    #[test]
    fn every_kind_names_its_sorts_and_the_first_is_its_default() {
        let support = ListSupport::all();
        assert_eq!(support.len(), 3);
        assert_eq!(support[0].sorts, [SortKey::Name]);
        assert_eq!(support[1].sorts[0], SortKey::Artist);
        assert_eq!(support[2].predicates, [PredicateName::ByArtist]);
    }
}
