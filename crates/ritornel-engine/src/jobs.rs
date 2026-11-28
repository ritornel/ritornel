//! Jobs and roots in `server.db`: work longer than one call, as a row with
//! a claim, a heartbeat and a checkpoint, and where each named root is on
//! this machine. See "Jobs and outbox" in docs/internals/server.md, and
//! "Commands, playback, and jobs" in docs/internals/contract.md.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use rusqlite::{Connection, OptionalExtension, Row, Transaction};
use sha2::{Digest, Sha256};

use crate::{Command, Error, Id, ServerStore};

/// Why a claimed job should stop before its next write or step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobStop {
    /// `jobs.cancel` asked this job to stop.
    Cancelled,
    /// The server is stopping: leave the job claimed for the next run.
    Shutdown,
}

/// A live handle that checks whether a claimed job was asked to stop or
/// the server is stopping. Held by a job's worker and checked at the start
/// of each library transaction before any mutation.
#[derive(Debug, Clone)]
pub struct JobCancel {
    job: i64,
    cancels: Arc<Mutex<HashSet<i64>>>,
    shutdown: Arc<AtomicBool>,
}

impl JobCancel {
    /// Whether the job was asked to stop, or the server is stopping.
    /// Cancellation takes precedence over shutdown so a cancelled job ends
    /// rather than waiting for the next run.
    pub fn stopped(&self) -> Option<JobStop> {
        if self
            .cancels
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(&self.job)
        {
            return Some(JobStop::Cancelled);
        }
        if self.shutdown.load(Ordering::SeqCst) {
            return Some(JobStop::Shutdown);
        }
        None
    }

    pub(crate) fn check(&self) -> Result<(), Error> {
        match self.stopped() {
            Some(stop) => Err(Error::JobStopped(stop)),
            None => Ok(()),
        }
    }
}

/// Where a job is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    /// Waiting for the worker.
    Queued,
    /// Claimed by a worker of this run.
    Running,
    /// Asked to stop, and not yet stopped.
    Cancelling,
    /// It ended and did what it was for.
    Succeeded,
    /// It ended on an error.
    Failed,
    /// It ended because it was asked to.
    Cancelled,
}

impl JobState {
    /// The state as the schema writes it.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Cancelling => "cancelling",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        [
            Self::Queued,
            Self::Running,
            Self::Cancelling,
            Self::Succeeded,
            Self::Failed,
            Self::Cancelled,
        ]
        .into_iter()
        .find(|state| state.as_str() == text)
    }

    /// Whether the job has ended.
    pub const fn ended(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
}

/// One job, as its row holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    /// The row's key, which never leaves the server.
    pub id: i64,
    /// The job's ID.
    pub public_id: Id,
    /// What kind of work it is, such as `scan`.
    pub kind: String,
    /// What its events invalidate: for a scan, the source.
    pub target: Option<Id>,
    /// The kind of resource identified by the target.
    pub target_kind: Option<String>,
    /// What it was asked to do.
    pub params_json: String,
    /// Where it is in its life.
    pub state: JobState,
    /// The last step whose checkpoint was saved.
    pub step: i64,
    /// What the job needs to carry on from that step, and at its end what
    /// came out.
    pub checkpoint_json: Option<String>,
    /// Whether it has been asked to stop.
    pub cancel_requested: bool,
    /// Items done.
    pub done: i64,
    /// Items in all, once that is known.
    pub total: Option<i64>,
    /// Bytes read.
    pub bytes: i64,
    /// Items that failed.
    pub failed: i64,
    /// Items passed over.
    pub skipped: i64,
    /// What it is doing now.
    pub label: Option<String>,
    /// Why it failed.
    pub error: Option<String>,
    /// When it was created, started and finished, as Unix milliseconds.
    pub created_at_ms: i64,
    /// See `created_at_ms`.
    pub started_at_ms: Option<i64>,
    /// See `created_at_ms`.
    pub finished_at_ms: Option<i64>,
}

/// A job to create.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewJob {
    /// What kind of work it is.
    pub kind: &'static str,
    /// One active job per kind and key: for a scan, the root's name.
    pub dedupe_key: String,
    /// What its events invalidate.
    pub target: Id,
    /// Indexed resource kind for this job.
    pub target_kind: &'static str,
    /// What it is asked to do.
    pub params_json: String,
    /// The session that asked, by its public ID.
    pub session: Option<Id>,
}

/// A job's counters and where it has got to, saved about once a second and
/// at every step.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Progress {
    /// The last step committed.
    pub step: i64,
    /// What the job needs to carry on from there.
    pub checkpoint_json: Option<String>,
    /// Items done.
    pub done: i64,
    /// Items in all, once that is known.
    pub total: Option<i64>,
    /// Bytes read.
    pub bytes: i64,
    /// Items that failed.
    pub failed: i64,
    /// Items passed over.
    pub skipped: i64,
    /// What the job is doing now.
    pub label: Option<String>,
}

const COLUMNS: &str = "id, public_id, kind, target_pid, params_json, state, step, checkpoint_json,
    cancel_requested_at_ms IS NOT NULL, done, total, bytes, failed_count, skipped_count,
    current_label, error_json, created_at_ms, started_at_ms, finished_at_ms, target_kind";

fn job(row: &Row<'_>) -> rusqlite::Result<Job> {
    let state: String = row.get(5)?;
    Ok(Job {
        id: row.get(0)?,
        public_id: Id::from_bytes(row.get(1)?),
        kind: row.get(2)?,
        target_kind: row.get(19)?,
        target: row.get::<_, Option<[u8; 16]>>(3)?.map(Id::from_bytes),
        params_json: row.get(4)?,
        state: JobState::parse(&state).ok_or(rusqlite::Error::InvalidQuery)?,
        step: row.get(6)?,
        checkpoint_json: row.get(7)?,
        cancel_requested: row.get(8)?,
        done: row.get(9)?,
        total: row.get(10)?,
        bytes: row.get(11)?,
        failed: row.get(12)?,
        skipped: row.get(13)?,
        label: row.get(14)?,
        error: row.get(15)?,
        created_at_ms: row.get(16)?,
        started_at_ms: row.get(17)?,
        finished_at_ms: row.get(18)?,
    })
}

