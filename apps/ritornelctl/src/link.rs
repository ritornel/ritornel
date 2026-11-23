//! `ritornelctl link`: inviting a satellite on a hub, joining a hub from
//! another server, and what each knows of the other. See
//! "Hub and satellites" in docs/internals/linking.md.

use std::io::Write;
use std::net::{SocketAddr, ToSocketAddrs};
use std::time::Duration;

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
                |status: &LinkStatus| render_status(status, &resolve_soon),
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
                |set: &AwakeSet| render_status(&set.link, &resolve_soon),
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
                |status: &LinkStatus| render_status(status, &resolve_soon),
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

/// What a name resolves to on this machine, as the satellite resolves it,
/// given a moment: a name that does not resolve soon is taken to give
/// nothing.
fn resolve_soon(route: &str) -> Vec<SocketAddr> {
    let route = route.to_owned();
    let (resolved, found) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let addresses = route
            .to_socket_addrs()
            .map(Iterator::collect)
            .unwrap_or_default();
        let _ = resolved.send(addresses);
    });
    found
        .recv_timeout(Duration::from_secs(1))
        .unwrap_or_default()
}

/// The route the satellite reached its hub on, as the link names it, and
/// the address that was: a route that is that address, or else the name
/// that resolves to it here. The status says only the address.
fn connected_via(hub: &HubStatus, resolve: &dyn Fn(&str) -> Vec<SocketAddr>) -> Option<String> {
    let address: SocketAddr = hub.route.as_deref()?.parse().ok()?;
    // An address the satellite has as a route, and by a name too, is
    // dialled as the address.
    if hub
        .routes
        .iter()
        .any(|route| route.parse::<SocketAddr>().ok() == Some(address))
    {
        return Some(address.to_string());
    }
    let named = hub
        .routes
        .iter()
        .filter(|route| route.parse::<SocketAddr>().is_err())
        .find(|route| resolve(route).contains(&address));
    Some(match named {
        Some(route) => format!("{route} ({address})"),
        None => address.to_string(),
    })
}

/// Whether the hub answers, as the satellite last found.
fn render_answers(hub: &HubStatus) -> String {
    match (hub.answers, &hub.problem) {
        (Some(true), _) => "yes".to_owned(),
        (Some(false), Some(problem)) => format!("no: {problem}"),
        (Some(false), None) => "no".to_owned(),
        (None, _) => "not asked yet".to_owned(),
    }
}

fn render_status(status: &LinkStatus, resolve: &dyn Fn(&str) -> Vec<SocketAddr>) -> String {
    let mut text = String::new();
    if let Some(hub) = &status.hub {
        let mut facts = vec![
            ("Role", "satellite".to_owned()),
            ("Hub", format!("{} ({})", hub.name, hub.server_id)),
            ("Library", hub.library_id.to_string()),
            ("Routes", hub.routes.join(", ")),
        ];
        if let Some(via) = connected_via(hub, resolve) {
            facts.push(("Connected via", via));
        }
        facts.push(("Hub answers", render_answers(hub)));
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

    /// What the names resolve to here, in these tests.
    fn resolve(route: &str) -> Vec<SocketAddr> {
        match route {
            "studio-mac.local:4747" => vec!["192.168.1.20:4747".parse().unwrap()],
            "studio.example-overlay.net:4747" => vec!["100.64.0.9:4747".parse().unwrap()],
            _ => Vec::new(),
        }
    }

    #[test]
    fn a_satellite_says_which_route_it_reached_its_hub_on_and_whether_it_answers() {
        let at = |route| connected_via(&hub(Some(route), Some(true)), &resolve);
        assert_eq!(
            at("192.168.1.20:4747").as_deref(),
            Some("studio-mac.local:4747 (192.168.1.20:4747)")
        );
        assert_eq!(at("10.0.0.2:4747").as_deref(), Some("10.0.0.2:4747"));
        assert_eq!(
            at("100.64.0.9:4747").as_deref(),
            Some("studio.example-overlay.net:4747 (100.64.0.9:4747)")
        );
        assert_eq!(connected_via(&hub(None, None), &resolve), None);

        let text = render_status(
            &LinkStatus {
                role: ritornel_client::contract::ServerRole::Satellite,
                hub: Some(hub(Some("192.168.1.20:4747"), Some(true))),
                satellites: Vec::new(),
                awake: Awake::default(),
                kept_awake: false,
            },
            &resolve,
        );
        assert!(
            text.contains("Connected via     studio-mac.local:4747 (192.168.1.20:4747)\n"),
            "{text}"
        );
        assert!(text.contains("Hub answers       yes\n"), "{text}");
        assert_eq!(
            render_answers(&hub(Some("192.168.1.20:4747"), Some(false))),
            "no: no answer in 4 seconds"
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
