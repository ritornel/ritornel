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
/// tied to, or 0 while there is none.
static WINDOW: AtomicIsize = AtomicIsize::new(0);

/// Says which window the controls are to be tied to: 0 for none, as the
/// window closes.
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
    pub(crate) fn set_track(&mut self, state: &PlayerState, cover: Option<&Path>) {
        let track = state.current.as_ref().and_then(|now| now.track.as_ref());
        let shown = Track {
            title: track.and_then(|track| track.title.as_deref()),
            artist: track.and_then(|track| track.artist.as_deref()),
            album: track.and_then(|track| track.album.as_deref()),
            cover,
        };
        if let Err(error) = self.0.set_track(&shown) {
            warn!(%error, "the media controls could not be told the track");
        }
    }

    /// Shows the status, the position, and the track's length.
    pub(crate) fn set_state(&mut self, state: &PlayerState) {
        let playback = match state.status {
            // About to make sound, so the keys pause it.
            Status::Playing | Status::Buffering => Playback::Playing,
            Status::Paused => Playback::Paused,
            Status::Stopped => Playback::Stopped,
        };
        let length = state.duration_ms.map(millis);
        if let Err(error) = self
            .0
            .set_state(playback, millis(state.position_ms), length)
        {
            warn!(%error, "the media controls could not be told the state");
        }
    }
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
        for (request, key) in [
            (Request::Play, Key::Play),
            (Request::Pause, Key::Pause),
            (Request::Stop, Key::Pause),
            (Request::Next, Key::Next),
            (Request::Previous, Key::Previous),
            (
                Request::Position(Duration::from_secs(90)),
                Key::Seek(90_000),
            ),
        ] {
            assert_eq!(key_of(request), key, "{request:?}");
        }
        assert_eq!(millis(-5), Duration::ZERO);
    }
}
