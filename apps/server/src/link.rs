//! The link between a hub and its satellites: the invites a hub holds, the
//! link sessions it checks, the addresses it offers, and how a satellite
//! makes sure it has reached its hub. See "Hub and satellites" in
//! docs/internals/linking.md.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::{Mutex, PoisonError, RwLock};
use std::time::{Duration, Instant};

use ritornel_contract::route;
use ritornel_contract::{
    CONTRACT_VERSION, Id, InviteLink, LinkGranted, LinkRequest, NONCE_PARAM, PublicDescriptor,
    ServerRole, descriptor_proof, hex, link_request_proof,
};
use ritornel_engine::{Entropy, LinkSession, SessionToken, Signer};

use crate::Listen;

/// How long an invite works.
pub(crate) const INVITE_LIFE: Duration = Duration::from_secs(10 * 60);

/// The invites a hub has made and not yet seen used. They are kept in
/// memory only: a restart ends them all, which an invite that lives ten
/// minutes can bear. A secret is 256 random bits, so wrong ones are not
/// counted: nothing guesses one, and a count would only let a stranger on
/// the network end the invites someone is about to use.
#[derive(Debug, Default)]
pub(crate) struct Invites(Mutex<Open>);

#[derive(Debug, Default)]
struct Open {
    /// The SHA-256 of each secret, with when it stops working.
    invites: Vec<([u8; 32], Instant)>,
}

impl Invites {
    /// Makes an invite's secret.
    pub(crate) fn issue(&self, entropy: &dyn Entropy) -> Result<[u8; 32], ritornel_engine::Error> {
        let mut secret = [0; 32];
        entropy.fill(&mut secret)?;
        let now = Instant::now();
        let mut open = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        open.invites.retain(|(_, until)| *until > now);
        open.invites.push((hash(&secret), now + INVITE_LIFE));
        Ok(secret)
    }

    /// Whether `secret` is an open invite's, which it then no longer is.
    pub(crate) fn redeem(&self, secret: &[u8]) -> bool {
        let hash = hash(secret);
        let now = Instant::now();
        let mut open = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        open.invites.retain(|(_, until)| *until > now);
        let before = open.invites.len();
        open.invites.retain(|(held, _)| *held != hash);
        open.invites.len() < before
    }
}

/// What an invite's secret is kept as: the SHA-256 of its hex, as a
/// session token's is.
fn hash(secret: &[u8]) -> [u8; 32] {
    SessionToken::new(hex::encode(secret)).hash()
}

/// A satellite a link session belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Peer {
    /// The satellite's server ID.
    pub(crate) server: Id,
    /// The link session's public ID.
    pub(crate) session: Id,
}

/// The link sessions a hub has issued, by the hash of their tokens. A
/// request's token is checked against them in memory, as the local
/// credential is.
#[derive(Debug, Default)]
pub(crate) struct LinkSessions(RwLock<HashMap<[u8; 32], Peer>>);

impl LinkSessions {
    pub(crate) fn new(sessions: Vec<LinkSession>) -> Self {
        let held = Self::default();
        for session in sessions {
            held.add(session.token_hash, session.peer, session.session);
        }
        held
    }

    /// Takes a session in, ending any the same satellite held before.
    pub(crate) fn add(&self, token_hash: [u8; 32], server: Id, session: Id) {
        let mut held = self.0.write().unwrap_or_else(PoisonError::into_inner);
        held.retain(|_, peer| peer.server != server);
        held.insert(token_hash, Peer { server, session });
    }

    /// The satellite whose link session `token` is, if it is one.
    pub(crate) fn peer(&self, token: &SessionToken) -> Option<Peer> {
        // A map lookup is not constant time, but the key is a SHA-256 of
        // the token: how long it takes says nothing of the token.
        self.0
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&token.hash())
            .copied()
    }
}

