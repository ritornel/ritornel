//! What a satellite's player reads of the library: the hub's, through the
//! link. A track's master is pulled whole from the hub into the satellite's
//! own cache before it plays, and kept there for the next time. The pull is
//! done off the player ([`HubCatalog::fetch`]), which answers meanwhile.
//! See "Hub and satellites" in docs/internals/linking.md.

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

/// The most tracks one `link.tracks` asks for.
const TRACKS_PER_CALL: usize = 500;

/// The most the cache of pulled masters holds, in bytes, unless the
/// `cache_size` setting says otherwise. The oldest go first once it holds
/// more.
pub(crate) const DEFAULT_CACHE_BYTES: u64 = 2 << 30;

/// The hub's library, as a satellite's player reads it.
#[derive(Debug)]
pub(crate) struct HubCatalog {
    uplink: Arc<Uplink>,
    /// Where pulled masters are kept.
    cache: PathBuf,
    /// The most it holds, in bytes.
    cache_bytes: u64,
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
    pub(crate) fn new(uplink: Arc<Uplink>, cache: PathBuf, cache_bytes: u64) -> Self {
        Self {
            uplink,
            cache,
            cache_bytes,
        }
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
        let name = format!("{}-{}", copy.source_item_id, copy.revision);
        if extension.is_empty() {
            self.cache.join(name)
        } else {
            self.cache.join(format!("{name}.{extension}"))
        }
    }

    /// The copies the hub offers of an appearance, best first: none while
    /// it does not answer.
    fn offered(&self, appearance: Id) -> Option<LinkCopies> {
        self.call::<LinkCopiesCall>(&LinkCopiesParams { appearance })
            .inspect_err(|error| warn!(%error, "a track's copies could not be read from the hub"))
            .ok()
    }

    /// Pulls the first of the hub's copies of an appearance that can be
    /// pulled into the cache, unless one is kept already. It may take as
    /// long as the master takes to cross the network, so it is done off
    /// the player.
    pub(crate) fn fetch(&self, appearance: Id) -> Result<(), Error> {
        let Some(LinkCopies { copies }) = self.offered(appearance) else {
            return Err(Error::Playback(
                "the hub did not say where this track is".to_owned(),
            ));
        };
        let mut last = Error::Playback("the hub has no copy of this track".to_owned());
        for copy in &copies {
            match self.pull(appearance, copy) {
                Ok(_) => return Ok(()),
                Err(error) => last = error,
            }
        }
        Err(last)
    }

    /// The master, pulled from the hub if it is not kept here already.
    fn pull(&self, appearance: Id, copy: &HubCopy) -> Result<PathBuf, Error> {
        let kept = self.kept(copy);
        if facts_of(&kept).is_some_and(|facts| facts.size_bytes == copy.size_bytes) {
            played_again(&kept);
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
                trim(&self.cache, self.cache_bytes, Some(&kept));
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
}

/// Marks a kept master as played now, so that it is the newest to keep.
fn played_again(kept: &Path) {
    let touched = std::fs::File::options()
        .append(true)
        .open(kept)
        .and_then(|file| file.set_modified(SystemTime::now()));
    if let Err(error) = touched {
        warn!(%error, "a kept master could not be marked as played");
    }
}

/// Lets the oldest masters kept in `cache` go until it holds no more than
/// `limit` bytes. It never lets go of `keep`, the one just pulled, nor of
/// the newest other, which is playing or about to, nor of one being
/// pulled.
fn trim(cache: &Path, limit: u64, keep: Option<&Path>) {
    let Ok(entries) = std::fs::read_dir(cache) else {
        return;
    };
    let mut kept: Vec<(SystemTime, u64, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let metadata = entry.metadata().ok().filter(std::fs::Metadata::is_file)?;
            Some((metadata.modified().ok()?, metadata.len(), entry.path()))
        })
        .collect();
    let mut total = kept
        .iter()
        .fold(0_u64, |total, (_, size, _)| total.saturating_add(*size));
    kept.sort();
    let pulling = |path: &Path| {
        path.extension()
            .is_some_and(|extension| extension == "part")
    };
    let newest = kept
        .iter()
        .rev()
        .find(|(_, _, path)| Some(path.as_path()) != keep && !pulling(path))
        .map(|(_, _, path)| path.clone());
    for (_, size, path) in kept {
        if Some(path.as_path()) == keep || Some(&path) == newest.as_ref() || pulling(&path) {
            continue;
        }
        if total <= limit {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(size);
        } else {
            warn!(path = %path.display(), "a kept master could not be let go");
        }
    }
}

/// Tidies the cache as the server starts, when no pull can be under way:
/// what a pull cut off left is let go, and the cache is brought within
/// `limit`, which may have been lowered since it was filled.
pub(crate) fn tidy(cache: &Path, limit: u64) {
    let Ok(entries) = std::fs::read_dir(cache) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "part")
            && let Err(error) = std::fs::remove_file(&path)
        {
            warn!(%error, path = %path.display(), "a master cut off as it was pulled could not be let go");
        }
    }
    trim(cache, limit, None);
}

