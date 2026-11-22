//! `ritornelctl lastfm`: signing the server in to Last.fm, the sync of the
//! history, the report of what matched, and scrobbling; and `ritornelctl
//! show`, one track with its plays and loves. See "Last.fm" in
//! docs/design/history.md.
//!
//! The server holds the session and does the work. This program never sees
//! the API key, the secret or the session key: what it prints of a sign-in
//! is the address where it is approved, which Last.fm's desktop flow puts
//! in front of the person signing in.

use std::io::Write;
use std::time::Duration;

use ritornel_client::contract::methods::{
    JobsGet, LastfmReportCall, LastfmSetScrobbling, LastfmSignInFinish, LastfmSignInStart,
    LastfmSignOut, LastfmStatusCall, LastfmSync, TracksGet,
};
use ritornel_client::contract::{
    Id, Job, JobGetParams, JobState, KeyParams, LastfmReport, LastfmReportParams, LastfmStatus,
    LastfmSyncParams, NoParams, ReceiptStatus, ScrobblingParams, ScrobblingSet, SignInFinishParams,
    SignedOut, SyncSummary, TrackDetail, TrackGetParams,
};
use ritornel_client::local;

use crate::cli::{Error, LastfmCommand, ShowArgs};
use crate::server;

/// How long this program waits between two looks at a sync.
const LOOK_EVERY: Duration = Duration::from_millis(500);

/// How long between two asks whether a sign-in is approved.
const ASK_EVERY: Duration = Duration::from_secs(3);

/// How long a sign-in is waited for: Last.fm's token lasts an hour, and a
/// person who has not approved in ten minutes is not at the browser.
const SIGN_IN_WAIT: Duration = Duration::from_secs(10 * 60);

/// An instant as a UTC date and time.
pub(crate) fn utc(at_ms: i64) -> String {
    let days = at_ms.div_euclid(86_400_000);
    let ms = at_ms.rem_euclid(86_400_000);
    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02} UTC",
        ms / 3_600_000,
        ms / 60_000 % 60
    )
}

fn share(part: u64, whole: u64) -> String {
    if whole == 0 {
        return "-".to_owned();
    }
    format!("{:.1}%", part as f64 * 100.0 / whole as f64)
}

fn render_status(status: &LastfmStatus) -> String {
    if !status.available {
        return "This server was built without Last.fm.\n".to_owned();
    }
    let mut lines = vec![
        (
            "API key",
            if status.configured {
                "set".to_owned()
            } else {
                "not set: SCROBL_API_KEY and SCROBL_API_SECRET, or lastfm.env".to_owned()
            },
        ),
        (
            "Account",
            status
                .account
                .clone()
                .unwrap_or_else(|| "not signed in".to_owned()),
        ),
    ];
    if status.sign_in_again {
        lines.push(("Session", "refused by Last.fm: sign in again".to_owned()));
    }
    lines.push((
        "Scrobbling",
        status.scrobbling_since.map_or_else(
            || "off".to_owned(),
            |since| format!("on, since {}", utc(since)),
        ),
    ));
    lines.push(("Imported", status.imported.to_string()));
    lines.push((
        "Zones",
        if status.zones_set {
            "set".to_owned()
        } else {
            "none: add one with `ritornelctl history zone add`".to_owned()
        },
    ));
    for count in &status.deliveries {
        lines.push(("Scrobbles", format!("{} {}", count.count, count.state)));
    }
    let import = &status.import;
    lines.push((
        "Last import",
        import.last.as_ref().map_or_else(
            || "never".to_owned(),
            |last| match &last.failure {
                Some(failure) => format!("{}, failed: {failure}", utc(last.ended)),
                None => format!("{}, {}", utc(last.ended), last.state.as_str()),
            },
        ),
    ));
    lines.push((
        "Next import",
        match (&import.off, import.running, import.next) {
            (_, true, _) => "running now".to_owned(),
            (Some(off), _, _) => format!("only by hand: {off}"),
            (None, false, Some(next)) => {
                let open = import
                    .on_open_after
                    .filter(|&after| after < next)
                    .map(|after| format!(", or when the app opens after {}", utc(after)))
                    .unwrap_or_default();
                format!("{}{open}", utc(next))
            }
            (None, false, None) => "-".to_owned(),
        },
    ));
    server::facts(&lines)
}

pub(crate) fn render_summary(summary: &SyncSummary) -> String {
    let mut lines = vec![
        ("Read", summary.read.to_string()),
        ("New", summary.new.to_string()),
    ];
    if let Some(total) = summary.source_total {
        lines.push(("At Last.fm", total.to_string()));
    }
    if let Some(stored) = summary.stored {
        lines.push(("Stored", stored.to_string()));
    }
    if summary.gone > 0 {
        lines.push((
            "Gone",
            format!("{}: stored here, no longer at Last.fm, kept", summary.gone),
        ));
    }
    if let Some(loved) = summary.loved {
        lines.push(("Loved", loved.to_string()));
    }
    if summary.matched_after > 0 {
        lines.push(("Matched again", summary.matched_after.to_string()));
    }
    server::facts(&lines)
}

