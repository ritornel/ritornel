//! What the Settings window shows, read from the server, and what it asks
//! the server to do. Every change is a command of the contract, with a key
//! of its own; nothing is kept here that the server does not hold.
//!
//! It reads everything again each time a connection is ready, and follows
//! the `jobs` topic for scans and imports as they run. It talks through the
//! app's one connection (`link.rs`), as the main window does.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{AnyWindowHandle, AppContext, Context, Entity, Subscription, Task};
use ritornel_client::{Error, idempotency_key};
use ritornel_contract::methods::{
    ArtistsImportPictures, ArtistsPictureFolders, HistorySetZones, HistoryZones, JobsCancel,
    JobsList, LastfmSetScrobbling, LastfmSignInFinish, LastfmSignInStart, LastfmSignOut,
    LastfmStatusCall, LastfmSync, LibraryCreate, LibrarySetWatching, RecordDismiss, ScanStart,
    ServerStatus as StatusCall, SourcesAdd, SourcesList,
};
use ritornel_contract::{
    DismissParams, ErrorCode, Event, Id, ImportPicturesParams, Job, JobCancelParams, JobState,
    JobTarget, KeyParams, LastfmStatus, LastfmSyncParams, LibraryCreateParams, LibraryCreated,
    Method, NoParams, PictureFolders, ReceiptStatus, ScanStartParams, ScrobblingParams, ServerRole,
    ServerStatus, SetZonesParams, SignInFinishParams, Source, SourceAdded, SourcesAddParams,
    WatchingParams, ZonePeriod,
};

use crate::link::{Link, LinkEvent};
use crate::net::{Net, no_library};
use crate::settings::words::{self, folder_name, is_ended};

/// The kind of job a Last.fm import is (apps/server/src/lastfm.rs).
const SYNC_JOB: &str = "lastfm.sync";

/// How long between two asks whether a sign-in is approved, as `ritornelctl` waits.
const ASK_EVERY: Duration = Duration::from_millis(3000);
/// How long a sign-in is waited for: a person who has not approved in ten minutes is not at the browser.
const SIGN_IN_WAIT: Duration = Duration::from_secs(10 * 60);

/// Where a sign-in to Last.fm stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SigningIn {
    Idle,
    Waiting { approval_url: String },
    Failed { message: String },
}

/// A group of the window, which says what went wrong in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Group {
    Record,
    Library,
    Pictures,
    Account,
    Scrobbling,
    Import,
    Zones,
}

/// Something being done that the window shows as under way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Busy {
    Adding,
    Pictures,
    Scrobbling,
    SigningOut,
    Zones,
    Syncing,
    Watching,
    Dismissing,
}

/// The zone being added, while its form is open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Adding {
    pub zone: String,
    /// `YYYY-MM-DD`, or empty for "always".
    pub from: String,
}

/// Where the listening time zones stand: editing stays off until the list
/// has been read, so a save cannot overwrite periods Settings never saw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Zones {
    Loading,
    NoLibrary,
    Failed(String),
    Loaded(Vec<ZonePeriod>),
}

impl Zones {
    pub(crate) fn loaded(&self) -> Option<&[ZonePeriod]> {
        match self {
            Self::Loaded(periods) => Some(periods),
            Self::Loading | Self::NoLibrary | Self::Failed(_) => None,
        }
    }

    pub(crate) fn is_loaded(&self) -> bool {
        self.loaded().is_some()
    }
}

pub(crate) struct Model {
    link: Entity<Link>,
    /// The music folders, or `None` until read.
    pub sources: Option<Vec<Source>>,
    /// The newest scan of each source, by its ID.
    pub scans: HashMap<Id, Job>,
    sources_read: u64,
    pub pictures: Option<PictureFolders>,
    pub lastfm: Option<LastfmStatus>,
    pub zones: Zones,
    zones_read: u64,
    pub status: Option<ServerStatus>,
    /// The data directory, as this app looks in it.
    pub data_dir: Option<PathBuf>,
    pub signing_in: SigningIn,
    /// What is under way, keyed to the operation that started it.
    busy: HashMap<Busy, u64>,
    busy_op: u64,
    /// What went wrong last, by the group it went wrong in.
    pub problems: HashMap<Group, String>,
    /// The zone chosen in the list, for the − button.
    pub chosen: Option<usize>,
    /// The zone being added, while its form is open.
    pub adding: Option<Adding>,
    /// The sign-in being waited for: dropping it stops the waiting.
    sign_in: Option<Task<()>>,
    /// Suppresses opening external apps (such as a browser) during scripted
    /// runs.
    pub quiet: bool,
    _link: Subscription,
}

/// What a group says when it is used before there is a library.
pub(crate) const NO_LIBRARY_YET: &str = "There is no library yet. Add a music folder first.";

/// The words of what went wrong, for a line under the group it went wrong in.
fn trouble(error: &Error) -> String {
    match error {
        _ if no_library(error) => NO_LIBRARY_YET.to_owned(),
        Error::Call(rpc) => rpc.message.clone(),
        other => other.to_string(),
    }
}

/// What went wrong, in the words the window shows.
pub(crate) struct Trouble(String);

impl From<Error> for Trouble {
    fn from(error: Error) -> Self {
        Self(trouble(&error))
    }
}

/// Whether the server said the name or the folder is taken.
fn conflict(error: &Error) -> bool {
    matches!(error, Error::Call(rpc) if rpc.data.code == ErrorCode::Conflict)
}

/// A new command's key.
fn key() -> Result<Id, Error> {
    idempotency_key()
}

/// Makes this server's library, in its own data folder, unless another
/// client has just made one.
async fn make_library(net: &Arc<Net>) -> Result<(), Error> {
    let params = LibraryCreateParams {
        idempotency_key: key()?,
    };
    made_or_found(ask::<LibraryCreate>(net, params).await)
}

/// Whether there is a library now: the one asked for, or one another
/// client made meanwhile, which the folder goes into instead.
fn made_or_found(created: Result<LibraryCreated, Error>) -> Result<(), Error> {
    match created {
        // Refused as a library is there already: one made meanwhile, which
        // the conflict names. Any other conflict stands.
        Err(Error::Call(rpc))
            if rpc.data.code == ErrorCode::Conflict
                && rpc.data.details.contains_key("libraryId") =>
        {
            Ok(())
        }
        Err(error) => Err(error),
        Ok(_) => Ok(()),
    }
}

/// Adds a folder with `add`. On a server that holds no library, `make`,
/// when there is one, makes it first: the refusal wrote nothing, so the
/// same command is sent again.
async fn add_making_library<Added, Made>(
    params: SourcesAddParams,
    add: impl Fn(SourcesAddParams) -> Added,
    make: Option<impl FnOnce() -> Made>,
) -> Result<SourceAdded, Error>
where
    Added: Future<Output = Result<SourceAdded, Error>>,
    Made: Future<Output = Result<(), Error>>,
{
    match (add(params.clone()).await, make) {
        (Err(error), Some(make)) if no_library(&error) => {
            make().await?;
            add(params).await
        }
        (added, _) => added,
    }
}

/// Calls `M` on the connection's runtime.
fn ask<M: Method>(
    net: &Arc<Net>,
    params: M::Params,
) -> impl Future<Output = Result<M::Result, Error>> + use<M>
where
    M::Params: Send + Sync + 'static,
    M::Result: Send + 'static,
{
    let client = Arc::clone(&net.client);
    let handle = net
        .rt()
        .spawn(async move { client.call::<M>(&params).await });
    async move {
        handle
            .await
            .unwrap_or_else(|join| Err(Error::Transport(join.to_string())))
    }
}

/// The source a scan job is of, or `None` for any other job.
fn scanned(job: &Job) -> Option<Id> {
    match (job.kind.as_str(), job.target) {
        ("scan", Some(JobTarget::Source { id })) => Some(id),
        _ => None,
    }
}

/// Progress within one run of a scan, including the walk before the total is known.
fn scan_progress(job: &Job) -> (u8, bool, u64, u64, u64, u64, u64) {
    let phase = match job.state {
        JobState::Queued => 0,
        JobState::Running => 1,
        JobState::Cancelling => 2,
        _ => 3,
    };
    (
        phase,
        job.total.is_some(),
        job.scan.map_or(0, |scan| scan.files),
        job.done,
        job.skipped,
        job.failed,
        job.bytes,
    )
}

/// A list is newest first. An event cannot order two jobs created in the same millisecond.
fn keep_scan(scans: &mut HashMap<Id, Job>, source: Id, job: Job, from_list: bool) -> bool {
    if let Some(kept) = scans.get(&source) {
        if job.job_id == kept.job_id {
            if scan_progress(&job) <= scan_progress(kept) {
                return false;
            }
        } else if !from_list && job.created_at <= kept.created_at {
            return false;
        }
    }
    scans.insert(source, job);
    true
}

