//! What a satellite's player reads of the library: the hub's, through the
//! link. A track's master is pulled whole from the hub into the satellite's
//! own cache before it plays, and kept there for the next time. See "The
//! link between the hub and a satellite" and "When the hub is unreachable"
//! in docs/design/linking.md.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use ritornel_contract::methods::{LinkCopiesCall, LinkTracksCall, Method};
use ritornel_contract::{
    HubCopy, Id, LinkCopies, LinkCopiesParams, LinkTracks, LinkTracksParams, RpcError,
    TrackSummary, route,
};
use ritornel_engine::Error;
use ritornel_engine::playback::{Catalog, Copy, FileFacts, TrackDisplay};
use tracing::{info, warn};

use crate::uplink::Uplink;

/// The most the cache of pulled masters holds, in bytes. The oldest go
/// first once it holds more.
const CACHE_BYTES: u64 = 2 << 30;

/// The hub's library, as a satellite's player reads it.
#[derive(Debug)]
pub(crate) struct HubCatalog {
    uplink: Arc<Uplink>,
    /// Where pulled masters are kept.
    cache: PathBuf,
}

/// A call the hub refused or could not be asked, as the player reports it.
fn failed(error: &RpcError) -> Error {
    Error::Playback(error.message.clone())
}

/// A track as the hub showed it.
fn display(summary: &TrackSummary) -> TrackDisplay {
    let mut track = TrackDisplay::new(summary.appearance_id);
    track.recording = summary.recording_id;
    track.release = summary.release_id;
    track.title.clone_from(&summary.title);
    track.artist.clone_from(&summary.artist);
    track.album.clone_from(&summary.album);
    track.disc = summary.disc_number.map(i64::from);
    track.track = summary.track_number.map(i64::from);
    track.duration_ms = summary.duration_ms.and_then(|ms| i64::try_from(ms).ok());
    track.art = summary.art_id;
    track.rating = summary.rating.map(i64::from);
    track.loved = summary.loved;
    track
}

/// What a file now is: its size and modification time, as the player
/// checks them.
fn facts_of(path: &Path) -> Option<FileFacts> {
    Some(FileFacts::of(&std::fs::metadata(path).ok()?))
}

impl HubCatalog {
    pub(crate) fn new(uplink: Arc<Uplink>, cache: PathBuf) -> Self {
        Self { uplink, cache }
    }

    fn call<M: Method>(&self, params: &M::Params) -> Result<M::Result, Error> {
        let params = serde_json::to_value(params)?;
        let answer = self
            .uplink
            .call(M::NAME, params)
            .map_err(|error| failed(&error))?;
        Ok(serde_json::from_value(answer)?)
    }

    fn read(&self, params: &LinkTracksParams) -> Result<LinkTracks, Error> {
        self.call::<LinkTracksCall>(params)
    }

    /// Where a master is kept once pulled: one file per revision, named so
    /// that a decoder that goes by the extension finds it.
    fn kept(&self, copy: &HubCopy) -> PathBuf {
        let extension: String = copy
            .extension
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .take(8)
            .collect();
        self.cache.join(format!(
            "{}-{}.{extension}",
            copy.source_item_id, copy.revision
        ))
    }

    /// The master, pulled from the hub if it is not kept here already.
    fn pull(&self, appearance: Id, copy: &HubCopy) -> Result<PathBuf, Error> {
        let kept = self.kept(copy);
        if facts_of(&kept).is_some_and(|facts| facts.size_bytes == copy.size_bytes) {
            return Ok(kept);
        }
        let route_to = self.uplink.route().ok_or(Error::Playback(
            "the link has no route to the hub".to_owned(),
        ))?;
        std::fs::create_dir_all(&self.cache).map_err(Error::Io)?;
        let part = kept.with_extension("part");
        let path = format!(
            "{}/{appearance}/{}/{}",
            route::AUDIO,
            copy.source_item_id,
            copy.revision
        );
        let credential = self.uplink.credential();
        let started = std::time::Instant::now();
        let pulled = self.uplink.block_on(ritornel_client::download(
            route_to,
            &path,
            &credential,
            &part,
            copy.size_bytes,
        ));
        match pulled {
            Ok(bytes) if bytes == copy.size_bytes => {
                std::fs::rename(&part, &kept)
                    .inspect_err(|_| {
                        let _ = std::fs::remove_file(&part);
                    })
                    .map_err(Error::Io)?;
                info!(
                    source_item = %copy.source_item_id,
                    bytes,
                    ms = started.elapsed().as_millis(),
                    "a master was pulled from the hub"
                );
                self.trim();
                Ok(kept)
            }
            Ok(bytes) => {
                let _ = std::fs::remove_file(&part);
                Err(Error::Playback(format!(
                    "the hub sent {bytes} bytes of a master of {}",
                    copy.size_bytes
                )))
            }
            Err(error) => {
                let _ = std::fs::remove_file(&part);
                Err(Error::Playback(format!(
                    "the master could not be pulled from the hub: {error}"
                )))
            }
        }
    }

