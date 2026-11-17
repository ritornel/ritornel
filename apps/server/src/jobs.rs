//! The worker that runs jobs: one thread that claims a queued job under
//! this run's boot ID and carries it a step at a time. See decisions 5 and
//! 23 and "An effect, a job and a lease" in docs/design/server.md.
//!
//! A job never depends on a connected client. The worker holds a database
//! only for the half of a step that writes, so a stop waits for no more
//! than the file in hand, and a job that was stopped is taken up again by
//! the next run from what it had committed.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use ritornel_contract::Id;
use ritornel_engine::{JobState, Progress, ScanRun, ScanSettings};
use tracing::{error, info, warn};

use crate::dispatch::Core;

/// How often a job's counters are saved, and sent to subscribers, while it
/// reads files. Each step's end is saved and sent as well.
const SAVE_EVERY: Duration = Duration::from_secs(1);

/// What wakes the worker: a job was queued, or the server is stopping.
#[derive(Debug, Default)]
pub(crate) struct Wake {
    pending: Mutex<bool>,
    woken: Condvar,
}

impl Wake {
    /// Wakes the worker. A wake that comes while it is busy is kept.
    pub(crate) fn wake(&self) {
        *self.pending.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.woken.notify_all();
    }

    /// Waits until woken.
    fn wait(&self) {
        let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        while !*pending {
            pending = self
                .woken
                .wait(pending)
                .unwrap_or_else(PoisonError::into_inner);
        }
        *pending = false;
    }
}

/// How this run does its jobs.
#[derive(Debug, Clone, Default)]
pub struct Run {
    /// What names this run of the server on every job it claims. A job
    /// claimed under another is one a dead run left.
    pub boot_id: Option<Id>,
    /// How a scan runs.
    pub scan: ScanSettings,
    /// How this run plays.
    pub playback: crate::playback::PlaybackRun,
    /// How this run reaches Last.fm.
    pub lastfm: crate::lastfm::LastfmRun,
    /// Whether and how this run watches the hub's folders.
    pub watch: crate::watch::WatchRun,
}

/// The running worker.
#[derive(Debug)]
pub(crate) struct Worker {
    core: Arc<Core>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Worker {
    /// Starts the worker for the run `boot`.
    pub(crate) fn start(core: Arc<Core>, boot: Id, scan: ScanSettings) -> std::io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (core, stop) = (Arc::clone(&core), Arc::clone(&stop));
            std::thread::Builder::new()
                .name("ritornel-jobs".to_owned())
                .spawn(move || work(&core, boot, scan, &stop))?
        };
        // Whatever the last run left is waiting.
        core.jobs().wake();
        Ok(Self {
            core,
            stop,
            thread: Some(thread),
        })
    }

    /// Stops the worker and waits for it: it ends after the file it is
    /// reading, or the write it is making.
    pub(crate) fn stop(mut self) {
        self.signal();
        if let Some(thread) = self.thread.take() {
            // It has panicked only if a job did, which the log has.
            let _ = thread.join();
        }
    }

    fn signal(&self) {
        self.stop.store(true, Ordering::SeqCst);
        self.core.jobs().wake();
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        // A server that is dropped and was never stopped: the thread is
        // told, and not waited for.
        self.signal();
    }
}

fn work(core: &Core, boot: Id, scan: ScanSettings, stop: &AtomicBool) {
    loop {
        core.jobs().wait();
        while !stop.load(Ordering::SeqCst) {
            let claimed = core.with_stores(|stores| stores.server().claim_job(boot));
            match claimed {
                Some(Ok(Some(job))) => run(core, job, scan, stop),
                Some(Ok(None)) => break,
                Some(Err(failed)) => {
                    error!(error = %failed, "no job could be claimed");
                    break;
                }
                // The databases are closed: the server is stopping.
                None => return,
            }
        }
        if stop.load(Ordering::SeqCst) {
            return;
        }
    }
}

