//! The library calls of M1: sources, scans, jobs and what a scan produced.
//! See "Playback and jobs" in docs/design/contract.md.

use std::path::Path;
use std::sync::PoisonError;

use ritornel_contract::methods::{JobsCancel, Method, ScanStart, SourcesAdd};
use ritornel_contract::{
    ErrorCode, Event, Job, JobCancelParams, JobCancelled, JobDetail, JobGetParams, JobItem,
    JobState, JobTarget, JobsEvent, JobsList, LibraryChange, LibraryEvent, LibraryStats, Receipt,
    ReceiptStatus, RpcError, ScanStartParams, ScanStarted, Source, SourceAdded, SourceKind,
    SourcesAddParams, SourcesList,
};
use ritornel_engine::{
    Client, Command, LibraryStore, NewJob, ServerStore, SubjectKind, WriteRequest, scan_summary,
};

use super::{Core, Stores, failed, fingerprint, lock, revision};
use crate::Error;

/// How many jobs `jobs.list` returns, and how many failed items `jobs.get`.
const LISTED: u32 = 100;

/// The longest a root's name may be, in characters.
const MAX_ROOT_NAME: usize = 100;

fn unsigned(number: i64) -> u64 {
    u64::try_from(number).unwrap_or(0)
}

/// A job as the contract carries it.
pub(crate) fn job(job: &ritornel_engine::Job) -> Job {
    use ritornel_engine::JobState as Engine;
    Job {
        job_id: job.public_id,
        kind: job.kind.clone(),
        target: job.target.map(|id| JobTarget::Source { id }),
        state: match job.state {
            Engine::Queued => JobState::Queued,
            Engine::Running => JobState::Running,
            Engine::Cancelling => JobState::Cancelling,
            Engine::Succeeded => JobState::Succeeded,
            Engine::Failed => JobState::Failed,
            Engine::Cancelled => JobState::Cancelled,
        },
        done: unsigned(job.done),
        total: job.total.map(unsigned),
        bytes: unsigned(job.bytes),
        failed: unsigned(job.failed),
        skipped: unsigned(job.skipped),
        label: job.label.clone(),
        error: job.error.clone(),
        created_at: job.created_at_ms,
        started_at: job.started_at_ms,
        finished_at: job.finished_at_ms,
        scan: scan_summary(job),
    }
}

fn no_library() -> RpcError {
    RpcError::new(
        ErrorCode::Unavailable,
        "this server holds no library: create one with library.create",
    )
}

fn stored(error: ritornel_engine::Error) -> RpcError {
    failed(Error::Store(error))
}

fn in_library(error: ritornel_engine::Error) -> RpcError {
    failed(Error::Library(error))
}

impl Stores {
    /// `server.db`.
    pub(crate) fn server(&self) -> &ServerStore {
        &self.server
    }
}

impl Core {
    /// What wakes the worker that runs jobs.
    pub(crate) fn jobs(&self) -> &crate::jobs::Wake {
        &self.jobs
    }

    /// Runs `work` with the databases, unless the server has closed them.
    pub(crate) fn with_stores<T>(&self, work: impl FnOnce(&Stores) -> T) -> Option<T> {
        let stores = self.stores.read().unwrap_or_else(PoisonError::into_inner);
        stores.as_ref().map(work)
    }

    /// Runs `work` with `server.db` and the library, unless the server has
    /// closed them or holds no library.
    pub(crate) fn with_library<T>(
        &self,
        work: impl FnOnce(&ServerStore, &LibraryStore) -> T,
    ) -> Option<T> {
        self.with_stores(|stores| {
            let library = stores
                .library
                .read()
                .unwrap_or_else(PoisonError::into_inner);
            library
                .as_ref()
                .map(|library| work(&stores.server, library))
        })
        .flatten()
    }

    /// The library revision, or 0 where there is no library to ask.
    pub(crate) fn library_revision(&self) -> u64 {
        self.with_library(|_, library| library.meta().map_or(0, |meta| revision(meta.revision)))
            .unwrap_or(0)
    }

