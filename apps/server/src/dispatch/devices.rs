//! Syncing part of the library to a player or a folder: the `devices.*`
//! calls and the job that carries a sync out. See
//! docs/internals/device-sync.md.
//!
//! The hub plans, makes every file and keeps every record. The plan of a
//! target's next sync is a preview held in memory and made again whenever
//! its inputs change; what a run did is in `library.db`.

mod run;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use ritornel_contract::methods::{
    DevicesAddFolder, DevicesEject, DevicesForget, DevicesSet, DevicesSync, Method,
};
use ritornel_contract::transcode::{Mp3, Target, Vbr};
use ritornel_contract::{
    ErrorCode, Event, Id, Receipt, ReceiptStatus, RpcError, SyncActionState, SyncActions,
    SyncActionsParams, SyncActivity, SyncAddFolderParams, SyncAttention, SyncAttentionLevel,
    SyncBasis, SyncCapabilities, SyncConnection, SyncDevice, SyncDeviceChange, SyncDeviceChanged,
    SyncDeviceCommandParams, SyncDeviceDetail, SyncDeviceIcon, SyncDeviceParams, SyncDevicesEvent,
    SyncDevicesList, SyncFacts, SyncFolder, SyncFolderKind, SyncFormat, SyncFormatOption,
    SyncFormats, SyncLastSync, SyncLossless, SyncMadeFor, SyncOutcome, SyncPageParams, SyncPlan,
    SyncPlays, SyncPlaysKind, SyncPlaysSummary, SyncRun, SyncRunFile, SyncRunStep, SyncSetParams,
    SyncSettings, SyncSetup, SyncSource, SyncSourceSpec, SyncStartParams, SyncStarted, SyncStep,
    SyncStepState, SyncTransport, SyncUnmanaged, SyncUnmanagedFile, SyncUnmanagedSummary,
    SyncWhenFull,
};
use ritornel_device::{Capabilities, ObjectId, ObjectInfo, Plays, RelPath, Session, Storage};
use ritornel_engine::sync::{
    self, Baseline, CopyRow, Encodes, FolderSession, FormatPolicy, Marker, Overlap, Plan, RunRow,
    Seen, Speed, TargetChanges, TargetRow, rows,
};
use ritornel_engine::transcode::{Codec, Master, Profile};
use ritornel_engine::{Command, LibraryStore, NewJob, ServerStore, WriteRequest};

use super::transcode::Transcodes;
use super::{Core, Stores, fingerprint, lock, revision};

/// The job that carries a sync.
pub(crate) const SYNC_JOB: &str = "devices.sync";

/// How a call wants a target's plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Use {
    /// The plan held, where the library and the target's settings have not
    /// moved: for the column, which reads no target to list them all.
    Held,
    /// The target read now. The plan held serves only if what is on the
    /// target is what it stood on.
    Read,
}

/// What the server holds of each target while it runs.
#[derive(Debug, Default)]
pub(crate) struct Devices {
    live: Mutex<HashMap<Id, Live>>,
}

#[derive(Debug, Default)]
struct Live {
    /// Moves with anything about the target.
    revision: u64,
    /// Whether it was attached when last looked at, and since when.
    attached: Option<(bool, i64)>,
    planned: Option<Arc<Planned>>,
    run: Option<LiveRun>,
}

/// A target's plan, with what it stood on.
#[derive(Debug)]
struct Planned {
    plan: Plan,
    items: Vec<rows::Item>,
    library_rev: i64,
    target_rev: i64,
    /// What was on the target when it was made.
    stood_on: blake3::Hash,
    /// Tracks and albums Ritornel manages on it now.
    managed: (u64, u64),
    /// Whether its marker is there and names this target's open epoch.
    adopted: bool,
}

/// A run under way, as its page shows it.
#[derive(Debug, Clone)]
struct LiveRun {
    run: RunRow,
    step: SyncStep,
    adopting: bool,
    stopping: bool,
    actions_total: u64,
    actions_done: u64,
    failed: u64,
    bytes_total: u64,
    /// Bytes of the files finished.
    bytes_settled: u64,
    /// The files being worked on, by their place in the run.
    current: BTreeMap<i64, SyncRunFile>,
    /// When copying began, and the bytes written since, for the rate.
    copying_since: Option<Instant>,
    written: u64,
}

fn internal(error: impl std::fmt::Display) -> RpcError {
    RpcError::new(ErrorCode::Internal, error.to_string())
}

fn in_library(error: ritornel_engine::Error) -> RpcError {
    use ritornel_engine::Error as Engine;
    match error {
        Engine::NotFound(what) => RpcError::new(ErrorCode::NotFound, what),
        Engine::Invalid(why) => RpcError::new(ErrorCode::Invalid, why),
        Engine::PlanStale => RpcError::new(ErrorCode::PlanStale, "the plan has changed"),
        Engine::CommandMismatch { .. } => RpcError::new(
            ErrorCode::IdempotencyMismatch,
            "this idempotency key was used before with other params",
        ),
        other => super::failed(crate::Error::Library(other)),
    }
}

fn no_device() -> RpcError {
    RpcError::new(ErrorCode::NotFound, "there is no such device")
}

fn busy() -> RpcError {
    RpcError::new(ErrorCode::Conflict, "the device is syncing: stop it first")
        .with_detail("reason", "busy")
}

fn stored<T: serde::de::DeserializeOwned>(method: &str, json: &str) -> Result<T, RpcError> {
    serde_json::from_str(json).map_err(|error| {
        internal(format!(
            "the stored result of {method} could not be read: {error}"
        ))
    })
}

/// What the planner asks of the hub's encoders and their cache.
struct Encoders<'a> {
    transcodes: Option<&'a Transcodes>,
    store: &'a ServerStore,
}

impl Encodes for Encoders<'_> {
    fn has(&self, codec: Codec) -> bool {
        let family = match codec {
            Codec::Mp3 => "lame",
            Codec::Opus => "opusenc",
            Codec::Flac => return true,
        };
        self.transcodes
            .is_some_and(|transcodes| transcodes.encoders.iter().any(|e| e.family == family))
    }

    fn key(&self, copy: &CopyRow, profile: Profile) -> Option<blake3::Hash> {
        // Only the audio keys an encode: where the master is does not.
        let master = Master {
            path: PathBuf::new(),
            asset: copy.asset(),
            facts: ritornel_engine::playback::FileFacts {
                size_bytes: copy.size_bytes,
                mtime_ms: copy.mtime_ms,
            },
        };
        self.transcodes?.encoding(profile)?.key(&master).ok()
    }

    fn size(&self, key: blake3::Hash) -> (Option<u64>, bool) {
        let Some(transcodes) = self.transcodes else {
            return (None, false);
        };
        let cache = transcodes.pool.cache();
        (
            cache.size_hint(self.store, key).ok().flatten(),
            cache.holds(self.store, key).unwrap_or(false),
        )
    }
}

