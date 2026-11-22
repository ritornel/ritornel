//! Now Playing and the media keys on macOS, through souvlaki. Handlers run
//! on the main thread's run loop, so they only forward.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::time::Duration;

use objc2_core_foundation::CFRunLoop;
use ritornel_engine::playback::{PlayerState, Status};
use souvlaki::{
    MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig,
};
use tokio::runtime::Handle;
use tracing::{debug, warn};

use crate::media_session::{Key, Keys};

/// souvlaki's controls, registered with the system.
pub(crate) struct Controls(MediaControls);

impl Controls {
    /// Registers with the system, with each key it delivers passed to
    /// `keys`. macOS needs no runtime of ours.
    pub(crate) fn open(keys: Keys, _: &Handle) -> Option<Self> {
        let config = PlatformConfig {
            display_name: "Ritornel",
            dbus_name: "ritornel",
            hwnd: None,
        };
        let mut controls = match MediaControls::new(config) {
            Ok(controls) => controls,
            Err(error) => {
                warn!(%error, "the media session could not be made");
                return None;
            }
        };
        let attached = controls.attach(move |event| {
            if let Some(key) = key_of(event) {
                keys(key);
            }
        });
        if let Err(error) = attached {
            warn!(%error, "the media keys could not be taken");
            return None;
        }
        debug!("the media session is open");
        Some(Self(controls))
    }

    /// Shows the track the player is on, with its cover.
    pub(crate) fn set_track(&mut self, state: &PlayerState, cover: Option<&str>) {
        let track = state.current.as_ref().and_then(|now| now.track.as_ref());
        let metadata = MediaMetadata {
            title: track.and_then(|track| track.title.as_deref()),
            album: track.and_then(|track| track.album.as_deref()),
            artist: track.and_then(|track| track.artist.as_deref()),
            cover_url: cover,
            duration: state.duration_ms.map(millis),
        };
        if let Err(error) = self.0.set_metadata(metadata) {
            warn!(%error, "Now Playing could not be told the track");
        }
    }

    /// Shows the status with the position, from which macOS decides who
    /// has the keys.
    pub(crate) fn set_state(&mut self, state: &PlayerState) {
        let progress = Some(MediaPosition(millis(state.position_ms)));
        let playback = match state.status {
            Status::Playing => MediaPlayback::Playing { progress },
            Status::Paused => MediaPlayback::Paused { progress },
            Status::Stopped => MediaPlayback::Stopped,
        };
        if let Err(error) = self.0.set_playback(playback) {
            warn!(%error, "Now Playing could not be told the state");
        }
    }
}

fn key_of(event: MediaControlEvent) -> Option<Key> {
    Some(match event {
        MediaControlEvent::Toggle => Key::Toggle,
        MediaControlEvent::Play => Key::Play,
        MediaControlEvent::Pause | MediaControlEvent::Stop => Key::Pause,
        MediaControlEvent::Next => Key::Next,
        MediaControlEvent::Previous => Key::Previous,
        MediaControlEvent::SetPosition(MediaPosition(at)) => {
            Key::Seek(u64::try_from(at.as_millis()).unwrap_or(u64::MAX))
        }
        _ => return None,
    })
}

fn millis(ms: i64) -> Duration {
    Duration::from_millis(u64::try_from(ms).unwrap_or(0))
}

/// Blocks in the run loop until `stopped` gives a reason to stop, when
/// called on the main thread: macOS delivers the media keys only to a
/// process whose main thread runs one. Anywhere else it simply waits.
pub(crate) fn wait<T: Send + 'static>(stopped: Receiver<T>) -> Option<T> {
    let on_main = match (CFRunLoop::current(), CFRunLoop::main()) {
        (Some(current), Some(main)) => std::ptr::eq(&*current, &*main),
        _ => false,
    };
    if !on_main {
        return stopped.recv().ok();
    }
    let done = Arc::new(AtomicBool::new(false));
    let left = Arc::new(AtomicBool::new(false));
    let waiter = {
        let (done, left) = (Arc::clone(&done), Arc::clone(&left));
        std::thread::Builder::new()
            .name("ritornel-stop".to_owned())
            .spawn(move || {
                let reason = stopped.recv().ok();
                done.store(true, Ordering::SeqCst);
                // A stop can come before the loop runs, so it is sent until
                // the main thread has left it.
                while !left.load(Ordering::SeqCst) {
                    if let Some(main) = CFRunLoop::main() {
                        main.stop();
                        main.wake_up();
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                reason
            })
    };
    let Ok(waiter) = waiter else {
        warn!("the run loop could not be watched; the media keys will not arrive");
        return None;
    };
    while !done.load(Ordering::SeqCst) {
        CFRunLoop::run();
        // It returns at once while nothing is registered with it.
        if !done.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    left.store(true, Ordering::SeqCst);
    waiter.join().ok().flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_keys_macos_sends_are_the_players_commands() {
        assert_eq!(key_of(MediaControlEvent::Toggle), Some(Key::Toggle));
        assert_eq!(
            key_of(MediaControlEvent::SetPosition(MediaPosition(
                Duration::from_secs(90)
            ))),
            Some(Key::Seek(90_000))
        );
        assert_eq!(key_of(MediaControlEvent::Raise), None);
    }
}