    fn publish(&self, event: &Event) {
        for session in lock(&self.sessions).open.values() {
            session.publish(event.clone());
        }
    }

    /// Sends a job, whole, to every session subscribed to the `jobs` topic.
    pub(crate) fn publish_job(&self, changed: &ritornel_engine::Job) {
        self.publish(&Event::Jobs(Box::new(JobsEvent { job: job(changed) })));
    }

    /// Sends a hint that the library changed to every session subscribed
    /// to the `library` topic.
    pub(crate) fn publish_library(&self, library_revision: u64, changed: &[LibraryChange]) {
        self.publish(&Event::Library(LibraryEvent {
            library_revision,
            changed: changed.to_vec(),
        }));
    }

    /// The session every command comes from: the only credential there is
    /// yet is the local one.
    fn client(&self) -> Option<Client> {
        self.local_credential.session().map(|session| Client {
            session,
            kind: SubjectKind::Person,
        })
    }

    fn library<T>(
        &self,
        stores: &Stores,
        work: impl FnOnce(&LibraryStore) -> Result<T, RpcError>,
    ) -> Result<T, RpcError> {
        let library = stores
            .library
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        work(library.as_ref().ok_or_else(no_library)?)
    }

    pub(super) fn sources_list(&self, stores: &Stores) -> Result<SourcesList, RpcError> {
        let roots = stores.server.roots().map_err(stored)?;
        let sources = self.library(stores, |library| library.sources().map_err(in_library))?;
        Ok(SourcesList {
            sources: sources
                .into_iter()
                .map(|source| Source {
                    source_id: source.public_id,
                    kind: SourceKind::Folder,
                    path: roots
                        .iter()
                        .find(|(name, _)| source.on_this_server && *name == source.name)
                        .map(|(_, path)| path.clone()),
                    name: source.name,
                })
                .collect(),
        })
    }

    /// `sources.add`: a named root on this server, and the folder that is
    /// the whole of it.
    ///
    /// Two files are written, this server's first. `server.db` gets where
    /// the root is; then `library.db` gets the root's name and its source,
    /// with the command's receipt, in one transaction. A server that stops
    /// between the two has a path for a root the library does not know,
    /// which changes nothing, and the call sent again finishes it.
    pub(super) fn sources_add(
        &self,
        stores: &Stores,
        params: &SourcesAddParams,
    ) -> Result<SourceAdded, RpcError> {
        let invalid = |field: &str, message: &str| {
            RpcError::new(ErrorCode::Invalid, message).with_detail("field", field)
        };
        let name = params.name.trim();
        if name.is_empty()
            || name.chars().count() > MAX_ROOT_NAME
            || name.chars().any(|c| c == '/' || c.is_control())
        {
            return Err(invalid(
                "name",
                "a root's name is 1 to 100 characters with no slash",
            ));
        }
        let path = Path::new(&params.path);
        if !path.is_absolute() {
            return Err(invalid("path", "a root's path is absolute"));
        }
        if !path.is_dir() {
            return Err(invalid(
                "path",
                "there is no folder at that path on this server",
            ));
        }
        let request = WriteRequest {
            method: SourcesAdd::NAME,
            command: Some(Command {
                id: params.idempotency_key,
                fingerprint: fingerprint::<SourcesAdd>(params)?,
            }),
            client: self.client(),
        };
        let (command_id, name, path) =
            (params.idempotency_key, name.to_owned(), params.path.clone());
        let written = self.library(stores, |library| {
            stores.server.add_root(&name, &path).map_err(stored)?;
            let receipt = move |source: &ritornel_engine::Source, rev: i64| {
                let added = SourceAdded {
                    receipt: Receipt {
                        command_id,
                        status: ReceiptStatus::Done,
                        revision: Some(revision(rev)),
                    },
                    source: Source {
                        source_id: source.public_id,
                        name: source.name.clone(),
                        kind: SourceKind::Folder,
                        path: Some(path),
                    },
                };
                // Writing these few fields cannot fail. If it did, the
                // result would not read back, and the call would say so.
                serde_json::to_string(&added).unwrap_or_default()
            };
            library
                .add_source(request, &name, receipt)
                .map_err(in_library)
        })?;
        if !written.replayed {
            self.publish_library(revision(written.revision), &[LibraryChange::Membership]);
        }
        serde_json::from_str(&written.result_json).map_err(|error| {
            RpcError::new(
                ErrorCode::Internal,
                format!(
                    "the stored result of {} could not be read: {error}",
                    SourcesAdd::NAME
                ),
            )
        })
    }