impl Model {
    /// Starts reading from the app's connection, now and each time it connects again.
    pub(crate) fn new(link: Entity<Link>, quiet: bool, cx: &mut Context<Self>) -> Self {
        let followed = cx.subscribe(&link, |model, _, event: &LinkEvent, cx| match event {
            LinkEvent::Connected(_) => {
                model.scans.clear();
                model.busy.clear();
                model.zones = Zones::Loading;
                model.chosen = None;
                model.adding = None;
                model.problems.remove(&Group::Zones);
                model.read_all(cx);
                cx.notify();
            }
            // A library made since it connected can now be read, and what
            // was refused for want of one no longer stands.
            LinkEvent::Restated(_) => {
                model.problems.retain(|_, said| said != NO_LIBRARY_YET);
                model.read_all(cx);
            }
            LinkEvent::Away => {
                model.zones = Zones::Loading;
                model.chosen = None;
                model.adding = None;
                cx.notify();
            }
            LinkEvent::Server(Event::Jobs(event)) => model.heard_job(event.job.clone(), cx),
            LinkEvent::Server(_) => cx.notify(),
        });
        let data_dir = link.read(cx).data_dir();
        let mut model = Self {
            link,
            sources: None,
            scans: HashMap::new(),
            sources_read: 0,
            pictures: None,
            lastfm: None,
            zones: Zones::Loading,
            zones_read: 0,
            status: None,
            data_dir,
            signing_in: SigningIn::Idle,
            busy: HashMap::new(),
            busy_op: 0,
            problems: HashMap::new(),
            chosen: None,
            adding: None,
            sign_in: None,
            quiet,
            _link: followed,
        };
        model.read_all(cx);
        model
    }

    pub(crate) fn is_busy(&self, what: Busy) -> bool {
        self.busy.contains_key(&what)
    }

    fn net(&self, cx: &gpui::App) -> Option<Arc<Net>> {
        self.link.read(cx).net()
    }

    /// The session's server, while there is one.
    pub(crate) fn server(&self, cx: &gpui::App) -> Option<Arc<Net>> {
        self.net(cx)
    }

