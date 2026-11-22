//! `ritornelctl play`, the transport, `now` and `queue`: the server's player
//! from the command line. See "Commands, playback, and jobs" in
//! docs/internals/contract.md. The player is the server's: it plays on when
//! this program ends.

use std::io::Write;
use std::path::Path;
use std::time::Duration;

use ritornel_client::Client;
use ritornel_client::contract::methods::{
    PlaybackGet, PlaybackPause, PlaybackPlay, PlaybackResume, PlaybackSeek, PlaybackSet,
    PlaybackSkip, PlaybackStop, QueueWindow as QueueWindowMethod, SourcesTracks,
};
use ritornel_client::contract::{
    ContextKind, Id, NoParams, PlayContext, PlaySource, PlaybackCommandParams, PlaybackPlayParams,
    PlaybackSeekParams, PlaybackSetParams, PlaybackSkipParams, PlaybackState, PlaybackStatus,
    QueueWindow, QueueWindowParams, Repeat, SkipDirection, SourcesTracksParams, TrackSummary,
};
use ritornel_client::local;

use crate::cli::{Error, PlayArgs, SeekArgs, ServerArgs, VolumeArgs};
use crate::server;

/// How often `play --follow` looks at the player.
const LOOK_EVERY: Duration = Duration::from_millis(500);

