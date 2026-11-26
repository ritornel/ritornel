//! Inspect this server's transcode settings and verify cached outputs.
use crate::cli::{Error, ServerArgs};
use crate::server;
use clap::Subcommand;
use ritornel_client::contract::methods::{TranscodePrune, TranscodeStart};
use ritornel_client::contract::transcode::ArtifactLayer;
use ritornel_client::contract::{Id, JobState, TranscodeParams, TranscodePruneParams};
use ritornel_client::local;
use std::io::Write;

#[derive(Debug, PartialEq, Eq, Subcommand)]
pub(crate) enum TranscodeCommand {
    Encoders(ServerArgs),
    Stats(ServerArgs),
    Prune(ServerArgs),
    Run {
        #[arg(long)]
        profile: String,
        #[arg(long)]
        derivative: bool,
        #[arg(required = true)]
        appearances: Vec<Id>,
        #[command(flatten)]
        server: ServerArgs,
    },
}

pub(crate) fn run(
    command: TranscodeCommand,
    env: local::Env<'_>,
    out: &mut dyn Write,
) -> Result<(), Error> {
    let (args, request) = match command {
        TranscodeCommand::Stats(ref args) | TranscodeCommand::Encoders(ref args) => {
            let status = server::ask(args, env, async |client| client.status().await)?
                .transcode
                .ok_or(Error::Nothing("transcode status unavailable"))?;
            if matches!(command, TranscodeCommand::Encoders(_)) {
                return server::print(
                    args,
                    out,
                    &status.encoders,
                    serde_json::to_string_pretty,
                    |list| {
                        if list.is_empty() {
                            return "No external encoders available.\n".into();
                        }
                        list.iter()
                            .map(|e| {
                                format!("{} {}  {}\n", e.family, e.version, e.formats.join(", "))
                            })
                            .collect()
                    },
                );
            }
            return server::print(args, out, &status, serde_json::to_string_pretty, |s| {
                format!(
                    "{} of {} used, {} derivatives, {} packages\n{} workers\nProfiles: {}\n",
                    crate::status::size(s.used_bytes),
                    crate::status::size(s.budget_bytes.value),
                    s.derivatives,
                    s.packages,
                    s.workers.value,
                    s.profiles
                        .value
                        .keys()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            });
        }
        TranscodeCommand::Prune(args) => (args, None),
        TranscodeCommand::Run {
            server,
            profile,
            derivative,
            appearances,
        } => (
            server,
            Some(TranscodeParams {
                idempotency_key: ritornel_client::idempotency_key()?,
                profile,
                appearances,
                layer: if derivative {
                    ArtifactLayer::Derivative
                } else {
                    ArtifactLayer::Packaged
                },
            }),
        ),
    };
    let detail = server::ask_for(&args, env, None, async |client| {
        let started = match request {
            Some(params) => client.call::<TranscodeStart>(&params).await?,
            None => {
                client
                    .call::<TranscodePrune>(&TranscodePruneParams {
                        idempotency_key: ritornel_client::idempotency_key()?,
                    })
                    .await?
            }
        };
        loop {
            let job = client.job(started.job.job_id).await?;
            if job.job.state.ended() {
                return Ok(job);
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
    })?;
    server::print(&args, out, &detail, serde_json::to_string_pretty, |d| {
        let mut text = format!("{} {}\n", d.job.kind, d.job.state.as_str());
        if let Some(e) = &d.job.error {
            text.push_str(e);
            text.push('\n')
        }
        for a in &d.artifacts {
            text.push_str(&format!(
                "{}  {}  {} bytes\n",
                a.appearance_id, a.key, a.size_bytes
            ));
        }
        text
    })?;
    if detail.job.state != JobState::Succeeded {
        return Err(Error::Job(detail.job.state.as_str()));
    }
    Ok(())
}