fn job_by_row(connection: &Connection, id: i64) -> Result<Job, Error> {
    Ok(connection.query_row(
        &format!("SELECT {COLUMNS} FROM job WHERE id = ?1"),
        [id],
        job,
    )?)
}

fn job_by_id(connection: &Connection, id: Id) -> Result<Option<Job>, Error> {
    Ok(connection
        .query_row(
            &format!("SELECT {COLUMNS} FROM job WHERE public_id = ?1"),
            [id.as_bytes()],
            job,
        )
        .optional()?)
}

/// Ends a job in `transaction`, unless it has ended: one asked to stop
/// never ends as succeeded, but as cancelled.
fn end(
    transaction: &Connection,
    job: i64,
    state: JobState,
    error: Option<String>,
    now_ms: i64,
) -> Result<Job, Error> {
    let current = job_by_row(transaction, job)?;
    if current.state.ended() {
        return Ok(current);
    }
    let (final_state, final_error) = if current.cancel_requested && state == JobState::Succeeded {
        (JobState::Cancelled, None)
    } else {
        (state, error)
    };
    transaction.execute(
        "UPDATE job SET state = ?2, error_json = ?3, finished_at_ms = ?4, current_label = NULL
          WHERE id = ?1",
        (job, final_state.as_str(), &final_error, now_ms),
    )?;
    job_by_row(transaction, job)
}

