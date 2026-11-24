//! The import calls: a run of streams staged in pieces, its dry run as a
//! job, and the run with its report. See "Spotify streams" in
//! docs/internals/history.md.

use ritornel_contract::methods::{ImportsDryRun, Method};
use ritornel_contract::{
    ErrorCode, ImportRun, ImportsDryRunParams, ImportsDryRunStarted, ImportsGetParams,
    ImportsStageParams, ImportsStaged, Receipt, ReceiptStatus, RpcError,
};
use ritornel_engine::{Command, NewJob};

use super::{Core, Stores, fingerprint};
use crate::dispatch::calls::job;
use crate::dispatch::playback::refused;
use crate::imports::{DRY_RUN_JOB, DryRunParams, Refused, Staging};

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
    let held = stores
        .library
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .is_some();
    if held {
        Ok(())
    } else {
        Err(RpcError::new(
            ErrorCode::Invalid,
            "this server holds no library: import on the library's hub",
        ))
    }
}

fn internal(error: impl std::fmt::Display) -> RpcError {
    RpcError::new(ErrorCode::Internal, error.to_string())
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

    pub(super) fn imports_dry_run(
        &self,
        stores: &Stores,
        params: &ImportsDryRunParams,
    ) -> Result<ImportsDryRunStarted, RpcError> {
        let now = self.clock.now_ms();
        let run = self
            .staging
            .run(params.run_id, now)
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
        no_library(stores)?;
        let command = Command {
            id: params.idempotency_key,
            fingerprint: fingerprint::<ImportsDryRun>(params)?,
        };
        let new = NewJob {
            kind: DRY_RUN_JOB,
            dedupe_key: params.run_id.to_string(),
            target: params.run_id,
            params_json: serde_json::to_string(&DryRunParams {
                run_id: params.run_id,
            })
            .unwrap_or_default(),
            session: self.local_credential.session(),
        };
        let (started, created) = stores
            .server()
            .start_job(command, ImportsDryRun::NAME, new)
            .map_err(refused)?;
        if created {
            self.publish_stored_job(stores, &started);
        }
        self.jobs.wake();
        Ok(ImportsDryRunStarted {
            receipt: Receipt {
                command_id: params.idempotency_key,
                status: ReceiptStatus::Accepted,
                revision: None,
            },
            job: job(&started),
        })
    }

    pub(super) fn imports_get(&self, params: &ImportsGetParams) -> Result<ImportRun, RpcError> {
        self.staging
            .run(params.run_id, self.clock.now_ms())
            .map_err(internal)?
            .ok_or_else(no_run)
    }
}