    /// Whether there is a connection, whether one is being looked for, and why not.
    pub(crate) fn link<'a>(&self, cx: &'a gpui::App) -> &'a Link {
        self.link.read(cx)
    }

    /// Whether what the window is first fitted to has been read.
    pub(crate) fn ready(&self, cx: &gpui::App) -> bool {
        self.net(cx).is_some() && self.sources.is_some() && self.lastfm.is_some()
    }

    pub(crate) fn read_all(&mut self, cx: &mut Context<Self>) {
        self.read_sources(cx);
        self.read_pictures(cx);
        self.read_lastfm(cx);
        self.read_zones(cx);
        self.read_status(cx);
    }

    /// Runs `work` against the server, and hands what it found to `then`.
    fn reading<T: 'static, F>(
        &mut self,
        cx: &mut Context<Self>,
        work: impl FnOnce(Arc<Net>) -> F,
        then: impl FnOnce(&mut Self, Result<T, Error>, &mut Context<Self>) + 'static,
    ) where
        F: Future<Output = Result<T, Error>> + 'static,
    {
        let Some(net) = self.net(cx) else { return };
        let work = work(Arc::clone(&net));
        cx.spawn(async move |model, cx| {
            let found = work.await;
            let _ = model.update(cx, |model, cx| {
                if model
                    .net(cx)
                    .is_some_and(|current| current.same_session(&net))
                {
                    then(model, found, cx);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(crate) fn read_sources(&mut self, cx: &mut Context<Self>) {
        self.sources_read += 1;
        let asked = self.sources_read;
        self.reading(
            cx,
            |net| {
                let (sources, jobs) = (
                    ask::<SourcesList>(&net, NoParams {}),
                    ask::<JobsList>(&net, NoParams {}),
                );
                async move { Ok((sources.await?, jobs.await?)) }
            },
            move |model, found, _| {
                if asked != model.sources_read {
                    return;
                }
                match found {
                    // Nothing to read until the first folder makes the library.
                    Err(error) if no_library(&error) => model.sources = Some(Vec::new()),
                    Ok((sources, jobs)) => {
                        model.sources = Some(sources.sources);
                        let mut seen = HashSet::new();
                        for job in jobs.jobs {
                            if let Some(source) = scanned(&job)
                                && seen.insert(source)
                            {
                                keep_scan(&mut model.scans, source, job, true);
                            }
                        }
                    }
                    Err(error) => {
                        model.problems.insert(Group::Library, trouble(&error));
                    }
                }
            },
        );
    }

    pub(crate) fn read_pictures(&mut self, cx: &mut Context<Self>) {
        self.reading(
            cx,
            |net| ask::<ArtistsPictureFolders>(&net, NoParams {}),
            |model, found, _| match found {
                Ok(pictures) => model.pictures = Some(pictures),
                Err(error) if no_library(&error) => {
                    model.pictures = Some(PictureFolders {
                        folders: Vec::new(),
                        last_import_at: None,
                        last_import: None,
                    });
                }
                Err(error) => {
                    model.problems.insert(Group::Pictures, trouble(&error));
                }
            },
        );
    }

    pub(crate) fn read_lastfm(&mut self, cx: &mut Context<Self>) {
        if self.net(cx).is_none() {
            return;
        }
        self.problems.remove(&Group::Account);
        cx.notify();
        self.reading(
            cx,
            |net| ask::<LastfmStatusCall>(&net, NoParams {}),
            |model, found, _| match found {
                Ok(status) => model.lastfm = Some(status),
                Err(error) => {
                    model.problems.insert(Group::Account, trouble(&error));
                }
            },
        );
    }

    pub(crate) fn read_zones(&mut self, cx: &mut Context<Self>) {
        if self.net(cx).is_none() {
            return;
        }
        self.zones_read += 1;
        let asked = self.zones_read;
        self.zones = Zones::Loading;
        self.chosen = None;
        self.adding = None;
        self.problems.remove(&Group::Zones);
        cx.notify();
        self.reading(
            cx,
            |net| ask::<HistoryZones>(&net, NoParams {}),
            move |model, found, _| {
                if asked != model.zones_read {
                    return;
                }
                match found {
                    Ok(zones) => {
                        model.zones = Zones::Loaded(zones.periods);
                        model.chosen = None;
                        model.problems.remove(&Group::Zones);
                    }
                    Err(error) if no_library(&error) => {
                        model.zones = Zones::NoLibrary;
                        model.chosen = None;
                        model.adding = None;
                        model.problems.remove(&Group::Zones);
                    }
                    Err(error) => {
                        model.zones = Zones::Failed(trouble(&error));
                        model.chosen = None;
                        model.adding = None;
                        model.problems.remove(&Group::Zones);
                    }
                }
            },
        );
    }

    pub(crate) fn read_status(&mut self, cx: &mut Context<Self>) {
        self.reading(
            cx,
            |net| ask::<StatusCall>(&net, NoParams {}),
            // The About section says what it has.
            |model, found, _| {
                if let Ok(status) = found {
                    model.status = Some(status);
                }
            },
        );
    }

    fn heard_job(&mut self, job: Job, cx: &mut Context<Self>) {
        if let Some(source) = scanned(&job) {
            let prior = self.scans.get(&source);
            let new_job = prior.is_none_or(|kept| kept.job_id != job.job_id);
            let tied = prior.is_some_and(|kept| new_job && kept.created_at == job.created_at);
            let ended = is_ended(&job);
            let changed = keep_scan(&mut self.scans, source, job, false);
            if tied || (changed && (new_job || ended)) {
                self.read_sources(cx);
                if ended {
                    self.read_status(cx);
                }
            }
        } else if job.kind == SYNC_JOB && is_ended(&job) {
            self.read_lastfm(cx);
            // A failed import stops the record, and one that works starts it.
            self.read_status(cx);
        } else if job.kind == SYNC_JOB
            && let Some(lastfm) = &mut self.lastfm
            && !lastfm.import.running
        {
            lastfm.import.running = true;
        }
        cx.notify();
    }

    /// Does `work`, showing `busy` meanwhile and what went wrong under
    /// `group`, and hands what it found to `then`. `true` once it has done it.
    fn doing<T: 'static, F>(
        &mut self,
        busy: Busy,
        group: Group,
        cx: &mut Context<Self>,
        work: impl FnOnce(Arc<Net>) -> F,
        then: impl FnOnce(&mut Self, T, &mut Context<Self>) + 'static,
    ) -> Task<bool>
    where
        F: Future<Output = Result<T, Trouble>> + 'static,
    {
        let Some(net) = self.net(cx) else {
            return Task::ready(false);
        };
        self.busy_op += 1;
        let op = self.busy_op;
        self.busy.insert(busy, op);
        self.problems.remove(&group);
        cx.notify();
        let work = work(Arc::clone(&net));
        cx.spawn(async move |model, cx| {
            let found = work.await;
            model
                .update(cx, |model, cx| {
                    let cleared = model.busy.get(&busy) == Some(&op);
                    if cleared {
                        model.busy.remove(&busy);
                    }
                    if model
                        .net(cx)
                        .is_none_or(|current| !current.same_session(&net))
                    {
                        if cleared {
                            cx.notify();
                        }
                        return false;
                    }
                    cx.notify();
                    match found {
                        Ok(found) => {
                            then(model, found, cx);
                            true
                        }
                        Err(Trouble(said)) => {
                            model.problems.insert(group, said);
                            false
                        }
                    }
                })
                .unwrap_or(false)
        })
    }

    // ---- The library

    /// Scans every music folder on this server. A folder being scanned carries on.
    pub(crate) fn scan_all(&mut self, cx: &mut Context<Self>) {
        self.problems.remove(&Group::Library);
        let here: Vec<Id> = self
            .sources
            .iter()
            .flatten()
            .filter(|source| source.path.is_some())
            .map(|source| source.source_id)
            .collect();
        for source_id in here {
            self.reading(
                cx,
                |net| async move {
                    let params = ScanStartParams {
                        idempotency_key: key()?,
                        source_id,
                    };
                    ask::<ScanStart>(&net, params).await
                },
                move |model, started, cx| match started {
                    Ok(started) => model.heard_job(started.job, cx),
                    Err(error) => {
                        model.problems.insert(Group::Library, trouble(&error));
                    }
                },
            );
        }
    }

    /// Stops the scans that are running. What they have done stays.
    pub(crate) fn stop_scans(&mut self, cx: &mut Context<Self>) {
        let running: Vec<Id> = self
            .scans
            .values()
            .filter(|job| !is_ended(job))
            .map(|job| job.job_id)
            .collect();
        for job_id in running {
            self.reading(
                cx,
                |net| async move {
                    let params = JobCancelParams {
                        idempotency_key: key()?,
                        job_id,
                    };
                    ask::<JobsCancel>(&net, params).await
                },
                |_, _, _| {},
            );
        }
    }

    /// Asks for a folder with the system's picker, as a sheet on `window`, and adds it.
    pub(crate) fn choose_folder(&mut self, window: AnyWindowHandle, cx: &mut Context<Self>) {
        cx.spawn(async move |model, cx| {
            let path = crate::platform::choose_folder(
                window,
                "Add".to_owned(),
                "Choose a folder of music for Ritornel to read. It never changes the files."
                    .to_owned(),
                cx,
            )
            .await;
            let _ = model.update(cx, |model, cx| match path {
                Ok(Some(path)) => { model.add_folder(path.display().to_string(), cx).detach(); }
                Ok(None) => {}
                Err(error) => {
                    model.problems.insert(Group::Library, format!(
                        "The folder chooser failed ({error}). Try again or run ritornelctl scan with the folder path."
                    ));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Adds a folder as a source and scans it. A name the library has is
    /// given a number. On a server with no library, the first folder makes
    /// it: in the server's data folder, never in the music folder, which is
    /// only read.
    pub(crate) fn add_folder(&mut self, path: String, cx: &mut Context<Self>) -> Task<bool> {
        let known = self
            .sources
            .iter()
            .flatten()
            .any(|source| source.path.as_deref() == Some(path.as_str()));
        self.doing(
            Busy::Adding,
            Group::Library,
            cx,
            move |net| async move {
                if known {
                    return Err(Trouble("That folder is in the library already.".to_owned()));
                }
                let base = folder_name(&path, "Music");
                let mut n = 1;
                let added = loop {
                    let name = if n == 1 {
                        base.clone()
                    } else {
                        format!("{base} {n}")
                    };
                    let params = SourcesAddParams {
                        idempotency_key: key()?,
                        name,
                        path: path.clone(),
                    };
                    // Only a server that is linked to none makes a library of
                    // its own.
                    let makes = net.role == ServerRole::Unassigned;
                    let added = add_making_library(
                        params,
                        |params| ask::<SourcesAdd>(&net, params),
                        makes.then_some(|| make_library(&net)),
                    );
                    match added.await {
                        Ok(added) => break added.source,
                        Err(error) if conflict(&error) && n < 20 => n += 1,
                        Err(error) => return Err(error.into()),
                    }
                };
                let params = ScanStartParams {
                    idempotency_key: key()?,
                    source_id: added.source_id,
                };
                let started = ask::<ScanStart>(&net, params).await?;
                Ok((added, started.job))
            },
            |model, (source, job), cx| {
                model.heard_job(job, cx);
                let sources = model.sources.get_or_insert_with(Vec::new);
                if !sources.iter().any(|row| row.source_id == source.source_id) {
                    sources.push(source);
                }
                model.read_sources(cx);
            },
        )
    }

    /// Turns watching the folders for changes on or off, for this machine.
    pub(crate) fn set_watching(&mut self, on: bool, cx: &mut Context<Self>) {
        self.doing(
            Busy::Watching,
            Group::Library,
            cx,
            move |net| async move {
                let params = WatchingParams {
                    idempotency_key: key()?,
                    on,
                };
                Ok(ask::<LibrarySetWatching>(&net, params).await?)
            },
            |model, set, _| {
                if let Some(status) = &mut model.status {
                    status.watch = Some(set.watch);
                }
            },
        )
        .detach();
    }

    /// Sets aside the scrobbles that ended unknown, withdrawn or refused so
    /// far: every scrobble note and warning.
    pub(crate) fn dismiss_record(&mut self, cx: &mut Context<Self>) {
        self.doing(
            Busy::Dismissing,
            Group::Record,
            cx,
            move |net| async move {
                let params = DismissParams {
                    idempotency_key: key()?,
                };
                Ok(ask::<RecordDismiss>(&net, params).await?)
            },
            |model, dismissed, _| {
                if let Some(status) = &mut model.status {
                    status.record = Some(dismissed.record);
                }
            },
        )
        .detach();
    }

    // ---- Artist pictures

    /// The picture folder on this server, if there is one: the last chosen.
    pub(crate) fn picture_folder(&self) -> Option<(String, String)> {
        self.pictures
            .as_ref()?
            .folders
            .iter()
            .rev()
            .find_map(|folder| Some((folder.name.clone(), folder.path.clone()?)))
    }

    /// Asks for the picture folder with the system's picker, and imports from it.
    pub(crate) fn choose_pictures(&mut self, window: AnyWindowHandle, cx: &mut Context<Self>) {
        cx.spawn(async move |model, cx| {
            let path = crate::platform::choose_folder(
                window,
                "Choose".to_owned(),
                "Choose the folder of artist pictures. Ritornel reads it and never changes it."
                    .to_owned(),
                cx,
            )
            .await;
            let _ = model.update(cx, |model, cx| match path {
                Ok(Some(path)) => { model.import_pictures(Some(path.display().to_string()), cx); }
                Ok(None) => {}
                Err(error) => {
                    model.problems.insert(Group::Pictures, format!(
                        "The folder chooser failed ({error}). Try again or run ritornelctl artists pictures with the folder path."
                    ));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Imports the portraits of the folder at `path`, by default the one there is.
    pub(crate) fn import_pictures(&mut self, path: Option<String>, cx: &mut Context<Self>) {
        let Some(folder) = path.or_else(|| self.picture_folder().map(|(_, path)| path)) else {
            return;
        };
        let known = self.pictures.as_ref().and_then(|pictures| {
            pictures
                .folders
                .iter()
                .find(|known| known.path.as_deref() == Some(folder.as_str()))
                .map(|known| known.name.clone())
        });
        self.doing(
            Busy::Pictures,
            Group::Pictures,
            cx,
            move |net| async move {
                let base = known
                    .clone()
                    .unwrap_or_else(|| folder_name(&folder, "Artists"));
                let mut n = 1;
                loop {
                    let name = if n == 1 || known.is_some() {
                        base.clone()
                    } else {
                        format!("{base} {n}")
                    };
                    let params = ImportPicturesParams {
                        idempotency_key: key()?,
                        name,
                        path: folder.clone(),
                    };
                    match ask::<ArtistsImportPictures>(&net, params).await {
                        Ok(_) => break,
                        Err(error) if conflict(&error) && known.is_none() && n < 20 => n += 1,
                        Err(error) => return Err(error.into()),
                    }
                }
                Ok(ask::<ArtistsPictureFolders>(&net, NoParams {}).await?)
            },
            |model, pictures, _| model.pictures = Some(pictures),
        )
        .detach();
    }

    // ---- Last.fm

    /// Opens an address in the browser: only `https`, and nothing while a
    /// script poses the window.
    fn open_in_browser(&self, url: &str, cx: &mut gpui::App) {
        if url.starts_with("https://") && !self.quiet {
            cx.open_url(url);
        }
    }

    /// Starts a sign-in: opens Last.fm's page and waits for it to be approved there.
    pub(crate) fn sign_in(&mut self, cx: &mut Context<Self>) {
        let Some(net) = self.net(cx) else { return };
        self.problems.remove(&Group::Account);
        // Replacing the task stops the waiting for any sign-in before.
        self.sign_in = Some(cx.spawn(async move |model, cx| {
            let failed: Result<(), Trouble> = async {
                let started = ask::<LastfmSignInStart>(&net, NoParams {}).await?;
                let url = started.approval_url.clone();
                model
                    .update(cx, |model, cx| {
                        model.signing_in = SigningIn::Waiting {
                            approval_url: url.clone(),
                        };
                        model.open_in_browser(&url, cx);
                        cx.notify();
                    })
                    .map_err(|_| Trouble("the window closed".to_owned()))?;
                let until = Instant::now() + SIGN_IN_WAIT;
                loop {
                    let params = SignInFinishParams {
                        idempotency_key: key()?,
                        sign_in: started.sign_in,
                    };
                    let finished = ask::<LastfmSignInFinish>(&net, params).await?;
                    if finished.receipt.status == ReceiptStatus::Done {
                        return Ok(());
                    }
                    if Instant::now() > until {
                        return Err(Trouble(
                            "Last.fm did not hear from you in ten minutes. Sign in again when you are ready."
                                .to_owned(),
                        ));
                    }
                    cx.background_executor().timer(ASK_EVERY).await;
                }
            }
            .await;
            let _ = model.update(cx, |model, cx| {
                match failed {
                    Ok(()) => {
                        model.signing_in = SigningIn::Idle;
                        model.read_lastfm(cx);
                    }
                    Err(Trouble(message)) => {
                        model.signing_in = SigningIn::Failed { message };
                    }
                }
                model.sign_in = None;
                cx.notify();
            });
        }));
    }

    /// Stops waiting for a sign-in.
    pub(crate) fn cancel_sign_in(&mut self, cx: &mut Context<Self>) {
        self.sign_in = None;
        self.signing_in = SigningIn::Idle;
        cx.notify();
    }

    /// Opens Last.fm's page for the sign-in being waited for, again.
    pub(crate) fn open_approval_again(&mut self, cx: &mut Context<Self>) {
        if let SigningIn::Waiting { approval_url } = &self.signing_in {
            let url = approval_url.clone();
            self.open_in_browser(&url, cx);
        }
    }

    pub(crate) fn sign_out(&mut self, cx: &mut Context<Self>) {
        self.doing(
            Busy::SigningOut,
            Group::Account,
            cx,
            |net| async move {
                let params = KeyParams {
                    idempotency_key: key()?,
                };
                ask::<LastfmSignOut>(&net, params).await?;
                Ok(ask::<LastfmStatusCall>(&net, NoParams {}).await?)
            },
            |model, status, _| model.lastfm = Some(status),
        )
        .detach();
    }

    pub(crate) fn set_scrobbling(&mut self, on: bool, cx: &mut Context<Self>) {
        self.doing(
            Busy::Scrobbling,
            Group::Scrobbling,
            cx,
            move |net| async move {
                let params = ScrobblingParams {
                    idempotency_key: key()?,
                    on,
                };
                ask::<LastfmSetScrobbling>(&net, params).await?;
                Ok(ask::<LastfmStatusCall>(&net, NoParams {}).await?)
            },
            |model, status, _| model.lastfm = Some(status),
        )
        .detach();
    }

    /// Imports what is new in the account's history now.
    pub(crate) fn sync_now(&mut self, cx: &mut Context<Self>) {
        self.doing(
            Busy::Syncing,
            Group::Import,
            cx,
            |net| async move {
                let params = LastfmSyncParams {
                    idempotency_key: key()?,
                    full: false,
                };
                ask::<LastfmSync>(&net, params).await?;
                Ok(ask::<LastfmStatusCall>(&net, NoParams {}).await?)
            },
            |model, status, _| model.lastfm = Some(status),
        )
        .detach();
    }

    /// Replaces the zones with `periods`, and reads back what the server keeps.
    /// Never sends a replacement unless the current list has been read.
    fn set_zones(&mut self, periods: Vec<ZonePeriod>, cx: &mut Context<Self>) -> Task<bool> {
        if !self.zones.is_loaded() {
            return Task::ready(false);
        }
        self.zones_read += 1;
        let asked = self.zones_read;
        self.doing(
            Busy::Zones,
            Group::Zones,
            cx,
            move |net| async move {
                let params = SetZonesParams {
                    idempotency_key: key()?,
                    periods,
                };
                ask::<HistorySetZones>(&net, params).await?;
                match ask::<HistoryZones>(&net, NoParams {}).await {
                    Ok(zones) => Ok(Ok(zones)),
                    Err(error) => Ok(Err(trouble(&error))),
                }
            },
            move |model, read, cx| {
                model.read_lastfm(cx);
                if asked != model.zones_read {
                    return;
                }
                match read {
                    Ok(zones) => {
                        model.zones = Zones::Loaded(zones.periods);
                        model.chosen = None;
                    }
                    Err(reason) => {
                        model.zones = Zones::Failed(reason);
                        model.chosen = None;
                        model.adding = None;
                    }
                }
            },
        )
    }

    /// Opens the form for a zone: the system's own, from today, or from
    /// always when it is the first.
    pub(crate) fn start_adding(&mut self, clock: &words::Clock, cx: &mut Context<Self>) {
        let Some(zones) = self.zones.loaded() else {
            return;
        };
        if self.is_busy(Busy::Zones) {
            return;
        }
        let first = zones.is_empty();
        self.adding = Some(Adding {
            zone: words::system_zone(),
            from: if first {
                String::new()
            } else {
                words::today(clock)
            },
        });
        cx.notify();
    }

    /// Adds the zone of the form open, and closes it once the server keeps it.
    pub(crate) fn add(&mut self, cx: &mut Context<Self>) {
        if self.is_busy(Busy::Zones) {
            return;
        }
        let Some(adding) = self.adding.clone() else {
            return;
        };
        let Some(zones) = self.zones.loaded() else {
            return;
        };
        let period = ZonePeriod {
            zone: adding.zone,
            from: (!adding.from.is_empty()).then_some(adding.from),
            until: None,
        };
        let mut periods = zones.to_vec();
        periods.push(period);
        let added = self.set_zones(periods, cx);
        cx.spawn(async move |model, cx| {
            if added.await {
                let _ = model.update(cx, |model, cx| {
                    model.adding = None;
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// Removes the zone chosen, and lets go of it once the server has.
    pub(crate) fn remove(&mut self, cx: &mut Context<Self>) {
        if self.is_busy(Busy::Zones) {
            return;
        }
        let Some(chosen) = self.chosen else { return };
        let Some(zones) = self.zones.loaded() else {
            return;
        };
        if chosen >= zones.len() {
            self.chosen = None;
            cx.notify();
            return;
        }
        let periods: Vec<ZonePeriod> = zones
            .iter()
            .enumerate()
            .filter(|(at, _)| *at != chosen)
            .map(|(_, period)| period.clone())
            .collect();
        let removed = self.set_zones(periods, cx);
        cx.spawn(async move |model, cx| {
            if removed.await {
                let _ = model.update(cx, |model, cx| {
                    model.chosen = None;
                    cx.notify();
                });
            }
        })
        .detach();
    }
}

/// A model for a window, on the app's connection.
pub(crate) fn new(link: Entity<Link>, quiet: bool, cx: &mut gpui::App) -> Entity<Model> {
    cx.new(|cx| Model::new(link, quiet, cx))
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::future::ready;

    use futures::executor::block_on;
    use ritornel_contract::RpcError;
    use serde_json::json;

    use super::*;

    fn params() -> SourcesAddParams {
        SourcesAddParams {
            idempotency_key: Id::from_bytes([0xa1; 16]),
            name: "Music".to_owned(),
            path: "/Volumes/Records/Music".to_owned(),
        }
    }

    fn refused(code: ErrorCode, reason: Option<&str>) -> Error {
        let error = RpcError::new(code, "refused");
        Error::Call(match reason {
            Some(reason) => error.with_detail("reason", reason),
            None => error,
        })
    }

    fn no_library_yet() -> Error {
        refused(ErrorCode::Unavailable, Some("no_library"))
    }

    fn added(params: &SourcesAddParams) -> Result<SourceAdded, Error> {
        serde_json::from_value(json!({
            "commandId": params.idempotency_key,
            "status": "done",
            "revision": 2,
            "source": {
                "sourceId": Id::from_bytes([0xb2; 16]),
                "name": params.name,
                "kind": "folder",
                "path": params.path,
            },
        }))
        .map_err(|error| Error::Protocol(error.to_string()))
    }

    #[test]
    fn the_first_folder_makes_the_library_and_is_sent_again_as_it_was() -> Result<(), Error> {
        let library = Cell::new(false);
        let sent = RefCell::new(Vec::new());
        let answer = block_on(add_making_library(
            params(),
            |params| {
                sent.borrow_mut().push(params.clone());
                ready(if library.get() {
                    added(&params)
                } else {
                    Err(no_library_yet())
                })
            },
            Some(|| {
                library.set(true);
                ready(Ok(()))
            }),
        ))?;
        assert_eq!(
            answer.source.path.as_deref(),
            Some("/Volumes/Records/Music")
        );
        assert_eq!(*sent.borrow(), [params(), params()]);
        Ok(())
    }

    #[test]
    fn a_folder_refused_for_another_reason_makes_no_library() {
        let made = Cell::new(false);
        let make = || {
            made.set(true);
            ready(Ok(()))
        };
        // Unavailable, but not for want of a library.
        let busy = || refused(ErrorCode::Unavailable, None);
        let answer = block_on(add_making_library(
            params(),
            |_| ready(Err(busy())),
            Some(make),
        ));
        assert!(matches!(answer, Err(Error::Call(rpc)) if rpc.data.code == ErrorCode::Unavailable));
        assert!(!made.get());
    }

    #[test]
    fn a_library_that_cannot_be_made_is_said_and_the_folder_not_sent_again() {
        let sent = Cell::new(0);
        let answer = block_on(add_making_library(
            params(),
            |_| {
                sent.set(sent.get() + 1);
                ready(Err(no_library_yet()))
            },
            Some(|| ready(Err(refused(ErrorCode::Internal, None)))),
        ));
        assert!(matches!(answer, Err(Error::Call(rpc)) if rpc.data.code == ErrorCode::Internal));
        assert_eq!(sent.get(), 1);
    }

    #[test]
    fn a_server_linked_to_another_makes_no_library_of_its_own() {
        let sent = Cell::new(0);
        let answer = block_on(add_making_library(
            params(),
            |_| {
                sent.set(sent.get() + 1);
                ready(Err(no_library_yet()))
            },
            None::<fn() -> std::future::Ready<Result<(), Error>>>,
        ));
        assert!(matches!(&answer, Err(error) if no_library(error)));
        assert_eq!(sent.get(), 1);
    }

    #[test]
    fn only_a_library_another_client_made_meanwhile_will_do() {
        let theirs = Error::Call(
            RpcError::new(
                ErrorCode::Conflict,
                "there is a library on this server already",
            )
            .with_detail("libraryId", Id::from_bytes([0xc3; 16]).to_string()),
        );
        assert!(made_or_found(Err(theirs)).is_ok());
        // One in the data folder that another server holds is not this one's.
        assert!(made_or_found(Err(refused(ErrorCode::Conflict, None))).is_err());
        assert!(made_or_found(Err(refused(ErrorCode::Forbidden, None))).is_err());
    }

    #[test]
    fn only_wanting_a_library_reads_as_having_none() {
        assert!(no_library(&no_library_yet()));
        assert!(!no_library(&refused(ErrorCode::Unavailable, None)));
        assert!(!no_library(&refused(
            ErrorCode::Invalid,
            Some("no_library")
        )));
        assert_eq!(
            trouble(&no_library_yet()),
            "There is no library yet. Add a music folder first."
        );
    }

    #[test]
    fn a_start_reply_after_completion_leaves_the_scan_finished() -> Result<(), serde_json::Error> {
        let id = Id::from_bytes([1; 16]);
        let queued: Job = serde_json::from_value(serde_json::json!({
            "jobId": id, "kind": "scan", "state": "queued", "createdAt": 1,
            "done": 0, "failed": 0, "skipped": 0, "bytes": 0
        }))?;
        let mut scans = HashMap::new();
        let mut done = queued.clone();
        done.state = JobState::Succeeded;
        assert!(keep_scan(&mut scans, id, done, false));
        assert!(!keep_scan(&mut scans, id, queued.clone(), false));
        assert!(scans.get(&id).is_some_and(is_ended));

        let mut next = queued.clone();
        next.job_id = Id::from_bytes([2; 16]);
        assert!(!keep_scan(&mut scans, id, next.clone(), false));
        assert!(keep_scan(&mut scans, id, next.clone(), true));
        assert!(!keep_scan(&mut scans, id, queued, false));
        assert_eq!(scans.get(&id), Some(&next));

        let mut walking = next;
        walking.state = JobState::Running;
        let mut reading = walking.clone();
        reading.total = Some(5);
        assert!(keep_scan(&mut scans, id, reading.clone(), false));
        assert!(!keep_scan(&mut scans, id, walking, true));
        let mut later = reading.clone();
        later.skipped = 4;
        later.failed = 1;
        assert!(keep_scan(&mut scans, id, later.clone(), false));
        assert!(!keep_scan(&mut scans, id, reading, true));
        assert_eq!(scans.get(&id), Some(&later));
        Ok(())
    }

    mod flow {
        use std::collections::HashMap;
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        use std::sync::{Arc, Mutex, PoisonError};
        use std::time::{Duration, Instant};

        use ritornel_contract::{
            CONTRACT_VERSION, EVENT_METHOD, Event, Frame, FreeDisk, Id, ImportSchedule, Job,
            JobState, JobTarget, LastfmStatus, LibraryCreated, LibraryStatus, Notification,
            Receipt, ReceiptStatus, Request, Response, RpcError, ScanStarted, ServerInfo,
            ServerRole, ServerStatus, ServersEvent, Source, SourceAdded, SourceKind, Versions,
            ZonePeriod, ZonePeriods, ZonesSet,
        };
        use serde_json::{Value, json};
        use tokio::sync::{mpsc, watch};

        use super::*;
        use crate::link::State;
        use crate::net::Net;

        /// A single-use gate that holds one RPC until a test releases it.
        #[derive(Clone)]
        struct Gate {
            tx: Arc<watch::Sender<bool>>,
            arrived: Arc<AtomicUsize>,
        }

        impl Gate {
            fn hold() -> Self {
                let (tx, _) = watch::channel(false);
                Self {
                    tx: Arc::new(tx),
                    arrived: Arc::new(AtomicUsize::new(0)),
                }
            }

            fn release(&self) {
                let _ = self.tx.send(true);
            }

            fn reached(&self) -> usize {
                self.arrived.load(Ordering::SeqCst)
            }

            async fn wait(&self) {
                let mut rx = self.tx.subscribe();
                self.arrived.fetch_add(1, Ordering::SeqCst);
                let _ = rx.wait_for(|open| *open).await;
            }
        }

        #[derive(Default)]
        struct ServerState {
            has_library: bool,
            zones_error: Option<String>,
            zones: Vec<ZonePeriod>,
            sources: Vec<Source>,
            calls: HashMap<String, usize>,
            sent_zones: Vec<Vec<ZonePeriod>>,
            hold_zones: Option<Gate>,
            hold_set_zones: Option<Gate>,
        }

        #[derive(Clone)]
        struct FakeServer {
            state: Arc<Mutex<ServerState>>,
            unanswered: Arc<AtomicUsize>,
            hold_scan: Option<Gate>,
            hold_add: Option<Gate>,
            emit_servers_on_create: bool,
        }

        impl FakeServer {
            fn new(has_library: bool) -> Self {
                Self {
                    state: Arc::new(Mutex::new(ServerState {
                        has_library,
                        ..ServerState::default()
                    })),
                    unanswered: Arc::new(AtomicUsize::new(0)),
                    hold_scan: None,
                    hold_add: None,
                    emit_servers_on_create: false,
                }
            }

            fn lock(&self) -> std::sync::MutexGuard<'_, ServerState> {
                self.state.lock().unwrap_or_else(PoisonError::into_inner)
            }

            fn asked(&self, method: &str) -> usize {
                self.lock().calls.get(method).copied().unwrap_or(0)
            }

            fn status(has_library: bool) -> ServerStatus {
                let server_id = Id::from_bytes([0x10; 16]);
                let library_id = Id::from_bytes([0x20; 16]);
                ServerStatus {
                    transcode: None,
                    server_id,
                    name: "Test Server".to_owned(),
                    role: if has_library {
                        ServerRole::Hub
                    } else {
                        ServerRole::Unassigned
                    },
                    versions: Versions {
                        server: "0.1.0".to_owned(),
                        contract: CONTRACT_VERSION,
                        sqlite: "3.0".to_owned(),
                        server_schema: 1,
                        library_schema: has_library.then_some(1),
                    },
                    library: has_library.then_some(LibraryStatus {
                        library_id,
                        revision: 1,
                    }),
                    free_disk: FreeDisk { data_bytes: None },
                    log_path: Some("/tmp".to_owned()),
                    watch: None,
                    backup: None,
                    record: None,
                    hub: None,
                }
            }

            fn connect(&self, role: ServerRole) -> (Arc<Net>, mpsc::UnboundedReceiver<Event>) {
                let (send, mut calls) = mpsc::channel::<String>(64);
                let (answers, receive) = mpsc::channel::<String>(64);
                let _runtime = crate::net::rt().enter();
                let mut client = ritornel_client::Client::over_memory(send, receive);
                let events = client.take_events();
                let client = Arc::new(client);
                let fake = self.clone();
                crate::net::rt().spawn(async move {
                    while let Some(text) = calls.recv().await {
                        let Ok(Frame::Request(request)) = Frame::decode(&text) else {
                            continue;
                        };
                        fake.unanswered.fetch_add(1, Ordering::SeqCst);
                        let fake = fake.clone();
                        let answers = answers.clone();
                        // Spawn each request independently so holding one RPC
                        // (e.g. history.zones or scan.start) never blocks others
                        // (e.g. server.status during Link::restate).
                        tokio::spawn(async move {
                            let (outcome, extra_event) = fake.handle(&request).await;
                            if let Some(event) = extra_event {
                                let frame = Frame::Notification(Notification {
                                    method: EVENT_METHOD.to_owned(),
                                    params: serde_json::to_value(event).ok(),
                                });
                                let _ = answers.send(frame.encode()).await;
                            }
                            let frame = Frame::Response(Response {
                                id: Some(request.id),
                                outcome,
                            });
                            let _ = answers.send(frame.encode()).await;
                            fake.unanswered.fetch_sub(1, Ordering::SeqCst);
                        });
                    }
                });
                (Arc::new(Net::test_session(client, role)), events)
            }

            async fn handle(&self, request: &Request) -> (Result<Value, RpcError>, Option<Event>) {
                let (zones_gate, set_zones_gate) = {
                    let mut state = self.lock();
                    *state.calls.entry(request.method.clone()).or_default() += 1;
                    (state.hold_zones.clone(), state.hold_set_zones.clone())
                };
                if request.method == "history.zones"
                    && let Some(gate) = zones_gate
                {
                    gate.wait().await;
                }
                if request.method == "history.setZones"
                    && let Some(gate) = set_zones_gate
                {
                    gate.wait().await;
                }
                if request.method == "scan.start"
                    && let Some(gate) = &self.hold_scan
                {
                    gate.wait().await;
                }
                if request.method == "sources.add"
                    && let Some(gate) = &self.hold_add
                {
                    gate.wait().await;
                }
                let mut state = self.lock();
                let mut event = None;
                let no_lib = || {
                    RpcError::new(ErrorCode::Unavailable, "no library")
                        .with_detail("reason", "no_library")
                };
                let ok = |value: Result<Value, serde_json::Error>| {
                    value.map_err(|error| RpcError::new(ErrorCode::Internal, error.to_string()))
                };
                let outcome = match request.method.as_str() {
                    "server.status" => ok(serde_json::to_value(Self::status(state.has_library))),
                    "sources.list" => {
                        if !state.has_library {
                            Err(no_lib())
                        } else {
                            ok(serde_json::to_value(json!({ "sources": state.sources })))
                        }
                    }
                    "jobs.list" => {
                        if !state.has_library {
                            Err(no_lib())
                        } else {
                            Ok(json!({ "jobs": [] }))
                        }
                    }
                    "artists.pictureFolders" => {
                        if !state.has_library {
                            Err(no_lib())
                        } else {
                            Ok(json!({ "folders": [] }))
                        }
                    }
                    "lastfm.status" => ok(serde_json::to_value(LastfmStatus {
                        available: true,
                        configured: true,
                        account: Some("pat".to_owned()),
                        sign_in_again: false,
                        scrobbling_since: None,
                        waiting_for_import_since: None,
                        zones_set: !state.zones.is_empty(),
                        imported: 0,
                        import: ImportSchedule::default(),
                        deliveries: Vec::new(),
                    })),
                    "history.zones" => {
                        if !state.has_library {
                            Err(no_lib())
                        } else if let Some(message) = &state.zones_error {
                            Err(RpcError::new(ErrorCode::Unavailable, message.clone()))
                        } else {
                            ok(serde_json::to_value(ZonePeriods {
                                periods: state.zones.clone(),
                            }))
                        }
                    }
                    "history.setZones" => {
                        if !state.has_library {
                            Err(no_lib())
                        } else {
                            match request
                                .params
                                .clone()
                                .ok_or_else(|| RpcError::new(ErrorCode::Invalid, "missing params"))
                                .and_then(|raw| {
                                    serde_json::from_value::<SetZonesParams>(raw).map_err(|err| {
                                        RpcError::new(ErrorCode::Invalid, err.to_string())
                                    })
                                }) {
                                Ok(params) => {
                                    state.sent_zones.push(params.periods.clone());
                                    state.zones = params.periods;
                                    ok(serde_json::to_value(ZonesSet {
                                        receipt: Receipt {
                                            command_id: params.idempotency_key,
                                            status: ReceiptStatus::Done,
                                            revision: Some(2),
                                        },
                                        changed: 0,
                                    }))
                                }
                                Err(err) => Err(err),
                            }
                        }
                    }
                    "library.create" => {
                        let key = request
                            .params
                            .as_ref()
                            .and_then(|params| params.get("idempotencyKey"))
                            .and_then(|value| serde_json::from_value::<Id>(value.clone()).ok())
                            .unwrap_or_else(|| Id::from_bytes([0x30; 16]));
                        state.has_library = true;
                        if self.emit_servers_on_create {
                            event = Some(Event::Servers(ServersEvent {
                                server: ServerInfo {
                                    server_id: Id::from_bytes([0x10; 16]),
                                    name: "Test Server".to_owned(),
                                    role: ServerRole::Hub,
                                    is_this_server: true,
                                    reachable: true,
                                    revision: 2,
                                },
                            }));
                        }
                        ok(serde_json::to_value(LibraryCreated {
                            receipt: Receipt {
                                command_id: key,
                                status: ReceiptStatus::Done,
                                revision: Some(1),
                            },
                            library_id: Id::from_bytes([0x20; 16]),
                        }))
                    }
                    "sources.add" => {
                        if !state.has_library {
                            Err(no_lib())
                        } else {
                            match request
                                .params
                                .clone()
                                .ok_or_else(|| RpcError::new(ErrorCode::Invalid, "missing params"))
                                .and_then(|raw| {
                                    serde_json::from_value::<SourcesAddParams>(raw).map_err(|err| {
                                        RpcError::new(ErrorCode::Invalid, err.to_string())
                                    })
                                }) {
                                Ok(params) => {
                                    let source = Source {
                                        source_id: Id::from_bytes([0x40; 16]),
                                        name: params.name,
                                        kind: SourceKind::Folder,
                                        path: Some(params.path),
                                    };
                                    state.sources.push(source.clone());
                                    ok(serde_json::to_value(SourceAdded {
                                        receipt: Receipt {
                                            command_id: params.idempotency_key,
                                            status: ReceiptStatus::Done,
                                            revision: Some(2),
                                        },
                                        source,
                                    }))
                                }
                                Err(err) => Err(err),
                            }
                        }
                    }
                    "scan.start" => {
                        let key = request
                            .params
                            .as_ref()
                            .and_then(|params| params.get("idempotencyKey"))
                            .and_then(|value| serde_json::from_value::<Id>(value.clone()).ok())
                            .unwrap_or_else(|| Id::from_bytes([0x50; 16]));
                        let source_id = request
                            .params
                            .as_ref()
                            .and_then(|params| params.get("sourceId"))
                            .and_then(|value| serde_json::from_value::<Id>(value.clone()).ok())
                            .unwrap_or_else(|| Id::from_bytes([0x40; 16]));
                        let job = Job {
                            job_id: Id::from_bytes([0x60; 16]),
                            kind: "scan".to_owned(),
                            target: Some(JobTarget::Source { id: source_id }),
                            state: JobState::Running,
                            done: 0,
                            total: None,
                            bytes: 0,
                            failed: 0,
                            skipped: 0,
                            label: None,
                            error: None,
                            created_at: 1,
                            started_at: Some(1),
                            finished_at: None,
                            scan: None,
                            sync: None,
                        };
                        ok(serde_json::to_value(ScanStarted {
                            receipt: Receipt {
                                command_id: key,
                                status: ReceiptStatus::Accepted,
                                revision: None,
                            },
                            job,
                        }))
                    }
                    _ => Err(RpcError::new(ErrorCode::Unsupported, "not in this test")),
                };
                (outcome, event)
            }
        }

        fn wait_until(
            cx: &mut gpui::TestAppContext,
            what: &str,
            mut done: impl FnMut(&mut gpui::TestAppContext) -> bool,
        ) {
            let deadline = Instant::now() + Duration::from_secs(60);
            loop {
                cx.run_until_parked();
                if done(cx) {
                    return;
                }
                assert!(Instant::now() < deadline, "never {what}");
                std::thread::yield_now();
            }
        }

        fn test_clock() -> words::Clock {
            words::Clock {
                now: 1_763_391_600_000,
                zone: jiff::tz::TimeZone::UTC,
                style: words::Style::BRITISH,
            }
        }

        fn period(zone: &str, from: Option<&str>) -> ZonePeriod {
            ZonePeriod {
                zone: zone.to_owned(),
                from: from.map(str::to_owned),
                until: None,
            }
        }

        #[gpui::test]
        fn saving_before_zones_load_never_replaces_them(cx: &mut gpui::TestAppContext) {
            cx.executor().allow_parking();
            let gate = Gate::hold();
            let fake = FakeServer::new(true);
            fake.lock().hold_zones = Some(gate.clone());
            fake.lock().zones = vec![period("Europe/London", None)];

            let (net, events) = fake.connect(ServerRole::Hub);
            let link = cx.update(crate::link::unlinked);
            link.update(cx, |link, cx| link.connect(net, events, cx));
            let model = cx.update(|cx| super::super::new(link.clone(), true, cx));

            // Wait until history.zones is in flight and other initial reads finished.
            wait_until(cx, "started reading zones", |cx| {
                gate.reached() == 1 && model.read_with(cx, |m, _| m.lastfm.is_some())
            });
            assert_eq!(model.read_with(cx, |m, _| m.zones.clone()), Zones::Loading);

            // Editing is refused while loading: start_adding stays closed,
            // and even a forced add/remove sends no replacement.
            let clock = test_clock();
            model.update(cx, |model, cx| {
                model.start_adding(&clock, cx);
                assert!(model.adding.is_none());
                model.adding = Some(Adding {
                    zone: "Asia/Tokyo".to_owned(),
                    from: "2026-11-17".to_owned(),
                });
                model.add(cx);
                model.chosen = Some(0);
                model.remove(cx);
            });
            cx.run_until_parked();
            assert_eq!(fake.asked("history.setZones"), 0);

            // Once the initial read finishes, adding preserves the server's periods.
            gate.release();
            wait_until(cx, "loaded zones", |cx| {
                model.read_with(cx, |m, _| {
                    m.zones == Zones::Loaded(vec![period("Europe/London", None)])
                })
            });
            model.update(cx, |model, cx| {
                model.adding = Some(Adding {
                    zone: "Asia/Tokyo".to_owned(),
                    from: "2026-11-17".to_owned(),
                });
                model.add(cx);
            });
            wait_until(cx, "saved added zone", |cx| {
                fake.asked("history.setZones") == 1
                    && model.read_with(cx, |m, _| !m.is_busy(Busy::Zones) && m.adding.is_none())
            });
            assert_eq!(
                fake.lock().sent_zones,
                vec![vec![
                    period("Europe/London", None),
                    period("Asia/Tokyo", Some("2026-11-17")),
                ]]
            );
        }

        #[gpui::test]
        fn a_failed_zones_read_blocks_edits_until_retry_succeeds(cx: &mut gpui::TestAppContext) {
            cx.executor().allow_parking();
            let fake = FakeServer::new(true);
            fake.lock().zones = vec![period("America/New_York", None)];
            fake.lock().zones_error = Some("Studio Mac does not answer".to_owned());

            let (net, events) = fake.connect(ServerRole::Hub);
            let link = cx.update(crate::link::unlinked);
            link.update(cx, |link, cx| link.connect(net, events, cx));
            let model = cx.update(|cx| super::super::new(link.clone(), true, cx));

            wait_until(cx, "recorded zones failure", |cx| {
                model.read_with(cx, |m, _| {
                    m.zones == Zones::Failed("Studio Mac does not answer".to_owned())
                })
            });
            assert!(model.read_with(cx, |m, _| !m.problems.contains_key(&Group::Zones)));

            // No edit can send a replacement after a failed read.
            let clock = test_clock();
            model.update(cx, |model, cx| {
                model.start_adding(&clock, cx);
                assert!(model.adding.is_none());
                model.adding = Some(Adding {
                    zone: "Europe/Berlin".to_owned(),
                    from: String::new(),
                });
                model.add(cx);
                model.chosen = Some(0);
                model.remove(cx);
            });
            cx.run_until_parked();
            assert_eq!(fake.asked("history.setZones"), 0);

            // Retrying transitions through Loading and enables editing once loaded.
            fake.lock().zones_error = None;
            model.update(cx, |model, cx| {
                model.read_zones(cx);
                assert_eq!(model.zones, Zones::Loading);
            });
            wait_until(cx, "reloaded zones after retry", |cx| {
                model.read_with(cx, |m, _| {
                    m.zones == Zones::Loaded(vec![period("America/New_York", None)])
                })
            });
            model.update(cx, |model, cx| {
                model.start_adding(&clock, cx);
                assert!(model.adding.is_some());
                model.adding = Some(Adding {
                    zone: "Europe/Berlin".to_owned(),
                    from: "2026-11-17".to_owned(),
                });
                model.add(cx);
            });
            wait_until(cx, "saved zone after retry", |cx| {
                fake.asked("history.setZones") == 1
                    && model.read_with(cx, |m, _| !m.is_busy(Busy::Zones) && m.adding.is_none())
            });
            assert_eq!(
                fake.lock().sent_zones,
                vec![vec![
                    period("America/New_York", None),
                    period("Europe/Berlin", Some("2026-11-17")),
                ]]
            );
        }

        #[gpui::test]
        fn first_folder_clears_busy_when_status_restates_before_scan_finishes(
            cx: &mut gpui::TestAppContext,
        ) {
            cx.executor().allow_parking();
            let scan_gate = Gate::hold();
            let mut fake = FakeServer::new(false);
            fake.hold_scan = Some(scan_gate.clone());
            fake.emit_servers_on_create = true;

            let (initial_net, events) = fake.connect(ServerRole::Unassigned);
            let link = cx.update(crate::link::unlinked);
            link.update(cx, |link, cx| {
                link.connect(Arc::clone(&initial_net), events, cx);
            });
            let model = cx.update(|cx| super::super::new(link.clone(), true, cx));

            wait_until(cx, "initial empty server reads settled", |cx| {
                fake.unanswered.load(Ordering::SeqCst) == 0
                    && model.read_with(cx, |m, _| {
                        m.sources == Some(Vec::new()) && m.zones == Zones::NoLibrary
                    })
            });

            let add_task = model.update(cx, |model, cx| {
                model.add_folder("/Volumes/Records/Music".to_owned(), cx)
            });
            assert!(model.read_with(cx, |m, _| m.is_busy(Busy::Adding)));

            // Wait until library.create triggered Link::restate (new Arc<Net>, same session)
            // while scan.start is still in flight.
            wait_until(cx, "restated status while scan.start is held", |cx| {
                scan_gate.reached() == 1
                    && link.read_with(cx, |link, _| {
                        link.net().is_some_and(|current| {
                            current.role == ServerRole::Hub
                                && current.holds
                                && !Arc::ptr_eq(&initial_net, &current)
                                && initial_net.same_session(&current)
                        })
                    })
            });

            scan_gate.release();
            let done = Arc::new(AtomicBool::new(false));
            let succeeded = Arc::new(AtomicBool::new(false));
            {
                let done = Arc::clone(&done);
                let succeeded = Arc::clone(&succeeded);
                cx.spawn(async move |_| {
                    let ok = add_task.await;
                    succeeded.store(ok, Ordering::SeqCst);
                    done.store(true, Ordering::SeqCst);
                })
                .detach();
            }
            wait_until(cx, "add_folder completed", |cx| {
                done.load(Ordering::SeqCst)
                    && fake.unanswered.load(Ordering::SeqCst) == 0
                    && model.read_with(cx, |m, _| !m.is_busy(Busy::Adding))
            });

            assert!(succeeded.load(Ordering::SeqCst));
            assert!(model.read_with(cx, |m, _| {
                !m.is_busy(Busy::Adding)
                    && m.sources.as_ref().is_some_and(|sources| {
                        sources
                            .iter()
                            .any(|source| source.path.as_deref() == Some("/Volumes/Records/Music"))
                    })
                    && !m.scans.is_empty()
            }));
        }

        #[gpui::test]
        fn stale_operation_clears_only_its_own_busy_slot(cx: &mut gpui::TestAppContext) {
            cx.executor().allow_parking();
            let first_gate = Gate::hold();
            let mut first = FakeServer::new(true);
            first.hold_add = Some(first_gate.clone());
            let (net1, events1) = first.connect(ServerRole::Hub);

            let second_gate = Gate::hold();
            let mut second = FakeServer::new(true);
            second.hold_add = Some(second_gate.clone());
            let (net2, events2) = second.connect(ServerRole::Hub);

            let link = cx.update(crate::link::unlinked);
            link.update(cx, |link, cx| link.connect(net1, events1, cx));
            let model = cx.update(|cx| super::super::new(link.clone(), true, cx));

            wait_until(cx, "first session settled", |cx| {
                first.unanswered.load(Ordering::SeqCst) == 0
                    && model.read_with(cx, |m, _| m.sources.is_some())
            });

            let op1 = model.update(cx, |model, cx| {
                model.add_folder("/Volumes/One/Music".to_owned(), cx)
            });
            wait_until(cx, "first add_folder reached server", |_| {
                first_gate.reached() == 1
            });
            assert!(model.read_with(cx, |m, _| m.is_busy(Busy::Adding)));

            // Reconnect to a new session and start a new Add Folder while op1 is still in flight.
            link.update(cx, |link, cx| link.connect(net2, events2, cx));
            wait_until(cx, "second session connected", |cx| {
                model.read_with(cx, |m, _| !m.is_busy(Busy::Adding))
            });
            let op2 = model.update(cx, |model, cx| {
                model.add_folder("/Volumes/Two/Music".to_owned(), cx)
            });
            wait_until(cx, "second add_folder reached server", |_| {
                second_gate.reached() == 1
            });
            assert!(model.read_with(cx, |m, _| m.is_busy(Busy::Adding)));

            // Finishing op1 must NOT clear op2's busy slot.
            first_gate.release();
            let op1_done = Arc::new(AtomicBool::new(false));
            {
                let op1_done = Arc::clone(&op1_done);
                cx.spawn(async move |_| {
                    assert!(!op1.await);
                    op1_done.store(true, Ordering::SeqCst);
                })
                .detach();
            }
            wait_until(cx, "op1 finished", |_| op1_done.load(Ordering::SeqCst));
            assert!(model.read_with(cx, |m, _| m.is_busy(Busy::Adding)));

            // Going Away and finishing op2 clears its matching busy slot.
            link.update(cx, |link, cx| {
                link.state = State::Away {
                    why: crate::link::Why::Lost,
                    message: None,
                    data_dir: None,
                };
                cx.emit(LinkEvent::Away);
            });
            second_gate.release();
            let op2_done = Arc::new(AtomicBool::new(false));
            {
                let op2_done = Arc::clone(&op2_done);
                cx.spawn(async move |_| {
                    assert!(!op2.await);
                    op2_done.store(true, Ordering::SeqCst);
                })
                .detach();
            }
            wait_until(cx, "op2 cleared busy on away", |cx| {
                op2_done.load(Ordering::SeqCst)
                    && model.read_with(cx, |m, _| !m.is_busy(Busy::Adding))
            });
        }

        #[gpui::test]
        fn refreshing_zones_disables_edits_and_clears_selection_until_loaded(
            cx: &mut gpui::TestAppContext,
        ) {
            cx.executor().allow_parking();
            let fake = FakeServer::new(true);
            fake.lock().zones = vec![
                period("Europe/London", None),
                period("America/New_York", Some("2021-06-01")),
            ];

            let (net, events) = fake.connect(ServerRole::Hub);
            let link = cx.update(crate::link::unlinked);
            link.update(cx, |link, cx| link.connect(net, events, cx));
            let model = cx.update(|cx| super::super::new(link.clone(), true, cx));

            wait_until(cx, "initial zones loaded", |cx| {
                model.read_with(cx, |m, _| m.zones.is_loaded())
            });

            // Select the second zone, then start a refresh on the same session
            // while another client changed the server's list.
            model.update(cx, |model, _| model.chosen = Some(1));
            let refresh_gate = Gate::hold();
            {
                let mut state = fake.lock();
                state.zones = vec![
                    period("Asia/Tokyo", None),
                    period("Europe/London", Some("2020-01-01")),
                ];
                state.hold_zones = Some(refresh_gate.clone());
            }
            model.update(cx, |model, cx| model.read_zones(cx));

            wait_until(cx, "refresh reached server", |_| {
                refresh_gate.reached() == 1
            });
            // Selection is cleared and writes are blocked while refresh is in flight.
            assert_eq!(model.read_with(cx, |m, _| m.zones.clone()), Zones::Loading);
            assert_eq!(model.read_with(cx, |m, _| m.chosen), None);
            model.update(cx, |model, cx| {
                model.chosen = Some(1);
                model.remove(cx);
                model.adding = Some(Adding {
                    zone: "Europe/Berlin".to_owned(),
                    from: String::new(),
                });
                model.add(cx);
            });
            cx.run_until_parked();
            assert_eq!(fake.asked("history.setZones"), 0);

            // Releasing the refresh loads the server's new list with no stale selection.
            refresh_gate.release();
            wait_until(cx, "refresh completed", |cx| {
                model.read_with(cx, |m, _| {
                    m.zones
                        == Zones::Loaded(vec![
                            period("Asia/Tokyo", None),
                            period("Europe/London", Some("2020-01-01")),
                        ])
                })
            });
            assert_eq!(model.read_with(cx, |m, _| m.chosen), None);
        }

        #[gpui::test]
        fn a_save_does_not_overwrite_a_newer_zones_read(cx: &mut gpui::TestAppContext) {
            cx.executor().allow_parking();
            let fake = FakeServer::new(true);
            fake.lock().zones = vec![period("Europe/London", None)];

            let (net, events) = fake.connect(ServerRole::Hub);
            let link = cx.update(crate::link::unlinked);
            link.update(cx, |link, cx| link.connect(net, events, cx));
            let model = cx.update(|cx| super::super::new(link.clone(), true, cx));

            wait_until(cx, "initial zones loaded", |cx| {
                model.read_with(cx, |m, _| m.zones.is_loaded())
            });

            // Hold history.setZones while a newer read_zones starts and fails.
            let save_gate = Gate::hold();
            fake.lock().hold_set_zones = Some(save_gate.clone());
            model.update(cx, |model, cx| {
                model.adding = Some(Adding {
                    zone: "Asia/Tokyo".to_owned(),
                    from: "2026-11-17".to_owned(),
                });
                model.add(cx);
            });
            wait_until(cx, "save reached server", |_| save_gate.reached() == 1);

            // A newer refresh starts while the save is still in flight and fails.
            fake.lock().zones_error = Some("Studio Mac does not answer".to_owned());
            model.update(cx, |model, cx| model.read_zones(cx));
            wait_until(cx, "newer refresh failed", |cx| {
                model.read_with(cx, |m, _| {
                    m.zones == Zones::Failed("Studio Mac does not answer".to_owned())
                })
            });

            // Even if the older save's read-back succeeds, it must not overwrite
            // the newer refresh's state.
            fake.lock().zones_error = None;
            save_gate.release();
            wait_until(cx, "save finished", |cx| {
                model.read_with(cx, |m, _| !m.is_busy(Busy::Zones))
            });
            assert_eq!(
                model.read_with(cx, |m, _| m.zones.clone()),
                Zones::Failed("Studio Mac does not answer".to_owned())
            );
        }
    }
}
