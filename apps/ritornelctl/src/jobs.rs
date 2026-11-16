//! `ritornelctl jobs`: the server's jobs, or one job in detail. A job is a
//! durable resource: it is here after the client that started it has gone,
//! and after it has ended. See decision 17 in docs/design/contract.md.

use std::io::Write;

use ritornel_client::contract::{Id, Job, JobDetail, JobsList};
use ritornel_client::local;

use crate::cli::{Error, JobsArgs};
use crate::server;
use crate::status::size;

pub(crate) fn jobs(args: &JobsArgs, env: local::Env<'_>, out: &mut dyn Write) -> Result<(), Error> {
    let job_id = match &args.job {
        Some(text) => Some(
            text.parse::<Id>()
                .map_err(|_| Error::Usage("that is not a job's ID"))?,
        ),
        None if args.cancel => return Err(Error::Usage("--cancel needs a job's ID")),
        None => None,
    };
    match job_id {
        None => {
            let list = server::ask(&args.server, env, async |client| client.jobs().await)?;
            server::print(
                &args.server,
                out,
                &list,
                serde_json::to_string_pretty,
                render_list,
            )
        }
        Some(job_id) => {
            let cancel = args.cancel;
            let detail = server::ask(&args.server, env, async |client| {
                if cancel {
                    client
                        .cancel_job(ritornel_client::idempotency_key()?, job_id)
                        .await?;
                }
                client.job(job_id).await
            })?;
            server::print(
                &args.server,
                out,
                &detail,
                serde_json::to_string_pretty,
                render,
            )
        }
    }
}

fn counted(job: &Job) -> String {
    match job.total {
        Some(total) => format!("{} of {total}", job.done),
        None => job.done.to_string(),
    }
}

/// The jobs, newest first, one a line.
fn render_list(list: &JobsList) -> String {
    if list.jobs.is_empty() {
        return "No jobs.\n".to_owned();
    }
    list.jobs
        .iter()
        .map(|job| {
            format!(
                "{}  {:<6}{:<11}{} read, {} unreadable\n",
                job.job_id,
                job.kind,
                job.state.as_str(),
                counted(job),
                job.failed
            )
        })
        .collect()
}

/// One job as a person reads it: what it is, how far it got, what a scan
/// found, and the first of what failed.
pub(crate) fn render(detail: &JobDetail) -> String {
    let job = &detail.job;
    let mut lines = vec![
        ("Job", job.job_id.to_string()),
        ("Kind", job.kind.clone()),
        ("State", job.state.as_str().to_owned()),
    ];
    if let Some(error) = &job.error {
        lines.push(("Error", error.clone()));
    }
    if let (Some(started), Some(finished)) = (job.started_at, job.finished_at) {
        let tenths = (finished - started).max(0) / 100;
        lines.push(("Took", format!("{}.{} s", tenths / 10, tenths % 10)));
    }
    lines.push(("Files read", counted(job)));
    lines.push(("Bytes read", size(job.bytes)));
    if let Some(scan) = &job.scan {
        lines.extend([
            ("Files found", scan.files.to_string()),
            ("Unchanged", scan.unchanged.to_string()),
            ("New", scan.new.to_string()),
            ("Changed", scan.changed.to_string()),
            ("Moved", scan.moved.to_string()),
            ("Returned", scan.returned.to_string()),
            ("Retired", scan.retired.to_string()),
            ("Missing", scan.missing.to_string()),
            ("Unreadable", scan.unreadable.to_string()),
            ("Bindings written", scan.assignments.to_string()),
            ("Recordings made", scan.recordings.to_string()),
            ("Releases made", scan.releases.to_string()),
        ]);
    }
    let mut text = server::facts(&lines);
    if !detail.failed_items.is_empty() {
        text.push_str("\nCould not be read:\n");
        for item in &detail.failed_items {
            text.push_str(&format!("  {}  {}\n", item.item, item.detail));
        }
    }
    text
}

#[cfg(test)]
pub(crate) mod tests {
    use ritornel_client::contract::{JobItem, JobState, JobTarget, ScanSummary};

    use super::*;

    /// A scan that has ended, as the server reports it.
    pub(crate) fn job() -> Job {
        Job {
            job_id: Id::from_bytes([0x10; 16]),
            kind: "scan".to_owned(),
            target: Some(JobTarget::Source {
                id: Id::from_bytes([0x50; 16]),
            }),
            state: JobState::Succeeded,
            done: 1500,
            total: Some(14_000),
            bytes: 31_234_000_000,
            failed: 3,
            skipped: 12_500,
            label: None,
            error: None,
            created_at: 1_000,
            started_at: Some(2_000),
            finished_at: Some(14_340),
            scan: Some(ScanSummary {
                files: 14_000,
                unchanged: 12_500,
                new: 1_490,
                changed: 10,
                unreadable: 3,
                bytes_read: 31_234_000_000,
                assignments: 1_497,
                recordings: 1_400,
                releases: 120,
                ..ScanSummary::default()
            }),
        }
    }

    #[test]
    fn a_job_is_shown_with_what_its_scan_found_and_what_failed() {
        let detail = JobDetail {
            job: job(),
            failed_items: vec![JobItem {
                item: "Elsewhere/broken.m4a".to_owned(),
                detail: "{\"kind\":\"malformed\"}".to_owned(),
            }],
        };
        assert_eq!(
            render(&detail),
            "\
Job               10101010-1010-1010-1010-101010101010
Kind              scan
State             succeeded
Took              12.3 s
Files read        1500 of 14000
Bytes read        31.2 GB
Files found       14000
Unchanged         12500
New               1490
Changed           10
Moved             0
Returned          0
Retired           0
Missing           0
Unreadable        3
Bindings written  1497
Recordings made   1400
Releases made     120

Could not be read:
  Elsewhere/broken.m4a  {\"kind\":\"malformed\"}
"
        );
    }

    #[test]
    fn the_jobs_are_listed_one_a_line_and_no_jobs_says_so() {
        let mut failed = job();
        failed.state = JobState::Failed;
        failed.total = None;
        let list = JobsList {
            jobs: vec![job(), failed],
        };
        assert_eq!(
            render_list(&list),
            "\
10101010-1010-1010-1010-101010101010  scan  succeeded  1500 of 14000 read, 3 unreadable
10101010-1010-1010-1010-101010101010  scan  failed     1500 read, 3 unreadable
"
        );
        assert_eq!(render_list(&JobsList { jobs: Vec::new() }), "No jobs.\n");
    }
}