/// What was read of an attached target.
struct Read {
    seen: Seen,
    marker: Option<Marker>,
}

impl Read {
    /// What tells this reading from another: every file by its path and
    /// size, the marker, and the room to the nearest 64 MiB, so that the
    /// free space of a busy disk does not make every reading a new one.
    fn digest(&self) -> blake3::Hash {
        let mut files: Vec<(&str, u64)> = self
            .seen
            .objects
            .iter()
            .map(|object| (object.path.as_str(), object.size))
            .collect();
        files.sort_unstable();
        let mut hasher = blake3::Hasher::new();
        for (path, size) in files {
            hasher.update(path.as_bytes());
            hasher.update(&[0]);
            hasher.update(&size.to_le_bytes());
        }
        hasher.update(&self.seen.storage.capacity_bytes.to_le_bytes());
        hasher.update(&(self.seen.storage.free_bytes >> 26).to_le_bytes());
        if let Some(marker) = &self.marker {
            hasher.update(&marker.to_bytes());
        }
        hasher.finalize()
    }
}

fn read(session: &mut dyn Session, music_root: &RelPath) -> Result<Read, ritornel_device::Error> {
    let storage = session.storage()?;
    let objects = session.inventory(music_root)?;
    let marker = session.marker()?.and_then(|bytes| Marker::read(&bytes));
    Ok(Read {
        seen: Seen {
            objects,
            storage,
            basis: SyncBasis::Live,
        },
        marker,
    })
}

/// What a target can do: its family's, with the codecs it is set to play.
/// A folder plays what the person says it does, and is ejected only where
/// it is a drive that comes and goes.
fn capabilities_of(target: &TargetRow) -> Capabilities {
    Capabilities {
        transport: target.transport,
        plays: Plays::None,
        ejectable: removable(target),
        can_sample: true,
        codecs: target
            .settings
            .codecs
            .iter()
            .map(|codec| codec.extension())
            .collect(),
        cover_sizes: vec![500, 300],
        path_bytes: 250,
        write_bps: 60 * 1024 * 1024,
    }
}

fn removable(target: &TargetRow) -> bool {
    matches!(target.settings.folder, Some((_, SyncFolderKind::Removable)))
}

/// The drawing a target has where none is chosen: a folder is drawn as
/// what it is made for.
fn default_icon(target: &TargetRow) -> String {
    match target.settings.folder {
        Some((SyncMadeFor::CarStereo, _)) => "folder.car_stereo".to_owned(),
        Some((SyncMadeFor::Phone, _)) => "folder.phone".to_owned(),
        Some((SyncMadeFor::OtherPlayer, _)) => "folder.player".to_owned(),
        Some((SyncMadeFor::PlainCopy, SyncFolderKind::Removable)) => "folder.drive".to_owned(),
        Some((SyncMadeFor::PlainCopy, SyncFolderKind::AlwaysHere)) => "folder".to_owned(),
        None => target.family.clone(),
    }
}

fn music_root(target: &TargetRow) -> Result<RelPath, RpcError> {
    RelPath::new(target.settings.music_root.clone())
        .map_err(|_| internal("the device's music folder is not a plain path"))
}

fn settings_wire(target: &TargetRow) -> SyncSettings {
    let settings = &target.settings;
    SyncSettings {
        format: settings.format.into(),
        cover_px: settings.cover_px,
        id3v1: settings.id3v1,
        reserve_bytes: settings.reserve_bytes,
        limit_bytes: settings.limit_bytes,
        when_full: settings.when_full,
        on_connect: settings.on_connect,
        read_plays: settings.read_plays,
        scrobble_plays: settings.scrobble_plays,
        remind_after_days: settings.remind_after_days,
    }
}

fn steps(step: SyncStep, adopting: bool, ended: Option<SyncOutcome>) -> Vec<SyncRunStep> {
    let order: Vec<SyncStep> = [
        Some(SyncStep::Read),
        Some(SyncStep::Plan),
        adopting.then_some(SyncStep::Adopt),
        Some(SyncStep::Copy),
        Some(SyncStep::Finish),
    ]
    .into_iter()
    .flatten()
    .collect();
    let at = order.iter().position(|each| *each == step).unwrap_or(0);
    let finished = matches!(
        ended,
        Some(SyncOutcome::Complete | SyncOutcome::FailedInPart)
    );
    order
        .into_iter()
        .enumerate()
        .map(|(index, step)| SyncRunStep {
            step,
            state: if finished || index < at {
                SyncStepState::Done
            } else if index == at && ended.is_none() {
                SyncStepState::Running
            } else {
                SyncStepState::ToCome
            },
        })
        .collect()
}

fn run_wire(row: &RunRow, live: Option<&LiveRun>) -> SyncRun {
    match live {
        Some(live) => {
            let bytes_done = live.bytes_settled
                + live
                    .current
                    .values()
                    .map(|file| file.bytes_done)
                    .sum::<u64>();
            let rate = live.copying_since.and_then(|since| {
                let ms = u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX);
                (ms >= 500 && live.written > 0).then(|| live.written.saturating_mul(1000) / ms)
            });
            SyncRun {
                run_id: row.id,
                job_id: row.job,
                running: true,
                stopping: live.stopping,
                steps: steps(live.step, live.adopting, None),
                outcome: None,
                error: None,
                actions_done: live.actions_done,
                actions_total: live.actions_total,
                failed: live.failed,
                bytes_done,
                bytes_total: live.bytes_total,
                rate_bytes_per_sec: rate,
                eta_ms: rate.filter(|rate| *rate > 0).map(|rate| {
                    live.bytes_total
                        .saturating_sub(bytes_done)
                        .saturating_mul(1000)
                        / rate
                }),
                started_at_ms: row.started_at_ms,
                ended_at_ms: None,
                current: live.current.values().cloned().collect(),
            }
        }
        None => SyncRun {
            run_id: row.id,
            job_id: row.job,
            running: row.ended_at_ms.is_none(),
            stopping: false,
            steps: steps(row.step, row.adopted, row.outcome),
            outcome: row.outcome,
            error: row.error.clone(),
            actions_done: row.actions_done,
            actions_total: row.actions_total,
            failed: row.actions_failed,
            bytes_done: row.bytes_done,
            bytes_total: row.bytes_total,
            rate_bytes_per_sec: None,
            eta_ms: None,
            started_at_ms: row.started_at_ms,
            ended_at_ms: row.ended_at_ms,
            current: Vec::new(),
        },
    }
}

