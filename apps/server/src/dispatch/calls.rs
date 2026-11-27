//! Library calls: sources, scans, jobs and what a scan produced. See
//! "Commands, playback, and jobs" in docs/internals/contract.md.

use std::path::Path;
use std::sync::PoisonError;

use ritornel_contract::methods::{
    ArtistsImportPictures, JobsCancel, Method, ScanStart, SourcesAdd,
};
use ritornel_contract::{
    DryRun, DryRunParams, ErrorCode, Event, ImportPicturesParams, Job, JobCancelParams,
    JobCancelled, JobDetail, JobGetParams, JobItem, JobState, JobTarget, JobsEvent, JobsList,
    LibraryStats, PictureFolder, PictureFolders, PicturesImported, Receipt, ReceiptStatus,
    ReviewList, RpcError, ScanStartParams, ScanStarted, Source, SourceAdded, SourceKind,
    SourcesAddParams, SourcesList,
};
use ritornel_engine::{
    Client, Command, LibraryStore, NewJob, ServerStore, SubjectKind, WriteRequest, is_policy,
    scan_summary,
};

use super::{Core, Stores, failed, fingerprint, lock, revision};
use crate::Error;

/// How many jobs `jobs.list` returns, and how many failed items `jobs.get`.
const LISTED: u32 = 100;

/// The longest a root's name may be, in characters.
const MAX_ROOT_NAME: usize = 100;

/// The widest free duration band a dry run may ask about: a minute.
const MAX_DURATION_GRACE_MS: u64 = 60_000;

fn unsigned(number: i64) -> u64 {
    u64::try_from(number).unwrap_or(0)
}

/// A job's state as the contract carries it.
pub(crate) fn job_state(state: ritornel_engine::JobState) -> JobState {
    use ritornel_engine::JobState as Engine;
    match state {
        Engine::Queued => JobState::Queued,
        Engine::Running => JobState::Running,
        Engine::Cancelling => JobState::Cancelling,
        Engine::Succeeded => JobState::Succeeded,
        Engine::Failed => JobState::Failed,
        Engine::Cancelled => JobState::Cancelled,
    }
}

/// A job as the contract carries it. Error messages that may hold host
/// filesystem paths are omitted when `show_paths` is false.
pub(crate) fn job_for(job: &ritornel_engine::Job, show_paths: bool) -> Job {
    Job {
        job_id: job.public_id,
        kind: job.kind.clone(),
        target: job.target.map(|id| {
            if job.target_kind.as_deref() == Some("account") || job.kind == crate::lastfm::SYNC_JOB
            {
                JobTarget::Account { id }
            } else if job.target_kind.as_deref() == Some("server") {
                JobTarget::Server { id }
            } else if job.target_kind.as_deref() == Some("import_run")
                || [
                    crate::imports::DRY_RUN_JOB,
                    crate::imports::APPLY_JOB,
                    crate::imports::UNDO_JOB,
                ]
                .contains(&job.kind.as_str())
            {
                JobTarget::ImportRun { id }
            } else {
                JobTarget::Source { id }
            }
        }),
        state: job_state(job.state),
        done: unsigned(job.done),
        total: job.total.map(unsigned),
        bytes: unsigned(job.bytes),
        failed: unsigned(job.failed),
        skipped: unsigned(job.skipped),
        // A scan's progress names the file it is at.
        label: if show_paths { job.label.clone() } else { None },
        error: if show_paths { job.error.clone() } else { None },
        created_at: job.created_at_ms,
        started_at: job.started_at_ms,
        finished_at: job.finished_at_ms,
        scan: scan_summary(job),
        sync: (job.kind == crate::lastfm::SYNC_JOB)
            .then(|| serde_json::from_str(job.checkpoint_json.as_deref()?).ok())
            .flatten(),
    }
}

fn no_library() -> RpcError {
    RpcError::new(
        ErrorCode::Unavailable,
        "this server holds no library: create one with library.create",
    )
    .with_detail("reason", "no_library")
}

