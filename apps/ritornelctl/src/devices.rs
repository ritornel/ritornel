//! `ritornelctl devices`: the players and folders a library syncs to. The
//! same calls the desktop's Devices tab makes (docs/internals/device-sync.md).

use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use clap::{Args, Subcommand};
use ritornel_client::Client;
use ritornel_client::contract::methods::{
    DevicesActions, DevicesAddFolder, DevicesEject, DevicesForget, DevicesFormats, DevicesGet,
    DevicesList, DevicesPlays, DevicesSet, DevicesSync,
};
use ritornel_client::contract::transcode::{Mp3, Target, Vbr};
use ritornel_client::contract::{
    Id, NoParams, SyncActionKind, SyncActionsParams, SyncAddFolderParams, SyncDevice,
    SyncDeviceCommandParams, SyncDeviceDetail, SyncDeviceParams, SyncFolderKind, SyncFormat,
    SyncGroupBy, SyncLossless, SyncMadeFor, SyncOutcome, SyncPageParams, SyncSelector,
    SyncSetParams, SyncSetup, SyncSourceRole, SyncSourceSpec, SyncStartParams, SyncUnit,
};
use ritornel_client::local;

use crate::cli::{Error, ServerArgs};
use crate::server;
use crate::status::size;

/// `ritornelctl devices`.
#[derive(Debug, PartialEq, Eq, Subcommand)]
pub(crate) enum DevicesCommand {
    /// Every device.
    List(ServerArgs),
    /// A device's page.
    Show(OneArgs),
    /// The rows of its next sync.
    Plan {
        device: String,
        /// One row for each track, where the default is one for each album.
        #[arg(long)]
        tracks: bool,
        /// Only rows of this kind: add, replace, remove, conflict or blocked.
        #[arg(long)]
        kind: Option<String>,
        #[command(flatten)]
        server: ServerArgs,
    },
    /// What its music would take in each format on offer.
    Formats(OneArgs),
    /// The plays read from it.
    Plays(OneArgs),
    AddFolder {
        folder: PathBuf,
        #[arg(long)]
        name: Option<String>,
        /// The most it may hold, such as `8GB`, for one that is always here.
        #[arg(long)]
        limit: Option<String>,
        /// What it is made for: car, phone, player or copy.
        #[arg(long = "for")]
        made_for: Option<String>,
        /// A drive or a card that comes and goes, and is ejected.
        #[arg(long)]
        removable: bool,
        #[command(flatten)]
        server: ServerArgs,
    },
    Sync {
        device: String,
        /// Sets a new device up first.
        #[arg(long)]
        setup: bool,
        /// Agrees to what does not fit being left off.
        #[arg(long)]
        accept_cut: bool,
        /// Leaves files whose only change is tags or cover for another day.
        #[arg(long)]
        skip_retags: bool,
        #[arg(long)]
        no_wait: bool,
        #[command(flatten)]
        server: ServerArgs,
    },
    Stop(OneArgs),
    Set {
        device: String,
        /// A rule, in order: `everything`, `artist:<id>`, `album:<id>` or `added:<days>`.
        #[arg(long)]
        source: Vec<String>,
        /// What always goes on, claimed before every rule.
        #[arg(long)]
        always: Vec<String>,
        /// What never goes on, whatever claims it.
        #[arg(long)]
        leave_out: Vec<String>,
        /// as-is, mp3-v0, mp3-v2, mp3-320 or opus-160.
        #[arg(long)]
        format: Option<String>,
        /// The embedded cover's longest side in pixels: 0 embeds none.
        #[arg(long)]
        cover: Option<u32>,
        /// on or off.
        #[arg(long)]
        id3v1: Option<String>,
        /// The most a folder may hold, such as `8GB`: 0 takes the limit away.
        #[arg(long)]
        limit: Option<String>,
        /// What a folder is made for: car, phone, player or copy.
        #[arg(long = "for")]
        made_for: Option<String>,
        /// The codecs it plays, such as `flac,mp3`.
        #[arg(long)]
        plays: Option<String>,
        #[command(flatten)]
        server: ServerArgs,
    },
    Rename {
        device: String,
        name: String,
        #[command(flatten)]
        server: ServerArgs,
    },
    Ignore {
        device: String,
        /// Stops ignoring it.
        #[arg(long)]
        off: bool,
        #[command(flatten)]
        server: ServerArgs,
    },
    Forget(OneArgs),
    Eject(OneArgs),
}

