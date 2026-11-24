//! Watching the hub's folders, so that new, changed, moved and removed
//! files reach the library without a scan by hand, and new or changed
//! artist pictures without an import by hand. See "Watching" in
//! docs/internals/identity.md.
//!
//! One thread watches every music root and picture folder through the
//! system: FSEvents on macOS, inotify on Linux. Once a folder's changes
//! settle it queues the ordinary scan job for a music root, which reads only
//! what changed and writes nothing when nothing did, or has a picture folder
//! imported again on a thread of its own. A timer rescans as well, for
//! folders the system says nothing about, such as a network share, and for
//! what changed while the hub was not running. Only the hub watches: it is
//! the library's one writer.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use ritornel_contract::{AutoRun, AutoTrigger, Id, JobState, WatchState, WatchedKind};
use ritornel_engine::folder_id;
use tracing::{info, warn};

use crate::dispatch::Core;

/// How long a folder's changes must have stopped before it is rescanned:
/// long enough for a copy of an album to finish, short enough to feel
/// prompt.
pub(crate) const QUIET: Duration = Duration::from_secs(4);

/// The longest a rescan waits while changes keep coming.
pub(crate) const LONGEST: Duration = Duration::from_secs(60);

/// How long after the last scan of a root ended the timer scans it again.
pub(crate) const RESCAN_EVERY: Duration = Duration::from_secs(6 * 60 * 60);

/// How long after the last scan of a root ended the hub, starting, scans
/// it again: what changed while it was not running is not seen otherwise.
pub(crate) const RESCAN_AT_START_AFTER: Duration = Duration::from_secs(60 * 60);

/// The longest the thread sleeps: how late the timer may be, and how often
/// a folder that went offline is looked for.
const IDLE: Duration = Duration::from_secs(60);

/// How many signals wait for the thread at most, and how many it takes in
/// before it acts on them. What finds the channel full is not kept: every
/// folder is rescanned instead.
pub(crate) const SIGNALS: usize = 4096;

/// How this run watches.
#[derive(Debug, Clone)]
pub struct WatchRun {
    /// Whether this run watches at all. `serve` does; a server in a test
    /// does not unless it asks.
    pub on: bool,
    /// How long changes must have stopped before a rescan.
    pub quiet: Duration,
    /// The longest a rescan waits while changes keep coming.
    pub longest: Duration,
    /// For tests: no system watcher, and events come only from what the
    /// test hands in.
    pub injected: bool,
}

impl Default for WatchRun {
    fn default() -> Self {
        Self {
            on: false,
            quiet: QUIET,
            longest: LONGEST,
            injected: false,
        }
    }
}

impl WatchRun {
    /// Watching on, as `serve` runs.
    pub fn on() -> Self {
        Self {
            on: true,
            ..Self::default()
        }
    }
}

/// What reaches the watching thread.
#[derive(Debug)]
pub(crate) enum Signal {
    /// Something changed at these paths.
    Changed(Vec<PathBuf>),
    /// The system lost track of changes at or above these paths, or
    /// anywhere if it names none.
    Lost(Vec<PathBuf>, String),
    /// Look again: the folders, the role or the setting may have changed,
    /// a scan or an import may have ended, or the server is stopping.
    Wake,
}

/// Where signals are sent to the watching thread. A signal that finds the
/// channel full is dropped and noted, so that no sender ever waits on the
/// thread and what waits for it stays bounded.
#[derive(Debug, Clone)]
pub(crate) struct Signals {
    sender: SyncSender<Signal>,
    dropped: Arc<AtomicBool>,
}

impl Signals {
    fn new() -> (Self, Receiver<Signal>) {
        let (sender, receiver) = mpsc::sync_channel(SIGNALS);
        let dropped = Arc::new(AtomicBool::new(false));
        (Self { sender, dropped }, receiver)
    }

