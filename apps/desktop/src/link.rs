//! Finding the server, and finding it again: the GPUI app's part of what
//! the Tauri shell (`src-tauri/src/lib.rs`, `open_connection`) and the page
//! (`lib/link.ts`, `lib/client.ts`) do together.
//!
//! The data directory is the one `--data-dir`, `RITORNEL_DATA_DIR` or the
//! system's default names, as the Tauri app's is. A `ritornel-server` beside
//! the program is started when none is running, as the app bundle's is
//! (with `--output` from `RITORNEL_TEST_OUTPUT`); with none beside it, the
//! app only finds one. While there is no server it keeps looking, a little
//! less often each time up to every two seconds, and says why in the words
//! the Tauri app uses; "Look again" looks at once. A session that ends is
//! "lost", and looked for again.

use std::ffi::OsString;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use gpui::{AppContext, Context, EventEmitter, Task};
use ritornel_client::launch::{Launcher, Program};
use ritornel_client::{Error, NotRunning, local};
use ritornel_contract::Event;

use crate::net::{Net, rt};

/// Why there is no server to talk to: the Tauri page's reasons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Why {
    NotRunning,
    Starting,
    Unreachable,
    Incompatible,
    Lost,
    Failed,
}

impl Why {
    fn of(error: &Error) -> Self {
        match error {
            Error::NotRunning { why, .. } => match why {
                NotRunning::NoServerJson | NotRunning::Killed => Self::NotRunning,
                NotRunning::Starting => Self::Starting,
                NotRunning::NoAnswer(_) | NotRunning::AnotherServer(_) => Self::Unreachable,
            },
            Error::Transport(_) | Error::Refused { .. } => Self::Unreachable,
            Error::ContractVersion { .. } => Self::Incompatible,
            _ => Self::Failed,
        }
    }

    /// What the Library page says, and whether looking again is worth offering.
    pub(crate) fn words(self) -> (&'static str, &'static str, bool) {
        match self {
            Self::NotRunning => (
                "No server is running.",
                "Ritornel plays from a server on this Mac. Start one, and this window will find it.",
                true,
            ),
            Self::Starting => (
                "The server is starting.",
                "It has not said where it listens yet. This takes a moment.",
                false,
            ),
            Self::Unreachable => (
                "The server is not answering.",
                "One is running on this Mac, but it could not be reached. It may be stopping.",
                true,
            ),
            Self::Incompatible => (
                "The server is from another release.",
                "This app and the server cannot talk to each other. Use the two from one release.",
                true,
            ),
            Self::Lost => (
                "The connection was lost.",
                "Looking for the server again.",
                false,
            ),
            Self::Failed => (
                "The server could not be found.",
                "Something went wrong while looking for it.",
                true,
            ),
        }
    }
}

#[derive(Clone)]
pub(crate) enum State {
    /// Looking, for the first time.
    Looking,
    Connected(Arc<Net>),
    Away {
        why: Why,
        /// One line that says what happened, when there is more to say than `why`.
        message: Option<String>,
        data_dir: Option<String>,
    },
}

/// What the link tells whoever follows it.
#[derive(Clone)]
pub(crate) enum LinkEvent {
    /// A session began: everything is read again through it.
    Connected(Arc<Net>),
    /// The session ended, or none could be had.
    Away,
    /// Something the server sent.
    Server(Event),
}

pub(crate) struct Link {
    pub(crate) state: State,
    launcher: Option<Arc<Launcher>>,
    /// Whether a look is under way, for "Looking…".
    pub(crate) looking: bool,
    attempt: Option<Task<()>>,
}

impl EventEmitter<LinkEvent> for Link {}

/// How long to wait before each look after one that found nothing. The
/// last repeats.
const BACKOFF_MS: [u64; 4] = [250, 500, 1000, 2000];

/// The variable whose value is passed to the server it starts as `--output`.
const OUTPUT_VAR: &str = "RITORNEL_TEST_OUTPUT";

impl Link {
    pub(crate) fn new(data_dir: Option<&Path>, cx: &mut Context<Self>) -> Self {
        let env = |name: &str| std::env::var_os(name);
        let program = std::env::current_exe()
            .ok()
            .as_deref()
            .and_then(Path::parent)
            .map(|dir| dir.join(crate::platform::server_program()))
            .filter(|path| path.is_file())
            .map(|path| {
                let mut program = Program::new(path);
                if let Some(output) = env(OUTPUT_VAR).filter(|output| !output.is_empty()) {
                    program.args = vec![OsString::from("--output"), output];
                }
                program
            });
        let launcher = local::data_dir(data_dir, &env)
            .ok()
            .map(|dir| Arc::new(Launcher::new(dir, program)));
        let mut link = Self {
            state: State::Looking,
            launcher,
            looking: false,
            attempt: None,
        };
        link.look(cx);
        link
    }

