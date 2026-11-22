//! The link between a hub and its satellites: inviting one, joining one,
//! and what each says of the other. See "Hub and satellites" in
//! docs/internals/linking.md.
//!
//! A hub invites a satellite with a one-time link that names the hub, its
//! public key and where it may be reached, and carries a secret. The
//! satellite checks that the server at a route proves it holds that key,
//! trades the secret for a link session at `POST /v1/link`, and from then
//! on dials the hub with that session. The hub never dials a satellite.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{Id, ServerRole, TrackSummary, hex};

/// The params of `link.invite`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct LinkInviteParams {
    /// Where the satellite may reach the hub, as `address:port`, in the
    /// order to try. Left empty, the hub offers the addresses it finds it
    /// has on the private network and a tailnet.
    #[serde(default)]
    pub routes: Vec<String>,
}

/// The result of `link.invite`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct LinkInvite {
    /// The link to give the satellite: `link.join` takes it. It holds a
    /// secret, and works once.
    pub link: String,
    /// When it stops working, as Unix milliseconds.
    pub expires_at_ms: i64,
}

/// The params of `link.join`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct LinkJoinParams {
    /// The command's ID, made by the client.
    pub idempotency_key: Id,
    /// The link `link.invite` gave on the hub.
    pub link: String,
}

/// The result of `link.status`, and of `link.join`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct LinkStatus {
    /// This server's role.
    pub role: ServerRole,
    /// On a satellite, its hub. Left out on any other server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub hub: Option<HubStatus>,
    /// On a hub, the satellites linked to it, oldest first.
    #[serde(default)]
    pub satellites: Vec<SatelliteStatus>,
}

/// A satellite's hub, as the satellite knows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct HubStatus {
    /// The hub's server ID.
    pub server_id: Id,
    /// The hub's name. Text from another machine: display only.
    pub name: String,
    /// The library the hub holds.
    pub library_id: Id,
    /// The hub epoch the satellite last saw.
    pub epoch: i64,
    /// Where the hub is reached, in the order they are tried.
    pub routes: Vec<String>,
    /// When the satellite joined, as Unix milliseconds.
    pub linked_at_ms: i64,
    /// Whether the hub answered the last time the satellite tried it. Left
    /// out until it has tried, which it does when it is first asked for
    /// something only the hub has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub answers: Option<bool>,
    /// Since when it has, or has not, as Unix milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub since_ms: Option<i64>,
    /// The route the satellite last reached it on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub route: Option<String>,
    /// Why the last try failed, while it does not answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub problem: Option<String>,
}

/// A satellite, as its hub knows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SatelliteStatus {
    /// The satellite's server ID.
    pub server_id: Id,
    /// Its name when it joined. Text from another machine: display only.
    pub name: String,
    /// When it joined, as Unix milliseconds.
    pub linked_at_ms: i64,
}

/// The params of `link.tracks`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct LinkTracksParams {
    /// Track appearances, each answered in its place.
    #[serde(default)]
    pub appearances: Vec<Id>,
    /// A release, whose tracks are answered after them, in disc and track
    /// order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub release: Option<Id>,
}

/// The result of `link.tracks`: what a satellite's player reads of the
/// library.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct LinkTracks {
    /// Each appearance asked for, in order: `null` for one the library
    /// does not have.
    pub tracks: Vec<Option<TrackSummary>>,
    /// The release's tracks, when one was asked for and the library has
    /// it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub release: Option<Vec<TrackSummary>>,
    /// The library's first listener: whom a new player plays for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub listener: Option<Id>,
}

/// The params of `link.copies`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct LinkCopiesParams {
    /// The track appearance.
    pub appearance: Id,
}

/// The result of `link.copies`: the masters of a track the hub holds, best
/// first, each fetched from `GET /v1/audio/{appearanceId}/{sourceItemId}/
/// {revision}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct LinkCopies {
    /// The copies, best first.
    pub copies: Vec<HubCopy>,
}

/// One master a hub holds of a track.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct HubCopy {
    /// The master's source item: what a listen names.
    pub source_item_id: Id,
    /// The revision of it the scan last read.
    pub revision: i64,
    /// The audio asset it plays as.
    pub asset_id: Id,
    /// Its length, as the scan read it. 0 when unknown.
    pub duration_ms: i64,
    /// Its size in bytes.
    pub size_bytes: u64,
    /// The file's extension, in lower case, which a decoder may go by.
    pub extension: String,
}

/// The params of `link.listen`: one listen a satellite's player recorded,
/// as its outbox sends it, with the stamps of this send.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct LinkListenParams {
    /// The listen's ID, which is also its source key at the hub: sent
    /// again, it is taken in once.
    pub idempotency_key: Id,
    /// The listen record, as the playing server staged it.
    pub record: serde_json::Value,
    /// The satellite's wall clock when it sent this.
    pub sent_at_ms: i64,
    /// Its monotonic clock then.
    pub sent_mono_ms: i64,
    /// The boot that clock counts from.
    pub sent_boot: Id,
}

