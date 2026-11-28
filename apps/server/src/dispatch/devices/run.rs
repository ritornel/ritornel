//! The job that carries a sync out. See "The run" in
//! docs/internals/device-sync.md.
//!
//! A run reads the target, freezes the plan the person saw, and then
//! copies: removes, replaces that shrink, replaces that grow, then adds, a
//! window at a time. A window is the few actions whose transfers are
//! prepared. In one `library.db` transaction the hub records the last
//! window's results, baselines among them, and writes the next window down
//! as issued; only then is the device asked. So what a crash leaves in
//! doubt is one window of actions, each with the digest it meant to leave.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use ritornel_contract::{
    Id, SyncActionKind, SyncActionState, SyncDeviceChange, SyncOutcome, SyncStartParams, SyncStep,
    SyncWhenFull,
};
use ritornel_device::{ErrorKind, ObjectId, Put, Session, Signature};
use ritornel_engine::sync::{
    self, Action, Issued, Left, Marker, Outcome, Plan, Recorded, RunRow, Sent, TargetRow,
};
use ritornel_engine::transcode::{
    Cover, PreparedTransfer, Tagging, TransferAudio, TransferRequest,
};
use ritornel_engine::{JobCancel, JobState, JobStop, Progress};
use tracing::{info, warn};

use super::{Core, LiveRun, working};

/// How many transfers are prepared ahead of the writer: what bounds the
/// scratch files a sync holds. A constant, and not a setting.
const WINDOW: usize = 4;

/// How often a run under way is sent to its followers, at most.
const TELL_EVERY: Duration = Duration::from_millis(250);

/// How a run ended, and why where it did not finish.
struct Ended {
    outcome: SyncOutcome,
    error: Option<String>,
}

fn ended(outcome: SyncOutcome, error: impl Into<String>) -> Ended {
    Ended {
        outcome,
        error: Some(error.into()),
    }
}

/// What a failed verb says of the run as a whole: a device that is gone
/// cuts it off.
fn gone(error: &ritornel_device::Error) -> bool {
    error.kind == ErrorKind::Gone
}

struct Running<'a> {
    core: &'a Core,
    target: TargetRow,
    run: RunRow,
    job: i64,
    cancel: JobCancel,
    told: Instant,
    saved: Instant,
}

impl Running<'_> {
    fn live(&self, change: impl FnOnce(&mut LiveRun)) {
        if let Some(run) = super::lock(&self.core.devices.live)
            .entry(self.target.id)
            .or_default()
            .run
            .as_mut()
        {
            change(run);
        }
    }

    /// Tells the target's followers, at once or at most every
    /// [`TELL_EVERY`].
    fn tell(&mut self, now: bool, changed: &[SyncDeviceChange]) {
        if now || self.told.elapsed() >= TELL_EVERY {
            self.told = Instant::now();
            self.core.device_changed(self.target.id, changed);
        }
        if self.saved.elapsed() >= crate::jobs::SAVE_EVERY {
            self.saved = Instant::now();
            let progress = super::lock(&self.core.devices.live)
                .get(&self.target.id)
                .and_then(|live| live.run.as_ref())
                .map(|run| Progress {
                    step: 1,
                    done: run.actions_done as i64,
                    total: Some(run.actions_total as i64),
                    bytes: i64::try_from(run.bytes_settled).unwrap_or(i64::MAX),
                    failed: run.failed as i64,
                    ..Default::default()
                });
            if let Some(progress) = progress
                && let Some(Ok(job)) = self
                    .core
                    .with_stores(|stores| stores.server().save_progress(self.job, progress))
            {
                self.core.publish_job(&job);
            }
        }
    }

    fn step(&mut self, step: SyncStep) {
        let run = self.run.clone();
        let _ = self
            .core
            .with_library(|_, library| library.sync_step(&run, step));
        self.live(|live| live.step = step);
        self.tell(true, &[SyncDeviceChange::State]);
    }

    fn stopped(&self) -> Option<JobStop> {
        self.cancel.stopped()
    }
}

/// One action whose transfer is being made ready, or is.
struct Pending {
    seq: usize,
    prepared: tokio::task::JoinHandle<Result<PreparedTransfer, String>>,
}

