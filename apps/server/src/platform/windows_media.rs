//! The media keys and the system's media transport controls on Windows,
//! tied to the hidden window `serve` opens. The controls themselves are in
//! `ritornel_windows::media`; Windows delivers their requests on a thread
//! of its own, so the handler only forwards.

use std::path::Path;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::time::Duration;

use ritornel_engine::playback::{PlayerState, Status};
use ritornel_windows::media::{MediaControls, Playback, Request, Track};
use tokio::runtime::Handle;
use tracing::{debug, warn};

use crate::media_session::{Key, Keys};

/// The handle of the hidden window `serve` opened, which the controls are
/// tied to, or 0 before it is open. A process serves once.
static WINDOW: AtomicIsize = AtomicIsize::new(0);

/// Says which window the controls are to be tied to.
pub(crate) fn tie_to(window: isize) {
    WINDOW.store(window, Ordering::Release);
}

/// The system's controls, tied to the hidden window.
pub(crate) struct Controls(MediaControls);

impl Controls {
    /// Takes the controls, with each request passed to `keys`. Windows
    /// needs no runtime of ours.
    pub(crate) fn open(keys: Keys, _: &Handle) -> Option<Self> {
        let window = WINDOW.load(Ordering::Acquire);
        if window == 0 {
            warn!("the media controls have no window to be tied to");
            return None;
        }
        match MediaControls::open(window, move |request| keys(key_of(request))) {
            Ok(controls) => {
                debug!("the media controls are open");
                Some(Self(controls))
            }
            Err(error) => {
                warn!(%error, "the media controls could not be taken");
                None
            }
        }
    }

    /// Shows the track the player is on, with its cover.
    pub(crate) fn set_track(&mut self, state: &PlayerState, cover: Option<&str>) {
        let track = state.current.as_ref().and_then(|now| now.track.as_ref());
        let cover = cover.and_then(path_of);
        let shown = Track {
            title: track.and_then(|track| track.title.as_deref()),
            artist: track.and_then(|track| track.artist.as_deref()),
            album: track.and_then(|track| track.album.as_deref()),
            cover: cover.as_deref().map(Path::new),
            duration: state.duration_ms.map(millis),
        };
        if let Err(error) = self.0.set_track(&shown) {
            warn!(%error, "the media controls could not be told the track");
        }
    }

    /// Shows the status with the position.
    pub(crate) fn set_state(&mut self, state: &PlayerState) {
        let playback = match state.status {
            Status::Playing => Playback::Playing,
            Status::Paused => Playback::Paused,
            Status::Stopped => Playback::Stopped,
        };
        if let Err(error) = self.0.set_state(playback, millis(state.position_ms)) {
            warn!(%error, "the media controls could not be told the state");
        }
    }
}

/// The path a cover's file URL names (`media_session::file_url`), which
/// escapes every byte but a few: Windows loads a cover by its path.
fn path_of(url: &str) -> Option<String> {
    let mut escaped = url.strip_prefix("file://")?.bytes();
    let mut bytes = Vec::new();
    while let Some(byte) = escaped.next() {
        if byte != b'%' {
            bytes.push(byte);
            continue;
        }
        let hex = [escaped.next()?, escaped.next()?];
        bytes.push(u8::from_str_radix(std::str::from_utf8(&hex).ok()?, 16).ok()?);
    }
    String::from_utf8(bytes).ok()
}

fn key_of(request: Request) -> Key {
    match request {
        Request::Play => Key::Play,
        Request::Pause | Request::Stop => Key::Pause,
        Request::Next => Key::Next,
        Request::Previous => Key::Previous,
        Request::Position(at) => Key::Seek(u64::try_from(at.as_millis()).unwrap_or(u64::MAX)),
    }
}

fn millis(ms: i64) -> Duration {
    Duration::from_millis(u64::try_from(ms).unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_requests_windows_sends_are_the_players_commands() {
        assert_eq!(key_of(Request::Stop), Key::Pause);
        assert_eq!(
            key_of(Request::Position(Duration::from_secs(90))),
            Key::Seek(90_000)
        );
    }

    #[test]
    fn a_cover_is_loaded_by_the_path_its_url_names() {
        let path = r"C:\Users\Pat Q\AppData\Local\Ritornel\cache\covers\x-512.jpg";
        let url = crate::media_session::file_url(Path::new(path));
        assert_eq!(path_of(&url).as_deref(), Some(path));
        assert_eq!(path_of("https://example.com/x.jpg"), None);
    }
}
