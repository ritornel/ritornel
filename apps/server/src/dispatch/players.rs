//! Local and linked players visible to the picker.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use ritornel_contract::LinkPlayerEvent;
use ritornel_contract::{
    ClientKind, ErrorCode, Event, HelloParams, Id, PlaybackState, PlaybackStatus, PlayerInfo,
    PlayerSummary, PlayersList, PresenceHello, RpcError,
};
use serde_json::Value;
use tokio::sync::{broadcast, watch};

use super::{Core, Stores, link::untrusted_name, lock};
use crate::session::Session;

#[derive(Debug)]
pub(super) struct Players {
    started_ms: i64,
    /// Live player reports, including each track start. Offline reports are not replayed.
    events: broadcast::Sender<(Id, LinkPlayerEvent)>,
    local: watch::Sender<Option<(u64, PlayerSummary)>>,
    peers: Mutex<BTreeMap<Id, PeerPlayer>>,
    /// Orders the durable membership change and registration of two hellos.
    registering: Mutex<()>,
}

#[derive(Debug)]
struct PeerPlayer {
    name: String,
    has_player: bool,
    since_ms: i64,
    newest_session: u64,
    #[cfg(feature = "lastfm")]
    sends_now_playing: bool,
    online: Option<Arc<Session>>,
    summary: Option<PlayerSummary>,
}

impl PeerPlayer {
    fn is_current(&self, session: &Session) -> bool {
        self.has_player
            && self
                .online
                .as_ref()
                .is_some_and(|s| s.id() == session.id() && !s.is_closed())
    }

    fn away(name: String, has_player: bool, since_ms: i64) -> Self {
        Self {
            name,
            has_player,
            since_ms,
            newest_session: 0,
            #[cfg(feature = "lastfm")]
            sends_now_playing: false,
            online: None,
            summary: None,
        }
    }
}

impl Players {
    pub(super) fn new(started_ms: i64, sessions: &[ritornel_engine::LinkSession]) -> Self {
        Self {
            started_ms,
            events: broadcast::Sender::new(64),
            local: watch::Sender::new(None),
            peers: Mutex::new(
                sessions
                    .iter()
                    .map(|s| {
                        (
                            s.peer,
                            PeerPlayer::away(s.label.clone(), s.has_player, started_ms),
                        )
                    })
                    .collect(),
            ),
            registering: Mutex::default(),
        }
    }
}

fn row(
    id: Id,
    name: String,
    local: bool,
    reachable: bool,
    since_ms: i64,
    state: Option<&PlayerSummary>,
) -> PlayerInfo {
    PlayerInfo {
        player_id: id,
        server_id: id,
        name,
        is_local: local,
        reachable,
        since_ms,
        status: state.map(|s| s.status),
        output: state.and_then(|s| s.output.clone()),
        title: state.and_then(|s| s.title.clone()),
    }
}

/// Only these known calls may cross a presence connection in either direction.
pub(super) fn is_player_method(name: &str) -> bool {
    use ritornel_contract::methods::*;
    matches!(
        name,
        PlaybackGet::NAME
            | PlaybackPlay::NAME
            | PlaybackPause::NAME
            | PlaybackResume::NAME
            | PlaybackStop::NAME
            | PlaybackSeek::NAME
            | PlaybackSkip::NAME
            | PlaybackJump::NAME
            | PlaybackSet::NAME
            | QueueWindow::NAME
            | QueueAdd::NAME
            | QueueRemove::NAME
            | QueueMove::NAME
            | QueueClear::NAME
    )
}

fn unreachable(name: &str, since: i64, problem: &str) -> RpcError {
    RpcError::new(ErrorCode::Unavailable, format!("{name} {problem}"))
        .with_detail("reason", "player_unreachable")
        .with_detail("name", name.to_owned())
        .with_detail("sinceMs", since)
}