/// A time as minutes and seconds.
pub(crate) fn clock(ms: u64) -> String {
    let seconds = ms / 1000;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

/// A track as one line: title, artist, album.
pub(crate) fn line(track: &TrackSummary) -> String {
    let title = track.title.as_deref().unwrap_or("(untitled)");
    let mut text = match &track.artist {
        Some(artist) => format!("{title} — {artist}"),
        None => title.to_owned(),
    };
    if let Some(album) = &track.album {
        text.push_str(&format!("  ({album})"));
    }
    text
}

/// The player's state, as a person reads it.
fn render(state: &PlaybackState) -> String {
    let status = match state.status {
        PlaybackStatus::Stopped => "Stopped",
        PlaybackStatus::Playing => "Playing",
        PlaybackStatus::Paused => "Paused",
        PlaybackStatus::Buffering => "Buffering",
    };
    let mut lines = vec![(
        "Status",
        match &state.track {
            Some(track) => format!("{status}: {}", line(track)),
            None => status.to_owned(),
        },
    )];
    if let Some(track) = &state.track
        && let (Some(disc), Some(number)) = (track.disc_number, track.track_number)
    {
        lines.push(("Track", format!("disc {disc}, track {number}")));
    }
    if state.entry_id.is_some() {
        let length = state.duration_ms.map_or_else(|| "?".to_owned(), clock);
        lines.push((
            "Position",
            format!("{} of {length}", clock(state.position_ms)),
        ));
    }
    if let Some(copy) = &state.copy {
        let bits = copy
            .bit_depth
            .map_or_else(String::new, |bits| format!(", {bits}-bit"));
        lines.push((
            "Copy",
            format!(
                "{} {} Hz{bits}, {} channels",
                copy.codec, copy.sample_rate_hz, copy.channels
            ),
        ));
    }
    let output = &state.output;
    lines.push((
        "Output",
        match &output.problem {
            Some(problem) => format!(
                "{} isn’t working, since {}: {}",
                output.name,
                crate::lastfm::utc(problem.since),
                problem.detail
            ),
            None if output.sample_rate_hz == 0 => format!("{} (not opened yet)", output.name),
            None => format!(
                "{}, shared, {} Hz, {} channels",
                output.name, output.sample_rate_hz, output.channels
            ),
        },
    ));
    if output.problem.is_some() {
        lines.push((
            "",
            if state.status == PlaybackStatus::Paused {
                "Playback is paused where it was. Check that the sound device is connected and \
                 on, then `ritornelctl resume`."
            } else {
                "Check that the sound device is connected and on, then play again."
            }
            .to_owned(),
        ));
    }
    lines.push((
        "Volume",
        format!(
            "{}%{}",
            (state.volume * 100.0).round(),
            if state.muted { ", muted" } else { "" }
        ),
    ));
    lines.push((
        "Queue",
        match state.index {
            Some(index) => format!("{} of {}", index + 1, state.queue_length),
            None => format!("{} entries", state.queue_length),
        },
    ));
    let repeat = match state.repeat {
        Repeat::Off => "repeat off",
        Repeat::All => "repeat all",
        Repeat::One => "repeat one",
    };
    let shuffle = if state.shuffle {
        "shuffled"
    } else {
        "in order"
    };
    lines.push(("Order", format!("{shuffle}, {repeat}")));
    server::facts(&lines)
}

/// The queue, one entry a line, the current one marked.
fn render_queue(window: &QueueWindow) -> String {
    if window.entries.is_empty() {
        return "The queue is empty.\n".to_owned();
    }
    window
        .entries
        .iter()
        .enumerate()
        .map(|(at, entry)| {
            let index = u32::try_from(at).unwrap_or(u32::MAX);
            let mark = if window.current_index == Some(index) {
                ">"
            } else {
                " "
            };
            let length = entry.track.duration_ms.map_or_else(String::new, clock);
            let failed = entry
                .failed_reason
                .as_deref()
                .map_or_else(String::new, |reason| format!("  [{reason}]"));
            format!(
                "{mark} {:>3}  {:>6}  {}{failed}\n",
                at + 1,
                length,
                line(&entry.track)
            )
        })
        .collect()
}

async fn state(client: &Client) -> Result<PlaybackState, ritornel_client::Error> {
    client.call::<PlaybackGet>(&NoParams {}).await
}

fn key() -> Result<Id, ritornel_client::Error> {
    ritornel_client::idempotency_key()
}

/// What `play` was given: IDs of tracks, or a folder of a root.
enum Target {
    Tracks(Vec<Id>),
    Release(Id),
    Folder(String),
}

fn target(args: &PlayArgs) -> Result<Target, Error> {
    if let Some(release) = &args.album {
        if !args.what.is_empty() {
            return Err(Error::Usage("play takes --album or what to play, not both"));
        }
        return release
            .parse()
            .map(Target::Release)
            .map_err(|_| Error::Usage("that is not a release's ID"));
    }
    let ids: Result<Vec<Id>, _> = args.what.iter().map(|text| text.parse::<Id>()).collect();
    match (ids, args.what.as_slice()) {
        (_, []) => Err(Error::Usage("play needs a folder, track IDs or --album")),
        (Ok(ids), _) => Ok(Target::Tracks(ids)),
        (Err(_), [folder]) => {
            // The server is given the folder's real path, as `scan` gives
            // it.
            let real = std::fs::canonicalize(Path::new(folder)).map_err(Error::PlayFolder)?;
            real.to_str()
                .map(|path| Target::Folder(path.to_owned()))
                .ok_or(Error::Usage("play needs a folder whose path is UTF-8"))
        }
        (Err(_), _) => Err(Error::Usage("play takes one folder, or track IDs")),
    }
}

pub(crate) fn play(args: &PlayArgs, env: local::Env<'_>, out: &mut dyn Write) -> Result<(), Error> {
    let target = target(args)?;
    let start = args.start.map(|start| start - 1);
    let (follow, quiet) = (args.follow, args.server.json);
    let limit = (!follow).then_some(server::ANSWER_WAIT);
    let state = server::ask_for(&args.server, env, limit, async |client| {
        let (source, context) = match target {
            Target::Tracks(ids) => (PlaySource::Tracks { ids }, None),
            Target::Release(id) => (PlaySource::Release { id }, None),
            Target::Folder(path) => {
                let found = client
                    .call::<SourcesTracks>(&SourcesTracksParams { path })
                    .await?;
                if found.tracks.is_empty() {
                    return Ok(None);
                }
                // A folder that is one album is played as that album.
                let releases: std::collections::BTreeSet<Option<Id>> =
                    found.tracks.iter().map(|track| track.release_id).collect();
                let context = match releases.into_iter().collect::<Vec<_>>().as_slice() {
                    [Some(release)] => Some(PlayContext {
                        kind: ContextKind::Album,
                        reference: Some(*release),
                    }),
                    _ => None,
                };
                let ids = found
                    .tracks
                    .iter()
                    .map(|track| track.appearance_id)
                    .collect();
                (PlaySource::Tracks { ids }, context)
            }
        };
        client
            .call::<PlaybackPlay>(&PlaybackPlayParams {
                idempotency_key: key()?,
                source,
                start,
                context,
                shuffle: None,
            })
            .await?;
        let mut now = state(client).await?;
        if !follow {
            return Ok(Some(now));
        }
        let mut seen: Option<Id> = None;
        loop {
            if now.entry_id != seen {
                seen = now.entry_id;
                if !quiet && let Some(track) = &now.track {
                    let place = now.index.map_or_else(String::new, |index| {
                        format!("{:>3}/{}  ", index + 1, now.queue_length)
                    });
                    // Nothing useful can be done if the terminal is gone.
                    let _ = writeln!(out, "{place}{}", line(track)).and_then(|()| out.flush());
                }
            }
            if now.status == PlaybackStatus::Stopped {
                return Ok(Some(now));
            }
            tokio::time::sleep(LOOK_EVERY).await;
            now = state(client).await?;
        }
    })?
    .ok_or(Error::Nothing(
        "nothing there is a track the library knows: scan the folder first",
    ))?;
    server::print(
        &args.server,
        out,
        &state,
        serde_json::to_string_pretty,
        render,
    )
}

/// The transport commands that name nothing: pause, resume and stop.
pub(crate) fn transport(
    which: &'static str,
    args: &ServerArgs,
    env: local::Env<'_>,
    out: &mut dyn Write,
) -> Result<(), Error> {
    let state = server::ask(args, env, async |client| {
        let params = PlaybackCommandParams {
            idempotency_key: key()?,
        };
        match which {
            "pause" => client.call::<PlaybackPause>(&params).await?,
            "resume" => client.call::<PlaybackResume>(&params).await?,
            _ => client.call::<PlaybackStop>(&params).await?,
        };
        state(client).await
    })?;
    server::print(args, out, &state, serde_json::to_string_pretty, render)
}

/// `next` and `previous`: a skip from the entry the player is on.
pub(crate) fn skip(
    direction: SkipDirection,
    args: &ServerArgs,
    env: local::Env<'_>,
    out: &mut dyn Write,
) -> Result<(), Error> {
    let state = server::ask(args, env, async |client| {
        let now = state(client).await?;
        if let Some(from_entry_id) = now.entry_id {
            client
                .call::<PlaybackSkip>(&PlaybackSkipParams {
                    idempotency_key: key()?,
                    from_entry_id,
                    direction,
                })
                .await?;
        }
        state(client).await
    })?;
    server::print(args, out, &state, serde_json::to_string_pretty, render)
}

/// A position: seconds, or minutes and seconds as `m:ss`.
fn position(text: &str) -> Option<u64> {
    let seconds = match text.split_once(':') {
        Some((minutes, seconds)) => {
            let (minutes, seconds): (u64, u64) = (minutes.parse().ok()?, seconds.parse().ok()?);
            if seconds >= 60 {
                return None;
            }
            minutes.checked_mul(60)?.checked_add(seconds)?
        }
        None => text.parse::<u64>().ok()?,
    };
    seconds.checked_mul(1000)
}

pub(crate) fn seek(args: &SeekArgs, env: local::Env<'_>, out: &mut dyn Write) -> Result<(), Error> {
    let position_ms =
        position(&args.position).ok_or(Error::Usage("a position is seconds, or m:ss"))?;
    let state = server::ask(&args.server, env, async |client| {
        let now = state(client).await?;
        if let Some(entry_id) = now.entry_id {
            client
                .call::<PlaybackSeek>(&PlaybackSeekParams {
                    idempotency_key: key()?,
                    entry_id,
                    position_ms,
                })
                .await?;
        }
        state(client).await
    })?;
    server::print(
        &args.server,
        out,
        &state,
        serde_json::to_string_pretty,
        render,
    )
}

/// What `volume` asks for: a level as a fraction, or muted or not.
fn volume_change(level: &str) -> Option<(Option<f64>, Option<bool>)> {
    match level {
        "mute" => Some((None, Some(true))),
        "unmute" => Some((None, Some(false))),
        _ => {
            let percent: u32 = level.trim_end_matches('%').parse().ok()?;
            (percent <= 100).then_some((Some(f64::from(percent) / 100.0), None))
        }
    }
}

pub(crate) fn volume(
    args: &VolumeArgs,
    env: local::Env<'_>,
    out: &mut dyn Write,
) -> Result<(), Error> {
    let change = match &args.level {
        Some(level) => {
            Some(volume_change(level).ok_or(Error::Usage("a volume is 0 to 100, mute or unmute"))?)
        }
        None => None,
    };
    let state = server::ask(&args.server, env, async |client| {
        if let Some((volume, muted)) = change {
            client
                .call::<PlaybackSet>(&PlaybackSetParams {
                    idempotency_key: key()?,
                    volume,
                    muted,
                    shuffle: None,
                    repeat: None,
                })
                .await?;
        }
        state(client).await
    })?;
    server::print(
        &args.server,
        out,
        &state,
        serde_json::to_string_pretty,
        render,
    )
}

pub(crate) fn now(
    args: &ServerArgs,
    env: local::Env<'_>,
    out: &mut dyn Write,
) -> Result<(), Error> {
    let state = server::ask(args, env, async |client| state(client).await)?;
    server::print(args, out, &state, serde_json::to_string_pretty, render)
}

pub(crate) fn queue(
    args: &ServerArgs,
    env: local::Env<'_>,
    out: &mut dyn Write,
) -> Result<(), Error> {
    let window = server::ask(args, env, async |client| {
        whole_queue(async |offset| {
            client
                .call::<QueueWindowMethod>(&QueueWindowParams {
                    offset,
                    limit: Some(500),
                    order: None,
                })
                .await
        })
        .await
    })?;
    server::print(
        args,
        out,
        &window,
        serde_json::to_string_pretty,
        render_queue,
    )
}

/// The whole queue, read a window at a time from `offset`. A change while
/// it is read starts it again, so that every entry is from one revision.
async fn whole_queue<E>(
    window: impl AsyncFn(u32) -> Result<QueueWindow, E>,
) -> Result<QueueWindow, E> {
    'again: loop {
        let mut whole = window(0).await?;
        while (whole.entries.len() as u64) < whole.total {
            let offset = u32::try_from(whole.entries.len()).unwrap_or(u32::MAX);
            let next = window(offset).await?;
            if next.queue_revision != whole.queue_revision {
                continue 'again;
            }
            if next.entries.is_empty() {
                break;
            }
            whole.entries.extend(next.entries);
        }
        return Ok(whole);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A queue of `total` invented entries at `revision`, read from
    /// `offset` at most 500 at a time.
    fn window(total: u32, revision: u64, offset: u32) -> QueueWindow {
        let entries = (offset..total.min(offset.saturating_add(500)))
            .map(|n| {
                serde_json::from_value(serde_json::json!({
                    "entryId": Id::from_bytes([0; 16]).to_string(),
                    "track": {
                        "appearanceId": Id::from_bytes([0; 16]).to_string(),
                        "title": format!("Song {n}"),
                    },
                }))
                .unwrap()
            })
            .collect();
        QueueWindow {
            queue_revision: revision,
            total: u64::from(total),
            current_index: Some(total - 1),
            entries,
        }
    }

    fn read(windows: impl AsyncFn(u32) -> Result<QueueWindow, ()>) -> Result<QueueWindow, ()> {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(whole_queue(windows))
    }

    #[test]
    fn the_queue_is_read_whole_and_marks_the_last_entry_current() {
        let whole = read(async |offset| Ok(window(501, 7, offset))).unwrap();
        assert_eq!(whole.entries.len(), 501);
        let shown = render_queue(&whole);
        assert_eq!(shown.lines().count(), 501);
        assert!(
            shown.lines().last().unwrap().starts_with("> 501"),
            "{shown}"
        );
    }

    #[test]
    fn a_queue_that_changes_while_it_is_read_is_read_again() {
        let reads = std::cell::Cell::new(0);
        let whole = read(async |offset| {
            reads.set(reads.get() + 1);
            // The second read finds the queue grown by one.
            let (total, revision) = if reads.get() < 2 { (501, 7) } else { (502, 8) };
            Ok(window(total, revision, offset))
        })
        .unwrap();
        assert_eq!((whole.queue_revision, whole.entries.len()), (8, 502));
        assert_eq!(reads.get(), 4);
    }

    #[test]
    fn an_output_that_is_not_working_is_said_with_since_when_and_what_to_do() {
        let mut state: PlaybackState = serde_json::from_value(serde_json::json!({
            "revision": 3,
            "status": "paused",
            "positionMs": 61_000,
            "volume": 1.0,
            "muted": false,
            "shuffle": false,
            "repeat": "off",
            "output": {
                "name": "default",
                "mode": "shared",
                "sampleRateHz": 0,
                "channels": 0,
                "problem": { "since": 1_794_776_400_000_i64, "detail": "no device is there" },
            },
            "queueRevision": 1,
            "queueLength": 2,
        }))
        .unwrap();
        let shown = render(&state);
        assert!(
            shown.contains("default isn’t working, since 2026-11-15 21:00 UTC: no device is there"),
            "{shown}"
        );
        assert!(shown.contains("then `ritornelctl resume`"), "{shown}");
        // Stopped since, it does not promise a place to resume from.
        state.status = PlaybackStatus::Stopped;
        let shown = render(&state);
        assert!(!shown.contains("resume"), "{shown}");
        assert!(shown.contains("then play again"), "{shown}");
        state.output.problem = None;
        assert!(render(&state).contains("default (not opened yet)"));
    }

    #[test]
    fn a_volume_is_a_percentage_or_mute_or_unmute() {
        assert_eq!(volume_change("40"), Some((Some(0.4), None)));
        assert_eq!(volume_change("100%"), Some((Some(1.0), None)));
        assert_eq!(volume_change("mute"), Some((None, Some(true))));
        assert_eq!(volume_change("unmute"), Some((None, Some(false))));
        assert_eq!(volume_change("101"), None);
        assert_eq!(volume_change("loud"), None);
    }

    #[test]
    fn a_position_is_seconds_or_minutes_and_seconds() {
        assert_eq!(position("90"), Some(90_000));
        assert_eq!(position("1:30"), Some(90_000));
        assert_eq!(position("0:05"), Some(5_000));
        assert_eq!(position("1:75"), None);
        assert_eq!(position("soon"), None);
        assert_eq!(position(&u64::MAX.to_string()), None);
        assert_eq!(position(&format!("{}:30", u64::MAX)), None);
        let last = u64::MAX / 1000;
        assert_eq!(position(&last.to_string()), Some(last * 1000));
        assert_eq!(clock(272_500), "4:32");
    }
}