/// The routes an invite offers when it is not told what to offer, each
/// with `port`, best first:
///
/// - this machine's multicast DNS name, such as `studio-mac.local`, which
///   still finds the hub after its address changes;
/// - the address the system sends from on the default route, if private;
/// - its addresses on a tailnet (`100.64.0.0/10`), read from its network
///   interfaces, which finds both Tailscale's and NetBird's.
///
/// A satellite that cannot resolve the name uses the addresses.
pub(crate) fn found_routes(port: u16) -> Vec<String> {
    let mut found = Vec::new();
    if let Some(name) = crate::platform::mdns_name() {
        found.push(format!("{name}:{port}"));
    }
    // TEST-NET-1 is never answered, so only the route toward it matters,
    // and nothing is sent.
    let default = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))
        .and_then(|socket| {
            socket.connect((Ipv4Addr::new(192, 0, 2, 1), 9))?;
            socket.local_addr()
        })
        .map(|address| address.ip());
    let tailnet = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter(|interface| interface.oper_status != if_addrs::IfOperStatus::Down)
        .map(|interface| interface.ip())
        .filter(|ip| matches!(ip, IpAddr::V4(v4) if crate::config::is_tailnet(*v4)));
    for ip in default.into_iter().chain(tailnet) {
        let route = SocketAddr::new(ip, port).to_string();
        if !ip.is_loopback() && is_private_route(ip) && !found.contains(&route) {
            found.push(route);
        }
    }
    found
}

/// Whether `ip` is an address a satellite may dial: this machine, or one
/// on a private or link-local network or a tailnet, never a public
/// address. What travels on a route is plain HTTP, so it must not leave
/// home, or must travel inside a tailnet's own encryption.
pub(crate) fn is_private_route(ip: IpAddr) -> bool {
    !ip.is_unspecified() && Listen::private_on_port(0).admits(ip)
}

/// The most routes an invite names, so that a connection to a hub that
/// does not answer is bounded.
pub(crate) const MAX_ROUTES: usize = 8;

/// The most addresses of one route's name a satellite tries.
const MAX_ADDRESSES: usize = 4;

/// The longest a name may take to resolve.
const RESOLVE_WAIT: Duration = Duration::from_secs(3);

/// The longest a connection to the hub may take at one address, the proof
/// and the hello included. On a home network it takes milliseconds.
pub(crate) const CONNECT_WAIT: Duration = Duration::from_secs(4);

/// Checks routes as an invite names them: at most [`MAX_ROUTES`], each a
/// host and a port, the host an address on a private network or a
/// tailnet, or a name, such as `studio-mac.local` or a tailnet's name for
/// the machine. A name is resolved at each dial ([`resolve`]), so a hub
/// whose address changes is still found.
pub(crate) fn check_routes(routes: &[String]) -> Result<(), String> {
    if routes.len() > MAX_ROUTES {
        return Err(format!("a link names at most {MAX_ROUTES} routes"));
    }
    for route in routes {
        if let Ok(address) = route.parse::<SocketAddr>() {
            if address.port() == 0 {
                return Err(format!("{route} is not a host and a port"));
            }
            if !is_private_route(address.ip()) {
                return Err(format!(
                    "{route} is not on a private network or a tailnet, and a link travels as \
                     plain HTTP"
                ));
            }
        } else if !is_name_and_port(route) {
            return Err(format!("{route} is not a host and a port"));
        }
    }
    Ok(())
}

/// Whether `route` is a DNS name and a port: labels of letters, digits and
/// inner hyphens, of at most 63 characters, and a final dot allowed.
fn is_name_and_port(route: &str) -> bool {
    let Some((host, port)) = route.rsplit_once(':') else {
        return false;
    };
    let host = host.strip_suffix('.').unwrap_or(host);
    port.bytes().all(|byte| byte.is_ascii_digit())
        && port.parse::<u16>().is_ok_and(|port| port != 0)
        && !host.is_empty()
        && host.len() <= 253
        && host.split('.').all(|label| {
            (1..=63).contains(&label.len())
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        })
}

/// `work`, given at most `wait`.
pub(crate) async fn within<T>(
    wait: Duration,
    work: impl Future<Output = Result<T, String>>,
) -> Result<T, String> {
    tokio::time::timeout(wait, work)
        .await
        .unwrap_or_else(|_| Err(format!("no answer in {} seconds", wait.as_secs())))
}

