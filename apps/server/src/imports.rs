//! Runs of streams staged on the hub: the pieces a client read from an
//! archive and sent, kept in the data directory until they are dry run,
//! and the job that dry runs them. See "Spotify streams" in
//! docs/internals/history.md.
//!
//! ```text
//! <data-dir>/imports/<run>/
//!   run.json        how many pieces, when it was begun, and each piece's count
//!   piece-<n>.json  one piece's streams, as the contract's type
//!   report.json     the last dry run's report
//! ```
//!
//! Everything here is readable by the OS user alone, holds the kept fields
//! of each stream and nothing else, and is removed a day after its run was
//! begun and whenever the server starts. Nothing in it names the archive.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;

use ritornel_contract::{Id, ImportReport, ImportRun, MAX_STREAMS_PER_PIECE, SpotifyStream};
use ritornel_engine::JobState;
use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};

use crate::Error;
use crate::dispatch::Core;
use crate::error::io_error;
use crate::platform::{create_private_dir, create_private_file};

/// The kind of the job that dry runs a staged run.
pub(crate) const DRY_RUN_JOB: &str = "imports.dryRun";

/// How long a staged run is kept after it was begun.
pub(crate) const KEEP_MS: i64 = 24 * 60 * 60 * 1000;

/// The most pieces one run may have: enough for about two million streams.
pub(crate) const MAX_PIECES: u32 = 2_000;

/// What `run.json` holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Meta {
    pieces: u32,
    begun_at: i64,
    /// Each piece held, by its number, with its count of streams.
    received: BTreeMap<u32, u64>,
}

/// Why a piece was refused.
#[derive(Debug)]
pub(crate) enum Refused {
    /// The piece does not fit the run: its number, its count of pieces, or
    /// its size.
    Invalid(&'static str),
    /// The same piece was sent before with other streams.
    Conflict,
    /// The disk failed.
    Disk(Error),
}

/// The staged runs, in the data directory's `imports/`.
#[derive(Debug)]
pub(crate) struct Staging {
    dir: PathBuf,
    /// Held while a run's files change, so that two pieces of one run
    /// stored at once both reach `run.json`.
    writing: Mutex<()>,
}

/// The dry run's job params.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DryRunParams {
    pub(crate) run_id: Id,
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Option<T>, Error> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| Error::Io {
                doing: "read",
                path: path.to_owned(),
                source: std::io::Error::other(error.to_string()),
            }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(io_error("read", path)(error)),
    }
}

/// Writes `value` to `path` whole or not at all: to a new file only the OS
/// user can read, renamed over the old.
fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), Error> {
    let bytes = serde_json::to_vec(value).map_err(|error| Error::Io {
        doing: "write",
        path: path.to_owned(),
        source: std::io::Error::other(error.to_string()),
    })?;
    let mut temporary = path.to_owned().into_os_string();
    temporary.push(".new");
    let temporary = PathBuf::from(temporary);
    let _ = std::fs::remove_file(&temporary);
    // The file is closed before it is renamed, which Windows needs, and
    // removed if anything fails.
    let written = create_private_file(&temporary)
        .and_then(|mut file| file.write_all(&bytes).and_then(|()| file.sync_all()))
        .map_err(io_error("write", &temporary))
        .and_then(|()| std::fs::rename(&temporary, path).map_err(io_error("rename", path)));
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    written
}