    fn send(&self, signal: Signal) {
        // The thread has gone only if the server is stopping.
        if let Err(TrySendError::Full(_)) = self.sender.try_send(signal) {
            self.dropped.store(true, Ordering::SeqCst);
            // Should the thread have emptied the channel before the drop was
            // noted, this wakes it to take the note in.
            let _ = self.sender.try_send(Signal::Wake);
        }
    }
}

/// Takes in what reached the thread: the first signal, waiting up to `wait`
/// for it, then those already there, no more than `most` in all ([`SIGNALS`]
/// on the thread), so that a storm of changes never keeps the thread from
/// acting on what it heard. Whether any signal was dropped since the last
/// time comes with them. `None` once no one can send.
fn receive(
    receiver: &Receiver<Signal>,
    dropped: &AtomicBool,
    wait: Duration,
    most: usize,
) -> Option<(Vec<Signal>, bool)> {
    let mut taken = match receiver.recv_timeout(wait) {
        Ok(signal) => vec![signal],
        Err(RecvTimeoutError::Timeout) => Vec::new(),
        Err(RecvTimeoutError::Disconnected) => return None,
    };
    taken.extend(receiver.try_iter().take(most.saturating_sub(taken.len())));
    Some((taken, dropped.swap(false, Ordering::SeqCst)))
}

/// What a folder is known by: its name, and whether it holds pictures. A
/// picture folder may have a music root's name, as when they are one
/// folder.
pub(crate) type Key = (String, bool);

/// A folder the hub watches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Folder {
    /// The root's name.
    pub(crate) name: String,
    pub(crate) path: PathBuf,
    /// A music root's source, or `None` for a picture folder.
    pub(crate) source: Option<Id>,
}

impl Folder {
    pub(crate) fn key(&self) -> Key {
        (self.name.clone(), self.source.is_none())
    }

    fn kind(&self) -> WatchedKind {
        match self.source {
            Some(_) => WatchedKind::Music,
            None => WatchedKind::Pictures,
        }
    }
}

/// Where the events come from.
pub(crate) trait Backend: Send {
    /// Starts watching a folder and everything under it. An error says
    /// why events are not available for it.
    fn watch(&mut self, path: &Path) -> Result<(), String>;
    fn unwatch(&mut self, path: &Path);
}

/// The system's watcher.
struct Native(notify::RecommendedWatcher);

impl Native {
    fn start(signals: Signals) -> Result<Self, String> {
        use notify::event::{AccessKind, AccessMode, EventKind};
        let handler = move |event: notify::Result<notify::Event>| {
            let signal = match event {
                Ok(event) if event.need_rescan() => {
                    Signal::Lost(event.paths, "events were dropped".to_owned())
                }
                // Reads are not changes: the scan and the player open files
                // all the time.
                Ok(event)
                    if matches!(event.kind, EventKind::Access(kind)
                        if kind != AccessKind::Close(AccessMode::Write)) =>
                {
                    return;
                }
                Ok(event) => Signal::Changed(event.paths),
                Err(error) => {
                    let why = error.to_string();
                    Signal::Lost(error.paths, why)
                }
            };
            signals.send(signal);
        };
        notify::recommended_watcher(handler)
            .map(Self)
            .map_err(|error| error.to_string())
    }
}

impl Backend for Native {
    fn watch(&mut self, path: &Path) -> Result<(), String> {
        use notify::Watcher as _;
        self.0
            .watch(path, notify::RecursiveMode::Recursive)
            .map_err(|error| error.to_string())
    }

    fn unwatch(&mut self, path: &Path) {
        use notify::Watcher as _;
        // A folder that has gone may have taken its watch with it.
        let _ = self.0.unwatch(path);
    }
}

/// No watcher: every folder is watched, and nothing is ever said.
struct Silent;

impl Backend for Silent {
    fn watch(&mut self, _: &Path) -> Result<(), String> {
        Ok(())
    }

    fn unwatch(&mut self, _: &Path) {}
}

/// Where the system cannot watch: every folder is polled.
struct Unavailable(String);

impl Backend for Unavailable {
    fn watch(&mut self, _: &Path) -> Result<(), String> {
        Err(self.0.clone())
    }