impl Core {
    /// Carries one claimed sync to its end, or until the server stops.
    pub(crate) fn run_device_sync(&self, job: ritornel_engine::Job, shutdown: &Arc<AtomicBool>) {
        let finish = |state: JobState, error: Option<String>| {
            if let Some(Ok(job)) =
                self.with_stores(|stores| stores.server().finish_job(job.id, state, error))
            {
                self.publish_job(&job);
            }
        };
        let Some(cancel) = crate::jobs::cancel_handle(self, job.id, shutdown) else {
            return;
        };
        let cancel = match cancel {
            Ok(cancel) => cancel,
            Err(error) => return finish(JobState::Failed, Some(error.to_string())),
        };
        // A job put back after a restart finds its run closed: the session
        // it held is gone with the boot that held it.
        if job.step > 0 {
            return finish(
                JobState::Failed,
                Some("The server stopped while this sync was running.".to_owned()),
            );
        }
        let params: SyncStartParams = match serde_json::from_str(&job.params_json) {
            Ok(params) => params,
            Err(error) => return finish(JobState::Failed, Some(error.to_string())),
        };
        let opened = self.with_library(|_, library| {
            let target = library
                .sync_target(params.device_id)?
                .ok_or(ritornel_engine::Error::NotFound("no such device"))?;
            let run =
                library.sync_start_run(&target, job.public_id, params.plan_revision.clone())?;
            Ok::<_, ritornel_engine::Error>((target, run))
        });
        let (target, run) = match opened {
            Some(Ok(opened)) => opened,
            Some(Err(error)) => return finish(JobState::Failed, Some(error.to_string())),
            None => return,
        };
        let _ = self.with_stores(|stores| {
            stores.server().save_progress(
                job.id,
                Progress {
                    step: 1,
                    ..Default::default()
                },
            )
        });
        info!(device = %target.id, run = %run.id, "a sync starts");
        super::lock(&self.devices.live)
            .entry(target.id)
            .or_default()
            .run = Some(LiveRun {
            run: run.clone(),
            step: SyncStep::Read,
            adopting: params.adopt,
            stopping: false,
            actions_total: 0,
            actions_done: 0,
            failed: 0,
            bytes_total: 0,
            bytes_settled: 0,
            current: Default::default(),
            copying_since: None,
            written: 0,
        });
        let mut running = Running {
            core: self,
            target,
            run,
            job: job.id,
            cancel,
            told: Instant::now(),
            saved: Instant::now(),
        };
        running.tell(true, &[SyncDeviceChange::State]);
        let (end, write_bps) = self.carry(&mut running, &params);
        let Running { target, run, .. } = running;
        let closed = self.with_library(|_, library| {
            library.sync_finish_run(&target, &run, end.outcome, end.error.clone(), write_bps)
        });
        if let Some(Err(error)) = closed {
            warn!(%error, "a sync's end could not be recorded");
        }
        info!(device = %target.id, run = %run.id, outcome = ?end.outcome, "a sync ends");
        if let Some(live) = super::lock(&self.devices.live).get_mut(&target.id) {
            live.run = None;
            live.planned = None;
        }
        self.device_changed(
            target.id,
            &[
                SyncDeviceChange::State,
                SyncDeviceChange::Plan,
                SyncDeviceChange::Actions,
            ],
        );
        match end.outcome {
            SyncOutcome::Complete | SyncOutcome::FailedInPart => finish(JobState::Succeeded, None),
            SyncOutcome::Stopped => finish(JobState::Cancelled, None),
            SyncOutcome::CutOff | SyncOutcome::Failed => finish(JobState::Failed, end.error),
        }
    }

