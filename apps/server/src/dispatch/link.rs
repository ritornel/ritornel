//! `link.invite`, `link.join` and `link.status`, the hub's side of a
//! satellite joining, what a hub tells a satellite's player (`link.tracks`
//! and `link.copies`), and the listens it takes from one (`link.listen`).
//! See "Hub and satellites" in docs/internals/linking.md.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::PoisonError;

use ritornel_contract::methods::{LinkListen, Method};
use ritornel_contract::{
    CONTRACT_VERSION, ErrorCode, HubCopy, HubStatus, Id, InviteLink, LinkCopies, LinkCopiesParams,
    LinkGranted, LinkInvite, LinkInviteParams, LinkJoinParams, LinkListenParams, LinkListened,
    LinkRequest, LinkStatus, LinkTracks, LinkTracksParams, ListenTaken, MAX_CLIENT_TEXT, RpcError,
    SatelliteStatus, ServerRole, hex, link_request_proof,
};
use ritornel_engine::playback::{Catalog, Delivered, FileFacts, deliver_listens_to};
use ritornel_engine::{Clock, HubLink, Ingested, LISTEN_RECORD_VERSION, ListenRecord, SendStamps};
use tracing::{info, warn};

use super::{Core, Stores, lock};
use crate::link::{self, INVITE_LIFE};
use crate::session::Session;

/// The most tracks one `link.tracks` answers for, so that no answer
/// outgrows a frame.
const MAX_TRACKS_ASKED: usize = 1000;

/// How long a listen waits before it is sent again to a hub that did not
/// take it. The hub not answering is the usual reason, and it is soon
/// back, so a satellite tries more often than a hub retries its own disk.
const HUB_BACKOFF_MS: i64 = 30_000;

/// The largest number a listen from a satellite may hold: about 35,000
/// years in milliseconds, far beyond any clock or track, and far enough
/// below the limit of the record's arithmetic that no sum or difference of
/// two overflows.
const MAX_PLAUSIBLE: u64 = 1 << 50;

/// Whether every number in a listen record is a plausible one.
fn plausible(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Number(number) => number
            .as_i64()
            .map(i64::unsigned_abs)
            .or_else(|| number.as_u64())
            .is_some_and(|number| number <= MAX_PLAUSIBLE),
        serde_json::Value::Array(values) => values.iter().all(plausible),
        serde_json::Value::Object(fields) => fields.values().all(plausible),
        _ => true,
    }
}

/// Where a call is answered on a satellite.
pub(super) enum Route {
    /// On the satellite: what is its own, such as its player.
    Here,
    /// At the hub, forwarded unchanged: reading and changing the library.
    Hub,
    /// Nowhere: it names folders on the hub's machine, which a satellite
    /// cannot see. It is made on the hub.
    Refused(&'static str),
}

/// Where `method` is answered on a satellite: library calls go to the
/// hub; playback, output and pairing are local. See "Hub and satellites"
/// in docs/internals/linking.md.
pub(super) fn on_a_satellite(method: &str) -> Route {
    const FOLDERS: &str = "the library's folders are on its hub's machine: do this on the hub";
    match method {
        "server.hello" | "server.status" | "servers.list" | "events.subscribe"
        | "events.unsubscribe" | "media.token" | "server.backup" => Route::Here,
        _ if method.starts_with("playback.")
            || method.starts_with("queue.")
            || method.starts_with("link.") =>
        {
            Route::Here
        }
        "library.create"
        | "sources.add"
        | "scan.start"
        | "library.setWatching"
        | "artists.importPictures"
        | "artists.pictureFolders"
        | "sources.tracks"
        | "history.export" => Route::Refused(FOLDERS),
        _ => Route::Hub,
    }
}

/// Why a satellite was refused at `POST /v1/link`: the HTTP status and what
/// it is told.
pub(crate) type Refusal = (u16, String);

fn internal(error: impl std::fmt::Display) -> RpcError {
    RpcError::new(ErrorCode::Internal, error.to_string())
}

/// Whether `method` is kept from a satellite's link session: what names
/// the hub's own folders, which a satellite cannot see, and what is for the
/// hub's own machine alone, such as its backups and its links. A satellite
/// is trusted with the library's calls and its own listens, and no more.
pub(super) fn kept_from_a_link(method: &str) -> bool {
    matches!(
        method,
        "library.create"
            | "sources.add"
            | "scan.start"
            | "library.setWatching"
            | "artists.importPictures"
            | "artists.pictureFolders"
            | "sources.tracks"
            | "history.export"
            | "server.backup"
            | "link.invite"
            | "link.join"
    )
}

fn not_a_hub() -> RpcError {
    RpcError::new(ErrorCode::Invalid, "this server is not a library's hub")
}

/// This server's roots, by name.
fn roots(stores: &Stores) -> Result<HashMap<String, PathBuf>, RpcError> {
    Ok(stores
        .server
        .roots()
        .map_err(internal)?
        .into_iter()
        .map(|(name, path)| (name, PathBuf::from(path)))
        .collect())
}

/// A link call made on a satellite's own link session is refused: a
/// satellite may not invite another server or join one through its hub.
fn not_through_a_link(session: &Session, method: &str) -> Result<(), RpcError> {
    match session.peer() {
        Some(_) => Err(RpcError::new(
            ErrorCode::Forbidden,
            format!("{method} is for a session on this server's own machine, not a satellite's"),
        )),
        None => Ok(()),
    }
}

/// A name from another machine as it is kept and shown: control
/// characters gone, and no longer than a client's name may be.
fn untrusted_name(name: &str) -> String {
    let name: String = name
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_CLIENT_TEXT)
        .collect();
    match name.trim() {
        "" => "Ritornel server".to_owned(),
        name => name.to_owned(),
    }
}

