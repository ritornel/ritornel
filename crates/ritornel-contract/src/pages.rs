//! The two pages about one thing that M1 has: an artist's and an album's.
//! See "What a client sees" and the M1 "Browse" row of the minimum call
//! list in docs/design/contract.md.
//!
//! - `artist.get` takes an [`ArtistGetParams`] and returns an
//!   [`ArtistPage`]: the artist's row and the colour of their picture.
//!   Their records are a list, read through a view: `list.open` of
//!   `albums`, filtered [`Predicate::WithArtist`](crate::Predicate) and
//!   ordered [`SortKey::Discography`](crate::SortKey), whose sections are
//!   the shelves; their songs on no record are `list.open` of `songs`,
//!   filtered by artist and [`Predicate::NoRecord`](crate::Predicate).
//! - `album.get` takes an [`AlbumGetParams`] and returns an [`AlbumPage`]:
//!   one release with its tracks, in disc and track order, and the other
//!   versions of its group.
//!
//! An ID that names nothing the library has fails with `not_found`.

use serde::{Deserialize, Serialize};

use crate::{AlbumRef, AlbumTile, ArtistRow, Id, Quality, Shelf, SongRow};

/// The colour a picture is mostly made of: the hue it has most of,
/// counting each pixel by how coloured it is, and how strong the colour
/// is there. It is a hue and not a colour to paint with: a client sets its
/// lightness for its own palette, so that it reads on a dark window and a
/// light one alike. `null` where it is given is a picture that is close to
/// grey all over, or one that could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ArtColor {
    /// The hue, in degrees from 0 to 359.
    pub hue: u16,
    /// How strong the colour is where it is that hue, from 0 to 100.
    pub saturation: u8,
}

/// The params of `artist.get`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ArtistGetParams {
    /// The artist.
    pub artist_id: Id,
}

/// The result of `artist.get`: one artist, as the Artists list has them,
/// and the colour of their picture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ArtistPage {
    /// The artist's row. Their `artId` is what their page is drawn from.
    #[serde(flatten)]
    pub artist: ArtistRow,
    /// The colour of that picture. `null` when it has none, or it is close
    /// to grey.
    pub art_color: Option<ArtColor>,
    /// The library revision this was read at.
    pub library_revision: u64,
}

/// The params of `album.get`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct AlbumGetParams {
    /// The album: a release group, which gives the release the group is
    /// set to show, or one release.
    #[serde(rename = "ref")]
    pub album: AlbumRef,
}

/// What kind of record a release group is, as a release-type tag writes it
/// (MusicBrainz's primary type).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum RecordType {
    /// An album.
    Album,
    /// An EP.
    Ep,
    /// A single.
    Single,
    /// A broadcast.
    Broadcast,
    /// Something else.
    Other,
}

/// One version of a record: a release of its group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct AlbumVersion {
    /// The release, for `album.get`.
    pub release_id: Id,
    /// Its title.
    pub title: Option<String>,
    /// The year it came out.
    pub year: Option<i64>,
    /// How many tracks it has.
    pub track_count: u64,
    /// The best copy across its tracks. `null` when none has a copy the
    /// library has read.
    pub quality: Option<Quality>,
    /// Whether it is the one the group is set to show.
    pub shown: bool,
}

/// The result of `album.get`: one release, with what an album's page
/// shows of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct AlbumPage {
    /// What an Albums tile says of it. Its `ref` is what was asked for: a
    /// release asked for by an ID it was merged from is named by the one
    /// it is now. Its `year` is the year the record first came out, or for
    /// one release asked for by its own ID, the year that release did.
    #[serde(flatten)]
    pub tile: AlbumTile,
    /// The release shown: for a group, the one the group is set to show.
    pub release_id: Id,
    /// The release group it is a version of.
    pub release_group_id: Id,
    /// What kind of record it is, where that is known.
    pub record_type: Option<RecordType>,
    /// The shelf its artist's discography files it on.
    pub shelf: Shelf,
    /// Its tracks, in disc and track order.
    pub tracks: Vec<SongRow>,
    /// The other versions of the record: every other release of its group,
    /// oldest first.
    pub versions: Vec<AlbumVersion>,
    /// The colour of its cover. `null` when it has none, or it is close to
    /// grey.
    pub art_color: Option<ArtColor>,
    /// The library revision this was read at.
    pub library_revision: u64,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn an_album_page_is_its_tile_with_its_parts() {
        let id = |byte: u8| Id::from_bytes([byte; 16]);
        let page = AlbumPage {
            tile: AlbumTile {
                album: AlbumRef::ReleaseGroup { id: id(1) },
                title: Some("Night".to_owned()),
                artist_credit: None,
                year: Some(1997),
                track_count: 0,
                duration_ms: 0,
                version_count: 2,
                art_id: None,
                quality: None,
            },
            release_id: id(2),
            release_group_id: id(1),
            record_type: None,
            shelf: Shelf::Albums,
            tracks: Vec::new(),
            versions: vec![AlbumVersion {
                release_id: id(3),
                title: Some("Night (Remastered)".to_owned()),
                year: Some(2011),
                track_count: 12,
                quality: None,
                shown: false,
            }],
            art_color: Some(ArtColor {
                hue: 212,
                saturation: 40,
            }),
            library_revision: 9,
        };
        let wire = serde_json::to_value(&page).unwrap();
        assert_eq!(wire["ref"], json!({ "type": "release_group", "id": id(1) }));
        assert_eq!(wire["releaseId"], json!(id(2)));
        assert_eq!(wire["recordType"], json!(null));
        assert_eq!(wire["shelf"], "albums");
        assert_eq!(wire["artColor"], json!({ "hue": 212, "saturation": 40 }));
        assert_eq!(wire["versions"][0]["shown"], false);
        assert_eq!(serde_json::from_value::<AlbumPage>(wire).unwrap(), page);
        let params: AlbumGetParams =
            serde_json::from_value(json!({ "ref": { "type": "release", "id": id(4) } })).unwrap();
        assert_eq!(params.album, AlbumRef::Release { id: id(4) });
    }
}
