//! `ritornelctl spotify import <archive>`: reads the archive here, sends
//! its kept records to the hub as a staged run, and shows the dry run of
//! that run, writing nothing to the library. `spotify apply <run>` applies
//! a staged run, `spotify undo <run>` takes an applied one out of force, and
//! `spotify runs` lists the runs the library holds. See "Spotify streams" in
//! docs/internals/history.md.

use std::fmt::Write as _;
use std::io::Write;
use std::time::Duration;

use ritornel_client::Client;
use ritornel_client::contract::methods::{
    ImportsApply, ImportsDryRun, ImportsGet, ImportsList, ImportsStage, ImportsUndo, Method,
};
use ritornel_client::contract::{
    AppliedRun, Id, ImportProblem, ImportReport, ImportRun, ImportsGetParams, ImportsJobStarted,
    ImportsRunParams, ImportsStageParams, Job, JobState, MAX_STREAMS_PER_PIECE, NoParams,
    SpotifyStream, StreamTier,
};
use ritornel_client::local;
use serde_json::json;

use super::{Tally, day, read, render_tally, to_json};
use crate::cli::{Error, ServerArgs, SpotifyImportArgs, SpotifyRunArgs};
use crate::server;

/// What read an archive, as a run records it.
const READER: &str = concat!(env!("CARGO_PKG_NAME"), " ", env!("CARGO_PKG_VERSION"));

/// How large one piece may be on the wire: half a message's limit, so that
/// the envelope and any long names never take it over.
const PIECE_BYTES: usize = 512 * 1024;

/// How long this program waits between two looks at the dry run.
const LOOK_EVERY: Duration = Duration::from_millis(500);

/// The streams cut into pieces, each at most [`MAX_STREAMS_PER_PIECE`]
/// streams and about [`PIECE_BYTES`] on the wire.
fn pieces(streams: Vec<SpotifyStream>) -> Vec<Vec<SpotifyStream>> {
    let mut pieces = Vec::new();
    let (mut piece, mut bytes) = (Vec::new(), 0);
    for stream in streams {
        let size = serde_json::to_vec(&stream).map_or(0, |written| written.len() + 1);
        if !piece.is_empty() && (piece.len() == MAX_STREAMS_PER_PIECE || bytes + size > PIECE_BYTES)
        {
            pieces.push(std::mem::take(&mut piece));
            bytes = 0;
        }
        bytes += size;
        piece.push(stream);
    }
    if !piece.is_empty() {
        pieces.push(piece);
    }
    pieces
}

pub(crate) fn import(
    args: &SpotifyImportArgs,
    env: local::Env<'_>,
    out: &mut dyn Write,
) -> Result<(), Error> {
    let archive = read(&args.archive).map_err(Error::Archive)?;
    if archive.streams.is_empty() {
        return Err(Error::Nothing("the archive holds no streams of music"));
    }
    let tally = archive.tally;
    let pieces = pieces(archive.streams);
    let count =
        u32::try_from(pieces.len()).map_err(|_| Error::Nothing("the archive is too large"))?;
    let run = server::ask_for(&args.server, env, None, async |client| {
        let run_id = ritornel_client::idempotency_key()?;
        for (at, streams) in pieces.into_iter().enumerate() {
            client
                .call::<ImportsStage>(&ImportsStageParams {
                    idempotency_key: ritornel_client::idempotency_key()?,
                    run_id,
                    piece: u32::try_from(at).unwrap_or(u32::MAX),
                    pieces: count,
                    reader: READER.to_owned(),
                    streams,
                })
                .await?;
        }
        let job = run_job::<ImportsDryRun>(client, run_id).await?;
        let run = client
            .call::<ImportsGet>(&ImportsGetParams { run_id })
            .await?;
        Ok((job, run))
    })?;
    let (job, run) = run;
    print(
        &args.server,
        out,
        &json!({ "read": to_json(&tally), "run": run, "job": job }),
        || rendered(&tally, &run, &job, args.samples),
    )?;
    match job.state {
        JobState::Succeeded => Ok(()),
        state => Err(Error::Job(state.as_str())),
    }
}

/// Starts a run's job and waits for it to end.
async fn run_job<M>(client: &Client, run_id: Id) -> Result<Job, ritornel_client::Error>
where
    M: Method<Params = ImportsRunParams, Result = ImportsJobStarted>,
{
    let started = client
        .call::<M>(&ImportsRunParams {
            idempotency_key: ritornel_client::idempotency_key()?,
            run_id,
        })
        .await?;
    let mut job = started.job;
    while !job.state.ended() {
        tokio::time::sleep(LOOK_EVERY).await;
        job = client.job(job.job_id).await?.job;
    }
    Ok(job)
}

fn run_id(args: &SpotifyRunArgs) -> Result<Id, Error> {
    args.run
        .parse::<Id>()
        .map_err(|_| Error::Usage("that is not a run's ID"))
}

