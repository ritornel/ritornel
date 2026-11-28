//! Library calls: sources, scans, jobs and what a scan produced. See
//! "Commands, playback, and jobs" and "Method summary" in
//! docs/internals/contract.md.
//!
//! A path appears only here, in the calls that manage sources ("Entities
//! and identifiers" in docs/internals/contract.md). Everything else a
//! client sees is an identity.

use serde::{Deserialize, Serialize};

use crate::{Id, Receipt};

/// What kind of place a source is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    /// A folder of master files: the whole of one named root.
    Folder,
}

/// One place the library's items come from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct Source {
    /// The source's ID.
    pub source_id: Id,
    /// Its name, which is the name of its root.
    pub name: String,
    /// What kind of place it is.
    pub kind: SourceKind,
    /// Where its root is on the server that answered. It is left out when
    /// that server has no path for the root, which is on another server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub path: Option<String>,
}

/// The result of `sources.list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SourcesList {
    /// Every folder source, in the order they were added.
    pub sources: Vec<Source>,
}

/// The params of `sources.add`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SourcesAddParams {
    /// The command's ID, made by the client.
    pub idempotency_key: Id,
    /// The root's name: 1 to 100 characters, with no `/`.
    pub name: String,
    /// The folder's absolute path on the server. The server only ever
    /// reads it.
    pub path: String,
}

/// The result of `sources.add`: the receipt, and the source that now
/// exists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SourceAdded {
    /// The receipt.
    #[serde(flatten)]
    pub receipt: Receipt,
    /// The new source.
    pub source: Source,
}

/// The params of `scan.start`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ScanStartParams {
    /// The command's ID, made by the client.
    pub idempotency_key: Id,
    /// The source to scan.
    pub source_id: Id,
}

/// The result of `scan.start`: the receipt, whose status is `accepted`,
/// and the job. Starting a scan of a source that is being scanned returns
/// the job that is running.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ScanStarted {
    /// The receipt.
    #[serde(flatten)]
    pub receipt: Receipt,
    /// The job.
    pub job: Job,
}

/// The params of `artists.importPictures`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ImportPicturesParams {
    /// The command's ID, made by the client.
    pub idempotency_key: Id,
    /// The picture folder's name, as a root's: 1 to 100 characters, with
    /// no `/`.
    pub name: String,
    /// The folder's absolute path on the server. The server only ever
    /// reads it.
    pub path: String,
}

/// The result of `artists.importPictures`: the receipt, and what the
/// import found among the artists with a record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct PicturesImported {
    /// The receipt.
    #[serde(flatten)]
    pub receipt: Receipt,
    /// The artists with a record.
    pub artists: u64,
    /// Those whose portrait was found by their MusicBrainz ID.
    pub by_mbid: u64,
    /// Those with no ID whose portrait was found by their name.
    pub by_name: u64,
    /// Those the folder has no portrait for.
    pub without: u64,
    /// Portraits the folder chose that could not be read.
    pub unreadable: u64,
    /// Portraits from this folder the library had, and the folder no
    /// longer chooses.
    pub dropped: u64,
    /// Artists with no ID whose names the folder makes the same as
    /// another's, whose portrait's manifest names neither. They are
    /// counted in `without` too.
    pub ambiguous: u64,
}

/// One picture folder the library takes artists' portraits from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct PictureFolder {
    /// Its name, as a root's.
    pub name: String,
    /// Where it is on the server that answered. It is left out when that
    /// server has no path for it, as for a source's root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub path: Option<String>,
    /// The portraits the library holds from it now.
    pub portraits: u64,
}

/// The result of `artists.pictureFolders`: the picture folders, and the
/// last import of one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct PictureFolders {
    /// Every picture folder, in the order they were first imported.
    pub folders: Vec<PictureFolder>,
    /// When the last import that wrote something was committed, asked for
    /// or started on its own, as Unix milliseconds. Left out before any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub last_import_at: Option<i64>,
    /// What that import answered, while the library keeps its receipt. One
    /// started on its own has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub last_import: Option<PicturesImported>,
}

/// Where a job is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    /// Waiting to start.
    Queued,
    /// Under way.
    Running,
    /// Asked to stop, and stopping after the item in flight.
    Cancelling,
    /// It ended and did what it was for.
    Succeeded,
    /// It ended on an error.
    Failed,
    /// It ended because it was asked to.
    Cancelled,
}

