//! The scan job: walk a root, read what is new or changed, record it, mark
//! what is gone, and resolve the library. See "Scanning and audio hashing"
//! and "Rescans and file moves" in docs/internals/identity.md, and "Jobs and
//! outbox" in docs/internals/server.md.
//!
//! A [`ScanRun`] is one claimed job, advanced a step at a time. Each step
//! is two halves, which is the note's seam between where the file is and
//! the hub: [`ScanRun::prepare`] touches files and no database, and
//! [`ScanRun::commit`] touches the databases and no file. No transaction is
//! open while a file is read.
//!
//! Every library write of the job is one step, and its command ID is
//! derived from the job's ID and the step's number. The library is the
//! checkpoint that matters: a file that was recorded has its size and time
//! stored, so a job that starts again walks the root, finds those files
//! unchanged, and reads only what is left. A step that committed just
//! before a crash, and whose number was never saved, is found by its
//! command ID and counted as done, and its counters are read from what it
//! stored as its result ([`StepResult`]). Once the resolver's step has
//! committed, nothing is walked again, and once the disc layouts' step has,
//! the scan is finished.
//!
//! The resolver runs once, at the end, over the whole library, and only if
//! the scan changed something. So a rescan that finds nothing changed
//! writes nothing at all, and takes no library revision.
//!
//! The sidecars found with the audio, such as rip logs and CUE sheets, are
//! read after the audio and before the resolver, and recorded in steps of
//! their own, which do not make the resolver run. The disc layouts are
//! derived from them last, when a write since the last derivation said they
//! were due (docs/internals/corrections.md, "Disc layouts").

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;

use ritornel_contract::ScanSummary;
use ritornel_domain::layout;
use ritornel_domain::payload::{FolderImage, Payload};
use ritornel_domain::{Facts, Policy, Replacement, extract, judge_replacements};
use serde::{Deserialize, Serialize};

use crate::evidence_store::{KnownSidecar, LAYOUTS_METHOD, SIDECARS_METHOD, SidecarOp};
use crate::jobs::{Job, Progress, step_command};
use crate::media::{UNAVAILABLE, read_file, read_folder_image};
use crate::scan_store::{Known, RecordOp, Source};
use crate::walk::{Seen, Walked, folder_id, folder_of, under, walk};
use crate::{Command, Error, Fingerprint, LibraryStore, Resolved, ServerStore, WriteRequest};

/// The most sidecar bytes one step reads and records.
const SIDECAR_BATCH_BYTES: usize = 8 << 20;

/// How a scan runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanSettings {
    /// Files read and written in one step: one library transaction.
    pub batch_files: usize,
    /// Threads that read and hash files.
    pub threads: usize,
    /// A fail point for crash tests: the scan stops with
    /// [`Error::FailPoint`] straight after the library commit of this
    /// step, before the step's checkpoint is saved.
    pub fail_after_step: Option<i64>,
}

impl Default for ScanSettings {
    fn default() -> Self {
        Self {
            batch_files: 500,
            threads: std::thread::available_parallelism().map_or(4, usize::from),
            fail_after_step: None,
        }
    }
}

/// What a scan saves with each step, to carry on from.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Checkpoint {
    #[serde(default)]
    summary: ScanSummary,
    /// Whether the job has recorded something the resolver has not seen.
    #[serde(default)]
    dirty: bool,
}

/// What a step of the scan's own stores as its result in the library, in
/// the transaction that commits it: the checkpoint as it is once the step
/// has committed. The checkpoint in `server.db` is saved after, and a crash
/// between the two leaves this to carry on from. The resolver's step stores
/// what it wrote ([`Resolved`]) instead.
#[derive(Debug, Serialize, Deserialize)]
struct StepResult {
    checkpoint: Checkpoint,
    /// Whether the step is the scan's last: the disc layouts'.
    #[serde(default)]
    last: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Walk,
    Read,
    Moves,
    Missing,
    Replaced,
    Sidecars,
    Resolve,
    /// The tracks other services named are judged against what the scan
    /// made: after a resolve that wrote anything, and after a scan taken up
    /// again whose resolve had committed.
    Match,
    Layouts,
    Done,
}

