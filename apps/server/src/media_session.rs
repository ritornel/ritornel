//! The system's media session: Now Playing and the media keys, held by the
//! server process so they work with no window open. See decision 23 and
//! "OS integration" in docs/design/playback.md, and spikes/now-playing.
//!
//! Only macOS has one in M1. A thread of its own owns souvlaki's controls:
//! it shows the player's state, and passes each key on to the player by
//! the path a contract call takes. The main thread blocks in the run loop,
//! which is all macOS needs to deliver the keys ([`wait`]).

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Weak, mpsc};
use std::time::Duration;

use objc2_core_foundation::CFRunLoop;
use ritornel_engine::Id;
use ritornel_engine::playback::{PlayerState, Status};
use souvlaki::{
    MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig,
};
use tracing::{debug, warn};

use crate::dispatch::Core;

/// What a media key asks of the player.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Key {
    Toggle,
    Play,
    Pause,
    Next,
    Previous,
    /// To this position, in milliseconds.
    Seek(u64),
}

enum Message {
    State(Box<PlayerState>),
    Key(Key),
    Stop,
}

/// The session's thread. Dropping it stops the thread.
#[derive(Debug)]
pub(crate) struct MediaSession {
    send: Sender<Message>,
}

impl MediaSession {
    /// Starts the session for `core`. Nothing is shown to the system until
    /// the player first plays, so a server restored paused leaves the keys
    /// with whatever played last, as macOS does for every player.
    pub(crate) fn start(core: Weak<Core>) -> std::io::Result<Self> {
        let (send, receive) = mpsc::channel();
        let keys = send.clone();
        std::thread::Builder::new()
            .name("ritornel-media-session".to_owned())
            .spawn(move || run(&core, &receive, &keys))?;
        Ok(Self { send })
    }

    /// Shows the player's state.
    pub(crate) fn show(&self, state: &PlayerState) {
        let _ = self.send.send(Message::State(Box::new(state.clone())));
    }
}

impl Drop for MediaSession {
    fn drop(&mut self) {
        let _ = self.send.send(Message::Stop);
    }
}

fn run(core: &Weak<Core>, receive: &Receiver<Message>, keys: &Sender<Message>) {
    let mut controls: Option<MediaControls> = None;
    // The entry and track shown, so the metadata is sent once a track.
    let mut shown: Option<(Id, Option<Id>)> = None;
    while let Ok(message) = receive.recv() {
        match message {
            Message::Stop => break,
            Message::Key(key) => {
                debug!(?key, "a media key");
                if let Some(core) = core.upgrade() {
                    core.media_key(key);
                }
            }
            Message::State(state) => {
                if controls.is_none() {
                    if state.status != Status::Playing {
                        continue;
                    }
                    controls = open(keys.clone());
                }
                if let Some(controls) = &mut controls {
                    show(core, controls, &state, &mut shown);
                }
            }
        }
    }
}

/// Registers with the system, with each key it delivers passed back here.
fn open(keys: Sender<Message>) -> Option<MediaControls> {
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
    // Handlers run on the main thread's run loop, so they only forward.
    let attached = controls.attach(move |event| {
        if let Some(key) = key_of(event) {
            let _ = keys.send(Message::Key(key));
        }
    });
    if let Err(error) = attached {
        warn!(%error, "the media keys could not be taken");
        return None;
    }
    debug!("the media session is open");
    Some(controls)
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

/// Sends the state: the track's metadata when the track has changed, and
/// always the status with the position, from which macOS decides who has
/// the keys.
fn show(
    core: &Weak<Core>,
    controls: &mut MediaControls,
    state: &PlayerState,
    shown: &mut Option<(Id, Option<Id>)>,
) {
    let now = state.current.as_ref();
    let showing = now.map(|now| (now.entry_id, now.track.as_ref().map(|t| t.appearance)));
    if showing != *shown {
        *shown = showing;
        let track = now.and_then(|now| now.track.as_ref());
        let cover = track
            .and_then(|track| core.upgrade()?.now_playing_cover(track.appearance))
            .map(|path| file_url(&path));
        let metadata = MediaMetadata {
            title: track.and_then(|track| track.title.as_deref()),
            album: track.and_then(|track| track.album.as_deref()),
            artist: track.and_then(|track| track.artist.as_deref()),
            cover_url: cover.as_deref(),
            duration: state.duration_ms.map(millis),
        };
        if let Err(error) = controls.set_metadata(metadata) {
            warn!(%error, "Now Playing could not be told the track");
        }
    }
    let progress = Some(MediaPosition(millis(state.position_ms)));
    let playback = match state.status {
        Status::Playing => MediaPlayback::Playing { progress },
        Status::Paused => MediaPlayback::Paused { progress },
        Status::Stopped => MediaPlayback::Stopped,
    };
    if let Err(error) = controls.set_playback(playback) {
        warn!(%error, "Now Playing could not be told the state");
    }
}

/// A file URL for a path, escaped: the data directory has a space in it.
fn file_url(path: &Path) -> String {
    let mut url = "file://".to_owned();
    for byte in path.to_string_lossy().bytes() {
        if byte.is_ascii_alphanumeric() || b"/-._~".contains(&byte) {
            url.push(char::from(byte));
        } else {
            url.push_str(&format!("%{byte:02X}"));
        }
    }
    url
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
    fn a_cover_path_is_escaped_into_a_file_url() {
        let url = file_url(Path::new(
            "/Users/a/Library/Application Support/Ritornel/cache/x-512.jpg",
        ));
        assert_eq!(
            url,
            "file:///Users/a/Library/Application%20Support/Ritornel/cache/x-512.jpg"
        );
    }

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

    #[test]
    fn off_the_main_thread_waiting_is_only_waiting() {
        let (send, receive) = mpsc::channel();
        send.send(7).unwrap();
        assert_eq!(wait(receive), Some(7));
    }
}
