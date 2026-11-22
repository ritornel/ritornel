//! `ritornelctl link`: inviting a satellite on a hub, joining a hub from
//! another server, and what each knows of the other. See
//! "Hub and satellites" in docs/internals/linking.md.

use std::io::Write;

use ritornel_client::contract::methods::{LinkInvite as Invite, LinkJoin, LinkStatusCall};
use ritornel_client::contract::{
    LinkInvite, LinkInviteParams, LinkJoinParams, LinkStatus, NoParams,
};
use ritornel_client::local;

use crate::cli::{Error, LinkCommand};
use crate::server;

pub(crate) fn link(
    command: &LinkCommand,
    env: local::Env<'_>,
    out: &mut dyn Write,
) -> Result<(), Error> {
    match command {
        LinkCommand::Invite(args) => {
            let params = LinkInviteParams {
                routes: args.route.clone(),
            };
            let invite = server::ask(&args.server, env, async |client| {
                client.call::<Invite>(&params).await
            })?;
            server::print(
                &args.server,
                out,
                &invite,
                serde_json::to_string_pretty,
                render_invite,
            )
        }
        LinkCommand::Join(args) => {
            let link = args.link.clone();
            // The hub may be a moment away on the network.
            let status = server::ask_for(&args.server, env, Some(JOIN_WAIT), async |client| {
                client
                    .call::<LinkJoin>(&LinkJoinParams {
                        idempotency_key: ritornel_client::idempotency_key()?,
                        link,
                    })
                    .await
            })?;
            server::print(
                &args.server,
                out,
                &status,
                serde_json::to_string_pretty,
                render_status,
            )
        }
        LinkCommand::Status(args) => {
            let status = server::ask(args, env, async |client| {
                client.call::<LinkStatusCall>(&NoParams {}).await
            })?;
            server::print(
                args,
                out,
                &status,
                serde_json::to_string_pretty,
                render_status,
            )
        }
    }
}

/// How long a join may take: the hub is tried at each of its routes.
const JOIN_WAIT: std::time::Duration = std::time::Duration::from_secs(60);

fn render_invite(invite: &LinkInvite) -> String {
    format!(
        "{}\n\nOn the other server, within ten minutes and once:\n  ritornelctl link join '<the link above>'\n",
        invite.link
    )
}

fn render_status(status: &LinkStatus) -> String {
    let mut text = String::new();
    if let Some(hub) = &status.hub {
        text += &server::facts(&[
            ("Role", "satellite".to_owned()),
            ("Hub", format!("{} ({})", hub.name, hub.server_id)),
            ("Library", hub.library_id.to_string()),
            ("Routes", hub.routes.join(", ")),
        ]);
    } else {
        text += &server::facts(&[("Role", status.role.as_str().to_owned())]);
        for satellite in &status.satellites {
            text += &server::facts(&[(
                "Satellite",
                format!("{} ({})", satellite.name, satellite.server_id),
            )]);
        }
    }
    text
}
