//! What the Settings window shows, read from the server, and what it asks
//! the server to do (`settings/model.svelte.ts`). Every change is a command
//! of the contract, with a key of its own; nothing is kept here that the
//! server does not hold.
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
    LastfmStatusCall, LastfmSync, LibrarySetWatching, RecordDismiss, ScanStart,
    ServerStatus as StatusCall, SourcesAdd, SourcesList,
};
use ritornel_contract::{
    DismissParams, ErrorCode, Event, Id, ImportPicturesParams, Job, JobCancelParams, JobState,
    JobTarget, KeyParams, LastfmStatus, LastfmSyncParams, Method, NoParams, PictureFolders,
    ReceiptStatus, ScanStartParams, ScrobblingParams, ServerStatus, SetZonesParams,
    SignInFinishParams, Source, SourcesAddParams, WatchingParams, ZonePeriod,
};

use crate::link::{Link, LinkEvent};
use crate::net::Net;
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

pub(crate) struct Model {
    link: Entity<Link>,
    /// The music folders, or `None` until read.
    pub sources: Option<Vec<Source>>,
    /// The newest scan of each source, by its ID.
    pub scans: HashMap<Id, Job>,
    sources_read: u64,
    pub pictures: Option<PictureFolders>,
    pub lastfm: Option<LastfmStatus>,
    pub zones: Option<Vec<ZonePeriod>>,
    pub status: Option<ServerStatus>,
    /// The data directory, as this app looks in it.
    pub data_dir: Option<PathBuf>,
    pub signing_in: SigningIn,
    /// What is under way.
    pub busy: HashSet<Busy>,
    /// What went wrong last, by the group it went wrong in.
    pub problems: HashMap<Group, String>,
    /// The zone chosen in the list, for the − button.
    pub chosen: Option<usize>,
    /// The zone being added, while its form is open.
    pub adding: Option<Adding>,
    /// The sign-in being waited for: dropping it stops the waiting.
    sign_in: Option<Task<()>>,
    /// Whether nothing is opened outside the app: while a script poses the
    /// window, as the real app opens no browser while it is measured.
    pub quiet: bool,
    _link: Subscription,
}

/// The words of what went wrong, for a line under the group it went wrong in.
fn trouble(error: &Error) -> String {
    match error {
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
                model.read_all(cx);
                cx.notify();
            }
            LinkEvent::Server(Event::Jobs(event)) => model.heard_job(event.job.clone(), cx),
            LinkEvent::Away | LinkEvent::Server(_) => cx.notify(),
        });
        let data_dir = link.read(cx).data_dir();
        let mut model = Self {
            link,
            sources: None,
            scans: HashMap::new(),
            sources_read: 0,
            pictures: None,
            lastfm: None,
            zones: None,
            status: None,
            data_dir,
            signing_in: SigningIn::Idle,
            busy: HashSet::new(),
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
                    .is_some_and(|current| Arc::ptr_eq(&net, &current))
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
                Err(error) => {
                    model.problems.insert(Group::Pictures, trouble(&error));
                }
            },
        );
    }

    pub(crate) fn read_lastfm(&mut self, cx: &mut Context<Self>) {
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
        self.reading(
            cx,
            |net| ask::<HistoryZones>(&net, NoParams {}),
            |model, found, _| match found {
                Ok(zones) => model.zones = Some(zones.periods),
                Err(error) => {
                    model.problems.insert(Group::Zones, trouble(&error));
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
        self.busy.insert(busy);
        self.problems.remove(&group);
        cx.notify();
        let work = work(Arc::clone(&net));
        cx.spawn(async move |model, cx| {
            let found = work.await;
            model
                .update(cx, |model, cx| {
                    if model
                        .net(cx)
                        .is_none_or(|current| !Arc::ptr_eq(&net, &current))
                    {
                        return false;
                    }
                    model.busy.remove(&busy);
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

    /// Adds a folder as a source and scans it. A name the library has is given a number.
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
                    match ask::<SourcesAdd>(&net, params).await {
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
    fn set_zones(&mut self, periods: Vec<ZonePeriod>, cx: &mut Context<Self>) -> Task<bool> {
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
                let zones = ask::<HistoryZones>(&net, NoParams {});
                let lastfm = ask::<LastfmStatusCall>(&net, NoParams {});
                Ok((zones.await?, lastfm.await?))
            },
            |model, (zones, lastfm), _| {
                model.zones = Some(zones.periods);
                model.lastfm = Some(lastfm);
            },
        )
    }

    /// Opens the form for a zone: the system's own, from today, or from
    /// always when it is the first.
    pub(crate) fn start_adding(&mut self, clock: &words::Clock, cx: &mut Context<Self>) {
        let first = self.zones.as_ref().is_none_or(Vec::is_empty);
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
        let Some(adding) = self.adding.clone() else {
            return;
        };
        let period = ZonePeriod {
            zone: adding.zone,
            from: (!adding.from.is_empty()).then_some(adding.from),
            until: None,
        };
        let mut periods = self.zones.clone().unwrap_or_default();
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
        let Some(chosen) = self.chosen else { return };
        let periods: Vec<ZonePeriod> = self
            .zones
            .iter()
            .flatten()
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
    use super::*;

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
}