impl JobState {
    /// The state as it is written on the wire.
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

    /// Whether the job has ended.
    pub const fn ended(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
}

/// What a job works on, and so what its events invalidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum JobTarget {
    /// The server whose cache the job changes.
    Server {
        /// The server ID.
        id: Id,
    },
    /// A source.
    Source {
        /// The source's ID.
        id: Id,
    },
    /// An account at an outside service, such as Last.fm.
    Account {
        /// The account's ID.
        id: Id,
    },
    /// A run of streams staged for import.
    ImportRun {
        /// The run's ID.
        id: Id,
    },
    /// A player or a folder the library syncs to.
    Device {
        /// The device's ID.
        id: Id,
    },
}

/// One job: a durable resource, readable after clients disconnect and
/// after it ends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct Job {
    /// The job's ID.
    pub job_id: Id,
    /// What kind of work it is, such as `scan`.
    pub kind: String,
    /// What it works on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub target: Option<JobTarget>,
    /// Where it is in its life.
    pub state: JobState,
    /// Items done. For a scan, files read.
    pub done: u64,
    /// Items in all. It is left out while that is not known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub total: Option<u64>,
    /// Bytes read.
    pub bytes: u64,
    /// Items that failed. For a scan, files that could not be read.
    pub failed: u64,
    /// Items passed over. For a scan, files that had not changed.
    pub skipped: u64,
    /// What it is doing now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub label: Option<String>,
    /// Why it failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub error: Option<String>,
    /// When it was created, as Unix milliseconds.
    pub created_at: i64,
    /// When it first started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub started_at: Option<i64>,
    /// When it ended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub finished_at: Option<i64>,
    /// What a scan has found so far, and at its end what came out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub scan: Option<ScanSummary>,
    /// What a sync of an account's history has read so far, and at its end
    /// what came out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub sync: Option<crate::SyncSummary>,
}

/// What a scan found and did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase", default)]
pub struct ScanSummary {
    /// Audio files found under the root.
    pub files: u64,
    /// Files left alone: the same size and time as when last read.
    pub unchanged: u64,
    /// Files at paths the library had not seen.
    pub new: u64,
    /// Files read again at a path it knew: retagged, touched, or the same
    /// song encoded again.
    pub changed: u64,
    /// Files whose audio turned up at a new path while the old one was
    /// gone.
    pub moved: u64,
    /// Files that had been missing and are back.
    pub returned: u64,
    /// Files where a different song took an old path, so the old item was
    /// retired.
    pub retired: u64,
    /// Files the library knew and the scan no longer found.
    pub missing: u64,
    /// Files that could not be read as audio.
    pub unreadable: u64,
    /// Bytes of the files read.
    pub bytes_read: u64,
    /// Bindings written when the library was resolved.
    pub assignments: u64,
    /// Recordings created.
    pub recordings: u64,
    /// Releases created.
    pub releases: u64,
}

/// The result of `jobs.list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct JobsList {
    /// The newest jobs, newest first.
    pub jobs: Vec<Job>,
}

/// The params of `jobs.get`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct JobGetParams {
    /// The job.
    pub job_id: Id,
}

/// One item of a job that failed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct JobItem {
    /// What the item was. For a scan, a path relative to the root.
    pub item: String,
    /// Why it failed.
    pub detail: String,
}

/// The result of `jobs.get`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct JobDetail {
    /// What became of each appearance of a transcode, in request order. Job history does not pin them.
    #[serde(default)]
    pub outcomes: Vec<crate::transcode::TranscodeOutcome>,
    /// The job.
    pub job: Job,
    /// The first of its items that failed.
    pub failed_items: Vec<JobItem>,
}

/// The params of `jobs.cancel`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct JobCancelParams {
    /// The command's ID, made by the client.
    pub idempotency_key: Id,
    /// The job to stop.
    pub job_id: Id,
}

/// The result of `jobs.cancel`: the receipt, and the job as it then is.
/// Cancel is a request: a running job stops after the step in flight.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct JobCancelled {
    /// The receipt.
    #[serde(flatten)]
    pub receipt: Receipt,
    /// The job.
    pub job: Job,
}

/// One named count.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct Count {
    /// What was counted.
    pub name: String,
    /// How many.
    pub count: u64,
}