/// Writes `text`, or `json` when asked for JSON.
fn print(
    args: &ServerArgs,
    out: &mut dyn Write,
    json: &serde_json::Value,
    text: impl FnOnce() -> String,
) -> Result<(), Error> {
    let text = if args.json {
        let mut text =
            serde_json::to_string_pretty(json).map_err(|error| Error::Output(error.into()))?;
        text.push('\n');
        text
    } else {
        text()
    };
    out.write_all(text.as_bytes())
        .and_then(|()| out.flush())
        .map_err(Error::Output)
}

/// `spotify apply <run>`.
pub(crate) fn apply(
    args: &SpotifyRunArgs,
    env: local::Env<'_>,
    out: &mut dyn Write,
) -> Result<(), Error> {
    let run_id = run_id(args)?;
    let (job, run, applied) = server::ask_for(&args.server, env, None, async |client| {
        let job = run_job::<ImportsApply>(client, run_id).await?;
        // The staged run, with the report applying worked out, while the
        // hub keeps it; the run as the library holds it in any case.
        let run = client
            .call::<ImportsGet>(&ImportsGetParams { run_id })
            .await
            .ok();
        let applied = match run.as_ref().and_then(|run| run.applied.clone()) {
            Some(applied) => Some(applied),
            None => client
                .call::<ImportsList>(&NoParams {})
                .await?
                .runs
                .into_iter()
                .find(|run| run.run_id == run_id),
        };
        Ok((job, run, applied))
    })?;
    print(
        &args.server,
        out,
        &json!({ "run": run, "applied": applied, "job": job }),
        || {
            let mut text = String::new();
            match (&job.state, &applied) {
                (JobState::Succeeded, Some(applied)) => {
                    let _ = writeln!(
                        text,
                        "Applied run {}: {} streams, {} of them new to the library. It is {}.",
                        applied.run_id,
                        applied.streams,
                        applied.new_streams,
                        if applied.in_force {
                            "in force"
                        } else {
                            "out of force: it was undone, and importing its archive again \
                             brings it back"
                        }
                    );
                    if let Some(report) = run.as_ref().and_then(|run| run.report.as_ref()) {
                        text.push_str(&report_text(
                            report,
                            "What applying did, as worked out in the write that applied it",
                            false,
                        ));
                    }
                }
                _ => {
                    let _ = writeln!(
                        text,
                        "The run was not applied: {}.",
                        job.error.as_deref().unwrap_or("it did not finish")
                    );
                }
            }
            text
        },
    )?;
    match job.state {
        JobState::Succeeded => Ok(()),
        state => Err(Error::Job(state.as_str())),
    }
}

/// `spotify undo <run>`.
pub(crate) fn undo(
    args: &SpotifyRunArgs,
    env: local::Env<'_>,
    out: &mut dyn Write,
) -> Result<(), Error> {
    let run_id = run_id(args)?;
    let (job, runs) = server::ask_for(&args.server, env, None, async |client| {
        let job = run_job::<ImportsUndo>(client, run_id).await?;
        let runs = client.call::<ImportsList>(&NoParams {}).await?;
        Ok((job, runs))
    })?;
    let run = runs.runs.into_iter().find(|run| run.run_id == run_id);
    print(
        &args.server,
        out,
        &json!({ "run": run, "job": job }),
        || match (&job.state, &run) {
            (JobState::Succeeded, Some(run)) => format!(
                "Run {} is out of force. Its {} streams stay stored, and any that a run in \
                 force holds too stay in force.\n",
                run.run_id, run.streams
            ),
            _ => format!(
                "The run was not undone: {}.\n",
                job.error.as_deref().unwrap_or("it did not finish")
            ),
        },
    )?;
    match job.state {
        JobState::Succeeded => Ok(()),
        state => Err(Error::Job(state.as_str())),
    }
}

fn run_line(run: &AppliedRun) -> String {
    let state = match run.undone_at {
        Some(undone) if !run.in_force => format!("undone {}", day(undone)),
        _ => "in force".to_owned(),
    };
    format!(
        "{}  applied {}  {state}  {} streams, {} new  {}\n",
        run.run_id,
        day(run.applied_at),
        run.streams,
        run.new_streams,
        run.reader
    )
}

/// `spotify runs`.
pub(crate) fn runs(
    args: &ServerArgs,
    env: local::Env<'_>,
    out: &mut dyn Write,
) -> Result<(), Error> {
    let runs = server::ask(args, env, async |client| {
        client.call::<ImportsList>(&NoParams {}).await
    })?;
    print(args, out, &json!(runs), || {
        if runs.runs.is_empty() {
            return "No Spotify history has been applied to this library.\n".to_owned();
        }
        runs.runs.iter().map(run_line).collect()
    })
}

fn tier_name(tier: StreamTier) -> &'static str {
    match tier {
        StreamTier::Start => "A, at the start",
        StreamTier::During => "B, while playing",
        StreamTier::Renamed => "C, renamed",
    }
}