impl Catalog for HubCatalog {
    /// What a track shows, or nothing while the hub does not answer: what
    /// plays from a copy kept here plays on, untitled, rather than stop.
    fn track(&self, appearance: Id) -> Result<Option<TrackDisplay>, Error> {
        match self.tracks(&[appearance]) {
            Ok(tracks) => Ok(tracks.into_iter().flatten().next()),
            Err(error) => {
                warn!(%error, "what a track shows could not be read from the hub");
                Ok(None)
            }
        }
    }

    /// Asked a few hundred at a time, so no answer outgrows a frame.
    fn tracks(&self, appearances: &[Id]) -> Result<Vec<Option<TrackDisplay>>, Error> {
        let mut shown = Vec::with_capacity(appearances.len());
        for chunk in appearances.chunks(TRACKS_PER_CALL) {
            let read = self.read(&LinkTracksParams {
                appearances: chunk.to_vec(),
                release: None,
            })?;
            if read.tracks.len() != chunk.len() {
                return Err(Error::Playback(
                    "the hub answered for other tracks than were asked".to_owned(),
                ));
            }
            shown.extend(read.tracks.iter().map(|track| track.as_ref().map(display)));
        }
        Ok(shown)
    }

    /// The first of the hub's copies that is kept here, or, where none
    /// is, the first as one to be fetched. The hub has chosen their order.
    /// While the hub does not answer there is none: the track is not
    /// played, and what plays now plays on.
    fn copies(&self, appearance: Id, _server: Id) -> Result<Vec<Copy>, Error> {
        let Some(LinkCopies { copies }) = self.offered(appearance) else {
            return Ok(Vec::new());
        };
        for copy in &copies {
            let kept = self.kept(copy);
            if !facts_of(&kept).is_some_and(|facts| facts.size_bytes == copy.size_bytes) {
                continue;
            }
            // Marked first, so the facts it plays under are the file's as
            // it now is.
            played_again(&kept);
            if let Some(facts) = facts_of(&kept) {
                return Ok(vec![Copy::fetched(
                    copy.source_item_id,
                    copy.revision,
                    copy.asset_id,
                    copy.duration_ms,
                    kept,
                    facts,
                )]);
            }
        }
        Ok(copies
            .first()
            .map(|copy| {
                Copy::elsewhere(
                    copy.source_item_id,
                    copy.revision,
                    copy.asset_id,
                    copy.duration_ms,
                )
            })
            .into_iter()
            .collect())
    }

    fn first_listener(&self) -> Result<Option<Id>, Error> {
        Ok(self.read(&LinkTracksParams::default())?.listener)
    }

    fn unknown_appearance(&self, appearances: &[Id]) -> Result<Option<Id>, Error> {
        let shown = self.tracks(appearances)?;
        Ok(appearances
            .iter()
            .zip(shown.iter())
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

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use super::{tidy, trim};
    use crate::testing::Scratch;

    #[test]
    fn the_oldest_masters_go_until_the_cache_fits_but_never_the_newest_two_or_one_being_pulled() {
        let scratch = Scratch::new();
        let cache = scratch.path().join("audio");
        std::fs::create_dir_all(&cache).unwrap();
        let start = SystemTime::now() - Duration::from_secs(3_600);
        // Oldest first: one being pulled, three kept, and the one just
        // pulled, each of 100 bytes.
        let names = ["e.part", "a.flac", "b.flac", "c.flac", "d.flac"];
        for (at, name) in names.iter().enumerate() {
            let file = std::fs::File::create(cache.join(name)).unwrap();
            file.set_len(100).unwrap();
            let at = start + Duration::from_secs(60 * u64::try_from(at).unwrap());
            file.set_modified(at).unwrap();
        }
        // The one just pulled is the oldest of all, as a file whose time was
        // set by a clock that was wrong could be.
        let keep = cache.join("d.flac");
        std::fs::File::options()
            .append(true)
            .open(&keep)
            .unwrap()
            .set_modified(start - Duration::from_secs(60))
            .unwrap();

        let left = || {
            let mut left: Vec<_> = std::fs::read_dir(&cache)
                .unwrap()
                .map(|entry| entry.unwrap().file_name().into_string().unwrap())
                .collect();
            left.sort();
            left
        };
        // The newest besides it, which plays, stays too, and so does the
        // one being pulled.
        trim(&cache, 300, Some(&keep));
        assert_eq!(left(), ["c.flac", "d.flac", "e.part"]);
        // A cache already within its limit loses nothing.
        trim(&cache, 300, Some(&keep));
        assert_eq!(left(), ["c.flac", "d.flac", "e.part"]);

        // As the server starts, a pull cut off is let go, and a limit
        // lowered since is kept to, sparing the newest.
        tidy(&cache, 100);
        assert_eq!(left(), ["c.flac"]);
    }
}