    /// `scan.start`: queues a scan of a source as a job, and wakes the
    /// worker. One root has one active scan: a source that is being scanned
    /// answers with the job that is running.
    pub(super) fn scan_start(
        &self,
        stores: &Stores,
        params: &ScanStartParams,
    ) -> Result<ScanStarted, RpcError> {
        let source = self
            .library(stores, |library| {
                library.source(params.source_id).map_err(in_library)
            })?
            .ok_or_else(|| RpcError::new(ErrorCode::NotFound, "the library has no such source"))?;
        let roots = stores.server.roots().map_err(stored)?;
        if !source.on_this_server || !roots.iter().any(|(name, _)| *name == source.name) {
            return Err(RpcError::new(
                ErrorCode::Unavailable,
                "this server has no path for that source's root",
            ));
        }
        let command = Command {
            id: params.idempotency_key,
            fingerprint: fingerprint::<ScanStart>(params)?,
        };
        let new = NewJob {
            kind: "scan",
            dedupe_key: source.name,
            target: source.public_id,
            params_json: serde_json::to_string(params).unwrap_or_default(),
            session: self.local_credential.session(),
        };
        let (started, created) = stores
            .server
            .start_job(command, ScanStart::NAME, new)
            .map_err(stored)?;
        if created {
            self.publish_job(&started);
        }
        self.jobs.wake();
        Ok(ScanStarted {
            receipt: Receipt {
                command_id: params.idempotency_key,
                status: ReceiptStatus::Accepted,
                revision: None,
            },
            job: job(&started),
        })
    }

    pub(super) fn jobs_list(&self, stores: &Stores) -> Result<JobsList, RpcError> {
        let jobs = stores.server.jobs(LISTED).map_err(stored)?;
        Ok(JobsList {
            jobs: jobs.iter().map(job).collect(),
        })
    }

    fn no_job() -> RpcError {
        RpcError::new(ErrorCode::NotFound, "there is no such job")
    }

    pub(super) fn jobs_get(
        &self,
        stores: &Stores,
        params: &JobGetParams,
    ) -> Result<JobDetail, RpcError> {
        let found = stores
            .server
            .job(params.job_id)
            .map_err(stored)?
            .ok_or_else(Self::no_job)?;
        let items = stores
            .server
            .failed_job_items(found.id, LISTED)
            .map_err(stored)?;
        Ok(JobDetail {
            job: job(&found),
            failed_items: items
                .into_iter()
                .map(|(item, detail)| JobItem { item, detail })
                .collect(),
        })
    }

    /// `jobs.cancel`: a request. A queued job is cancelled at once, and a
    /// running one stops after the step in flight.
    pub(super) fn jobs_cancel(
        &self,
        stores: &Stores,
        params: &JobCancelParams,
    ) -> Result<JobCancelled, RpcError> {
        let command = Command {
            id: params.idempotency_key,
            fingerprint: fingerprint::<JobsCancel>(params)?,
        };
        let asked = stores
            .server
            .cancel_job(command, JobsCancel::NAME, params.job_id)
            .map_err(stored)?
            .ok_or_else(Self::no_job)?;
        self.publish_job(&asked);
        Ok(JobCancelled {
            receipt: Receipt {
                command_id: params.idempotency_key,
                status: if asked.state.ended() {
                    ReceiptStatus::Done
                } else {
                    ReceiptStatus::Accepted
                },
                revision: None,
            },
            job: job(&asked),
        })
    }

    pub(super) fn library_stats(&self, stores: &Stores) -> Result<LibraryStats, RpcError> {
        self.library(stores, |library| library.stats().map_err(in_library))
    }
}