#[derive(Debug, PartialEq, Eq, Args)]
pub(crate) struct OneArgs {
    /// The device, by its ID or its name.
    device: String,
    #[command(flatten)]
    server: ServerArgs,
}

/// A size as a person writes one: `8GB`, `500 MB`, `1024`.
fn bytes(text: &str) -> Result<u64, Error> {
    let text = text.trim();
    let digits = text.trim_end_matches(|c: char| c.is_ascii_alphabetic() || c == ' ');
    let unit = text.get(digits.len()..).unwrap_or_default().trim();
    let scale: u64 = match unit.to_ascii_uppercase().as_str() {
        "" | "B" => 1,
        "KB" => 1_000,
        "MB" => 1_000_000,
        "GB" => 1_000_000_000,
        "TB" => 1_000_000_000_000,
        "KIB" => 1 << 10,
        "MIB" => 1 << 20,
        "GIB" => 1 << 30,
        "TIB" => 1 << 40,
        _ => return Err(Error::Usage("a size is a number and a unit, such as 8GB")),
    };
    let number: f64 = digits
        .trim()
        .parse()
        .map_err(|_| Error::Usage("a size is a number and a unit, such as 8GB"))?;
    if !number.is_finite() || number < 0.0 {
        return Err(Error::Usage("a size is a number and a unit, such as 8GB"));
    }
    Ok((number * scale as f64) as u64)
}

fn format(name: &str) -> Result<SyncFormat, Error> {
    let encode = |target| SyncFormat {
        lossless: SyncLossless::Encode { target },
    };
    Ok(match name {
        "as-is" => SyncFormat {
            lossless: SyncLossless::AsIs,
        },
        "mp3-v0" => encode(Target::Mp3 { mode: Mp3::V0 }),
        "mp3-v2" => encode(Target::Mp3 { mode: Mp3::V2 }),
        "mp3-320" => encode(Target::Mp3 {
            mode: Mp3::Cbr(320),
        }),
        "opus-160" => encode(Target::Opus {
            bitrate_kbps: 160,
            complexity: 10,
            vbr: Vbr::Variable,
        }),
        _ => {
            return Err(Error::Usage(
                "a format is as-is, mp3-v0, mp3-v2, mp3-320 or opus-160",
            ));
        }
    })
}

fn made_for(name: &str) -> Result<SyncMadeFor, Error> {
    Ok(match name {
        "car" => SyncMadeFor::CarStereo,
        "phone" => SyncMadeFor::Phone,
        "player" => SyncMadeFor::OtherPlayer,
        "copy" => SyncMadeFor::PlainCopy,
        _ => return Err(Error::Usage("--for is car, phone, player or copy")),
    })
}

fn made_for_words(made_for: SyncMadeFor) -> &'static str {
    match made_for {
        SyncMadeFor::CarStereo => "a car stereo",
        SyncMadeFor::Phone => "a phone",
        SyncMadeFor::OtherPlayer => "another player",
        SyncMadeFor::PlainCopy => "a plain copy",
    }
}

/// A format in words.
pub(crate) fn format_words(format: SyncFormat) -> String {
    match format.lossless {
        SyncLossless::AsIs => "FLAC as it is".to_owned(),
        SyncLossless::Encode {
            target: Target::Mp3 { mode },
        } => match mode {
            Mp3::V0 => "MP3 V0".to_owned(),
            Mp3::V2 => "MP3 V2".to_owned(),
            Mp3::Cbr(kbps) => format!("MP3 {kbps}"),
        },
        SyncLossless::Encode {
            target: Target::Opus { bitrate_kbps, .. },
        } => format!("Opus {bitrate_kbps}"),
        SyncLossless::Encode {
            target: Target::Flac {},
        } => "FLAC".to_owned(),
    }
}

