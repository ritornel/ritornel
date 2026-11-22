//! Last.fm in the server: the client and its API key and secret, the
//! sign-ins in progress, the job that syncs an account's history, when the
//! hub starts that job by itself, and the worker that scrobbles and tells
//! Last.fm what is playing. See "Last.fm" in docs/design/history.md.
//!
//! All of it is built with the `lastfm` feature. Without it a server has
//! the same calls, which say Last.fm is not built in, and links nothing of
//! `ritornel-lastfm` or `scrobl`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::dispatch::Core;

/// The service's name, in `server.db` and the library.
#[cfg(feature = "lastfm")]
pub(crate) const SERVICE: &str = "lastfm";

/// The kind of the job that syncs an account's history.
pub(crate) const SYNC_JOB: &str = "lastfm.sync";

/// The longest the scrobbler sleeps with nothing to do. It is also how
/// late the six-hourly import may start.
const IDLE: Duration = Duration::from_secs(60);

/// How long after an import ends a client opening starts another.
#[cfg(feature = "lastfm")]
pub(crate) const IMPORT_ON_OPEN_AFTER: Duration = Duration::from_secs(60 * 60);

/// How long after an import ends the hub starts another by itself.
#[cfg(feature = "lastfm")]
pub(crate) const IMPORT_EVERY: Duration = Duration::from_secs(6 * 60 * 60);

/// How this run reaches Last.fm. Everything but the credentials is for
/// tests alone.
#[derive(Debug, Clone, Default)]
pub struct LastfmRun {
    /// The API key and secret from the environment. With none, the server
    /// looks in its data directory.
    #[cfg(feature = "lastfm")]
    pub credentials: Option<ritornel_lastfm::ApiCredentials>,
    /// For tests: a loopback address to send everything to, with no pause
    /// between requests.
    pub base_url: Option<String>,
    /// For tests: a span holds at most this many scrobbles.
    pub chunk: Option<u64>,
    /// For tests: rows to a page.
    pub page_size: Option<u32>,
    /// For the crash test: the server aborts straight after the library
    /// commits this many batches of a sync.
    pub fail_after_batch: Option<u64>,
}

/// What every session shares of Last.fm.
#[derive(Debug)]
pub(crate) struct Lastfm {
    #[cfg(feature = "lastfm")]
    run: LastfmRun,
    /// What wakes the scrobbler.
    wake: crate::playback::Wake,
    /// Set when a client opens, until the scrobbler's thread has looked at
    /// whether that starts an import.
    opened: AtomicBool,
    /// The client, when the server has an API key and secret.
    #[cfg(feature = "lastfm")]
    client: Option<ritornel_lastfm::Lastfm>,
    /// Why there is no client, when the credentials could not be read.
    #[cfg(feature = "lastfm")]
    problem: Option<String>,
    /// Sign-ins in progress, by the ID handed out for each.
    #[cfg(feature = "lastfm")]
    sign_ins: std::sync::Mutex<std::collections::HashMap<ritornel_contract::Id, SignIn>>,
    /// The track to show as playing now, until the scrobbler takes it.
    #[cfg(feature = "lastfm")]
    playing: Slot,
    /// The account nothing is scrobbled for until then, on the monotonic
    /// clock, because Last.fm said its daily limit was reached, refused a
    /// request or left a batch unanswered: the plays held back go first
    /// when it lifts.
    #[cfg(feature = "lastfm")]
    paused: std::sync::Mutex<Option<(ritornel_contract::Id, std::time::Instant)>>,
    /// Taken by the scrobbler for each batch and by every change to the
    /// session.
    #[cfg(feature = "lastfm")]
    turn: Turn,
}

/// Where a sign-in has got to.
#[cfg(feature = "lastfm")]
#[derive(Debug, Clone)]
enum SignIn {
    /// Waiting for the listener to approve the token.
    Waiting(ritornel_lastfm::AuthToken),
    /// Approved, and the session kept, for this account.
    Done(String),
}

impl Lastfm {
    /// Builds what this run needs. A server with no API key and secret has
    /// no client, and says so when asked; a credentials file others may
    /// read is refused, and said so in the log.
    #[cfg_attr(not(feature = "lastfm"), allow(unused_variables))]
    pub(crate) fn new(run: LastfmRun, data_dir: &std::path::Path) -> Self {
        #[cfg(feature = "lastfm")]
        {
            use tracing::{debug, info, warn};
            let found = match run.credentials.clone() {
                Some(credentials) => Ok(Some(credentials)),
                None => ritornel_lastfm::ApiCredentials::find(|_| None, data_dir),
            };
            let settings = ritornel_lastfm::Settings {
                base_url: run.base_url.clone(),
                min_interval: if run.base_url.is_some() {
                    Duration::ZERO
                } else {
                    ritornel_lastfm::Settings::default().min_interval
                },
                chunk: run
                    .chunk
                    .unwrap_or(ritornel_lastfm::Settings::default().chunk),
                page_size: run
                    .page_size
                    .unwrap_or(ritornel_lastfm::Settings::default().page_size),
                ..ritornel_lastfm::Settings::default()
            };
            let (client, problem) = match found {
                Ok(Some(credentials)) => {
                    match ritornel_lastfm::Lastfm::new(credentials, settings) {
                        Ok(client) => {
                            info!("Last.fm: an API key and secret are set");
                            (Some(client), None)
                        }
                        Err(error) => {
                            warn!(%error, "Last.fm's client could not be built");
                            (None, Some(error.to_string()))
                        }
                    }
                }
                Ok(None) => {
                    debug!(
                        "Last.fm: no API key and secret; set {} and {}, or put them in {}",
                        ritornel_lastfm::KEY_VARIABLE,
                        ritornel_lastfm::SECRET_VARIABLE,
                        ritornel_lastfm::CREDENTIALS_FILE
                    );
                    (None, None)
                }
                Err(problem) => {
                    warn!(%problem, "Last.fm's API key and secret could not be read");
                    (None, Some(problem))
                }
            };
            Self {
                run,
                wake: crate::playback::Wake::default(),
                opened: AtomicBool::new(false),
                client,
                problem,
                sign_ins: std::sync::Mutex::default(),
                playing: Slot::default(),
                paused: std::sync::Mutex::default(),
                turn: Turn::default(),
            }
        }
        #[cfg(not(feature = "lastfm"))]
        Self {
            wake: crate::playback::Wake::default(),
            opened: AtomicBool::new(false),
        }
    }

    /// Wakes the scrobbler.
    pub(crate) fn wake(&self) {
        self.wake.wake();
    }

    /// Says a client such as the desktop app has opened, which may start
    /// an import. The thread decides, so that a hello never waits on it.
    pub(crate) fn client_opened(&self) {
        self.opened.store(true, Ordering::SeqCst);
        self.wake.wake();
    }

    /// Says a track began to be heard, or was heard again after a pause: it
    /// replaces any not yet shown as playing, and the scrobbler is woken to
    /// show it once it has settled.
    #[cfg_attr(not(feature = "lastfm"), allow(unused_variables))]
    pub(crate) fn began(&self, appearance: ritornel_contract::Id, duration_ms: i64) {
        #[cfg(feature = "lastfm")]
        {
            self.playing.put(Began {
                appearance,
                duration_ms,
                at: std::time::Instant::now(),
            });
            self.wake.wake();
        }
    }

