//! The system's media session: Now Playing and the media keys, held by the
//! server process so they work with no window open. See decision 23 and
//! "OS integration" in docs/design/playback.md, and spikes/now-playing.
//!
//! A thread of its own owns the platform's controls: it shows the player's
//! state, and passes each key on to the player by the path a contract call
//! takes. The platform is behind [`Controls`]:
//!
//! - macOS: souvlaki's Now Playing. The main thread blocks in the run loop,
//!   which is all macOS needs to deliver the keys ([`wait`]).
//! - Linux: MPRIS on the session bus, served from the server's own async
//!   runtime ([`mpris`]). A server with no session bus has none, quietly.
//! - Windows: none yet. The system's media transport controls are to be
//!   tied to the hidden window `serve` opens there.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex, PoisonError, Weak, mpsc};
use std::thread::JoinHandle;

use ritornel_engine::Id;
use ritornel_engine::playback::{PlayerState, Status};
use tokio::runtime::Handle;
use tracing::debug;

use crate::dispatch::Core;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "linux")]
mod mpris;

#[cfg(target_os = "macos")]
use self::macos::Controls;
#[cfg(target_os = "macos")]
pub(crate) use self::macos::wait;
#[cfg(target_os = "linux")]
use self::mpris::Controls;

/// What a media key asks of the player.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Key {
    Toggle,
    Play,
    Pause,
    Next,
    Previous,
    /// To this position, in milliseconds, in whichever entry the player is
    /// on. Only macOS's seeks name no entry.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Seek(u64),
    /// To this position, in milliseconds, in this entry only: nothing if
    /// the player has moved on from it. Only MPRIS names the entry.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    SeekIn {
        entry: Id,
        position_ms: u64,
    },
    /// The player's own volume, 0 to 1. Only MPRIS sets it.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Volume(f64),
}

/// Where the platform's controls send each key: back to the session's
/// thread, which carries it out.
type Keys = Arc<dyn Fn(Key) + Send + Sync>;

enum Message {
    State(Box<PlayerState>),
    Key(Key),
    Stop,
}

/// The session's thread. Dropping it, or [`stop`](Self::stop), stops the
/// thread.
#[derive(Debug)]
pub(crate) struct MediaSession {
    send: Sender<Message>,
    /// Set as the session stops, so that the thread does not work through
    /// what is queued before the stop, opening the controls again.
    stopping: Arc<AtomicBool>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl MediaSession {
    /// Starts the session for `core`, with `runtime` for a platform that
    /// serves the session from it. Nothing is shown to the system until
    /// the player first plays, so a server restored paused leaves the keys
    /// with whatever played last, as macOS does for every player.
    pub(crate) fn start(core: Weak<Core>, runtime: Handle) -> std::io::Result<Self> {
        let (send, receive) = mpsc::channel();
        let keys = send.clone();
        let stopping = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&stopping);
        let thread = std::thread::Builder::new()
            .name("ritornel-media-session".to_owned())
            .spawn(move || run(&core, &receive, &keys, &runtime, &stop))?;
        Ok(Self {
            send,
            stopping,
            thread: Mutex::new(Some(thread)),
        })
    }

    /// Shows the player's state.
    pub(crate) fn show(&self, state: &PlayerState) {
        let _ = self.send.send(Message::State(Box::new(state.clone())));
    }

    /// Stops the thread and waits for it, which takes the player off the
    /// session bus. The server does this while its runtime still runs,
    /// on Linux only (`Server::stop`). The wait is at most one attempt to
    /// connect, which is bounded.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub(crate) fn stop(&self) {
        self.stopping.store(true, Ordering::Release);
        let _ = self.send.send(Message::Stop);
        let thread = self
            .thread
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(thread) = thread {
            let _ = thread.join();
        }
    }
}

impl Drop for MediaSession {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        let _ = self.send.send(Message::Stop);
    }
}

fn run(
    core: &Weak<Core>,
    receive: &Receiver<Message>,
    keys: &Sender<Message>,
    runtime: &Handle,
    stopping: &AtomicBool,
) {
    let mut controls: Option<Controls> = None;
    // The entry and track shown, so the metadata is sent once a track.
    let mut shown: Option<(Id, Option<Id>)> = None;
    while let Ok(message) = receive.recv() {
        if stopping.load(Ordering::Acquire) {
            break;
        }
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
                    let keys = keys.clone();
                    let keys: Keys = Arc::new(move |key| {
                        let _ = keys.send(Message::Key(key));
                    });
                    controls = Controls::open(keys, runtime);
                }
                if let Some(controls) = &mut controls {
                    show(core, controls, &state, &mut shown);
                }
            }
        }
    }
}

/// Sends the state: the track when it has changed, and always the status
/// with the position, from which macOS decides who has the keys.
fn show(
    core: &Weak<Core>,
    controls: &mut Controls,
    state: &PlayerState,
    shown: &mut Option<(Id, Option<Id>)>,
) {
    let now = state.current.as_ref();
    let showing = now.map(|now| (now.entry_id, now.track.as_ref().map(|t| t.appearance)));
    if showing != *shown {
        *shown = showing;
        let cover = now
            .and_then(|now| now.track.as_ref())
            .and_then(|track| core.upgrade()?.now_playing_cover(track.appearance))
            .map(|path| file_url(&path));
        controls.set_track(state, cover.as_deref());
    }
    controls.set_state(state);
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

/// Waits until `stopped` gives a reason to stop. Only macOS needs the main
/// thread for its media session; elsewhere it simply waits.
#[cfg(not(target_os = "macos"))]
pub(crate) fn wait<T: Send + 'static>(stopped: Receiver<T>) -> Option<T> {
    stopped.recv().ok()
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
    fn off_the_main_thread_waiting_is_only_waiting() {
        let (send, receive) = mpsc::channel();
        send.send(7).unwrap();
        assert_eq!(wait(receive), Some(7));
    }
}
