//! The scan job: walk a root, read what is new or changed, record it, mark
//! what is gone, and resolve the library. See "Scanning one file" and
//! "Rescans" in docs/design/identity.md, and "An effect, a job and a lease"
//! in docs/design/server.md.
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
//! command ID and counted as done.
//!
//! The resolver runs once, at the end, over the whole library, and only if
//! the scan changed something. So a rescan that finds nothing changed
//! writes nothing at all, and takes no library revision.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;

use ritornel_contract::ScanSummary;
use ritornel_domain::payload::{FolderImage, Payload};
use ritornel_domain::{Facts, Policy, Replacement, extract, judge_replacements};
use serde::{Deserialize, Serialize};

use crate::jobs::{Job, Progress, step_command};
use crate::media::{read_file, read_folder_image};
use crate::scan_store::{Known, RecordOp, Source};
use crate::walk::{Seen, Walked, folder_of, walk};
use crate::{Command, Error, Fingerprint, LibraryStore, ServerStore, WriteRequest};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Walk,
    Read,
    Missing,
    Replaced,
    Resolve,
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
    /// Items whose path the walk did not find, by audio hash and by key.
    gone_by_audio: HashMap<String, Vec<i64>>,
    gone: Vec<i64>,
    /// The files to read, and how far the reading has got.
    queue: Vec<Seen>,
    cursor: usize,
    batch: Vec<Payload>,
    images: HashMap<String, Vec<FolderImage>>,
    /// Files whose audio changed at a known path, held for the resolver.
    replaced: Vec<(i64, Payload)>,
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
        // found by its command ID, and is not done again.
        let (mut step, mut dirty) = (job.step, checkpoint.dirty);
        while library.has_command(step_command(job.public_id, step + 1))? {
            step += 1;
            dirty = true;
        }
        Ok(Self {
            job,
            source,
            root,
            settings,
            phase: Phase::Walk,
            step,
            summary: checkpoint.summary,
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
                            read_folder_image(&self.root.join(&image.path), name, image.mtime_ms)
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
                match batch {
                    Some(batch) => self.batch = batch,
                    None => return Ok(false),
                }
            }
            _ => {}
        }
        Ok(!stop.load(Ordering::Relaxed))
    }

    /// The request of the job's next library write: its next step.
    fn next_step(&mut self) -> WriteRequest {
        self.step += 1;
        WriteRequest {
            method: "scan.step",
            command: Some(Command {
                id: step_command(self.job.public_id, self.step),
                fingerprint: Fingerprint::of("scan.step", &self.step.to_be_bytes()),
            }),
            client: None,
        }
    }

    /// What follows every library commit of the job: the fail point.
    fn committed(&mut self) -> Result<(), Error> {
        self.dirty = true;
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
            Phase::Missing => {
                // What the walk did not find and no file moved from. The
                // root was reachable, or the walk would have failed.
                let gone: Vec<i64> = std::mem::take(&mut self.gone);
                self.summary.missing = gone.len() as u64;
                self.phase = Phase::Replaced;
                if !gone.is_empty() {
                    let request = self.next_step();
                    library.mark_missing(request, gone)?;
                    self.committed()?;
                }
            }
            Phase::Replaced => self.judge_replaced(library)?,
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
                        }
                        // Nothing was written, so the step was not taken.
                        None => self.step -= 1,
                    }
                    self.dirty = false;
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
        for (path, known) in &self.known {
            if present.contains(path.as_str()) {
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
        for (path, why) in &walked.skipped {
            self.failed
                .push((path.clone(), "skipped", format!("{{\"reason\":\"{why}\"}}")));
        }
        self.summary.files = walked.files.len() as u64;
        self.summary.unchanged = unchanged;
        self.phase = if self.queue.is_empty() {
            Phase::Missing
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
            counted.bytes_read += payload.size;
            let audio = payload.audio.as_ref().map(|audio| audio.hash.clone());
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
            match self.known.get(&payload.path) {
                // The same audio under other tags, a file that is back, or
                // a file that could not be read before or cannot now.
                Some(known) if known.audio == audio || known.audio.is_none() || audio.is_none() => {
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
                // is the resolver's to say, once everything is recorded.
                Some(known) => self.replaced.push((known.id, payload)),
                None => {
                    // A new path whose audio matches exactly one item of
                    // this root whose path is gone: the file moved.
                    let moved = audio
                        .as_ref()
                        .and_then(|audio| self.gone_by_audio.get_mut(audio))
                        .filter(|items| items.len() == 1)
                        .and_then(Vec::pop);
                    match moved {
                        Some(item) => {
                            self.gone.retain(|gone| *gone != item);
                            counted.moved += 1;
                            ops.push(RecordOp::Move { item, payload });
                        }
                        None => {
                            counted.new += 1;
                            ops.push(RecordOp::New(payload));
                        }
                    }
                }
            }
        }
        self.cursor = (self.cursor + self.settings.batch_files.max(1)).min(self.queue.len());
        if self.cursor >= self.queue.len() {
            self.phase = Phase::Missing;
        }
        self.summary.bytes_read = counted.bytes_read;
        if !ops.is_empty() {
            let request = self.next_step();
            library.record(request, self.source.row, ops)?;
            self.summary = counted;
            self.committed()?;
        }
        Ok(())
    }

    /// Settles the files whose audio changed at a path the library knew.
    /// The new audio is resolved as if it were a new file: if it lands on
    /// the recording the item has, the item is kept, and if not, the item
    /// is retired and a new one takes the path.
    fn judge_replaced(&mut self, library: &LibraryStore) -> Result<(), Error> {
        self.phase = Phase::Resolve;
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
        let kept = judge_replacements(&model, &replacements, &Policy::v1());
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
        library.record(request, self.source.row, ops)?;
        self.committed()
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