    /// The run itself: read, plan, adopt, copy and finish. Answers how it
    /// ended, and the write speed it measured.
    fn carry(&self, running: &mut Running<'_>, params: &SyncStartParams) -> (Ended, Option<u64>) {
        let target = running.target.clone();
        // Read: nothing below starts until this returns.
        let opened = self.with_stores(|stores| self.open_device(stores, &target));
        let mut session = match opened {
            Some(Ok(session)) => session,
            _ => return (ended(SyncOutcome::Failed, "The device is not here."), None),
        };
        if let Err(error) = session.clear_partials() {
            return (ended(SyncOutcome::Failed, error.detail), None);
        }
        // What the last run left in doubt is checked before a new plan.
        if let Err(error) = self.settle(&target, session.as_mut()) {
            return (ended(SyncOutcome::Failed, error), None);
        }
        running.step(SyncStep::Plan);
        let planned = self.with_stores(|stores| {
            Self::library(stores, |library| {
                // The target as it is now: its settings may have moved.
                let target = Self::target(library, target.id)?;
                let (planned, _) = self.planned(stores, library, &target, super::Use::Read)?;
                Ok((target, planned))
            })
        });
        let (target, planned) = match planned {
            Some(Ok((target, Some(planned)))) => (target, planned),
            Some(Err(error)) => return (ended(SyncOutcome::Failed, error.message), None),
            _ => {
                return (
                    ended(SyncOutcome::Failed, "The device could not be read."),
                    None,
                );
            }
        };
        running.target = target.clone();
        let plan = &planned.plan;
        if plan.revision != params.plan_revision {
            return (
                ended(
                    SyncOutcome::Failed,
                    "The plan changed before the sync began. Nothing was written.",
                ),
                None,
            );
        }
        if !plan.fits && target.settings.when_full == SyncWhenFull::Ask && !params.accept_cut {
            return (
                ended(
                    SyncOutcome::Failed,
                    "Not everything fits, and nothing was agreed.",
                ),
                None,
            );
        }
        let adopting = !planned.adopted;
        if adopting && !params.adopt {
            return (
                ended(SyncOutcome::Failed, "The device is not set up."),
                None,
            );
        }
        // Retags left for another day, by their place in the plan.
        let skip: Vec<usize> = plan
            .actions
            .iter()
            .enumerate()
            .filter(|(_, action)| params.skip_retags && action.retag)
            .map(|(at, _)| at)
            .collect();
        let marker_root = self.entropy_id();
        let frozen = self.with_library(|_, library| {
            library.sync_freeze(
                &target,
                &running.run,
                plan,
                adopting.then_some(marker_root),
                skip.clone(),
                planned.library_rev,
            )
        });
        running.run = match frozen {
            Some(Ok(run)) => run,
            Some(Err(error)) => return (ended(SyncOutcome::Failed, error.to_string()), None),
            None => return (ended(SyncOutcome::CutOff, "The server stopped."), None),
        };
        let (total, bytes) = (running.run.actions_total, running.run.bytes_total);
        let run = running.run.clone();
        running.live(|live| {
            live.run = run;
            live.adopting = adopting;
            live.actions_total = total;
            live.bytes_total = bytes;
        });
        // Adopt: the marker that makes this storage Ritornel's.
        let mut marker = Marker {
            v: 1,
            target: target.id,
            epoch: running.run.epoch,
            root: if adopting {
                marker_root
            } else {
                target.epoch.map_or(marker_root, |(_, root)| root)
            },
            run: None,
        };
        if adopting {
            running.step(SyncStep::Adopt);
            if let Err(error) = session.set_marker(Some(&marker.to_bytes())) {
                return (ended(SyncOutcome::Failed, error.detail), None);
            }
        }
        running.step(SyncStep::Copy);
        running.tell(true, &[SyncDeviceChange::Actions]);
        let copied = self.copy(running, session.as_mut(), plan, &skip);
        // Finish: the marker names the last run that finished.
        running.step(SyncStep::Finish);
        let end = match copied {
            Ok(end) => end,
            Err(end) => end,
        };
        if matches!(
            end.outcome,
            SyncOutcome::Complete | SyncOutcome::FailedInPart
        ) {
            marker.run = Some(running.run.id);
            if let Err(error) = session.set_marker(Some(&marker.to_bytes())) {
                warn!(error = %error.detail, "the marker could not name the run that finished");
            }
        }
        let _ = session.close(false);
        let write_bps = super::lock(&self.devices.live)
            .get(&target.id)
            .and_then(|live| live.run.as_ref())
            .and_then(|run| {
                let ms = u64::try_from(run.copying_since?.elapsed().as_millis()).ok()?;
                // A few small files measure the per-file cost, not the speed.
                (run.written >= 32 * 1024 * 1024 && ms > 0)
                    .then(|| run.written.saturating_mul(1000) / ms)
            });
        (end, write_bps)
    }

