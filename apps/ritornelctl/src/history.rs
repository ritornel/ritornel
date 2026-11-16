//! `ritornelctl history`: recent listening, "on this day", and the rebuild
//! of the derived tables. See "Queries M1 must answer" and "Rebuild" in
//! docs/design/history.md.
//!
//! It reads history through two direct calls, `history.recent` and
//! `history.onThisDay`, until list views arrive with `list.open` and
//! `kind: "history"`.

use std::io::Write;

use ritornel_client::contract::methods::{HistoryOnThisDay, HistoryRebuild, HistoryRecent};
use ritornel_client::contract::{
    HistoryRebuildParams, HistoryRebuilt, HistoryRecentParams, OnThisDay, OnThisDayParams, Play,
    PlayOutcome, RecentPlays,
};
use ritornel_client::local;

use crate::cli::{Error, HistoryArgs, HistoryCommand};
use crate::playback::clock;
use crate::server;

/// When a play was, on the wall clock where it was heard.
fn when(play: &Play) -> String {
    let (Some(date), Some(at)) = (&play.local_date, play.at) else {
        return match play.at {
            Some(_) => "(no local date)   ".to_owned(),
            None => "(undated)         ".to_owned(),
        };
    };
    let offset_ms = i64::from(play.utc_offset_minutes.unwrap_or(0)) * 60_000;
    let minute_of_day = (at + offset_ms).rem_euclid(86_400_000) / 60_000;
    format!("{date} {:02}:{:02}", minute_of_day / 60, minute_of_day % 60)
}

/// One play as a line, and a line for each listen behind it when there is
/// more than one.
fn render_play(play: &Play) -> String {
    let title = play.track.title.as_deref().unwrap_or("(untitled)");
    let what = match &play.track.artist {
        Some(artist) => format!("{title} — {artist}"),
        None => title.to_owned(),
    };
    let outcome = match play.outcome {
        PlayOutcome::Listened => "listened",
        PlayOutcome::Skipped => "skipped",
        PlayOutcome::Partial => "partial",
        PlayOutcome::Unknown => "unknown",
    };
    let length = play.duration_ms.map_or_else(|| "?".to_owned(), clock);
    let counts = if play.excluded {
        "excluded"
    } else if play.counts {
        "counts"
    } else {
        "-"
    };
    let mut text = format!(
        "{}  {:<8} {:<8} {:>5} of {:<5}  {what}\n",
        when(play),
        outcome,
        counts,
        clock(play.ms_played),
        length
    );
    if play.listens.len() > 1 {
        for listen in &play.listens {
            text.push_str(&format!(
                "    {} from {} to {}, {}, ended by {}\n",
                listen.start_reason,
                clock(listen.start_position_ms),
                clock(listen.end_position_ms),
                clock(listen.ms_played),
                listen.end_reason
            ));
        }
    }
    text
}

fn render_recent(recent: &RecentPlays) -> String {
    let mut text = if recent.plays.is_empty() {
        "No plays yet.\n".to_owned()
    } else {
        recent.plays.iter().map(render_play).collect()
    };
    if recent.undelivered > 0 {
        text.push_str(&format!(
            "{} finished listens are still on their way to history.\n",
            recent.undelivered
        ));
    }
    text
}

fn render_day(day: &OnThisDay) -> String {
    if day.plays.is_empty() {
        return format!("Nothing on {} in any year yet.\n", day.day);
    }
    let mut text = String::new();
    let mut year = None;
    for play in &day.plays {
        let this = play.local_date.as_deref().and_then(|date| date.get(..4));
        if this != year {
            year = this;
            text.push_str(&format!("{}\n", this.unwrap_or("?")));
        }
        text.push_str("  ");
        text.push_str(&render_play(play));
    }
    text
}

fn render_rebuilt(rebuilt: &HistoryRebuilt) -> String {
    let lead = if rebuilt.dry_run {
        "A rebuild would change"
    } else {
        "Rebuilt: changed"
    };
    format!(
        "{lead} {} plays: {} before, {} after.\n",
        rebuilt.changed, rebuilt.before, rebuilt.after
    )
}

pub(crate) fn history(
    args: &HistoryArgs,
    env: local::Env<'_>,
    out: &mut dyn Write,
) -> Result<(), Error> {
    match &args.command {
        None => {
            let params = HistoryRecentParams {
                limit: args.limit,
                before: None,
            };
            let recent = server::ask(&args.server, env, async |client| {
                client.call::<HistoryRecent>(&params).await
            })?;
            server::print(
                &args.server,
                out,
                &recent,
                serde_json::to_string_pretty,
                render_recent,
            )
        }
        Some(HistoryCommand::OnThisDay(day)) => {
            let params = OnThisDayParams {
                day: day.day.clone(),
            };
            let found = server::ask(&day.server, env, async |client| {
                client.call::<HistoryOnThisDay>(&params).await
            })?;
            server::print(
                &day.server,
                out,
                &found,
                serde_json::to_string_pretty,
                render_day,
            )
        }
        Some(HistoryCommand::Rebuild(rebuild)) => {
            let dry_run = rebuild.dry_run;
            let rebuilt = server::ask(&rebuild.server, env, async |client| {
                client
                    .call::<HistoryRebuild>(&HistoryRebuildParams {
                        idempotency_key: ritornel_client::idempotency_key()?,
                        dry_run,
                    })
                    .await
            })?;
            server::print(
                &rebuild.server,
                out,
                &rebuilt,
                serde_json::to_string_pretty,
                render_rebuilt,
            )
        }
    }
}