/// One scan job in progress.
#[derive(Debug)]
pub struct ScanRun {
    job: Job,
    source: Source,
    root: PathBuf,
    settings: ScanSettings,
    phase: Phase,
    step: i64,
    summary: ScanSummary,
    dirty: bool,
    walked: Option<Walked>,
    known: HashMap<String, Known>,
    /// Items a file may have moved from, by audio hash: those whose path
    /// the walk did not find, and those whose path now holds other audio.
    /// A file at a new path is matched as it is read, and one at a known
    /// path once every file is read ([`ScanRun::settle_moves`]).
    gone_by_audio: HashMap<String, Vec<i64>>,
    gone: Vec<i64>,
    /// The files to read, and how far the reading has got.
    queue: Vec<Seen>,
    cursor: usize,
    batch: Vec<Payload>,
    images: HashMap<String, Vec<FolderImage>>,
    /// Files whose audio changed at a known path, held until every file is
    /// read: each is then a move, or is judged by the resolver.
    replaced: Vec<(i64, Payload)>,
    /// The sidecars the library has for this source, by path.
    known_sidecars: HashMap<String, KnownSidecar>,
    /// The sidecar files to read: new, or changed since they were read.
    sidecar_queue: Vec<Seen>,
    /// What to record of the sidecars.
    sidecar_ops: Vec<SidecarOp>,
    failed: Vec<(String, &'static str, String)>,
    label: Option<String>,
}

fn unsigned(number: i64) -> u64 {
    u64::try_from(number).unwrap_or(0)
}

/// Reads files on several threads, and tells `read` of each as it is done.
/// `None` if it was asked to stop.
fn read_batch(
    root: &Path,
    files: &[Seen],
    images: &HashMap<String, Vec<FolderImage>>,
    threads: usize,
    stop: &AtomicBool,
    read: &mut dyn FnMut(&Payload),
) -> Option<Vec<Payload>> {
    let next = AtomicUsize::new(0);
    let (send, done) = mpsc::channel::<(usize, Payload)>();
    let mut payloads: Vec<(usize, Payload)> = Vec::with_capacity(files.len());
    std::thread::scope(|scope| {
        for _ in 0..threads.clamp(1, files.len().max(1)) {
            let (send, next) = (send.clone(), &next);
            scope.spawn(move || {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(file) = files.get(index).filter(|_| !stop.load(Ordering::Relaxed))
                    else {
                        break;
                    };
                    let folder = images
                        .get(folder_of(&file.path))
                        .cloned()
                        .unwrap_or_default();
                    let payload = read_file(root, &file.path, file.size, file.mtime_ms, folder);
                    if send.send((index, payload)).is_err() {
                        break;
                    }
                }
            });
        }
        // The channel closes when every reader has finished.
        drop(send);
        for (index, payload) in done {
            read(&payload);
            payloads.push((index, payload));
        }
    });
    if stop.load(Ordering::Relaxed) || payloads.len() != files.len() {
        return None;
    }
    payloads.sort_by_key(|(index, _)| *index);
    Some(payloads.into_iter().map(|(_, payload)| payload).collect())
}

impl ScanRun {
    /// Takes up a claimed scan job, from the start or from where an
    /// earlier run of it stopped.
    pub fn open(
        job: Job,
        server: &ServerStore,
        library: &LibraryStore,
        settings: ScanSettings,
    ) -> Result<Self, Error> {
        let source = job
            .target
            .map(|target| library.source(target))
            .transpose()?
            .flatten()
            .ok_or(Error::NoSuchSource)?;
        let root = server
            .roots()?
            .into_iter()
            .find(|(name, _)| *name == source.name)
            .map(|(_, path)| PathBuf::from(path))
            .ok_or_else(|| Error::NoRootPath {
                name: source.name.clone(),
            })?;
        let checkpoint: Checkpoint = job
            .checkpoint_json
            .as_deref()
            .and_then(|json| serde_json::from_str(json).ok())
            .unwrap_or_default();
        // A step that committed and whose checkpoint was never saved is
        // found by its command ID, and is not done again. What it counted
        // is in what it stored. A step an older build wrote stored only how
        // many rows it wrote, and its counts are not known.
        let (mut step, mut summary, mut dirty) = (job.step, checkpoint.summary, checkpoint.dirty);
        while let Some(result) = library.command_result(step_command(job.public_id, step + 1))? {
            step += 1;
            dirty = true;
            if let Ok(stored) = serde_json::from_str::<StepResult>(&result) {
                summary = stored.checkpoint.summary;
                dirty = stored.checkpoint.dirty;
            } else if let Ok(resolved) = serde_json::from_str::<Resolved>(&result) {
                summary.assignments += resolved.assignments;
                summary.recordings += resolved.recordings;
                summary.releases += resolved.releases;
            }
        }
        // Everything that needs the root is done before the resolver's step,
        // and the layouts' is the scan's last. Once the resolver's has
        // committed, nothing is walked again: a root that is offline now
        // does not fail a scan that was done. Once the layouts' has, the
        // scan is finished.
        let last = match step {
            0 => None,
            _ => library.command_result(step_command(job.public_id, step))?,
        };
        let resolved = last
            .as_deref()
            .is_some_and(|result| serde_json::from_str::<Resolved>(result).is_ok());
        let finished = last
            .as_deref()
            .and_then(|result| serde_json::from_str::<StepResult>(result).ok())
            .is_some_and(|stored| stored.last);
        if resolved || finished {
            dirty = false;
        }
        Ok(Self {
            job,
            source,
            root,
            settings,
            // A scan whose resolve committed may have stopped before it
            // judged the tracks; judging them again changes nothing if it
            // did not. One whose layouts' step committed is finished.
            phase: if finished {
                Phase::Done
            } else if resolved {
                Phase::Match
            } else {
                Phase::Walk
            },
            step,
            summary,
            dirty,
            walked: None,
            known: HashMap::new(),
            gone_by_audio: HashMap::new(),
            gone: Vec::new(),
            queue: Vec::new(),
            cursor: 0,
            batch: Vec::new(),
            images: HashMap::new(),
            replaced: Vec::new(),
            known_sidecars: HashMap::new(),
            sidecar_queue: Vec::new(),
            sidecar_ops: Vec::new(),
            failed: Vec::new(),
            label: None,
        })
    }