    /// A new ID, for the root a marker names.
    fn entropy_id(&self) -> Id {
        let mut random = [0; 10];
        // A marker root only has to differ from the last one.
        let _ = self.entropy.fill(&mut random);
        Id::new_v7(self.clock.now_ms(), random)
    }

    /// Checks what the last run left in doubt against the device: a write
    /// whose file is there as it was meant is done, and earns its baseline;
    /// anything else was not done, and the next plan takes it up.
    fn settle(&self, target: &TargetRow, session: &mut dyn Session) -> Result<(), String> {
        let doubts = self
            .with_library(|_, library| library.sync_in_doubt(target))
            .ok_or("The server stopped.")?
            .map_err(|error| error.to_string())?;
        if doubts.is_empty() {
            return Ok(());
        }
        let mut settled = Vec::new();
        for doubt in doubts {
            let found = doubt
                .path
                .as_deref()
                .and_then(|path| ritornel_device::RelPath::new(path).ok())
                .map(|path| session.signature(&ObjectId::Path(path)))
                .transpose()
                .map_err(|error| error.detail)?
                .flatten();
            let landed = match (&found, doubt.size, &doubt.sampled) {
                // A write landed if what is there is what it meant to leave.
                (Some(found), Some(size), Some(sampled)) if doubt.writes => {
                    found.size == size && found.sampled.as_slice() == sampled.as_slice()
                }
                // A removal landed if nothing is there.
                (None, ..) => !doubt.writes,
                _ => false,
            };
            settled.push((doubt, landed));
        }
        self.with_library(|_, library| library.sync_settle(target, settled))
            .ok_or("The server stopped.")?
            .map_err(|error| error.to_string())
    }

    /// The transfer for one action, as a request to the pool: its master,
    /// how its audio is made, and its tags and cover.
    fn transfer_request(
        &self,
        action: &Action,
        covers: &mut HashMap<Id, Option<Cover>>,
        library_id: Id,
    ) -> Result<TransferRequest, String> {
        let make = action.make.as_ref().ok_or("nothing to make")?;
        let transcodes = self.transcodes().ok_or("the encoders are not running")?;
        self.with_library(|store, library| {
            let roots: HashMap<String, PathBuf> = store
                .roots()
                .map_err(|error| error.to_string())?
                .into_iter()
                .map(|(name, path)| (name, PathBuf::from(path)))
                .collect();
            let master = make
                .copy
                .master(&roots)
                .ok_or("the master's folder is not on this server")?;
            let cover = match make.cover {
                Some((art, px)) => {
                    if let std::collections::hash_map::Entry::Vacant(slot) = covers.entry(art.id) {
                        let listed: Vec<(String, String)> = roots
                            .iter()
                            .map(|(name, path)| (name.clone(), path.to_string_lossy().into_owned()))
                            .collect();
                        let bytes = library
                            .original_cover(art.id, &listed)
                            .map_err(|error| error.to_string())?;
                        slot.insert(bytes.map(|bytes| Cover::new(bytes.into(), px)));
                    }
                    covers.get(&art.id).cloned().flatten()
                }
                None => None,
            };
            let mut tags = make.tags.clone();
            // Read from what the scan stored of the master: no file is
            // opened to tag one.
            tags.gain = library
                .sync_gain(make.copy.revision)
                .map_err(|error| error.to_string())?;
            tags.ritornel_id = action.appearance.map(|_| {
                sync::ritornel_id(
                    library_id,
                    make.copy.source_item_pid,
                    action_appearance(action),
                )
            });
            Ok(TransferRequest {
                master,
                audio: match make.sent {
                    Sent::AsIs(codec) => TransferAudio::PassThrough(codec),
                    Sent::Encode(_, profile) => TransferAudio::Encode(
                        transcodes
                            .encoding(profile)
                            .ok_or("the hub has no encoder for this format")?,
                    ),
                },
                tagging: Arc::new(Tagging {
                    tags,
                    cover,
                    id3v1: make.id3v1,
                }),
            })
        })
        .ok_or("the server is stopping")?
    }

