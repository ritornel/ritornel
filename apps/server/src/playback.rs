//! The player's two threads: the control loop, which ticks the player a
//! few times a second, and the effect worker, which delivers finished
//! listens from the outbox to history. See "The pipeline" and decision 21
//! in docs/design/playback.md, and "An effect, a job and a lease" in
//! docs/design/server.md.
//!
//! Neither depends on a connected client: closing every window changes
//! nothing for either.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use ritornel_engine::playback::{
    CHECKPOINT_EVERY_MS, OnDemand, Output, Player, PlayerSetup, deliver_listens,
};
use ritornel_engine::{
    Clock, Entropy, Error as EngineError, Id, LibraryStore, SendStamps, ServerStore,
    SystemTimekeeper, Timekeeper, clock_boot,
};
use tracing::{debug, error, info, warn};

use crate::dispatch::Core;

/// How often the control loop reads what the output has played.
const TICK: Duration = Duration::from_millis(50);

/// The longest the effect worker sleeps with nothing due.
const IDLE: Duration = Duration::from_secs(60);

/// How this run plays, with what the crash tests ask of it.
#[derive(Debug, Clone, Copy, Default)]
pub struct PlaybackRun {
    /// The output, or `None` for a server with no player.
    pub output: Option<Output>,
    /// For the crash test alone: the server aborts once the playing track
    /// passes this position, without saving anything.
    pub fail_at_position_ms: Option<i64>,
    /// For the record's tests alone: once the playing track passes this
    /// position, saving its listen fails, and playback goes on.
    pub fail_saves_after_ms: Option<i64>,
    /// For the crash test alone: the server aborts once the library has
    /// taken a listen in and before `server.db` lets it go.
    pub fail_after_ingest: bool,
    /// Whether the player is shown in the system's media session, where
    /// the build has one. Only `serve` asks for it.
    pub media_session: bool,
}

/// What wakes a worker, with a deadline.
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

    /// Waits until woken, or for `at_most`.
    pub(crate) fn wait(&self, at_most: Duration) {
        let pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        let (mut pending, _) = self
            .woken
            .wait_timeout_while(pending, at_most, |pending| !*pending)
            .unwrap_or_else(PoisonError::into_inner);
        *pending = false;
    }
}

/// Makes the player for this run, as the last run left it, if this run has
/// an output. Listens a dead run left open are closed as crashes either
/// way, so that they are delivered.
pub(crate) fn restore(
    server: &ServerStore,
    library: Option<&LibraryStore>,
    run: PlaybackRun,
    server_id: Id,
    run_id: Id,
    clock: &Arc<dyn Clock>,
    entropy: &Arc<dyn Entropy>,
) -> Result<Option<Player>, crate::Error> {
    let Some(output) = run.output else {
        let crashed = server
            .close_crashed_listens()
            .map_err(crate::Error::Store)?;
        if !crashed.is_empty() {
            info!(
                crashed = crashed.len(),
                "listens an earlier run left open are closed"
            );
        }
        return Ok(None);
    };
    // The output is opened when the first track is, so a server restored
    // paused opens no device until it is asked to play.
    let renderer = OnDemand::new(output);
    match SystemTimekeeper.boot() {
        Some(boot) => debug!(%boot, "listens are stamped with the machine's boot"),
        None => warn!(
            "the machine's boot ID cannot be read: listens are stamped with this run's, and one \
             closed by a restart is dated by the wall clock"
        ),
    }
    let setup = PlayerSetup {
        server_id,
        run: run_id,
        clock: Arc::clone(clock),
        timekeeper: Arc::new(SystemTimekeeper),
        entropy: Arc::clone(entropy),
        checkpoint_every_ms: CHECKPOINT_EVERY_MS,
        fail_at_position_ms: run.fail_at_position_ms,
        fail_saves_after_ms: run.fail_saves_after_ms,
    };
    let (player, crashed) =
        Player::restore(setup, Box::new(renderer), server, library).map_err(crate::Error::Store)?;
    if !crashed.is_empty() {
        info!(
            crashed = crashed.len(),
            "listens an earlier run left open are closed as crashes"
        );
    }
    let state = player.state();
    // Every start of a server with an output says this, so it is not news.
    debug!(
        output = %state.output.0,
        rate = state.output.1,
        status = state.status.as_str(),
        position_ms = state.position_ms,
        queue = state.queue_length,
        "the player is restored"
    );
    Ok(Some(player))
}

/// The two running threads. Dropping them tells them to stop.
#[derive(Debug)]
pub(crate) struct Workers {
    core: Arc<Core>,
    /// This run of the server, the boot ID of last resort.
    run_id: Id,
    stop: Arc<AtomicBool>,
    ticker: Option<JoinHandle<()>>,
    deliverer: Option<JoinHandle<()>>,
}