    /// The job this run is of.
    pub fn job(&self) -> &Job {
        &self.job
    }

    /// The half of a step that touches files and no database: the walk, or
    /// reading the next batch. It returns `false` if it was asked to stop
    /// part-way; nothing is kept of a step that stopped, and the caller
    /// does not commit it.
    ///
    /// `tick` is called after every file read, with the job's progress as
    /// it then is. The caller decides how often to save it: about once a
    /// second.
    pub fn prepare(
        &mut self,
        stop: &AtomicBool,
        tick: &mut dyn FnMut(Progress),
    ) -> Result<bool, Error> {
        match self.phase {
            Phase::Walk => {
                self.label = Some(format!("walking {}", self.source.name));
                self.walked = Some(walk(&self.root, stop)?);
            }
            Phase::Read => {
                let end = (self.cursor + self.settings.batch_files.max(1)).min(self.queue.len());
                let files = self.queue.get(self.cursor..end).unwrap_or(&[]);
                self.label = files.first().map(|file| file.path.clone());
                // A folder's own images are read once for all its files.
                let walked = self.walked.as_ref();
                for file in files {
                    let folder = folder_of(&file.path);
                    if self.images.contains_key(folder) {
                        continue;
                    }
                    let found = walked.and_then(|walked| walked.images.get(folder));
                    let images = found
                        .into_iter()
                        .flatten()
                        .filter_map(|image| {
                            let name = image.path.rsplit('/').next().unwrap_or(&image.path);
                            read_folder_image(&under(&self.root, &image.path), name, image.mtime_ms)
                        })
                        .collect();
                    self.images.insert(folder.to_owned(), images);
                }
                let mut progress = self.progress();
                let mut read = |payload: &Payload| {
                    progress.done += 1;
                    progress.bytes += i64::try_from(payload.size).unwrap_or(0);
                    progress.label = Some(payload.path.clone());
                    tick(progress.clone());
                };
                let batch = read_batch(
                    &self.root,
                    files,
                    &self.images,
                    self.settings.threads,
                    stop,
                    &mut read,
                );
                let Some(batch) = batch else {
                    return Ok(false);
                };
                self.batch = batch;
            }
            Phase::Sidecars => self.read_sidecars(stop),
            _ => {}
        }
        // What a batch read, and which files the walk found gone or other
        // audio at, are of the folder that was walked. A root that is no
        // longer that folder, as when its disk went away or another was
        // mounted at its path, ends the scan: nothing more is recorded, and
        // nothing is taken for gone or replaced.
        let judged = matches!(
            self.phase,
            Phase::Read | Phase::Moves | Phase::Missing | Phase::Replaced | Phase::Sidecars
        );
        let walked_id = self.walked.as_ref().and_then(|walked| walked.root_id);
        if judged && folder_id(&self.root) != walked_id {
            return Err(Error::RootOffline {
                path: self.root.clone(),
            });
        }
        Ok(!stop.load(Ordering::Relaxed))
    }

    /// The request of the job's next library write: its next step.
    fn next_step(&mut self) -> WriteRequest {
        self.next_step_as("scan.step")
    }