    /// Copy: removes first, then every write a window at a time.
    fn copy(
        &self,
        running: &mut Running<'_>,
        session: &mut dyn Session,
        plan: &Plan,
        skip: &[usize],
    ) -> Result<Ended, Ended> {
        let runtime = self
            .runtime
            .clone()
            .ok_or_else(|| ended(SyncOutcome::Failed, "The server is stopping."))?;
        let transcodes = self
            .transcodes()
            .ok_or_else(|| ended(SyncOutcome::Failed, "The encoders are not running."))?;
        let library_id = self
            .with_library(|_, library| library.library_id())
            .ok_or_else(|| ended(SyncOutcome::CutOff, "The server stopped."))?;
        let target = running.target.clone();
        let wanted = |at: &usize| {
            plan.actions
                .get(*at)
                .is_some_and(|action| action.runs() && !skip.contains(at))
        };
        let removes: Vec<usize> = (0..plan.actions.len())
            .filter(wanted)
            .filter(|at| {
                plan.actions
                    .get(*at)
                    .is_some_and(|a| a.kind == SyncActionKind::Remove)
            })
            .collect();
        let writes: VecDeque<usize> = (0..plan.actions.len())
            .filter(wanted)
            .filter(|at| {
                plan.actions
                    .get(*at)
                    .is_some_and(|a| a.kind != SyncActionKind::Remove)
            })
            .collect();
        let mut results: Vec<Recorded> = Vec::new();
        let mut cut_off: Option<String> = None;
        let record = |running: &mut Running<'_>, results: &mut Vec<Recorded>, next: Vec<Issued>| {
            let (run, done) = (running.run.clone(), std::mem::take(results));
            self.with_library(|_, library| library.sync_window(&target, &run, plan, done, next))
                .ok_or_else(|| ended(SyncOutcome::CutOff, "The server stopped."))?
                .map_err(|error| ended(SyncOutcome::Failed, error.to_string()))
        };
        let settle = |running: &mut Running<'_>, seq: usize, bytes: u64, failed: bool| {
            running.live(|live| {
                live.current.remove(&(seq as i64));
                live.actions_done += 1;
                live.failed += u64::from(failed);
                live.bytes_settled += bytes;
            });
            running.tell(false, &[SyncDeviceChange::Actions]);
        };

        // A deletion is recorded before it is made, so Ritornel's own
        // deletion is never read as the person's.
        record(
            running,
            &mut results,
            removes
                .iter()
                .map(|at| Issued {
                    seq: *at as i64,
                    leaves: None,
                })
                .collect(),
        )?;
        let roots: HashMap<String, PathBuf> = self
            .with_stores(|stores| stores.server().roots())
            .and_then(Result::ok)
            .unwrap_or_default()
            .into_iter()
            .map(|(name, path)| (name, PathBuf::from(path)))
            .collect();
        for at in &removes {
            let Some(action) = plan.actions.get(*at) else {
                continue;
            };
            let Some(old) = &action.old else { continue };
            let outcome = if running.stopped().is_some() || cut_off.is_some() {
                Outcome::Skipped
            } else if !action.masters.iter().any(|(root, rel)| {
                // The master's file itself, since a file can be deleted
                // while its folder stays online and the scan not know.
                roots.get(root).is_some_and(|folder| {
                    let mut path = folder.clone();
                    path.extend(rel.split('/').filter(|part| !part.is_empty()));
                    path.is_file()
                })
            }) {
                Outcome::Conflict(
                    "Its master is no longer in the library's folders, so it was kept.".to_owned(),
                )
            } else {
                let expect = Signature {
                    size: old.size,
                    sampled: old.sampled,
                };
                match session.remove(&ObjectId::Path(old.path.clone()), expect) {
                    Ok(_) => Outcome::Done(None),
                    Err(error) if error.kind == ErrorKind::Changed => {
                        Outcome::Conflict(error.detail)
                    }
                    Err(error) => {
                        if gone(&error) {
                            cut_off = Some(error.detail.clone());
                        }
                        Outcome::Failed(error.code().to_owned(), error.detail)
                    }
                }
            };
            let failed = matches!(outcome, Outcome::Failed(..) | Outcome::Conflict(_));
            results.push(Recorded {
                seq: *at as i64,
                outcome,
                old: Some(old.baseline),
                old_gone: false,
            });
            settle(running, *at, 0, failed);
        }

        running.live(|live| live.copying_since = Some(Instant::now()));
        let mut covers: HashMap<Id, Option<Cover>> = HashMap::new();
        let mut queue = writes;
        while !queue.is_empty() {
            if running.stopped().is_some() || cut_off.is_some() {
                break;
            }
            // Prepare a window: every transfer in it, a few at once.
            let mut window: Vec<Pending> = Vec::new();
            while window.len() < WINDOW {
                let Some(at) = queue.pop_front() else { break };
                let Some(action) = plan.actions.get(at) else {
                    continue;
                };
                running.live(|live| {
                    live.current.insert(
                        at as i64,
                        working(
                            SyncActionState::Preparing,
                            &action.shown.title,
                            &action.shown.artist,
                            at as i64,
                            action.bytes,
                        ),
                    );
                });
                let prepared = match self.transfer_request(action, &mut covers, library_id) {
                    Ok(request) => {
                        let pool = Arc::clone(&transcodes.pool);
                        runtime.spawn(async move {
                            pool.prepare(request)
                                .await
                                .map_err(|error| error.to_string())
                        })
                    }
                    Err(why) => runtime.spawn(async move { Err(why) }),
                };
                window.push(Pending { seq: at, prepared });
            }
            running.tell(true, &[SyncDeviceChange::Actions]);
            let mut ready: Vec<(usize, PreparedTransfer)> = Vec::new();
            for pending in window {
                let prepared = runtime
                    .block_on(pending.prepared)
                    .unwrap_or_else(|_| Err("its file could not be made".to_owned()));
                match prepared {
                    Ok(transfer) => ready.push((pending.seq, transfer)),
                    Err(why) => {
                        results.push(Recorded {
                            seq: pending.seq as i64,
                            outcome: Outcome::Failed("prepare_failed".to_owned(), why),
                            old: None,
                            old_gone: false,
                        });
                        settle(running, pending.seq, 0, true);
                    }
                }
            }
            // The last window's results and this one's intent, together.
            record(
                running,
                &mut results,
                ready
                    .iter()
                    .map(|(seq, transfer)| Issued {
                        seq: *seq as i64,
                        leaves: Some((
                            transfer.len(),
                            *transfer.digest().as_bytes(),
                            *transfer.sampled_hash().as_bytes(),
                        )),
                    })
                    .collect(),
            )?;
            for (at, transfer) in ready {
                let Some(action) = plan.actions.get(at) else {
                    continue;
                };
                if running.stopped().is_some() || cut_off.is_some() {
                    results.push(Recorded {
                        seq: at as i64,
                        outcome: Outcome::Skipped,
                        old: None,
                        old_gone: false,
                    });
                    settle(running, at, 0, false);
                    continue;
                }
                let outcome = self.put(running, session, action, at, &transfer);
                let (recorded, bytes, failed) = match outcome {
                    Ok((left, old_gone)) => {
                        let bytes = left.size;
                        (
                            Recorded {
                                seq: at as i64,
                                outcome: Outcome::Done(Some(left)),
                                old: action.old.as_ref().map(|old| old.baseline),
                                old_gone,
                            },
                            bytes,
                            false,
                        )
                    }
                    Err(error) => {
                        if gone(&error) {
                            cut_off = Some(error.detail.clone());
                        }
                        let outcome = match error.kind {
                            ErrorKind::Stopped => Outcome::Skipped,
                            ErrorKind::Changed => Outcome::Conflict(error.detail),
                            _ => Outcome::Failed(error.code().to_owned(), error.detail),
                        };
                        let failed = !matches!(outcome, Outcome::Skipped);
                        (
                            Recorded {
                                seq: at as i64,
                                outcome,
                                old: None,
                                old_gone: false,
                            },
                            0,
                            failed,
                        )
                    }
                };
                results.push(recorded);
                settle(running, at, bytes, failed);
            }
        }
        // What was never reached is left for the next sync.
        for at in queue {
            results.push(Recorded {
                seq: at as i64,
                outcome: Outcome::Skipped,
                old: None,
                old_gone: false,
            });
        }
        record(running, &mut results, Vec::new())?;
        running.tell(true, &[SyncDeviceChange::Actions]);
        let failed = super::lock(&self.devices.live)
            .get(&target.id)
            .and_then(|live| live.run.as_ref())
            .map_or(0, |run| run.failed);
        Ok(match (cut_off, running.stopped()) {
            (Some(why), _) => ended(SyncOutcome::CutOff, why),
            (None, Some(JobStop::Cancelled)) => Ended {
                outcome: SyncOutcome::Stopped,
                error: None,
            },
            (None, Some(JobStop::Shutdown)) => ended(
                SyncOutcome::CutOff,
                "The server stopped while this sync was running.",
            ),
            (None, None) if failed > 0 => Ended {
                outcome: SyncOutcome::FailedInPart,
                error: None,
            },
            (None, None) => Ended {
                outcome: SyncOutcome::Complete,
                error: None,
            },
        })
    }

    /// Sends one prepared transfer. A replace at the same path takes its
    /// old file's place once the new copy is whole; one at another path is
    /// added, and the old file then removed. Answers what was left, and
    /// whether the old file is known to be gone.
    fn put(
        &self,
        running: &mut Running<'_>,
        session: &mut dyn Session,
        action: &Action,
        at: usize,
        transfer: &PreparedTransfer,
    ) -> Result<(Left, bool), ritornel_device::Error> {
        let path = action.path.as_ref().ok_or_else(|| {
            ritornel_device::Error::new(ErrorKind::Unsafe, "the file has no path")
        })?;
        running.live(|live| {
            if let Some(file) = live.current.get_mut(&(at as i64)) {
                file.state = SyncActionState::Copying;
                file.bytes = transfer.len();
            }
        });
        let old = action
            .old
            .as_ref()
            .filter(|_| action.kind == SyncActionKind::Replace)
            .map(|old| {
                (
                    ObjectId::Path(old.path.clone()),
                    Signature {
                        size: old.size,
                        sampled: old.sampled,
                    },
                )
            });
        let in_place = old.as_ref().filter(|(id, _)| id.path() == Some(path));
        let mut reader = transfer.reader().map_err(|error| {
            ritornel_device::Error::new(
                ErrorKind::Io,
                format!("its copy could not be read: {error}"),
            )
        })?;
        let mut last = 0u64;
        let cancel = running.cancel.clone();
        let core = running.core;
        let device = running.target.id;
        let mut told = Instant::now();
        let info = session.put(
            &Put {
                path,
                len: transfer.len(),
                digest: *transfer.digest().as_bytes(),
                replaces: in_place.map(|(id, signature)| (id, *signature)),
            },
            &mut reader,
            &mut |sent| {
                let step = sent.saturating_sub(last);
                last = sent;
                if let Some(run) = super::lock(&core.devices.live)
                    .get_mut(&device)
                    .and_then(|live| live.run.as_mut())
                {
                    run.written += step;
                    if let Some(file) = run.current.get_mut(&(at as i64)) {
                        file.bytes_done = sent;
                    }
                }
                if told.elapsed() >= TELL_EVERY {
                    told = Instant::now();
                    core.device_changed(device, &[SyncDeviceChange::Actions]);
                }
                cancel.stopped().is_none()
            },
        )?;
        let left = Left {
            path: info.path,
            size: info.size,
            sampled: *transfer.sampled_hash().as_bytes(),
            digest: *transfer.digest().as_bytes(),
        };
        // A new path beside the old file: the old one goes once the new
        // one is there. One that was changed meanwhile is left, and is no
        // longer Ritornel's.
        let mut old_gone = in_place.is_some();
        if in_place.is_none()
            && let Some((id, signature)) = &old
        {
            old_gone = session.remove(id, *signature).is_ok();
        }
        Ok((left, old_gone))
    }
}

/// The appearance a file is of, as Ritornel's identifier names it.
fn action_appearance(action: &Action) -> Id {
    action
        .make
        .as_ref()
        .map_or(Id::from_bytes([0; 16]), |make| make.appearance)
}