fn render_job(job: &Job) -> String {
    let mut text = format!("Sync {}: {}\n", job.job_id, job.state.as_str());
    if let Some(error) = &job.error {
        text.push_str(&format!("{error}\n"));
    }
    if let Some(summary) = &job.sync {
        text.push_str(&render_summary(summary));
    }
    text
}

fn render_report(report: &LastfmReport) -> String {
    let mut text = server::facts(&[
        ("Account", report.account.clone()),
        (
            "Imported",
            format!(
                "{} scrobbles, {} of them echoes of plays sent from here",
                report.imported, report.echoes
            ),
        ),
        (
            "Plays matched",
            format!(
                "{} of {} ({})",
                report.matched_plays,
                report.plays,
                share(report.matched_plays, report.plays)
            ),
        ),
        (
            "Tracks matched",
            format!(
                "{} of {} ({})",
                report.matched_tracks,
                report.tracks,
                share(report.matched_tracks, report.tracks)
            ),
        ),
        (
            "Doubtful",
            format!(
                "{} tracks, {} plays",
                report.doubtful_tracks, report.doubtful_plays
            ),
        ),
        (
            "Not in library",
            format!(
                "{} tracks, {} plays",
                report.absent_tracks, report.absent_plays
            ),
        ),
        (
            "Loved",
            format!("{}, {} matched", report.loved, report.loved_matched),
        ),
    ]);
    if !report.doubtful.is_empty() {
        text.push_str("\nDoubtful, most played first:\n");
        for track in &report.doubtful {
            let album = track
                .album
                .as_deref()
                .map_or_else(String::new, |album| format!(" [{album}]"));
            text.push_str(&format!(
                "{:>6}  {} — {}{album}  ({})\n",
                track.plays, track.artist, track.title, track.doubt
            ));
            for candidate in &track.candidates {
                text.push_str(&format!(
                    "          near: {} — {}\n",
                    candidate.artist.as_deref().unwrap_or("?"),
                    candidate.title.as_deref().unwrap_or("?")
                ));
            }
        }
    }
    text
}

fn render_track(track: &TrackDetail) -> String {
    let mut lines = vec![
        (
            "Title",
            track.title.clone().unwrap_or_else(|| "?".to_owned()),
        ),
        (
            "Artist",
            track.artist.clone().unwrap_or_else(|| "?".to_owned()),
        ),
    ];
    if let Some(album) = &track.album {
        lines.push(("Album", album.clone()));
    }
    lines.push(("Track ID", track.track_id.to_string()));
    lines.push(("Plays", track.plays.to_string()));
    if let Some(last) = track.last_played {
        lines.push(("Last played", utc(last)));
    }
    if track.loves.is_empty() {
        lines.push(("Loved", "no".to_owned()));
    }
    for love in &track.loves {
        lines.push(("Loved", format!("by {}, {}", love.by, utc(love.at))));
    }
    server::facts(&lines)
}

pub(crate) fn show(args: &ShowArgs, env: local::Env<'_>, out: &mut dyn Write) -> Result<(), Error> {
    let track_id: Id = args
        .track
        .parse()
        .map_err(|_| Error::Usage("show needs a track's ID"))?;
    let track = server::ask(&args.server, env, async |client| {
        client.call::<TracksGet>(&TrackGetParams { track_id }).await
    })?;
    server::print(
        &args.server,
        out,
        &track,
        serde_json::to_string_pretty,
        render_track,
    )
}