    /// The request of the job's next library write, as `method`.
    fn next_step_as(&mut self, method: &'static str) -> WriteRequest {
        self.step += 1;
        WriteRequest {
            method,
            command: Some(Command {
                id: step_command(self.job.public_id, self.step),
                fingerprint: Fingerprint::of(method, &self.step.to_be_bytes()),
            }),
            client: None,
        }
    }

    /// What a step of the scan's own stores as its result: the checkpoint
    /// once it has committed, with `summary` as its counters.
    fn step_result(&self, summary: ScanSummary) -> Result<String, Error> {
        stored(summary, true, false)
    }

    /// What follows every library commit of the job that the resolver
    /// must see.
    fn committed(&mut self) -> Result<(), Error> {
        self.dirty = true;
        self.fail_point()
    }

    /// The fail point, after any library commit of the job.
    fn fail_point(&self) -> Result<(), Error> {
        if self.settings.fail_after_step == Some(self.step) {
            return Err(Error::FailPoint);
        }
        Ok(())
    }

    /// The half of a step that touches the databases and no file. It
    /// returns the summary once the scan is finished.
    pub fn commit(&mut self, library: &LibraryStore) -> Result<Option<ScanSummary>, Error> {
        match self.phase {
            Phase::Walk => self.sort_walk(library)?,
            Phase::Read => self.record(library)?,
            Phase::Moves => {
                self.phase = Phase::Missing;
                let ops = self.settle_moves();
                if !ops.is_empty() {
                    let request = self.next_step();
                    let receipt = self.step_result(self.summary)?;
                    library.record(request, self.source.row, ops, receipt)?;
                    self.committed()?;
                }
            }
            Phase::Missing => {
                // What the walk did not find and no file moved from. The
                // root was reachable, or the walk would have failed.
                // A job that started again adds to what it marked before,
                // which its walk now finds missing already.
                let gone: Vec<i64> = std::mem::take(&mut self.gone);
                self.summary.missing += gone.len() as u64;
                self.phase = Phase::Replaced;
                if !gone.is_empty() {
                    let request = self.next_step();
                    let receipt = self.step_result(self.summary)?;
                    library.mark_missing(request, gone, receipt)?;
                    self.committed()?;
                }
            }
            Phase::Replaced => self.judge_replaced(library)?,
            Phase::Sidecars => {
                if self.sidecar_queue.is_empty() {
                    self.phase = Phase::Resolve;
                }
                let ops = std::mem::take(&mut self.sidecar_ops);
                if !ops.is_empty() {
                    let request = self.next_step_as(SIDECARS_METHOD);
                    let receipt = stored(self.summary, self.dirty, false)?;
                    library.record_sidecars(request, self.source.row, ops, receipt)?;
                    self.fail_point()?;
                }
            }
            Phase::Resolve => {
                if self.dirty || library.needs_resolving()? {
                    self.label = Some("resolving".to_owned());
                    if library.claims_are_stale()? {
                        let request = self.next_step();
                        library.rebuild_claims(request)?;
                        self.committed()?;
                    }
                    let request = self.next_step();
                    match library.resolve(request)? {
                        Some((_, resolved)) => {
                            self.summary.assignments += resolved.assignments;
                            self.summary.recordings += resolved.recordings;
                            self.summary.releases += resolved.releases;
                            self.committed()?;
                            // What the library holds changed: the tracks other
                            // services named are judged again next, so that
                            // a record bought has its plays at once.
                            self.phase = Phase::Match;
                        }
                        // Nothing was written, so the step was not taken.
                        None => {
                            self.step -= 1;
                            self.phase = Phase::Layouts;
                        }
                    }
                    self.dirty = false;
                } else {
                    self.phase = Phase::Layouts;
                }
                self.label = None;
            }
            Phase::Match => {
                // A write of its own, which writes nothing when no binding
                // moves, and so is safe to repeat.
                library.match_library()?;
                self.phase = Phase::Layouts;
            }
            Phase::Layouts => {
                // Due whenever a write since the last derivation said so,
                // in this job or one that stopped before it got here.
                if library.layouts_are_stale()? {
                    self.label = Some("reading disc layouts".to_owned());
                    let request = self.next_step_as(LAYOUTS_METHOD);
                    let receipt = stored(self.summary, self.dirty, true)?;
                    library.derive_layouts(request, receipt)?;
                    self.fail_point()?;
                }
                self.phase = Phase::Done;
                self.label = None;
                return Ok(Some(self.summary));
            }
            Phase::Done => return Ok(Some(self.summary)),
        }
        Ok(None)
    }

