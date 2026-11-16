//! Last.fm in the server: the client and its API key and secret, the
//! sign-ins in progress, the job that syncs an account's history, and the
//! worker that scrobbles. See "Last.fm" in docs/design/history.md.
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

/// The longest the scrobbler sleeps with nothing to do.
const IDLE: Duration = Duration::from_secs(60);

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
    /// The client, when the server has an API key and secret.
    #[cfg(feature = "lastfm")]
    client: Option<ritornel_lastfm::Lastfm>,
    /// Why there is no client, when the credentials could not be read.
    #[cfg(feature = "lastfm")]
    problem: Option<String>,
    /// Sign-ins in progress, by the ID handed out for each.
    #[cfg(feature = "lastfm")]
    sign_ins: std::sync::Mutex<std::collections::HashMap<ritornel_contract::Id, SignIn>>,
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
                client,
                problem,
                sign_ins: std::sync::Mutex::default(),
            }
        }
        #[cfg(not(feature = "lastfm"))]
        Self {
            wake: crate::playback::Wake::default(),
        }
    }

    /// Wakes the scrobbler.
    pub(crate) fn wake(&self) {
        self.wake.wake();
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
    use ritornel_lastfm::{Outcome, Sent, SessionKey, ToScrobble, Unsent};
    use ritornel_source::{
        Batch, Error as SourceError, HistorySource, Love, LovesStored, Progress as Read, Sink,
        Stored,
    };
    use tracing::{error, info, warn};

    use std::sync::atomic::{AtomicBool, Ordering};

    use super::{Lastfm, SERVICE, SignIn};
    use crate::dispatch::Core;

    /// How long a play that could not be sent waits before it is sent
    /// again.
    const RETRY_MS: i64 = 5 * 60 * 1000;

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
        row: i64,
        stop: &'a std::sync::atomic::AtomicBool,
        cancelled: bool,
        batches: u64,
        summary: SyncSummary,
        saved: Instant,
    }

    fn sink_failed(error: impl std::fmt::Display) -> SourceError {
        SourceError::sink(error.to_string())
    }

    impl JobSink<'_> {
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
            let row = self.row;
            if let Some(Ok(job)) = self
                .core
                .with_stores(|stores| stores.server().save_progress(row, progress))
            {
                self.cancelled |= job.cancel_requested;
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
            self.cancelled || self.stop.load(std::sync::atomic::Ordering::SeqCst)
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
                    info!(job_id = %job_id, state = state.as_str(), "a job ended");
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
            row,
            stop,
            cancelled: job.cancel_requested,
            batches: 0,
            summary: SyncSummary::default(),
            saved: Instant::now(),
        };
        let synced = client.source(&session.account_name).sync(&mut sink, full);
        let mut summary = sink.summary;
        match synced {
            Ok(synced) if synced.stopped => {
                if sink.cancelled {
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
            session: &|| match core.with_stores(|stores| stores.server().service_session(SERVICE)) {
                Some(Ok(session)) => session,
                _ => None,
            },
            stopping: &|| stop.load(Ordering::SeqCst),
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
            refuse: &|| {
                if let Some(Err(failed)) =
                    core.with_stores(|stores| stores.server().refuse_service_session(SERVICE))
                {
                    error!(error = %failed, "a refused session could not be recorded");
                }
            },
            now: &|| ritornel_engine::Clock::now_ms(&ritornel_engine::SystemClock),
        });
    }

    /// Sends a batch of plays as a session, once.
    pub(crate) type SendBatch<'a> = dyn Fn(&SessionKey, &[ToScrobble]) -> Result<Sent, Unsent> + 'a;

    /// What sending scrobbles works through: the server's stores and
    /// Last.fm in use, and fakes in a test.
    pub(crate) struct Sending<'a> {
        /// This server's session at Last.fm, as it is now.
        pub(crate) session: &'a dyn Fn() -> Option<ServiceSession>,
        /// Whether the server is stopping.
        pub(crate) stopping: &'a dyn Fn() -> bool,
        /// Takes the next batch for an account, checked against the time
        /// scrobbling was turned on; `None` when it could not.
        pub(crate) take: &'a dyn Fn(Id, i64) -> Option<Vec<Due>>,
        /// Sends a batch, once.
        pub(crate) send: &'a SendBatch<'a>,
        /// Records what became of a batch; `false` when it could not.
        pub(crate) settle: &'a dyn Fn(Vec<(i64, Settled)>) -> bool,
        /// Records that Last.fm refused the session.
        pub(crate) refuse: &'a dyn Fn(),
        /// The time now.
        pub(crate) now: &'a dyn Fn() -> i64,
    }

    /// Sends what is due, a batch at a time, until nothing is due. Before
    /// each batch the session is read again: signed out, scrobbling turned
    /// off, the session refused or the server stopping, nothing more is
    /// sent, and a new sign-in's key is the one used.
    pub(crate) fn send_due(sending: &Sending<'_>) {
        loop {
            if (sending.stopping)() {
                return;
            }
            let Some(session) = (sending.session)() else {
                return;
            };
            let Some(from) = session.scrobble_from_ms.filter(|_| !session.sign_in_again) else {
                return;
            };
            let key = SessionKey::new(session.key.expose());
            let due = match (sending.take)(session.account, from) {
                Some(due) if !due.is_empty() => due,
                _ => return,
            };
            let plays: Vec<ToScrobble> = due.iter().map(to_send).collect();
            let now = (sending.now)();
            let (settled, stop): (Vec<(i64, Settled)>, bool) = match (sending.send)(&key, &plays) {
                Ok(outcomes) => (
                    due.iter()
                        .zip(outcomes)
                        .map(|(due, outcome)| {
                            (
                                due.delivery,
                                match outcome {
                                    Outcome::Accepted { artist, title } => {
                                        Settled::Accepted { artist, title }
                                    }
                                    Outcome::Ignored { code, message } => Settled::Ignored {
                                        code: i64::from(code),
                                        message,
                                    },
                                },
                            )
                        })
                        .collect(),
                    false,
                ),
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
                            (sending.refuse)();
                            (
                                Box::new(move || Settled::Later {
                                    detail: detail.clone(),
                                    at_ms: now + RETRY_MS,
                                }),
                                true,
                            )
                        }
                        Unsent::Refused(detail) => {
                            (Box::new(move || Settled::Refused(detail.clone())), false)
                        }
                        Unsent::Unknown(detail) => {
                            (Box::new(move || Settled::Unknown(detail.clone())), false)
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
        use ritornel_engine::{Due, ServiceKey, ServiceSession};
        use ritornel_lastfm::{Outcome, Sent, SessionKey, ToScrobble, Unsent};

        use super::{Sending, send_due};

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
            send_due(&Sending {
                session: &|| {
                    reads.set(reads.get() + 1);
                    sessions(reads.get() - 1)
                },
                stopping: &|| sent.borrow().len() >= stop_after,
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
                refuse: &|| {},
                now: &|| 0,
            });
            sent.into_inner()
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
/// or every minute, and sends what is due.
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

#[cfg_attr(not(feature = "lastfm"), allow(unused_variables))]
fn scrobble(core: &Core, stop: &AtomicBool) {
    while !stop.load(Ordering::SeqCst) {
        #[cfg(feature = "lastfm")]
        if let Some(lastfm) = core.lastfm() {
            scrobble_once(core, lastfm, stop);
        }
        match core.lastfm() {
            Some(lastfm) => lastfm.wake.wait(IDLE),
            None => return,
        }
    }
}
