//! `ritornelctl link`: inviting a satellite on a hub, joining a hub from
//! another server, and what each knows of the other. See
//! "Hub and satellites" in docs/internals/linking.md.

use std::io::Write;

use ritornel_client::contract::methods::{
    LinkInvite as Invite, LinkJoin, LinkSetAwake, LinkStatusCall,
};
use ritornel_client::contract::{
    Awake, AwakeParams, AwakeSet, LinkInvite, LinkInviteParams, LinkJoinParams, LinkStatus,
    NoParams,
};
use ritornel_client::local;

use crate::cli::{AwakeArgs, Error, LinkCommand};
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
        LinkCommand::Awake(args) => {
            let awake = awake(args)?;
            let set = server::ask(&args.server, env, async |client| {
                client
                    .call::<LinkSetAwake>(&AwakeParams {
                        idempotency_key: ritornel_client::idempotency_key()?,
                        awake,
                    })
                    .await
            })?;
            server::print(
                &args.server,
                out,
                &set,
                serde_json::to_string_pretty,
                |set: &AwakeSet| render_status(&set.link),
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

/// The wake setting `link awake` was given.
fn awake(args: &AwakeArgs) -> Result<Awake, Error> {
    match (args.mode.as_str(), args.minutes.as_deref()) {
        ("while-active", None) => Ok(Awake::default()),
        ("while-active", Some(minutes)) => minutes
            .parse()
            .map(|minutes| Awake::WhileActive { minutes })
            .map_err(|_| Error::Usage("while-active takes a whole number of minutes")),
        ("always", None) => Ok(Awake::Always),
        ("never", None) => Ok(Awake::Never),
        _ => Err(Error::Usage(
            "awake is `while-active [<minutes>]`, `always` or `never`",
        )),
    }
}

/// The wake setting, as a person reads it, and whether it holds now.
fn render_awake(status: &LinkStatus) -> String {
    let setting = match status.awake {
        Awake::WhileActive { minutes } => {
            format!("while a linked server plays or called in the last {minutes} minutes")
        }
        Awake::Always => "always".to_owned(),
        Awake::Never => "never".to_owned(),
    };
    let now = if status.kept_awake {
        "kept awake now"
    } else {
        "free to sleep now"
    };
    format!("{setting}; {now}")
}

fn render_status(status: &LinkStatus) -> String {
    let mut text = String::new();
    if let Some(hub) = &status.hub {
        text += &server::facts(&[
            ("Role", "satellite".to_owned()),
            ("Hub", format!("{} ({})", hub.name, hub.server_id)),
            ("Library", hub.library_id.to_string()),
            ("Routes", hub.routes.join(", ")),
            ("Awake", render_awake(status)),
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
    if status.hub.is_none() {
        text += &server::facts(&[("Awake", render_awake(status))]);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(mode: &str, minutes: Option<&str>) -> AwakeArgs {
        AwakeArgs {
            mode: mode.to_owned(),
            minutes: minutes.map(str::to_owned),
            ..AwakeArgs::default()
        }
    }

    #[test]
    fn awake_is_while_active_always_or_never() {
        assert_eq!(
            awake(&args("while-active", None)).unwrap(),
            Awake::default()
        );
        assert_eq!(
            awake(&args("while-active", Some("45"))).unwrap(),
            Awake::WhileActive { minutes: 45 }
        );
        assert_eq!(awake(&args("always", None)).unwrap(), Awake::Always);
        assert_eq!(awake(&args("never", None)).unwrap(), Awake::Never);
        for wrong in [
            args("while-active", Some("soon")),
            args("always", Some("5")),
            args("sometimes", None),
        ] {
            assert!(matches!(awake(&wrong), Err(Error::Usage(_))), "{wrong:?}");
        }
    }
}