    /// Compares the walk with what the library holds: which files to read,
    /// and which items' files are gone.
    fn sort_walk(&mut self, library: &LibraryStore) -> Result<(), Error> {
        let Some(walked) = &self.walked else {
            return Err(Error::Corrupt("a scan with no walk"));
        };
        self.known = library.known_items(self.source.row)?;
        let mut present: HashSet<&str> = HashSet::with_capacity(walked.files.len());
        let mut unchanged = 0;
        for file in &walked.files {
            present.insert(file.path.as_str());
            let images: Vec<(String, u64, i64)> = walked
                .images
                .get(folder_of(&file.path))
                .into_iter()
                .flatten()
                .map(|image| {
                    let name = image.path.rsplit('/').next().unwrap_or(&image.path);
                    (name.to_owned(), image.size, image.mtime_ms)
                })
                .collect();
            // A known path with the same size and time is skipped, unless
            // its folder's image changed.
            let same = self.known.get(&file.path).is_some_and(|known| {
                !known.missing
                    && !known.io_failed
                    && unsigned(known.size) == file.size
                    && known.mtime_ms == file.mtime_ms
                    && known.images == images
            });
            if same {
                unchanged += 1;
            } else {
                self.queue.push(file.clone());
            }
        }
        // A file in a folder the walk could not read is neither there nor
        // gone: it is left as it was.
        let unread = |path: &str| {
            walked.unreadable.iter().any(|folder| {
                path.strip_prefix(folder.as_str())
                    .is_some_and(|rest| rest.starts_with('/'))
            })
        };
        for (path, known) in &self.known {
            if present.contains(path.as_str()) || unread(path) {
                continue;
            }
            if let Some(audio) = &known.audio {
                self.gone_by_audio
                    .entry(audio.clone())
                    .or_default()
                    .push(known.id);
            }
            if !known.missing {
                self.gone.push(known.id);
            }
        }
        self.gone.sort_unstable();
        // Known paths are read first, so that by the time a new path is
        // recorded, every item whose path now holds other audio is known.
        // Files at known paths are matched once all are read.
        let known = &self.known;
        self.queue
            .sort_by_key(|file| !known.contains_key(&file.path));
        for (path, why) in &walked.skipped {
            self.failed
                .push((path.clone(), "skipped", format!("{{\"reason\":\"{why}\"}}")));
        }
        self.sort_sidecars(library)?;
        let Some(walked) = &self.walked else {
            return Err(Error::Corrupt("a scan with no walk"));
        };
        self.summary.files = walked.files.len() as u64;
        self.summary.unchanged = unchanged;
        self.phase = if self.queue.is_empty() {
            Phase::Moves
        } else {
            Phase::Read
        };
        Ok(())
    }

    /// Records the batch that was read, in one library transaction.
    fn record(&mut self, library: &LibraryStore) -> Result<(), Error> {
        let batch = std::mem::take(&mut self.batch);
        let mut ops = Vec::with_capacity(batch.len());
        let mut counted = self.summary;
        for payload in batch {
            if let Some(error) = payload
                .error
                .as_ref()
                .filter(|error| error.kind == UNAVAILABLE)
            {
                // Nothing is recorded: the file keeps what was last read of
                // it, and its size and time, so the next scan reads it.
                let detail = serde_json::to_string(error)?;
                self.failed.push((payload.path.clone(), "skipped", detail));
                continue;
            }
            let audio = payload.audio.as_ref().map(|audio| audio.hash.clone());
            // Read again only because its last read failed on I/O, and it
            // still cannot be read: there is nothing new to record.
            let still = self.known.get(&payload.path).is_some_and(|known| {
                known.io_failed
                    && !known.missing
                    && audio.is_none()
                    && unsigned(known.size) == payload.size
                    && known.mtime_ms == payload.mtime_ms
            });
            if still {
                counted.unchanged += 1;
                continue;
            }
            counted.bytes_read += payload.size;
            if audio.is_none() {
                counted.unreadable += 1;
                let detail = payload
                    .error
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()?;
                self.failed
                    .push((payload.path.clone(), "failed", detail.unwrap_or_default()));
            }
            // The item's own file: the same audio under other tags, a file
            // that is back, or one that cannot be read now. After a failed
            // read, it is the only audio the item has had. An item that has
            // had none is judged, as another item's file may have moved
            // onto its path.
            let own = |known: &Known| match (&known.audio, &audio) {
                (_, None) => true,
                (Some(had), Some(now)) => had == now,
                (None, Some(now)) => {
                    !known.earlier_audio.is_empty()
                        && known.earlier_audio.iter().all(|had| had == now)
                }
            };
            match self.known.get(&payload.path) {
                Some(known) if own(known) => {
                    if known.missing {
                        counted.returned += 1;
                    } else {
                        counted.changed += 1;
                    }
                    ops.push(RecordOp::Update {
                        item: known.id,
                        payload,
                    });
                }
                // Other audio at a known path. Whether it is the same song
                // is the resolver's to say, once everything is recorded,
                // unless the item's own file turns up at a new path.
                Some(known) => {
                    if let Some(audio) = &known.audio {
                        self.gone_by_audio
                            .entry(audio.clone())
                            .or_default()
                            .push(known.id);
                    }
                    self.replaced.push((known.id, payload));
                }
                None => self.place(payload, &mut counted, &mut ops),
            }
        }
        self.cursor = (self.cursor + self.settings.batch_files.max(1)).min(self.queue.len());
        if self.cursor >= self.queue.len() {
            self.phase = Phase::Moves;
        }
        self.summary.bytes_read = counted.bytes_read;
        if !ops.is_empty() {
            let request = self.next_step();
            let receipt = self.step_result(counted)?;
            library.record(request, self.source.row, ops, receipt)?;
            self.summary = counted;
            self.committed()?;
        }
        Ok(())
    }