/// The result of `library.stats`: what the library holds, and what the
/// scans made of it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct LibraryStats {
    /// The library revision the counts were read at.
    pub library_revision: u64,
    /// Files the library has, which are not retired.
    pub files: u64,
    /// Their size in bytes.
    pub file_bytes: u64,
    /// Of them, files the last scan of their root did not find.
    pub files_missing: u64,
    /// Of them, files that could not be read as audio.
    pub files_unreadable: u64,
    /// Why, most common first.
    pub unreadable_reasons: Vec<Count>,
    /// Files whose old path a different song took.
    pub files_retired: u64,
    /// Readable files by codec, most common first.
    pub codecs: Vec<Count>,
    /// Files that carry any MusicBrainz ID in their tags.
    pub files_with_musicbrainz_id: u64,
    /// Readable files with no binding yet.
    pub unresolved: u64,
    /// Artists with something credited to them.
    pub artists: u64,
    /// Release groups with a release in them.
    pub release_groups: u64,
    /// Releases with a track on them.
    pub releases: u64,
    /// Recordings with a file bound to them.
    pub recordings: u64,
    /// Track appearances with a file bound to them.
    pub appearances: u64,
    /// Of them, appearances on no release.
    pub loose_tracks: u64,
    /// Current bindings by how they were decided.
    pub decisions: Vec<Count>,
    /// Current bindings marked for review.
    pub needs_review: u64,
    /// Release groups whose kind is known.
    pub groups_with_kind: u64,
    /// Those by kind.
    pub kinds: Vec<Count>,
    /// Release groups that have a first year.
    pub groups_with_first_year: u64,
    /// Of them, those whose year came from an original-date tag and not
    /// from the earliest date among their releases.
    pub groups_with_first_year_from_tag: u64,
    /// Distinct pictures recorded.
    pub pictures: u64,
    /// Releases that have a cover.
    pub releases_with_cover: u64,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::ReceiptStatus;

    #[test]
    fn a_job_names_its_target_and_leaves_out_what_is_not_known() {
        let job = Job {
            job_id: Id::from_bytes([0x10; 16]),
            kind: "scan".to_owned(),
            target: Some(JobTarget::Source {
                id: Id::from_bytes([0x50; 16]),
            }),
            state: JobState::Running,
            done: 3,
            total: None,
            bytes: 9,
            failed: 0,
            skipped: 1,
            label: None,
            error: None,
            created_at: 5,
            started_at: Some(6),
            finished_at: None,
            scan: None,
            sync: None,
        };
        let wire = json!({
            "jobId": "10101010-1010-1010-1010-101010101010",
            "kind": "scan",
            "target": { "type": "source", "id": "50505050-5050-5050-5050-505050505050" },
            "state": "running",
            "done": 3,
            "bytes": 9,
            "failed": 0,
            "skipped": 1,
            "createdAt": 5,
            "startedAt": 6,
        });
        assert_eq!(serde_json::to_value(&job).unwrap(), wire);
        assert_eq!(serde_json::from_value::<Job>(wire).unwrap(), job);
        let started = ScanStarted {
            receipt: Receipt {
                command_id: Id::from_bytes([0xc0; 16]),
                status: ReceiptStatus::Accepted,
                revision: None,
            },
            job,
        };
        let wire = serde_json::to_value(&started).unwrap();
        assert_eq!(wire["status"], "accepted");
        assert_eq!(wire["job"]["state"], "running");
        assert_eq!(
            serde_json::from_value::<ScanStarted>(wire).unwrap(),
            started
        );
    }

    #[test]
    fn a_source_carries_its_path_only_where_the_server_has_one() {
        let mut source = Source {
            source_id: Id::from_bytes([0x50; 16]),
            name: "main".to_owned(),
            kind: SourceKind::Folder,
            path: Some("/srv/invented".to_owned()),
        };
        assert_eq!(
            serde_json::to_value(&source).unwrap()["path"],
            "/srv/invented"
        );
        source.path = None;
        assert_eq!(
            serde_json::to_value(&source).unwrap(),
            json!({ "sourceId": "50505050-5050-5050-5050-505050505050", "name": "main", "kind": "folder" })
        );
        for state in [JobState::Queued, JobState::Running, JobState::Cancelling] {
            assert!(!state.ended());
            assert_eq!(serde_json::to_value(state).unwrap(), state.as_str());
        }
        assert!(
            JobState::Cancelled.ended() && JobState::Failed.ended() && JobState::Succeeded.ended()
        );
    }
}
