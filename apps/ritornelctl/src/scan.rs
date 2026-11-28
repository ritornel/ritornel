//! `ritornelctl scan <folder>`: scans a folder into the library, as a job
//! on the server. See "Jobs and outbox" in docs/internals/server.md.
//!
//! The folder is added as a source if it is not one, which makes it a named
//! root on the server. The server only ever reads it. The scan is the
//! server's, not this program's: it carries on if this program is stopped,
//! and `ritornelctl jobs` shows it.

use std::io::Write;
use std::time::Duration;

use ritornel_client::Client;
use ritornel_client::contract::{Id, Job, JobDetail, JobState};
use ritornel_client::local;

use crate::cli::{Error, ScanArgs};
use crate::jobs;
use crate::server;

/// How long this program waits between two looks at the job.
const LOOK_EVERY: Duration = Duration::from_millis(500);

/// One line of progress, as a person reads it.
fn progress(job: &Job) -> String {
    let total = job
        .total
        .map_or_else(|| "?".to_owned(), |total| total.to_string());
    let label = job.label.as_deref().unwrap_or("");
    format!(
        "{}: {} of {total} files read, {}, {} unreadable  {label}\n",
        job.state.as_str(),
        job.done,
        crate::status::size(job.bytes),
        job.failed
    )
}

/// Finds the source whose root is this folder, or adds the folder as one,
/// and starts a scan of it.
async fn start(client: &Client, path: &str, name: &str) -> Result<Job, ritornel_client::Error> {
    let sources = client.sources().await?;
    let source_id: Id = match sources
        .sources
        .iter()
        .find(|source| source.path.as_deref() == Some(path))
    {
        Some(source) => source.source_id,
        None => {
            let key = ritornel_client::idempotency_key()?;
            client.add_source(key, name, path).await?.source.source_id
        }
    };
    let key = ritornel_client::idempotency_key()?;
    Ok(client.start_scan(key, source_id).await?.job)
}

pub(crate) fn scan(args: &ScanArgs, env: local::Env<'_>, out: &mut dyn Write) -> Result<(), Error> {
    // The server is given the folder's real path, so that the same folder
    // is the same source however it was named on the command line.
    let folder = std::fs::canonicalize(&args.folder).map_err(Error::Folder)?;
    let (Some(path), true) = (folder.to_str(), folder.is_dir()) else {
        return Err(Error::Usage("scan needs a folder whose path is UTF-8"));
    };
    let name = match &args.name {
        Some(name) => name.clone(),
        None => folder
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("root")
            .to_owned(),
    };
    let quiet = args.server.json;
    let mut last = String::new();
    let detail = server::ask_for(
        &args.server,
        env,
        args.no_wait.then_some(server::ANSWER_WAIT),
        async |client| {
            let job = start(client, path, &name).await?;
            let mut detail = JobDetail {
                outcomes: Vec::new(),
                job,
                failed_items: Vec::new(),
            };
            if args.no_wait {
                return Ok(detail);
            }
            loop {
                detail = client.job(detail.job.job_id).await?;
                let line = progress(&detail.job);
                if !quiet && line != last {
                    // Nothing useful can be done if the terminal is gone.
                    let _ = out.write_all(line.as_bytes()).and_then(|()| out.flush());
                    last = line;
                }
                if detail.job.state.ended() {
                    break;
                }
                tokio::time::sleep(LOOK_EVERY).await;
            }
            Ok(detail)
        },
    )?;
    server::print(
        &args.server,
        out,
        &detail,
        serde_json::to_string_pretty,
        jobs::render,
    )?;
    match detail.job.state {
        JobState::Failed | JobState::Cancelled => Err(Error::Job(detail.job.state.as_str())),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_of_progress_says_how_far_the_scan_has_got() {
        let mut job = jobs::tests::job();
        job.state = JobState::Running;
        job.label = Some("Moss Arcade/Tin Weather/02.flac".to_owned());
        assert_eq!(
            progress(&job),
            "running: 1500 of 14000 files read, 31.2 GB, 3 unreadable  Moss Arcade/Tin Weather/02.flac\n"
        );
        job.total = None;
        job.label = None;
        assert_eq!(
            progress(&job),
            "running: 1500 of ? files read, 31.2 GB, 3 unreadable  \n"
        );
    }
}