/// The addresses `route` dials now: an address as it is, a name as the
/// system resolves it at this moment, by DNS or multicast DNS. Only IPv4
/// addresses on a private network or a tailnet are kept: the hub listens
/// on IPv4, and what travels is plain HTTP.
///
/// A name that answers with another machine, by mistake or by spoofing,
/// gets nothing from the satellite but a nonce: [`reach`] asks whatever
/// answers to prove it holds the hub's key first. What the proof cannot
/// stop is what plain HTTP cannot: someone able to relay traffic on the
/// network reads what travels, by a name as by an address. That is why a
/// route must be private, or inside a tailnet's own encryption.
pub(crate) async fn resolve(route: &str) -> Result<Vec<SocketAddr>, String> {
    let found: Vec<SocketAddr> = within(RESOLVE_WAIT, async {
        tokio::net::lookup_host(route)
            .await
            .map(Iterator::collect)
            .map_err(|error| format!("it could not be resolved: {error}"))
    })
    .await?;
    let usable = usable(route, &found);
    if usable.is_empty() {
        return Err(if found.is_empty() {
            "it resolves to no address".to_owned()
        } else {
            "it resolves to no IPv4 address on a private network or a tailnet".to_owned()
        });
    }
    Ok(usable)
}

/// What a satellite may dial of what `route` resolved to: its IPv4
/// addresses on a private network or a tailnet, best first ([`Way`]), and
/// at most [`MAX_ADDRESSES`] of them.
fn usable(route: &str, found: &[SocketAddr]) -> Vec<SocketAddr> {
    let mut usable: Vec<SocketAddr> = Vec::new();
    for address in found {
        if address.is_ipv4() && is_private_route(address.ip()) && !usable.contains(address) {
            usable.push(*address);
        }
    }
    // Stable, so the resolver's order holds among equals.
    usable.sort_by_key(|address| Way::of(route, *address));
    usable.truncate(MAX_ADDRESSES);
    usable
}

/// How long a satellite waits on one address before it also tries the
/// next. An address on a home network answers in milliseconds.
const STAGGER: Duration = Duration::from_millis(500);

/// Tries `attempt` at each of `candidates`, best first, each given
/// [`CONNECT_WAIT`], and returns the first that succeeds, with the route and
/// the address it was. The next is
/// started when the one before fails or has not answered within
/// [`STAGGER`], so the best way that answers soon is taken, and one that
/// never answers does not hold up the rest. What failed is said.
pub(crate) async fn first_answer<T, F>(
    candidates: Vec<(String, SocketAddr)>,
    attempt: impl Fn(SocketAddr) -> F,
) -> Result<(String, SocketAddr, T), Vec<String>>
where
    F: Future<Output = Result<T, String>>,
{
    use futures_util::StreamExt;
    let start = |(route, address): (String, SocketAddr)| {
        let tried = within(CONNECT_WAIT, attempt(address));
        async move { (route, address, tried.await) }
    };
    let mut waiting = candidates.into_iter();
    let mut running = futures_util::stream::FuturesUnordered::new();
    let mut failed = Vec::new();
    loop {
        if running.is_empty() {
            match waiting.next() {
                Some(next) => running.push(start(next)),
                None => return Err(failed),
            }
        }
        tokio::select! {
            Some((route, address, tried)) = running.next() => match tried {
                Ok(answer) => return Ok((route, address, answer)),
                Err(why) => {
                    failed.push(format!("{route} ({address}): {why}"));
                    if let Some(next) = waiting.next() {
                        running.push(start(next));
                    }
                }
            },
            () = tokio::time::sleep(STAGGER), if waiting.len() > 0 => {
                if let Some(next) = waiting.next() {
                    running.push(start(next));
                }
            }
        }
    }
}

/// Where a route's address leads, in the order a satellite tries them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Way {
    /// An address on this network, written as an address.
    Here,
    /// A name, such as `studio-mac.local`, that resolves to an address on
    /// this network.
    NamedHere,
    /// An address on a tailnet: through a tunnel, which is slower for the
    /// masters a satellite pulls.
    Tailnet,
}