fn plan_wire(plan: &Plan, when_full: SyncWhenFull) -> SyncPlan {
    let mut needs_you = Vec::new();
    if !plan.fits && when_full == SyncWhenFull::Ask {
        needs_you.push("does_not_fit".to_owned());
    }
    if plan.counts.conflict.tracks > 0 {
        needs_you.push("conflicts".to_owned());
    }
    SyncPlan {
        plan_revision: plan.revision.clone(),
        basis: plan.basis,
        figures: plan.figures,
        figures_skipping_retags: plan.figures_skipping_retags,
        fits: plan.fits,
        cut: plan.cut,
        counts: plan.counts,
        by_source: plan.by_source.clone(),
        encodes_needed: plan.encodes_needed,
        encodes_cached: plan.encodes_cached,
        needs_you,
    }
}

/// The formats a target is offered: lossless as it is, then encodes from
/// the largest to the smallest.
fn offered() -> Vec<SyncFormat> {
    let encode = |target| SyncFormat {
        lossless: SyncLossless::Encode { target },
    };
    vec![
        SyncFormat {
            lossless: SyncLossless::AsIs,
        },
        encode(Target::Mp3 {
            mode: Mp3::Cbr(320),
        }),
        encode(Target::Mp3 { mode: Mp3::V0 }),
        encode(Target::Mp3 { mode: Mp3::V2 }),
        encode(Target::Opus {
            bitrate_kbps: 160,
            complexity: 10,
            vbr: Vbr::Variable,
        }),
    ]
}

impl Core {
    fn device_transport(target: &TargetRow) -> SyncTransport {
        match target.transport {
            ritornel_device::Transport::Folder => SyncTransport::Folder,
            ritornel_device::Transport::MassStorage => SyncTransport::MassStorage,
            ritornel_device::Transport::Mtp => SyncTransport::Mtp,
        }
    }

