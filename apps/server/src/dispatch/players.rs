//! Local and linked players visible to the picker.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use ritornel_contract::{
    ClientKind, ErrorCode, Event, HelloParams, Id, PlaybackState, PlaybackStatus, PlayerInfo,
    PlayerSummary, PlayersList, PresenceHello, RpcError,
};
use tokio::sync::watch;

use super::{Core, Stores, link::untrusted_name, lock};
use crate::session::Session;

#[derive(Debug)]
pub(super) struct Players {
    started_ms: i64,
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
    online: Option<Arc<Session>>,
    summary: Option<PlayerSummary>,
}

impl PeerPlayer {
    fn away(name: String, has_player: bool, since_ms: i64) -> Self {
        Self {
            name,
            has_player,
            since_ms,
            newest_session: 0,
            online: None,
            summary: None,
        }
    }
}

impl Players {
    pub(super) fn new(started_ms: i64, sessions: &[ritornel_engine::LinkSession]) -> Self {
        Self {
            started_ms,
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

impl Core {
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
            .filter(|p| {
                p.has_player
                    && p.online
                        .as_ref()
                        .is_some_and(|s| s.id() == session.id() && !s.is_closed())
            })
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
    use ritornel_contract::{OutputInfo, OutputMode, SessionKind, Topic};

    fn peer_session(core: &Core, peer: Id) -> Arc<Session> {
        core.open(&mut lock(&core.sessions), SessionKind::Full, Some(peer))
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
        let observer = core.open_session(SessionKind::Limited).unwrap();
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
