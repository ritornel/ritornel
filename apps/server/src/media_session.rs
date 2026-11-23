//! The system's media session: Now Playing and the media keys, held by the
//! server process so they work with no window open. See "OS integration" in
//! docs/internals/playback.md.
//!
//! A thread of its own owns the system's controls: it shows the player's
//! state, and passes each key on to the player by the path a contract call
//! takes. The system's own controls are `platform::MediaControls`:
//!
//! - macOS: souvlaki's Now Playing. The main thread blocks in the run loop,
//!   which is all macOS needs to deliver the keys (`platform::wait`).
//! - Linux: MPRIS on the session bus, served from the server's own async
//!   runtime. A server with no session bus has none, quietly.
//! - Windows: the system's media transport controls (`ritornel-windows`),
//!   tied to the hidden window `serve` opens there.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex, PoisonError, Weak, mpsc};
use std::thread::JoinHandle;

use ritornel_engine::Id;
use ritornel_engine::playback::PlayerState;
use tokio::runtime::Handle;
use tracing::debug;

use crate::dispatch::Core;
use crate::platform::MediaControls;

/// What a media key asks of the player.
#[derive(Debug, Clone, Copy, PartialEq)]
#[allow(dead_code, reason = "each system's controls send only some keys")]
pub(crate) enum Key {
    Toggle,
    Play,
    Pause,
    Next,
    /// To the entry after this one only: nothing if the player has moved
    /// on from it. Only MPRIS names the entry.
    NextFrom(Id),
    Previous,
    /// To this position, in milliseconds, in whichever entry the player is
    /// on. MPRIS's seeks name the entry; macOS's and Windows' do not.
    Seek(u64),
    /// To this position, in milliseconds, in this entry only: nothing if
    /// the player has moved on from it. Only MPRIS names the entry.
    SeekIn {
        entry: Id,
        position_ms: u64,
    },
    /// The player's own volume, 0 to 1. Only MPRIS sets it.
    Volume(f64),
}

/// Where the system's controls send each key: back to the session's
/// thread, which carries it out.
pub(crate) type Keys = Arc<dyn Fn(Key) + Send + Sync>;

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
    /// where the session is served from it (`Server::stop`). Each wait on
    /// the bus is bounded: an attempt to connect, and each update.
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
    let mut controls: Option<MediaControls> = None;
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
                    if !state.status.wants_sound() {
                        continue;
                    }
                    let keys = keys.clone();
                    let keys: Keys = Arc::new(move |key| {
                        let _ = keys.send(Message::Key(key));
                    });
                    controls = MediaControls::open(keys, runtime);
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
    controls: &mut MediaControls,
    state: &PlayerState,
    shown: &mut Option<(Id, Option<Id>)>,
) {
    let now = state.current.as_ref();
    let showing = now.map(|now| (now.entry_id, now.track.as_ref().map(|t| t.appearance)));
    if showing != *shown {
        *shown = showing;
        let cover = now
            .and_then(|now| now.track.as_ref())
            .and_then(|track| core.upgrade()?.now_playing_cover(track.appearance));
        controls.set_track(state, cover.as_deref());
    }
    controls.set_state(state);
}

/// A file URL for a path, escaped: the data directory has a space in it.
/// Now Playing and MPRIS take a cover by its URL.
#[allow(dead_code, reason = "Windows' controls take a cover by its path")]
pub(crate) fn file_url(path: &Path) -> String {
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
}
