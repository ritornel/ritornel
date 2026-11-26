//! The player's threads: the control loop, which ticks the player a few
//! times a second; the effect worker, which delivers finished listens from
//! the outbox to history; and on a satellite, the puller, which pulls the
//! masters the player waits for. See "Audio pipeline" and "Recording a
//! listen" in docs/internals/playback.md, and "Jobs and outbox" in
//! docs/internals/server.md.
//!
//! Neither depends on a connected client: closing every window changes
//! nothing for either.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use ritornel_engine::playback::{
    CHECKPOINT_EVERY_MS, CLOSE_OUTPUT_AFTER_MS, OnDemand, Output, Player, PlayerSetup,
    deliver_listens,
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
        close_output_after_ms: CLOSE_OUTPUT_AFTER_MS,
        fail_at_position_ms: run.fail_at_position_ms,
        fail_saves_after_ms: run.fail_saves_after_ms,
    };
    let (player, crashed) = Player::restore(
        setup,
        Box::new(renderer),
        server,
        library.map(|library| library as &dyn ritornel_engine::playback::Catalog),
    )
    .map_err(crate::Error::Store)?;
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

/// Delivers due staged listens from `server.db`'s outbox into `library`
/// synchronously during startup. Used when starting on a restored library so
/// crashed or queued local listens are already in history before a Last.fm
/// import can release the restore hold.
pub(crate) fn deliver_startup_outbox(
    server: &ServerStore,
    library: &LibraryStore,
    clock: &dyn Clock,
    run_id: Id,
) {
    let stamps = || stamps(clock, run_id);
    loop {
        match deliver_listens(server, library, clock, &stamps, &|| Ok(())) {
            Ok(done) => {
                if !done.delivered.is_empty() {
                    info!(
                        delivered = done.delivered.len(),
                        duplicates = done.duplicates,
                        "staged listens delivered to history before restore reconciliation"
                    );
                }
                for (listen, problem) in &done.failed {
                    warn!(
                        listen = %listen,
                        error = %problem,
                        "staged listen could not be delivered before restore reconciliation"
                    );
                }
                if done.delivered.is_empty()
                    || done.next_due_ms.is_none_or(|due| due > clock.now_ms())
                {
                    break;
                }
            }
            Err(failed) => {
                warn!(
                    error = %failed,
                    "staged listens could not be delivered before restore reconciliation"
                );
                break;
            }
        }
    }
}

/// How long the thread that pulls masters waits when nothing wakes it.
/// Every call and tick that leaves the player waiting wakes it, so this is
/// only a fallback.
const PULL_IDLE: Duration = Duration::from_secs(30);

/// The running threads. Dropping them tells them to stop.
#[derive(Debug)]
pub(crate) struct Workers {
    core: Arc<Core>,
    /// This run of the server, the boot ID of last resort.
    run_id: Id,
    stop: Arc<AtomicBool>,
    /// Ends a pull under way, which `stop` cannot reach while the pull
    /// waits on the network.
    cancel_pull: tokio::sync::watch::Sender<bool>,
    ticker: Option<JoinHandle<()>>,
    puller: Option<JoinHandle<()>>,
    deliverer: Option<JoinHandle<()>>,
}