impl Staging {
    /// The staging folder of a data directory, removed: a run staged before
    /// the server started is not kept. It is made again when a run is first
    /// staged.
    pub(crate) fn start(dir: PathBuf) -> Result<Self, Error> {
        match std::fs::remove_dir_all(&dir) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                return Err(io_error("remove", &dir)(error));
            }
            _ => {}
        }
        Ok(Self {
            dir,
            writing: Mutex::default(),
        })
    }

    fn run_dir(&self, run: Id) -> PathBuf {
        self.dir.join(run.to_string())
    }

    fn meta(&self, run: Id) -> Result<Option<Meta>, Error> {
        read_json(&self.run_dir(run).join("run.json"))
    }

    /// Removes the runs begun more than a day before `now`, holding the
    /// lock: a run is never removed while a piece of it is stored or a
    /// report saved.
    pub(crate) fn sweep(&self, now: i64) {
        let _writing = self
            .writing
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.sweep_held(now);
    }

    fn sweep_held(&self, now: i64) {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let begun = read_json::<Meta>(&path.join("run.json"))
                .ok()
                .flatten()
                .map(|meta| meta.begun_at);
            if begun.is_none_or(|begun| now.saturating_sub(begun) > KEEP_MS)
                && let Err(failed) = std::fs::remove_dir_all(&path)
            {
                warn!(error = %failed, "a staged run that has expired could not be removed");
            }
        }
    }

    /// Keeps one piece of a run. The same piece sent again with the same
    /// streams is kept once; with others it is refused.
    pub(crate) fn stage(
        &self,
        run: Id,
        piece: u32,
        pieces: u32,
        streams: &[SpotifyStream],
        now: i64,
    ) -> Result<ImportRun, Refused> {
        if pieces == 0 || pieces > MAX_PIECES || piece >= pieces {
            return Err(Refused::Invalid(
                "a piece's number is from 0 to one less than the run's pieces",
            ));
        }
        if streams.is_empty() || streams.len() > MAX_STREAMS_PER_PIECE {
            return Err(Refused::Invalid(
                "a piece holds from one stream to the most a piece may hold",
            ));
        }
        if streams.iter().any(|stream| stream.plausible().is_err()) {
            return Err(Refused::Invalid(
                "a stream ends outside 1970 to 2100, or played longer than a day",
            ));
        }
        let _writing = self
            .writing
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.sweep_held(now);
        let dir = self.run_dir(run);
        let mut meta = match self.meta(run).map_err(Refused::Disk)? {
            Some(meta) if meta.pieces != pieces => {
                return Err(Refused::Invalid(
                    "a run's pieces are the same in every piece",
                ));
            }
            Some(meta) => meta,
            None => {
                create_private_dir(&self.dir).map_err(Refused::Disk)?;
                create_private_dir(&dir).map_err(Refused::Disk)?;
                Meta {
                    pieces,
                    begun_at: now,
                    received: BTreeMap::new(),
                }
            }
        };
        let file = dir.join(format!("piece-{piece}.json"));
        match read_json::<Vec<SpotifyStream>>(&file).map_err(Refused::Disk)? {
            Some(held) if held != streams => return Err(Refused::Conflict),
            Some(_) => {}
            None => write_json(&file, &streams).map_err(Refused::Disk)?,
        }
        // Said in run.json, if it is not: a piece written whole once before,
        // whose run.json was not, is said now.
        if meta.received.get(&piece) != Some(&(streams.len() as u64)) {
            meta.received.insert(piece, streams.len() as u64);
            write_json(&dir.join("run.json"), &meta).map_err(Refused::Disk)?;
            // A new piece makes the last report out of date.
            let _ = std::fs::remove_file(dir.join("report.json"));
        }
        Ok(self.shown(run, &meta))
    }

    fn shown(&self, run: Id, meta: &Meta) -> ImportRun {
        ImportRun {
            run_id: run,
            pieces: meta.pieces,
            received: u32::try_from(meta.received.len()).unwrap_or(u32::MAX),
            streams: meta.received.values().sum(),
            expires_at: meta.begun_at + KEEP_MS,
            report: read_json(&self.run_dir(run).join("report.json"))
                .ok()
                .flatten(),
        }
    }

    /// A staged run that has not expired, with its last report.
    pub(crate) fn run(&self, run: Id, now: i64) -> Result<Option<ImportRun>, Error> {
        Ok(self
            .meta(run)?
            .filter(|meta| now.saturating_sub(meta.begun_at) <= KEEP_MS)
            .map(|meta| self.shown(run, &meta)))
    }

    /// Every stream of a run whose every piece is held, in the order of its
    /// pieces, with when the run was begun, which tells it from a run
    /// staged again under its ID. `None` when the run is gone or a piece is
    /// missing.
    pub(crate) fn streams(
        &self,
        run: Id,
        now: i64,
    ) -> Result<Option<(Vec<SpotifyStream>, i64)>, Error> {
        let Some(meta) = self
            .meta(run)?
            .filter(|meta| now.saturating_sub(meta.begun_at) <= KEEP_MS)
        else {
            return Ok(None);
        };
        if meta.received.len() != meta.pieces as usize {
            return Ok(None);
        }
        let mut streams = Vec::new();
        for piece in 0..meta.pieces {
            let file = self.run_dir(run).join(format!("piece-{piece}.json"));
            match read_json::<Vec<SpotifyStream>>(&file)? {
                Some(held) => streams.extend(held),
                None => return Ok(None),
            }
        }
        Ok(Some((streams, meta.begun_at)))
    }

    /// Keeps a dry run's report beside its run, if the run is still the one
    /// begun at `begun_at` and holds every piece the report read. Returns
    /// whether it was kept.
    pub(crate) fn save_report(
        &self,
        run: Id,
        begun_at: i64,
        report: &ImportReport,
    ) -> Result<bool, Error> {
        let _writing = self
            .writing
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match self.meta(run)? {
            Some(meta)
                if meta.begun_at == begun_at && meta.received.len() == meta.pieces as usize =>
            {
                write_json(&self.run_dir(run).join("report.json"), report)?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }
}

/// How often the staged runs are looked at for any that have expired.
const SWEEP_EVERY: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// Removes the staged runs that have expired, once an hour, for as long as
/// the server runs: a run is not kept past its day because nothing else was
/// staged after it.
pub(crate) async fn sweep(core: std::sync::Arc<Core>) {
    loop {
        tokio::time::sleep(SWEEP_EVERY).await;
        let core = std::sync::Arc::clone(&core);
        let _ = tokio::task::spawn_blocking(move || core.staging().sweep(core.now_ms())).await;
    }
}

/// Carries a claimed dry run to its end: reads the run's streams, works
/// out what applying them would do, and keeps the report beside the run.
pub(crate) fn run_dry_run(core: &Core, job: ritornel_engine::Job, _stop: &AtomicBool) {
    let (row, job_id) = (job.id, job.public_id);
    info!(job_id = %job_id, "a dry run of an import is claimed");
    let finish = |state: JobState, failure: Option<String>| match core
        .with_stores(|stores| stores.server().finish_job(row, state, failure))
    {
        Some(Ok(job)) => core.publish_job(&job),
        Some(Err(failed)) => {
            error!(job_id = %job_id, error = %failed, "a job's end could not be saved");
        }
        None => {}
    };
    let Ok(params) = serde_json::from_str::<DryRunParams>(&job.params_json) else {
        return finish(
            JobState::Failed,
            Some("the job's params are not a run".to_owned()),
        );
    };
    let (streams, begun_at) = match core.staging().streams(params.run_id, core.now_ms()) {
        Ok(Some(found)) => found,
        Ok(None) => {
            return finish(
                JobState::Failed,
                Some("the staged run is gone, or a piece of it is missing".to_owned()),
            );
        }
        Err(failed) => return finish(JobState::Failed, Some(failed.to_string())),
    };
    let report = core.with_library(|_, library| library.stream_dry_run(streams));
    match report {
        Some(Ok(report)) => match core.staging().save_report(params.run_id, begun_at, &report) {
            Ok(true) => finish(JobState::Succeeded, None),
            Ok(false) => finish(
                JobState::Failed,
                Some(
                    "the staged run expired, or was staged again, while it was dry run".to_owned(),
                ),
            ),
            Err(failed) => finish(JobState::Failed, Some(failed.to_string())),
        },
        Some(Err(failed)) => finish(JobState::Failed, Some(failed.to_string())),
        None => finish(
            JobState::Failed,
            Some("this server holds no library".to_owned()),
        ),
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::platform::path_is_private;
    use crate::testing::Scratch;

    const NOW: i64 = 1_794_776_400_000;

    fn stream(title: &str) -> SpotifyStream {
        SpotifyStream {
            ended_at: 1_552_684_449_000,
            ms_played: 221_000,
            track_uri: "spotify:track:0000000000000000000001".to_owned(),
            title: title.to_owned(),
            artist: "Calla Brandt".to_owned(),
            album: Some("Glasshouse".to_owned()),
            reason_start: None,
            reason_end: None,
            shuffle: None,
            skipped: None,
            offline: None,
            offline_timestamp: None,
            incognito: None,
            platform: None,
            device_kind: None,
            via: None,
        }
    }

    fn run() -> Id {
        Id::from_bytes([0x5a; 16])
    }

    #[test]
    fn a_run_is_kept_piece_by_piece_and_read_whole_once_every_piece_is_held() {
        let scratch = Scratch::new();
        let staging = Staging::start(scratch.path().join("imports")).unwrap();
        let second = vec![stream("Paper Lanterns")];
        let shown = staging.stage(run(), 1, 2, &second, NOW).unwrap();
        assert_eq!((shown.pieces, shown.received, shown.streams), (2, 1, 1));
        assert_eq!(staging.streams(run(), NOW).unwrap(), None);
        let first = vec![stream("Low Tide"), stream("Night Ferry")];
        let shown = staging.stage(run(), 0, 2, &first, NOW).unwrap();
        assert_eq!(
            (shown.received, shown.streams, shown.expires_at),
            (2, 3, NOW + KEEP_MS)
        );
        let (whole, _) = staging.streams(run(), NOW).unwrap().unwrap();
        let titles: Vec<&str> = whole.iter().map(|s| s.title.as_str()).collect();
        assert_eq!(titles, ["Low Tide", "Night Ferry", "Paper Lanterns"]);
        // Only the OS user can read what was kept.
        let dir = scratch.path().join("imports").join(run().to_string());
        assert!(path_is_private(&dir));
        assert!(path_is_private(&dir.join("piece-0.json")));
    }

    #[test]
    fn a_piece_sent_again_is_kept_once_and_one_that_differs_is_refused() {
        let scratch = Scratch::new();
        let staging = Staging::start(scratch.path().join("imports")).unwrap();
        let piece = vec![stream("Low Tide")];
        staging.stage(run(), 0, 1, &piece, NOW).unwrap();
        let again = staging.stage(run(), 0, 1, &piece, NOW).unwrap();
        assert_eq!((again.received, again.streams), (1, 1));
        assert!(matches!(
            staging.stage(run(), 0, 1, &[stream("Night Ferry")], NOW),
            Err(Refused::Conflict)
        ));
        for (piece, pieces) in [(1, 1), (0, 0), (0, 2)] {
            assert!(matches!(
                staging.stage(run(), piece, pieces, &[], NOW),
                Err(Refused::Invalid(_))
            ));
        }
        let many = vec![stream("Low Tide"); MAX_STREAMS_PER_PIECE + 1];
        assert!(matches!(
            staging.stage(Id::from_bytes([1; 16]), 0, 1, &many, NOW),
            Err(Refused::Invalid(_))
        ));
    }

    #[test]
    fn a_piece_whose_run_was_not_said_is_said_when_it_is_sent_again() {
        let scratch = Scratch::new();
        let staging = Staging::start(scratch.path().join("imports")).unwrap();
        staging
            .stage(run(), 0, 2, &[stream("Low Tide")], NOW)
            .unwrap();
        // Piece 1 reached the disk and run.json did not, as when the disk
        // filled between the two.
        let dir = scratch.path().join("imports").join(run().to_string());
        write_json(&dir.join("piece-1.json"), &vec![stream("Night Ferry")]).unwrap();
        assert_eq!(staging.run(run(), NOW).unwrap().unwrap().received, 1);
        let again = staging
            .stage(run(), 1, 2, &[stream("Night Ferry")], NOW)
            .unwrap();
        assert_eq!((again.received, again.streams), (2, 2));
        assert!(staging.streams(run(), NOW).unwrap().is_some());
    }

    #[test]
    fn a_stream_whose_times_no_stream_can_have_is_refused() {
        let scratch = Scratch::new();
        let staging = Staging::start(scratch.path().join("imports")).unwrap();
        let mut long = stream("Low Tide");
        long.ms_played = i64::MAX;
        let mut late = stream("Low Tide");
        late.ended_at = i64::MAX;
        for wrong in [long, late] {
            assert!(matches!(
                staging.stage(run(), 0, 1, &[wrong], NOW),
                Err(Refused::Invalid(_))
            ));
        }
        // A piece holds a stream at least.
        assert!(matches!(
            staging.stage(run(), 0, 1, &[], NOW),
            Err(Refused::Invalid(_))
        ));
    }

    #[test]
    fn a_report_is_kept_only_for_the_run_it_was_made_of() {
        let scratch = Scratch::new();
        let staging = Staging::start(scratch.path().join("imports")).unwrap();
        staging
            .stage(run(), 0, 1, &[stream("Low Tide")], NOW)
            .unwrap();
        let (_, begun_at) = staging.streams(run(), NOW).unwrap().unwrap();
        assert!(staging.streams(run(), NOW + KEEP_MS + 1).unwrap().is_none());
        // Swept, and staged again under the same ID a day later: a report
        // of the first is not the second's.
        staging.sweep(NOW + KEEP_MS + 1);
        staging
            .stage(run(), 0, 1, &[stream("Low Tide")], NOW + KEEP_MS + 2)
            .unwrap();
        let report = ImportReport {
            made_at: NOW,
            grouper_version: 1,
            streams: 1,
            events: 1,
            scrobbles: 0,
            scrobbles_excluded: 0,
            scrobbles_echoes: 0,
            tiers: Vec::new(),
            years: Vec::new(),
            offline: 0,
            offline_timestamped: 0,
            offline_seconds: 0,
            offline_milliseconds: 0,
            paused: 0,
            problems: Vec::new(),
            samples: Vec::new(),
        };
        assert!(!staging.save_report(run(), begun_at, &report).unwrap());
        let later = NOW + KEEP_MS + 2;
        assert!(staging.run(run(), later).unwrap().unwrap().report.is_none());
        let (_, begun_again) = staging.streams(run(), later).unwrap().unwrap();
        assert!(staging.save_report(run(), begun_again, &report).unwrap());
    }

    #[test]
    fn a_run_is_gone_after_a_day_and_after_a_start() {
        let scratch = Scratch::new();
        let folder = scratch.path().join("imports");
        let staging = Staging::start(folder.clone()).unwrap();
        staging
            .stage(run(), 0, 1, &[stream("Low Tide")], NOW)
            .unwrap();
        assert!(staging.run(run(), NOW + KEEP_MS).unwrap().is_some());
        assert!(staging.run(run(), NOW + KEEP_MS + 1).unwrap().is_none());
        // Staging another run a day later sweeps it away.
        let other = Id::from_bytes([2; 16]);
        staging
            .stage(other, 0, 1, &[stream("Low Tide")], NOW + KEEP_MS + 1)
            .unwrap();
        assert!(!folder.join(run().to_string()).exists());
        let staging = Staging::start(folder.clone()).unwrap();
        assert!(staging.run(other, NOW + KEEP_MS + 1).unwrap().is_none());
        assert!(!folder.exists());
    }
}
