//! `ritornelctl history`: recent listening, "on this day", and the rebuild
//! of the derived tables. See "Queries M1 must answer" and "Rebuild" in
//! docs/design/history.md.
//!
//! It reads history through two direct calls, `history.recent` and
//! `history.onThisDay`, until list views arrive with `list.open` and
//! `kind: "history"`.

use std::io::Write;

use ritornel_client::contract::methods::{
    HistoryAlbum, HistoryArtist, HistoryOnThisDay, HistoryRebuild, HistoryRecent, HistorySetZones,
    HistoryZones, ListClose, ListOpen,
};
use ritornel_client::contract::{
    AlbumGetParams, AlbumRef, ArtistGetParams, ErrorCode, HistoryOf, HistoryRebuildParams,
    HistoryRebuilt, HistoryRecentParams, Id, ListKind, ListOpenParams, NoParams, OnThisDay,
    OnThisDayParams, Play, PlayOutcome, Query, RecentPlays, Row, SetZonesParams, ViewParams,
    Window, ZonePeriod, ZonePeriods, ZonesSet,
};
use ritornel_client::{Client, local};

use crate::cli::{Error, HistoryArgs, HistoryCommand, OfArgs, ServerArgs, ZoneCommand};
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
    // A play another service reported says only that it was played.
    let heard = match play.ms_played {
        Some(ms) => format!("{:>5} of {:<5}", clock(ms), length),
        None => format!("{:<14}", "scrobbled"),
    };
    let mut text = format!(
        "{}  {:<8} {:<8} {heard}  {what}\n",
        when(play),
        outcome,
        counts,
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

fn render_zones(zones: &ZonePeriods) -> String {
    if zones.periods.is_empty() {
        return "No zones: a play whose source gave no zone has no local date.\n".to_owned();
    }
    zones
        .periods
        .iter()
        .map(|period| {
            format!(
                "{:<12} {:<12} {}\n",
                period.from.as_deref().unwrap_or("always"),
                period.until.as_deref().unwrap_or("on"),
                period.zone
            )
        })
        .collect()
}

/// What a page says of your history with an artist or an album: its
/// sentence as the page shows it, then what the sentence rests on, then
/// every year of the history with its plays.
fn render_of(of: &HistoryOf) -> String {
    if of.plays == 0 {
        return if of.span.is_none() {
            "No plays yet.\n".to_owned()
        } else {
            "No plays matched to it.\n".to_owned()
        };
    }
    let mut text: String = of
        .sentence
        .iter()
        .map(|phrase| phrase.text.as_str())
        .collect();
    text.push_str("\n\n");
    let mut lines = vec![("Plays", of.plays.to_string())];
    if let Some(first) = &of.first_played {
        lines.push(("First played", first.clone()));
    }
    if let Some(last) = &of.last_played {
        lines.push(("Last played", last.clone()));
    }
    lines.push(("In the last year", of.last_year.to_string()));
    text.push_str(&server::facts(&lines));
    if let Some(span) = of.span {
        let most = of
            .years
            .iter()
            .map(|year| year.plays)
            .max()
            .unwrap_or(1)
            .max(1);
        text.push('\n');
        for year in span.from..=span.to {
            let plays = of
                .years
                .iter()
                .find(|played| played.year == year)
                .map_or(0, |played| played.plays);
            // A bar of up to 40, as long as the plays: a measure, where the
            // page's picture is eased to show the quiet years.
            let bar = "█".repeat(usize::try_from(plays * 40 / most).unwrap_or(0));
            let bar = if plays > 0 && bar.is_empty() {
                "▏"
            } else {
                &bar
            };
            text.push_str(&format!("{year}  {plays:>6}  {bar}\n"));
        }
    }
    if !of.records.is_empty() {
        text.push_str("\nPlayed most\n");
        for record in &of.records {
            text.push_str(&format!(
                "  {:<40} {:>6}  most in {}\n",
                record.tile.title.as_deref().unwrap_or("(untitled)"),
                record.plays,
                record.peak_year
            ));
        }
    }
    text
}

/// How many search results to fetch at a time.
const LOOKUP: u64 = 50;

/// What a name or an ID finds: the one it names, or the rows it may mean,
/// one a line, each with its ID.
enum Named {
    One(Id),
    Choose(Vec<(Id, String)>),
}

/// Finds an artist or an album by its ID, or by its name: the one whose
/// name it is, ignoring case, or the only one whose text holds it.
async fn named(
    client: &Client,
    kind: ListKind,
    name: &str,
) -> Result<Named, ritornel_client::Error> {
    if let Ok(id) = name.parse::<Id>() {
        return Ok(Named::One(id));
    }
    let opened = client
        .call::<ListOpen>(&ListOpenParams {
            kind,
            query: Query {
                text: Some(name.to_owned()),
                ..Query::default()
            },
            window: Some(Window {
                offset: 0,
                limit: LOOKUP,
            }),
            replaces: None,
        })
        .await?;
    let found = async {
        let mut rows = opened.rows;
        let mut found = Vec::new();
        let mut offset = 0;
        loop {
            offset += rows.len() as u64;
            found.extend(rows.into_iter().filter_map(candidate));
            if offset >= opened.total {
                break;
            }
            let window = client.list_window(&opened.view, offset, LOOKUP).await?;
            if window.offset != offset || window.rows.is_empty() {
                return Err(ritornel_client::Error::Protocol(
                    "incomplete name search results".into(),
                ));
            }
            rows = window.rows;
        }
        Ok(found)
    }
    .await;
    let _ = client
        .call::<ListClose>(&ViewParams { view: opened.view })
        .await;
    let found = found?;
    let wanted = name.to_lowercase();
    let exact: Vec<&(Id, String, String)> = found
        .iter()
        .filter(|(_, named, _)| named.to_lowercase() == wanted)
        .collect();
    Ok(match (exact.as_slice(), found.as_slice()) {
        ([(id, ..)], _) | ([], [(id, ..)]) => Named::One(*id),
        _ => Named::Choose(found.into_iter().map(|(id, _, line)| (id, line)).collect()),
    })
}

fn candidate(row: Row) -> Option<(Id, String, String)> {
    match row {
        Row::Artist(artist) => {
            let name = artist.name.unwrap_or_default();
            Some((artist.artist_id, name.clone(), name))
        }
        Row::Album(tile) => {
            let title = tile.title.unwrap_or_default();
            let by = tile
                .artist_credit
                .map(|credit| format!(" — {}", credit.text))
                .unwrap_or_default();
            Some((tile.album.id(), title.clone(), format!("{title}{by}")))
        }
        _ => None,
    }
}

/// What a page says of an artist or an album, named by ID or by name.
fn history_of(
    args: &OfArgs,
    env: local::Env<'_>,
    out: &mut dyn Write,
    album: bool,
) -> Result<(), Error> {
    let kind = if album {
        ListKind::Albums
    } else {
        ListKind::Artists
    };
    let answer = server::ask(&args.server, env, async |client| {
        let id = match named(client, kind, &args.name).await? {
            Named::One(id) => id,
            Named::Choose(choices) => return Ok(Err(choices)),
        };
        if !album {
            return client
                .call::<HistoryArtist>(&ArtistGetParams { artist_id: id })
                .await
                .map(Ok);
        }
        // An album's ID is its record's or one release's: the record's first.
        let record = client
            .call::<HistoryAlbum>(&AlbumGetParams {
                album: AlbumRef::ReleaseGroup { id },
            })
            .await;
        match record {
            Err(ritornel_client::Error::Call(error)) if error.data.code == ErrorCode::NotFound => {
                client
                    .call::<HistoryAlbum>(&AlbumGetParams {
                        album: AlbumRef::Release { id },
                    })
                    .await
                    .map(Ok)
            }
            answered => answered.map(Ok),
        }
    })?;
    match answer {
        Ok(of) => server::print(
            &args.server,
            out,
            &of,
            serde_json::to_string_pretty,
            render_of,
        ),
        Err(choices) => {
            server::print(
                &args.server,
                out,
                &choices,
                |choices| {
                    let rows: Vec<_> = choices
                        .iter()
                        .map(|(id, label)| serde_json::json!({"id":id, "label":label}))
                        .collect();
                    serde_json::to_string_pretty(&serde_json::json!({"choices":rows}))
                },
                |choices| {
                    choices
                        .iter()
                        .map(|(id, label)| format!("{id}  {label}\n"))
                        .collect()
                },
            )?;
            Err(Error::Nothing(if album {
                "that names no one album: name it by one of these IDs"
            } else {
                "that names no one artist: name them by one of these IDs"
            }))
        }
    }
}

/// Makes `periods` the zones, and prints them as they now are.
fn set_zones(
    args: &ServerArgs,
    env: local::Env<'_>,
    out: &mut dyn Write,
    change: impl FnOnce(Vec<ZonePeriod>) -> Vec<ZonePeriod>,
) -> Result<(), Error> {
    let (set, zones) = server::ask(args, env, async |client| {
        let now = client.call::<HistoryZones>(&NoParams {}).await?;
        let set = client
            .call::<HistorySetZones>(&SetZonesParams {
                idempotency_key: ritornel_client::idempotency_key()?,
                periods: change(now.periods),
            })
            .await?;
        Ok((set, client.call::<HistoryZones>(&NoParams {}).await?))
    })?;
    server::print(
        args,
        out,
        &(set, zones),
        |(set, _)| serde_json::to_string_pretty(set),
        |(set, zones): &(ZonesSet, ZonePeriods)| {
            format!(
                "{}{} plays dated again.\n",
                render_zones(zones),
                set.changed
            )
        },
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
        Some(HistoryCommand::Artist(of)) => history_of(of, env, out, false),
        Some(HistoryCommand::Album(of)) => history_of(of, env, out, true),
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
        Some(HistoryCommand::Zones(zones)) => {
            let found = server::ask(zones, env, async |client| {
                client.call::<HistoryZones>(&NoParams {}).await
            })?;
            server::print(
                zones,
                out,
                &found,
                serde_json::to_string_pretty,
                render_zones,
            )
        }
        Some(HistoryCommand::Zone(ZoneCommand::Add(add))) => {
            let period = ZonePeriod {
                zone: add.zone.clone(),
                from: add.from.clone(),
                until: add.until.clone(),
            };
            set_zones(&add.server, env, out, |mut periods| {
                periods.push(period);
                periods
            })
        }
        Some(HistoryCommand::Zone(ZoneCommand::Clear(clear))) => {
            set_zones(clear, env, out, |_| Vec::new())
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    #[tokio::test]
    async fn an_exact_artist_on_the_second_page_wins() {
        let (outgoing, mut requests) = tokio::sync::mpsc::channel::<String>(2);
        let (replies, incoming) = tokio::sync::mpsc::channel(2);
        let client = Client::over_memory(outgoing, incoming);
        let wanted = Id::from_bytes([42; 16]);
        let artist = |name: &str, id: Id| json!({"type":"artist","artistId":id,"name":name,"albumCount":0,"trackCount":0,"portrait":false});
        let serve = async {
            while let Some(text) = requests.recv().await {
                let call: Value = serde_json::from_str(&text).unwrap();
                let result = match call["method"].as_str().unwrap() {
                    "list.open" => {
                        json!({"view":"artists","total":51,"libraryRevision":1,"offset":0,"rows":vec![artist("A Partial Match", Id::from_bytes([1;16]));50]})
                    }
                    "list.window" => {
                        assert_eq!(call["params"]["offset"], 50);
                        json!({"view":"artists","offset":50,"rows":[artist("Match", wanted)]})
                    }
                    "list.close" => json!({"closed":true}),
                    method => panic!("unexpected method {method}"),
                };
                replies
                    .send(json!({"jsonrpc":"2.0","id":call["id"],"result":result}).to_string())
                    .await
                    .unwrap();
                if call["method"] == "list.close" {
                    break;
                }
            }
        };
        let (found, ()) = tokio::join!(named(&client, ListKind::Artists, "Match"), serve);
        assert!(matches!(found.unwrap(), Named::One(id) if id == wanted));
    }
}
