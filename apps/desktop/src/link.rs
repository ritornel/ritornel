//! Finding the server, and finding it again ("Workspace and server
//! connection" in docs/internals/desktop.md).
//!
//! The data directory comes from `--data-dir`, `RITORNEL_DATA_DIR`, or the
//! system's default. A `ritornel-server` beside the program is started when
//! none is running (with `--output` from `RITORNEL_TEST_OUTPUT`); with none
//! beside it, the app only finds one. Where the server outlives the app
//! (`platform::SERVER_APART`), it is started apart from it, and Quit stops
//! it, on this run or a later one; elsewhere the launcher holds it, and it
//! stops when the app ends. While there is no server it keeps looking, a
//! little less often each time up to every two seconds, and explains why;
//! "Look again" looks at once. A session that ends is "lost", and looked
//! for again. Until there is a session it also watches for what a server
//! that holds the directory says it is preparing (`preparing.json`), such
//! as updating the library after an upgrade, so that the window can say so.

use std::ffi::OsString;
use std::path::Path;
use std::process::Child;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use futures::StreamExt;
use gpui::{AppContext, Context, EventEmitter, Task};
use ritornel_client::launch::{Launcher, Program};
use ritornel_client::local::LocalServer;
use ritornel_client::{Error, NotRunning, local};
use ritornel_contract::Event;
use ritornel_contract::local::Preparing;

use crate::net::{Net, rt};