    /// Records a file at a path the library has no live item at. If its
    /// audio matches exactly one item of this root whose path is gone, or
    /// now holds other audio, the file moved and the item follows it. An
    /// item that leaves a path with other audio in it leaves that audio to
    /// be placed in its turn, as a file at a new path, and so on along a
    /// chain of files that each moved into the last one's place.
    fn place(&mut self, payload: Payload, counted: &mut ScanSummary, ops: &mut Vec<RecordOp>) {
        let mut next = Some(payload);
        while let Some(payload) = next.take() {
            let moved = payload
                .audio
                .as_ref()
                .and_then(|audio| self.gone_by_audio.get_mut(&audio.hash))
                .filter(|items| items.len() == 1)
                .and_then(Vec::pop);
            let Some(item) = moved else {
                counted.new += 1;
                ops.push(RecordOp::New(payload));
                break;
            };
            self.gone.retain(|gone| *gone != item);
            counted.moved += 1;
            // The move re-keys the item before anything takes its old path.
            ops.push(RecordOp::Move { item, payload });
            if let Some(at) = self
                .replaced
                .iter()
                .position(|(replaced, _)| *replaced == item)
            {
                next = Some(self.replaced.swap_remove(at).1);
            }
        }
    }

    /// Settles the files at known paths that hold other audio, once every
    /// file has been read. Where that audio is one gone item's, and no
    /// other such file holds it, the item moved here: files that swapped
    /// paths, or moved round a ring, each take another's place, and an item
    /// another's file moved onto, whose own file this scan found nowhere,
    /// is retired with everything it has, as when a different song takes a
    /// path. A file at a path whose item moved away is a new item, as in
    /// [`place`](Self::place). The rest are judged as replaced audio.
    fn settle_moves(&mut self) -> Vec<RecordOp> {
        let replaced = std::mem::take(&mut self.replaced);
        let audio = |payload: &Payload| payload.audio.as_ref().map(|audio| audio.hash.clone());
        // Two files with an item's audio leave unclear which it moved to.
        let mut holders: HashMap<String, usize> = HashMap::new();
        for (_, payload) in &replaced {
            if let Some(hash) = audio(payload) {
                *holders.entry(hash).or_default() += 1;
            }
        }
        let found: Vec<Option<i64>> = replaced
            .iter()
            .map(|(_, payload)| {
                let hash = audio(payload).filter(|hash| holders.get(hash) == Some(&1))?;
                match self.gone_by_audio.get(&hash)?.as_slice() {
                    [item] => Some(*item),
                    _ => None,
                }
            })
            .collect();
        let moved: HashSet<i64> = found.iter().flatten().copied().collect();
        let mut ops: Vec<RecordOp> = replaced
            .iter()
            .zip(&found)
            .filter(|((here, _), item)| item.is_some() && !moved.contains(here))
            .map(|((here, _), _)| RecordOp::Displace(*here))
            .collect();
        self.summary.retired += ops.len() as u64;
        self.summary.moved += moved.len() as u64;
        let mut left = Vec::new();
        for ((here, payload), item) in replaced.into_iter().zip(found) {
            match item {
                Some(item) => {
                    self.gone.retain(|gone| *gone != item);
                    ops.push(RecordOp::Move { item, payload });
                }
                None => left.push((here, payload)),
            }
        }
        for (here, payload) in left {
            if moved.contains(&here) {
                self.summary.new += 1;
                ops.push(RecordOp::New(payload));
            } else {
                self.replaced.push((here, payload));
            }
        }
        ops
    }