    fn unwatch(&mut self, _: &Path) {}
}

fn ms(duration: Duration) -> i64 {
    i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}

/// A folder as the thread keeps it.
#[derive(Debug)]
struct Watched {
    folder: Folder,
    /// The path with its links resolved, as FSEvents reports it.
    real: PathBuf,
    state: WatchState,
    detail: Option<String>,
    /// The first and the last change not yet taken in, and what the first
    /// was.
    first_ms: Option<i64>,
    last_ms: Option<i64>,
    trigger: AutoTrigger,
    /// A rescan or an import is running that started before the changes:
    /// they wait for it to end.
    busy: bool,
    /// A picture folder's import was asked for and has not ended.
    importing: bool,
    /// The last import of a picture folder, which is no job.
    imported: Option<AutoRun>,
    /// When the folder was first found. The timer leaves a new one alone a
    /// moment, as whoever added it may be about to scan it.
    seen_ms: i64,
    /// Which folder was watched: its device and inode (on Windows, its
    /// volume and file index). A disk mounted again
    /// is another folder at the same path, and its old watch hears nothing.
    id: Option<(u64, u64)>,
    /// The system lost track of it: it is watched afresh at the next look.
    rearm: bool,
}

/// The system's watches: one for each path, however many folders share it,
/// and how many do. A music root may be a picture folder too, and the
/// system keeps one watch for a path, which one folder's unwatching would
/// take from the other.
struct Registry {
    backend: Box<dyn Backend>,
    held: BTreeMap<PathBuf, usize>,
}

impl Registry {
    /// Watches `path` for one more folder. `afresh` puts a new watch in
    /// place of the system's, which may have stopped hearing.
    fn watch(&mut self, path: &Path, afresh: bool) -> Result<(), String> {
        let held = self.held.get(path).copied().unwrap_or(0);
        if held == 0 || afresh {
            if held > 0 {
                self.backend.unwatch(path);
            }
            if let Err(why) = self.backend.watch(path) {
                self.held.remove(path);
                return Err(why);
            }
        }
        self.held.insert(path.to_owned(), held + 1);
        Ok(())
    }

    /// Lets go of `path` for one folder: the system's watch goes with the
    /// last.
    fn unwatch(&mut self, path: &Path) {
        match self.held.get_mut(path) {
            Some(held) if *held > 1 => *held -= 1,
            Some(_) => {
                self.held.remove(path);
                self.backend.unwatch(path);
            }
            None => {}
        }
    }
}

/// Watches a folder that is there, or says why it cannot be. `afresh`, as
/// in [`Registry::watch`].
fn arm(registry: &mut Registry, watched: &mut Watched, afresh: bool) {
    watched.real =
        std::fs::canonicalize(&watched.folder.path).unwrap_or_else(|_| watched.folder.path.clone());
    watched.id = folder_id(&watched.folder.path);
    watched.rearm = false;
    match registry.watch(&watched.folder.path, afresh) {
        Ok(()) => (watched.state, watched.detail) = (WatchState::Watching, None),
        Err(why) => {
            warn!(
                root = %watched.folder.name,
                error = %why,
                "a folder cannot be watched, and is rescanned on a timer alone"
            );
            (watched.state, watched.detail) = (WatchState::Polling, Some(why));
        }
    }
}

impl Watched {
    fn mark(&mut self, now_ms: i64, trigger: AutoTrigger) {
        if self.first_ms.is_none() || trigger == AutoTrigger::Lost {
            self.trigger = trigger;
        }
        self.first_ms.get_or_insert(now_ms);
        self.last_ms = Some(now_ms);
    }

    fn clear(&mut self) {
        (self.first_ms, self.last_ms, self.busy) = (None, None, false);
    }

    /// Whether `path` is a folder this one is inside: one removed, moved or
    /// mounted over takes this one with it.
    fn below(&self, path: &Path) -> bool {
        !path.as_os_str().is_empty()
            && [&self.folder.path, &self.real]
                .iter()
                .any(|root| root.starts_with(path) && root.as_path() != path)
    }

