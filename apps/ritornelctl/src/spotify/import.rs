//! `ritornelctl spotify import <archive>`: reads the archive here, sends
//! its kept records to the hub as a staged run, and shows the dry run of
//! that run. Nothing is written to the library. See "Spotify streams" in
//! docs/internals/history.md.

use std::fmt::Write as _;
use std::io::Write;
use std::time::Duration;

use ritornel_client::contract::methods::{ImportsDryRun, ImportsGet, ImportsStage};
use ritornel_client::contract::{
    ImportProblem, ImportReport, ImportRun, ImportsDryRunParams, ImportsGetParams,
    ImportsStageParams, JobState, MAX_STREAMS_PER_PIECE, SpotifyStream, StreamTier,
};
use ritornel_client::local;
use serde_json::json;

use super::{Tally, read, render, to_json};
use crate::cli::{Error, SpotifyImportArgs};
use crate::server;

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
                    streams,
                })
                .await?;
        }
        let started = client
            .call::<ImportsDryRun>(&ImportsDryRunParams {
                idempotency_key: ritornel_client::idempotency_key()?,
                run_id,
            })
            .await?;
        let mut job = started.job;
        while !job.state.ended() {
            tokio::time::sleep(LOOK_EVERY).await;
            job = client.job(job.job_id).await?.job;
        }
        let run = client
            .call::<ImportsGet>(&ImportsGetParams { run_id })
            .await?;
        Ok((job, run))
    })?;
    let (job, run) = run;
    let text = if args.server.json {
        let mut text = serde_json::to_string_pretty(&json!({
            "read": to_json(&tally),
            "run": run,
            "job": job,
        }))
        .map_err(|error| Error::Output(error.into()))?;
        text.push('\n');
        text
    } else {
        rendered(&tally, &run, args.samples)
    };
    out.write_all(text.as_bytes())
        .and_then(|()| out.flush())
        .map_err(Error::Output)?;
    match job.state {
        JobState::Succeeded => Ok(()),
        state => Err(Error::Job(state.as_str())),
    }
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
fn rendered(tally: &Tally, run: &ImportRun, samples: bool) -> String {
    let mut text = render(tally).replace("Nothing was stored or sent.\n", "");
    let _ = writeln!(
        text,
        "\nSent {} streams to the hub as run {}, kept there for a day. Nothing was written to \
         the library.",
        run.streams, run.run_id
    );
    let Some(report) = &run.report else {
        text.push_str("The dry run made no report.\n");
        return text;
    };
    text.push_str(&report_text(report, samples));
    text
}

fn report_text(report: &ImportReport, samples: bool) -> String {
    let mut text = String::from("\nDry run\n");
    let mut lines = vec![
        (
            "Listens",
            format!("{} in {} streams", report.events, report.streams),
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
        "\nYear  Listens  Joined (A B C)        Alone: counted  not  unknown  Scrobbles alone  \
         Plays now  after   Counted now  after  No zone\n",
    );
    for year in &report.years {
        let _ = writeln!(
            text,
            "{:<4}  {:>7}  {:>7} {:>6} {:>6}  {:>14} {:>5} {:>8}  {:>15}  {:>9} {:>6}  {:>11} {:>6}  {:>7}",
            year.year,
            year.streams,
            year.joined_start,
            year.joined_during,
            year.joined_renamed,
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
    use ritornel_client::contract::SpotifyPlatform;

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
            platform: SpotifyPlatform::Ios,
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