impl Workers {
    /// Starts the control loop and the puller, if there is a player, and
    /// the effect worker.
    pub(crate) fn start(core: Arc<Core>, run: PlaybackRun, run_id: Id) -> std::io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let cancel_pull = tokio::sync::watch::Sender::new(false);
        let (ticker, puller) = if core.has_player() {
            let (pulling, stop_pulling) = (Arc::clone(&core), Arc::clone(&stop));
            let cancel = cancel_pull.subscribe();
            let puller = std::thread::Builder::new()
                .name("ritornel-pull".to_owned())
                .spawn(move || pull(&pulling, &stop_pulling, &cancel))?;
            let (core, stop) = (Arc::clone(&core), Arc::clone(&stop));
            let ticker = std::thread::Builder::new()
                .name("ritornel-player".to_owned())
                .spawn(move || tick(&core, &stop))?;
            (Some(ticker), Some(puller))
        } else {
            (None, None)
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
            cancel_pull,
            ticker,
            puller,
            deliverer: Some(deliverer),
        })
    }

    /// Stops the threads and waits for them, then shuts the player down
    /// cleanly and delivers what that closed. A pull under way is ended,
    /// and its partial file removed, before this returns, so nothing of
    /// the server writes to its data directory afterwards.
    pub(crate) fn stop(mut self) {
        self.signal();
        for thread in [
            self.ticker.take(),
            self.puller.take(),
            self.deliverer.take(),
        ]
        .into_iter()
        .flatten()
        {
            // It has panicked only if a step did, which the log has.
            let _ = thread.join();
        }
        self.core.shutdown_player();
        let (clock, run_id) = (ritornel_engine::SystemClock, self.run_id);
        let delivered = deliver_now(&self.core, &clock, run_id, &|| Ok(()));
        if let Some(Err(failed)) = delivered {
            warn!(error = %failed, "listens could not be delivered before the stop; the next run will");
        }
    }

    fn signal(&self) {
        self.stop.store(true, Ordering::SeqCst);
        self.cancel_pull.send_replace(true);
        self.core.deliveries().wake();
        self.core.pulls().wake();
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

/// Delivers the listens whose turn has come: into the library, on a hub,
/// or to the hub, on a satellite. `None` on a server with neither.
fn deliver_now(
    core: &Core,
    clock: &dyn Clock,
    run_id: Id,
    after_ingest: &dyn Fn() -> Result<(), EngineError>,
) -> Option<Result<ritornel_engine::playback::Delivered, EngineError>> {
    let stamps = || stamps(clock, run_id);
    core.with_library(|server, library| {
        deliver_listens(server, library, clock, &stamps, after_ingest)
    })
    .or_else(|| core.deliver_to_hub(clock, &stamps, after_ingest))
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

/// Pulls what the player waits for, one master at a time, with the player
/// let go meanwhile, and hands each back to it. `cancel` ends a pull under
/// way.
fn pull(core: &Core, stop: &AtomicBool, cancel: &tokio::sync::watch::Receiver<bool>) {
    while !stop.load(Ordering::SeqCst) {
        match core.wanted_master() {
            Some((fetch, hub)) => {
                let outcome = hub
                    .fetch(fetch.appearance, cancel)
                    .map_err(|error| error.to_string());
                if let Err(why) = &outcome {
                    warn!(appearance = %fetch.appearance, %why, "a master could not be pulled from the hub");
                }
                if !stop.load(Ordering::SeqCst) {
                    core.master_fetched(fetch, outcome);
                }
            }
            None => core.pulls().wait(PULL_IDLE),
        }
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
        let done = deliver_now(core, &clock, run_id, &after_ingest);
        let wait = match done {
            Some(Ok(done)) => {
                if !done.delivered.is_empty() {
                    // A play that counts may now be scrobbled.
                    core.wake_scrobbler();
                    info!(
                        delivered = done.delivered.len(),
                        sent_again = done.duplicates,
                        "listens delivered"
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
            // No library and no hub: nothing can be delivered until there
            // is one.
            None => IDLE,
        };
        core.deliveries().wait(wait);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use ritornel_engine::{Id, LibraryStore, OsEntropy, ServerStore, WriteRequest};

    use super::deliver_startup_outbox;
    use crate::testing::{Scratch, SetClock};

    #[test]
    fn deliver_startup_outbox_drains_due_local_outbox_rows_synchronously() {
        let t0 = 1_794_776_400_000_i64;
        let scratch = Scratch::new();
        let server_path = scratch.path().join("server.db");
        let library_path = scratch.path().join("library.db");
        let clock = Arc::new(SetClock::at(t0));
        let entropy = Arc::new(OsEntropy);
        let server = ServerStore::open(&server_path, clock.clone(), entropy.clone()).unwrap();
        let (identity, _) = server.ensure_identity("desk").unwrap();
        let (library, _) = LibraryStore::create(
            &library_path,
            &identity,
            WriteRequest {
                method: "library.create",
                command: None,
                client: None,
            },
            |_| String::new(),
            clock.clone(),
            entropy.clone(),
        )
        .unwrap();
        let listener = library.default_listener().unwrap().unwrap();
        let account = library.ensure_service_account("lastfm", "someone").unwrap();

        // Seed one track in `library.db` so the staged listen resolves cleanly.
        let app_id = Id::from_bytes([0x11; 16]);
        let asset_id = Id::from_bytes([0x12; 16]);
        let item_id = Id::from_bytes([0x13; 16]);
        let listen_id = Id::from_bytes([0x14; 16]);
        let old_boot = Id::from_bytes([0x15; 16]);
        drop(library);
        {
            let lib = rusqlite::Connection::open(&library_path).unwrap();
            lib.execute(
                "INSERT INTO library_root (public_id, server_id, name, kind, created_at_ms, rev)
                 VALUES (?1, (SELECT id FROM server WHERE public_id = ?2), 'music', 'master', 0, 1)",
                (Id::from_bytes([0x21; 16]).as_bytes(), identity.server_id.as_bytes()),
            )
            .unwrap();
            let root = lib.last_insert_rowid();
            lib.execute(
                "INSERT INTO source_instance (public_id, kind, name, root_id, created_at_ms, rev)
                 VALUES (?1, 'folder', 'music', ?2, 0, 1)",
                (Id::from_bytes([0x22; 16]).as_bytes(), root),
            )
            .unwrap();
            let source = lib.last_insert_rowid();
            lib.execute(
                "INSERT INTO artist_credit (display_text, rev) VALUES ('Moss Arcade', 1)",
                [],
            )
            .unwrap();
            let credit = lib.last_insert_rowid();
            lib.execute(
                "INSERT INTO release_group (public_id, rev) VALUES (?1, 1)",
                [Id::from_bytes([0x23; 16]).as_bytes()],
            )
            .unwrap();
            let group = lib.last_insert_rowid();
            lib.execute(
                "INSERT INTO release (id, public_id, release_group_id, canonical_id, display_title, rev)
                 VALUES (1, ?1, ?2, 1, 'Tin Weather', 1)",
                (Id::from_bytes([0x24; 16]).as_bytes(), group),
            )
            .unwrap();
            lib.execute(
                "INSERT INTO recording (id, public_id, canonical_id, display_title, display_artist_credit_id, rev)
                 VALUES (1, ?1, 1, 'Paper Kites', ?2, 1)",
                (Id::from_bytes([0x25; 16]).as_bytes(), credit),
            )
            .unwrap();
            lib.execute(
                "INSERT INTO track_appearance
                   (id, public_id, recording_id, release_id, canonical_id, display_title,
                    display_artist_credit_id, display_disc_number, display_track_number,
                    display_duration_ms, rev)
                 VALUES (1, ?1, 1, 1, 1, 'Paper Kites', ?2, 1, 1, 240000, 1)",
                (app_id.as_bytes(), credit),
            )
            .unwrap();
            lib.execute(
                "INSERT INTO audio_asset
                   (public_id, hash_scheme, audio_hash, codec, sample_rate_hz, channel_layout,
                    bit_depth, lossless, duration_ms, rev)
                 VALUES (?1, 1, ?2, 'flac', 44100, 'stereo', 16, 1, 240000, 1)",
                (asset_id.as_bytes(), Id::from_bytes([0x26; 16]).as_bytes()),
            )
            .unwrap();
            let asset = lib.last_insert_rowid();
            lib.execute(
                "INSERT INTO source_item
                   (public_id, source_instance_id, item_kind, provider_key, size_bytes, mtime_ms,
                    first_seen_at_ms, rev)
                 VALUES (?1, ?2, 'file', '01.flac', 1000, 0, 0, 1)",
                (item_id.as_bytes(), source),
            )
            .unwrap();
            let item = lib.last_insert_rowid();
            lib.execute(
                "INSERT INTO source_item_revision
                   (source_item_id, revision_no, payload_hash, payload_json, audio_asset_id,
                    observed_at_ms, rev)
                 VALUES (?1, 1, ?2, '{}', ?3, 0, 1)",
                (item, Id::from_bytes([0x27; 16]).as_bytes(), asset),
            )
            .unwrap();
            let revision = lib.last_insert_rowid();
            lib.execute(
                "UPDATE source_item SET current_revision_id = ?2, current_recording_id = 1
                 WHERE id = ?1",
                (item, revision),
            )
            .unwrap();
            lib.execute(
                "INSERT INTO identity_decision
                   (action, source_item_id, method, resolver_version, normalizer_version,
                    evidence_json, actor_kind, decided_at_ms, rev)
                 VALUES ('bind', ?1, 'new', 1, 1, '{}', 'resolver', 0, 1)",
                [item],
            )
            .unwrap();
            let decision = lib.last_insert_rowid();
            lib.execute(
                "INSERT INTO assignment
                   (source_item_id, recording_id, appearance_id, decision_id, effective_from_ms, rev)
                 VALUES (?1, 1, 1, ?2, 0, 1)",
                (item, decision),
            )
            .unwrap();
        }
        // Mark the library copy as backed up (creates per-account hold).
        assert!(ritornel_engine::backup::hold_sending(&library_path, t0).unwrap());

        // Stage an open listen in `server.db`, as a backup taken mid-playback holds.
        drop(server);
        {
            let srv = rusqlite::Connection::open(&server_path).unwrap();
            srv.execute(
                "INSERT INTO listen_staging (
                   listen_id, queue_entry_pid, listener_pid, played_on_server_pid,
                   appearance_pid, asset_pid, copy_kind, source_item_pid,
                   source_revision_no, started_at_utc_ms, zone_name, started_mono_ms,
                   boot_id, start_reason, continues_listen_id, start_position_ms,
                   track_duration_ms, ms_played, heard_ranges_json, max_position_ms,
                   context_kind, shuffle, checkpoint_position_ms, checkpoint_at_ms,
                   checkpoint_mono_ms, end_position_ms, end_reason, ended_at_utc_ms,
                   ended_mono_ms)
                 VALUES (?1, NULL, ?2, ?3, ?4, ?5, 'master', ?6, 1, ?7, 'America/New_York',
                   1000, ?8, 'user_pick', NULL, 0, 240000, 180000, '[[0,180000]]', 180000,
                   'album', 0, 180000, ?9, NULL, NULL, NULL, NULL, NULL)",
                rusqlite::params![
                    listen_id.as_bytes(),
                    listener.as_bytes(),
                    identity.server_id.as_bytes(),
                    app_id.as_bytes(),
                    asset_id.as_bytes(),
                    item_id.as_bytes(),
                    t0 - 180_000,
                    old_boot.as_bytes(),
                    t0,
                ],
            )
            .unwrap();
        }

        // Reopen both stores as startup does: close crashed listens and drain
        // synchronously before workers start.
        clock.set(t0 + 60_000);
        let server = ServerStore::open(&server_path, clock.clone(), entropy.clone()).unwrap();
        let library =
            LibraryStore::open(&library_path, identity.server_id, clock.clone(), entropy).unwrap();
        assert!(library.opened_restored(Some(account)).unwrap().is_some());
        assert_eq!(server.close_crashed_listens().unwrap(), [listen_id]);
        assert_eq!(server.undelivered_listens().unwrap(), 1);

        deliver_startup_outbox(&server, &library, &*clock, Id::from_bytes([0x99; 16]));

        assert_eq!(server.undelivered_listens().unwrap(), 0);
        let recent = library.recent_plays(listener, 10, None).unwrap();
        assert_eq!(recent.len(), 1);
    }
}