fn minutes(ms: i64) -> String {
    let seconds = ms / 1000;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

/// What was read, the run, and its dry run, as a person reads them.
fn rendered(tally: &Tally, run: &ImportRun, job: &Job, samples: bool) -> String {
    let mut text = render_tally(tally);
    let _ = writeln!(
        text,
        "\nSent {} streams to the hub as run {}, kept there for a day. Nothing was written to \
         the library. To apply it: ritornelctl spotify apply {}",
        run.streams, run.run_id, run.run_id
    );
    let Some(report) = &run.report else {
        let _ = writeln!(
            text,
            "The dry run made no report: {}.",
            job.error.as_deref().unwrap_or("it did not finish")
        );
        return text;
    };
    text.push_str(&report_text(report, "Dry run", samples));
    text
}

fn report_text(report: &ImportReport, heading: &str, samples: bool) -> String {
    let mut text = format!("\n{heading}\n");
    let mut lines = vec![
        (
            "Listens",
            format!(
                "{} in {} streams; {} stored already, {} of whose rows this archive spells \
                 otherwise, which changes nothing",
                report.events,
                report.streams,
                report
                    .years
                    .iter()
                    .map(|year| year.already_stored)
                    .sum::<u64>(),
                report.respelled
            ),
        ),
        (
            "Scrobbles offered",
            format!(
                "{}; {} set aside and {} of this library's own listens are offered to none",
                report.scrobbles, report.scrobbles_excluded, report.scrobbles_echoes
            ),
        ),
    ];
    let tiers: Vec<(&str, String)> = report
        .tiers
        .iter()
        .map(|tier| {
            (
                tier_name(tier.tier),
                format!(
                    "{} joined, {} second reports{}",
                    tier.joined,
                    tier.second,
                    if tier.in_force {
                        ""
                    } else {
                        ", not yet in force"
                    }
                ),
            )
        })
        .collect();
    lines.extend(tiers);
    lines.push((
        "Offline",
        format!(
            "{} streams, {} with a time: {} read as seconds near the end, {} as milliseconds",
            report.offline,
            report.offline_timestamped,
            report.offline_seconds,
            report.offline_milliseconds
        ),
    ));
    lines.push((
        "Paused",
        format!(
            "{} scrobbles no stream took sit where a pause would put them",
            report.paused
        ),
    ));
    text.push_str(&server::facts(&lines));
    text.push_str(
        "\nYear  Listens  Joined (A B C pin)          Alone: counted   not  unknown  Scrobbles alone  \
         Plays now  after  Counted now  after  No zone\n",
    );
    for year in &report.years {
        let _ = writeln!(
            text,
            "{:<4}  {:>7}  {:>7} {:>6} {:>6} {:>4}  {:>14} {:>5} {:>8}  {:>15}  {:>9} {:>6}  {:>11} {:>6}  {:>7}",
            year.year,
            year.streams,
            year.joined_start,
            year.joined_during,
            year.joined_renamed,
            year.joined_pinned,
            year.alone_counted,
            year.alone_not_counted,
            year.alone_unknown,
            year.scrobbles_alone,
            year.plays_before,
            year.plays_after,
            year.counted_before,
            year.counted_after,
            year.no_zone
        );
    }
    if !report.problems.is_empty() {
        text.push_str("\nIn the way\n");
        for problem in &report.problems {
            text.push_str(match problem {
                ImportProblem::NoZone => {
                    "  Some streams fall where no zone covers, so they would have no local date: \
                     say where you were then, with `ritornelctl history zone add`.\n"
                }
                ImportProblem::TierNotInForce => {
                    "  A tier not yet in force finds joins: until it is, what it would join stands \
                     as plays of its own.\n"
                }
            });
        }
    }
    if samples && !report.samples.is_empty() {
        text.push_str("\nSamples of the tiers not yet in force: stream | scrobble\n");
        for sample in &report.samples {
            let _ = writeln!(
                text,
                "  {}{}  {:+} s  {}  {} - {} | {} - {}",
                tier_name(sample.tier),
                if sample.second { ", second" } else { "" },
                sample.from_start_ms / 1000,
                minutes(sample.ms_played),
                sample.stream_artist,
                sample.stream_title,
                sample.scrobble_artist,
                sample.scrobble_title
            );
        }
    }
    text
}

#[cfg(test)]
mod tests {

    use super::*;

    fn stream(title: String) -> SpotifyStream {
        SpotifyStream {
            ended_at: 1_552_684_449_000,
            ms_played: 221_000,
            track_uri: "spotify:track:0000000000000000000001".to_owned(),
            title,
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

    #[test]
    fn pieces_hold_at_most_a_thousand_streams_and_half_a_message() {
        let short: Vec<_> = (0..2500).map(|at| stream(format!("Song {at}"))).collect();
        let cut = pieces(short);
        assert_eq!(
            cut.iter().map(Vec::len).collect::<Vec<_>>(),
            [1000, 1000, 500]
        );
        let long: Vec<_> = (0..1000).map(|_| stream("Low Tide ".repeat(200))).collect();
        let cut = pieces(long);
        assert!(cut.len() > 2);
        for piece in &cut {
            assert!(serde_json::to_vec(piece).unwrap().len() <= PIECE_BYTES + 4096);
        }
        assert_eq!(cut.iter().map(Vec::len).sum::<usize>(), 1000);
    }
}