    /// Lets the oldest kept masters go until the cache holds no more than
    /// [`CACHE_BYTES`].
    fn trim(&self) {
        let Ok(entries) = std::fs::read_dir(&self.cache) else {
            return;
        };
        let mut kept: Vec<(SystemTime, u64, PathBuf)> = entries
            .flatten()
            .filter_map(|entry| {
                let metadata = entry.metadata().ok()?;
                Some((metadata.modified().ok()?, metadata.len(), entry.path()))
            })
            .collect();
        let mut total: u64 = kept.iter().map(|(_, size, _)| size).sum();
        kept.sort();
        for (_, size, path) in kept {
            if total <= CACHE_BYTES {
                break;
            }
            if std::fs::remove_file(&path).is_ok() {
                total = total.saturating_sub(size);
            } else {
                warn!(path = %path.display(), "a kept master could not be let go");
            }
        }
    }
}

impl Catalog for HubCatalog {
    fn track(&self, appearance: Id) -> Result<Option<TrackDisplay>, Error> {
        Ok(self.tracks(&[appearance])?.into_iter().flatten().next())
    }

    fn tracks(&self, appearances: &[Id]) -> Result<Vec<Option<TrackDisplay>>, Error> {
        let read = self.read(&LinkTracksParams {
            appearances: appearances.to_vec(),
            release: None,
        })?;
        Ok(read
            .tracks
            .iter()
            .map(|track| track.as_ref().map(display))
            .collect())
    }

    /// The first of the hub's copies that can be pulled, kept here. The
    /// hub has chosen their order.
    fn copies(&self, appearance: Id, _server: Id) -> Result<Vec<Copy>, Error> {
        let offered = self.call::<LinkCopiesCall>(&LinkCopiesParams { appearance })?;
        let LinkCopies { copies } = offered;
        let mut last = None;
        for copy in copies {
            match self.pull(appearance, &copy) {
                Ok(path) => {
                    let Some(facts) = facts_of(&path) else {
                        continue;
                    };
                    return Ok(vec![Copy::fetched(
                        copy.source_item_id,
                        copy.revision,
                        copy.asset_id,
                        copy.duration_ms,
                        path,
                        facts,
                    )]);
                }
                Err(error) => last = Some(error),
            }
        }
        match last {
            Some(error) => Err(error),
            None => Ok(Vec::new()),
        }
    }

    fn first_listener(&self) -> Result<Option<Id>, Error> {
        Ok(self.read(&LinkTracksParams::default())?.listener)
    }

    fn unknown_appearance(&self, appearances: &[Id]) -> Result<Option<Id>, Error> {
        let read = self.read(&LinkTracksParams {
            appearances: appearances.to_vec(),
            release: None,
        })?;
        Ok(appearances
            .iter()
            .zip(read.tracks.iter())
            .find(|(_, track)| track.is_none())
            .map(|(appearance, _)| *appearance))
    }

    fn said_again(&self, track: &mut TrackDisplay) -> Result<(), Error> {
        if let Some(again) = self.track(track.appearance)? {
            track.rating = again.rating;
            track.loved = again.loved;
        }
        Ok(())
    }

    fn release_tracks(&self, release: Id) -> Result<Option<Vec<TrackDisplay>>, Error> {
        let read = self.read(&LinkTracksParams {
            appearances: Vec::new(),
            release: Some(release),
        })?;
        Ok(read
            .release
            .map(|tracks| tracks.iter().map(display).collect()))
    }
}