/// The stored result of a server-local command, if it was carried out
/// before. The same ID with another request is refused.
pub(crate) fn receipt(connection: &Connection, command: &Command) -> Result<Option<String>, Error> {
    let stored: Option<(Vec<u8>, String)> = connection
        .query_row(
            "SELECT request_fingerprint, result_json FROM command_receipt WHERE command_id = ?1",
            [command.id.as_bytes()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    match stored {
        Some((fingerprint, result)) if fingerprint == command.fingerprint.bytes() => {
            Ok(Some(result))
        }
        Some(_) => Err(Error::CommandMismatch {
            command: command.id,
        }),
        None => Ok(None),
    }
}

const RECEIPT_LIFE_MS: i64 = 30 * 24 * 60 * 60 * 1_000;
const RECEIPT_SWEEP_INTERVAL: i64 = 256;

fn prune_receipts(connection: &Connection, now_ms: i64) -> Result<(), Error> {
    // A hub persists move.<key> until it releases the source. The destination
    // may be offline when the hub probes a lost load reply. Only the latest
    // load needs this protection: the hub resolves it before it can send
    // another load to the same destination.
    connection.execute(
        "WITH latest_load AS (
             SELECT command_id FROM command_receipt WHERE method = 'link.handoff.load'
             ORDER BY id DESC LIMIT 1
         ), handoff(command_id) AS (
             SELECT command_id FROM latest_load
             UNION ALL SELECT unhex(substr(key, length('move.') + 1), '-')
                 FROM server_setting WHERE key GLOB 'move.*'
             UNION ALL SELECT unhex(substr(key, length('handoff.take.') + 1), '-')
                 FROM server_setting WHERE key GLOB 'handoff.take.*'
         )
         DELETE FROM command_receipt WHERE created_at_ms < ?1
           AND command_id NOT IN (SELECT command_id FROM handoff WHERE command_id IS NOT NULL)
           AND NOT EXISTS (
               SELECT 1 FROM job WHERE public_id = unhex(command_receipt.result_json, '-')
                 AND state IN ('queued', 'running', 'cancelling')
           )",
        [now_ms.saturating_sub(RECEIPT_LIFE_MS)],
    )?;
    Ok(())
}

pub(crate) fn record_receipt(
    transaction: &Transaction<'_>,
    command: &Command,
    method: &str,
    result: &str,
    now_ms: i64,
) -> Result<(), Error> {
    transaction.execute(
        "INSERT INTO command_receipt (command_id, request_fingerprint, method, result_json, created_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        (command.id.as_bytes(), command.fingerprint.bytes(), method, result, now_ms),
    )?;
    // There is no timestamp index. Amortize the scan across new receipts.
    if transaction.last_insert_rowid() % RECEIPT_SWEEP_INTERVAL == 0 {
        prune_receipts(transaction, now_ms)?;
    }
    Ok(())
}

/// The command ID of one step of a job: 16 bytes derived from the job's ID
/// and the step number, so that a step redone after a crash is recognised
/// as the step it is.
pub fn step_command(job: Id, step: i64) -> Id {
    let mut hash = Sha256::new();
    hash.update(b"ritornel-job-step");
    hash.update(job.as_bytes());
    hash.update(step.to_be_bytes());
    let digest = hash.finalize();
    let mut bytes = [0; 16];
    for (byte, from) in bytes.iter_mut().zip(digest.iter()) {
        *byte = *from;
    }
    Id::from_bytes(bytes)
}

impl ServerStore {
    /// Removes expired server-local command receipts after startup migrations finish.
    pub fn prune_receipts(&self) -> Result<(), Error> {
        self.transact(|tx, now_ms, _| prune_receipts(tx, now_ms))
    }

    /// Records where a named root is on this machine. Naming the same path
    /// again changes nothing, and a name that is at another path is
    /// refused: a root is re-pointed by a call of its own.
    pub fn add_root(&self, name: &str, abs_path: &str) -> Result<(), Error> {
        let (name, abs_path) = (name.to_owned(), abs_path.to_owned());
        self.transact(move |transaction, now_ms, _| {
            let existing: Option<String> = transaction
                .query_row(
                    "SELECT abs_path FROM root WHERE name = ?1",
                    [&name],
                    |row| row.get(0),
                )
                .optional()?;
            match existing {
                Some(path) if path == abs_path => Ok(()),
                Some(_) => Err(Error::RootTaken { name }),
                None => {
                    transaction.execute(
                        "INSERT INTO root (name, abs_path, created_at_ms) VALUES (?1, ?2, ?3)",
                        (&name, &abs_path, now_ms),
                    )?;
                    Ok(())
                }
            }
        })
    }

    /// Every root this server has a path for: its name and its path.
    pub fn roots(&self) -> Result<Vec<(String, String)>, Error> {
        self.read(|connection| {
            let mut statement =
                connection.prepare("SELECT name, abs_path FROM root ORDER BY id")?;
            let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
    }

    /// Carries out the command that starts a job, and returns the job with
    /// whether this call created it. A job of the same kind and key that
    /// has not ended is returned in place of a second one, and the command
    /// sent again returns the job it returned the first time.
    pub fn start_job(
        &self,
        command: Command,
        method: &'static str,
        new: NewJob,
    ) -> Result<(Job, bool), Error> {
        self.start_job_upgrading(command, method, new, |_| None)
    }

    /// As [`start_job`](Self::start_job), and asks more of a queued or
    /// running job of the same kind and key: `upgrade` is given its params
    /// and returns the params it is to run with instead, or `None` when it
    /// does what is asked already. A queued job is claimed with them; a
    /// running one finds them when it would end
    /// ([`finish_succeeded_job`](Self::finish_succeeded_job)) and runs
    /// again, so what was asked is never dropped. One being cancelled is
    /// refused as [`Error::JobEnding`], and the command is not kept.
    pub fn start_job_upgrading(
        &self,
        command: Command,
        method: &'static str,
        new: NewJob,
        upgrade: impl FnOnce(&str) -> Option<String> + Send + 'static,
    ) -> Result<(Job, bool), Error> {
        self.transact(move |transaction, now_ms, entropy| {
            if let Some(result) = receipt(transaction, &command)? {
                let id = result.parse().map_err(|_| Error::Corrupt("a receipt"))?;
                let job =
                    job_by_id(transaction, id)?.ok_or(Error::Corrupt("a receipt for no job"))?;
                return Ok((job, false));
            }
            let running: Option<i64> = transaction
                .query_row(
                    "SELECT id FROM job WHERE kind = ?1 AND dedupe_key = ?2
                       AND state IN ('queued', 'running', 'cancelling')",
                    (new.kind, &new.dedupe_key),
                    |row| row.get(0),
                )
                .optional()?;
            let (row, created) = match running {
                Some(row) => {
                    let current = job_by_row(transaction, row)?;
                    if let Some(params) = upgrade(&current.params_json) {
                        // One being cancelled will not do it, and the
                        // command is kept for none.
                        if current.state == JobState::Cancelling {
                            return Err(Error::JobEnding);
                        }
                        transaction.execute(
                            "UPDATE job SET params_json = ?2 WHERE id = ?1",
                            (row, &params),
                        )?;
                    }
                    (row, false)
                }
                None => {
                    let public_id = crate::new_id(now_ms, entropy)?;
                    transaction.execute(
                        "INSERT INTO job
                           (public_id, kind, dedupe_key, target_kind, target_pid, params_json,
                            created_by_session_id, created_at_ms)
                         VALUES (?1, ?2, ?3, ?8, ?4, ?5,
                                 (SELECT id FROM session WHERE public_id = ?6), ?7)",
                        (
                            public_id.as_bytes(),
                            new.kind,
                            &new.dedupe_key,
                            new.target.as_bytes(),
                            &new.params_json,
                            new.session.as_ref().map(Id::as_bytes),
                            now_ms,
                            new.target_kind,
                        ),
                    )?;
                    (transaction.last_insert_rowid(), true)
                }
            };
            let job = job_by_row(transaction, row)?;
            record_receipt(
                transaction,
                &command,
                method,
                &job.public_id.to_string(),
                now_ms,
            )?;
            Ok((job, created))
        })
    }

    /// Puts back in the queue every job that a run other than `boot`
    /// claimed, and returns how many. This server holds the data
    /// directory's lock, so that run is provably gone. A job that was
    /// asked to stop is cancelled instead.
    pub fn requeue_jobs_of_other_runs(&self, boot: Id) -> Result<usize, Error> {
        self.transact(move |transaction, now_ms, _| {
            transaction.execute(
                "UPDATE job SET state = 'cancelled', finished_at_ms = ?2, claimed_boot_id = NULL
                 WHERE state = 'cancelling' AND claimed_boot_id IS NOT ?1",
                (boot.as_bytes(), now_ms),
            )?;
            Ok(transaction.execute(
                "UPDATE job SET state = 'queued', claimed_boot_id = NULL
                 WHERE state = 'running' AND claimed_boot_id IS NOT ?1",
                [boot.as_bytes()],
            )?)
        })
    }

    /// Claims the oldest queued job for the run `boot`.
    pub fn claim_job(&self, boot: Id) -> Result<Option<Job>, Error> {
        self.transact(move |transaction, now_ms, _| {
            let next: Option<i64> = transaction
                .query_row(
                    "SELECT id FROM job WHERE state = 'queued' ORDER BY id LIMIT 1",
                    [],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(row) = next else { return Ok(None) };
            transaction.execute(
                "UPDATE job SET state = 'running', claimed_boot_id = ?2, heartbeat_at_ms = ?3,
                                started_at_ms = coalesce(started_at_ms, ?3)
                 WHERE id = ?1",
                (row, boot.as_bytes(), now_ms),
            )?;
            Ok(Some(job_by_row(transaction, row)?))
        })
    }

    /// Saves a job's counters and its checkpoint, and with them its
    /// heartbeat. It returns the job as it then is, which says whether it
    /// has been asked to stop.
    pub fn save_progress(&self, job: i64, progress: Progress) -> Result<Job, Error> {
        self.transact(move |transaction, now_ms, _| {
            transaction.execute(
                "UPDATE job SET step = ?2, checkpoint_json = coalesce(?3, checkpoint_json), done = ?4,
                                total = ?5, bytes = ?6, failed_count = ?7, skipped_count = ?8,
                                current_label = ?9, heartbeat_at_ms = ?10
                 WHERE id = ?1",
                (
                    job,
                    progress.step,
                    &progress.checkpoint_json,
                    progress.done,
                    progress.total,
                    progress.bytes,
                    progress.failed,
                    progress.skipped,
                    &progress.label,
                    now_ms,
                ),
            )?;
            job_by_row(transaction, job)
        })
    }

    /// Records the outcome of items of a job: what each was, its state
    /// (`succeeded`, `failed` or `skipped`) and the detail as JSON.
    pub fn add_job_items(
        &self,
        job: i64,
        items: Vec<(String, &'static str, String)>,
    ) -> Result<(), Error> {
        if items.is_empty() {
            return Ok(());
        }
        self.transact(move |transaction, _, _| {
            let mut insert = transaction
                .prepare("INSERT INTO job_item (job_id, item_ref, state, detail_json) VALUES (?1, ?2, ?3, ?4)")?;
            for (item, state, detail) in &items {
                insert.execute((job, item, state, detail))?;
            }
            Ok(())
        })
    }

    /// Ends a job. A job already asked to stop never ends as succeeded:
    /// it ends as cancelled instead. A job that has already ended is
    /// left as it ended.
    pub fn finish_job(
        &self,
        job: i64,
        state: JobState,
        error: Option<String>,
    ) -> Result<Job, Error> {
        let ended = self
            .transact(move |transaction, now_ms, _| end(transaction, job, state, error, now_ms))?;
        self.forget_cancel(job);
        Ok(ended)
    }

    /// Ends a job that succeeded, run with the params `ran`, as
    /// [`finish_job`](Self::finish_job) does, unless they were upgraded
    /// while it ran ([`start_job_upgrading`](Self::start_job_upgrading)):
    /// then it is left running, and `Err` holds the params to run it again
    /// with. The check and the end are one transaction, so an upgrade
    /// either comes before the end and is run, or finds the job ended and
    /// starts a new one.
    pub fn finish_succeeded_job(&self, job: i64, ran: &str) -> Result<Result<Job, String>, Error> {
        let ran = ran.to_owned();
        let ended = self.transact(move |transaction, now_ms, _| {
            let current = job_by_row(transaction, job)?;
            if !current.state.ended() && !current.cancel_requested && current.params_json != ran {
                return Ok(Err(current.params_json));
            }
            end(transaction, job, JobState::Succeeded, None, now_ms).map(Ok)
        })?;
        if ended.is_ok() {
            self.forget_cancel(job);
        }
        Ok(ended)
    }

    fn forget_cancel(&self, job: i64) {
        self.cancels
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&job);
    }

    /// Whether this job has been asked to stop.
    pub fn cancel_requested(&self, job: i64) -> Result<bool, Error> {
        self.read(|connection| {
            Ok(connection
                .query_row(
                    "SELECT cancel_requested_at_ms IS NOT NULL FROM job WHERE id = ?1",
                    [job],
                    |row| row.get(0),
                )
                .optional()?
                .unwrap_or(false))
        })
    }

    /// A live handle for a claimed job, checked before each write begins
    /// its transaction. Sees both cancels recorded already and any
    /// acknowledged while the job runs.
    pub fn job_cancel(&self, job: i64, shutdown: Arc<AtomicBool>) -> Result<JobCancel, Error> {
        if self.cancel_requested(job)? {
            self.cancels
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(job);
        }
        Ok(JobCancel {
            job,
            cancels: Arc::clone(&self.cancels),
            shutdown,
        })
    }

    /// Carries out the command that asks a job to stop, and returns the
    /// job, or `None` if there is no such job. A queued job is cancelled
    /// at once; a running one stops after the step in flight. Asking again,
    /// or asking a job that has ended, changes nothing.
    pub fn cancel_job(
        &self,
        command: Command,
        method: &'static str,
        id: Id,
    ) -> Result<Option<Job>, Error> {
        let updated = self.transact(move |transaction, now_ms, _| {
            let Some(found) = job_by_id(transaction, id)? else {
                return Ok(None);
            };
            if receipt(transaction, &command)?.is_none() {
                transaction.execute(
                    "UPDATE job SET cancel_requested_at_ms = coalesce(cancel_requested_at_ms, ?2),
                                    state = CASE state WHEN 'queued' THEN 'cancelled'
                                                       WHEN 'running' THEN 'cancelling' ELSE state END,
                                    finished_at_ms = CASE state WHEN 'queued' THEN ?2 ELSE finished_at_ms END
                     WHERE id = ?1 AND state IN ('queued', 'running', 'cancelling')",
                    (found.id, now_ms),
                )?;
                record_receipt(transaction, &command, method, &id.to_string(), now_ms)?;
            }
            Ok(Some(job_by_row(transaction, found.id)?))
        })?;
        // Publish after commit and before returning so a failed cancel never
        // stops a running job, while any acknowledged cancel is visible before
        // the caller or event stream sees it.
        if let Some(job) = &updated
            && job.state == JobState::Cancelling
        {
            self.cancels
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(job.id);
        }
        Ok(updated)
    }

    /// The job a command started before, if it did: what the command sent
    /// again is answered with, whatever has become of what it started from.
    /// The same ID with other params is [`Error::CommandMismatch`].
    pub fn job_of_command(&self, command: Command) -> Result<Option<Job>, Error> {
        self.read(move |connection| match receipt(connection, &command)? {
            Some(result) => {
                let id = result.parse().map_err(|_| Error::Corrupt("a receipt"))?;
                job_by_id(connection, id)
            }
            None => Ok(None),
        })
    }

    /// The job with this ID.
    pub fn job(&self, id: Id) -> Result<Option<Job>, Error> {
        self.read(|connection| job_by_id(connection, id))
    }

    /// The newest jobs, newest first, at most `limit` of them.
    pub fn jobs(&self, limit: u32) -> Result<Vec<Job>, Error> {
        self.read(|connection| {
            let mut statement = connection.prepare(&format!(
                "SELECT {COLUMNS} FROM job ORDER BY id DESC LIMIT ?1"
            ))?;
            let rows = statement.query_map([limit], job)?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
    }

    /// The job of this kind and key that is queued, running or stopping,
    /// if there is one: there is at most one.
    pub fn active_job(&self, kind: &str, dedupe_key: &str) -> Result<Option<Job>, Error> {
        self.read(|connection| {
            Ok(connection
                .query_row(
                    &format!(
                        "SELECT {COLUMNS} FROM job WHERE kind = ?1 AND dedupe_key = ?2
                           AND state IN ('queued', 'running', 'cancelling')"
                    ),
                    (kind, dedupe_key),
                    job,
                )
                .optional()?)
        })
    }

    /// Whether a job of this kind is running: claimed and not yet ended.
    pub fn running(&self, kind: &str) -> Result<bool, Error> {
        self.read(|connection| {
            Ok(connection.query_row(
                "SELECT EXISTS (SELECT 1 FROM job WHERE kind = ?1 AND state IN ('running', 'cancelling'))",
                [kind],
                |row| row.get(0),
            )?)
        })
    }

    /// The job of this kind and key that ended last, however it ended.
    pub fn last_ended_job(&self, kind: &str, dedupe_key: &str) -> Result<Option<Job>, Error> {
        self.read(|connection| {
            Ok(connection
                .query_row(
                    &format!(
                        "SELECT {COLUMNS} FROM job WHERE kind = ?1 AND dedupe_key = ?2
                           AND finished_at_ms IS NOT NULL
                         ORDER BY finished_at_ms DESC, id DESC LIMIT 1"
                    ),
                    (kind, dedupe_key),
                    job,
                )
                .optional()?)
        })
    }

    /// The newest job of this kind and key that the server started on its
    /// own, however far it has got: one whose params name its `trigger`.
    pub fn last_automatic_job(&self, kind: &str, dedupe_key: &str) -> Result<Option<Job>, Error> {
        self.read(|connection| {
            Ok(connection
                .query_row(
                    &format!(
                        "SELECT {COLUMNS} FROM job WHERE kind = ?1 AND dedupe_key = ?2
                           AND json_extract(params_json, '$.trigger') IS NOT NULL
                         ORDER BY id DESC LIMIT 1"
                    ),
                    (kind, dedupe_key),
                    job,
                )
                .optional()?)
        })
    }

    /// The items of a job that failed: what each was and the detail, at
    /// most `limit` of them.
    pub fn failed_job_items(&self, job: i64, limit: u32) -> Result<Vec<(String, String)>, Error> {
        self.read(|connection| {
            let mut statement = connection.prepare(
                "SELECT item_ref, coalesce(detail_json, '') FROM job_item
                 WHERE job_id = ?1 AND state = 'failed' ORDER BY id LIMIT ?2",
            )?;
            let rows = statement.query_map((job, limit), |row| Ok((row.get(0)?, row.get(1)?)))?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::Fingerprint;
    use crate::testing::{CountingEntropy, FixedClock};

    const T0: i64 = 1_763_000_000_000;

    fn store(clock: &Arc<FixedClock>) -> ServerStore {
        let store =
            ServerStore::open_in_memory(clock.clone(), Arc::new(CountingEntropy::default()))
                .unwrap();
        store.ensure_identity("desk").unwrap();
        store
    }

    fn id(byte: u8) -> Id {
        Id::from_bytes([byte; 16])
    }

    fn command(byte: u8, params: &str) -> Command {
        Command {
            id: id(byte),
            fingerprint: Fingerprint::of("scan.start", params.as_bytes()),
        }
    }

    fn scan(root: &str) -> NewJob {
        NewJob {
            kind: "scan",
            dedupe_key: root.to_owned(),
            target_kind: "source",
            target: id(0x50),
            params_json: "{}".to_owned(),
            session: None,
        }
    }

    #[test]
    fn a_root_is_a_name_and_a_path_and_a_name_keeps_its_path() {
        let store = store(&Arc::new(FixedClock::at(T0)));
        store.add_root("main", "/srv/invented").unwrap();
        store.add_root("main", "/srv/invented").unwrap();
        let refused = store.add_root("main", "/srv/elsewhere");
        assert!(
            matches!(&refused, Err(Error::RootTaken { name }) if name == "main"),
            "{refused:?}"
        );
        store.add_root("other", "/srv/elsewhere").unwrap();
        assert_eq!(
            store.roots().unwrap(),
            [
                ("main".to_owned(), "/srv/invented".to_owned()),
                ("other".to_owned(), "/srv/elsewhere".to_owned())
            ]
        );
    }

    #[test]
    fn a_receipt_sweep_keeps_recent_commands_and_jobs_still_in_progress() {
        let clock = Arc::new(FixedClock::at(T0));
        let store = store(&clock);
        let start = |key, root| {
            store
                .start_job(command(key, "a"), "scan.start", scan(root))
                .unwrap()
                .0
        };
        let old = start(1, "old");
        store.finish_job(old.id, JobState::Succeeded, None).unwrap();
        let active = start(2, "active");
        clock.set(T0 + 1);
        let recent = start(3, "recent");
        store
            .finish_job(recent.id, JobState::Succeeded, None)
            .unwrap();
        clock.set(T0 + RECEIPT_LIFE_MS + 1);
        // Reach the normal insertion that triggers the sweep.
        for byte in 4..=255 {
            start(byte, "active");
        }
        start(0, "active");
        assert!(store.job_of_command(command(1, "a")).unwrap().is_none());
        assert_eq!(
            store
                .job_of_command(command(2, "a"))
                .unwrap()
                .map(|job| job.public_id),
            Some(active.public_id)
        );
        assert_eq!(start(3, "recent").public_id, recent.public_id);
        assert!(matches!(
            store.job_of_command(command(3, "different")),
            Err(Error::CommandMismatch { .. })
        ));
    }

    #[test]
    fn handoff_recovery_keeps_its_evidence_past_the_retry_window() {
        let clock = Arc::new(FixedClock::at(T0));
        let store = store(&clock);
        let movement = ritornel_contract::PlaybackMoveParams {
            idempotency_key: id(1),
            from: id(20),
            to: id(21),
            entry_id: id(22),
        };
        let pending = crate::playback::PendingMove {
            movement,
            attempt: id(10),
        };
        store.begin_move(command(1, "move")).unwrap();
        store.expect_load(pending).unwrap();
        for (byte, method) in [
            (2, "link.handoff.load"),
            (3, "link.handoff.load"),
            (4, "link.handoff.take"),
        ] {
            store
                .record_command(command(byte, method), method, "null".to_owned(), None)
                .unwrap();
        }
        clock.set(T0 + RECEIPT_LIFE_MS + 1);
        let kept = |byte, method| {
            store
                .stored_result(command(byte, method))
                .unwrap()
                .is_some()
        };
        store.prune_receipts().unwrap();
        assert!(kept(1, "move"));
        assert!(!kept(2, "link.handoff.load"));
        assert!(kept(3, "link.handoff.load"));
        assert!(kept(4, "link.handoff.take"));

        store.forget_move(pending).unwrap();
        store
            .record_command(
                command(5, "queue.clear"),
                "queue.clear",
                "null".to_owned(),
                None,
            )
            .unwrap();
        store.prune_receipts().unwrap();
        assert!(!kept(1, "move"));
        assert!(!kept(4, "link.handoff.take"));
    }

    #[test]
    fn one_root_has_one_active_scan_and_a_command_sent_again_gets_its_job() {
        let store = store(&Arc::new(FixedClock::at(T0)));
        let session = store.local_session().unwrap();
        let (first, created) = store
            .start_job(
                command(1, "a"),
                "scan.start",
                NewJob {
                    session,
                    ..scan("main")
                },
            )
            .unwrap();
        assert!(created);
        assert_eq!(
            (first.state, first.kind.as_str(), first.target),
            (JobState::Queued, "scan", Some(id(0x50)))
        );
        // Sent again, the command returns its job and starts nothing.
        assert_eq!(
            store
                .start_job(command(1, "a"), "scan.start", scan("main"))
                .unwrap(),
            (first.clone(), false)
        );
        let refused = store.start_job(command(1, "b"), "scan.start", scan("main"));
        assert!(
            matches!(refused, Err(Error::CommandMismatch { .. })),
            "{refused:?}"
        );
        // Another command for the same root gets the job that is running.
        assert_eq!(
            store
                .start_job(command(2, "a"), "scan.start", scan("main"))
                .unwrap(),
            (first.clone(), false)
        );
        // Another root has a scan of its own.
        let (other, created) = store
            .start_job(command(3, "a"), "scan.start", scan("other"))
            .unwrap();
        assert!(created && other.public_id != first.public_id);
        assert_eq!(store.jobs(10).unwrap(), [other, first.clone()]);

        // Once it has ended, the root can be scanned again.
        store
            .finish_job(first.id, JobState::Succeeded, None)
            .unwrap();
        let (again, created) = store
            .start_job(command(4, "a"), "scan.start", scan("main"))
            .unwrap();
        assert!(created && again.public_id != first.public_id);
    }

    #[test]
    fn more_asked_of_a_queued_or_running_job_is_run_and_never_dropped() {
        let store = store(&Arc::new(FixedClock::at(T0)));
        let ask = |n: u8, params: &'static str| {
            store
                .start_job_upgrading(
                    command(n, params),
                    "scan.start",
                    NewJob {
                        params_json: params.to_owned(),
                        ..scan("main")
                    },
                    move |active| (active != "{\"full\":true}").then(|| params.to_owned()),
                )
                .unwrap()
        };
        // Queued, it is claimed with what was asked of it since.
        let (queued, created) = ask(1, "{}");
        assert!(created);
        let (same, created) = ask(2, "{\"full\":true}");
        assert_eq!((same.id, created), (queued.id, false));
        let claimed = store.claim_job(id(0xb0)).unwrap().unwrap();
        assert_eq!(claimed.params_json, "{\"full\":true}");
        assert_eq!(
            store
                .finish_succeeded_job(claimed.id, &claimed.params_json)
                .unwrap()
                .map(|job| job.state),
            Ok(JobState::Succeeded)
        );

        // Running, it does not end as it ran: it runs again as asked.
        let (running, _) = ask(3, "{}");
        let running = {
            let claimed = store.claim_job(id(0xb0)).unwrap().unwrap();
            assert_eq!(claimed.id, running.id);
            claimed
        };
        let (same, created) = ask(4, "{\"full\":true}");
        assert_eq!((same.id, created), (running.id, false));
        let again = store
            .finish_succeeded_job(running.id, &running.params_json)
            .unwrap();
        assert_eq!(again, Err("{\"full\":true}".to_owned()));
        assert_eq!(
            store.job(running.public_id).unwrap().unwrap().state,
            JobState::Running
        );
        // Asked again for what it runs already, it changes nothing more.
        ask(5, "{\"full\":true}");
        let ended = store
            .finish_succeeded_job(running.id, "{\"full\":true}")
            .unwrap()
            .unwrap();
        assert_eq!(ended.state, JobState::Succeeded);

        // Asked of a job that has ended, it is a job of its own.
        let (next, created) = ask(6, "{\"full\":true}");
        assert!(created && next.id != running.id);

        // Asked of one being cancelled, it is refused, and kept for no job:
        // asked again once that job has ended, it is a job of its own.
        store
            .finish_job(next.id, JobState::Succeeded, None)
            .unwrap();
        let (dying, _) = ask(7, "{}");
        store
            .cancel_job(command(8, "cancel"), "jobs.cancel", dying.public_id)
            .unwrap();
        let claimed = store.claim_job(id(0xb0)).unwrap();
        assert!(claimed.is_none(), "a queued job cancelled ends at once");
        let (dying, _) = ask(9, "{}");
        store.claim_job(id(0xb0)).unwrap().unwrap();
        store
            .cancel_job(command(10, "cancel"), "jobs.cancel", dying.public_id)
            .unwrap();
        let asked = store.start_job_upgrading(
            command(11, "{\"full\":true}"),
            "scan.start",
            NewJob {
                params_json: "{\"full\":true}".to_owned(),
                ..scan("main")
            },
            |_| Some("{\"full\":true}".to_owned()),
        );
        assert!(matches!(asked, Err(Error::JobEnding)), "{asked:?}");
        store
            .finish_job(dying.id, JobState::Cancelled, None)
            .unwrap();
        let (fresh, created) = ask(11, "{\"full\":true}");
        assert!(created && fresh.id != dying.id);
    }

    #[test]
    fn the_last_automatic_job_is_the_newest_whose_params_name_a_trigger() {
        let store = store(&Arc::new(FixedClock::at(T0)));
        assert_eq!(store.last_automatic_job("scan", "main").unwrap(), None);
        let (manual, _) = store
            .start_job(command(1, "a"), "scan.start", scan("main"))
            .unwrap();
        assert_eq!(store.last_automatic_job("scan", "main").unwrap(), None);
        store
            .finish_job(manual.id, JobState::Succeeded, None)
            .unwrap();
        let automatic = NewJob {
            params_json: r#"{"trigger":"watch"}"#.to_owned(),
            ..scan("main")
        };
        let (started, _) = store
            .start_job(command(2, "b"), "scan.start", automatic)
            .unwrap();
        assert_eq!(
            store.last_automatic_job("scan", "main").unwrap(),
            Some(started)
        );
        assert_eq!(store.last_automatic_job("scan", "other").unwrap(), None);
    }

    #[test]
    fn watching_is_on_until_it_is_turned_off() {
        let store = store(&Arc::new(FixedClock::at(T0)));
        assert!(store.watching().unwrap());
        store.set_watching(false).unwrap();
        assert!(!store.watching().unwrap());
        store.set_watching(true).unwrap();
        assert!(store.watching().unwrap());
    }

    #[test]
    fn the_active_job_and_the_last_to_end_are_found_by_kind_and_key() {
        let clock = Arc::new(FixedClock::at(T0));
        let store = store(&clock);
        assert_eq!(store.active_job("scan", "main").unwrap(), None);
        assert_eq!(store.last_ended_job("scan", "main").unwrap(), None);
        let (first, _) = store
            .start_job(command(1, "a"), "scan.start", scan("main"))
            .unwrap();
        store
            .start_job(command(2, "a"), "scan.start", scan("other"))
            .unwrap();
        assert_eq!(
            store.active_job("scan", "main").unwrap(),
            Some(first.clone())
        );
        assert_eq!(store.last_ended_job("scan", "main").unwrap(), None);

        clock.set(T0 + 50);
        let failed = store
            .finish_job(first.id, JobState::Failed, Some("offline".to_owned()))
            .unwrap();
        assert_eq!(store.active_job("scan", "main").unwrap(), None);
        assert_eq!(store.last_ended_job("scan", "main").unwrap(), Some(failed));

        let (second, _) = store
            .start_job(command(3, "a"), "scan.start", scan("main"))
            .unwrap();
        clock.set(T0 + 90);
        let succeeded = store
            .finish_job(second.id, JobState::Succeeded, None)
            .unwrap();
        let last = store.last_ended_job("scan", "main").unwrap().unwrap();
        assert_eq!(
            (last.finished_at_ms, last.state),
            (Some(T0 + 90), JobState::Succeeded)
        );
        assert_eq!(last, succeeded);
        assert_eq!(store.last_ended_job("scan", "other").unwrap(), None);
    }

    #[test]
    fn a_job_is_claimed_checkpointed_and_requeued_when_its_run_is_gone() {
        let clock = Arc::new(FixedClock::at(T0));
        let store = store(&clock);
        let (created, _) = store
            .start_job(command(1, "a"), "scan.start", scan("main"))
            .unwrap();
        assert_eq!(store.requeue_jobs_of_other_runs(id(0xb1)).unwrap(), 0);

        clock.set(T0 + 10);
        let claimed = store.claim_job(id(0xb1)).unwrap().unwrap();
        assert_eq!(
            (claimed.state, claimed.started_at_ms),
            (JobState::Running, Some(T0 + 10))
        );
        assert_eq!(store.claim_job(id(0xb1)).unwrap(), None);
        let saved = store
            .save_progress(
                claimed.id,
                Progress {
                    step: 3,
                    checkpoint_json: Some("{\"at\":3}".to_owned()),
                    done: 1500,
                    total: Some(14_000),
                    bytes: 9,
                    failed: 2,
                    skipped: 4,
                    label: Some("a/b.flac".to_owned()),
                },
            )
            .unwrap();
        assert_eq!(
            (saved.step, saved.done, saved.total),
            (3, 1500, Some(14_000))
        );
        assert_eq!((saved.failed, saved.skipped, saved.bytes), (2, 4, 9));
        // A save with no checkpoint keeps the last one.
        let saved = store
            .save_progress(
                claimed.id,
                Progress {
                    step: 3,
                    ..Progress::default()
                },
            )
            .unwrap();
        assert_eq!(saved.checkpoint_json.as_deref(), Some("{\"at\":3}"));

        // The run that claimed it leaves its own job alone. Another run
        // finds the lock free, so that one is gone, and takes the job back.
        assert_eq!(store.requeue_jobs_of_other_runs(id(0xb1)).unwrap(), 0);
        assert_eq!(store.requeue_jobs_of_other_runs(id(0xb2)).unwrap(), 1);
        let again = store.claim_job(id(0xb2)).unwrap().unwrap();
        assert_eq!(
            (again.public_id, again.step, again.started_at_ms),
            (created.public_id, 3, Some(T0 + 10))
        );
        assert_eq!(again.checkpoint_json.as_deref(), Some("{\"at\":3}"));
    }

    #[test]
    fn cancelling_stops_a_queued_job_at_once_and_asks_a_running_one() {
        let clock = Arc::new(FixedClock::at(T0));
        let store = store(&clock);
        let (queued, _) = store
            .start_job(command(1, "a"), "scan.start", scan("main"))
            .unwrap();
        let cancelled = store
            .cancel_job(command(9, "x"), "jobs.cancel", queued.public_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            (cancelled.state, cancelled.finished_at_ms),
            (JobState::Cancelled, Some(T0))
        );
        assert_eq!(
            store
                .cancel_job(command(8, "x"), "jobs.cancel", id(0x77))
                .unwrap(),
            None
        );

        let (running, _) = store
            .start_job(command(2, "a"), "scan.start", scan("main"))
            .unwrap();
        store.claim_job(id(0xb1)).unwrap().unwrap();
        let asked = store
            .cancel_job(command(7, "x"), "jobs.cancel", running.public_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            (asked.state, asked.cancel_requested),
            (JobState::Cancelling, true)
        );
        // The worker sees the request when it saves its progress.
        assert!(
            store
                .save_progress(running.id, Progress::default())
                .unwrap()
                .cancel_requested
        );
        let ended = store
            .finish_job(running.id, JobState::Cancelled, None)
            .unwrap();
        assert!(ended.state.ended());
        // A job that was asked to stop, and whose run then died, is
        // cancelled when the next run starts.
        let (third, _) = store
            .start_job(command(3, "a"), "scan.start", scan("main"))
            .unwrap();
        store.claim_job(id(0xb1)).unwrap().unwrap();
        store
            .cancel_job(command(6, "x"), "jobs.cancel", third.public_id)
            .unwrap();
        store.requeue_jobs_of_other_runs(id(0xb2)).unwrap();
        assert_eq!(
            store.job(third.public_id).unwrap().unwrap().state,
            JobState::Cancelled
        );
    }

    #[test]
    fn a_cancelling_job_never_finishes_as_succeeded_and_ended_jobs_stay_ended() {
        let clock = Arc::new(FixedClock::at(T0));
        let store = store(&clock);
        let (running, _) = store
            .start_job(command(1, "a"), "scan.start", scan("main"))
            .unwrap();
        assert!(!store.cancel_requested(running.id).unwrap());
        store.claim_job(id(0xb1)).unwrap().unwrap();
        store
            .cancel_job(command(2, "x"), "jobs.cancel", running.public_id)
            .unwrap()
            .unwrap();
        assert!(store.cancel_requested(running.id).unwrap());

        clock.set(T0 + 10);
        let ended = store
            .finish_job(running.id, JobState::Succeeded, None)
            .unwrap();
        assert_eq!(
            (ended.state, ended.error.as_deref(), ended.finished_at_ms),
            (JobState::Cancelled, None, Some(T0 + 10))
        );
        // Once ended, a second finish changes neither its state nor its time.
        clock.set(T0 + 20);
        let again = store
            .finish_job(running.id, JobState::Succeeded, None)
            .unwrap();
        assert_eq!(
            (again.state, again.finished_at_ms),
            (JobState::Cancelled, Some(T0 + 10))
        );

        // A real failure on a cancelling job still records the failure.
        let (failing, _) = store
            .start_job(command(3, "a"), "scan.start", scan("main"))
            .unwrap();
        store.claim_job(id(0xb1)).unwrap().unwrap();
        store
            .cancel_job(command(4, "x"), "jobs.cancel", failing.public_id)
            .unwrap()
            .unwrap();
        let failed = store
            .finish_job(failing.id, JobState::Failed, Some("disk full".to_owned()))
            .unwrap();
        assert_eq!(
            (failed.state, failed.error.as_deref()),
            (JobState::Failed, Some("disk full"))
        );
    }

    #[test]
    fn a_steps_command_id_is_the_same_for_the_same_job_and_step_and_for_no_other() {
        let a = step_command(id(1), 13);
        assert_eq!(a, step_command(id(1), 13));
        assert_ne!(a, step_command(id(1), 14));
        assert_ne!(a, step_command(id(2), 13));
    }

    #[test]
    fn what_failed_in_a_job_can_be_listed() {
        let store = store(&Arc::new(FixedClock::at(T0)));
        let (job, _) = store
            .start_job(command(1, "a"), "scan.start", scan("main"))
            .unwrap();
        store
            .add_job_items(
                job.id,
                vec![
                    (
                        "a/1.flac".to_owned(),
                        "failed",
                        "{\"kind\":\"io\"}".to_owned(),
                    ),
                    ("a/2.flac".to_owned(), "skipped", "{}".to_owned()),
                ],
            )
            .unwrap();
        assert_eq!(
            store.failed_job_items(job.id, 10).unwrap(),
            [("a/1.flac".to_owned(), "{\"kind\":\"io\"}".to_owned())]
        );
    }
}