fn selector(text: &str) -> Result<SyncSelector, Error> {
    const HOW: &str = "a source is everything, artist:<id>, album:<id> or added:<days>";
    let (kind, value) = text.split_once(':').unwrap_or((text, ""));
    Ok(match kind {
        "everything" => SyncSelector::Everything,
        "artist" => SyncSelector::Artist {
            artist_id: value.parse().map_err(|_| Error::Usage(HOW))?,
        },
        "album" => SyncSelector::Album {
            album_id: value.parse().map_err(|_| Error::Usage(HOW))?,
        },
        "added" => SyncSelector::AddedWithin {
            days: value.parse().map_err(|_| Error::Usage(HOW))?,
        },
        _ => return Err(Error::Usage(HOW)),
    })
}

fn selector_words(selector: SyncSelector, label: &str) -> String {
    match selector {
        SyncSelector::Everything => "Everything".to_owned(),
        SyncSelector::Artist { .. } => format!("Artist: {label}"),
        SyncSelector::Album { .. } => format!("Album: {label}"),
        SyncSelector::AddedWithin { days } => format!("Added in the last {days} days"),
    }
}

/// The device a person named, by its ID or, ignoring case, its name.
async fn find(client: &Client, named: &str) -> Result<SyncDevice, ritornel_client::Error> {
    let devices = client.call::<DevicesList>(&NoParams {}).await?.devices;
    let id: Option<Id> = named.parse().ok();
    devices
        .into_iter()
        .find(|device| Some(device.device_id) == id || device.name.eq_ignore_ascii_case(named))
        .ok_or_else(|| ritornel_client::Error::Protocol(format!("there is no device {named:?}")))
}

fn state(device: &SyncDevice) -> String {
    let here = match (device.connection.attached, device.setup) {
        (_, SyncSetup::Ignored) => "ignored",
        (true, SyncSetup::New) => "not set up",
        (true, SyncSetup::Ready) => "connected",
        (false, _) => "not connected",
    };
    let mut state = here.to_owned();
    if device.activity == ritornel_client::contract::SyncActivity::Syncing {
        state = "syncing".to_owned();
    }
    if let Some(bytes) = device.to_copy_bytes.filter(|bytes| *bytes > 0) {
        state.push_str(&format!(", {} to sync", size(bytes)));
    }
    if !device.attention.reasons.is_empty() {
        state.push_str(&format!(" ({})", device.attention.reasons.join(", ")));
    }
    state
}

fn duration(ms: u64) -> String {
    match ms / 1000 {
        seconds if seconds < 60 => format!("{} s", seconds.max(1)),
        seconds if seconds < 3600 => format!("{} min", seconds.div_ceil(60)),
        seconds => format!("{} h {} min", seconds / 3600, (seconds % 3600) / 60),
    }
}

fn outcome_words(outcome: SyncOutcome) -> &'static str {
    match outcome {
        SyncOutcome::Complete => "complete",
        SyncOutcome::CutOff => "cut off",
        SyncOutcome::FailedInPart => "finished with some files failed",
        SyncOutcome::Failed => "failed",
        SyncOutcome::Stopped => "stopped",
    }
}

