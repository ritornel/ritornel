//! The import calls: a run of streams staged in pieces; its dry run, its
//! apply and its undo, each a job; the run with its report; and the runs
//! the library holds. See "Spotify streams" in docs/internals/history.md.

use ritornel_contract::methods::{ImportsApply, ImportsDryRun, ImportsUndo, Method};
use ritornel_contract::{
    AppliedRun, ErrorCode, ImportRun, ImportRuns, ImportsGetParams, ImportsJobStarted,
    ImportsRunParams, ImportsStageParams, ImportsStaged, Receipt, ReceiptStatus, RpcError,
};
use ritornel_engine::{Command, NewJob, StoredRun};

use super::{Core, Stores, fingerprint};
use crate::dispatch::calls::job;
use crate::dispatch::playback::refused;
use crate::imports::{APPLY_JOB, DRY_RUN_JOB, Refused, RunJobParams, Staging, UNDO_JOB, stale};

fn no_run() -> RpcError {
    RpcError::new(
        ErrorCode::NotFound,
        "there is no such staged run: it was never sent, or it is more than a day old, or the \
         server has started since",
    )
}

/// Refuses a call on a server that holds no library. A satellite sends
/// these calls on to its hub, so this is a server with neither.
fn no_library(stores: &Stores) -> Result<(), RpcError> {
    Core::library(stores, |_| Ok(())).map_err(|_| {
        RpcError::new(
            ErrorCode::Invalid,
            "this server holds no library: import on the library's hub",
        )
    })
}

fn internal(error: impl std::fmt::Display) -> RpcError {
    RpcError::new(ErrorCode::Internal, error.to_string())
}

fn unsigned(number: i64) -> u64 {
    u64::try_from(number).unwrap_or(0)
}

/// A run as the contract carries it.
fn applied(stored: StoredRun) -> AppliedRun {
    AppliedRun {
        run_id: stored.run,
        streams: unsigned(stored.streams),
        new_streams: unsigned(stored.new_streams),
        in_force: stored.in_force,
        applied_at: stored.applied_at_ms,
        undone_at: stored.undone_at_ms,
        reader: stored.reader,
    }
}

impl Core {
    /// The runs of streams staged for a dry run.
    pub(crate) fn staging(&self) -> &Staging {
        &self.staging
    }

    pub(super) fn imports_stage(
        &self,
        stores: &Stores,
        params: &ImportsStageParams,
    ) -> Result<ImportsStaged, RpcError> {
        no_library(stores)?;
        let now = self.clock.now_ms();
        let run = self
            .staging
            .stage(
                params.run_id,
                params.piece,
                params.pieces,
                &params.reader,
                &params.streams,
                now,
            )
            .map_err(|refusal| match refusal {
                Refused::Invalid(why) => RpcError::new(ErrorCode::Invalid, why),
                Refused::Conflict => RpcError::new(
                    ErrorCode::Conflict,
                    "that piece of the run was sent before with other streams",
                ),
                Refused::Disk(error) => internal(error),
            })?;
        Ok(ImportsStaged {
            receipt: Receipt {
                command_id: params.idempotency_key,
                status: ReceiptStatus::Done,
                revision: None,
            },
            run,
        })
    }

    /// Starts a run's job, or returns the one of that kind running for it,
    /// or the one this command started before.
    fn start_run_job<M: Method<Params = ImportsRunParams>>(
        &self,
        stores: &Stores,
        params: &ImportsRunParams,
        kind: &'static str,
    ) -> Result<ImportsJobStarted, RpcError> {
        let command = Command {
            id: params.idempotency_key,
            fingerprint: fingerprint::<M>(params)?,
        };
        let session = self.local_credential.session();
        let new = NewJob {
            kind,
            dedupe_key: params.run_id.to_string(),
            target: params.run_id,
            params_json: serde_json::to_string(&RunJobParams {
                run_id: params.run_id,
                session,
            })
            .unwrap_or_default(),
            session,
        };
        let (started, created) = stores
            .server()
            .start_job(command, M::NAME, new)
            .map_err(refused)?;
        if created {
            self.publish_stored_job(stores, &started);
        }
        self.jobs.wake();
        Ok(ImportsJobStarted {
            receipt: Receipt {
                command_id: params.idempotency_key,
                status: ReceiptStatus::Accepted,
                revision: None,
            },
            job: job(&started),
        })
    }

    /// A staged run that holds every piece, or why there is none.
    fn whole_run(&self, params: &ImportsRunParams) -> Result<ImportRun, RpcError> {
        let run = self
            .staging
            .run(params.run_id, self.clock.now_ms())
            .map_err(internal)?
            .ok_or_else(no_run)?;
        if run.received != run.pieces {
            return Err(RpcError::new(
                ErrorCode::Invalid,
                "the run is missing pieces: send every piece first",
            )
            .with_detail("received", run.received.to_string())
            .with_detail("pieces", run.pieces.to_string()));
        }
        Ok(run)
    }

    pub(super) fn imports_dry_run(
        &self,
        stores: &Stores,
        params: &ImportsRunParams,
    ) -> Result<ImportsJobStarted, RpcError> {
        no_library(stores)?;
        self.whole_run(params)?;
        self.start_run_job::<ImportsDryRun>(stores, params, DRY_RUN_JOB)
    }

    pub(super) fn imports_apply(
        &self,
        stores: &Stores,
        params: &ImportsRunParams,
    ) -> Result<ImportsJobStarted, RpcError> {
        no_library(stores)?;
        let run = self.whole_run(params)?;
        if let Some(why) = stale(run.report.as_ref()) {
            return Err(RpcError::new(ErrorCode::Invalid, why).with_detail("reason", "dry_run"));
        }
        self.start_run_job::<ImportsApply>(stores, params, APPLY_JOB)
    }

    pub(super) fn imports_undo(
        &self,
        stores: &Stores,
        params: &ImportsRunParams,
    ) -> Result<ImportsJobStarted, RpcError> {
        let stored = Self::library(stores, |library| {
            library.stored_run(params.run_id).map_err(internal)
        })?;
        if stored.is_none() {
            return Err(RpcError::new(
                ErrorCode::NotFound,
                "no run of that ID was applied to this library",
            ));
        }
        self.start_run_job::<ImportsUndo>(stores, params, UNDO_JOB)
    }

    pub(super) fn imports_get(
        &self,
        stores: &Stores,
        params: &ImportsGetParams,
    ) -> Result<ImportRun, RpcError> {
        let mut run = self
            .staging
            .run(params.run_id, self.clock.now_ms())
            .map_err(internal)?
            .ok_or_else(no_run)?;
        run.applied = Self::library(stores, |library| {
            library.stored_run(params.run_id).map_err(internal)
        })?
        .map(applied);
        Ok(run)
    }

    pub(super) fn imports_list(&self, stores: &Stores) -> Result<ImportRuns, RpcError> {
        let runs = Self::library(stores, |library| library.stored_runs().map_err(internal))?;
        Ok(ImportRuns {
            runs: runs.into_iter().map(applied).collect(),
        })
    }
}