impl Core {
    /// `link.invite`: a one-time link for a satellite to join with.
    pub(super) fn link_invite(
        &self,
        stores: &Stores,
        session: &Session,
        params: &LinkInviteParams,
    ) -> Result<LinkInvite, RpcError> {
        not_through_a_link(session, "link.invite")?;
        let is_hub = lock(&self.this).role == ServerRole::Hub
            && stores
                .library
                .read()
                .unwrap_or_else(PoisonError::into_inner)
                .is_some();
        if !is_hub {
            return Err(RpcError::new(
                ErrorCode::Invalid,
                "only a library's hub invites satellites: create a library here first",
            ));
        }
        let routes = if params.routes.is_empty() {
            self.offered_routes()
        } else {
            // Checked as a satellite will check them, so a link is never
            // made that no satellite would dial.
            link::check_routes(&params.routes)
                .map_err(|why| RpcError::new(ErrorCode::Invalid, why))?;
            params.routes.clone()
        };
        if routes.is_empty() {
            return Err(RpcError::new(
                ErrorCode::Invalid,
                "this server listens on loopback only, so no other machine can reach it: start \
                 it with --listen private:<port>",
            )
            .with_detail("reason", "loopback_only"));
        }
        let secret = self.invites.issue(&*self.entropy).map_err(internal)?;
        let invite = InviteLink {
            hub: self.server_id,
            key: self.signer.public_key(),
            routes,
            secret,
        };
        // How the routes were found is worth a line: when a satellite
        // cannot reach any of them, this is where to look.
        info!(routes = ?invite.routes, "an invite was made");
        Ok(LinkInvite {
            link: invite.to_string(),
            expires_at_ms: self
                .clock
                .now_ms()
                .saturating_add(i64::try_from(INVITE_LIFE.as_millis()).unwrap_or(i64::MAX)),
        })
    }

    /// The hub's side of `POST /v1/link`: checks the secret and the
    /// satellite's version, registers it in the library, and issues its
    /// link session. It writes both databases, so it runs where waiting is
    /// allowed.
    pub(crate) fn grant_link(&self, asked: &LinkRequest) -> Result<LinkGranted, Refusal> {
        let refused = |status: u16, why: &str| (status, format!("{why}\n"));
        if asked.contract_version != CONTRACT_VERSION {
            return Err(refused(
                409,
                &link::versions_differ(CONTRACT_VERSION, asked.contract_version),
            ));
        }
        let secret = hex::decode(&asked.secret).unwrap_or_default();
        // The satellite proves it holds the key it names, for this hub and
        // this invite, before the invite is spent.
        let key = hex::decode_array::<32>(&asked.public_key);
        let proved = key
            .zip(hex::decode_array::<64>(&asked.proof))
            .is_some_and(|(key, proof)| {
                ritornel_engine::verify_signature(
                    &key,
                    &link_request_proof(self.server_id, &secret),
                    &proof,
                )
            });
        let (Some(key), true) = (key, proved) else {
            return Err(refused(400, "the satellite did not prove it holds its key"));
        };
        if asked.server_id == self.server_id {
            return Err(refused(409, "a server cannot be its own satellite"));
        }
        if !self.invites.redeem(&secret) {
            warn!(peer = %asked.server_id, "a satellite offered a secret that is no open invite's");
            return Err(refused(403, "no such invite"));
        }
        let name = untrusted_name(&asked.name);
        let stores = self.stores.read().unwrap_or_else(PoisonError::into_inner);
        let stores = stores
            .as_ref()
            .ok_or_else(|| refused(503, "the server is stopping"))?;
        let library = stores
            .library
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        let Some(library) = library.as_ref() else {
            return Err(refused(409, "this server is not a library's hub"));
        };
        let failed = |error: ritornel_engine::Error| {
            warn!(%error, "a satellite could not be linked");
            refused(500, "the hub could not record the link")
        };
        // The library first: a satellite registered with no session can
        // join again, and a session with no registry row would deliver
        // listens the library refuses.
        library
            .register_server(asked.server_id, &name)
            .map_err(failed)?;
        let meta = library.meta().map_err(failed)?;
        let (session, token) = match stores
            .server
            .issue_link_session(asked.server_id, key, &name)
        {
            Ok(issued) => issued,
            Err(ritornel_engine::Error::Invalid(why)) => return Err(refused(409, why)),
            Err(error) => return Err(failed(error)),
        };
        self.link_sessions
            .add(token.hash(), asked.server_id, session);
        // Whatever the satellite had open on the session it held before
        // ends with it.
        self.close_linked(asked.server_id);
        self.stay_awake_while_linked();
        info!(satellite = %asked.server_id, name = %name, "a satellite joined");
        Ok(LinkGranted {
            token: token.expose().to_owned(),
            library_id: meta.library_id,
            epoch: meta.hub_epoch,
        })
    }