    /// Says the player paused or stopped: a track not yet shown as playing
    /// is let go.
    pub(crate) fn stopped(&self) {
        #[cfg(feature = "lastfm")]
        self.playing.clear();
    }

    /// For the crash test: how many batches of a sync are committed before
    /// the server aborts.
    #[cfg(feature = "lastfm")]
    pub(crate) fn fail_after_batch(&self) -> Option<u64> {
        self.run.fail_after_batch
    }
}

#[cfg(feature = "lastfm")]
pub(crate) use enabled::*;

#[cfg(feature = "lastfm")]
mod enabled {
    use std::time::Instant;

    use ritornel_contract::{ErrorCode, Id, RpcError, SyncSummary};
    use ritornel_engine::{
        Due, Job, JobState, LibraryStore, Observations, Progress, ServerStore, ServiceKey,
        ServiceSession, Settled,
    };
    use ritornel_lastfm::{
        DAILY_LIMIT, NotShown, Outcome, Playing, Sent, SessionKey, ToScrobble, Unsent,
    };
    use ritornel_source::{
        Batch, Error as SourceError, HistorySource, Love, LovesStored, Progress as Read, Sink,
        Stored,
    };
    use tracing::{debug, error, info, warn};

    use std::sync::atomic::{AtomicBool, Ordering};

    use super::{IMPORT_EVERY, IMPORT_ON_OPEN_AFTER, Lastfm, SERVICE, SignIn};
    use crate::dispatch::Core;