    pub(crate) fn net(&self) -> Option<Arc<Net>> {
        match &self.state {
            State::Connected(net) => Some(Arc::clone(net)),
            _ => None,
        }
    }

    /// The data directory the app looks in, if one could be worked out.
    pub(crate) fn data_dir(&self) -> Option<std::path::PathBuf> {
        self.launcher
            .as_ref()
            .map(|launcher| launcher.data_dir().to_path_buf())
    }

    /// Looks at once, as "Look again" does.
    pub(crate) fn look_again(&mut self, cx: &mut Context<Self>) {
        if matches!(self.state, State::Connected(_)) {
            return;
        }
        self.look(cx);
    }

    /// Stops the server this app started, if it started one, as quitting does.
    pub(crate) fn stop_owned(&self) {
        if let Some(launcher) = &self.launcher {
            let _ = launcher.stop_owned(ritornel_client::launch::STOP_WAIT);
        }
    }

    fn look(&mut self, cx: &mut Context<Self>) {
        let Some(launcher) = self.launcher.clone() else {
            self.state = State::Away {
                why: Why::Failed,
                message: Some(Error::NoDataDir.to_string()),
                data_dir: None,
            };
            cx.emit(LinkEvent::Away);
            cx.notify();
            return;
        };
        self.looking = true;
        cx.notify();
        // Dropping the attempt before stops the looking it was doing.
        self.attempt = Some(cx.spawn(async move |link, cx| {
            let mut tries = 0usize;
            loop {
                let launched = Arc::clone(&launcher);
                let found = rt()
                    .spawn(async move {
                        let server = launched.find_or_start().await?;
                        Net::open(&server).await
                    })
                    .await;
                let failed = match found {
                    Ok(Ok((net, events))) => {
                        let net = Arc::new(net);
                        let _ = link
                            .update(cx, |link, cx| link.connected(Arc::clone(&net), events, cx));
                        return;
                    }
                    Ok(Err(error)) => error,
                    Err(join) => Error::Start(join.to_string()),
                };
                let data_dir = launcher.data_dir().display().to_string();
                let away = link.update(cx, |link, cx| {
                    link.looking = false;
                    let why = Why::of(&failed);
                    link.state = State::Away {
                        why,
                        // The page says what went wrong only when it is
                        // none of the known reasons, as the real page does;
                        // otherwise where it looked.
                        message: (why == Why::Failed).then(|| failed.to_string()),
                        data_dir: Some(data_dir),
                    };
                    cx.emit(LinkEvent::Away);
                    cx.notify();
                });
                if away.is_err() {
                    return;
                }
                let wait = BACKOFF_MS.get(tries).copied().unwrap_or(2000);
                tries += 1;
                cx.background_executor()
                    .timer(Duration::from_millis(wait))
                    .await;
            }
        }));
    }

    fn connected(
        &mut self,
        net: Arc<Net>,
        mut events: tokio::sync::mpsc::UnboundedReceiver<Event>,
        cx: &mut Context<Self>,
    ) {
        self.looking = false;
        self.state = State::Connected(Arc::clone(&net));
        cx.emit(LinkEvent::Connected(net));
        cx.notify();
        // The events, read on the runtime and handed to the foreground in
        // bursts; their end is the session's.
        let (forward, mut arrived) = futures::channel::mpsc::unbounded();
        rt().spawn(async move {
            while let Some(event) = events.recv().await {
                if forward.unbounded_send(event).is_err() {
                    break;
                }
            }
        });
        self.attempt = Some(cx.spawn(async move |link, cx| {
            while let Some(event) = arrived.next().await {
                if link
                    .update(cx, |_, cx| cx.emit(LinkEvent::Server(event)))
                    .is_err()
                {
                    return;
                }
            }
            let _ = link.update(cx, |link, cx| {
                link.state = State::Away {
                    why: Why::Lost,
                    message: None,
                    data_dir: None,
                };
                cx.emit(LinkEvent::Away);
                cx.notify();
                link.look(cx);
            });
        }));
    }
}

/// The link, made once for the app.
pub(crate) fn new(data_dir: Option<&Path>, cx: &mut gpui::App) -> gpui::Entity<Link> {
    cx.new(|cx| Link::new(data_dir, cx))
}