impl Workers {
    /// Starts the control loop, if there is a player, and the effect
    /// worker.
    pub(crate) fn start(core: Arc<Core>, run: PlaybackRun, run_id: Id) -> std::io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let ticker = if core.has_player() {
            let (core, stop) = (Arc::clone(&core), Arc::clone(&stop));
            Some(
                std::thread::Builder::new()
                    .name("ritornel-player".to_owned())
                    .spawn(move || tick(&core, &stop))?,
            )
        } else {
            None
        };
        let deliverer = {
            let (core, stop) = (Arc::clone(&core), Arc::clone(&stop));
            std::thread::Builder::new()
                .name("ritornel-outbox".to_owned())
                .spawn(move || deliver(&core, &stop, run_id, run.fail_after_ingest))?
        };
        // Whatever the last run left is waiting.
        core.deliveries().wake();
        Ok(Self {
            core,
            run_id,
            stop,
            ticker,
            deliverer: Some(deliverer),
        })
    }

    /// Stops both threads and waits for them, then shuts the player down
    /// cleanly and delivers what that closed.
    pub(crate) fn stop(mut self) {
        self.signal();
        for thread in [self.ticker.take(), self.deliverer.take()]
            .into_iter()
            .flatten()
        {
            // It has panicked only if a step did, which the log has.
            let _ = thread.join();
        }
        self.core.shutdown_player();
        let (clock, run_id) = (ritornel_engine::SystemClock, self.run_id);
        let delivered = self.core.with_library(|server, library| {
            deliver_listens(server, library, &clock, &|| stamps(&clock, run_id), &|| {
                Ok(())
            })
        });
        if let Some(Err(failed)) = delivered {
            warn!(error = %failed, "listens could not be delivered before the stop; the next run will");
        }
    }

    fn signal(&self) {
        self.stop.store(true, Ordering::SeqCst);
        self.core.deliveries().wake();
    }
}

impl Drop for Workers {
    fn drop(&mut self) {
        self.signal();
    }
}

/// The stamps of one send, taken afresh: the machine's boot with its
/// monotonic clock, or this run where the machine's boot cannot be read.
fn stamps(clock: &dyn Clock, run_id: Id) -> SendStamps {
    SendStamps {
        sent_at_ms: clock.now_ms(),
        sent_mono_ms: SystemTimekeeper.mono_ms(),
        sent_boot: clock_boot(&SystemTimekeeper, run_id),
    }
}

fn tick(core: &Core, stop: &AtomicBool) {
    while !stop.load(Ordering::SeqCst) {
        match core.tick_player() {
            Ok(()) => {}
            Err(EngineError::FailPoint) => {
                // The crash test asked for the server to die here, with the
                // listen open and its last checkpoint saved.
                warn!("stopping at a fail point, as a crash would");
                std::process::abort();
            }
            Err(failed) => error!(error = %failed, "the player's tick failed"),
        }
        std::thread::sleep(TICK);
    }
}

fn deliver(core: &Core, stop: &AtomicBool, run_id: Id, fail_after_ingest: bool) {
    let clock = ritornel_engine::SystemClock;
    while !stop.load(Ordering::SeqCst) {
        let after_ingest = || {
            if fail_after_ingest {
                warn!("stopping at a fail point, as a crash would");
                std::process::abort();
            }
            Ok(())
        };
        let done = core.with_library(|server, library| {
            deliver_listens(
                server,
                library,
                &clock,
                &|| stamps(&clock, run_id),
                &after_ingest,
            )
        });
        let wait = match done {
            Some(Ok(done)) => {
                if !done.delivered.is_empty() {
                    // A play that counts may now be scrobbled.
                    core.wake_scrobbler();
                    info!(
                        delivered = done.delivered.len(),
                        sent_again = done.duplicates,
                        "listens delivered to history"
                    );
                }
                for (listen, reason) in &done.failed {
                    warn!(listen_id = %listen, %reason, "a listen could not be delivered; it is tried again");
                }
                done.next_due_ms.map_or(IDLE, |due| {
                    let wait = due.saturating_sub(clock.now_ms()).max(0);
                    Duration::from_millis(u64::try_from(wait).unwrap_or(0)).min(IDLE)
                })
            }
            Some(Err(failed)) => {
                error!(error = %failed, "delivering listens failed");
                IDLE
            }
            // No library: nothing can be delivered until there is one.
            None => IDLE,
        };
        core.deliveries().wait(wait);
    }
}