    /// Settles the files whose audio changed at a path the library knew.
    /// The new audio is resolved as if it were a new file: if it lands on
    /// the recording the item has, the item is kept, and if not, the item
    /// is retired and a new one takes the path.
    fn judge_replaced(&mut self, library: &LibraryStore) -> Result<(), Error> {
        self.phase = Phase::Sidecars;
        self.label = Some("reading rip logs and CUE sheets".to_owned());
        let replaced = std::mem::take(&mut self.replaced);
        if replaced.is_empty() {
            return Ok(());
        }
        let (model, _) = library.load()?;
        let mut replacements = Vec::with_capacity(replaced.len());
        for (item, payload) in &replaced {
            let Some(index) = model.items.iter().position(|known| known.id == *item) else {
                return Err(Error::Corrupt("a scan of an item that is gone"));
            };
            let audio = match &payload.audio {
                Some(audio) => library.asset_of(audio.hash_scheme, &audio.hash)?,
                None => None,
            };
            replacements.push(Replacement {
                index,
                audio,
                facts: Facts::from_claims(&extract(payload)),
            });
        }
        let kept = judge_replacements(&model, &replacements, &Policy::current());
        let mut ops = Vec::with_capacity(replaced.len());
        for ((item, payload), kept) in replaced.into_iter().zip(kept) {
            if kept {
                self.summary.changed += 1;
                ops.push(RecordOp::Update { item, payload });
            } else {
                self.summary.retired += 1;
                ops.push(RecordOp::Retire { item, payload });
            }
        }
        let request = self.next_step();
        let receipt = self.step_result(self.summary)?;
        library.record(request, self.source.row, ops, receipt)?;
        self.committed()
    }

    /// Compares the sidecars the walk found near the audio with those the
    /// library has: which to read, and which are gone. A sidecar counts
    /// in a folder of audio, in a folder inside one, or in the folder
    /// above one.
    fn sort_sidecars(&mut self, library: &LibraryStore) -> Result<(), Error> {
        let Some(walked) = &self.walked else {
            return Err(Error::Corrupt("a scan with no walk"));
        };
        self.known_sidecars = library.known_sidecars(self.source.row)?;
        let unread = |path: &str| {
            walked.unreadable.iter().any(|folder| {
                path.strip_prefix(folder.as_str())
                    .is_some_and(|rest| rest.starts_with('/'))
            })
        };
        // The folders of audio: those the walk found, and those of known
        // files in folders it could not read, which are left as they were.
        let audio: HashSet<&str> = walked
            .files
            .iter()
            .map(|file| file.path.as_str())
            .chain(
                self.known
                    .keys()
                    .map(String::as_str)
                    .filter(|path| unread(path)),
            )
            .map(folder_of)
            .collect();
        let above: HashSet<&str> = audio
            .iter()
            .filter(|folder| !folder.is_empty())
            .map(|folder| folder_of(folder))
            .collect();
        let near = |folder: &str| {
            audio.contains(folder)
                || above.contains(folder)
                || (!folder.is_empty() && audio.contains(folder_of(folder)))
        };
        let mut present: HashSet<&str> = HashSet::new();
        for seen in &walked.sidecars {
            if !near(folder_of(&seen.path)) {
                continue;
            }
            present.insert(seen.path.as_str());
            let same = self.known_sidecars.get(&seen.path).is_some_and(|known| {
                !known.missing
                    && unsigned(known.size) == seen.size
                    && known.mtime_ms == seen.mtime_ms
            });
            if !same {
                self.sidecar_queue.push(seen.clone());
            }
        }
        let mut gone: Vec<i64> = self
            .known_sidecars
            .iter()
            .filter(|(path, known)| {
                !known.missing && !present.contains(path.as_str()) && !unread(path)
            })
            .map(|(_, known)| known.id)
            .collect();
        gone.sort_unstable();
        self.sidecar_ops
            .extend(gone.into_iter().map(|id| SidecarOp::Gone { id }));
        Ok(())
    }