fn online_player(
    peers: &BTreeMap<Id, PeerPlayer>,
    target: Id,
) -> Result<(&str, &Arc<Session>), RpcError> {
    let peer = peers.get(&target).filter(|p| p.has_player).ok_or_else(|| {
        RpcError::new(
            ErrorCode::Unsupported,
            "no such player is linked to this hub",
        )
        .with_detail("reason", "no_player")
    })?;
    let session = peer
        .online
        .as_ref()
        .ok_or_else(|| unreachable(&peer.name, peer.since_ms, "cannot be reached"))?;
    Ok((&peer.name, session))
}

impl Core {
    /// Refuse an absent target before doing library work. Sending the call
    /// checks the current presence session again after the resolution.
    pub(super) fn require_online_player(&self, target: Id) -> Result<(), RpcError> {
        online_player(&lock(&self.players.peers), target).map(|_| ())
    }

    pub(super) async fn call_player(
        &self,
        target: Id,
        method: &str,
        params: Option<Value>,
    ) -> Result<Value, RpcError> {
        self.call_player_within(target, method, params, crate::bounds::PLAYER_CALL_WAIT)
            .await
    }

    pub(super) async fn call_player_within(
        &self,
        target: Id,
        method: &str,
        params: Option<Value>,
        within: std::time::Duration,
    ) -> Result<Value, RpcError> {
        let sent_at = self.clock.now_ms();
        let (name, call) = {
            let peers = lock(&self.players.peers);
            let (name, session) = online_player(&peers, target)?;
            let call = session
                .queue_player_call(method, params)
                .ok_or_else(|| unreachable(name, sent_at, "cannot be reached"))?;
            (name.to_owned(), call)
        };
        call.answer_within(within).await.unwrap_or_else(|| {
            let peers = lock(&self.players.peers);
            if let Some(away) = peers.get(&target).filter(|p| p.online.is_none()) {
                Err(unreachable(&away.name, away.since_ms, "cannot be reached"))
            } else {
                Err(unreachable(&name, sent_at, "did not answer the call"))
            }
        })
    }

    /// Answers a request received on this satellite's verified hub connection.
    pub(crate) fn answer_hub_call(
        &self,
        session: &Session,
        request: ritornel_contract::Request,
    ) -> ritornel_contract::Response {
        let outcome = if request.method == "link.handoff" {
            super::run::<ritornel_contract::methods::LinkHandoff>(request.params, |p| {
                self.local_handoff(p)
            })
        } else if !is_player_method(&request.method) {
            Err(RpcError::new(
                ErrorCode::Forbidden,
                "the hub may only call this player and its queue",
            ))
        } else if request
            .params
            .as_ref()
            .is_some_and(|p| p.get("player").is_some())
        {
            Err(RpcError::new(
                ErrorCode::Invalid,
                "a forwarded call cannot name another player",
            ))
        } else {
            // Only this authenticated direction may vouch for a resolved track list.
            let known = request.params.as_ref().and_then(|p| p.get("tracksKnown"))
                == Some(&Value::Bool(true));
            self.call_with_known_tracks(session, &request.method, request.params, known)
        };
        ritornel_contract::Response {
            id: Some(request.id),
            outcome,
        }
    }

    pub(crate) fn player_events(&self) -> broadcast::Receiver<(Id, LinkPlayerEvent)> {
        self.players.events.subscribe()
    }

    /// Publishes a report with the player's identity from this server or the authenticated session.
    pub(super) fn publish_player_event(&self, player: Option<Id>, mut report: LinkPlayerEvent) {
        #[cfg(feature = "lastfm")]
        if player.is_none() {
            self.observe_now_playing(self.server_id, &report);
        }
        let event = match &mut report {
            LinkPlayerEvent::Playback(e) => {
                e.player = player;
                Some(Event::Playback(e.clone()))
            }
            LinkPlayerEvent::Progress(e) => {
                e.player = player;
                Some(Event::Progress(*e))
            }
            LinkPlayerEvent::Queue(e) => {
                e.player = player;
                Some(Event::Queue(*e))
            }
            LinkPlayerEvent::Began { .. } => None,
        };
        if let Some(event) = event {
            self.publish(&event);
        }
        let _ = self
            .players
            .events
            .send((player.unwrap_or(self.server_id), report));
    }