    /// Where a folder target is on this machine, if this is the server it
    /// is on and it has a path for it.
    fn folder_of(&self, stores: &Stores, target: &TargetRow) -> Result<PathBuf, &'static str> {
        if target.host != Some(self.server_id) {
            return Err("root_unbound");
        }
        let root = target.root.ok_or("root_unbound")?;
        stores
            .server()
            .sync_roots()
            .ok()
            .and_then(|roots| roots.into_iter().find(|(id, _)| *id == root))
            .map(|(_, path)| PathBuf::from(path))
            .ok_or("root_unbound")
    }

    /// Opens a session on a target, or says as a word why it cannot be
    /// used: it is not here.
    fn open_device(
        &self,
        stores: &Stores,
        target: &TargetRow,
    ) -> Result<Box<dyn Session>, &'static str> {
        match target.transport {
            ritornel_device::Transport::Folder => {
                let path = self.folder_of(stores, target)?;
                FolderSession::open(&path, removable(target))
                    .map(|session| Box::new(session) as Box<dyn Session>)
                    .map_err(|_| "root_missing")
            }
            // No other family is built yet.
            _ => Err("not_supported"),
        }
    }

    /// Bumps a target's revision and tells its followers what changed.
    fn device_changed(&self, device: Id, changed: &[SyncDeviceChange]) {
        let (revision, run) = {
            let mut live = lock(&self.devices.live);
            let live = live.entry(device).or_default();
            live.revision = live.revision.max(revision(self.clock.now_ms())) + 1;
            if changed
                .iter()
                .any(|change| matches!(change, SyncDeviceChange::Settings | SyncDeviceChange::Plan))
            {
                live.planned = None;
            }
            (
                live.revision,
                live.run.as_ref().map(|run| run_wire(&run.run, Some(run))),
            )
        };
        let mut changed = changed.to_vec();
        changed.sort_unstable();
        changed.dedup();
        self.publish(&Event::Devices(Box::new(SyncDevicesEvent {
            device_id: device,
            device_revision: revision,
            changed,
            run,
        })));
    }

    /// The plan of a target's next sync: the one held, while what it stood
    /// on has not moved, or one made now. `None` for a target that cannot
    /// be planned: an ignored one, or one that is away and was never read.
    fn planned(
        &self,
        stores: &Stores,
        library: &LibraryStore,
        target: &TargetRow,
        how: Use,
    ) -> Result<(Option<Arc<Planned>>, Option<&'static str>), RpcError> {
        let library_rev = library.meta().map_err(in_library)?.revision;
        let session = self.open_device(stores, target);
        let problem = session.as_ref().err().copied();
        self.note_attached(target.id, problem.is_none());
        if target.setup == SyncSetup::Ignored {
            return Ok((None, problem));
        }
        let held = lock(&self.devices.live)
            .get(&target.id)
            .and_then(|live| live.planned.clone())
            .filter(|held| {
                held.library_rev == library_rev
                    && held.target_rev == target.rev
                    // A plan of a target that was away is no plan of one
                    // that is here.
                    && (held.plan.basis == SyncBasis::Live) == problem.is_none()
            });
        if how == Use::Held
            && let Some(held) = &held
        {
            return Ok((Some(Arc::clone(held)), problem));
        }
        let read = match session {
            Ok(mut session) => match read(session.as_mut(), &music_root(target)?) {
                Ok(read) => Ok(read),
                Err(_) => return Ok((None, Some("root_missing"))),
            },
            Err(problem) => Err(problem),
        };
        if let (Ok(read), Some(held)) = (&read, &held)
            && held.stood_on == read.digest()
        {
            return Ok((Some(Arc::clone(held)), problem));
        }
        if let (Err(_), Some(held)) = (&read, &held) {
            // Away, nothing can have been read since.
            return Ok((Some(Arc::clone(held)), problem));
        }
        let planned = self.make_plan(stores, library, target, read, library_rev)?;
        let Some(planned) = planned.map(Arc::new) else {
            return Ok((None, problem));
        };
        let moved = {
            let mut live = lock(&self.devices.live);
            let live = live.entry(target.id).or_default();
            let moved = live
                .planned
                .as_ref()
                .is_some_and(|held| held.plan.revision != planned.plan.revision);
            live.planned = Some(Arc::clone(&planned));
            moved
        };
        if moved {
            // Told without dropping the plan just made.
            self.plan_moved(target.id);
        }
        Ok((Some(planned), problem))
    }

    /// Makes a plan for `target` as given, on what was read of it or, away,
    /// on what was last recorded of it, and holds it nowhere.
    fn make_plan(
        &self,
        stores: &Stores,
        library: &LibraryStore,
        target: &TargetRow,
        read: Result<Read, &'static str>,
        library_rev: i64,
    ) -> Result<Option<Planned>, RpcError> {
        let (tracks, sources, baselines, _) = library.sync_inputs(target).map_err(in_library)?;
        let root = music_root(target)?;
        let capabilities = capabilities_of(target);
        let (seen, stood_on, adopted, baselines) = match read {
            Ok(read) => {
                // The marker makes what is recorded true of this storage.
                // Without it, everything there is a file Ritornel did not
                // write.
                let adopted = target.setup == SyncSetup::Ready
                    && read.marker.as_ref().is_some_and(|marker| {
                        marker.target == target.id
                            && target.epoch == Some((marker.epoch, marker.root))
                    });
                let baselines = if adopted { baselines } else { Vec::new() };
                let stood_on = read.digest();
                (read.seen, stood_on, adopted, baselines)
            }
            Err(_) => {
                // Away: as it was last recorded, every file as its baseline
                // has it.
                let Some((capacity, free, _)) = target.seen else {
                    return Ok(None);
                };
                let seen = Seen {
                    objects: baselines
                        .iter()
                        .map(|baseline: &Baseline| ObjectInfo {
                            id: ObjectId::Path(baseline.path.clone()),
                            path: baseline.path.clone(),
                            size: baseline.size,
                            modified_ms: None,
                        })
                        .collect(),
                    storage: Storage {
                        capacity_bytes: capacity,
                        free_bytes: free,
                        cluster_bytes: None,
                    },
                    basis: SyncBasis::LastSeen,
                };
                (seen, blake3::hash(b"away"), true, baselines)
            }
        };
        let transcodes = self.transcodes();
        let encoders = Encoders {
            transcodes: transcodes.as_deref(),
            store: stores.server(),
        };
        let speed = Speed {
            write_bps: target.write_bps.unwrap_or(capabilities.write_bps),
            workers: transcodes
                .as_ref()
                .map_or(1, |transcodes| transcodes.config.workers.get() as u64),
        };
        let plan = sync::plan(
            &tracks,
            &sources,
            &baselines,
            &seen,
            &target.settings,
            &root,
            &capabilities,
            &encoders,
            speed,
        );
        let there: HashSet<&RelPath> = seen.objects.iter().map(|object| &object.path).collect();
        let managed: Vec<&Baseline> = baselines
            .iter()
            .filter(|baseline| there.contains(&baseline.path))
            .collect();
        let albums: HashSet<&str> = managed
            .iter()
            .map(|baseline| baseline.shown.group_key.as_str())
            .collect();
        let planned = Planned {
            items: rows::of_plan(&plan),
            managed: (managed.len() as u64, albums.len() as u64),
            plan,
            library_rev,
            target_rev: target.rev,
            stood_on,
            adopted,
        };
        Ok(Some(planned))
    }

    fn plan_moved(&self, device: Id) {
        let revision = {
            let mut live = lock(&self.devices.live);
            let live = live.entry(device).or_default();
            live.revision = live.revision.max(revision(self.clock.now_ms())) + 1;
            live.revision
        };
        self.publish(&Event::Devices(Box::new(SyncDevicesEvent {
            device_id: device,
            device_revision: revision,
            changed: vec![SyncDeviceChange::Plan],
            run: None,
        })));
    }

    /// Keeps since when a target has been here, or away.
    fn note_attached(&self, device: Id, attached: bool) {
        let mut live = lock(&self.devices.live);
        let live = live.entry(device).or_default();
        if live.attached.is_none_or(|(was, _)| was != attached) {
            live.attached = Some((attached, self.clock.now_ms()));
        }
    }

    /// One target as the column shows it.
    fn device(
        &self,
        library: &LibraryStore,
        target: &TargetRow,
        planned: Option<&Planned>,
        problem: Option<&'static str>,
    ) -> Result<SyncDevice, RpcError> {
        let last = library.sync_last_run(target).map_err(in_library)?;
        let (revision, since, running) = {
            let live = lock(&self.devices.live);
            let live = live.get(&target.id);
            (
                live.map_or(0, |live| live.revision),
                live.and_then(|live| live.attached).map(|(_, since)| since),
                live.is_some_and(|live| live.run.is_some()),
            )
        };
        let open = last.as_ref().is_some_and(|run| run.ended_at_ms.is_none());
        let mut reasons = Vec::new();
        let mut level = SyncAttentionLevel::None;
        let mut raise = |to: SyncAttentionLevel, reason: &str| {
            reasons.push(reason.to_owned());
            if to == SyncAttentionLevel::Failed || level == SyncAttentionLevel::None {
                level = to;
            }
        };
        if target.setup != SyncSetup::Ignored {
            if problem == Some("root_unbound") {
                raise(SyncAttentionLevel::NeedsYou, "root_unbound");
            }
            if let Some(planned) = planned {
                if target.setup == SyncSetup::Ready
                    && planned.plan.basis == SyncBasis::Live
                    && !planned.adopted
                {
                    raise(SyncAttentionLevel::NeedsYou, "marker_gone");
                }
                if !planned.plan.fits && target.settings.when_full == SyncWhenFull::Ask {
                    raise(SyncAttentionLevel::NeedsYou, "does_not_fit");
                }
                if planned.plan.counts.conflict.tracks > 0 {
                    raise(SyncAttentionLevel::NeedsYou, "conflicts");
                }
            }
            match last.as_ref().and_then(|run| run.outcome) {
                Some(SyncOutcome::CutOff) => raise(SyncAttentionLevel::NeedsYou, "cut_off"),
                Some(SyncOutcome::FailedInPart) => {
                    raise(SyncAttentionLevel::Failed, "files_failed")
                }
                Some(SyncOutcome::Failed) => raise(SyncAttentionLevel::Failed, "sync_failed"),
                _ => {}
            }
        }
        Ok(SyncDevice {
            device_id: target.id,
            kind: target.kind,
            family: target.family.clone(),
            transport: Self::device_transport(target),
            name: target.name.clone(),
            icon: SyncDeviceIcon {
                key: target.icon_key.clone(),
                default: default_icon(target),
            },
            setup: target.setup,
            connection: SyncConnection {
                attached: problem.is_none(),
                host_server_id: target.host,
                since_ms: since,
                problem: problem.map(str::to_owned),
            },
            activity: if running || open {
                SyncActivity::Syncing
            } else {
                SyncActivity::Idle
            },
            attention: SyncAttention { level, reasons },
            to_copy_bytes: planned.map(|planned| planned.plan.figures.to_copy_bytes),
            last_sync: last
                .iter()
                .filter_map(|run| {
                    Some(SyncLastSync {
                        at_ms: run.ended_at_ms?,
                        outcome: run.outcome?,
                        copied: run.copied,
                        removed: run.removed,
                        failed: run.actions_failed,
                    })
                })
                .next(),
            // A folder gives no plays back, so nothing ages out.
            connect_by_ms: None,
            device_revision: revision,
        })
    }

    fn target(library: &LibraryStore, device: Id) -> Result<TargetRow, RpcError> {
        library
            .sync_target(device)
            .map_err(in_library)?
            .ok_or_else(no_device)
    }

    pub(super) fn devices_list(&self, stores: &Stores) -> Result<SyncDevicesList, RpcError> {
        Self::library(stores, |library| {
            let targets = library.sync_targets().map_err(in_library)?;
            let devices = targets
                .iter()
                .map(|target| {
                    let (planned, problem) = self.planned(stores, library, target, Use::Held)?;
                    self.device(library, target, planned.as_deref(), problem)
                })
                .collect::<Result<_, _>>()?;
            Ok(SyncDevicesList { devices })
        })
    }

    pub(super) fn devices_get(
        &self,
        stores: &Stores,
        params: &SyncDeviceParams,
        show_paths: bool,
    ) -> Result<SyncDeviceDetail, RpcError> {
        Self::library(stores, |library| {
            let target = Self::target(library, params.device_id)?;
            // The page is the truth of the target: it is read now, but not
            // under a sync, whose own rows are what the page shows then.
            let syncing = lock(&self.devices.live)
                .get(&target.id)
                .is_some_and(|live| live.run.is_some());
            let how = if syncing { Use::Held } else { Use::Read };
            let (planned, problem) = self.planned(stores, library, &target, how)?;
            let device = self.device(library, &target, planned.as_deref(), problem)?;
            let sources = library.sync_sources(&target).map_err(in_library)?;
            let capabilities = capabilities_of(&target);
            let last = library.sync_last_run(&target).map_err(in_library)?;
            let live = lock(&self.devices.live)
                .get(&target.id)
                .and_then(|live| live.run.clone());
            let run = match (&live, &last) {
                (Some(live), _) => Some(run_wire(&live.run, Some(live))),
                (None, Some(last)) => Some(run_wire(last, None)),
                (None, None) => None,
            };
            let unmanaged = match &planned {
                Some(planned) => SyncUnmanagedSummary {
                    files: planned.plan.unmanaged.len() as u64,
                    bytes: planned.plan.room.unmanaged_bytes,
                },
                None => {
                    let files = library.sync_unmanaged(&target).map_err(in_library)?;
                    SyncUnmanagedSummary {
                        files: files.len() as u64,
                        bytes: files.iter().map(|(_, size, _)| size).sum(),
                    }
                }
            };
            Ok(SyncDeviceDetail {
                path: show_paths
                    .then(|| self.folder_of(stores, &target).ok())
                    .flatten()
                    .map(|path| path.to_string_lossy().into_owned()),
                folder: target.settings.folder.map(|(made_for, kind)| SyncFolder {
                    made_for,
                    kind,
                    playlists: target.settings.playlists,
                }),
                facts: SyncFacts {
                    tracks: planned.as_ref().map_or(0, |planned| planned.managed.0),
                    albums: planned.as_ref().map_or(0, |planned| planned.managed.1),
                    music_root: target.settings.music_root.clone(),
                    layout: "artist_album_track".to_owned(),
                    ..SyncFacts::default()
                },
                capabilities: SyncCapabilities {
                    plays: match capabilities.plays {
                        Plays::None => SyncPlaysKind::None,
                        Plays::Log => SyncPlaysKind::PlayLog,
                        Plays::Counters => SyncPlaysKind::Counters,
                    },
                    ejectable: capabilities.ejectable,
                    cover_sizes: capabilities.cover_sizes.clone(),
                    codecs: capabilities
                        .codecs
                        .iter()
                        .map(|c| (*c).to_owned())
                        .collect(),
                },
                room: planned.as_ref().map(|planned| planned.plan.room),
                settings: settings_wire(&target),
                sources: sources
                    .iter()
                    .enumerate()
                    .map(|(at, source)| {
                        let totals = planned
                            .as_ref()
                            .and_then(|planned| planned.plan.sources.get(at))
                            .copied()
                            .unwrap_or_default();
                        SyncSource {
                            source_id: source.id,
                            spec: SyncSourceSpec {
                                role: source.role,
                                selector: source.selector,
                                unit: source.unit,
                            },
                            label: source.label.clone(),
                            detail: source.detail.clone(),
                            art_id: source.art,
                            albums: totals.albums,
                            claimed: totals.claimed,
                            cut: totals.cut,
                        }
                    })
                    .collect(),
                unmanaged,
                // While a run is under way its rows are what is read.
                plan: planned
                    .as_ref()
                    .filter(|_| live.is_none())
                    .map(|planned| plan_wire(&planned.plan, target.settings.when_full)),
                run,
                plays: SyncPlaysSummary::default(),
                device,
            })
        })
    }

    pub(super) fn devices_formats(
        &self,
        stores: &Stores,
        params: &SyncDeviceParams,
    ) -> Result<SyncFormats, RpcError> {
        Self::library(stores, |library| {
            let target = Self::target(library, params.device_id)?;
            let current: SyncFormat = target.settings.format.into();
            let transcodes = self.transcodes();
            let encoders = Encoders {
                transcodes: transcodes.as_deref(),
                store: stores.server(),
            };
            let capabilities = capabilities_of(&target);
            let mut formats = Vec::new();
            for format in offered() {
                // The same target, as it would plan in this format.
                let mut as_if = target.clone();
                as_if.settings.format = FormatPolicy::from(format);
                // Asked afresh, and not held: only the target's own plan is.
                let policy = as_if.settings.format;
                let codec = super::transcode::codec_of(policy.profile().target);
                let available =
                    encoders.has(codec) && capabilities.codecs.contains(&codec.extension());
                let plan = self.plan_as(stores, library, &as_if)?;
                formats.push(SyncFormatOption {
                    format,
                    current: format == current,
                    encoder: transcodes.as_ref().and_then(|transcodes| {
                        transcodes
                            .capabilities()
                            .into_iter()
                            .find(|encoder| encoder.formats.iter().any(|f| f == codec.extension()))
                    }),
                    available,
                    estimated_bytes: plan.as_ref().map_or(0, |plan| plan.wanted_bytes),
                    fits: plan.as_ref().is_some_and(|plan| plan.fits),
                    over_bytes: plan.as_ref().map_or(0, |plan| plan.room.over_bytes),
                    estimated_ms: plan.as_ref().map_or(0, |plan| plan.figures.estimated_ms),
                });
            }
            Ok(SyncFormats { formats })
        })
    }

    /// A plan for a target as given, held nowhere: what `devices.formats`
    /// asks for each format on offer.
    fn plan_as(
        &self,
        stores: &Stores,
        library: &LibraryStore,
        target: &TargetRow,
    ) -> Result<Option<Plan>, RpcError> {
        let library_rev = library.meta().map_err(in_library)?.revision;
        let read = match self.open_device(stores, target) {
            Ok(mut session) => match read(session.as_mut(), &music_root(target)?) {
                Ok(read) => Ok(read),
                Err(_) => return Ok(None),
            },
            Err(problem) => Err(problem),
        };
        let planned = self.make_plan(stores, library, target, read, library_rev)?;
        Ok(planned.map(|planned| planned.plan))
    }

    pub(super) fn devices_actions(
        &self,
        stores: &Stores,
        params: &SyncActionsParams,
    ) -> Result<SyncActions, RpcError> {
        Self::library(stores, |library| {
            let target = Self::target(library, params.device_id)?;
            if let Some(run_id) = params.run_id {
                let run = library
                    .sync_run(&target, run_id)
                    .map_err(in_library)?
                    .ok_or_else(|| RpcError::new(ErrorCode::NotFound, "there is no such run"))?;
                let actions = library.sync_actions(&run).map_err(in_library)?;
                if actions.is_empty() && run.actions_total > 0 {
                    return Err(RpcError::new(
                        ErrorCode::NotFound,
                        "a later sync has replaced this run's rows",
                    )
                    .with_detail("reason", "actions_expired"));
                }
                let live = lock(&self.devices.live)
                    .get(&target.id)
                    .and_then(|live| live.run.clone())
                    .filter(|live| live.run.id == run.id);
                let items = rows::of_run(actions, run.ended_at_ms.is_none(), |seq| {
                    live.as_ref()
                        .and_then(|live| live.current.get(&seq))
                        .map(|file| (file.state, file.bytes_done))
                });
                return Ok(rows::page(&items, params));
            }
            let (planned, _) = self.planned(stores, library, &target, Use::Held)?;
            let planned = planned
                .ok_or_else(|| RpcError::new(ErrorCode::Unavailable, "the device has no plan"))?;
            if params
                .plan_revision
                .as_ref()
                .is_some_and(|asked| *asked != planned.plan.revision)
            {
                return Err(RpcError::new(
                    ErrorCode::PlanStale,
                    "the plan has changed: read the device again",
                ));
            }
            Ok(rows::page(&planned.items, params))
        })
    }

    pub(super) fn devices_unmanaged(
        &self,
        stores: &Stores,
        params: &SyncPageParams,
    ) -> Result<SyncUnmanaged, RpcError> {
        Self::library(stores, |library| {
            let target = Self::target(library, params.device_id)?;
            let (planned, _) = self.planned(stores, library, &target, Use::Held)?;
            let files: Vec<SyncUnmanagedFile> = match planned {
                Some(planned) => planned
                    .plan
                    .unmanaged
                    .iter()
                    .map(|object| SyncUnmanagedFile {
                        path: object.path.as_str().to_owned(),
                        size_bytes: object.size,
                        modified_ms: object.modified_ms,
                    })
                    .collect(),
                None => library
                    .sync_unmanaged(&target)
                    .map_err(in_library)?
                    .into_iter()
                    .map(|(path, size_bytes, modified_ms)| SyncUnmanagedFile {
                        path,
                        size_bytes,
                        modified_ms,
                    })
                    .collect(),
            };
            Ok(SyncUnmanaged {
                total: files.len() as u64,
                offset: params.offset,
                files: files
                    .into_iter()
                    .skip(params.offset as usize)
                    .take(params.limit.clamp(1, 500) as usize)
                    .collect(),
            })
        })
    }

    pub(super) fn devices_plays(
        &self,
        stores: &Stores,
        params: &SyncPageParams,
    ) -> Result<SyncPlays, RpcError> {
        // No family built yet gives plays back.
        Self::library(stores, |library| Self::target(library, params.device_id))?;
        Ok(SyncPlays {
            total: 0,
            offset: params.offset,
            plays: Vec::new(),
        })
    }

    fn device_receipt(key: Id, device: Id, rev: i64) -> String {
        // These few fields always write.
        serde_json::to_string(&SyncDeviceChanged {
            receipt: Receipt {
                command_id: key,
                status: ReceiptStatus::Done,
                revision: Some(revision(rev)),
            },
            device_id: device,
        })
        .unwrap_or_default()
    }

    pub(super) fn devices_set(
        &self,
        stores: &Stores,
        params: &SyncSetParams,
    ) -> Result<SyncDeviceChanged, RpcError> {
        let (key, device) = (params.idempotency_key, params.device_id);
        let request = WriteRequest {
            method: DevicesSet::NAME,
            command: Some(Command {
                id: key,
                fingerprint: fingerprint::<DevicesSet>(params)?,
            }),
            client: self.client(),
        };
        let changes = TargetChanges {
            name: params.name.clone(),
            icon_key: params
                .icon_key
                .clone()
                .map(|key| Some(key).filter(|key| !key.is_empty())),
            ignored: params.ignored,
            sources: params.sources.clone(),
            format: params.format.map(FormatPolicy::from),
            cover_px: params.cover_px,
            id3v1: params.id3v1,
            reserve_bytes: params
                .reserve_bytes
                .map(|bytes| Some(bytes).filter(|b| *b > 0)),
            limit_bytes: params
                .limit_bytes
                .map(|bytes| Some(bytes).filter(|b| *b > 0)),
            when_full: params.when_full,
            on_connect: params.on_connect,
            made_for: params.made_for,
            folder_kind: params.folder_kind,
            codecs: params
                .codecs
                .as_ref()
                .map(|codecs| {
                    codecs
                        .iter()
                        .map(|codec| {
                            Codec::named(codec).ok_or_else(|| {
                                RpcError::new(ErrorCode::Invalid, "a codec is flac, mp3 or opus")
                                    .with_detail("field", "codecs")
                            })
                        })
                        .collect::<Result<Vec<_>, _>>()
                })
                .transpose()?,
            playlists: params.playlists,
        };
        let written = Self::library(stores, |library| {
            // What goes on a target is not changed under a sync of it.
            let target = Self::target(library, device)?;
            if target.settings.folder.is_none()
                && (changes.made_for.is_some() || changes.folder_kind.is_some())
            {
                return Err(RpcError::new(
                    ErrorCode::Invalid,
                    "only a folder is made for something",
                ));
            }
            let plans = changes.sources.is_some()
                || changes.format.is_some()
                || changes.made_for.is_some()
                || changes.codecs.is_some();
            if plans
                && library
                    .sync_open_run(&target)
                    .map_err(in_library)?
                    .is_some()
            {
                return Err(busy());
            }
            library
                .sync_set(request, device, changes, move |rev| {
                    Self::device_receipt(key, device, rev)
                })
                .map_err(in_library)
        })?;
        if !written.replayed {
            self.device_changed(
                device,
                &[SyncDeviceChange::Settings, SyncDeviceChange::Plan],
            );
        }
        stored(DevicesSet::NAME, &written.result_json)
    }

    /// `devices.addFolder`: a folder on this server as a target. Its path
    /// goes in this server's `server.db` and the library gets an opaque
    /// root ID, as `sources.add` records a root: this server's file first,
    /// and the call sent again finishes it.
    pub(super) fn devices_add_folder(
        &self,
        stores: &Stores,
        params: &SyncAddFolderParams,
    ) -> Result<SyncDeviceChanged, RpcError> {
        let key = params.idempotency_key;
        let command = Command {
            id: key,
            fingerprint: fingerprint::<DevicesAddFolder>(params)?,
        };
        let replayed = Self::library(stores, |library| {
            library.replay(&command).map_err(in_library)
        })?;
        if let Some(written) = replayed {
            return stored(DevicesAddFolder::NAME, &written.result_json);
        }
        let invalid = |field: &str, why: String| {
            RpcError::new(ErrorCode::Invalid, why).with_detail("field", field)
        };
        let name = params.name.trim().to_owned();
        if name.is_empty() || name.chars().count() > 100 {
            return Err(invalid(
                "name",
                "a device's name is 1 to 100 characters".into(),
            ));
        }
        let given = Path::new(&params.path);
        if !given.is_absolute() {
            return Err(invalid("path", "a folder's path is absolute".into()));
        }
        let folder = std::fs::canonicalize(given)
            .ok()
            .filter(|folder| folder.is_dir())
            .ok_or_else(|| {
                invalid(
                    "path",
                    "there is no folder at that path on this server".into(),
                )
            })?;
        // A sync writes and removes files: never among the library's own,
        // the server's data, or another target's.
        let lies = |other: &Path, what: &str| match sync::overlap(&folder, other) {
            Some(Overlap::Same) => Some(format!("This folder is {what}.")),
            Some(Overlap::Inside) => Some(format!("This folder is inside {what}.")),
            Some(Overlap::Holds) => Some(format!("This folder holds {what}.")),
            None => None,
        };
        let mut refused = lies(self.dir.root(), "the server's own data");
        for (root, path) in stores.server().roots().map_err(internal)? {
            refused =
                refused.or_else(|| lies(Path::new(&path), &format!("the library folder {root}")));
        }
        let taken = stores.server().sync_roots().map_err(internal)?;
        let live: HashSet<Id> = Self::library(stores, |library| {
            Ok(library
                .sync_targets()
                .map_err(in_library)?
                .into_iter()
                .filter_map(|target| target.root)
                .collect())
        })?;
        for (root, path) in &taken {
            // A path a forgotten target left behind holds no target now.
            if live.contains(root) {
                refused = refused.or_else(|| lies(Path::new(path), "another device's folder"));
            }
        }
        if let Some(why) = refused {
            return Err(invalid(
                "path",
                format!("{why} Choose a separate folder to sync to."),
            ));
        }
        let request = WriteRequest {
            method: DevicesAddFolder::NAME,
            command: Some(command),
            client: self.client(),
        };
        // The command's own ID names the root, so a call sent again names
        // the same one.
        let (root, host) = (key, self.server_id);
        let limit = params.limit_bytes.filter(|limit| *limit > 0);
        let made_for = params.made_for.unwrap_or(SyncMadeFor::PlainCopy);
        let kind = params.kind.unwrap_or(SyncFolderKind::AlwaysHere);
        let path = folder.to_string_lossy().into_owned();
        let written = Self::library(stores, |library| {
            stores
                .server()
                .add_sync_root(root, &path)
                .map_err(internal)?;
            library
                .sync_add_folder(
                    request,
                    name,
                    root,
                    host,
                    limit,
                    made_for,
                    kind,
                    move |rev, device| Self::device_receipt(key, device, rev),
                )
                .map_err(in_library)
        })?;
        let changed: SyncDeviceChanged = stored(DevicesAddFolder::NAME, &written.result_json)?;
        if !written.replayed {
            self.device_changed(changed.device_id, &[SyncDeviceChange::State]);
        }
        Ok(changed)
    }

    pub(super) fn devices_forget(
        &self,
        stores: &Stores,
        params: &SyncDeviceCommandParams,
    ) -> Result<SyncDeviceChanged, RpcError> {
        let (key, device) = (params.idempotency_key, params.device_id);
        let command = Command {
            id: key,
            fingerprint: fingerprint::<DevicesForget>(params)?,
        };
        let replayed = Self::library(stores, |library| {
            library.replay(&command).map_err(in_library)
        })?;
        if let Some(written) = replayed {
            return stored(DevicesForget::NAME, &written.result_json);
        }
        let request = WriteRequest {
            method: DevicesForget::NAME,
            command: Some(command),
            client: self.client(),
        };
        let (written, target) = Self::library(stores, |library| {
            let target = Self::target(library, device)?;
            if library
                .sync_open_run(&target)
                .map_err(in_library)?
                .is_some()
            {
                return Err(busy());
            }
            // The marker goes from a target that is here: plugged in again
            // it is a new one, whose files are not Ritornel's.
            if let Ok(mut session) = self.open_device(stores, &target) {
                let ours = session
                    .marker()
                    .ok()
                    .flatten()
                    .and_then(|bytes| Marker::read(&bytes))
                    .is_some_and(|marker| marker.target == target.id);
                if ours {
                    let _ = session.set_marker(None);
                }
            }
            let written = library
                .sync_forget(request, device, move |rev| {
                    Self::device_receipt(key, device, rev)
                })
                .map_err(in_library)?;
            Ok((written, target))
        })?;
        if let Some(root) = target.root {
            stores.server().remove_sync_root(root).map_err(internal)?;
        }
        lock(&self.devices.live).remove(&device);
        self.device_changed(device, &[SyncDeviceChange::State]);
        stored(DevicesForget::NAME, &written.result_json)
    }

    /// `devices.eject`: makes a target safe to unplug, and answers only
    /// once it is. It changes nothing the library records, so it has no
    /// receipt to answer a second time from: sent again, it ejects again.
    pub(super) fn devices_eject(
        &self,
        stores: &Stores,
        params: &SyncDeviceCommandParams,
    ) -> Result<SyncDeviceChanged, RpcError> {
        fingerprint::<DevicesEject>(params)?;
        let session = Self::library(stores, |library| {
            let target = Self::target(library, params.device_id)?;
            if library
                .sync_open_run(&target)
                .map_err(in_library)?
                .is_some()
            {
                return Err(busy());
            }
            if !capabilities_of(&target).ejectable {
                return Err(RpcError::new(
                    ErrorCode::Unsupported,
                    "a folder that is always here is not ejected: it is safe as soon as a sync has finished",
                )
                .with_detail("reason", "not_ejectable"));
            }
            self.open_device(stores, &target).map_err(|problem| {
                RpcError::new(ErrorCode::Unavailable, "the device is not here")
                    .with_detail("reason", problem)
            })
        })?;
        session.close(true).map_err(|error| {
            let code = match error.kind {
                ritornel_device::ErrorKind::Unsupported => ErrorCode::Unsupported,
                _ => ErrorCode::Unavailable,
            };
            RpcError::new(code, error.detail).with_detail("reason", "eject_failed")
        })?;
        self.device_changed(params.device_id, &[SyncDeviceChange::State]);
        Ok(SyncDeviceChanged {
            receipt: Receipt {
                command_id: params.idempotency_key,
                status: ReceiptStatus::Done,
                revision: None,
            },
            device_id: params.device_id,
        })
    }

    /// `devices.sync`: starts the job that carries a sync out. What the
    /// run itself must refuse, a plan that has moved or a cut not agreed
    /// to, it refuses once it has read the target, and its run says why.
    pub(super) fn devices_sync(
        &self,
        stores: &Stores,
        params: &SyncStartParams,
        show_paths: bool,
    ) -> Result<SyncStarted, RpcError> {
        let key = params.idempotency_key;
        let command = Command {
            id: key,
            fingerprint: fingerprint::<DevicesSync>(params)?,
        };
        let started = |job: &ritornel_engine::Job| SyncStarted {
            receipt: Receipt {
                command_id: key,
                status: ReceiptStatus::Accepted,
                revision: None,
            },
            job: super::calls::job_for(job, show_paths),
        };
        if let Some(job) = stores.server().job_of_command(command).map_err(internal)? {
            return Ok(started(&job));
        }
        Self::library(stores, |library| {
            let target = Self::target(library, params.device_id)?;
            if target.setup == SyncSetup::Ignored {
                return Err(RpcError::new(
                    ErrorCode::Invalid,
                    "this device is ignored: stop ignoring it to sync it",
                ));
            }
            if library
                .sync_open_run(&target)
                .map_err(in_library)?
                .is_some()
            {
                return Err(busy());
            }
            let (planned, problem) = self.planned(stores, library, &target, Use::Read)?;
            if let Some(problem) = problem {
                return Err(
                    RpcError::new(ErrorCode::Unavailable, "the device is not here")
                        .with_detail("reason", problem),
                );
            }
            let planned = planned
                .ok_or_else(|| RpcError::new(ErrorCode::Unavailable, "the device has no plan"))?;
            // First contact is explicit: a target that is not Ritornel's,
            // or whose marker is gone, is written to only when asked.
            if !planned.adopted && !params.adopt {
                return Err(RpcError::new(
                    ErrorCode::Invalid,
                    "this device is not set up: set it up to sync it",
                )
                .with_detail("reason", "adopt_required"));
            }
            if planned.plan.revision != params.plan_revision {
                return Err(RpcError::new(
                    ErrorCode::PlanStale,
                    "the plan has changed: read the device again",
                ));
            }
            if !planned.plan.fits
                && target.settings.when_full == SyncWhenFull::Ask
                && !params.accept_cut
            {
                return Err(RpcError::new(
                    ErrorCode::Invalid,
                    "not everything fits: agree to what is left out, or change what goes on",
                )
                .with_detail("reason", "cut_not_accepted"));
            }
            Ok(())
        })?;
        let json = serde_json::to_string(params).map_err(internal)?;
        let (job, created) = stores
            .server()
            .start_job(
                command,
                DevicesSync::NAME,
                NewJob {
                    kind: SYNC_JOB,
                    dedupe_key: params.device_id.to_string(),
                    params_json: json,
                    target_kind: "device",
                    target: params.device_id,
                    session: self.local_credential.session(),
                },
            )
            .map_err(|error| super::failed(crate::Error::Store(error)))?;
        if created {
            self.publish_stored_job(stores, &job);
        }
        self.jobs.wake();
        Ok(started(&job))
    }

    /// Closes the runs a dead run of the server left open, once at start:
    /// a target is never left busy by a run no one holds.
    pub(crate) fn close_stale_syncs(&self) {
        match self.with_library(|_, library| library.sync_close_stale_runs()) {
            Some(Ok(closed)) if closed > 0 => {
                tracing::warn!(closed, "closed syncs the last run of the server left open")
            }
            Some(Err(error)) => tracing::warn!(%error, "could not close stale syncs"),
            _ => {}
        }
    }
}

/// How a row of a run under way reads before it is settled.
fn working(state: SyncActionState, title: &str, artist: &str, seq: i64, bytes: u64) -> SyncRunFile {
    SyncRunFile {
        action_id: seq.to_string(),
        title: title.to_owned(),
        artist: artist.to_owned(),
        state,
        bytes_done: 0,
        bytes,
    }
}