impl Way {
    fn of(route: &str, address: SocketAddr) -> Self {
        match address.ip() {
            IpAddr::V4(v4) if crate::config::is_tailnet(v4) => Self::Tailnet,
            _ if route.parse::<SocketAddr>().is_ok() => Self::Here,
            _ => Self::NamedHere,
        }
    }
}

/// The addresses a satellite tries to reach its hub, best first, with the
/// route each came from, and why any route gave none. Every route is
/// resolved again, all at once, and what it gives is ordered by where it
/// leads ([`Way`]): an address on this network, then a name that resolves
/// to one, then a tailnet, so a satellite at home pulls masters over its
/// own network and uses a tunnel only when that fails. Among equals, the
/// order the invite named them holds.
pub(crate) async fn candidates(routes: &[String]) -> (Vec<(String, SocketAddr)>, Vec<String>) {
    let routes: Vec<&String> = routes.iter().take(MAX_ROUTES).collect();
    let resolved =
        futures_util::future::join_all(routes.iter().map(|route| resolve_recently(route))).await;
    let mut problems = Vec::new();
    let mut found = Vec::new();
    for (route, addresses) in routes.into_iter().zip(resolved) {
        match addresses {
            Ok(addresses) => found.push((route.clone(), addresses)),
            Err(why) => problems.push(format!("{route}: {why}")),
        }
    }
    (order(found), problems)
}

/// What routes resolved to, one address a line, best first: by where each
/// leads ([`Way`]), then in the order the routes were named, and an
/// address that two routes give only once, where it ranks best.
fn order(resolved: Vec<(String, Vec<SocketAddr>)>) -> Vec<(String, SocketAddr)> {
    let mut found: Vec<(Way, String, SocketAddr)> = resolved
        .into_iter()
        .flat_map(|(route, addresses)| {
            addresses
                .into_iter()
                .map(move |address| (Way::of(&route, address), route.clone(), address))
        })
        .collect();
    // Stable, so the routes' order holds among equals.
    found.sort_by_key(|(way, ..)| *way);
    let mut seen = Vec::new();
    found
        .into_iter()
        .filter(|(_, _, address)| {
            let new = !seen.contains(address);
            seen.push(*address);
            new
        })
        .map(|(_, route, address)| (route, address))
        .collect()
}

/// How long what a route resolved to, or that it resolved to nothing, is
/// used again. A satellite opens a connection for each client session,
/// and a name that does not resolve, such as a `.local` name away from
/// home, would make each of them wait out [`RESOLVE_WAIT`] again.
const RESOLVED_FOR: Duration = Duration::from_secs(30);

type Resolved = Result<Vec<SocketAddr>, String>;

/// What each route resolved to lately, and when.
static RECENTLY: std::sync::LazyLock<Mutex<HashMap<String, (Instant, Resolved)>>> =
    std::sync::LazyLock::new(Mutex::default);

/// [`resolve`], or what it gave for `route` within [`RESOLVED_FOR`].
async fn resolve_recently(route: &str) -> Resolved {
    let recent = RECENTLY
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(route)
        .filter(|(at, _)| at.elapsed() < RESOLVED_FOR)
        .map(|(_, resolved)| resolved.clone());
    if let Some(resolved) = recent {
        return resolved;
    }
    let resolved = resolve(route).await;
    RECENTLY
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(route.to_owned(), (Instant::now(), resolved.clone()));
    resolved
}