    pub(super) fn link_event(
        &self,
        session: &Session,
        report: LinkPlayerEvent,
    ) -> Result<ritornel_contract::NoParams, RpcError> {
        let peer = session.peer().ok_or_else(|| {
            RpcError::new(ErrorCode::Forbidden, "link.event needs a satellite link")
        })?;
        let peers = lock(&self.players.peers);
        #[cfg_attr(not(feature = "lastfm"), allow(unused_variables))]
        let presence = peers
            .get(&peer)
            .filter(|p| p.is_current(session))
            .ok_or_else(stale)?;
        #[cfg(feature = "lastfm")]
        if !presence.sends_now_playing {
            self.observe_now_playing(peer, &report);
        }
        // Order publication against replacement so a stale socket cannot publish after its successor.
        self.publish_player_event(Some(peer), report);
        Ok(ritornel_contract::NoParams {})
    }

    pub(crate) fn player_updates(&self) -> watch::Receiver<Option<(u64, PlayerSummary)>> {
        self.players.local.subscribe()
    }

    pub(super) fn player_state_changed(&self, playback: &PlaybackState) {
        let summary = PlayerSummary {
            status: playback.status,
            title: playback
                .track
                .as_ref()
                .and_then(|track| track.title.clone())
                .filter(|_| playback.status != PlaybackStatus::Stopped),
            output: (playback.output.sample_rate_hz > 0).then(|| playback.output.clone()),
        };
        if self.players.local.send_if_modified(|current| {
            if let Some((rev, held)) = current {
                if *rev > playback.revision {
                    return false;
                }
                *rev = playback.revision;
                if *held == summary {
                    return false;
                }
            }
            *current = Some((playback.revision, summary));
            true
        }) {
            self.publish(&Event::Players);
        }
    }

    pub(super) fn players_list(&self) -> Result<PlayersList, RpcError> {
        let mut players = Vec::new();
        if let Some(up) = self.uplink() {
            if !up.away() {
                match up.call("players.list", serde_json::json!({})) {
                    Ok(answer) => {
                        let mut list: PlayersList = serde_json::from_value(answer)
                            .map_err(|e| RpcError::new(ErrorCode::Internal, e.to_string()))?;
                        for p in &mut list.players {
                            p.is_local = p.server_id == self.server_id;
                        }
                        return Ok(list);
                    }
                    Err(error) if error.kind() == ErrorCode::Unavailable => {}
                    Err(error) => return Err(error),
                }
            }
        } else {
            players.extend(
                lock(&self.players.peers)
                    .iter()
                    .filter(|(_, p)| p.has_player)
                    .map(|(&id, p)| {
                        row(
                            id,
                            p.name.clone(),
                            false,
                            p.online.is_some(),
                            p.since_ms,
                            p.summary.as_ref(),
                        )
                    }),
            );
        }
        if let Some((_, state)) = &*self.players.local.borrow() {
            players.insert(
                0,
                row(
                    self.server_id,
                    self.name(),
                    true,
                    true,
                    self.players.started_ms,
                    Some(state),
                ),
            );
        }
        Ok(PlayersList { players })
    }