    /// Reads the next batch of the sidecar files that are new or changed:
    /// files whose sizes, as the walk found them, come to at most
    /// [`SIDECAR_BATCH_BYTES`], and of each file at most one byte more than
    /// a sidecar may be. Every file read counts, kept or not. A file that is
    /// no longer a sidecar, such as a text file that no longer reads as a
    /// rip log, is gone. A file that cannot be read now is left as it was.
    fn read_sidecars(&mut self, stop: &AtomicBool) {
        let mut bytes_read = 0;
        while bytes_read < SIDECAR_BATCH_BYTES && !stop.load(Ordering::Relaxed) {
            let Some(seen) = self.sidecar_queue.pop() else {
                break;
            };
            let size = usize::try_from(seen.size).unwrap_or(usize::MAX);
            if bytes_read > 0 && bytes_read.saturating_add(size) > SIDECAR_BATCH_BYTES {
                self.sidecar_queue.push(seen);
                break;
            }
            // A sidecar already marked gone is not marked again.
            let live = self
                .known_sidecars
                .get(&seen.path)
                .filter(|known| !known.missing)
                .map(|known| known.id);
            let name = seen.path.rsplit('/').next().unwrap_or(&seen.path);
            let read = std::fs::File::open(under(&self.root, &seen.path)).and_then(|file| {
                let mut bytes = Vec::new();
                file.take(layout::SIDECAR_MAX_BYTES + 1)
                    .read_to_end(&mut bytes)
                    .map(|_| bytes)
            });
            if let Ok(bytes) = &read {
                bytes_read += bytes.len();
            }
            match read {
                Ok(bytes) if layout::keeps(name, &bytes) => {
                    self.sidecar_ops.push(SidecarOp::Read {
                        path: seen.path,
                        size: seen.size,
                        mtime_ms: seen.mtime_ms,
                        bytes,
                    });
                }
                Ok(_) => self
                    .sidecar_ops
                    .extend(live.map(|id| SidecarOp::Gone { id })),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    self.sidecar_ops
                        .extend(live.map(|id| SidecarOp::Gone { id }));
                }
                Err(_) => {}
            }
        }
    }

    /// The job's counters and checkpoint as they now are, to be saved.
    pub fn progress(&self) -> Progress {
        let checkpoint = Checkpoint {
            summary: self.summary,
            dirty: self.dirty,
        };
        let signed = |number: u64| i64::try_from(number).unwrap_or(i64::MAX);
        let read =
            self.summary.new + self.summary.changed + self.summary.moved + self.summary.returned;
        Progress {
            step: self.step,
            checkpoint_json: serde_json::to_string(&checkpoint).ok(),
            done: signed(read),
            // A job that started again counts what it read before, too.
            total: (self.phase != Phase::Walk).then(|| {
                signed(
                    self.summary
                        .files
                        .saturating_sub(self.summary.unchanged)
                        .max(read),
                )
            }),
            bytes: signed(self.summary.bytes_read),
            failed: signed(self.summary.unreadable),
            skipped: signed(self.summary.unchanged),
            label: self.label.clone(),
        }
    }

    /// The outcomes of single files since this was last asked: what could
    /// not be read, and what was passed over.
    pub fn take_items(&mut self) -> Vec<(String, &'static str, String)> {
        std::mem::take(&mut self.failed)
    }
}

/// What a step stores as its result ([`StepResult`]): the checkpoint, with
/// `summary` as its counters and `dirty` as it is once the step has
/// committed, and whether the step is the scan's last.
fn stored(summary: ScanSummary, dirty: bool, last: bool) -> Result<String, Error> {
    Ok(serde_json::to_string(&StepResult {
        checkpoint: Checkpoint { summary, dirty },
        last,
    })?)
}

/// What a job's checkpoint says the scan has found so far.
pub fn scan_summary(job: &Job) -> Option<ScanSummary> {
    let checkpoint: Checkpoint = serde_json::from_str(job.checkpoint_json.as_deref()?).ok()?;
    Some(checkpoint.summary)
}

/// Runs a claimed scan job to its end in one go, saving its progress at
/// every step. A server's worker does the same a step at a time, so that
/// it can stop between steps.
pub fn run_scan(
    job: Job,
    server: &ServerStore,
    library: &LibraryStore,
    settings: ScanSettings,
) -> Result<ScanSummary, Error> {
    let stop = AtomicBool::new(false);
    let mut run = ScanRun::open(job, server, library, settings)?;
    loop {
        run.prepare(&stop, &mut |_| {})?;
        let finished = run.commit(library)?;
        server.add_job_items(run.job.id, run.take_items())?;
        server.save_progress(run.job.id, run.progress())?;
        if let Some(summary) = finished {
            return Ok(summary);
        }
    }
}