/// Asks the server at `route` for its descriptor with a fresh nonce, and
/// returns it if that server is the hub `hub`: it has that ID, proves it
/// holds `key`, is a hub, and speaks this server's contract. Nothing but
/// the nonce is sent.
pub(crate) async fn reach(
    route: SocketAddr,
    hub: Id,
    key: &[u8; 32],
    entropy: &dyn Entropy,
) -> Result<PublicDescriptor, String> {
    let mut nonce = [0; 16];
    entropy
        .fill(&mut nonce)
        .map_err(|error| error.to_string())?;
    let path = format!(
        "{}?{NONCE_PARAM}={}",
        route::DESCRIPTOR,
        hex::encode(&nonce)
    );
    let answer = ritornel_client::get(route, &path, None)
        .await
        .map_err(|error| error.to_string())?;
    if answer.status != 200 {
        return Err(format!("it answered with HTTP {}", answer.status));
    }
    let descriptor: PublicDescriptor = serde_json::from_slice(&answer.body)
        .map_err(|_| "what answered is not a Ritornel server".to_owned())?;
    let proof = descriptor
        .proof
        .as_deref()
        .and_then(hex::decode_array::<64>);
    let proved = descriptor.server_id == hub
        && descriptor.public_key.as_deref() == Some(hex::encode(key).as_str())
        && proof.is_some_and(|proof| {
            ritornel_engine::verify_signature(key, &descriptor_proof(hub, &nonce), &proof)
        });
    if !proved {
        return Err("another server answered there, not the hub the link names".to_owned());
    }
    if descriptor.contract_version != CONTRACT_VERSION {
        return Err(versions_differ(
            descriptor.contract_version,
            CONTRACT_VERSION,
        ));
    }
    if descriptor.role != ServerRole::Hub {
        return Err("that server is no longer a library's hub".to_owned());
    }
    Ok(descriptor)
}

/// What a satellite is told when it and its hub speak different contract
/// versions: which side is behind, and so which to update. The hub says it
/// from the satellite's side too, since the satellite shows it.
pub(crate) fn versions_differ(hub: u32, satellite: u32) -> String {
    let behind = if hub < satellite {
        "update the hub, which is the older"
    } else {
        "update this server, which is the older"
    };
    format!(
        "this server and its hub speak different versions of the contract (the hub {hub}, this \
         server {satellite}): {behind}, so both run the same release"
    )
}