pub(crate) fn lastfm(
    command: &LastfmCommand,
    env: local::Env<'_>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<(), Error> {
    match command {
        LastfmCommand::Status(args) => {
            let status = server::ask(args, env, async |client| {
                client.call::<LastfmStatusCall>(&NoParams {}).await
            })?;
            server::print(
                args,
                out,
                &status,
                serde_json::to_string_pretty,
                render_status,
            )
        }
        LastfmCommand::Signin(args) => {
            let account = server::ask_for(args, env, Some(SIGN_IN_WAIT), async |client| {
                let started = client.call::<LastfmSignInStart>(&NoParams {}).await?;
                let printed = if args.json {
                    writeln!(err, "{}", started.approval_url).and_then(|()| err.flush())
                } else {
                    writeln!(
                        out,
                        "Open this address, sign in to Last.fm there and allow access:\n\n  {}\n\n\
                         Waiting for Last.fm to say it is allowed…",
                        started.approval_url
                    )
                    .and_then(|()| out.flush())
                };
                if let Err(error) = printed {
                    return Ok(Err(Error::Output(error)));
                }
                loop {
                    let finished = client
                        .call::<LastfmSignInFinish>(&SignInFinishParams {
                            idempotency_key: ritornel_client::idempotency_key()?,
                            sign_in: started.sign_in,
                        })
                        .await?;
                    if finished.receipt.status == ReceiptStatus::Done {
                        return Ok(Ok(finished));
                    }
                    tokio::time::sleep(ASK_EVERY).await;
                }
            })??;
            server::print(args, out, &account, serde_json::to_string_pretty, |done| {
                format!(
                    "Signed in to Last.fm as {}.\n",
                    done.account.as_deref().unwrap_or("?")
                )
            })
        }
        LastfmCommand::Signout(args) => {
            let gone = server::ask(args, env, async |client| {
                client
                    .call::<LastfmSignOut>(&KeyParams {
                        idempotency_key: ritornel_client::idempotency_key()?,
                    })
                    .await
            })?;
            server::print(
                args,
                out,
                &gone,
                serde_json::to_string_pretty,
                |gone: &SignedOut| {
                    if gone.was_signed_in {
                        "Signed out of Last.fm. What was imported stays.\n".to_owned()
                    } else {
                        "Not signed in to Last.fm.\n".to_owned()
                    }
                },
            )
        }
        LastfmCommand::Sync(args) => {
            let (full, wait, quiet) = (args.full, !args.no_wait, args.server.json);
            let mut last = String::new();
            let job = server::ask_for(
                &args.server,
                env,
                args.no_wait.then_some(server::ANSWER_WAIT),
                async |client| {
                    let mut job = client
                        .call::<LastfmSync>(&LastfmSyncParams {
                            idempotency_key: ritornel_client::idempotency_key()?,
                            full,
                        })
                        .await?
                        .job;
                    while wait && !job.state.ended() {
                        tokio::time::sleep(LOOK_EVERY).await;
                        job = client
                            .call::<JobsGet>(&JobGetParams { job_id: job.job_id })
                            .await?
                            .job;
                        let total = job
                            .total
                            .map_or_else(|| "?".to_owned(), |total| total.to_string());
                        let line = format!(
                            "{}: {} of {total} read  {}\n",
                            job.state.as_str(),
                            job.done,
                            job.label.as_deref().unwrap_or("")
                        );
                        if !quiet && line != last {
                            let _ = out.write_all(line.as_bytes()).and_then(|()| out.flush());
                            last = line;
                        }
                    }
                    Ok(job)
                },
            )?;
            server::print(
                &args.server,
                out,
                &job,
                serde_json::to_string_pretty,
                render_job,
            )?;
            match job.state {
                JobState::Failed | JobState::Cancelled => Err(Error::Job(job.state.as_str())),
                _ => Ok(()),
            }
        }
        LastfmCommand::Report(args) => {
            let limit = args.limit;
            let report = server::ask(&args.server, env, async |client| {
                client
                    .call::<LastfmReportCall>(&LastfmReportParams { limit })
                    .await
            })?;
            server::print(
                &args.server,
                out,
                &report,
                serde_json::to_string_pretty,
                render_report,
            )
        }
        LastfmCommand::Scrobbling(args) => {
            let on = match args.state.as_str() {
                "on" => true,
                "off" => false,
                _ => return Err(Error::Usage("scrobbling is `on` or `off`")),
            };
            let set = server::ask(&args.server, env, async |client| {
                client
                    .call::<LastfmSetScrobbling>(&ScrobblingParams {
                        idempotency_key: ritornel_client::idempotency_key()?,
                        on,
                    })
                    .await
            })?;
            server::print(
                &args.server,
                out,
                &set,
                serde_json::to_string_pretty,
                |set: &ScrobblingSet| match set.scrobbling_since {
                    Some(since) => format!(
                        "Scrobbling to Last.fm: plays from {} on that count are sent.\n",
                        utc(since)
                    ),
                    None => "Scrobbling to Last.fm is off.\n".to_owned(),
                },
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_instant_is_written_as_a_utc_date_and_time() {
        assert_eq!(utc(0), "1970-01-01 00:00 UTC");
        assert_eq!(utc(1_794_776_400_000), "2026-11-15 21:00 UTC");
        assert_eq!(utc(951_782_400_000), "2000-02-29 00:00 UTC");
    }

    #[test]
    fn a_report_gives_the_share_matched_and_lists_the_doubtful_with_what_they_were_near() {
        let report = LastfmReport {
            account: "someone".to_owned(),
            imported: 10,
            plays: 9,
            matched_plays: 6,
            echoes: 1,
            tracks: 4,
            matched_tracks: 2,
            doubtful_tracks: 1,
            doubtful_plays: 2,
            absent_tracks: 1,
            absent_plays: 1,
            doubtful: vec![ritornel_client::contract::DoubtfulTrack {
                artist: "Moss Arcade".to_owned(),
                album: None,
                title: "Salt Road (Live)".to_owned(),
                plays: 2,
                doubt: "near".to_owned(),
                candidates: vec![ritornel_client::contract::NearlyMatched {
                    title: Some("Salt Road".to_owned()),
                    artist: Some("Moss Arcade".to_owned()),
                }],
            }],
            loved: 3,
            loved_matched: 2,
            first: None,
            last: None,
        };
        let text = render_report(&report);
        assert!(text.contains("Plays matched     6 of 9 (66.7%)"), "{text}");
        assert!(
            text.contains("     2  Moss Arcade — Salt Road (Live)  (near)\n          near: Moss Arcade — Salt Road\n"),
            "{text}"
        );
    }
}