    /// `link.join`: makes this server the satellite of the hub that made
    /// the link.
    pub(super) fn link_join(
        &self,
        stores: &Stores,
        session: &Session,
        params: &LinkJoinParams,
    ) -> Result<LinkStatus, RpcError> {
        not_through_a_link(session, "link.join")?;
        let invite = InviteLink::parse(&params.link).ok_or_else(|| {
            RpcError::new(
                ErrorCode::Invalid,
                "that is not a link from `ritornelctl link invite`",
            )
        })?;
        // Sent again after it joined: the link it made.
        if let Some(held) = stores.server.hub_link().map_err(internal)?
            && held.hub == invite.hub
        {
            return self.link_status(stores);
        }
        if lock(&self.this).role != ServerRole::Unassigned {
            return Err(RpcError::new(
                ErrorCode::Conflict,
                "this server already holds a library or is linked to a hub, and a satellite \
                 holds none",
            ));
        }
        let routes =
            link::routes_of(&invite).map_err(|why| RpcError::new(ErrorCode::Invalid, why))?;
        let runtime = self.runtime.as_ref().ok_or_else(|| {
            RpcError::new(
                ErrorCode::Unavailable,
                "this server cannot reach the network",
            )
        })?;
        let name = self.name();
        let (route, descriptor) = runtime
            .block_on(async {
                let mut tried = Vec::new();
                for route in routes {
                    match link::reach(route, invite.hub, &invite.key, &*self.entropy).await {
                        Ok(descriptor) => return Ok((route, descriptor)),
                        Err(why) => tried.push(format!("{route}: {why}")),
                    }
                }
                Err(format!("no route reached the hub. {}", tried.join("; ")))
            })
            .map_err(|why| {
                RpcError::new(ErrorCode::Unavailable, why).with_detail("reason", "hub_unreachable")
            })?;
        // The hub answered: if it now refuses, that is its answer, not
        // the network's.
        let granted = runtime
            .block_on(link::request(
                route,
                &invite,
                self.server_id,
                &name,
                &self.signer,
            ))
            .map_err(|why| {
                RpcError::new(ErrorCode::Conflict, why).with_detail("reason", "refused")
            })?;
        // The route that worked goes first.
        let first = route.to_string();
        let mut routes = vec![first.clone()];
        routes.extend(
            invite
                .routes
                .iter()
                .filter(|other| **other != first)
                .cloned(),
        );
        let identity = stores
            .server
            .link_to_hub(&HubLink {
                hub: invite.hub,
                hub_public_key: invite.key,
                hub_name: untrusted_name(&descriptor.name),
                library_id: granted.library_id,
                hub_epoch: granted.epoch,
                routes,
                token: ritornel_engine::SessionToken::new(granted.token),
                linked_at_ms: self.clock.now_ms(),
            })
            .map_err(internal)?;
        info!(hub = %invite.hub, library_id = %granted.library_id, %route, "linked to a hub");
        if let Some(link) = stores.server.hub_link().map_err(internal)? {
            *self.uplink.write().unwrap_or_else(PoisonError::into_inner) =
                Some(std::sync::Arc::new(crate::uplink::Uplink::new(
                    link,
                    name,
                    runtime.clone(),
                    std::sync::Arc::clone(&self.clock),
                    std::sync::Arc::clone(&self.entropy),
                )));
        }
        {
            let mut this = lock(&self.this);
            this.role = super::role(identity.role);
            self.changed(&mut this);
        }
        self.link_status(stores)
    }

