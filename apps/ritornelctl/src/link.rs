//! `ritornelctl link`: inviting a satellite on a hub, joining a hub from
//! another server, and what each knows of the other. See
//! "Hub and satellites" in docs/internals/linking.md.

use std::io::Write;

use ritornel_client::contract::methods::{
    LinkInvite as Invite, LinkJoin, LinkSetAwake, LinkStatusCall,
};
use ritornel_client::contract::{
    Awake, AwakeParams, AwakeSet, HubStatus, LinkInvite, LinkInviteParams, LinkJoinParams,
    LinkStatus, NoParams,
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

/// Whether the hub answers, as the satellite last found, and how it last
/// reached it: the route as the link names it and the address it resolved
/// to. Nothing before the satellite has tried.
fn reach(hub: &HubStatus) -> Vec<(&'static str, String)> {
    let mut facts = Vec::new();
    if let Some(route) = &hub.route {
        let label = if hub.answers == Some(true) {
            "Connected via"
        } else {
            "Last reached via"
        };
        facts.push((label, route.clone()));
    }
    match (hub.answers, &hub.problem) {
        (Some(true), _) => facts.push(("Hub answers", "yes".to_owned())),
        (Some(false), Some(problem)) => facts.push(("Hub answers", format!("no: {problem}"))),
        (Some(false), None) => facts.push(("Hub answers", "no".to_owned())),
        (None, _) => {}
    }
    facts
}

fn render_status(status: &LinkStatus) -> String {
    let mut text = String::new();
    if let Some(hub) = &status.hub {
        let mut facts = vec![
            ("Role", "satellite".to_owned()),
            ("Hub", format!("{} ({})", hub.name, hub.server_id)),
            ("Library", hub.library_id.to_string()),
            ("Routes", hub.routes.join(", ")),
        ];
        facts.extend(reach(hub));
        facts.push(("Awake", render_awake(status)));
        text += &server::facts(&facts);
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

    fn hub(route: Option<&str>, answers: Option<bool>) -> HubStatus {
        HubStatus {
            server_id: ritornel_client::contract::Id::from_bytes([1; 16]),
            name: "Studio Mac".to_owned(),
            library_id: ritornel_client::contract::Id::from_bytes([2; 16]),
            epoch: 1,
            routes: [
                "studio-mac.local:4747",
                "10.0.0.2:4747",
                "studio.example-overlay.net:4747",
            ]
            .map(str::to_owned)
            .to_vec(),
            linked_at_ms: 0,
            answers,
            since_ms: None,
            route: route.map(str::to_owned),
            problem: (answers == Some(false)).then(|| "no answer in 4 seconds".to_owned()),
        }
    }

    #[test]
    fn a_satellite_says_how_it_reached_its_hub_and_whether_it_answers() {
        let status = |hub| LinkStatus {
            role: ritornel_client::contract::ServerRole::Satellite,
            hub: Some(hub),
            satellites: Vec::new(),
            awake: Awake::default(),
            kept_awake: false,
        };
        let via = "studio-mac.local:4747 (192.168.1.20:4747)";
        let text = render_status(&status(hub(Some(via), Some(true))));
        assert!(
            text.contains(&format!("Connected via     {via}\n")),
            "{text}"
        );
        assert!(text.contains("Hub answers       yes\n"), "{text}");

        let text = render_status(&status(hub(Some(via), Some(false))));
        assert!(
            text.contains(&format!("Last reached via  {via}\n")),
            "{text}"
        );
        assert!(
            text.contains("Hub answers       no: no answer in 4 seconds\n"),
            "{text}"
        );

        // Not tried yet, or never reached: no route to speak of.
        let text = render_status(&status(hub(None, None)));
        assert!(!text.contains("via"), "{text}");
        assert!(!text.contains("Hub answers"), "{text}");
        let text = render_status(&status(hub(None, Some(false))));
        assert!(!text.contains("via"), "{text}");
        assert!(
            text.contains("Hub answers       no: no answer in 4 seconds\n"),
            "{text}"
        );
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