    pub(super) fn player_connected(
        &self,
        stores: &Stores,
        session: &Session,
        hello: &HelloParams,
        presence: &PresenceHello,
    ) -> Result<(), RpcError> {
        let peer = session
            .peer()
            .filter(|_| hello.kind == ClientKind::Server)
            .ok_or_else(|| {
                RpcError::new(ErrorCode::Forbidden, "presence needs a satellite link")
            })?;
        if presence.has_player != presence.player.is_some() {
            return Err(RpcError::new(
                ErrorCode::Invalid,
                "a player hello includes its current state",
            ));
        }
        let shared = self.shared(session).ok_or_else(stale)?;
        let _registering = lock(&self.players.registering);
        let known = {
            let peers = lock(&self.players.peers);
            if peers
                .get(&peer)
                .is_some_and(|p| p.newest_session >= session.id())
                || session.is_closed()
            {
                return Err(stale());
            }
            peers.get(&peer).is_some_and(|p| p.has_player)
        };
        if known != presence.has_player {
            stores
                .server
                .set_peer_player(peer, presence.has_player)
                .map_err(|e| RpcError::new(ErrorCode::Internal, e.to_string()))?;
        }
        self.satellite_named(stores, peer, &hello.name);
        let name = untrusted_name(&hello.name);
        let now = self.clock.now_ms();
        let mut peers = lock(&self.players.peers);
        let p = peers
            .entry(peer)
            .or_insert_with(|| PeerPlayer::away(name.clone(), false, now));
        let was_reachable = p.online.is_some();
        if let Some(old) = p.online.take() {
            old.close();
        }
        #[cfg(feature = "lastfm")]
        {
            p.sends_now_playing = presence.sends_now_playing;
            if let Some(lastfm) = self.lastfm() {
                lastfm.cancel_now_playing(peer);
            }
        }
        p.newest_session = session.id();
        p.name = name;
        p.has_player = presence.has_player;
        p.summary = None;
        if !session.is_closed() {
            p.summary = presence.player.clone().map(clean);
            session.enable_heartbeat();
            p.online = Some(shared);
        }
        if was_reachable != p.online.is_some() || known != p.has_player {
            p.since_ms = now;
        }
        drop(peers);
        self.moves.player_available();
        if known || presence.has_player {
            self.publish(&Event::Players);
        }
        Ok(())
    }

    pub(super) fn player_disconnected(&self, session: &Session) {
        let Some(peer) = session.peer() else { return };
        let mut peers = lock(&self.players.peers);
        if let Some(p) = peers.get_mut(&peer)
            && p.newest_session == session.id()
            && p.online.take().is_some()
        {
            #[cfg(feature = "lastfm")]
            if let Some(lastfm) = self.lastfm() {
                lastfm.cancel_now_playing(peer);
            }
            p.since_ms = self.clock.now_ms();
            p.summary = None;
            if p.has_player {
                drop(peers);
                self.publish(&Event::Players);
            }
        }
    }

    pub(super) fn player_renamed(&self, peer: Id, name: &str) {
        let mut peers = lock(&self.players.peers);
        if let Some(p) = peers.get_mut(&peer)
            && p.name != name
        {
            p.name = name.to_owned();
            if p.has_player {
                drop(peers);
                self.publish(&Event::Players);
            }
        }
    }

    pub(super) fn player_reported(
        &self,
        peer: Id,
        session: &Session,
        summary: PlayerSummary,
    ) -> Result<(), RpcError> {
        let mut peers = lock(&self.players.peers);
        let p = peers
            .get_mut(&peer)
            .filter(|p| p.is_current(session))
            .ok_or_else(stale)?;
        let summary = clean(summary);
        if p.summary.as_ref() != Some(&summary) {
            p.summary = Some(summary);
            drop(peers);
            self.publish(&Event::Players);
        }
        Ok(())
    }
}

fn stale() -> RpcError {
    RpcError::new(
        ErrorCode::Unavailable,
        "this connection does not hold the player's presence",
    )
}