/// What an import stored as its result, read back.
fn stored_result<T: serde::de::DeserializeOwned>(
    method: &str,
    written: &ritornel_engine::Written,
) -> Result<T, RpcError> {
    serde_json::from_str(&written.result_json).map_err(|error| {
        RpcError::new(
            ErrorCode::Internal,
            format!("the stored result of {method} could not be read: {error}"),
        )
    })
}

/// A root's name, trimmed, after checking it and the folder's path.
fn root(name: &str, path: &str) -> Result<String, RpcError> {
    let invalid = |field: &str, message: &str| {
        RpcError::new(ErrorCode::Invalid, message).with_detail("field", field)
    };
    let name = name.trim();
    if name.is_empty()
        || name.chars().count() > MAX_ROOT_NAME
        || name.chars().any(|c| c == '/' || c.is_control())
    {
        return Err(invalid(
            "name",
            "a root's name is 1 to 100 characters with no slash",
        ));
    }
    let path = Path::new(path);
    if !path.is_absolute() {
        return Err(invalid("path", "a root's path is absolute"));
    }
    if !path.is_dir() {
        return Err(invalid(
            "path",
            "there is no folder at that path on this server",
        ));
    }
    Ok(name.to_owned())
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

    pub(super) fn publish(&self, event: &Event) {
        for session in lock(&self.sessions).open.values() {
            session.publish(event.clone());
        }
    }

    /// Sends a job, whole, to every session subscribed to the `jobs` topic.
    pub(crate) fn publish_job(&self, changed: &ritornel_engine::Job) {
        self.with_stores(|stores| self.publish_stored_job(stores, changed));
    }

    pub(super) fn publish_stored_job(&self, stores: &Stores, changed: &ritornel_engine::Job) {
        let _sending = lock(&self.job_events);
        // Finished jobs cannot change again. For an earlier snapshot the
        // durable row tells us whether another publisher has overtaken it.
        if !changed.state.ended() {
            match stores.server.job(changed.public_id) {
                Ok(current) if current.as_ref() == Some(changed) => {}
                Ok(_) => return,
                Err(error) => {
                    tracing::warn!(%error, "could not check job progress");
                    return;
                }
            }
        }
        let sessions: Vec<_> = lock(&self.sessions).open.values().cloned().collect();
        for session in sessions {
            let show_paths = session.has(ritornel_contract::Permission::FoldersManage);
            session.publish(Event::Jobs(Box::new(JobsEvent {
                job: job_for(changed, show_paths),
            })));
        }
    }

    /// The session every command comes from: the only credential there is
    /// yet is the local one.
    pub(super) fn client(&self) -> Option<Client> {
        self.local_credential.session().map(|session| Client {
            session,
            kind: SubjectKind::Person,
        })
    }

    /// Runs `work` on the library, or says this server holds none.
    pub(super) fn library<T>(
        stores: &Stores,
        work: impl FnOnce(&LibraryStore) -> Result<T, RpcError>,
    ) -> Result<T, RpcError> {
        let library = stores
            .library
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        work(library.as_ref().ok_or_else(no_library)?)
    }

    pub(super) fn sources_list(
        &self,
        stores: &Stores,
        show_paths: bool,
    ) -> Result<SourcesList, RpcError> {
        let roots = if show_paths {
            stores.server.roots().map_err(stored)?
        } else {
            Vec::new()
        };
        let sources = Self::library(stores, |library| library.sources().map_err(in_library))?;
        Ok(SourcesList {
            // A folder is the one kind the contract has. The account a
            // history is imported from is a source too, and is no folder.
            sources: sources
                .into_iter()
                .filter(|source| source.kind == "folder")
                .map(|source| Source {
                    source_id: source.public_id,
                    kind: SourceKind::Folder,
                    path: if show_paths {
                        roots
                            .iter()
                            .find(|(name, _)| source.on_this_server && *name == source.name)
                            .map(|(_, path)| path.clone())
                    } else {
                        None
                    },
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
        let _adding = lock(&stores.adding_source);
        let command = Command {
            id: params.idempotency_key,
            fingerprint: fingerprint::<SourcesAdd>(params)?,
        };
        // A call sent again answers from its receipt before the folder is
        // looked at or a root recorded, as `artists.importPictures` does:
        // the folder may have gone since, and a key sent with another root
        // must not leave that root behind. The write checks again, for one
        // sent twice at once.
        let replayed = Self::library(stores, |library| {
            library.replay(&command).map_err(in_library)
        })?;
        if let Some(written) = replayed {
            return stored_result(SourcesAdd::NAME, &written);
        }
        let name = root(&params.name, &params.path)?;
        let folder = Path::new(&params.path);
        let resolved = std::fs::canonicalize(folder).map_err(|_| {
            RpcError::new(
                ErrorCode::Invalid,
                "Cannot open that folder. Choose it again.",
            )
            .with_detail("field", "path")
        })?;
        let sources = self.sources_list(stores, true)?.sources;
        if let Some(source) = sources
            .iter()
            .find(|source| source.name == name && source.path.as_deref() == Some(&params.path))
        {
            return Err(in_library(ritornel_engine::Error::SourceExists {
                source: source.source_id,
            }));
        }
        for source in sources {
            let Some(path) = source.path else { continue };
            let existing = Path::new(&path);
            let physical = std::fs::canonicalize(existing).ok();
            let (folder, existing) = match &physical {
                Some(path) => (resolved.as_path(), path.as_path()),
                None => (folder, existing),
            };
            let relation = if folder == existing {
                if source.name == name {
                    return Err(in_library(ritornel_engine::Error::SourceExists {
                        source: source.source_id,
                    }));
                }
                "is already registered as"
            } else if folder.starts_with(existing) {
                "is inside"
            } else if existing.starts_with(folder) {
                "contains"
            } else {
                continue;
            };
            return Err(RpcError::new(
                ErrorCode::Invalid,
                format!(
                    "This folder {relation} {} ({}). Choose a separate music folder.",
                    source.name, path
                ),
            )
            .with_detail("field", "path"));
        }
        let request = WriteRequest {
            method: SourcesAdd::NAME,
            command: Some(command),
            client: self.client(),
        };
        let (command_id, name, path) = (params.idempotency_key, name, params.path.clone());
        let written = Self::library(stores, |library| {
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
        // The library tells subscribers of the write itself, after it
        // commits (`lists::hook`).
        stored_result(SourcesAdd::NAME, &written)
    }

    /// `artists.pictureFolders`: each picture folder, with its path where
    /// this server has one, and the last import.
    pub(super) fn artists_picture_folders(
        &self,
        stores: &Stores,
    ) -> Result<PictureFolders, RpcError> {
        let roots = stores.server.roots().map_err(stored)?;
        let (folders, last) = Self::library(stores, |library| {
            library
                .picture_folders_and_last_import()
                .map_err(in_library)
        })?;
        Ok(PictureFolders {
            folders: folders
                .into_iter()
                .map(|folder| PictureFolder {
                    path: roots
                        .iter()
                        .find(|(name, _)| folder.on_this_server && *name == folder.name)
                        .map(|(_, path)| path.clone()),
                    name: folder.name,
                    portraits: folder.portraits,
                })
                .collect(),
            last_import_at: last.as_ref().map(|last| last.at_ms),
            // A receipt that will not read is as good as one pruned.
            last_import: last
                .and_then(|last| last.result_json)
                .and_then(|json| serde_json::from_str(&json).ok()),
        })
    }

    /// `artists.importPictures`: a picture folder as a named root on this
    /// server, and the portraits it chooses, as `sources.add` writes a
    /// root: `server.db` first, then the library with the receipt.
    pub(super) fn artists_import_pictures(
        &self,
        stores: &Stores,
        params: &ImportPicturesParams,
    ) -> Result<PicturesImported, RpcError> {
        let command = Command {
            id: params.idempotency_key,
            fingerprint: fingerprint::<ArtistsImportPictures>(params)?,
        };
        // An import sent again answers from its receipt before the folder
        // is looked at, as it may have changed since. The write checks
        // again, for one sent twice at once.
        let replayed = Self::library(stores, |library| {
            library.replay(&command).map_err(in_library)
        })?;
        if let Some(written) = replayed {
            return stored_result(ArtistsImportPictures::NAME, &written);
        }
        let name = root(&params.name, &params.path)?;
        // Checked once, before the root is recorded, so a folder refused
        // leaves its name free and none is refused after.
        let folder =
            ritornel_engine::check_picture_folder(Path::new(&params.path)).map_err(|error| {
                RpcError::new(ErrorCode::Invalid, error.to_string()).with_detail("field", "path")
            })?;
        let request = WriteRequest {
            method: ArtistsImportPictures::NAME,
            command: Some(command),
            client: self.client(),
        };
        let command_id = params.idempotency_key;
        let written = Self::library(stores, |library| {
            stores
                .server
                .add_root(&name, &params.path)
                .map_err(stored)?;
            let receipt = move |counts: &ritornel_engine::PicturesImported, rev: i64| {
                let imported = PicturesImported {
                    receipt: Receipt {
                        command_id,
                        status: ReceiptStatus::Done,
                        revision: Some(revision(rev)),
                    },
                    artists: counts.artists,
                    by_mbid: counts.by_mbid,
                    by_name: counts.by_name,
                    without: counts.without,
                    unreadable: counts.unreadable,
                    dropped: counts.dropped,
                    ambiguous: counts.ambiguous,
                };
                serde_json::to_string(&imported).unwrap_or_default()
            };
            library
                .import_artist_pictures(request, &name, &folder, receipt)
                .map_err(in_library)
        })?;
        stored_result(ArtistsImportPictures::NAME, &written)
    }

    /// `scan.start`: queues a scan of a source as a job, and wakes the
    /// worker. One root has one active scan: a source that is being scanned
    /// answers with the job that is running.
    pub(super) fn scan_start(
        &self,
        stores: &Stores,
        params: &ScanStartParams,
        show_paths: bool,
    ) -> Result<ScanStarted, RpcError> {
        let source = Self::library(stores, |library| {
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
            target_kind: "source",
            target: source.public_id,
            params_json: serde_json::to_string(params).unwrap_or_default(),
            session: self.local_credential.session(),
        };
        let (started, created) = stores
            .server
            .start_job(command, ScanStart::NAME, new)
            .map_err(stored)?;
        if created {
            self.publish_stored_job(stores, &started);
        }
        self.jobs.wake();
        Ok(ScanStarted {
            receipt: Receipt {
                command_id: params.idempotency_key,
                status: ReceiptStatus::Accepted,
                revision: None,
            },
            job: job_for(&started, show_paths),
        })
    }

    pub(super) fn jobs_list(
        &self,
        stores: &Stores,
        show_paths: bool,
    ) -> Result<JobsList, RpcError> {
        let jobs = stores.server.jobs(LISTED).map_err(stored)?;
        Ok(JobsList {
            jobs: jobs.iter().map(|j| job_for(j, show_paths)).collect(),
        })
    }

    fn no_job() -> RpcError {
        RpcError::new(ErrorCode::NotFound, "there is no such job")
    }

    pub(super) fn jobs_get(
        &self,
        stores: &Stores,
        params: &JobGetParams,
        show_paths: bool,
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
            artifacts: if found.kind == "transcode.start"
                && found.state == ritornel_engine::JobState::Succeeded
            {
                found
                    .checkpoint_json
                    .as_deref()
                    .map(serde_json::from_str)
                    .transpose()
                    .map_err(|_| {
                        RpcError::new(ErrorCode::Internal, "invalid transcode job result")
                    })?
                    .unwrap_or_default()
            } else {
                Vec::new()
            },
            job: job_for(&found, show_paths),
            failed_items: if show_paths {
                items
                    .into_iter()
                    .map(|(item, detail)| JobItem { item, detail })
                    .collect()
            } else {
                Vec::new()
            },
        })
    }

    /// `jobs.cancel`: a request. A queued job is cancelled at once, and a
    /// running one stops after the step in flight.
    ///
    /// A session cancels only a job it could have started: a transcode
    /// needs `server:manage` to cancel as it did to start.
    pub(super) fn jobs_cancel(
        &self,
        stores: &Stores,
        params: &JobCancelParams,
        session: &crate::session::Session,
    ) -> Result<JobCancelled, RpcError> {
        let found = stores
            .server
            .job(params.job_id)
            .map_err(stored)?
            .ok_or_else(Self::no_job)?;
        if let Some(started_by) = ritornel_contract::methods::method(&found.kind)
            && !session.has_all(started_by.needs)
        {
            return Err(RpcError::new(
                ErrorCode::Forbidden,
                format!(
                    "cancelling a {} job requires what starting one does",
                    found.kind
                ),
            ));
        }
        let show_paths = session.has(ritornel_contract::Permission::FoldersManage);
        let command = Command {
            id: params.idempotency_key,
            fingerprint: fingerprint::<JobsCancel>(params)?,
        };
        let asked = stores
            .server
            .cancel_job(command, JobsCancel::NAME, params.job_id)
            .map_err(stored)?
            .ok_or_else(Self::no_job)?;
        self.publish_stored_job(stores, &asked);
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
            job: job_for(&asked, show_paths),
        })
    }

    pub(super) fn library_stats(&self, stores: &Stores) -> Result<LibraryStats, RpcError> {
        Self::library(stores, |library| library.stats().map_err(in_library))
    }

    /// `identity.dryRun`: the resolver run again, by the policy scans use,
    /// by another named policy, or with values the params put in its
    /// place. It writes nothing.
    pub(super) fn identity_dry_run(
        &self,
        stores: &Stores,
        params: &DryRunParams,
    ) -> Result<DryRun, RpcError> {
        let policy = &params.policy;
        for (field, value) in [
            ("bindMax", policy.bind_max),
            ("minGap", policy.min_gap),
            ("reviewBelow", policy.review_below),
        ] {
            if value.is_some_and(|value| !(0.0..=1.0).contains(&value)) {
                return Err(
                    RpcError::new(ErrorCode::Invalid, format!("{field} must be 0 to 1"))
                        .with_detail("field", field),
                );
            }
        }
        if policy.version.is_some_and(|version| !is_policy(version)) {
            return Err(RpcError::new(ErrorCode::Invalid, "version names no policy")
                .with_detail("field", "version"));
        }
        if policy
            .marker_list
            .is_some_and(|list| !(1..=2).contains(&list))
        {
            return Err(
                RpcError::new(ErrorCode::Invalid, "markerList must be 1 or 2")
                    .with_detail("field", "markerList"),
            );
        }
        if policy
            .across_albums_within_ms
            .is_some_and(|ms| ms > MAX_DURATION_GRACE_MS)
        {
            return Err(RpcError::new(
                ErrorCode::Invalid,
                "acrossAlbumsWithinMs must be at most a minute",
            )
            .with_detail("field", "acrossAlbumsWithinMs"));
        }
        if policy
            .duration_grace_ms
            .is_some_and(|ms| ms > MAX_DURATION_GRACE_MS)
        {
            return Err(RpcError::new(
                ErrorCode::Invalid,
                "durationGraceMs must be at most a minute",
            )
            .with_detail("field", "durationGraceMs"));
        }
        Self::library(stores, |library| {
            library.dry_run(params).map_err(in_library)
        })
    }

    /// `identity.review`: the bindings marked for review.
    pub(super) fn identity_review(&self, stores: &Stores) -> Result<ReviewList, RpcError> {
        Self::library(stores, |library| library.review().map_err(in_library))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_job_shows_no_path_to_a_session_without_folders() {
        let scanning = ritornel_engine::Job {
            id: 1,
            public_id: ritornel_contract::Id::from_bytes([1; 16]),
            kind: ScanStart::NAME.to_owned(),
            target: None,
            target_kind: None,
            params_json: "{}".to_owned(),
            state: ritornel_engine::JobState::Running,
            step: 0,
            checkpoint_json: None,
            cancel_requested: false,
            done: 1,
            total: Some(2),
            bytes: 0,
            failed: 0,
            skipped: 0,
            label: Some("Invented Artist/Invented Album/01 Song.flac".to_owned()),
            error: Some("/srv/invented/music: permission denied".to_owned()),
            created_at_ms: 0,
            started_at_ms: None,
            finished_at_ms: None,
        };
        let shown = job_for(&scanning, true);
        assert!(shown.label.is_some() && shown.error.is_some());
        let hidden = job_for(&scanning, false);
        assert_eq!((hidden.label, hidden.error), (None, None));
    }
}