/// Carries one claimed job to its end, or until the server stops.
fn run(core: &Core, job: ritornel_engine::Job, settings: ScanSettings, stop: &AtomicBool) {
    if job.kind == crate::lastfm::SYNC_JOB {
        return sync(core, job, stop);
    }
    scan(core, job, settings, stop);
}

/// Carries a claimed sync of a Last.fm history on.
#[cfg(feature = "lastfm")]
fn sync(core: &Core, job: ritornel_engine::Job, stop: &AtomicBool) {
    crate::lastfm::run_sync(core, job, stop);
}

/// A sync of a Last.fm history, on a server built without Last.fm: it
/// fails, and says why.
#[cfg(not(feature = "lastfm"))]
fn sync(core: &Core, job: ritornel_engine::Job, _stop: &AtomicBool) {
    let ended = core.with_stores(|stores| {
        stores.server().finish_job(
            job.id,
            JobState::Failed,
            Some("this server was built without Last.fm".to_owned()),
        )
    });
    if let Some(Ok(job)) = ended {
        core.publish_job(&job);
    }
}

/// Carries one claimed scan to its end, or until the server stops.
fn scan(core: &Core, job: ritornel_engine::Job, settings: ScanSettings, stop: &AtomicBool) {
    let (row, job_id) = (job.id, job.public_id);
    info!(job_id = %job_id, kind = %job.kind, step = job.step, "a job is claimed");
    let finish = |state: JobState, failure: Option<String>| {
        let finished = core.with_stores(|stores| stores.server().finish_job(row, state, failure));
        match finished {
            Some(Ok(job)) => {
                info!(job_id = %job_id, state = state.as_str(), "a job ended");
                core.publish_job(&job);
                // Changes that waited for this scan to end may go now.
                core.wake_watch();
            }
            Some(Err(failed)) => {
                error!(job_id = %job_id, error = %failed, "a job's end could not be saved")
            }
            None => {}
        }
    };
    let opened = core.with_library(|server, library| ScanRun::open(job, server, library, settings));
    let mut scan = match opened {
        Some(Ok(scan)) => scan,
        Some(Err(failed)) => return finish(JobState::Failed, Some(failed.to_string())),
        None => return,
    };
    let mut saved = Instant::now();
    loop {
        if stop.load(Ordering::SeqCst) {
            // The job stays claimed by this run, and the next run takes it
            // back from what it has committed.
            return;
        }
        // Reading files: no database is held. The counters are saved, and
        // sent to subscribers, about once a second.
        let mut tick = |progress: Progress| {
            let now = Instant::now();
            if now.duration_since(saved) >= SAVE_EVERY {
                saved = now;
                if let Some(Ok(job)) =
                    core.with_stores(|stores| stores.server().save_progress(row, progress))
                {
                    core.publish_job(&job);
                }
            }
        };
        match scan.prepare(stop, &mut tick) {
            Ok(true) => {}
            Ok(false) => return,
            Err(failed) => return finish(JobState::Failed, Some(failed.to_string())),
        }
        // What the step wrote reaches subscribers from the library itself,
        // after each commit (`dispatch::lists::hook`).
        let step = core.with_library(|server, library| {
            let finished = scan.commit(library)?;
            server.add_job_items(row, scan.take_items())?;
            Ok::<_, ritornel_engine::Error>((finished, server.save_progress(row, scan.progress())?))
        });
        let (finished, job) = match step {
            Some(Ok(step)) => step,
            Some(Err(ritornel_engine::Error::FailPoint)) => {
                // A crash test asked for the server to die here, between a
                // step's commit and its checkpoint.
                warn!(job_id = %job_id, "stopping at a fail point, as a crash would");
                std::process::abort();
            }
            Some(Err(failed)) => return finish(JobState::Failed, Some(failed.to_string())),
            None => return,
        };
        core.publish_job(&job);
        saved = Instant::now();
        if finished.is_some() {
            return finish(JobState::Succeeded, None);
        }
        if job.cancel_requested {
            // Committed steps stay.
            return finish(JobState::Cancelled, None);
        }
    }
}