    /// Whether `path`, under this folder, is something a walk reads: a
    /// hidden name anywhere on the way is not.
    fn covers(&self, path: &Path) -> bool {
        [&self.folder.path, &self.real].iter().any(|root| {
            path.strip_prefix(root).is_ok_and(|rest| {
                !rest
                    .components()
                    .any(|part| part.as_os_str().to_string_lossy().starts_with('.'))
            })
        })
    }
}

/// What the watching thread knows: the folders, and the changes each has
/// not yet taken in. It decides when; [`tick`] acts.
pub(crate) struct Watch {
    registry: Registry,
    folders: BTreeMap<Key, Watched>,
    /// Imports asked for and not yet handed to the thread that imports.
    imports: Vec<(Folder, AutoTrigger)>,
    quiet_ms: i64,
    longest_ms: i64,
    /// Whether the hub's start has been looked at.
    started: bool,
    /// Where changes are never the library's: the server's own data
    /// directory, which it writes all the time, should it be in a root.
    skip: Vec<PathBuf>,
}

impl std::fmt::Debug for Watch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Watch")
            .field("folders", &self.folders)
            .finish_non_exhaustive()
    }
}

impl Watch {
    pub(crate) fn new(backend: Box<dyn Backend>, run: &WatchRun) -> Self {
        Self {
            registry: Registry {
                backend,
                held: BTreeMap::new(),
            },
            folders: BTreeMap::new(),
            imports: Vec::new(),
            quiet_ms: ms(run.quiet),
            longest_ms: ms(run.longest),
            started: false,
            skip: Vec::new(),
        }
    }

    /// Takes no change under `path` for one.
    pub(crate) fn skip(&mut self, path: &Path) {
        self.skip.push(path.to_owned());
        if let Ok(real) = std::fs::canonicalize(path) {
            self.skip.push(real);
        }
    }

    /// Takes the folders as they now are: watches new ones, forgets gone
    /// ones, and notices one that went offline or came back.
    pub(crate) fn refresh(&mut self, found: Vec<Folder>, now_ms: i64) {
        let keys: Vec<Key> = found.iter().map(Folder::key).collect();
        let gone: Vec<Key> = self
            .folders
            .keys()
            .filter(|key| !keys.contains(key))
            .cloned()
            .collect();
        for key in gone {
            if let Some(watched) = self.folders.remove(&key)
                && watched.state == WatchState::Watching
            {
                self.registry.unwatch(&watched.folder.path);
            }
        }
        for folder in found {
            let key = folder.key();
            let fresh = self
                .folders
                .get(&key)
                .is_none_or(|watched| watched.folder != folder);
            if fresh {
                if let Some(old) = self.folders.remove(&key)
                    && old.state == WatchState::Watching
                {
                    self.registry.unwatch(&old.folder.path);
                }
                self.folders.insert(
                    key.clone(),
                    Watched {
                        real: folder.path.clone(),
                        folder: folder.clone(),
                        state: WatchState::Offline,
                        detail: None,
                        first_ms: None,
                        last_ms: None,
                        trigger: AutoTrigger::Change,
                        busy: false,
                        importing: false,
                        imported: None,
                        seen_ms: now_ms,
                        id: None,
                        rearm: false,
                    },
                );
            }
            let Some(watched) = self.folders.get_mut(&key) else {
                continue;
            };
            let present = watched.folder.path.is_dir();
            match (watched.state, present) {
                (WatchState::Offline, true) => {
                    arm(&mut self.registry, watched, false);
                    // A folder that is back may have changed while it was
                    // away. One that is new is the timer's.
                    if !fresh {
                        info!(root = %watched.folder.name, "a folder that was offline is back");
                        watched.mark(now_ms, AutoTrigger::Returned);
                    }
                }
                (WatchState::Watching | WatchState::Polling, false) => {
                    if watched.state == WatchState::Watching {
                        self.registry.unwatch(&watched.folder.path);
                    }
                    info!(
                        root = %watched.folder.name,
                        "a folder is offline: nothing in it is taken to be gone"
                    );
                    (watched.state, watched.detail) = (WatchState::Offline, None);
                    watched.clear();
                }
                (WatchState::Watching | WatchState::Polling, true) => {
                    let other = folder_id(&watched.folder.path) != watched.id;
                    if other || watched.rearm {
                        if watched.state == WatchState::Watching {
                            self.registry.unwatch(&watched.folder.path);
                        }
                        arm(&mut self.registry, watched, true);
                    }
                    if other {
                        info!(
                            root = %watched.folder.name,
                            "a folder is another folder now, as a disk mounted again is: it is watched afresh"
                        );
                        watched.mark(now_ms, AutoTrigger::Returned);
                    }
                }
                (WatchState::Offline, false) => {}
            }
        }
    }