    /// `link.tracks`, on a hub.
    pub(super) fn link_tracks(
        &self,
        stores: &Stores,
        params: &LinkTracksParams,
    ) -> Result<LinkTracks, RpcError> {
        let library = stores
            .library
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        let library = library.as_ref().ok_or_else(not_a_hub)?;
        if params.appearances.len() > MAX_TRACKS_ASKED {
            return Err(RpcError::new(
                ErrorCode::Invalid,
                format!("ask for at most {MAX_TRACKS_ASKED} tracks at a time"),
            ));
        }
        let tracks = library
            .tracks(&params.appearances)
            .map_err(internal)?
            .iter()
            .map(|track| track.as_ref().map(super::playback::track))
            .collect();
        let release = match params.release {
            Some(release) => library
                .release_tracks(release)
                .map_err(internal)?
                .map(|tracks| tracks.iter().map(super::playback::track).collect()),
            None => None,
        };
        Ok(LinkTracks {
            tracks,
            release,
            listener: Catalog::first_listener(library).map_err(internal)?,
        })
    }

    /// `link.copies`, on a hub: the masters it can serve, best first.
    pub(super) fn link_copies(
        &self,
        stores: &Stores,
        params: &LinkCopiesParams,
    ) -> Result<LinkCopies, RpcError> {
        let library = stores
            .library
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        let library = library.as_ref().ok_or_else(not_a_hub)?;
        let roots = roots(stores)?;
        let copies = Catalog::copies(library, params.appearance, self.server_id)
            .map_err(internal)?
            .into_iter()
            .filter_map(|copy| {
                let path = copy.path(&roots)?;
                Some(HubCopy {
                    source_item_id: copy.source_item(),
                    revision: copy.revision_no(),
                    asset_id: copy.asset(),
                    duration_ms: copy.duration_ms(),
                    size_bytes: copy.facts().size_bytes,
                    extension: path
                        .extension()
                        .map(|extension| extension.to_string_lossy().to_lowercase())
                        .unwrap_or_default(),
                })
            })
            .collect();
        Ok(LinkCopies { copies })
    }

    /// Where a master of `appearance` is, with what the scan recorded of
    /// it, for `GET /v1/audio`: only one `link.copies` offers, and only at
    /// the revision it offered.
    pub(crate) fn master(
        &self,
        appearance: Id,
        source_item: Id,
        revision: i64,
    ) -> Option<(PathBuf, FileFacts)> {
        let stores = self.stores.read().unwrap_or_else(PoisonError::into_inner);
        let stores = stores.as_ref()?;
        let library = stores
            .library
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        let library = library.as_ref()?;
        let roots = roots(stores).ok()?;
        Catalog::copies(library, appearance, self.server_id)
            .ok()?
            .into_iter()
            .find(|copy| copy.source_item() == source_item && copy.revision_no() == revision)
            .and_then(|copy| Some((copy.path(&roots)?, copy.facts())))
    }