/// Why there is no server to talk to.
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
                "Ritornel plays from a server on this computer. Start one, and this window will find it.",
                true,
            ),
            Self::Starting => (
                "The server is starting.",
                "It has not said where it listens yet. This takes a moment.",
                false,
            ),
            Self::Unreachable => (
                "The server is not answering.",
                "One is running on this computer, but it could not be reached. It may be stopping.",
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

/// What the Library and History pages say while the server prepares the
/// library before it listens.
pub(crate) fn preparing_words(step: Preparing) -> (&'static str, &'static str) {
    let text = match step {
        Preparing::Backup => {
            "Saving a copy of your library first. Your music and history will be here when it is done."
        }
        Preparing::Migration => {
            "A copy of your library is saved, and it is being updated now. Your music and history will be here when it is done."
        }
        Preparing::History => {
            "Bringing your listening history up to date. Your music and history will be here when it is done."
        }
    };
    ("Updating your library…", text)
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
    /// The same session, with what the server now says of itself: its
    /// name, its role, and whether it holds a library.
    Restated(Arc<Net>),
}

pub(crate) struct Link {
    pub(crate) state: State,
    launcher: Option<Arc<Launcher>>,
    /// The server to start apart from the app, where it outlives it.
    apart: Option<Arc<Program>>,
    pending: Arc<Mutex<Pending>>,
    /// Whether a look is under way, for "Looking…".
    pub(crate) looking: bool,
    attempt: Option<Task<()>>,
    /// Counts the reads of what the server says of itself, so that only
    /// the latest is kept.
    restating: u64,
    /// What the server holding the directory says it is preparing, while
    /// there is no session.
    pub(crate) preparing: Option<Preparing>,
    /// Reads that again until there is a session.
    watching: Option<Task<()>>,
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
        let (held, apart) = if crate::platform::SERVER_APART {
            (None, program.map(Arc::new))
        } else {
            (program, None)
        };
        let launcher = local::data_dir(data_dir, &env)
            .ok()
            .map(|dir| Arc::new(Launcher::new(dir, held)));
        let mut link = Self {
            state: State::Looking,
            launcher,
            apart,
            pending: Arc::default(),
            looking: false,
            attempt: None,
            restating: 0,
            preparing: None,
            watching: None,
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

    /// What Quit stops: the server this app started, held or apart, on this
    /// run or an earlier one. A server it found is left running.
    pub(crate) fn stop_started(&self) {
        let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        let started = self.started_apart();
        if let Some(pid) = started {
            pending.ready(pid);
        }
        pending.quit();
        drop(pending);
        self.stop_owned();
        if let Some(pid) = started {
            crate::platform::stop_server(pid, ritornel_client::launch::STOP_WAIT);
        }
    }

    /// Whether quitting the app stops the server it talks to, so that
    /// opening it again starts the server afresh: one it started. A server
    /// it found, such as the background service, runs on.
    pub(crate) fn quitting_stops_the_server(&self) -> bool {
        self.launcher
            .as_ref()
            .is_some_and(|launcher| launcher.owned_pid().is_some())
            || self.started_apart().is_some()
    }

    /// The server holding the data directory's lock, if this app started
    /// it apart from itself, on this run or an earlier one.
    fn started_apart(&self) -> Option<u32> {
        let dir = self.data_dir()?;
        let holder = std::fs::read_to_string(dir.join(ritornel_contract::local::SERVER_LOCK))
            .ok()
            .and_then(|text| ritornel_contract::local::lock_holder(&text))?;
        crate::platform::started_here(holder).then_some(holder)
    }

    /// What the title bar says while there is no session.
    pub(crate) fn unconnected(&self) -> &'static str {
        match (self.preparing, &self.state) {
            (Some(_), _) => "Updating your library…",
            (
                None,
                State::Looking
                | State::Away {
                    why: Why::Starting, ..
                },
            ) => "Connecting…",
            _ => "Not connected",
        }
    }

    /// Reads what the server holding the directory says it is preparing,
    /// four times a second, until a session begins: a server the launcher
    /// waits for says nothing else until it listens.
    fn watch_preparing(&mut self, cx: &mut Context<Self>) {
        let Some(dir) = self.data_dir() else { return };
        if self.watching.is_some() {
            return;
        }
        self.watching = Some(cx.spawn(async move |link, cx| {
            loop {
                let dir = dir.clone();
                let step = cx
                    .background_executor()
                    .spawn(async move { local::preparing(&dir) })
                    .await;
                let changed = link.update(cx, |link, cx| {
                    if link.preparing == step {
                        return;
                    }
                    // Done preparing, the server is about to listen: look
                    // now rather than at the end of a wait between looks.
                    let done = link.preparing.is_some() && step.is_none();
                    link.preparing = step;
                    if done && !link.looking && !matches!(link.state, State::Connected(_)) {
                        link.look(cx);
                    } else {
                        cx.notify();
                    }
                });
                if changed.is_err() {
                    return;
                }
                cx.background_executor()
                    .timer(Duration::from_millis(250))
                    .await;
            }
        }));
    }

    fn look(&mut self, cx: &mut Context<Self>) {
        self.watch_preparing(cx);
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
        let apart = self.apart.clone();
        let pending = Arc::clone(&self.pending);
        // Dropping the attempt before stops the looking it was doing.
        self.attempt = Some(cx.spawn(async move |link, cx| {
            let mut tries = 0usize;
            loop {
                let (launched, apart) = (Arc::clone(&launcher), apart.clone());
                let pending = Arc::clone(&pending);
                let found = rt()
                    .spawn(async move {
                        let server = find_or_start(&launched, apart.as_deref(), &pending).await?;
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
                        // Show the error detail only when it is none of the
                        // known reasons; otherwise show where we looked.
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

    /// Reads again what the server says of itself, once it has said that
    /// changed, and hands on the session with it: a library made since it
    /// connected opens without connecting again.
    fn restate(&mut self, cx: &mut Context<Self>) {
        let State::Connected(net) = &self.state else {
            return;
        };
        let client = Arc::clone(&net.client);
        self.restating += 1;
        let asked = self.restating;
        let read = {
            let client = Arc::clone(&client);
            rt().spawn(async move { client.status().await })
        };
        cx.spawn(async move |link, cx| {
            let Ok(Ok(status)) = read.await else { return };
            let _ = link.update(cx, |link, cx| {
                // Overtaken by a later read, or by another session.
                let State::Connected(net) = &link.state else {
                    return;
                };
                if link.restating != asked || !Arc::ptr_eq(&net.client, &client) {
                    return;
                }
                let net = Arc::new(net.restated(&status));
                link.state = State::Connected(Arc::clone(&net));
                link.say_when_quiet_ends(&net, cx);
                cx.emit(LinkEvent::Restated(net));
                cx.notify();
            });
        })
        .detach();
    }

    /// A hub that has just gone is not spoken of for a few seconds
    /// (`crate::away`). When those end and it is still away, the session is
    /// handed on again as it is, so that what says so is drawn.
    fn say_when_quiet_ends(&self, net: &Arc<Net>, cx: &mut Context<Self>) {
        let Some(away) = crate::away::away(net) else {
            return;
        };
        if away.wait_ms(crate::away::now_ms()).is_none() {
            return;
        }
        let net = Arc::clone(net);
        cx.spawn(async move |link, cx| {
            // The timer keeps time apart from the clock the absence is
            // dated by, and may end a moment early by it: it waits until
            // the absence may be said.
            while let Some(wait) = away.wait_ms(crate::away::now_ms()) {
                cx.background_executor()
                    .timer(Duration::from_millis(wait))
                    .await;
            }
            let _ = link.update(cx, |link, cx| {
                // Compare the status snapshot itself so a newer restate supersedes this timer.
                if let State::Connected(held) = &link.state
                    && Arc::ptr_eq(held, &net)
                {
                    cx.emit(LinkEvent::Restated(Arc::clone(&net)));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn connected(
        &mut self,
        net: Arc<Net>,
        mut events: tokio::sync::mpsc::UnboundedReceiver<Event>,
        cx: &mut Context<Self>,
    ) {
        self.looking = false;
        self.state = State::Connected(Arc::clone(&net));
        self.watching = None;
        self.preparing = None;
        self.say_when_quiet_ends(&net, cx);
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
                    .update(cx, |link, cx| {
                        let restated = matches!(&event, Event::Servers(changed) if changed.server.is_this_server);
                        cx.emit(LinkEvent::Server(event));
                        if restated {
                            link.restate(cx);
                        }
                    })
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

    #[cfg(test)]
    pub(crate) fn connect(
        &mut self,
        net: Arc<Net>,
        events: tokio::sync::mpsc::UnboundedReceiver<Event>,
        cx: &mut Context<Self>,
    ) {
        self.connected(net, events, cx);
    }
}

/// How long a server started apart has to come up.
const START_WITHIN: Duration = Duration::from_secs(10);

/// A detached child that has not announced itself yet. Explicit Quit and
/// starting the process take the same lock, including the spawn itself.
#[derive(Default)]
struct Pending {
    quitting: bool,
    child: Option<Child>,
}

impl Pending {
    fn start(&mut self, spawn: impl FnOnce() -> std::io::Result<Child>) -> Result<(), Error> {
        if self.quitting {
            return Err(Error::Start("Ritornel is quitting.".into()));
        }
        if let Some(child) = self.child.as_mut()
            && child
                .try_wait()
                .map_err(|err| Error::Start(err.to_string()))?
                .is_none()
        {
            return Ok(());
        }
        self.child = Some(spawn().map_err(|err| Error::Start(err.to_string()))?);
        Ok(())
    }

    fn ready(&mut self, pid: u32) {
        if self.child.as_ref().is_some_and(|child| child.id() != pid) {
            // Another server won the directory. Our child is no longer needed.
            self.stop_child();
        } else if let Some(mut child) = self.child.take() {
            let _ = std::thread::Builder::new()
                .name("server reaper".into())
                .spawn(move || child.wait());
        }
    }

    fn stop_child(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    fn quit(&mut self) {
        self.quitting = true;
        self.stop_child();
    }
}

/// Finds the server, or, with a program to start apart from the app and
/// none running, starts it and finds it once it is up. One that loses the
/// race to another ends, and the other is found.
async fn find_or_start(
    launcher: &Launcher,
    apart: Option<&Program>,
    pending: &Mutex<Pending>,
) -> Result<LocalServer, Error> {
    let pending = || pending.lock().unwrap_or_else(PoisonError::into_inner);
    let error = match launcher.find_or_start().await {
        Ok(server) => {
            pending().ready(server.pid());
            return Ok(server);
        }
        Err(error) => error,
    };
    let (
        Error::NotRunning {
            why: NotRunning::NoServerJson | NotRunning::Killed,
            ..
        },
        Some(program),
    ) = (&error, apart)
    else {
        return Err(error);
    };
    pending().start(|| {
        crate::platform::start_server(program, launcher.data_dir()).map_err(|err| {
            std::io::Error::new(err.kind(), format!("{}: {err}", program.path.display()))
        })
    })?;
    let started = Instant::now();
    let mut ended = None;
    loop {
        tokio::time::sleep(Duration::from_millis(50)).await;
        if ended.is_none() {
            let mut pending = pending();
            if let Some(child) = pending.child.as_mut() {
                ended = child.try_wait().ok().flatten();
            } else if pending.quitting {
                return Err(Error::Start("Ritornel is quitting.".into()));
            }
        }
        let lost_race = ended.is_some_and(|status| {
            status.code() == Some(i32::from(ritornel_contract::local::EXIT_IN_USE))
        });
        match launcher.find_or_start().await {
            Ok(server) => {
                pending().ready(server.pid());
                return Ok(server);
            }
            Err(_) if ended.is_some() && !lost_race => {
                let status = ended.map(|status| status.to_string()).unwrap_or_default();
                return Err(Error::Start(format!(
                    "the server ended before it was ready ({status})"
                )));
            }
            Err(error) if started.elapsed() >= START_WITHIN => return Err(error),
            Err(_) => {}
        }
    }
}

/// A link that never looks for a server, for tests of views that hold one.
#[cfg(test)]
pub(crate) fn unlinked(cx: &mut gpui::App) -> gpui::Entity<Link> {
    cx.new(|_| Link {
        state: State::Looking,
        launcher: None,
        apart: None,
        pending: Arc::default(),
        looking: false,
        attempt: None,
        restating: 0,
        preparing: None,
        watching: None,
    })
}

/// The link, made once for the app.
pub(crate) fn new(data_dir: Option<&Path>, cx: &mut gpui::App) -> gpui::Entity<Link> {
    cx.new(|cx| Link::new(data_dir, cx))
}

#[cfg(all(test, unix))]
mod launch_tests {
    use super::*;

    #[test]
    fn quit_stops_a_pending_child_and_prevents_a_later_spawn() -> gpui::Result<()> {
        let mut pending = Pending::default();
        pending.start(|| std::process::Command::new("sleep").arg("60").spawn())?;
        let pid = pending
            .child
            .as_ref()
            .ok_or_else(|| std::io::Error::other("child missing"))?
            .id();
        pending.quit();
        assert!(
            !std::process::Command::new("kill")
                .args(["-0", &pid.to_string()])
                .output()?
                .status
                .success()
        );
        let mut spawned = false;
        assert!(
            pending
                .start(|| {
                    spawned = true;
                    Err(std::io::ErrorKind::Interrupted.into())
                })
                .is_err()
        );
        assert!(!spawned);
        Ok(())
    }
}