/// Trades the invite's secret for a link session at the hub at `route`.
pub(crate) async fn request(
    route: SocketAddr,
    invite: &InviteLink,
    server_id: Id,
    name: &str,
    signer: &Signer,
) -> Result<LinkGranted, String> {
    let asked = LinkRequest {
        secret: hex::encode(&invite.secret),
        server_id,
        name: name.to_owned(),
        public_key: hex::encode(&signer.public_key()),
        proof: hex::encode(&signer.sign(&link_request_proof(invite.hub, &invite.secret))),
        contract_version: CONTRACT_VERSION,
    };
    let answer = ritornel_client::post(route, route::LINK, None, &asked)
        .await
        .map_err(|error| error.to_string())?;
    match answer.status {
        200 => serde_json::from_slice(&answer.body)
            .map_err(|_| "the hub's answer could not be read".to_owned()),
        403 => Err(
            "the hub refused the link: it has been used, has expired, or the hub has restarted \
             since it was made. Make another with `ritornelctl link invite` on the hub"
                .to_owned(),
        ),
        _ => Err(String::from_utf8_lossy(&answer.body).trim().to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_invite_works_once_and_wrong_secrets_end_none() {
        let invites = Invites::default();
        let entropy = ritornel_engine::OsEntropy;
        let first = invites.issue(&entropy).unwrap();
        assert!(invites.redeem(&first));
        assert!(!invites.redeem(&first));

        let second = invites.issue(&entropy).unwrap();
        for _ in 0..100 {
            assert!(!invites.redeem(&[0; 32]));
        }
        assert!(invites.redeem(&second));
    }

    #[test]
    fn a_satellite_holds_one_link_session() {
        let sessions = LinkSessions::default();
        let (peer, other) = (Id::from_bytes([1; 16]), Id::from_bytes([2; 16]));
        let (old, new) = (SessionToken::new("old"), SessionToken::new("new"));
        sessions.add(old.hash(), peer, Id::from_bytes([3; 16]));
        sessions.add(
            SessionToken::new("o").hash(),
            other,
            Id::from_bytes([4; 16]),
        );
        sessions.add(new.hash(), peer, Id::from_bytes([5; 16]));
        assert_eq!(sessions.peer(&old), None);
        assert_eq!(sessions.peer(&new).map(|peer| peer.server), Some(peer));
        assert_eq!(
            sessions
                .peer(&SessionToken::new("o"))
                .map(|peer| peer.server),
            Some(other)
        );
    }

    #[test]
    fn a_version_mismatch_says_which_side_to_update() {
        assert!(versions_differ(3, 2).contains("update this server"));
        assert!(versions_differ(2, 3).contains("update the hub"));
        assert!(versions_differ(2, 3).contains("the hub 2, this server 3"));
    }

    #[test]
    fn a_new_listen_record_is_a_new_contract() {
        // `link.listen` carries the listen record as JSON, which the
        // contract's digest does not see. A hub and a satellite that agree
        // on the contract must agree on the record too, so a change to the
        // record raises CONTRACT_VERSION, and this pair with it.
        assert_eq!(
            (
                ritornel_contract::CONTRACT_VERSION,
                ritornel_engine::LISTEN_RECORD_VERSION
            ),
            (2, 1),
            "the listen record changed: raise CONTRACT_VERSION too"
        );
    }

    #[test]
    fn only_private_routes_are_dialled() {
        for private in [
            "127.0.0.1",
            "192.168.1.20",
            "10.0.0.2",
            "100.101.7.3",
            "fd7a:115c:a1e0::1",
        ] {
            assert!(is_private_route(private.parse().unwrap()), "{private}");
        }
        for not in ["0.0.0.0", "8.8.8.8", "2001:db8::1"] {
            assert!(!is_private_route(not.parse().unwrap()), "{not}");
        }
        let check = |route: &str| check_routes(&[route.to_owned()]);
        assert!(check("192.168.1.20:4747").is_ok());
        assert!(check("[fd7a:115c:a1e0::1]:4747").is_ok());
        assert!(check("8.8.8.8:4747").is_err());
        // A name is checked when it is dialled, by what it resolves to.
        assert!(check("studio-mac.local:4747").is_ok());
        assert!(check("studio.example-overlay.net:4747").is_ok());
        assert!(check("Studio.Example-Overlay.NET.:4747").is_ok());
        let long = format!("{}.local:4747", "a".repeat(64));
        for wrong in [
            "127.0.0.1:0",
            "8.8.8.8:+4747",
            "-studio.local:4747",
            "studio-.local:4747",
            long.as_str(),
        ] {
            assert!(check(wrong).is_err(), "{wrong}");
        }
        let nine = vec!["192.168.1.20:4747".to_owned(); MAX_ROUTES + 1];
        assert!(check_routes(&nine).is_err());
        for wrong in [
            "attic",
            "attic:",
            "attic:0",
            ":4747",
            "http://attic:4747",
            "at tic:4747",
        ] {
            assert!(check(wrong).is_err(), "{wrong}");
        }
        for route in found_routes(4747) {
            if let Ok(address) = route.parse::<SocketAddr>() {
                assert!(is_private_route(address.ip()), "{route}");
            } else {
                assert!(route.ends_with(".local:4747"), "{route}");
            }
        }
    }

    #[test]
    fn a_tailnet_is_the_shared_address_space() {
        for ip in ["100.64.0.1", "100.100.100.100", "100.127.255.254"] {
            assert!(crate::config::is_tailnet(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["100.63.255.255", "100.128.0.1", "192.168.1.20"] {
            assert!(!crate::config::is_tailnet(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn addresses_are_ordered_by_where_they_lead_and_each_tried_once() {
        let routes = |list: &[(&str, &[&str])]| -> Vec<(String, Vec<SocketAddr>)> {
            list.iter()
                .map(|(route, addresses)| {
                    (
                        (*route).to_owned(),
                        addresses.iter().map(|a| a.parse().unwrap()).collect(),
                    )
                })
                .collect()
        };
        let ordered = order(routes(&[
            // A tailnet's name, first as a satellite may have kept it.
            ("studio.example-overlay.net:4747", &["100.64.0.9:4747"]),
            // A name for this network that also gives a tailnet address.
            (
                "studio-mac.local:4747",
                &["100.64.0.9:4747", "192.168.1.20:4747"],
            ),
            ("192.168.1.20:4747", &["192.168.1.20:4747"]),
            ("10.0.0.2:4747", &["10.0.0.2:4747"]),
        ]));
        let ordered: Vec<(&str, String)> = ordered
            .iter()
            .map(|(route, address)| (route.as_str(), address.to_string()))
            .collect();
        assert_eq!(
            ordered,
            [
                ("192.168.1.20:4747", "192.168.1.20:4747".to_owned()),
                ("10.0.0.2:4747", "10.0.0.2:4747".to_owned()),
                (
                    "studio.example-overlay.net:4747",
                    "100.64.0.9:4747".to_owned()
                ),
            ]
        );
    }

    #[test]
    fn a_names_addresses_are_ranked_before_the_cap() {
        let found: Vec<SocketAddr> = [
            "100.64.0.1:4747",
            "100.64.0.2:4747",
            "8.8.8.8:4747",
            "100.64.0.3:4747",
            "100.64.0.4:4747",
            "192.168.1.20:4747",
            "100.64.0.1:4747",
        ]
        .iter()
        .map(|address| address.parse().unwrap())
        .collect();
        let kept = usable("studio.example-overlay.net:4747", &found);
        assert_eq!(kept.len(), MAX_ADDRESSES);
        assert_eq!(kept[0], "192.168.1.20:4747".parse().unwrap());
        assert_eq!(kept[1], "100.64.0.1:4747".parse().unwrap());
    }

    fn at(address: &str) -> (String, SocketAddr) {
        (address.to_owned(), address.parse().unwrap())
    }

    #[tokio::test]
    async fn the_best_address_that_answers_soon_is_taken_and_a_silent_one_holds_up_nothing() {
        let (lan, tunnel) = ("192.168.1.20:4747", "100.64.0.1:4747");
        // The better answers a moment later than the next would: it is
        // still taken, since the next is not asked until it has had time.
        let answered = first_answer(vec![at(lan), at(tunnel)], |address| async move {
            if address.ip().to_string() == "192.168.1.20" {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Ok::<_, String>(address)
        })
        .await
        .unwrap();
        assert_eq!(answered.1, lan.parse().unwrap());

        // One that never answers: the next is asked after the stagger, not
        // after the whole connect wait.
        let started = Instant::now();
        let answered = first_answer(vec![at(lan), at(tunnel)], |address| async move {
            if address.ip().to_string() == "192.168.1.20" {
                std::future::pending::<()>().await;
            }
            Ok::<_, String>(address)
        })
        .await
        .unwrap();
        assert_eq!(answered.1, tunnel.parse().unwrap());
        assert!(started.elapsed() < CONNECT_WAIT, "{:?}", started.elapsed());

        // None answers: each says why.
        let failed = first_answer(vec![at(lan), at(tunnel)], |_| async {
            Err::<(), _>("refused".to_owned())
        })
        .await
        .unwrap_err();
        assert_eq!(failed.len(), 2, "{failed:?}");
    }

    #[tokio::test]
    async fn this_network_is_tried_before_a_name_for_it_and_a_tailnet_last() {
        let routes = [
            "100.64.7.3:4747",
            "localhost:4747",
            "nowhere.invalid:4747",
            "192.168.1.20:4747",
            "10.0.0.2:4747",
        ]
        .map(str::to_owned);
        let (found, problems) = candidates(&routes).await;
        let order: Vec<_> = found
            .iter()
            .map(|(route, address)| (route.as_str(), address.ip().to_string()))
            .collect();
        assert_eq!(
            order,
            [
                ("192.168.1.20:4747", "192.168.1.20".to_owned()),
                ("10.0.0.2:4747", "10.0.0.2".to_owned()),
                ("localhost:4747", "127.0.0.1".to_owned()),
                ("100.64.7.3:4747", "100.64.7.3".to_owned()),
            ]
        );
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(
            problems[0].starts_with("nowhere.invalid:4747"),
            "{problems:?}"
        );
    }

    #[tokio::test]
    async fn a_route_resolves_to_its_private_addresses_only() {
        let resolved = resolve("localhost:4747").await.unwrap();
        assert!(!resolved.is_empty());
        assert!(
            resolved
                .iter()
                .all(|address| address.is_ipv4() && address.ip().is_loopback())
        );
        assert_eq!(
            resolve("192.168.1.20:4747").await.unwrap(),
            vec!["192.168.1.20:4747".parse().unwrap()]
        );
        assert!(resolve("8.8.8.8:4747").await.is_err());
    }
}