    /// `link.listen`, on a hub: one listen from a satellite's outbox, taken
    /// in as the hub's own player's are. A satellite reports listens for
    /// its own player only.
    pub(super) fn link_listen(
        &self,
        stores: &Stores,
        session: &Session,
        params: LinkListenParams,
    ) -> Result<LinkListened, RpcError> {
        let Some(peer) = session.peer() else {
            return Err(RpcError::new(
                ErrorCode::Forbidden,
                "link.listen is for a satellite's link session",
            ));
        };
        // What another machine sends is checked before the record's
        // arithmetic sees it: no time or count beyond what any clock or
        // track reaches, and a record of the version this hub reads.
        let stamps = [params.sent_at_ms, params.sent_mono_ms];
        if !plausible(&params.record)
            || stamps
                .iter()
                .any(|stamp| stamp.unsigned_abs() > MAX_PLAUSIBLE)
        {
            return Err(RpcError::new(
                ErrorCode::Invalid,
                "that listen holds a number no clock or track reaches",
            ));
        }
        let record: ListenRecord = serde_json::from_value(params.record).map_err(|error| {
            RpcError::new(ErrorCode::Invalid, format!("that is not a listen: {error}"))
        })?;
        if record.v != LISTEN_RECORD_VERSION {
            return Err(RpcError::new(
                ErrorCode::Invalid,
                format!(
                    "that listen is of record version {}, and this hub reads {LISTEN_RECORD_VERSION}",
                    record.v
                ),
            ));
        }
        if record.played_on_server != peer || record.listen_id != params.idempotency_key {
            return Err(RpcError::new(
                ErrorCode::Forbidden,
                "a satellite reports the listens of its own player, each under its own ID",
            ));
        }
        let library = stores
            .library
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        let library = library.as_ref().ok_or_else(not_a_hub)?;
        let ingested = library
            .ingest_listen(
                &record,
                SendStamps {
                    sent_at_ms: params.sent_at_ms,
                    sent_mono_ms: params.sent_mono_ms,
                    sent_boot: params.sent_boot,
                },
            )
            .map_err(|error| match error {
                // The satellite's to put right, not the hub's.
                ritornel_engine::Error::BadListen(why) => RpcError::new(ErrorCode::Invalid, why),
                other => internal(other),
            })?;
        let taken = match ingested {
            Ingested::Stored(_) => {
                // A play that counts may now be scrobbled, here.
                self.wake_scrobbler();
                ListenTaken::Stored
            }
            Ingested::Duplicate => ListenTaken::Duplicate,
            Ingested::Conflict => {
                warn!(
                    listen_id = %record.listen_id,
                    satellite = %peer,
                    "a satellite sent a listen the record holds otherwise"
                );
                ListenTaken::Conflict
            }
        };
        Ok(LinkListened { taken })
    }

    /// On a satellite, delivers the listens whose turn has come to its hub,
    /// with `link.listen` on the server's own connection, as
    /// [`deliver_listens`](ritornel_engine::playback::deliver_listens)
    /// delivers to a library. `None` on any other server.
    pub(crate) fn deliver_to_hub(
        &self,
        clock: &dyn Clock,
        stamps: &dyn Fn() -> SendStamps,
        after_ingest: &dyn Fn() -> Result<(), ritornel_engine::Error>,
    ) -> Option<Result<Delivered, ritornel_engine::Error>> {
        let uplink = self.uplink()?;
        let stores = self.stores.read().unwrap_or_else(PoisonError::into_inner);
        let stores = stores.as_ref()?;
        let send = |record: &ListenRecord, stamps: SendStamps| {
            let params = LinkListenParams {
                idempotency_key: record.listen_id,
                record: serde_json::to_value(record)?,
                sent_at_ms: stamps.sent_at_ms,
                sent_mono_ms: stamps.sent_mono_ms,
                sent_boot: stamps.sent_boot,
            };
            let answer = uplink
                .call(LinkListen::NAME, serde_json::to_value(params)?)
                .map_err(|error| ritornel_engine::Error::Playback(error.message))?;
            let listened: LinkListened = serde_json::from_value(answer)?;
            Ok(match listened.taken {
                // The hub's revision is the hub's: nothing here reads it.
                ListenTaken::Stored => Ingested::Stored(0),
                ListenTaken::Duplicate => Ingested::Duplicate,
                ListenTaken::Conflict => Ingested::Conflict,
            })
        };
        Some(deliver_listens_to(
            &stores.server,
            &send,
            clock,
            stamps,
            after_ingest,
            HUB_BACKOFF_MS,
        ))
    }

    /// `link.status`.
    pub(super) fn link_status(&self, stores: &Stores) -> Result<LinkStatus, RpcError> {
        let hub = match self.uplink() {
            Some(uplink) => Some(uplink.status()),
            None => stores
                .server
                .hub_link()
                .map_err(internal)?
                .map(|link| HubStatus {
                    server_id: link.hub,
                    name: link.hub_name,
                    library_id: link.library_id,
                    epoch: link.hub_epoch,
                    routes: link.routes,
                    linked_at_ms: link.linked_at_ms,
                    answers: None,
                    since_ms: None,
                    route: None,
                    problem: None,
                }),
        };
        let satellites = stores
            .server
            .link_sessions()
            .map_err(internal)?
            .into_iter()
            .map(|session| SatelliteStatus {
                server_id: session.peer,
                name: session.label,
                linked_at_ms: session.created_at_ms,
            })
            .collect();
        Ok(LinkStatus {
            role: lock(&self.this).role,
            hub,
            satellites,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_from_another_machine_is_kept_short_and_plain() {
        assert_eq!(untrusted_name("  desk\u{7}\n"), "desk");
        assert_eq!(untrusted_name("\u{1b}"), "Ritornel server");
        assert_eq!(untrusted_name(&"x".repeat(500)).len(), MAX_CLIENT_TEXT);
    }
}