    /// Takes in what the system said. It returns whether the folders
    /// should be looked at again: the system lost track of one, or one may
    /// have gone with a folder it is inside.
    pub(crate) fn signal(&mut self, signal: Signal, now_ms: i64) -> bool {
        match signal {
            Signal::Changed(paths) => {
                let mut look = false;
                for path in paths {
                    if self.skip.iter().any(|skip| path.starts_with(skip)) {
                        continue;
                    }
                    // A picture folder may be inside a music root: both.
                    for watched in self.folders.values_mut() {
                        if watched.below(&path) {
                            // Its watch may have gone with it.
                            (watched.rearm, look) = (true, true);
                            watched.mark(now_ms, AutoTrigger::Change);
                        } else if watched.covers(&path) {
                            watched.mark(now_ms, AutoTrigger::Change);
                        }
                    }
                }
                look
            }
            Signal::Lost(paths, why) => {
                warn!(
                    paths = ?paths,
                    error = %why,
                    "the system lost track of changes: the folder is rescanned"
                );
                // A path at or under a folder, or above it.
                let lost = |watched: &Watched| {
                    paths.is_empty()
                        || paths.iter().any(|path| {
                            path.starts_with(&watched.folder.path)
                                || path.starts_with(&watched.real)
                                || watched.below(path)
                        })
                };
                // One offline is looked for anyway, and marked when it is back.
                let folders = self.folders.values_mut();
                for watched in
                    folders.filter(|watched| watched.state != WatchState::Offline && lost(watched))
                {
                    watched.rearm = true;
                    watched.mark(now_ms, AutoTrigger::Lost);
                }
                true
            }
            Signal::Wake => true,
        }
    }

    /// Takes in that the import of the picture folder `key` ended, or was
    /// not run.
    pub(crate) fn imported(&mut self, key: &Key, run: Option<AutoRun>) {
        if let Some(watched) = self.folders.get_mut(key) {
            (watched.importing, watched.busy) = (false, false);
            watched.imported = run.or(watched.imported.take());
        }
    }

    /// The imports asked for since this was last asked, for the thread that
    /// imports.
    pub(crate) fn take_imports(&mut self) -> Vec<(Folder, AutoTrigger)> {
        std::mem::take(&mut self.imports)
    }

    /// The folders whose changes have settled: quiet for long enough, or
    /// changing for longer than a rescan waits.
    pub(crate) fn settled(&self, now_ms: i64) -> Vec<Key> {
        self.folders
            .values()
            .filter(|watched| watched.state != WatchState::Offline)
            .filter(|watched| match (watched.first_ms, watched.last_ms) {
                (Some(first), Some(last)) => {
                    now_ms - last >= self.quiet_ms || now_ms - first >= self.longest_ms
                }
                _ => false,
            })
            .map(|watched| watched.folder.key())
            .collect()
    }

    /// How long until a folder's changes settle: the thread sleeps no
    /// longer. A folder waiting on a running scan or import is woken when
    /// it ends.
    fn next_wait(&self, now_ms: i64) -> Option<Duration> {
        self.folders
            .values()
            .filter(|watched| !watched.busy && watched.state != WatchState::Offline)
            .filter_map(|watched| {
                let due =
                    (watched.last_ms? + self.quiet_ms).min(watched.first_ms? + self.longest_ms);
                Some(Duration::from_millis(
                    u64::try_from(due - now_ms).unwrap_or(0),
                ))
            })
            .min()
    }

