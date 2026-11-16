//! `artist.get` and `album.get`: the reads of an artist's page and an
//! album's that are one entity each. Their lists are views. See
//! `ritornel_contract`'s `pages` module.

use ritornel_contract::{
    AlbumGetParams, AlbumPage, ArtColor, ArtistGetParams, ArtistPage, ErrorCode, Id, RpcError,
};
use ritornel_engine::LibraryStore;
use tracing::debug;

use super::{Core, Stores};

fn failed(error: impl std::fmt::Display) -> RpcError {
    RpcError::new(ErrorCode::Internal, error.to_string())
}

fn not_found(what: &str) -> RpcError {
    RpcError::new(
        ErrorCode::NotFound,
        format!("the library has no such {what}"),
    )
}

impl Core {
    /// The colour of a picture: from the cover cache, or else worked out
    /// now from the picture and kept there. A picture that cannot be read
    /// has none: the page is drawn without it.
    fn art_color(
        &self,
        stores: &Stores,
        library: &LibraryStore,
        art: Option<Id>,
    ) -> Option<ArtColor> {
        let art = art?;
        if let Some(known) = self.covers.cached_color(art) {
            return known;
        }
        let started = std::time::Instant::now();
        let color = self.covers.color(library, &stores.server, art);
        debug!(
            art = %art,
            ms = started.elapsed().as_secs_f64() * 1000.0,
            "worked out a cover's colour"
        );
        color.ok().flatten()
    }

    pub(super) fn artist_get(
        &self,
        stores: &Stores,
        params: &ArtistGetParams,
    ) -> Result<ArtistPage, RpcError> {
        Self::library(stores, |library| {
            let (artist, revision) = library
                .artist_row(params.artist_id)
                .map_err(failed)?
                .ok_or_else(|| not_found("artist"))?;
            let art_color = self.art_color(stores, library, artist.art_id);
            Ok(ArtistPage {
                artist,
                art_color,
                library_revision: revision,
            })
        })
    }

    pub(super) fn album_get(
        &self,
        stores: &Stores,
        params: &AlbumGetParams,
    ) -> Result<AlbumPage, RpcError> {
        Self::library(stores, |library| {
            let mut page = library
                .album_page(params.album)
                .map_err(failed)?
                .ok_or_else(|| not_found("album"))?;
            page.art_color = self.art_color(stores, library, page.tile.art_id);
            Ok(page)
        })
    }
}