    /// What may start an import on its own.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum Trigger {
        /// The hub has just started.
        Start,
        /// A client such as the desktop app has opened.
        ClientOpened,
        /// The scrobbler's thread woke, as it does at least every minute.
        Timer,
    }

    impl Trigger {
        fn as_str(self) -> &'static str {
            match self {
                Self::Start => "start",
                Self::ClientOpened => "client_opened",
                Self::Timer => "timer",
            }
        }
    }

    /// What a look at whether an import is due came to.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) enum Auto {
        /// A sync was queued: this job.
        Started(Id),
        /// One is queued or running already, and that one counts.
        Running,
        /// The last one ended too recently.
        NotDue,
        /// None starts on its own, and this is why.
        Off(String),
        /// One was due and could not be queued.
        Failed(String),
    }

    pub(crate) fn ms(duration: std::time::Duration) -> i64 {
        i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
    }

    /// Whether `trigger` starts an import, given when the last one ended.
    /// A failed one counts, so a failure waits for the next trigger.
    pub(crate) fn due(trigger: Trigger, last_ended_ms: Option<i64>, now_ms: i64) -> bool {
        let after = match trigger {
            Trigger::Start => return true,
            Trigger::ClientOpened => IMPORT_ON_OPEN_AFTER,
            Trigger::Timer => IMPORT_EVERY,
        };
        last_ended_ms.is_none_or(|ended| now_ms.saturating_sub(ended) >= ms(after))
    }

    /// One look at whether an import is due, as the scrobbler's thread
    /// takes it each time it wakes. Its first look is the hub's start.
    pub(crate) fn import_tick(core: &Core, lastfm: &Lastfm, first: bool) -> Auto {
        let opened = lastfm.opened.swap(false, Ordering::SeqCst);
        let trigger = if first {
            Trigger::Start
        } else if opened {
            Trigger::ClientOpened
        } else {
            Trigger::Timer
        };
        let auto = core.import_if_due(trigger);
        let (timer, trigger) = (trigger == Trigger::Timer, trigger.as_str());
        match &auto {
            Auto::Started(job_id) => {
                info!(trigger, job_id = %job_id, "a Last.fm import starts on its own")
            }
            Auto::Running => debug!(trigger, "a Last.fm import is already queued or running"),
            // The timer would say so every minute.
            Auto::NotDue | Auto::Off(_) if timer => {}
            Auto::NotDue => debug!(trigger, "the last Last.fm import ended too recently"),
            Auto::Off(why) => debug!(trigger, why = %why, "no Last.fm import starts on its own"),
            Auto::Failed(failure) => {
                warn!(trigger, error = %failure, "a Last.fm import could not be started")
            }
        }
        auto
    }

    /// How long a play that could not be sent waits before it is sent
    /// again.
    const RETRY_MS: i64 = 5 * 60 * 1000;

    /// How long plays that met the account's daily limit wait before they
    /// are sent again. Last.fm does not say when the limit lifts.
    pub(super) const DAILY_LIMIT_RETRY_MS: i64 = 60 * 60 * 1000;

    /// How long the account sends nothing after the daily limit is met.
    pub(super) const DAILY_LIMIT_WAIT: std::time::Duration =
        std::time::Duration::from_secs(60 * 60);

    /// How long the account sends nothing after Last.fm refuses a request
    /// whole: long enough not to throw every queued play at a refusal,
    /// short enough that a request refused for its own content does not
    /// hold back the rest for long.
    pub(super) const REFUSED_WAIT: std::time::Duration = std::time::Duration::from_secs(5 * 60);

    /// How long the account sends nothing after a batch went unanswered:
    /// the plays in it are never sent again, as they may have arrived, so
    /// the rest wait rather than meet the same fate while Last.fm, or the
    /// way to it, is failing.
    pub(super) const UNANSWERED_WAIT: std::time::Duration = std::time::Duration::from_secs(5 * 60);

    /// How long a track is heard before Last.fm is told it is playing:
    /// skipping through several tells it of the one stopped at alone.
    const SETTLE: std::time::Duration = std::time::Duration::from_secs(1);

    /// The longest a track waits to be shown as playing. One older, kept
    /// back while the scrobbler was busy, is let go.
    const STALE: std::time::Duration = std::time::Duration::from_secs(10);

    /// A track that began to be heard.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) struct Began {
        pub(crate) appearance: Id,
        pub(crate) duration_ms: i64,
        pub(crate) at: Instant,
    }

    /// What there is to show as playing.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum Taken {
        /// Nothing.
        Nothing,
        /// A track that has not settled yet: look again after this long.
        Wait(std::time::Duration),
        /// This track.
        Show(Began),
    }

    /// The one track to show as playing: the latest that began wins, and
    /// it is never kept anywhere else nor sent twice.
    #[derive(Debug, Default)]
    pub(crate) struct Slot(std::sync::Mutex<Option<Began>>);

    impl Slot {
        pub(crate) fn put(&self, began: Began) {
            *lock(&self.0) = Some(began);
        }

        pub(crate) fn clear(&self) {
            *lock(&self.0) = None;
        }

        /// Takes the track waiting, once it has been heard for [`SETTLE`];
        /// one older than [`STALE`] is let go.
        pub(crate) fn take(&self, now: Instant) -> Taken {
            let mut slot = lock(&self.0);
            let Some(began) = *slot else {
                return Taken::Nothing;
            };
            let age = now.saturating_duration_since(began.at);
            if age < SETTLE {
                return Taken::Wait(SETTLE - age);
            }
            *slot = None;
            if age > STALE {
                debug!("a track that began too long ago is not shown as playing at Last.fm");
                return Taken::Nothing;
            }
            Taken::Show(began)
        }
    }

    /// Whose turn it is with the session: the scrobbler's, from its read of
    /// the session until what it sent as that session is recorded, or a
    /// change's. A change waits for a batch on its way, and the scrobbler
    /// stops once a change is waiting, so no batch leaves as a session
    /// that has changed since, and a change is never kept waiting long.
    ///
    /// A change takes its turn inside a call, which holds the stores for
    /// reading, and the scrobbler reads the stores during its turn. That is
    /// safe because nothing waits to write the stores while the scrobbler
    /// runs: the server stops the scrobbler before it closes them.
    #[derive(Debug, Default)]
    pub(crate) struct Turn {
        lock: std::sync::Mutex<()>,
        changes: std::sync::atomic::AtomicUsize,
    }

    /// A change to the session under way: while it is held, nothing is
    /// sent.
    #[derive(Debug)]
    pub(crate) struct Change<'a> {
        _held: std::sync::MutexGuard<'a, ()>,
        changes: &'a std::sync::atomic::AtomicUsize,
    }

    impl Drop for Change<'_> {
        fn drop(&mut self) {
            self.changes.fetch_sub(1, Ordering::SeqCst);
        }
    }

    impl Turn {
        /// Waits for a batch on its way to be recorded, and holds the
        /// scrobbler back until the change is dropped.
        pub(crate) fn change(&self) -> Change<'_> {
            self.changes.fetch_add(1, Ordering::SeqCst);
            Change {
                _held: lock(&self.lock),
                changes: &self.changes,
            }
        }

        /// The scrobbler's turn for one batch, unless a change is waiting
        /// or under way.
        fn batch(&self) -> Option<std::sync::MutexGuard<'_, ()>> {
            let waiting = || self.changes.load(Ordering::SeqCst) > 0;
            if waiting() {
                return None;
            }
            let held = lock(&self.lock);
            // A change may have begun to wait meanwhile.
            (!waiting()).then_some(held)
        }

        #[cfg(test)]
        fn changing(&self) -> bool {
            self.changes.load(Ordering::SeqCst) > 0
        }
    }

    /// Tells Last.fm what began to play, once it has settled, when
    /// scrobbling is on: once, never kept and never sent again. Returns
    /// how long until a track waiting has settled.
    pub(crate) fn now_playing_once(core: &Core, lastfm: &Lastfm) -> Option<std::time::Duration> {
        let began = match lastfm.playing.take(Instant::now()) {
            Taken::Nothing => return None,
            Taken::Wait(wait) => return Some(wait),
            Taken::Show(began) => began,
        };
        let client = lastfm.client().ok()?;
        // As a batch of scrobbles is: never as a session that has changed
        // by the time it is sent, nor while a restored library waits.
        let _turn = lastfm.turn.batch()?;
        let session = core.with_stores(|stores| stores.server().service_session(SERVICE));
        let Some(Ok(Some(session))) = session else {
            return None;
        };
        if session.scrobble_from_ms.is_none() || session.sign_in_again {
            return None;
        }
        if !matches!(
            core.with_library(|_, library| library.held_for_import()),
            Some(Ok(None))
        ) {
            return None;
        }
        let names = match core.with_library(|_, library| library.playing(began.appearance)) {
            Some(Ok(Some(names))) => names,
            // Not the library's hub, which alone scrobbles.
            None => return None,
            Some(Ok(None)) => {
                debug!("the track playing has no artist or title to show at Last.fm");
                return None;
            }
            Some(Err(failed)) => {
                warn!(error = %failed, "the track playing could not be read for Last.fm");
                return None;
            }
        };
        let playing = Playing {
            artist: names.artist,
            title: names.title,
            album: names.album,
            album_artist: names.album_artist,
            track_number: names.track_number.and_then(|n| u32::try_from(n).ok()),
            duration_ms: Some(began.duration_ms).filter(|ms| *ms > 0),
        };
        match client.now_playing(&SessionKey::new(session.key.expose()), &playing) {
            Ok(()) => debug!("Last.fm is told what is playing"),
            Err(NotShown::SignInAgain(_)) => {
                warn!("Last.fm refused the session: nothing is scrobbled until a new sign-in");
                if let Some(Err(failed)) = core.with_stores(|stores| {
                    stores
                        .server()
                        .refuse_service_session(SERVICE, session.key.expose())
                }) {
                    error!(error = %failed, "a refused session could not be recorded");
                }
            }
            Err(NotShown::Failed(detail)) => {
                debug!(error = %detail, "Last.fm could not be told what is playing");
            }
        }
        None
    }

    /// How often a sync's counters are saved and sent to subscribers.
    const SAVE_EVERY: std::time::Duration = std::time::Duration::from_secs(1);

    fn lock<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
        mutex
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn unavailable(message: &str) -> RpcError {
        RpcError::new(ErrorCode::Unavailable, message)
    }

    impl Lastfm {
        /// Waits for a batch the scrobbler is sending to be recorded, and
        /// holds it back while the session changes: signed out or with
        /// scrobbling off, the change is the cutoff, and nothing more goes
        /// out as the session before it.
        pub(crate) fn hold(&self) -> Change<'_> {
            self.turn.change()
        }

        /// Whether the server has an API key and secret.
        pub(crate) fn configured(&self) -> bool {
            self.client.is_some()
        }

        /// The client, or why there is none.
        pub(crate) fn client(&self) -> Result<&ritornel_lastfm::Lastfm, RpcError> {
            self.client.as_ref().ok_or_else(|| {
                let reason = self.problem.clone().unwrap_or_else(|| {
                    format!(
                        "this server has no Last.fm API key and secret: set {} and {} in its \
                         environment, or put them in {} in its data directory, readable by \
                         you alone, and start it again",
                        ritornel_lastfm::KEY_VARIABLE,
                        ritornel_lastfm::SECRET_VARIABLE,
                        ritornel_lastfm::CREDENTIALS_FILE
                    )
                });
                unavailable(&reason).with_detail("reason", "no_credentials")
            })
        }

        /// Starts a sign-in: a token from Last.fm, kept here under a new
        /// ID, and the address where the listener approves it.
        pub(crate) fn begin_sign_in(&self, id: Id) -> Result<String, RpcError> {
            let started = self
                .client()?
                .begin_sign_in()
                .map_err(|error| unavailable(&error.to_string()))?;
            lock(&self.sign_ins).insert(id, SignIn::Waiting(started.token));
            Ok(started.approval_url)
        }

        /// Exchanges the sign-in for a session once the listener has
        /// approved it, and keeps it with `keep`. `None` while they have
        /// not. A sign-in already done answers with its account again.
        pub(crate) fn finish_sign_in(
            &self,
            id: Id,
            keep: impl FnOnce(&ritornel_lastfm::Account) -> Result<(), RpcError>,
        ) -> Result<Option<String>, RpcError> {
            let state = lock(&self.sign_ins).get(&id).cloned().ok_or_else(|| {
                RpcError::new(
                    ErrorCode::NotFound,
                    "no such sign-in on this server: start another",
                )
            })?;
            let token = match state {
                SignIn::Done(account) => return Ok(Some(account)),
                SignIn::Waiting(token) => token,
            };
            let Some(account) = self
                .client()?
                .finish_sign_in(&token)
                .map_err(|error| unavailable(&error.to_string()))?
            else {
                return Ok(None);
            };
            keep(&account)?;
            lock(&self.sign_ins).insert(id, SignIn::Done(account.name.clone()));
            info!(account = %account.name, "signed in to Last.fm");
            Ok(Some(account.name))
        }
    }

    /// Where a sync writes: the library, through the one ingest call, with
    /// the job's counters saved as it goes.
    struct JobSink<'a> {
        core: &'a Core,
        lastfm: &'a Lastfm,
        account: Id,
        job: &'a Job,
        stop: &'a std::sync::atomic::AtomicBool,
        batches: u64,
        summary: SyncSummary,
        saved: Instant,
    }

    fn sink_failed(error: impl std::fmt::Display) -> SourceError {
        SourceError::sink(error.to_string())
    }

    impl JobSink<'_> {
        fn cancellation_requested(&self) -> bool {
            self.job.cancel_requested || self.core.with_stores(|stores| {
                matches!(stores.server().job(self.job.public_id), Ok(Some(job)) if job.cancel_requested)
            }).unwrap_or(false)
        }

        fn library<T>(
            &self,
            work: impl FnOnce(&LibraryStore) -> Result<T, ritornel_engine::Error>,
        ) -> Result<T, SourceError> {
            self.core
                .with_library(|_, library| work(library))
                .ok_or_else(|| sink_failed("the library is closed"))?
                .map_err(sink_failed)
        }

        fn save(&mut self, read: &Read) {
            let progress = Progress {
                step: i64::try_from(self.batches).unwrap_or(i64::MAX),
                checkpoint_json: serde_json::to_string(&self.summary).ok(),
                done: i64::try_from(read.read).unwrap_or(i64::MAX),
                total: read.total.and_then(|total| i64::try_from(total).ok()),
                label: read.label.clone(),
                ..Progress::default()
            };
            let row = self.job.id;
            if let Some(Ok(job)) = self
                .core
                .with_stores(|stores| stores.server().save_progress(row, progress))
            {
                self.core.publish_job(&job);
            }
            self.saved = Instant::now();
        }
    }

    impl Sink for JobSink<'_> {
        fn cursor(&mut self) -> Result<Option<String>, SourceError> {
            let account = self.account;
            self.library(|library| library.history_cursor(account))
        }

        fn commit(&mut self, batch: Batch) -> Result<Stored, SourceError> {
            let account = self.account;
            let outcome = self.library(|library| {
                library.ingest(Observations::Imports {
                    account,
                    batch: &batch,
                })
            })?;
            let ritornel_engine::IngestOutcome::Imports(stored) = outcome else {
                return Err(sink_failed("imports ingested as a listen"));
            };
            self.batches += 1;
            self.summary.read += batch.records.len() as u64;
            self.summary.new += stored.new;
            self.summary.gone += stored.gone;
            if self.lastfm.fail_after_batch() == Some(self.batches) {
                // The crash test asked for the server to die here, after
                // the library committed the batch and its cursor.
                warn!("stopping at a fail point, as a crash would");
                std::process::abort();
            }
            Ok(stored)
        }

        fn loves(&mut self, loves: Vec<Love>) -> Result<LovesStored, SourceError> {
            let account = self.account;
            let stored = self.library(|library| library.replace_loves(account, &loves))?;
            self.summary.loved = Some(stored.loved);
            Ok(stored)
        }

        fn progress(&mut self, progress: Read) {
            if self.saved.elapsed() >= SAVE_EVERY || progress.read == 0 {
                self.save(&progress);
            }
        }

        fn stopping(&self) -> bool {
            self.stop.load(std::sync::atomic::Ordering::SeqCst) || self.cancellation_requested()
        }
    }

    /// Runs a claimed sync to its end, or until the server stops, which
    /// leaves it claimed for the next run to carry on from the cursor.
    pub(crate) fn run_sync(core: &Core, job: Job, stop: &std::sync::atomic::AtomicBool) {
        let (row, job_id) = (job.id, job.public_id);
        info!(job_id = %job_id, kind = %job.kind, "a job is claimed");
        let finish = |state: JobState, failure: Option<String>, summary: Option<&SyncSummary>| {
            let finished = core.with_stores(|stores| {
                if let Some(summary) = summary {
                    stores.server().save_progress(
                        row,
                        Progress {
                            checkpoint_json: serde_json::to_string(summary).ok(),
                            done: i64::try_from(summary.read).unwrap_or(i64::MAX),
                            total: summary.source_total.and_then(|t| i64::try_from(t).ok()),
                            ..Progress::default()
                        },
                    )?;
                }
                stores.server().finish_job(row, state, failure)
            });
            match finished {
                Some(Ok(job)) => {
                    match &job.error {
                        Some(failure) if state == JobState::Failed => warn!(
                            job_id = %job_id,
                            error = %failure,
                            "a Last.fm sync failed: the next import tries again"
                        ),
                        _ => info!(job_id = %job_id, state = state.as_str(), "a job ended"),
                    }
                    core.publish_job(&job);
                }
                Some(Err(failed)) => {
                    error!(job_id = %job_id, error = %failed, "a job's end could not be saved")
                }
                None => {}
            }
        };
        let Some(lastfm) = core.lastfm() else {
            return finish(
                JobState::Failed,
                Some("Last.fm is not set up".to_owned()),
                None,
            );
        };
        let client = match lastfm.client() {
            Ok(client) => client,
            Err(error) => return finish(JobState::Failed, Some(error.message), None),
        };
        let full = serde_json::from_str::<ritornel_contract::LastfmSyncParams>(&job.params_json)
            .is_ok_and(|params| params.full);
        let session = core.with_stores(|stores| stores.server().service_session(SERVICE));
        let session = match session {
            Some(Ok(Some(session))) => session,
            Some(Ok(None)) => {
                return finish(
                    JobState::Failed,
                    Some("not signed in to Last.fm".to_owned()),
                    None,
                );
            }
            Some(Err(failed)) => return finish(JobState::Failed, Some(failed.to_string()), None),
            None => return,
        };
        let mut sink = JobSink {
            core,
            lastfm,
            account: session.account,
            job: &job,
            stop,
            batches: 0,
            summary: SyncSummary::default(),
            saved: Instant::now(),
        };
        let synced = client.source(&session.account_name).sync(&mut sink, full);
        let mut summary = sink.summary;
        match synced {
            Ok(synced) if synced.stopped => {
                if sink.cancellation_requested() {
                    finish(JobState::Cancelled, None, Some(&summary));
                }
                // Stopped with the server: the next run carries on.
            }
            Ok(synced) => {
                summary.source_total = synced.source_total;
                let account = session.account;
                let after = core.with_library(|_, library| {
                    let matched = library.match_scrobbles(account)?;
                    Ok::<_, ritornel_engine::Error>((matched, library.imported_count(account)?))
                });
                match after {
                    Some(Ok((matched, stored))) => {
                        // What Last.fm has is known now: plays a restored
                        // library had waiting that it took are not sent.
                        match core.with_library(|_, library| library.release_hold(account)) {
                            Some(Ok(true)) => {
                                info!("an import has run since the restore: scrobbling resumes");
                                core.wake_scrobbler();
                            }
                            Some(Err(failed)) => {
                                error!(error = %failed, "scrobbling could not be resumed after an import")
                            }
                            Some(Ok(false)) | None => {}
                        }
                        summary.matched_after = matched as u64;
                        summary.stored = u64::try_from(stored).ok();
                        info!(
                            read = summary.read,
                            new = summary.new,
                            gone = summary.gone,
                            stored = stored,
                            lastfm_total = ?summary.source_total,
                            "the Last.fm history is synced"
                        );
                        finish(JobState::Succeeded, None, Some(&summary));
                    }
                    Some(Err(failed)) => {
                        finish(JobState::Failed, Some(failed.to_string()), Some(&summary))
                    }
                    None => {}
                }
            }
            Err(failed) => finish(JobState::Failed, Some(failed.to_string()), Some(&summary)),
        }
    }

    fn to_send(due: &Due) -> ToScrobble {
        ToScrobble {
            artist: due.artist.clone(),
            title: due.title.clone(),
            album: due.album.clone(),
            album_artist: due.album_artist.clone(),
            track_number: due.track_number.and_then(|n| u32::try_from(n).ok()),
            duration_ms: due.duration_ms,
            timestamp_s: u64::try_from(due.timestamp_s).unwrap_or(0),
        }
    }

    /// Records what is eligible to be scrobbled and sends what is due, in
    /// batches, until nothing is due. Each batch is marked as being sent
    /// before it leaves.
    pub(crate) fn scrobble_once(core: &Core, lastfm: &Lastfm, stop: &AtomicBool) {
        let Ok(client) = lastfm.client() else {
            return;
        };
        let session = core.with_stores(|stores| stores.server().service_session(SERVICE));
        let Some(Ok(Some(session))) = session else {
            return;
        };
        let Some(from) = session.scrobble_from_ms.filter(|_| !session.sign_in_again) else {
            return;
        };
        let account = session.account;
        match core.with_library(|_, library| library.plan_deliveries(account, from)) {
            Some(Ok(0)) | None => {}
            Some(Ok(planned)) => info!(planned, "plays to scrobble are recorded"),
            Some(Err(failed)) => {
                error!(error = %failed, "plays to scrobble could not be recorded");
                return;
            }
        }
        send_due(&Sending {
            turn: &lastfm.turn,
            session: &|| match core.with_stores(|stores| stores.server().service_session(SERVICE)) {
                Some(Ok(session)) => session,
                _ => None,
            },
            stopping: &|| stop.load(Ordering::SeqCst),
            held: &|| match core.with_library(|_, library| library.held_for_import()) {
                Some(Ok(held)) => held.is_some(),
                Some(Err(failed)) => {
                    error!(error = %failed, "whether scrobbling waits for an import could not be read");
                    true
                }
                None => true,
            },
            take: &|account, from| match core.with_library(|_, library| {
                library.take_due_deliveries(account, from, ritornel_lastfm::MAX_BATCH)
            }) {
                Some(Ok(due)) => Some(due),
                Some(Err(failed)) => {
                    error!(error = %failed, "plays to scrobble could not be taken");
                    None
                }
                None => None,
            },
            send: &|key, plays| client.scrobble(key, plays),
            settle: &|settled| match core
                .with_library(|_, library| library.settle_deliveries(settled))
            {
                Some(Ok(())) => true,
                Some(Err(failed)) => {
                    error!(error = %failed, "what became of scrobbles could not be recorded");
                    false
                }
                None => false,
            },
            refuse: &|key| {
                if let Some(Err(failed)) = core.with_stores(|stores| {
                    stores
                        .server()
                        .refuse_service_session(SERVICE, key.expose())
                }) {
                    error!(error = %failed, "a refused session could not be recorded");
                }
            },
            now: &|| ritornel_engine::Clock::now_ms(&ritornel_engine::SystemClock),
            paused: &|account| {
                lastfm
                    .paused
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .is_some_and(|(paused, until)| {
                        paused == account && std::time::Instant::now() < until
                    })
            },
            pause: &|account, wait| {
                *lastfm
                    .paused
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) =
                    Some((account, std::time::Instant::now() + wait));
            },
        });
    }

    /// Sends a batch of plays as a session, once.
    pub(crate) type SendBatch<'a> = dyn Fn(&SessionKey, &[ToScrobble]) -> Result<Sent, Unsent> + 'a;

    /// What sending scrobbles works through: the server's stores and
    /// Last.fm in use, and fakes in a test.
    pub(crate) struct Sending<'a> {
        /// Taken from each read of the session until the batch sent as it
        /// is recorded; see [`Turn`].
        pub(crate) turn: &'a Turn,
        /// This server's session at Last.fm, as it is now.
        pub(crate) session: &'a dyn Fn() -> Option<ServiceSession>,
        /// Whether the server is stopping.
        pub(crate) stopping: &'a dyn Fn() -> bool,
        /// Whether nothing is sent until an import has run, as in a library
        /// restored from a backup: Last.fm may already have plays it holds
        /// waiting, and the import brings them back first.
        pub(crate) held: &'a dyn Fn() -> bool,
        /// Takes the next batch for an account, checked against the time
        /// scrobbling was turned on; `None` when it could not.
        pub(crate) take: &'a dyn Fn(Id, i64) -> Option<Vec<Due>>,
        /// Sends a batch, once.
        pub(crate) send: &'a SendBatch<'a>,
        /// Records what became of a batch; `false` when it could not.
        pub(crate) settle: &'a dyn Fn(Vec<(i64, Settled)>) -> bool,
        /// Records that Last.fm refused the session.
        pub(crate) refuse: &'a dyn Fn(&SessionKey),
        /// The time now.
        pub(crate) now: &'a dyn Fn() -> i64,
        /// Whether nothing is sent for this account for now, after its
        /// daily limit was met, a request was refused or a batch went
        /// unanswered.
        pub(crate) paused: &'a dyn Fn(Id) -> bool,
        /// Sends nothing for this account for this long.
        pub(crate) pause: &'a dyn Fn(Id, std::time::Duration),
    }

    /// Sends what is due, a batch at a time, until nothing is due. Before
    /// each batch the session is read again: signed out, scrobbling turned
    /// off, the session refused or the server stopping, nothing more is
    /// sent, and a new sign-in's key is the one used. The session cannot
    /// change from that read until the batch is recorded, and a change
    /// waiting ends the sending: whoever made it wakes the scrobbler.
    pub(crate) fn send_due(sending: &Sending<'_>) {
        loop {
            if (sending.stopping)() {
                return;
            }
            let Some(_turn) = sending.turn.batch() else {
                return;
            };
            let Some(session) = (sending.session)() else {
                return;
            };
            let Some(from) = session.scrobble_from_ms.filter(|_| !session.sign_in_again) else {
                return;
            };
            // This account met its daily limit, was refused or went
            // unanswered: nothing goes
            // until the wait is over, and then the plays held back go first,
            // being the oldest due. Another account signed in since is not
            // held back.
            if (sending.paused)(session.account) || (sending.held)() {
                return;
            }
            let key = SessionKey::new(session.key.expose());
            let due = match (sending.take)(session.account, from) {
                Some(due) if !due.is_empty() => due,
                _ => return,
            };
            let plays: Vec<ToScrobble> = due.iter().map(to_send).collect();
            let now = (sending.now)();
            let (settled, stop): (Vec<(i64, Settled)>, bool) = match (sending.send)(&key, &plays) {
                Ok(outcomes) => {
                    // A play over the daily limit is no answer about it: it
                    // is sent again later, and nothing more is sent now.
                    let mut limited = false;
                    let settled = due
                        .iter()
                        .zip(outcomes)
                        .map(|(due, outcome)| {
                            (
                                due.delivery,
                                match outcome {
                                    Outcome::Unknown => Settled::Unknown(
                                        "Last.fm's answer did not identify this play".to_owned(),
                                    ),
                                    Outcome::Accepted { artist, title } => {
                                        Settled::Accepted { artist, title }
                                    }
                                    Outcome::Ignored { code, message } if code == DAILY_LIMIT => {
                                        limited = true;
                                        Settled::Later {
                                            detail: message,
                                            at_ms: now + DAILY_LIMIT_RETRY_MS,
                                        }
                                    }
                                    Outcome::Ignored { code, message } => Settled::Ignored {
                                        code: i64::from(code),
                                        message,
                                    },
                                },
                            )
                        })
                        .collect();
                    if limited {
                        warn!("Last.fm's daily limit is reached: nothing is scrobbled for an hour");
                        (sending.pause)(session.account, DAILY_LIMIT_WAIT);
                    }
                    (settled, limited)
                }
                Err(unsent) => {
                    let (each, stop): (Box<dyn Fn() -> Settled>, bool) = match unsent {
                        Unsent::Later(detail) => (
                            Box::new(move || Settled::Later {
                                detail: detail.clone(),
                                at_ms: now + RETRY_MS,
                            }),
                            true,
                        ),
                        Unsent::SignInAgain(detail) => {
                            warn!(
                                "Last.fm refused the session: nothing is scrobbled until a new sign-in"
                            );
                            (sending.refuse)(&key);
                            (
                                Box::new(move || Settled::Later {
                                    detail: detail.clone(),
                                    at_ms: now + RETRY_MS,
                                }),
                                true,
                            )
                        }
                        // Last.fm refuses the request itself: every batch
                        // would be, so nothing more goes for a while.
                        Unsent::Refused(detail) => {
                            warn!("Last.fm refused the request: nothing is scrobbled for a while");
                            (sending.pause)(session.account, REFUSED_WAIT);
                            (Box::new(move || Settled::Refused(detail.clone())), true)
                        }
                        // No answer: these may have arrived, and are never
                        // sent again. The rest wait, so that an outage
                        // does not use up every play waiting.
                        Unsent::Unknown(detail) => {
                            warn!(
                                "Last.fm did not answer a batch of scrobbles: nothing more is \
                                 scrobbled for a while"
                            );
                            (sending.pause)(session.account, UNANSWERED_WAIT);
                            (Box::new(move || Settled::Unknown(detail.clone())), true)
                        }
                    };
                    (due.iter().map(|due| (due.delivery, each())).collect(), stop)
                }
            };
            let accepted = settled
                .iter()
                .filter(|(_, s)| matches!(s, Settled::Accepted { .. }))
                .count();
            if !(sending.settle)(settled) {
                return;
            }
            info!(sent = plays.len(), accepted, "plays scrobbled to Last.fm");
            if stop {
                return;
            }
        }
    }

    /// Keeps a session the listener approved: the account in the library,
    /// made if new, and the session key in `server.db`.
    pub(crate) fn keep_session(
        server: &ServerStore,
        library: &LibraryStore,
        account: &ritornel_lastfm::Account,
    ) -> Result<Id, ritornel_engine::Error> {
        let id = library.ensure_service_account(SERVICE, &account.name)?;
        server.save_service_session(
            SERVICE,
            id,
            &account.name,
            &ServiceKey::new(account.key.expose()),
            account.subscriber,
        )?;
        Ok(id)
    }

    #[cfg(test)]
    mod tests {
        use std::cell::{Cell, RefCell};

        use ritornel_contract::Id;
        use ritornel_engine::{Due, ServiceKey, ServiceSession, Settled};
        use ritornel_lastfm::{Outcome, Sent, SessionKey, ToScrobble, Unsent};

        use super::{
            Began, DAILY_LIMIT_RETRY_MS, DAILY_LIMIT_WAIT, Instant, JobSink, Lastfm, REFUSED_WAIT,
            SETTLE, STALE, Sending, Sink, Slot, Taken, UNANSWERED_WAIT, send_due,
        };

        #[test]
        fn a_sync_observes_cancellation_between_progress_saves() {
            use ritornel_engine::{Command, Fingerprint, NewJob};
            let (core, scratch) = crate::testing::core();
            let lastfm = Lastfm::new(Default::default(), scratch.path());
            let id = |n| Id::from_bytes([n; 16]);
            let command = |n, method| Command {
                id: id(n),
                fingerprint: Fingerprint::of(method, b"{}"),
            };
            let job = core
                .with_stores(|stores| {
                    let store = stores.server();
                    store
                        .start_job(
                            command(1, "lastfm.sync"),
                            "lastfm.sync",
                            NewJob {
                                kind: "lastfm.sync",
                                dedupe_key: "someone".into(),
                                target: id(2),
                                params_json: "{}".into(),
                                session: None,
                            },
                        )
                        .unwrap();
                    store.claim_job(id(3)).unwrap().unwrap()
                })
                .unwrap();
            let stop = std::sync::atomic::AtomicBool::new(false);
            let sink = JobSink {
                core: &core,
                lastfm: &lastfm,
                account: id(2),
                job: &job,
                stop: &stop,
                batches: 0,
                summary: Default::default(),
                saved: Instant::now(),
            };
            assert!(!sink.stopping());
            core.with_stores(|stores| {
                stores
                    .server()
                    .cancel_job(command(4, "jobs.cancel"), "jobs.cancel", job.public_id)
                    .unwrap()
                    .unwrap()
            })
            .unwrap();
            assert!(sink.stopping());
        }

        #[test]
        fn of_tracks_skipped_through_only_the_last_is_shown_once_it_settles() {
            let t0 = std::time::Instant::now();
            let ms = std::time::Duration::from_millis;
            let began = |byte: u8, after: u64| Began {
                appearance: Id::from_bytes([byte; 16]),
                duration_ms: 200_000,
                at: t0 + ms(after),
            };
            let slot = Slot::default();
            assert_eq!(slot.take(t0), Taken::Nothing);
            // Three in quick succession: the last wins, once it settles.
            for (byte, after) in [(1, 0), (2, 300), (3, 600)] {
                slot.put(began(byte, after));
            }
            assert_eq!(slot.take(t0 + ms(700)), Taken::Wait(SETTLE - ms(100)));
            assert_eq!(slot.take(t0 + ms(600) + SETTLE), Taken::Show(began(3, 600)));
            // Taken once, it is gone.
            assert_eq!(slot.take(t0 + ms(5_000)), Taken::Nothing);
            // A pause before it settles lets it go.
            slot.put(began(4, 0));
            slot.clear();
            assert_eq!(slot.take(t0 + SETTLE), Taken::Nothing);
            // One kept back too long is let go.
            slot.put(began(5, 0));
            assert_eq!(slot.take(t0 + STALE + ms(1)), Taken::Nothing);
            assert_eq!(slot.take(t0 + STALE + ms(2)), Taken::Nothing);
        }

        fn session(key: &str, on: bool) -> ServiceSession {
            ServiceSession {
                account: Id::from_bytes([7; 16]),
                account_name: "someone".to_owned(),
                key: ServiceKey::new(key),
                subscriber: false,
                signed_in_at_ms: 0,
                scrobble_from_ms: on.then_some(0),
                sign_in_again: false,
            }
        }

        /// A batch of 50 plays, each of its own second.
        fn batch(first: i64) -> Vec<Due> {
            (first..first + 50)
                .map(|n| Due {
                    delivery: n,
                    artist: "Invented Artist".to_owned(),
                    title: format!("Song {n}"),
                    album: None,
                    album_artist: None,
                    track_number: None,
                    duration_ms: None,
                    timestamp_s: 1_800_000_000 + n,
                })
                .collect()
        }

        /// Sends three batches' worth of plays, with the session as
        /// `sessions` says before each read of it, and the server stopping
        /// once `stop_after` batches are sent. Returns the keys each batch
        /// went out with.
        fn run(
            sessions: &dyn Fn(usize) -> Option<ServiceSession>,
            stop_after: usize,
        ) -> Vec<String> {
            let reads = Cell::new(0);
            let taken = Cell::new(0_i64);
            let sent = RefCell::new(Vec::new());
            let turn = super::Turn::default();
            send_due(&Sending {
                turn: &turn,
                session: &|| {
                    reads.set(reads.get() + 1);
                    sessions(reads.get() - 1)
                },
                stopping: &|| sent.borrow().len() >= stop_after,
                held: &|| false,
                take: &|_, _| {
                    taken.set(taken.get() + 1);
                    (taken.get() <= 3).then(|| batch(taken.get() * 100))
                },
                send: &|key: &SessionKey, plays: &[ToScrobble]| -> Result<Sent, Unsent> {
                    sent.borrow_mut().push(key.expose().to_owned());
                    Ok(plays
                        .iter()
                        .map(|play| Outcome::Accepted {
                            artist: play.artist.clone(),
                            title: play.title.clone(),
                        })
                        .collect())
                },
                settle: &|_| true,
                refuse: &|_| {},
                now: &|| 0,
                paused: &|_| false,
                pause: &|_, _| {},
            });
            sent.into_inner()
        }

        #[test]
        fn a_play_over_the_daily_limit_is_sent_again_later_and_one_too_old_is_not() {
            let sent = Cell::new(0);
            let settled = RefCell::new(Vec::new());
            // Which account is paused, and whether its wait is over.
            let paused = Cell::new(None);
            let over = Cell::new(false);
            let turn = super::Turn::default();
            let sending = Sending {
                turn: &turn,
                session: &|| Some(session("KEY_A", true)),
                stopping: &|| false,
                held: &|| false,
                take: &|_, _| Some(batch(100)),
                send: &|_: &SessionKey, plays: &[ToScrobble]| -> Result<Sent, Unsent> {
                    sent.set(sent.get() + 1);
                    Ok(plays
                        .iter()
                        .enumerate()
                        .map(|(n, _)| Outcome::Ignored {
                            code: if n == 0 { 3 } else { 5 },
                            message: if n == 0 { "too old" } else { "daily limit" }.to_owned(),
                        })
                        .collect())
                },
                settle: &|batch| {
                    settled.borrow_mut().extend(batch);
                    true
                },
                refuse: &|_| {},
                now: &|| 1_000,
                paused: &|account| {
                    paused.get().is_some_and(|(held, _)| held == account) && !over.get()
                },
                pause: &|account, wait| paused.set(Some((account, wait))),
            };
            send_due(&sending);
            // The limit is reached: nothing more is sent, however often it
            // is woken, until the wait is over.
            assert_eq!(sent.get(), 1);
            assert!(matches!(paused.get(), Some((_, wait)) if wait == DAILY_LIMIT_WAIT));
            let first = std::mem::take(&mut *settled.borrow_mut());
            assert_eq!(first.len(), 50);
            assert!(matches!(&first[0].1, Settled::Ignored { code: 3, .. }));
            for (_, later) in &first[1..] {
                assert!(
                    matches!(later, Settled::Later { at_ms, .. } if *at_ms == 1_000 + DAILY_LIMIT_RETRY_MS),
                    "{later:?}"
                );
            }
            send_due(&sending);
            assert_eq!(sent.get(), 1);
            over.set(true);
            send_due(&sending);
            assert_eq!(sent.get(), 2);
        }

        #[test]
        fn one_accounts_pause_does_not_hold_back_another() {
            let sent = Cell::new(0);
            let another = Id::from_bytes([8; 16]);
            let turn = super::Turn::default();
            send_due(&Sending {
                turn: &turn,
                session: &|| Some(session("KEY_B", true)),
                stopping: &|| sent.get() >= 1,
                held: &|| false,
                take: &|_, _| Some(batch(100)),
                send: &|_: &SessionKey, plays: &[ToScrobble]| -> Result<Sent, Unsent> {
                    sent.set(sent.get() + 1);
                    Ok(plays
                        .iter()
                        .map(|play| Outcome::Accepted {
                            artist: play.artist.clone(),
                            title: play.title.clone(),
                        })
                        .collect())
                },
                settle: &|_| true,
                refuse: &|_| {},
                now: &|| 1_000,
                paused: &|account| account == another,
                pause: &|_, _| {},
            });
            assert_eq!(sent.get(), 1);
        }

        #[test]
        fn a_request_refused_whole_stops_the_sending_and_pauses_it() {
            let sent = Cell::new(0);
            let paused = Cell::new(None);
            let refused = RefCell::new(Vec::new());
            let turn = super::Turn::default();
            send_due(&Sending {
                turn: &turn,
                session: &|| Some(session("KEY_A", true)),
                stopping: &|| false,
                held: &|| false,
                take: &|_, _| Some(batch(100)),
                send: &|_: &SessionKey, _: &[ToScrobble]| -> Result<Sent, Unsent> {
                    sent.set(sent.get() + 1);
                    Err(Unsent::Refused("Invalid API key".to_owned()))
                },
                settle: &|_| true,
                refuse: &|key| refused.borrow_mut().push(key.expose().to_owned()),
                now: &|| 1_000,
                paused: &|_| paused.get().is_some(),
                pause: &|_, wait| paused.set(Some(wait)),
            });
            assert_eq!(sent.get(), 1);
            assert_eq!(paused.get(), Some(REFUSED_WAIT));
            // A request refused is not the session refused.
            assert!(refused.borrow().is_empty());
        }

        #[test]
        fn a_batch_that_goes_unanswered_stops_the_sending_and_the_rest_wait() {
            let (sent, taken) = (Cell::new(0), Cell::new(0_i64));
            let settled = RefCell::new(Vec::new());
            let paused = Cell::new(None);
            let turn = super::Turn::default();
            send_due(&Sending {
                turn: &turn,
                session: &|| Some(session("KEY_A", true)),
                stopping: &|| false,
                held: &|| false,
                take: &|_, _| {
                    taken.set(taken.get() + 1);
                    (taken.get() <= 3).then(|| batch(taken.get() * 100))
                },
                send: &|_: &SessionKey, _: &[ToScrobble]| -> Result<Sent, Unsent> {
                    sent.set(sent.get() + 1);
                    Err(Unsent::Unknown("502 Bad Gateway".to_owned()))
                },
                settle: &|batch| {
                    settled.borrow_mut().extend(batch);
                    true
                },
                refuse: &|_| {},
                now: &|| 1_000,
                paused: &|_| paused.get().is_some(),
                pause: &|_, wait| paused.set(Some(wait)),
            });
            // One batch may have arrived and is never sent again; the
            // others were not taken, and wait.
            assert_eq!((sent.get(), taken.get()), (1, 1));
            assert_eq!(settled.borrow().len(), 50);
            assert!(
                settled
                    .borrow()
                    .iter()
                    .all(|(_, s)| matches!(s, Settled::Unknown(_)))
            );
            assert_eq!(paused.get(), Some(UNANSWERED_WAIT));
        }

        #[test]
        fn nothing_is_sent_from_a_restored_library_until_an_import_has_run() {
            let (sent, taken) = (Cell::new(0), Cell::new(0));
            let imported = Cell::new(false);
            let turn = super::Turn::default();
            let sending = Sending {
                turn: &turn,
                session: &|| Some(session("KEY_A", true)),
                stopping: &|| false,
                held: &|| !imported.get(),
                take: &|_, _| {
                    taken.set(taken.get() + 1);
                    (taken.get() == 1).then(|| batch(100))
                },
                send: &|_: &SessionKey, plays: &[ToScrobble]| -> Result<Sent, Unsent> {
                    sent.set(sent.get() + 1);
                    Ok(plays
                        .iter()
                        .map(|play| Outcome::Accepted {
                            artist: play.artist.clone(),
                            title: play.title.clone(),
                        })
                        .collect())
                },
                settle: &|_| true,
                refuse: &|_| {},
                now: &|| 1_000,
                paused: &|_| false,
                pause: &|_, _| {},
            };
            // Nothing is even taken, so nothing waiting is withdrawn or
            // marked as sending.
            send_due(&sending);
            assert_eq!((taken.get(), sent.get()), (0, 0));
            imported.set(true);
            send_due(&sending);
            assert_eq!(sent.get(), 1);
        }

        #[test]
        fn a_sign_out_waits_for_the_batch_on_its_way_and_nothing_leaves_after_it() {
            use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
            use std::sync::{Arc, Barrier};
            let turn = Arc::new(super::Turn::default());
            let signed_in = Arc::new(AtomicBool::new(true));
            let sent = Arc::new(AtomicUsize::new(0));
            let settled = Arc::new(AtomicUsize::new(0));
            // The first batch is held at the moment it would leave until
            // the sign-out is waiting for it.
            let sending = Arc::new(Barrier::new(2));
            let scrobbler = {
                let (turn, signed_in, sent) = (turn.clone(), signed_in.clone(), sent.clone());
                let (sending, settled) = (sending.clone(), settled.clone());
                std::thread::spawn(move || {
                    let taken = Cell::new(0_i64);
                    send_due(&Sending {
                        turn: &turn,
                        session: &|| {
                            signed_in
                                .load(Ordering::SeqCst)
                                .then(|| session("KEY_A", true))
                        },
                        stopping: &|| false,
                        held: &|| false,
                        take: &|_, _| {
                            taken.set(taken.get() + 1);
                            (taken.get() <= 3).then(|| batch(taken.get() * 100))
                        },
                        send: &|_: &SessionKey, plays: &[ToScrobble]| -> Result<Sent, Unsent> {
                            if sent.fetch_add(1, Ordering::SeqCst) == 0 {
                                sending.wait();
                                while !turn.changing() {
                                    std::thread::yield_now();
                                }
                            }
                            Ok(plays
                                .iter()
                                .map(|play| Outcome::Accepted {
                                    artist: play.artist.clone(),
                                    title: play.title.clone(),
                                })
                                .collect())
                        },
                        settle: &|_| {
                            settled.fetch_add(1, Ordering::SeqCst);
                            true
                        },
                        refuse: &|_| {},
                        now: &|| 0,
                        paused: &|_| false,
                        pause: &|_, _| {},
                    });
                })
            };
            sending.wait();
            // The sign-out, as the call makes it: it waits for the batch on
            // its way, and once it is done nothing more leaves.
            {
                let _held = turn.change();
                // The batch on its way was sent and recorded before the
                // change went ahead.
                assert_eq!(sent.load(Ordering::SeqCst), 1);
                assert_eq!(settled.load(Ordering::SeqCst), 1);
                signed_in.store(false, Ordering::SeqCst);
            }
            scrobbler.join().unwrap();
            assert_eq!(sent.load(Ordering::SeqCst), 1);
        }

        #[test]
        fn every_batch_goes_out_with_the_session_as_it_is_then() {
            // Untouched, all three go out.
            assert_eq!(run(&|_| Some(session("KEY_A", true)), usize::MAX).len(), 3);
            // Turned off after the first batch: nothing more is sent.
            assert_eq!(
                run(&|read| Some(session("KEY_A", read == 0)), usize::MAX),
                ["KEY_A"]
            );
            // Signed out after the first batch: nothing more is sent.
            assert_eq!(
                run(
                    &|read| (read == 0).then(|| session("KEY_A", true)),
                    usize::MAX
                ),
                ["KEY_A"]
            );
            // Signed in again as a new session: the next batch goes out
            // with the new key.
            assert_eq!(
                run(
                    &|read| Some(session(if read == 0 { "KEY_A" } else { "KEY_B" }, true)),
                    usize::MAX
                ),
                ["KEY_A", "KEY_B", "KEY_B"]
            );
            // The server stopping after the first batch: nothing more.
            assert_eq!(run(&|_| Some(session("KEY_A", true)), 1), ["KEY_A"]);
        }
    }
}