    /// What the status shows of each folder. A music root's last
    /// automatic scan is read from its job.
    pub(crate) fn shown(&self) -> Vec<Shown> {
        self.folders
            .values()
            .map(|watched| Shown {
                name: watched.folder.name.clone(),
                kind: watched.folder.kind(),
                state: watched.state,
                detail: watched.detail.clone(),
                pending_since: watched.first_ms,
                imported: watched.imported.clone(),
            })
            .collect()
    }
}

/// A folder as the status shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Shown {
    pub(crate) name: String,
    pub(crate) kind: WatchedKind,
    pub(crate) state: WatchState,
    pub(crate) detail: Option<String>,
    pub(crate) pending_since: Option<i64>,
    pub(crate) imported: Option<AutoRun>,
}

/// What asking for an automatic rescan came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Rescan {
    /// A scan was queued: this job.
    Started(Id),
    /// A scan that will see the changes is queued or running, and counts.
    Joined,
    /// A scan is running that started before the changes: they wait for
    /// it to end.
    Busy,
    /// The last scan ended too recently for the timer.
    NotDue,
    /// One could not be queued.
    Failed(String),
}

/// One look by the thread: rescans what has settled, asks for picture
/// folders that changed to be imported, and rescans what the timer says is
/// due. The first look is the hub's start.
pub(crate) fn tick(core: &Core, watch: &mut Watch, now_ms: i64) {
    let start = !watch.started;
    watch.started = true;
    for key in watch.settled(now_ms) {
        let Some(watched) = watch.folders.get_mut(&key) else {
            continue;
        };
        let (folder, trigger) = (watched.folder.clone(), watched.trigger);
        let name = &folder.name;
        match folder.source {
            Some(source) => match core.auto_scan(name, source, trigger, watched.last_ms) {
                Rescan::Busy => watched.busy = true,
                Rescan::Failed(why) => {
                    // The timer tries again.
                    warn!(root = %name, error = %why, "a rescan could not be queued");
                    watched.clear();
                }
                done => {
                    log(name, trigger, &done);
                    watched.clear();
                }
            },
            // An import that started before the changes: they wait for it.
            None if watched.importing => watched.busy = true,
            None => {
                watched.clear();
                watched.importing = true;
                watch.imports.push((folder, trigger));
            }
        }
    }
    let (trigger, after) = if start {
        (AutoTrigger::Start, RESCAN_AT_START_AFTER)
    } else {
        (AutoTrigger::Timer, RESCAN_EVERY)
    };
    let quiet_ms = watch.quiet_ms;
    for watched in watch.folders.values_mut() {
        if watched.state == WatchState::Offline
            || watched.first_ms.is_some()
            || watched.importing
            || !start && now_ms.saturating_sub(watched.seen_ms) < quiet_ms
        {
            continue;
        }
        let folder = watched.folder.clone();
        match folder.source {
            Some(source) => {
                let done = core.scan_if_stale(&folder.name, source, trigger, ms(after));
                log(&folder.name, trigger, &done);
            }
            // An import that changes nothing writes nothing, so a picture
            // folder is looked at when the hub starts whatever its age.
            None => {
                let due = watched
                    .imported
                    .as_ref()
                    .is_none_or(|last| now_ms.saturating_sub(last.started_at) >= ms(RESCAN_EVERY));
                if due {
                    watched.importing = true;
                    watch.imports.push((folder, trigger));
                }
            }
        }
    }
}

fn log(root: &str, trigger: AutoTrigger, done: &Rescan) {
    match done {
        Rescan::Started(job_id) => info!(
            root,
            trigger = trigger.as_str(),
            job_id = %job_id,
            "a rescan starts on its own"
        ),
        Rescan::Failed(why) => warn!(root, error = %why, "a rescan could not be queued"),
        Rescan::Joined | Rescan::Busy | Rescan::NotDue => {}
    }
}