fn page(detail: &SyncDeviceDetail) -> String {
    let device = &detail.device;
    let mut text = format!("{}  {}\n{}\n", device.name, device.device_id, state(device));
    if let Some(path) = &detail.path {
        text.push_str(&format!("Folder: {path}\n"));
    }
    if let Some(folder) = &detail.folder {
        text.push_str(&format!(
            "Made for {}, {}, plays {}\n",
            made_for_words(folder.made_for),
            match folder.kind {
                SyncFolderKind::Removable => "removable",
                SyncFolderKind::AlwaysHere => "always here",
            },
            detail.capabilities.codecs.join(", "),
        ));
    }
    text.push_str(&format!(
        "Format: {}, covers {}\n",
        format_words(detail.settings.format),
        match detail.settings.cover_px {
            0 => "none".to_owned(),
            px => format!("{px} px"),
        }
    ));
    if let Some(room) = &detail.room {
        text.push_str(&format!(
            "Room: {} of {} used by Ritornel, {} left alone, {} free ({} reserved)\n",
            size(room.managed_bytes),
            size(room.capacity_bytes),
            size(room.unmanaged_bytes),
            size(room.free_bytes),
            size(room.reserve_bytes),
        ));
    }
    text.push_str("What goes on:\n");
    for source in &detail.sources {
        let role = match source.spec.role {
            SyncSourceRole::Rule => "",
            SyncSourceRole::Always => "always ",
            SyncSourceRole::LeaveOut => "leave out ",
        };
        text.push_str(&format!(
            "  {role}{}: {} albums, {} tracks, {}",
            selector_words(source.spec.selector, &source.label),
            source.albums,
            source.claimed.tracks,
            size(source.claimed.bytes),
        ));
        if source.cut.tracks > 0 {
            text.push_str(&format!(" ({} tracks don't fit)", source.cut.tracks));
        }
        text.push('\n');
    }
    if detail.unmanaged.files > 0 {
        text.push_str(&format!(
            "Already on it: {} files Ritornel didn't write, {}, left alone\n",
            detail.unmanaged.files,
            size(detail.unmanaged.bytes)
        ));
    }
    if let Some(plan) = &detail.plan {
        let counts = &plan.counts;
        text.push_str(&format!(
            "Next sync: {} to copy, {} free after, about {}{}\n  add {}, replace {}, remove {}, conflicts {}, blocked {}\n  plan {}\n",
            size(plan.figures.to_copy_bytes),
            size(plan.figures.free_after_bytes),
            duration(plan.figures.estimated_ms),
            if plan.fits { "" } else { " (not everything fits)" },
            counts.add.tracks,
            counts.replace.tracks,
            counts.remove.tracks,
            counts.conflict.tracks,
            counts.blocked.tracks,
            plan.plan_revision,
        ));
    }
    if let Some(run) = &detail.run {
        if run.running {
            text.push_str(&format!(
                "Syncing: {} of {} files, {} of {}\n",
                run.actions_done,
                run.actions_total,
                size(run.bytes_done),
                size(run.bytes_total)
            ));
        } else if let Some(outcome) = run.outcome {
            text.push_str(&format!(
                "Last sync: {}, {} of {} files, {} failed\n",
                outcome_words(outcome),
                run.actions_done.saturating_sub(run.failed),
                run.actions_total,
                run.failed
            ));
            if let Some(error) = &run.error {
                text.push_str(&format!("  {error}\n"));
            }
        }
    }
    text
}

fn key() -> Result<Id, ritornel_client::Error> {
    ritornel_client::idempotency_key()
}

async fn set(
    client: &Client,
    device: &str,
    change: impl FnOnce(&mut SyncSetParams),
) -> Result<SyncDeviceDetail, ritornel_client::Error> {
    let found = find(client, device).await?;
    let mut params = SyncSetParams {
        idempotency_key: key()?,
        device_id: found.device_id,
        name: None,
        icon_key: None,
        ignored: None,
        sources: None,
        format: None,
        cover_px: None,
        id3v1: None,
        reserve_bytes: None,
        limit_bytes: None,
        when_full: None,
        on_connect: None,
        made_for: None,
        folder_kind: None,
        codecs: None,
        playlists: None,
    };
    change(&mut params);
    client.call::<DevicesSet>(&params).await?;
    client
        .call::<DevicesGet>(&SyncDeviceParams {
            device_id: found.device_id,
        })
        .await
}

