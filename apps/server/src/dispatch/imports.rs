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

    /// The job this command started before, if it did: a command sent again
    /// is answered with it before anything it needed is looked at again,
    /// since the staged run may have gone since.
    fn replayed<M: Method<Params = ImportsRunParams>>(
        &self,
        stores: &Stores,
        params: &ImportsRunParams,
    ) -> Result<Option<ImportsJobStarted>, RpcError> {
        let command = Command {
            id: params.idempotency_key,
            fingerprint: fingerprint::<M>(params)?,
        };
        let found = stores.server().job_of_command(command).map_err(refused)?;
        Ok(found.map(|started| ImportsJobStarted {
            receipt: Receipt {
                command_id: params.idempotency_key,
                status: ReceiptStatus::Accepted,
                revision: None,
            },
            job: job(&started),
        }))
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
            target_kind: "import_run",
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
        if let Some(replayed) = self.replayed::<ImportsDryRun>(stores, params)? {
            return Ok(replayed);
        }
        // An applied run keeps the report it was applied with.
        let applied = Self::library(stores, |library| {
            library.stored_run(params.run_id).map_err(internal)
        })?;
        if applied.is_some() {
            return Err(RpcError::new(
                ErrorCode::Invalid,
                "that run is applied, and keeps the report it was applied with",
            )
            .with_detail("reason", "applied"));
        }
        self.whole_run(params)?;
        self.start_run_job::<ImportsDryRun>(stores, params, DRY_RUN_JOB)
    }

    pub(super) fn imports_apply(
        &self,
        stores: &Stores,
        params: &ImportsRunParams,
    ) -> Result<ImportsJobStarted, RpcError> {
        no_library(stores)?;
        if let Some(replayed) = self.replayed::<ImportsApply>(stores, params)? {
            return Ok(replayed);
        }
        let run = self.whole_run(params)?;
        // An applied run needs no dry run: its job ends as done.
        let applied = Self::library(stores, |library| {
            library.stored_run(params.run_id).map_err(internal)
        })?;
        if applied.is_none()
            && let Some(why) = stale(run.report.as_ref())
        {
            return Err(RpcError::new(ErrorCode::Invalid, why).with_detail("reason", "dry_run"));
        }
        self.start_run_job::<ImportsApply>(stores, params, APPLY_JOB)
    }

    pub(super) fn imports_undo(
        &self,
        stores: &Stores,
        params: &ImportsRunParams,
    ) -> Result<ImportsJobStarted, RpcError> {
        no_library(stores)?;
        if let Some(replayed) = self.replayed::<ImportsUndo>(stores, params)? {
            return Ok(replayed);
        }
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
        no_library(stores)?;
        let mut run = self
            .staging
            .run(params.run_id, self.clock.now_ms())
            .map_err(internal)?
            .ok_or_else(no_run)?;
        run.applied = Self::library(stores, |library| {
            library.stored_run(params.run_id).map_err(internal)
        })?
        .map(applied);
        // An applied run shows what its apply did, or nothing: never its
        // dry run.
        if run.applied.is_some() {
            run.report = self.staging.applied_report(params.run_id);
        }
        Ok(run)
    }

    pub(super) fn imports_list(&self, stores: &Stores) -> Result<ImportRuns, RpcError> {
        no_library(stores)?;
        let runs = Self::library(stores, |library| library.stored_runs().map_err(internal))?;
        Ok(ImportRuns {
            runs: runs.into_iter().map(applied).collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use ritornel_contract::{
        CONTRACT_VERSION, ErrorCode, Frame, Id, ImportReport, Request, RequestId,
    };
    use ritornel_engine::{OsEntropy, ServerStore};
    use serde_json::{Value, json};

    use super::{Core, Stores};
    use crate::Bounds;
    use crate::data_dir::DataDir;
    use crate::imports::KEEP_MS;
    use crate::session::{Session, SessionGrant};
    use crate::testing::{Scratch, SetClock};

    /// 2026-11-16 12:00 UTC.
    const T0: i64 = 1_763_294_400_000;

    fn server(clock: &Arc<SetClock>) -> (Core, Scratch) {
        let scratch = Scratch::new();
        let dir = DataDir::prepare(scratch.path().join("data")).unwrap();
        let store = ServerStore::open_in_memory(clock.clone(), Arc::new(OsEntropy)).unwrap();
        let (identity, issued) = store.ensure_identity("desk").unwrap();
        let (_, verifier) = crate::credential::ensure(&store, &dir.credential(), issued).unwrap();
        let core = Core::new(
            Stores::new(store, None),
            &identity,
            &dir,
            verifier,
            clock.clone(),
            Arc::new(OsEntropy),
            Bounds::default(),
        )
        .unwrap();
        (core, scratch)
    }

    fn call(
        core: &Core,
        session: &Session,
        method: &str,
        params: Value,
    ) -> Result<Value, ErrorCode> {
        let request = Frame::Request(Request {
            id: RequestId::Number(1),
            method: method.to_owned(),
            params: Some(params),
        });
        let Some(Frame::Response(response)) = core.dispatch(session, request) else {
            panic!("a request gets a response");
        };
        response.outcome.map_err(|error| error.data.code)
    }

    fn id(byte: u8) -> String {
        Id::from_bytes([byte; 16]).to_string()
    }

    /// A server with a library, a session on it, and one stream staged as
    /// run 9, in a run of `pieces` pieces.
    fn staged(clock: &Arc<SetClock>, pieces: u32) -> (Core, Scratch, Arc<Session>) {
        let (core, scratch) = server(clock);
        let session = core.open_session(SessionGrant::OWNER).unwrap();
        let hello = json!({
            "name": "test", "kind": "cli", "version": "1", "contractVersion": CONTRACT_VERSION
        });
        call(&core, &session, "server.hello", hello).unwrap();
        call(
            &core,
            &session,
            "library.create",
            json!({ "idempotencyKey": id(1) }),
        )
        .unwrap();
        let stage = json!({
            "idempotencyKey": id(2), "runId": id(9), "piece": 0, "pieces": pieces,
            "reader": "ritornelctl 0.1.0", "streams": [stream()],
        });
        call(&core, &session, "imports.stage", stage).unwrap();
        (core, scratch, session)
    }

    fn stream() -> Value {
        json!({
            "endedAt": 1_552_684_449_000_i64, "msPlayed": 221_000,
            "trackUri": "spotify:track:0000000000000000000001", "title": "Low Tide",
            "artist": "Calla Brandt", "platform": "ios",
        })
    }

    /// A report of run 9 by this grouper, kept as its dry run's.
    fn dry_run(core: &Core, grouper: i64) {
        let run = Id::from_bytes([9; 16]);
        let (_, begun_at) = core.staging().streams(run, T0).unwrap().unwrap();
        let report: ImportReport = serde_json::from_value(json!({
            "madeAt": T0, "grouperVersion": grouper,
            "playRuleVersion": ritornel_engine::PLAY_RULE_VERSION, "streams": 1, "events": 1,
            "respelled": 0, "scrobbles": 0, "scrobblesExcluded": 0, "scrobblesEchoes": 0,
            "tiers": [], "years": [], "offline": 0, "offlineTimestamped": 0,
            "offlineSeconds": 0, "offlineMilliseconds": 0, "paused": 0, "problems": [],
            "samples": [],
        }))
        .unwrap();
        assert!(core.staging().save_report(run, begun_at, &report).unwrap());
    }

    fn apply(key: u8) -> Value {
        json!({ "idempotencyKey": id(key), "runId": id(9) })
    }

    #[test]
    fn an_apply_is_refused_without_every_piece_or_a_dry_run_by_the_grouper_in_force() {
        let clock = Arc::new(SetClock::at(T0));
        let (core, _scratch, session) = staged(&clock, 2);
        assert_eq!(
            call(&core, &session, "imports.apply", apply(3)),
            Err(ErrorCode::Invalid),
            "a piece is missing"
        );
        let (core, _scratch, session) = staged(&clock, 1);
        assert_eq!(
            call(&core, &session, "imports.apply", apply(3)),
            Err(ErrorCode::Invalid),
            "no dry run"
        );
        dry_run(&core, ritornel_engine::GROUPER_VERSION - 1);
        assert_eq!(
            call(&core, &session, "imports.apply", apply(4)),
            Err(ErrorCode::Invalid),
            "a dry run by another grouper"
        );
        dry_run(&core, ritornel_engine::GROUPER_VERSION);
        assert!(call(&core, &session, "imports.apply", apply(5)).is_ok());
    }

    #[test]
    fn a_run_applied_is_not_dry_run_again_and_shows_no_dry_run_as_what_it_did() {
        let clock = Arc::new(SetClock::at(T0));
        let (core, _scratch, session) = staged(&clock, 1);
        dry_run(&core, ritornel_engine::GROUPER_VERSION);
        let run = Id::from_bytes([9; 16]);
        let streams = core.staging().streams(run, T0).unwrap().unwrap().0;
        core.with_library(|_, library| {
            library.apply_streams(
                ritornel_engine::WriteRequest {
                    method: ritornel_engine::APPLY_METHOD,
                    command: None,
                    client: None,
                },
                run,
                streams,
                "ritornelctl 0.1.0".to_owned(),
                None,
            )
        })
        .unwrap()
        .unwrap();
        assert_eq!(
            call(&core, &session, "imports.dryRun", apply(6)),
            Err(ErrorCode::Invalid)
        );
        // Its apply's report was never kept: it shows none, and not its
        // dry run's.
        let got = call(&core, &session, "imports.get", json!({ "runId": id(9) })).unwrap();
        assert!(got["applied"].is_object());
        assert!(got.get("report").is_none(), "{got}");
        // Applied again, it needs no dry run of this grouper, and its job
        // ends as done.
        dry_run(&core, ritornel_engine::GROUPER_VERSION - 1);
        assert!(call(&core, &session, "imports.apply", apply(7)).is_ok());
    }

    #[test]
    fn an_apply_that_wrote_before_the_server_stopped_ends_as_done_with_its_run_gone() {
        let clock = Arc::new(SetClock::at(T0));
        let (core, _scratch, session) = staged(&clock, 1);
        dry_run(&core, ritornel_engine::GROUPER_VERSION);
        let started = call(&core, &session, "imports.apply", apply(4)).unwrap();
        // The write is made, and then the server stops: the restart
        // removes the staged run, and the job is taken up again.
        let run = Id::from_bytes([9; 16]);
        let streams = core.staging().streams(run, T0).unwrap().unwrap().0;
        core.with_library(|_, library| {
            library.apply_streams(
                ritornel_engine::WriteRequest {
                    method: ritornel_engine::APPLY_METHOD,
                    command: None,
                    client: None,
                },
                run,
                streams,
                "ritornelctl 0.1.0".to_owned(),
                None,
            )
        })
        .unwrap()
        .unwrap();
        core.staging().sweep(T0 + KEEP_MS + 1);
        let job = core
            .with_stores(|stores| stores.server().claim_job(Id::from_bytes([0xb0; 16])))
            .unwrap()
            .unwrap()
            .unwrap();
        crate::imports::run_apply(
            &core,
            job,
            &Arc::new(std::sync::atomic::AtomicBool::new(false)),
        );
        let job_id = started["job"]["jobId"].as_str().unwrap();
        let ended = call(&core, &session, "jobs.get", json!({ "jobId": job_id })).unwrap();
        assert_eq!(ended["job"]["state"], "succeeded");
    }

    #[test]
    fn a_command_sent_again_is_answered_with_its_job_after_its_run_has_gone() {
        let clock = Arc::new(SetClock::at(T0));
        let (core, _scratch, session) = staged(&clock, 1);
        dry_run(&core, ritornel_engine::GROUPER_VERSION);
        let started = call(&core, &session, "imports.apply", apply(4)).unwrap();

        // A day later the staged run is gone. The command sent again is
        // answered with the job it started; a new one finds no run.
        clock.set(T0 + KEEP_MS + 1);
        core.staging().sweep(T0 + KEEP_MS + 1);
        let again = call(&core, &session, "imports.apply", apply(4)).unwrap();
        assert_eq!(again["job"]["jobId"], started["job"]["jobId"]);
        assert_eq!(
            call(&core, &session, "imports.apply", apply(5)),
            Err(ErrorCode::NotFound)
        );
    }

    fn observations(core: &Core, session: &Session, scratch: &Scratch, name: &str, key: u8) -> u64 {
        let dir = scratch.path().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let exported = call(
            core,
            session,
            "history.export",
            json!({ "idempotencyKey": id(key), "folder": dir.to_str().unwrap() }),
        )
        .unwrap();
        exported["observations"].as_u64().unwrap()
    }

    #[test]
    fn a_cancelled_apply_before_the_library_transaction_writes_nothing_and_ends_cancelled() {
        let clock = Arc::new(SetClock::at(T0));
        let (core, scratch, session) = staged(&clock, 1);
        dry_run(&core, ritornel_engine::GROUPER_VERSION);
        let before_observations = observations(&core, &session, &scratch, "export-before", 80);
        let before_revision = core
            .with_library(|_, library| library.meta().unwrap().revision)
            .unwrap();
        let started = call(&core, &session, "imports.apply", apply(4)).unwrap();
        let job_id = started["job"]["jobId"].as_str().unwrap().to_owned();
        let claimed = core
            .with_stores(|stores| stores.server().claim_job(Id::from_bytes([0xb0; 16])))
            .unwrap()
            .unwrap()
            .unwrap();
        crate::imports::run_apply_before_commit(
            &core,
            claimed,
            &Arc::new(std::sync::atomic::AtomicBool::new(false)),
            || {
                let cancelled = call(
                    &core,
                    &session,
                    "jobs.cancel",
                    json!({ "idempotencyKey": id(5), "jobId": job_id }),
                )
                .unwrap();
                assert_eq!(cancelled["job"]["state"], "cancelling");
            },
        );
        let ended = call(&core, &session, "jobs.get", json!({ "jobId": job_id })).unwrap();
        assert_eq!(ended["job"]["state"], "cancelled");
        assert_eq!(
            observations(&core, &session, &scratch, "export-after", 81),
            before_observations
        );
        assert_eq!(
            core.with_library(|_, library| library.meta().unwrap().revision)
                .unwrap(),
            before_revision
        );
        assert!(
            core.with_library(|_, library| library.stored_run(Id::from_bytes([9; 16])).unwrap())
                .unwrap()
                .is_none()
        );
        assert!(
            core.staging()
                .applied_report(Id::from_bytes([9; 16]))
                .is_none()
        );
    }

    #[test]
    fn a_cancel_after_apply_commits_keeps_the_run_and_never_reports_success() {
        let clock = Arc::new(SetClock::at(T0));
        let (core, scratch, session) = staged(&clock, 1);
        dry_run(&core, ritornel_engine::GROUPER_VERSION);
        let started = call(&core, &session, "imports.apply", apply(4)).unwrap();
        let job_id = started["job"]["jobId"].as_str().unwrap().to_owned();
        let claimed = core
            .with_stores(|stores| stores.server().claim_job(Id::from_bytes([0xb0; 16])))
            .unwrap()
            .unwrap()
            .unwrap();
        let core = Arc::new(core);
        {
            let (core_hook, session_hook, job_id_hook) =
                (Arc::downgrade(&core), Arc::clone(&session), job_id.clone());
            core.with_library(|_, library| {
                library.on_commit(Arc::new(move |committed| {
                    if committed.method == ritornel_engine::APPLY_METHOD
                        && let Some(core_hook) = core_hook.upgrade()
                    {
                        let cancelled = call(
                            &core_hook,
                            &session_hook,
                            "jobs.cancel",
                            json!({ "idempotencyKey": id(5), "jobId": job_id_hook }),
                        )
                        .unwrap();
                        assert_eq!(cancelled["job"]["state"], "cancelling");
                    }
                }));
            });
        }
        crate::imports::run_apply(
            &core,
            claimed,
            &Arc::new(std::sync::atomic::AtomicBool::new(false)),
        );
        let ended = call(&core, &session, "jobs.get", json!({ "jobId": job_id })).unwrap();
        assert_eq!(ended["job"]["state"], "cancelled");
        // The commit had already begun and committed atomically.
        assert_eq!(
            observations(&core, &session, &scratch, "export-after", 82),
            1
        );
        assert!(
            core.with_library(|_, library| library.stored_run(Id::from_bytes([9; 16])).unwrap())
                .unwrap()
                .is_some_and(|run| run.in_force)
        );
    }

    #[test]
    fn a_cancelled_apply_with_a_missing_staged_run_still_ends_cancelled() {
        let clock = Arc::new(SetClock::at(T0));
        let (core, _scratch, session) = staged(&clock, 1);
        dry_run(&core, ritornel_engine::GROUPER_VERSION);
        let started = call(&core, &session, "imports.apply", apply(4)).unwrap();
        let job_id = started["job"]["jobId"].as_str().unwrap().to_owned();
        let claimed = core
            .with_stores(|stores| stores.server().claim_job(Id::from_bytes([0xb0; 16])))
            .unwrap()
            .unwrap()
            .unwrap();
        // Cancel is acknowledged and the staged run expires before apply runs.
        let cancelled = call(
            &core,
            &session,
            "jobs.cancel",
            json!({ "idempotencyKey": id(5), "jobId": job_id }),
        )
        .unwrap();
        assert_eq!(cancelled["job"]["state"], "cancelling");
        core.staging().sweep(T0 + KEEP_MS + 1);
        crate::imports::run_apply(
            &core,
            claimed,
            &Arc::new(std::sync::atomic::AtomicBool::new(false)),
        );
        let ended = call(&core, &session, "jobs.get", json!({ "jobId": job_id })).unwrap();
        assert_eq!(ended["job"]["state"], "cancelled");
    }

    #[test]
    fn a_shutdown_leaves_a_claimed_apply_running_for_the_next_boot() {
        let clock = Arc::new(SetClock::at(T0));
        let (core, scratch, session) = staged(&clock, 1);
        dry_run(&core, ritornel_engine::GROUPER_VERSION);
        let started = call(&core, &session, "imports.apply", apply(4)).unwrap();
        let job_id = started["job"]["jobId"].as_str().unwrap().to_owned();
        let claimed = core
            .with_stores(|stores| stores.server().claim_job(Id::from_bytes([0xb0; 16])))
            .unwrap()
            .unwrap()
            .unwrap();
        let shutdown = Arc::new(std::sync::atomic::AtomicBool::new(false));
        crate::imports::run_apply_before_commit(&core, claimed, &shutdown, || {
            shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        let paused = call(&core, &session, "jobs.get", json!({ "jobId": job_id })).unwrap();
        assert_eq!(paused["job"]["state"], "running");
        assert_eq!(observations(&core, &session, &scratch, "export-mid", 83), 0);

        // Next boot requeues the abandoned running job.
        let requeued = core
            .with_stores(|stores| {
                stores
                    .server()
                    .requeue_jobs_of_other_runs(Id::from_bytes([0xb1; 16]))
            })
            .unwrap()
            .unwrap();
        assert_eq!(requeued, 1);
        let queued = call(&core, &session, "jobs.get", json!({ "jobId": job_id })).unwrap();
        assert_eq!(queued["job"]["state"], "queued");
    }

    #[test]
    fn a_cancelled_undo_before_the_library_transaction_leaves_the_run_in_force() {
        let clock = Arc::new(SetClock::at(T0));
        let (core, _scratch, session) = staged(&clock, 1);
        dry_run(&core, ritornel_engine::GROUPER_VERSION);
        let run = Id::from_bytes([9; 16]);
        let streams = core.staging().streams(run, T0).unwrap().unwrap().0;
        core.with_library(|_, library| {
            library.apply_streams(
                ritornel_engine::WriteRequest {
                    method: ritornel_engine::APPLY_METHOD,
                    command: None,
                    client: None,
                },
                run,
                streams,
                "ritornelctl 0.1.0".to_owned(),
                None,
            )
        })
        .unwrap()
        .unwrap();
        let before_revision = core
            .with_library(|_, library| library.meta().unwrap().revision)
            .unwrap();
        let started = call(&core, &session, "imports.undo", apply(4)).unwrap();
        let job_id = started["job"]["jobId"].as_str().unwrap().to_owned();
        let claimed = core
            .with_stores(|stores| stores.server().claim_job(Id::from_bytes([0xb0; 16])))
            .unwrap()
            .unwrap()
            .unwrap();
        crate::imports::run_undo_before_commit(
            &core,
            claimed,
            &Arc::new(std::sync::atomic::AtomicBool::new(false)),
            || {
                let cancelled = call(
                    &core,
                    &session,
                    "jobs.cancel",
                    json!({ "idempotencyKey": id(5), "jobId": job_id }),
                )
                .unwrap();
                assert_eq!(cancelled["job"]["state"], "cancelling");
            },
        );
        let ended = call(&core, &session, "jobs.get", json!({ "jobId": job_id })).unwrap();
        assert_eq!(ended["job"]["state"], "cancelled");
        assert_eq!(
            core.with_library(|_, library| library.meta().unwrap().revision)
                .unwrap(),
            before_revision
        );
        assert!(
            core.with_library(|_, library| library.stored_run(run).unwrap())
                .unwrap()
                .is_some_and(|stored| stored.in_force)
        );
    }

    #[test]
    fn a_cancelled_dry_run_saves_no_report_and_ends_cancelled() {
        let clock = Arc::new(SetClock::at(T0));
        let (core, _scratch, session) = staged(&clock, 1);
        let started = call(&core, &session, "imports.dryRun", apply(4)).unwrap();
        let job_id = started["job"]["jobId"].as_str().unwrap().to_owned();
        let claimed = core
            .with_stores(|stores| stores.server().claim_job(Id::from_bytes([0xb0; 16])))
            .unwrap()
            .unwrap()
            .unwrap();
        crate::imports::run_dry_run_before_save(
            &core,
            claimed,
            &Arc::new(std::sync::atomic::AtomicBool::new(false)),
            || {
                let cancelled = call(
                    &core,
                    &session,
                    "jobs.cancel",
                    json!({ "idempotencyKey": id(5), "jobId": job_id }),
                )
                .unwrap();
                assert_eq!(cancelled["job"]["state"], "cancelling");
            },
        );
        let ended = call(&core, &session, "jobs.get", json!({ "jobId": job_id })).unwrap();
        assert_eq!(ended["job"]["state"], "cancelled");
        assert!(
            core.staging()
                .run(Id::from_bytes([9; 16]), T0)
                .unwrap()
                .unwrap()
                .report
                .is_none()
        );
    }
}