/// What every session shares of watching: how this run watches, what wakes
/// the thread, and what it last found.
#[derive(Debug)]
pub(crate) struct Watching {
    run: WatchRun,
    signals: Signals,
    receiver: Mutex<Option<Receiver<Signal>>>,
    shown: Mutex<(Option<String>, Vec<Shown>)>,
}

impl Watching {
    pub(crate) fn new(run: WatchRun) -> Self {
        let (signals, receiver) = Signals::new();
        Self {
            run,
            signals,
            receiver: Mutex::new(Some(receiver)),
            shown: Mutex::new((None, Vec::new())),
        }
    }

    pub(crate) fn run(&self) -> &WatchRun {
        &self.run
    }

    /// Hands the thread a signal. A test hands in events this way.
    pub(crate) fn send(&self, signal: Signal) {
        self.signals.send(signal);
    }

    /// What the thread last found: why it is off, or each folder.
    pub(crate) fn shown(&self) -> (Option<String>, Vec<Shown>) {
        self.shown
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn show(&self, off: Option<String>, folders: Vec<Shown>) {
        *self.shown.lock().unwrap_or_else(PoisonError::into_inner) = (off, folders);
    }
}

/// The thread that watches, and the one that imports picture folders for
/// it.
#[derive(Debug)]
pub(crate) struct Watcher {
    core: Arc<Core>,
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

impl Watcher {
    /// Starts the threads, on a run that watches.
    pub(crate) fn start(core: Arc<Core>) -> std::io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let receiver = core.watching().and_then(|watching| {
            watching
                .receiver
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take()
                .filter(|_| watching.run.on)
        });
        let mut threads = Vec::new();
        if let Some(receiver) = receiver {
            // An import can take a while: it runs on a thread of its own, so
            // that the watcher goes on hearing changes and queuing scans.
            let (asks, asked) = mpsc::channel::<(Folder, AutoTrigger)>();
            let (done, imported) = mpsc::channel::<(Key, Option<AutoRun>)>();
            let (core_, stop_) = (Arc::clone(&core), Arc::clone(&stop));
            threads.push(
                std::thread::Builder::new()
                    .name("ritornel-pictures".to_owned())
                    .spawn(move || import(&core_, &asked, &done, &stop_))?,
            );
            let lines = Lines {
                signals: receiver,
                asks,
                imported,
            };
            let (core_, stop_) = (Arc::clone(&core), Arc::clone(&stop));
            threads.push(
                std::thread::Builder::new()
                    .name("ritornel-watch".to_owned())
                    .spawn(move || watch(&core_, &lines, &stop_))?,
            );
        }
        Ok(Self {
            core,
            stop,
            threads,
        })
    }

    /// Stops the threads and waits for them: each ends after what it is
    /// doing.
    pub(crate) fn stop(mut self) {
        self.signal();
        // The watcher first: once it has gone, nothing more is asked of the
        // importer.
        for thread in self.threads.drain(..).rev() {
            let _ = thread.join();
        }
    }