fn clean(mut summary: PlayerSummary) -> PlayerSummary {
    fn display(text: &str) -> String {
        text.chars()
            .filter(|c| !c.is_control())
            .take(512)
            .collect::<String>()
            .trim()
            .to_owned()
    }
    summary.title = summary
        .title
        .filter(|_| summary.status != PlaybackStatus::Stopped)
        .map(|t| display(&t));
    if let Some(output) = &mut summary.output {
        output.name = display(&output.name);
        if let Some(problem) = &mut output.problem {
            problem.detail = display(&problem.detail);
        }
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionGrant;
    use ritornel_contract::{OutputInfo, OutputMode, TokenRole, Topic};

    fn peer_session(core: &Core, peer: Id) -> Arc<Session> {
        core.open(
            &mut lock(&core.sessions),
            SessionGrant::for_role(TokenRole::Personal),
            Some(peer),
        )
        .unwrap()
    }

    fn hello(name: &str) -> HelloParams {
        HelloParams {
            name: name.to_owned(),
            kind: ClientKind::Server,
            version: "1".into(),
            contract_version: ritornel_contract::CONTRACT_VERSION,
            presence: Some(PresenceHello {
                has_player: true,
                sends_now_playing: false,
                player: Some(PlayerSummary {
                    status: PlaybackStatus::Stopped,
                    output: None,
                    title: None,
                }),
            }),
        }
    }

    fn announce(core: &Core, session: &Session, name: &str) {
        core.with_stores(|stores| core.hello(stores, session, &hello(name)))
            .unwrap()
            .unwrap();
    }

    fn players_event(observer: &Session) {
        let frames = observer.drain();
        assert_eq!(frames.len(), 1);
        assert!(
            matches!(&frames[0], ritornel_contract::Frame::Notification(n)
            if n.params == Some(serde_json::json!({"topic":"players"})))
        );
    }

    #[test]
    fn a_delayed_or_inconsistent_hello_cannot_replace_the_current_player() {
        let (core, _scratch) = crate::testing::core();
        let peer = Id::from_bytes([8; 16]);
        let delayed = peer_session(&core, peer);
        let current = peer_session(&core, peer);
        announce(&core, &current, "Current");
        let inconsistent = peer_session(&core, peer);
        let mut invalid = hello("No player");
        invalid.presence.as_mut().unwrap().has_player = false;
        core.with_stores(|stores| {
            assert_eq!(
                core.hello(stores, &delayed, &hello("Older"))
                    .unwrap_err()
                    .kind(),
                ErrorCode::Unavailable
            );
            assert_eq!(
                core.hello(stores, &inconsistent, &invalid)
                    .unwrap_err()
                    .kind(),
                ErrorCode::Invalid
            );
        })
        .unwrap();
        assert!(!current.is_closed());
        assert_eq!(core.players_list().unwrap().players[0].name, "Current");
    }

    #[test]
    fn only_the_replacement_can_report_or_go_away() {
        let (core, _scratch) = crate::testing::core();
        let observer = core
            .open_session(SessionGrant::for_role(TokenRole::Guest))
            .unwrap();
        observer.subscribe(&[Topic::Players]);
        let peer = Id::from_bytes([7; 16]);
        let first = peer_session(&core, peer);
        announce(&core, &first, "Desk");
        players_event(&observer);
        let second = peer_session(&core, peer);
        announce(&core, &second, "New Desk");
        assert!(first.is_closed());
        players_event(&observer);
        let mut summary = PlayerSummary {
            status: PlaybackStatus::Paused,
            title: Some("Paper wings".into()),
            output: None,
        };
        assert!(core.player_reported(peer, &first, summary.clone()).is_err());
        core.close_session(&first);
        assert!(observer.drain().is_empty());
        core.player_reported(peer, &second, summary.clone())
            .unwrap();
        players_event(&observer);
        summary.output = Some(OutputInfo {
            name: "Speakers".into(),
            mode: OutputMode::Shared,
            sample_rate_hz: 44100,
            channels: 2,
            problem: None,
        });
        core.player_reported(peer, &second, summary.clone())
            .unwrap();
        players_event(&observer);
        summary.title = Some("Second song".into());
        core.player_reported(peer, &second, summary.clone())
            .unwrap();
        players_event(&observer);
        core.player_reported(peer, &second, summary.clone())
            .unwrap();
        assert!(observer.drain().is_empty());
        let visible = &core.players_list().unwrap().players[0];
        assert!(visible.reachable);
        assert_eq!(visible.name, "New Desk");
        assert_eq!(visible.status, Some(summary.status));
        assert_eq!(visible.title, summary.title);
        assert_eq!(visible.output, summary.output);
        core.close_session(&second);
        players_event(&observer);
        let away = &core.players_list().unwrap().players[0];
        assert!(!away.reachable);
        assert!(away.status.is_none());
    }

    #[test]
    fn only_the_current_presence_can_report_and_the_hub_stamps_its_player() {
        use ritornel_contract::{LinkPlayerEvent, QueueEvent};
        let (core, _scratch) = crate::testing::core();
        let id = Id::from_bytes([5; 16]);
        let local = core.open_session(SessionGrant::OWNER).unwrap();
        let report = LinkPlayerEvent::Queue(QueueEvent {
            player: Some(core.server_id),
            queue_revision: 9,
        });
        assert_eq!(
            core.link_event(&local, report.clone()).unwrap_err().kind(),
            ErrorCode::Forbidden
        );
        let old = peer_session(&core, id);
        announce(&core, &old, "Room");
        let current = peer_session(&core, id);
        announce(&core, &current, "Room");
        assert_eq!(
            core.link_event(&old, report.clone()).unwrap_err().kind(),
            ErrorCode::Unavailable
        );
        local
            .change_topics(Some(id), &[Topic::Queue], true)
            .unwrap();
        core.link_event(&current, report).unwrap();
        let frames = local.drain();
        assert!(
            matches!(&frames[..], [ritornel_contract::Frame::Notification(n)] if
            n.params == Some(serde_json::json!({"topic":"queue","player":id,"queueRevision":9})))
        );
    }

    #[test]
    fn the_satellite_refuses_other_methods_and_nested_player_targets() {
        use ritornel_contract::{Request, RequestId};
        use serde_json::json;

        let (core, _scratch) = crate::testing::core();
        let session = Session::new(0, SessionGrant::for_role(TokenRole::Guest));
        session.greet();
        for (method, params, code) in [
            ("server.status", json!({}), ErrorCode::Forbidden),
            ("list.open", json!({}), ErrorCode::Forbidden),
            ("queue.unknown", json!({}), ErrorCode::Forbidden),
            ("queue.window", json!({"player":null}), ErrorCode::Invalid),
            (
                "playback.get",
                json!({"player":core.server_id}),
                ErrorCode::Invalid,
            ),
        ] {
            let request = Request {
                id: RequestId::Number(1),
                method: method.into(),
                params: Some(params),
            };
            assert_eq!(
                core.answer_hub_call(&session, request)
                    .outcome
                    .unwrap_err()
                    .kind(),
                code
            );
        }
    }

    #[tokio::test]
    async fn a_replaced_player_releases_the_controller_and_only_its_successor_can_answer() {
        use ritornel_contract::{Frame, Request, RequestId, Response};
        use serde_json::json;

        let (core, _scratch) = crate::testing::core();
        let core = Arc::new(core);
        let peer = Id::from_bytes([8; 16]);
        let first = peer_session(&core, peer);
        announce(&core, &first, "Desk");
        let controller = core
            .open_session(SessionGrant::for_role(TokenRole::Guest))
            .unwrap();
        controller.greet();
        let read = || {
            let (core, controller) = (Arc::clone(&core), Arc::clone(&controller));
            tokio::task::spawn_blocking(move || {
                let request = Request {
                    id: RequestId::Number(7),
                    method: "queue.window".into(),
                    params: Some(json!({"player":peer})),
                };
                let Some(Frame::Response(answer)) =
                    core.dispatch(&controller, Frame::Request(request))
                else {
                    panic!("expected the controller's answer")
                };
                answer.outcome
            })
        };

        let pending = read();
        let Some(Frame::Request(old)) = first.next().await else {
            panic!("expected a request")
        };
        assert!(
            core.stores.try_write().is_ok(),
            "the hub holds its stores while waiting"
        );
        let second = peer_session(&core, peer);
        announce(&core, &second, "Desk");
        assert_eq!(
            pending.await.unwrap().unwrap_err().data.details["reason"],
            "player_unreachable"
        );

        let pending = read();
        let Some(Frame::Request(new)) = second.next().await else {
            panic!("expected a request")
        };
        // Both sessions start their own request numbering at one.
        assert_eq!(new.id, old.id);
        assert_eq!(new.params, Some(json!({})));
        first.answered(Response {
            id: Some(old.id),
            outcome: Ok(json!("old")),
        });
        second.answered(Response {
            id: Some(new.id),
            outcome: Ok(json!("new")),
        });
        assert_eq!(pending.await.unwrap().unwrap(), "new");
    }

    #[tokio::test(start_paused = true)]
    async fn a_player_has_four_seconds_to_answer_and_only_on_its_own_session() {
        use ritornel_contract::{Frame, Response};
        use serde_json::json;
        use std::time::Duration;

        let (core, _scratch) = crate::testing::core();
        let peer = Id::from_bytes([8; 16]);
        let desk = peer_session(&core, peer);
        let other = peer_session(&core, Id::from_bytes([7; 16]));
        announce(&core, &desk, "Desk");
        let start = tokio::time::Instant::now();
        let earliest = core.clock.now_ms();
        let call = core.call_player(peer, "queue.window", Some(json!({})));
        let read = async {
            let Some(Frame::Request(request)) = desk.next().await else {
                panic!("expected a request")
            };
            let reply = Response {
                id: Some(request.id),
                outcome: Ok(json!({})),
            };
            other.answered(reply.clone());
            reply
        };
        let (error, late) = tokio::join!(call, read);
        let error = error.unwrap_err();
        assert_eq!(start.elapsed(), Duration::from_secs(4));
        assert_eq!(error.message, "Desk did not answer the call");
        assert_eq!(error.kind(), ErrorCode::Unavailable);
        assert_eq!(error.data.details["reason"], "player_unreachable");
        assert_eq!(error.data.details["name"], "Desk");
        assert!(
            (earliest..=core.clock.now_ms())
                .contains(&error.data.details["sinceMs"].as_i64().unwrap())
        );
        desk.answered(late);
        assert!(!desk.is_closed());
    }

    #[derive(Default)]
    struct SilentWire {
        pings: Vec<tokio::time::Instant>,
    }

    impl crate::transport::Wire for SilentWire {
        async fn recv(&mut self) -> Option<crate::transport::Incoming> {
            std::future::pending().await
        }
        async fn send(&mut self, _: String) -> bool {
            true
        }
        async fn close(&mut self) {}
        async fn ping(&mut self) -> bool {
            self.pings.push(tokio::time::Instant::now());
            true
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_silent_player_is_pinged_at_five_seconds_and_away_at_fifteen() {
        use std::time::Duration;
        let (core, _scratch) = crate::testing::core();
        let core = Arc::new(core);
        let session = peer_session(&core, Id::from_bytes([7; 16]));
        announce(&core, &session, "Desk");
        let mut wire = SilentWire::default();
        let start = tokio::time::Instant::now();
        crate::transport::pump(&core, &session, &mut wire).await;
        core.close_session(&session);
        assert_eq!(start.elapsed(), Duration::from_secs(15));
        assert_eq!(wire.pings[0] - start, Duration::from_secs(5));
        assert_eq!(wire.pings[1] - start, Duration::from_secs(10));
        assert!(!core.players_list().unwrap().players[0].reachable);
    }
}