/// The thread that scrobbles: it wakes when a listen has been delivered,
/// when a track begins to play, when a client opens, or every minute; tells
/// Last.fm what is playing, starts an import if one is due, and sends what
/// is due.
#[derive(Debug)]
pub(crate) struct Scrobbler {
    core: Arc<Core>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Scrobbler {
    /// Starts the thread, on a server built with Last.fm that holds a
    /// library.
    pub(crate) fn start(core: Arc<Core>) -> std::io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let thread = if cfg!(feature = "lastfm") && core.lastfm().is_some() {
            let (core, stop) = (Arc::clone(&core), Arc::clone(&stop));
            Some(
                std::thread::Builder::new()
                    .name("ritornel-scrobbler".to_owned())
                    .spawn(move || scrobble(&core, &stop))?,
            )
        } else {
            None
        };
        Ok(Self { core, stop, thread })
    }

    /// Stops the thread and waits for it: it ends after the batch it is
    /// sending.
    pub(crate) fn stop(mut self) {
        self.signal();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }

    fn signal(&self) {
        self.stop.store(true, Ordering::SeqCst);
        self.core.wake_scrobbler();
    }
}

impl Drop for Scrobbler {
    fn drop(&mut self) {
        self.signal();
    }
}

#[cfg_attr(
    not(feature = "lastfm"),
    allow(unused_variables, unused_assignments, unused_mut)
)]
fn scrobble(core: &Core, stop: &AtomicBool) {
    let mut first = true;
    while !stop.load(Ordering::SeqCst) {
        let mut wait = IDLE;
        #[cfg(feature = "lastfm")]
        if let Some(lastfm) = core.lastfm() {
            // First, as it is about now and the rest can wait.
            if let Some(settling) = now_playing_once(core, lastfm) {
                wait = settling;
            }
            import_tick(core, lastfm, first);
            scrobble_once(core, lastfm, stop);
        }
        first = false;
        match core.lastfm() {
            Some(lastfm) => lastfm.wake.wait(wait),
            None => return,
        }
    }
}
