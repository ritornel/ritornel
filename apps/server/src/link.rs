//! The link between a hub and its satellites: the invites a hub holds, the
//! link sessions it checks, the addresses it offers, and how a satellite
//! makes sure it has reached its hub. See "The link between the hub and a
//! satellite" and "Routes" in docs/design/linking.md.

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

/// The addresses this machine has on the private network and on a tailnet,
/// with `port`: what an invite offers when it is not told what to offer.
///
/// Each is the address the system would send from toward a far address,
/// which it says without sending anything: the default route's for the
/// private network, and the route to the tailnet's own resolver for a
/// tailnet. Either is left out when it is not private.
pub(crate) fn found_routes(port: u16) -> Vec<String> {
    let toward = [
        // TEST-NET-1: never answered, so only the default route matters.
        Ipv4Addr::new(192, 0, 2, 1),
        // The resolver every tailnet has.
        Ipv4Addr::new(100, 100, 100, 100),
    ];
    let mut found = Vec::new();
    for far in toward {
        let from = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))
            .and_then(|socket| {
                socket.connect((far, 9))?;
                socket.local_addr()
            })
            .map(|address| address.ip());
        if let Ok(ip) = from
            && !ip.is_loopback()
            && is_private_route(ip)
        {
            let route = SocketAddr::new(ip, port).to_string();
            if !found.contains(&route) {
                found.push(route);
            }
        }
    }
    found
}

/// Whether `ip` is an address a satellite may dial: this machine, or one
/// on a private or link-local network or a tailnet, never a public
/// address. What travels on a route is plain HTTP, so it must not leave
/// home.
pub(crate) fn is_private_route(ip: IpAddr) -> bool {
    !ip.is_unspecified() && Listen::private_on_port(0).admits(ip)
}

/// The routes of an invite as addresses, refusing any that is not private.
pub(crate) fn routes_of(invite: &InviteLink) -> Result<Vec<SocketAddr>, String> {
    invite
        .routes
        .iter()
        .map(|route| match route.parse::<SocketAddr>() {
            Ok(address) if is_private_route(address.ip()) => Ok(address),
            Ok(_) => Err(format!(
                "{route} is not on a private network or a tailnet, and a link travels as plain \
                 HTTP"
            )),
            Err(_) => Err(format!("{route} is not an address and a port")),
        })
        .collect()
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
        return Err(format!(
            "the hub speaks contract version {} and this server {CONTRACT_VERSION}: update \
             both to the same release",
            descriptor.contract_version
        ));
    }
    if descriptor.role != ServerRole::Hub {
        return Err("that server is no longer a library's hub".to_owned());
    }
    Ok(descriptor)
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
        let invite = |route: &str| InviteLink {
            hub: Id::from_bytes([1; 16]),
            key: [2; 32],
            routes: vec![route.to_owned()],
            secret: [3; 32],
        };
        assert!(routes_of(&invite("192.168.1.20:4747")).is_ok());
        assert!(routes_of(&invite("8.8.8.8:4747")).is_err());
        assert!(routes_of(&invite("attic:4747")).is_err());
        for route in found_routes(4747) {
            assert!(is_private_route(route.parse::<SocketAddr>().unwrap().ip()));
        }
    }
}