pub(crate) fn run(
    command: DevicesCommand,
    env: local::Env<'_>,
    out: &mut dyn Write,
) -> Result<(), Error> {
    match command {
        DevicesCommand::List(at) => {
            let list = server::ask(&at, env, async |client| {
                client.call::<DevicesList>(&NoParams {}).await
            })?;
            server::print(&at, out, &list, serde_json::to_string_pretty, |list| {
                if list.devices.is_empty() {
                    return "No devices. Add a folder to sync to with `devices add-folder`.\n"
                        .into();
                }
                list.devices
                    .iter()
                    .map(|device| {
                        format!("{}  {}  {}\n", device.device_id, device.name, state(device))
                    })
                    .collect()
            })
        }
        DevicesCommand::Show(one) => {
            let detail = server::ask(&one.server, env, async |client| {
                let device = find(client, &one.device).await?;
                client
                    .call::<DevicesGet>(&SyncDeviceParams {
                        device_id: device.device_id,
                    })
                    .await
            })?;
            server::print(
                &one.server,
                out,
                &detail,
                serde_json::to_string_pretty,
                page,
            )
        }
        DevicesCommand::Plan {
            device,
            tracks,
            kind,
            server: at,
        } => {
            let kind = match kind.as_deref() {
                None => None,
                Some("add") => Some(SyncActionKind::Add),
                Some("replace") => Some(SyncActionKind::Replace),
                Some("remove") => Some(SyncActionKind::Remove),
                Some("conflict") => Some(SyncActionKind::Conflict),
                Some("blocked") => Some(SyncActionKind::Blocked),
                Some(_) => {
                    return Err(Error::Usage(
                        "--kind is add, replace, remove, conflict or blocked",
                    ));
                }
            };
            let rows = server::ask(&at, env, async |client| {
                let found = find(client, &device).await?;
                client
                    .call::<DevicesActions>(&SyncActionsParams {
                        device_id: found.device_id,
                        plan_revision: None,
                        run_id: None,
                        kind,
                        state: None,
                        group_by: if tracks {
                            SyncGroupBy::Track
                        } else {
                            SyncGroupBy::Album
                        },
                        parent_id: None,
                        offset: 0,
                        limit: 500,
                    })
                    .await
            })?;
            server::print(&at, out, &rows, serde_json::to_string_pretty, |rows| {
                let mut text = String::new();
                for row in &rows.rows {
                    let what = match row.tracks {
                        Some(tracks) => {
                            format!("{tracks} of {} tracks", row.album_tracks.unwrap_or(tracks))
                        }
                        None => row.path.clone().unwrap_or_default(),
                    };
                    text.push_str(&format!(
                        "{:8} {} — {}  {}  {}  {}{}\n",
                        format!("{:?}", row.kind).to_lowercase(),
                        row.artist,
                        row.title,
                        what,
                        row.codec.as_deref().unwrap_or("-"),
                        size(row.bytes),
                        if row.reason == "wanted" {
                            String::new()
                        } else {
                            format!("  ({})", row.reason)
                        },
                    ));
                }
                if rows.total as usize > rows.rows.len() {
                    text.push_str(&format!("… {} rows in all\n", rows.total));
                }
                if text.is_empty() {
                    text.push_str("Nothing to do.\n");
                }
                text
            })
        }
        DevicesCommand::Formats(one) => {
            let formats = server::ask(&one.server, env, async |client| {
                let device = find(client, &one.device).await?;
                client
                    .call::<DevicesFormats>(&SyncDeviceParams {
                        device_id: device.device_id,
                    })
                    .await
            })?;
            server::print(
                &one.server,
                out,
                &formats,
                serde_json::to_string_pretty,
                |formats| {
                    formats
                        .formats
                        .iter()
                        .map(|option| {
                            format!(
                                "{} {:14} {:>9}  {}{}\n",
                                if option.current { "*" } else { " " },
                                format_words(option.format),
                                size(option.estimated_bytes),
                                if option.fits { "fits" } else { "does not fit" },
                                if option.available {
                                    ""
                                } else {
                                    "  (no encoder on this server)"
                                },
                            )
                        })
                        .collect()
                },
            )
        }
        DevicesCommand::Plays(one) => {
            let plays = server::ask(&one.server, env, async |client| {
                let device = find(client, &one.device).await?;
                client
                    .call::<DevicesPlays>(&SyncPageParams {
                        device_id: device.device_id,
                        offset: 0,
                        limit: 500,
                    })
                    .await
            })?;
            server::print(
                &one.server,
                out,
                &plays,
                serde_json::to_string_pretty,
                |plays| {
                    if plays.plays.is_empty() {
                        return "No plays have been read from it.\n".into();
                    }
                    plays
                        .plays
                        .iter()
                        .map(|play| format!("{}  {}\n", play.title, play.delivery))
                        .collect()
                },
            )
        }
        DevicesCommand::AddFolder {
            folder,
            name,
            limit,
            made_for: purpose,
            removable,
            server: at,
        } => {
            let purpose = purpose.as_deref().map(made_for).transpose()?;
            let path = std::path::absolute(&folder).map_err(Error::Folder)?;
            let name = name.unwrap_or_else(|| {
                path.file_name().map_or_else(
                    || "Folder".to_owned(),
                    |name| name.to_string_lossy().into_owned(),
                )
            });
            let limit_bytes = limit.as_deref().map(bytes).transpose()?;
            let detail = server::ask(&at, env, async |client| {
                let added = client
                    .call::<DevicesAddFolder>(&SyncAddFolderParams {
                        idempotency_key: key()?,
                        path: path.to_string_lossy().into_owned(),
                        name,
                        limit_bytes,
                        made_for: purpose,
                        kind: removable.then_some(SyncFolderKind::Removable),
                    })
                    .await?;
                client
                    .call::<DevicesGet>(&SyncDeviceParams {
                        device_id: added.device_id,
                    })
                    .await
            })?;
            server::print(&at, out, &detail, serde_json::to_string_pretty, page)
        }
        DevicesCommand::Sync {
            device,
            setup,
            accept_cut,
            skip_retags,
            no_wait,
            server: at,
        } => {
            let detail = server::ask_for(
                &at,
                env,
                no_wait.then_some(server::ANSWER_WAIT),
                async |client| {
                    let found = find(client, &device).await?;
                    let params = SyncDeviceParams {
                        device_id: found.device_id,
                    };
                    let before = client.call::<DevicesGet>(&params).await?;
                    let plan = before.plan.ok_or_else(|| {
                        ritornel_client::Error::Protocol(
                            "the device has no plan: is it connected?".to_owned(),
                        )
                    })?;
                    let started = client
                        .call::<DevicesSync>(&SyncStartParams {
                            idempotency_key: key()?,
                            device_id: found.device_id,
                            plan_revision: plan.plan_revision,
                            adopt: setup,
                            accept_cut,
                            skip_retags,
                        })
                        .await?;
                    loop {
                        let job = client.job(started.job.job_id).await?;
                        if no_wait || job.job.state.ended() {
                            return client.call::<DevicesGet>(&params).await;
                        }
                        tokio::time::sleep(Duration::from_millis(200)).await;
                    }
                },
            )?;
            server::print(&at, out, &detail, serde_json::to_string_pretty, page)?;
            match detail.run.as_ref().and_then(|run| run.outcome) {
                Some(SyncOutcome::Complete) | None => Ok(()),
                Some(outcome) => Err(Error::Job(outcome_words(outcome))),
            }
        }
        DevicesCommand::Stop(one) => {
            let detail = server::ask(&one.server, env, async |client| {
                let found = find(client, &one.device).await?;
                let params = SyncDeviceParams {
                    device_id: found.device_id,
                };
                let detail = client.call::<DevicesGet>(&params).await?;
                if let Some(job) = detail
                    .run
                    .as_ref()
                    .filter(|run| run.running)
                    .and_then(|run| run.job_id)
                {
                    client.cancel_job(key()?, job).await?;
                }
                client.call::<DevicesGet>(&params).await
            })?;
            server::print(
                &one.server,
                out,
                &detail,
                serde_json::to_string_pretty,
                page,
            )
        }
        DevicesCommand::Set {
            device,
            source,
            always,
            leave_out,
            format: named,
            cover,
            id3v1,
            limit,
            made_for: purpose,
            plays,
            server: at,
        } => {
            let purpose = purpose.as_deref().map(made_for).transpose()?;
            let plays: Option<Vec<String>> = plays.map(|plays| {
                plays
                    .split(',')
                    .map(|codec| codec.trim().to_owned())
                    .collect()
            });
            let mut sources = Vec::new();
            for (role, given) in [
                (SyncSourceRole::Rule, &source),
                (SyncSourceRole::Always, &always),
                (SyncSourceRole::LeaveOut, &leave_out),
            ] {
                for text in given {
                    sources.push(SyncSourceSpec {
                        role,
                        selector: selector(text)?,
                        unit: SyncUnit::Albums,
                    });
                }
            }
            let named = named.as_deref().map(format).transpose()?;
            let id3v1 = match id3v1.as_deref() {
                None => None,
                Some("on") => Some(true),
                Some("off") => Some(false),
                Some(_) => return Err(Error::Usage("--id3v1 is on or off")),
            };
            let limit_bytes = limit.as_deref().map(bytes).transpose()?;
            let detail = server::ask(&at, env, async |client| {
                set(client, &device, |params| {
                    params.sources = (!sources.is_empty()).then_some(sources);
                    params.format = named;
                    params.cover_px = cover;
                    params.id3v1 = id3v1;
                    params.limit_bytes = limit_bytes;
                    params.made_for = purpose;
                    params.codecs = plays;
                })
                .await
            })?;
            server::print(&at, out, &detail, serde_json::to_string_pretty, page)
        }
        DevicesCommand::Rename {
            device,
            name,
            server: at,
        } => {
            let detail = server::ask(&at, env, async |client| {
                set(client, &device, |params| params.name = Some(name)).await
            })?;
            server::print(&at, out, &detail, serde_json::to_string_pretty, page)
        }
        DevicesCommand::Ignore {
            device,
            off,
            server: at,
        } => {
            let detail = server::ask(&at, env, async |client| {
                set(client, &device, |params| params.ignored = Some(!off)).await
            })?;
            server::print(&at, out, &detail, serde_json::to_string_pretty, page)
        }
        DevicesCommand::Forget(one) => {
            let forgotten = server::ask(&one.server, env, async |client| {
                let found = find(client, &one.device).await?;
                client
                    .call::<DevicesForget>(&SyncDeviceCommandParams {
                        idempotency_key: key()?,
                        device_id: found.device_id,
                    })
                    .await?;
                Ok(found)
            })?;
            server::print(
                &one.server,
                out,
                &forgotten,
                serde_json::to_string_pretty,
                |device| {
                    format!(
                        "Forgot {}. The files on it are left as they are, no longer Ritornel's.\n",
                        device.name
                    )
                },
            )
        }
        DevicesCommand::Eject(one) => {
            let ejected = server::ask(&one.server, env, async |client| {
                let found = find(client, &one.device).await?;
                client
                    .call::<DevicesEject>(&SyncDeviceCommandParams {
                        idempotency_key: key()?,
                        device_id: found.device_id,
                    })
                    .await?;
                Ok(found)
            })?;
            server::print(
                &one.server,
                out,
                &ejected,
                serde_json::to_string_pretty,
                |device| format!("{} is safe to unplug.\n", device.name),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_size_is_read_with_or_without_a_unit() {
        assert_eq!(bytes("1024").unwrap(), 1024);
        assert_eq!(bytes("8GB").unwrap(), 8_000_000_000);
        assert_eq!(bytes("1.5 GiB").unwrap(), 3 << 29);
        assert_eq!(bytes("0").unwrap(), 0);
        assert!(bytes("lots").is_err());
        assert!(bytes("5 parsecs").is_err());
    }

    #[test]
    fn a_source_and_a_format_are_read_from_their_words() {
        assert_eq!(selector("everything").unwrap(), SyncSelector::Everything);
        assert_eq!(
            selector("added:30").unwrap(),
            SyncSelector::AddedWithin { days: 30 }
        );
        assert!(selector("artist:nobody").is_err());
        assert!(selector("genre:jazz").is_err());
        assert_eq!(format_words(format("mp3-320").unwrap()), "MP3 320");
        assert_eq!(format_words(format("as-is").unwrap()), "FLAC as it is");
        assert!(format("wma").is_err());
    }
}