/// What became of a listen a satellite sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum ListenTaken {
    /// It is in the record now.
    Stored,
    /// The record had it already: this was a resend.
    Duplicate,
    /// The record has another listen under its ID, and kept that one.
    Conflict,
}

/// The result of `link.listen`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct LinkListened {
    /// What became of it.
    pub taken: ListenTaken,
}

/// What a satellite sends to `POST /v1/link` to join: the invite's secret
/// and who it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkRequest {
    /// The invite's secret, as hex.
    pub secret: String,
    /// The satellite's server ID.
    pub server_id: Id,
    /// The satellite's name.
    pub name: String,
    /// The satellite's public key, as 64 hex digits.
    pub public_key: String,
    /// The signature, as 128 hex digits, of [`link_request_proof`] for the
    /// hub and this secret: proof that the satellite holds its key.
    pub proof: String,
    /// The [contract version](crate::CONTRACT_VERSION) the satellite
    /// speaks. A hub refuses another.
    pub contract_version: u32,
}

/// What the hub answers a satellite that joined.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkGranted {
    /// The link session's token, which the satellite presents from now on.
    pub token: String,
    /// The library the hub holds.
    pub library_id: Id,
    /// The hub epoch.
    pub epoch: i64,
}

/// What a satellite signs to prove it holds its key when it joins: a fixed
/// label, the hub's ID and the invite's secret, so the signature is good
/// for this hub and this invite alone.
pub fn link_request_proof(hub: Id, secret: &[u8]) -> Vec<u8> {
    const LABEL: &[u8] = b"ritornel link request proof v1\0";
    let mut message = Vec::with_capacity(LABEL.len() + 16 + secret.len());
    message.extend_from_slice(LABEL);
    message.extend_from_slice(hub.as_bytes());
    message.extend_from_slice(secret);
    message
}

/// The scheme of an invite link.
const SCHEME: &str = "ritornel://link/";

/// An invite, as its link writes it:
/// `ritornel://link/<hub ID>?key=<public key>&route=<address:port>…#<secret>`.
#[derive(Clone, PartialEq, Eq)]
pub struct InviteLink {
    /// The hub's server ID.
    pub hub: Id,
    /// The hub's public key.
    pub key: [u8; 32],
    /// Where the hub may be reached, in order.
    pub routes: Vec<String>,
    /// The secret that is traded for a link session.
    pub secret: [u8; 32],
}

impl InviteLink {
    /// Reads a link. `None` if it is not one.
    pub fn parse(text: &str) -> Option<Self> {
        let rest = text.trim().strip_prefix(SCHEME)?;
        let (rest, secret) = rest.split_once('#')?;
        let (hub, query) = rest.split_once('?')?;
        let mut key = None;
        let mut routes = Vec::new();
        for pair in query.split('&') {
            match pair.split_once('=')? {
                ("key", value) => key = Some(hex::decode_array(value)?),
                ("route", value) if !value.is_empty() => routes.push(value.to_owned()),
                _ => return None,
            }
        }
        if routes.is_empty() {
            return None;
        }
        Some(Self {
            hub: hub.parse().ok()?,
            key: key?,
            routes,
            secret: hex::decode_array(secret)?,
        })
    }
}

/// The link itself, secret and all: it is what a person copies to the
/// satellite.
impl fmt::Display for InviteLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{SCHEME}{}?key={}", self.hub, hex::encode(&self.key))?;
        for route in &self.routes {
            write!(f, "&route={route}")?;
        }
        write!(f, "#{}", hex::encode(&self.secret))
    }
}

/// Never the secret.
impl fmt::Debug for InviteLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InviteLink")
            .field("hub", &self.hub)
            .field("routes", &self.routes)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invite() -> InviteLink {
        InviteLink {
            hub: Id::from_bytes([0x11; 16]),
            key: [0x22; 32],
            routes: vec![
                "192.168.1.20:4747".to_owned(),
                "100.101.7.3:4747".to_owned(),
            ],
            secret: [0x33; 32],
        }
    }

    #[test]
    fn a_link_reads_back_as_it_was_written() {
        let text = invite().to_string();
        assert!(text.starts_with("ritornel://link/11111111-1111-1111-1111-111111111111?key=2222"));
        assert!(text.contains("&route=192.168.1.20:4747&route=100.101.7.3:4747#3333"));
        assert_eq!(InviteLink::parse(&format!("  {text}\n")), Some(invite()));
        assert!(!format!("{:?}", invite()).contains("3333"));
    }

    #[test]
    fn anything_else_is_not_a_link() {
        let text = invite().to_string();
        for wrong in [
            String::new(),
            text.replace("ritornel://link/", "http://"),
            text.replace('#', "&"),
            text.replace("key=22", "key=zz"),
            text.replace("&route=192.168.1.20:4747&route=100.101.7.3:4747", ""),
            text.replace("route=", "other="),
            text.replace("#3333", "#33"),
        ] {
            assert_eq!(InviteLink::parse(&wrong), None, "{wrong}");
        }
    }
}