    fn signal(&self) {
        self.stop.store(true, Ordering::SeqCst);
        self.core.wake_watch();
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.signal();
    }
}

/// Imports each picture folder the watcher asks for, and tells it when the
/// import ends, until the watcher has gone.
fn import(
    core: &Core,
    asked: &Receiver<(Folder, AutoTrigger)>,
    done: &Sender<(Key, Option<AutoRun>)>,
    stop: &AtomicBool,
) {
    for (folder, trigger) in asked {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        // Asked for before watching was turned off: the timer imports it
        // when it is turned on again. The watcher is told all the same.
        let run = core
            .watch_off()
            .is_none()
            .then(|| core.auto_import(&folder, trigger));
        if done.send((folder.key(), run)).is_err() {
            return;
        }
        core.wake_watch();
    }
}

/// What the watching thread hears from and speaks to.
struct Lines {
    signals: Receiver<Signal>,
    /// Imports for the importer.
    asks: Sender<(Folder, AutoTrigger)>,
    /// Imports that have ended, or were not run.
    imported: Receiver<(Key, Option<AutoRun>)>,
}

fn backend(run: &WatchRun, signals: Signals) -> Box<dyn Backend> {
    if run.injected {
        return Box::new(Silent);
    }
    match Native::start(signals) {
        Ok(native) => Box::new(native),
        Err(why) => {
            warn!(error = %why, "the system cannot watch folders here: they are rescanned on a timer alone");
            Box::new(Unavailable(why))
        }
    }
}

fn watch(core: &Core, lines: &Lines, stop: &AtomicBool) {
    let Some(watching) = core.watching() else {
        return;
    };
    let mut state: Option<Watch> = None;
    // The folders are read again when woken, and at least every minute.
    let mut look = true;
    let mut looked_ms = i64::MIN;
    loop {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        let now_ms = core.now_ms();
        let wait = match core.watch_off() {
            Some(why) => {
                if state.take().is_some() {
                    info!(why = %why, "stopped watching the library's folders");
                }
                watching.show(Some(why), Vec::new());
                IDLE
            }
            None => {
                let watch = state.get_or_insert_with(|| {
                    info!("watching the library's folders");
                    let mut watch = Watch::new(
                        backend(&watching.run, watching.signals.clone()),
                        &watching.run,
                    );
                    watch.skip(core.data_dir());
                    watch
                });
                for (key, run) in lines.imported.try_iter() {
                    watch.imported(&key, run);
                }
                if look || now_ms.saturating_sub(looked_ms) >= ms(IDLE) {
                    if let Some(found) = core.watched_folders() {
                        watch.refresh(found, now_ms);
                    }
                    (look, looked_ms) = (false, now_ms);
                }
                tick(core, watch, now_ms);
                for ask in watch.take_imports() {
                    // The importer has gone only if the server is stopping.
                    let _ = lines.asks.send(ask);
                }
                watching.show(None, watch.shown());
                watch
                    .next_wait(core.now_ms())
                    .map_or(IDLE, |wait| wait.min(IDLE))
            }
        };
        let Some((signals, dropped)) =
            receive(&lines.signals, &watching.signals.dropped, wait, SIGNALS)
        else {
            return;
        };
        // Events that come while watching is off are dropped: the timer
        // covers them when it is turned on again. Waking, or the system
        // losing track, has the folders looked at again either way.
        let now_ms = core.now_ms();
        for signal in signals {
            let again = match state.as_mut() {
                Some(watch) => watch.signal(signal, now_ms),
                None => matches!(signal, Signal::Wake | Signal::Lost(..)),
            };
            look |= again;
        }
        // What the dropped signals said is not known: it may have been that
        // the system lost track, so every folder is watched afresh and
        // rescanned.
        if dropped && let Some(watch) = state.as_mut() {
            let why = "too many changes came at once to hear each".to_owned();
            look |= watch.signal(Signal::Lost(Vec::new(), why), now_ms);
        }
    }
}

/// An automatic run as the status shows it, from the job that is its rescan.
pub(crate) fn auto_run(job: &ritornel_engine::Job) -> Option<AutoRun> {
    #[derive(serde::Deserialize)]
    struct Params {
        trigger: AutoTrigger,
    }
    let params: Params = serde_json::from_str(&job.params_json).ok()?;
    Some(AutoRun {
        trigger: params.trigger,
        started_at: job.started_at_ms.unwrap_or(job.created_at_ms),
        finished_at: job.finished_at_ms,
        state: crate::dispatch::job_state(job.state),
        job_id: Some(job.public_id),
    })
}

/// An import as the status shows it.
pub(crate) fn imported(trigger: AutoTrigger, started_at: i64, now_ms: i64, ok: bool) -> AutoRun {
    AutoRun {
        trigger,
        started_at,
        finished_at: Some(now_ms),
        state: if ok {
            JobState::Succeeded
        } else {
            JobState::Failed
        },
        job_id: None,
    }
}

#[cfg(test)]
mod tests;
