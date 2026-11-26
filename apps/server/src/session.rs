//! One client's session on one transport: what it may do, whether it has
//! said hello, what it is subscribed to, and what is waiting to be sent to
//! it. See "Events and subscriptions" in docs/internals/contract.md.
//!
//! Everything a session is sent goes through its queue, in order: the
//! response to each call, and events. A response is never dropped. Of
//! events the queue keeps at most one unsent per topic and key, and merges
//! a newer one into it, so a client that reads slowly cannot build a
//! backlog.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use ritornel_contract::{
    EVENT_METHOD, Event, Frame, Id, Notification, Permission, PermissionSet, TokenRole, Topic,
};
use ritornel_engine::View;
use tokio::sync::{Notify, watch};
use tracing::error;

use crate::views::Views;

/// What a verified credential or socket ticket grants a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SessionGrant {
    pub(crate) role: Option<TokenRole>,
    pub(crate) permissions: PermissionSet,
}

impl SessionGrant {
    /// Full local-owner access (every permission, role `owner`).
    pub(crate) const OWNER: Self = Self {
        role: Some(TokenRole::Owner),
        permissions: PermissionSet::ALL,
    };

    /// Grants the exact permission set of a named role.
    pub(crate) const fn for_role(role: TokenRole) -> Self {
        Self {
            role: Some(role),
            permissions: PermissionSet::for_role(role),
        }
    }

    /// Grants a custom permission set with an optional origin role.
    pub(crate) const fn custom(role: Option<TokenRole>, permissions: PermissionSet) -> Self {
        Self { role, permissions }
    }

    /// The named role when `permissions` still match it exactly.
    pub(crate) const fn effective_role(self) -> Option<TokenRole> {
        self.permissions.exact_role(self.role)
    }
}

/// One client's session on one transport.
#[derive(Debug)]
pub(crate) struct Session {
    id: u64,
    grant: SessionGrant,
    /// On a hub, the satellite this session is the link of.
    peer: Option<Id>,
    state: Mutex<State>,
    /// Wakes the one task that sends this session's frames.
    wake: Notify,
    /// Whether the session has closed, for whoever is waiting on something
    /// else and must stop when it does.
    shut: watch::Sender<bool>,
    /// Last input on a presence connection, or None on any other session.
    last_heard: watch::Sender<Option<tokio::time::Instant>>,
    /// The views it holds.
    views: Mutex<Views>,
    pub(crate) rewrites: Mutex<crate::dispatch::routed_source::Rewrites>,
    /// Only the uplink takes this lock, to order connection and subscription changes for this session.
    pub(crate) upstream: Mutex<()>,
}

#[derive(Debug, Default)]
struct State {
    /// Whether `server.hello` has succeeded.
    greeted: bool,
    topics: BTreeMap<Option<Id>, BTreeSet<Topic>>,
    /// What is waiting to be sent, oldest first.
    queue: VecDeque<Queued>,
    /// The unsent event behind each [`Queued::Event`].
    events: HashMap<(Option<Id>, Topic, String), Event>,
    closed: bool,
    next_request: i64,
    pending: HashMap<i64, tokio::sync::oneshot::Sender<PlayerAnswer>>,
}

type PlayerAnswer = Result<serde_json::Value, ritornel_contract::RpcError>;

/// A request to the connected player. Dropping it cancels the reply slot and
/// any request still waiting to be sent.
pub(crate) struct PlayerCall {
    session: Arc<Session>,
    id: i64,
    response: tokio::sync::oneshot::Receiver<PlayerAnswer>,
}

impl PlayerCall {
    pub(crate) async fn answer_within(
        mut self,
        within: std::time::Duration,
    ) -> Option<PlayerAnswer> {
        tokio::time::timeout(within, &mut self.response)
            .await
            .ok()?
            .ok()
    }
}

impl Drop for PlayerCall {
    fn drop(&mut self) {
        let mut state = self.session.state();
        if state.pending.remove(&self.id).is_some() {
            let id = ritornel_contract::RequestId::Number(self.id);
            state.queue.retain(|queued| match queued {
                Queued::Frame(Frame::Request(request)) => request.id != id,
                _ => true,
            });
        }
    }
}

#[derive(Debug)]
enum Queued {
    Frame(Frame),
    /// The unsent event with this topic and key. It keeps the place in the
    /// queue of the first event it was merged from.
    Event(Option<Id>, Topic, String),
}

/// What tells one resource of a topic from another: the key the queue keeps
/// one unsent event for.
fn key(event: &Event) -> String {
    match event {
        Event::Servers(event) => event.server.server_id.to_string(),
        Event::Jobs(event) => event.job.job_id.to_string(),
        // The library, the player and the queue are one resource each.
        Event::Players
        | Event::Library(_)
        | Event::Playback(_)
        | Event::Progress(_)
        | Event::Queue(_) => String::new(),
        Event::List(event) => event.view.clone(),
    }
}

/// Merges a newer event into the unsent one for the same topic and key.
///
/// A server's status and a job are full copies, so that is keeping the
/// newer copy. A library or list event is a hint: the merged one has the
/// highest revision, `reorder` if either had it, and every kind of change
/// either had, so a `reorder` is never lost behind a later change that
/// would not have reordered.
fn merge(unsent: &mut Event, newer: Event) {
    match (&mut *unsent, newer) {
        (Event::Servers(held), Event::Servers(new)) => {
            if new.server.revision > held.server.revision {
                *held = new;
            }
        }
        (Event::Jobs(held), Event::Jobs(new)) => *held = new,
        (Event::Playback(held), Event::Playback(new)) => {
            if new.state.revision >= held.state.revision {
                *held = new;
            }
        }
        (Event::Progress(held), Event::Progress(new)) => *held = new,
        (Event::Queue(held), Event::Queue(new)) => {
            held.queue_revision = held.queue_revision.max(new.queue_revision);
        }
        (Event::Library(held), Event::Library(new)) => {
            held.library_revision = held.library_revision.max(new.library_revision);
            held.changed.extend(new.changed);
            held.changed.sort_unstable();
            held.changed.dedup();
        }
        (Event::List(held), Event::List(new)) => {
            held.library_revision = held.library_revision.max(new.library_revision);
            held.reorder |= new.reorder;
        }
        // The key is per topic, so the two are always of one topic.
        _ => {}
    }
}

impl Session {
    #[cfg(test)]
    pub(crate) fn new(id: u64, grant: SessionGrant) -> Self {
        Self::linked(id, grant, None)
    }

    /// A session that is the link of the satellite `peer`, if there is one.
    pub(crate) fn linked(id: u64, grant: SessionGrant, peer: Option<Id>) -> Self {
        Self {
            last_heard: watch::Sender::new(None),
            id,
            grant,
            peer,
            state: Mutex::default(),
            wake: Notify::new(),
            shut: watch::Sender::new(false),
            views: Mutex::default(),
            rewrites: Mutex::default(),
            upstream: Mutex::default(),
        }
    }

    /// What names the session in the log.
    pub(crate) fn id(&self) -> u64 {
        self.id
    }

    pub(crate) fn grant(&self) -> SessionGrant {
        self.grant
    }

    pub(crate) fn permissions(&self) -> PermissionSet {
        self.grant.permissions
    }

    pub(crate) fn has(&self, permission: Permission) -> bool {
        self.grant.permissions.contains(permission)
    }

    pub(crate) fn has_all(&self, required: &[Permission]) -> bool {
        self.grant.permissions.contains_all(required)
    }

    pub(crate) fn has_any(&self, candidates: &[Permission]) -> bool {
        self.grant.permissions.contains_any(candidates)
    }

    /// The satellite this session is the link of: `None` for a client's.
    pub(crate) fn peer(&self) -> Option<Id> {
        self.peer
    }

    fn state(&self) -> MutexGuard<'_, State> {
        // Nothing here can leave the state half changed.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Starts the silence deadline on a satellite presence connection.
    pub(crate) fn enable_heartbeat(&self) {
        self.last_heard
            .send_replace(Some(tokio::time::Instant::now()));
    }

    /// Extends the silence deadline when a transport receives a frame.
    pub(crate) fn heard(&self) {
        self.last_heard.send_if_modified(|last| {
            if last.is_none() {
                return false;
            }
            *last = Some(tokio::time::Instant::now());
            true
        });
    }

    /// Follows the silence deadline from the transport loop.
    pub(crate) fn heartbeat(&self) -> watch::Receiver<Option<tokio::time::Instant>> {
        self.last_heard.subscribe()
    }

    /// Records that the client has said hello.
    pub(crate) fn greet(&self) {
        self.state().greeted = true;
    }

    pub(crate) fn greeted(&self) -> bool {
        self.state().greeted
    }

    /// Adds or removes only the subscriptions in the requested scope.
    pub(crate) fn change_topics(
        &self,
        player: Option<Id>,
        topics: &[Topic],
        add: bool,
    ) -> Result<Vec<Topic>, ritornel_contract::RpcError> {
        let mut state = self.state();
        if state.closed {
            return Err(ritornel_contract::RpcError::new(
                ritornel_contract::ErrorCode::Unavailable,
                "the session has closed",
            ));
        }
        if add {
            // Bound the number of distinct targets even when the hub is offline.
            if !topics.is_empty() {
                if player.is_some()
                    && !state.topics.contains_key(&player)
                    && state.topics.keys().flatten().count() >= crate::bounds::MAX_FOLLOWED_PLAYERS
                {
                    return Err(ritornel_contract::RpcError::new(
                        ritornel_contract::ErrorCode::Invalid,
                        "too many players followed on this session",
                    ));
                }
                state.topics.entry(player).or_default().extend(topics);
            }
        } else {
            if let Some(held) = state.topics.get_mut(&player) {
                held.retain(|t| !topics.contains(t));
                if held.is_empty() {
                    state.topics.remove(&player);
                }
            }
            state
                .events
                .retain(|(p, t, _), _| *p != player || !topics.contains(t));
            state.queue.retain(
                |q| !matches!(q, Queued::Event(p,t,_) if *p == player && topics.contains(t)),
            );
        }
        Ok(state
            .topics
            .get(&player)
            .into_iter()
            .flatten()
            .copied()
            .collect())
    }

    pub(crate) fn subscriptions(&self) -> BTreeMap<Option<Id>, BTreeSet<Topic>> {
        self.state().topics.clone()
    }

    #[cfg(test)]
    pub(crate) fn subscribe(&self, topics: &[Topic]) -> Vec<Topic> {
        self.change_topics(None, topics, true).unwrap()
    }

    #[cfg(test)]
    pub(crate) fn unsubscribe(&self, topics: &[Topic]) -> Vec<Topic> {
        self.change_topics(None, topics, false).unwrap()
    }

    /// Queues an event, if the session is subscribed to its topic. A
    /// session that holds a view needs no subscription for the `list`
    /// topic: holding the view is the subscription.
    pub(crate) fn publish(&self, event: Event) {
        let mut state = self.state();
        let topic = event.topic();
        let player = event.player();
        if state.closed
            || (topic != Topic::List
                && !state
                    .topics
                    .get(&player)
                    .is_some_and(|topics| topics.contains(&topic)))
        {
            return;
        }
        let key = key(&event);
        match state.events.get_mut(&(player, topic, key.clone())) {
            Some(unsent) => merge(unsent, event),
            None => {
                state.events.insert((player, topic, key.clone()), event);
                state.queue.push_back(Queued::Event(player, topic, key));
            }
        }
        drop(state);
        self.wake.notify_one();
    }

    /// Queues a hub request on this player's session, alongside ordinary responses.
    pub(crate) fn queue_player_call(
        self: &Arc<Self>,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Option<PlayerCall> {
        use ritornel_contract::{Request, RequestId};
        let mut state = self.state();
        if state.closed {
            return None;
        }
        state.next_request += 1;
        let id = state.next_request;
        let (send, response) = tokio::sync::oneshot::channel();
        state.pending.insert(id, send);
        state.queue.push_back(Queued::Frame(Frame::Request(Request {
            id: RequestId::Number(id),
            method: method.to_owned(),
            params,
        })));
        drop(state);
        self.wake.notify_one();
        Some(PlayerCall {
            session: Arc::clone(self),
            id,
            response,
        })
    }

    /// Resolves only a call issued on this session. Late replies have no slot.
    pub(crate) fn answered(&self, response: ritornel_contract::Response) {
        if let Some(ritornel_contract::RequestId::Number(id)) = response.id
            && let Some(send) = self.state().pending.remove(&id)
        {
            let _ = send.send(response.outcome);
        }
    }

    /// Queues the response to a call.
    pub(crate) fn reply(&self, frame: Frame) {
        let mut state = self.state();
        if state.closed {
            return;
        }
        state.queue.push_back(Queued::Frame(frame));
        drop(state);
        self.wake.notify_one();
    }

    fn views(&self) -> MutexGuard<'_, Views> {
        // Nothing here can leave the views half changed.
        self.views.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Holds a view under `id`, ending one the session holds if it holds
    /// as many as it may.
    pub(crate) fn hold_view(&self, id: String, resolved: Arc<View>) {
        self.views().hold(id, resolved, Instant::now());
    }

    /// The view held under `id`, if it has not ended.
    pub(crate) fn view(&self, id: &str) -> Option<Arc<View>> {
        self.views().get(id, Instant::now())
    }

    /// Ends the view held under `id`. Whether there was one.
    pub(crate) fn close_view(&self, id: &str) -> bool {
        self.views().close(id)
    }

    /// The views the session holds, by their IDs, as they were resolved.
    /// Asking is not a use: it keeps none of them from ending.
    pub(crate) fn held_views(&self) -> Vec<(String, Arc<View>)> {
        self.views().all()
    }

    /// What the session's views cost to hold, in bytes, near enough.
    pub(crate) fn view_bytes(&self) -> usize {
        self.views().bytes()
    }

    /// Ends the session: what is unsent is dropped, and [`next`](Self::next)
    /// says so.
    pub(crate) fn close(&self) {
        *self.views() = Views::default();
        let mut state = self.state();
        state.closed = true;
        state.topics.clear();
        state.queue.clear();
        state.events.clear();
        state.pending.clear();
        drop(state);
        self.wake.notify_one();
        self.shut.send_replace(true);
    }

    /// Whether the session has closed.
    pub(crate) fn is_closed(&self) -> bool {
        self.state().closed
    }

    /// Resolves once the session has closed. A transport waits on this
    /// beside whatever else it is waiting on, a write to a client that has
    /// stopped reading above all, so that closing the session ends it.
    pub(crate) async fn closed(&self) {
        // The sender is this session's own, so the channel cannot close
        // first.
        let _ = self.shut.subscribe().wait_for(|closed| *closed).await;
    }

    /// How many frames are queued and not yet taken to be sent.
    #[cfg(test)]
    pub(crate) fn unsent(&self) -> usize {
        self.state().queue.len()
    }

    /// Everything waiting to be sent, taken, for a test elsewhere in the
    /// server.
    #[cfg(test)]
    pub(crate) fn drain(&self) -> Vec<Frame> {
        std::iter::from_fn(|| self.take().ok().flatten()).collect()
    }

    /// Takes the next frame to send without waiting. `Err` when the session
    /// has closed.
    fn take(&self) -> Result<Option<Frame>, Closed> {
        let mut state = self.state();
        if state.closed {
            return Err(Closed);
        }
        while let Some(queued) = state.queue.pop_front() {
            match queued {
                Queued::Frame(frame) => return Ok(Some(frame)),
                Queued::Event(player, topic, key) => {
                    if let Some(event) = state.events.remove(&(player, topic, key)) {
                        match serde_json::to_value(&event) {
                            Ok(params) => {
                                return Ok(Some(Frame::Notification(Notification {
                                    method: EVENT_METHOD.to_owned(),
                                    params: Some(params),
                                })));
                            }
                            Err(error) => error!(%error, "an event could not be written"),
                        }
                    }
                }
            }
        }
        Ok(None)
    }

    /// The next frame to send, waiting until there is one. `None` when the
    /// session has closed. Only one task may wait at a time.
    pub(crate) async fn next(&self) -> Option<Frame> {
        loop {
            match self.take() {
                Ok(Some(frame)) => return Some(frame),
                Ok(None) => self.wake.notified().await,
                Err(Closed) => return None,
            }
        }
    }
}

/// The session has closed.
#[derive(Debug)]
struct Closed;

#[cfg(test)]
mod tests {
    use ritornel_contract::{Id, RequestId, Response, ServerInfo, ServerRole, ServersEvent};
    use serde_json::json;

    use super::*;

    fn status(server: u8, name: &str, revision: u64) -> Event {
        Event::Servers(ServersEvent {
            server: ServerInfo {
                server_id: Id::from_bytes([server; 16]),
                name: name.to_owned(),
                role: ServerRole::Unassigned,
                is_this_server: server == 1,
                reachable: true,
                revision,
            },
        })
    }

    fn reply(id: i64) -> Frame {
        Frame::Response(Response {
            id: Some(RequestId::Number(id)),
            outcome: Ok(json!({})),
        })
    }

    fn subscribed() -> Session {
        let session = Session::new(1, SessionGrant::OWNER);
        assert_eq!(session.subscribe(&[Topic::Servers]), [Topic::Servers]);
        session
    }

    /// Everything waiting to be sent, in order.
    fn drain(session: &Session) -> Vec<Frame> {
        std::iter::from_fn(|| session.take().unwrap()).collect()
    }

    /// The name and revision an event frame carries.
    fn carried(frame: &Frame) -> (String, u64) {
        let Frame::Notification(Notification { method, params }) = frame else {
            panic!("{frame:?}");
        };
        assert_eq!(method, "event");
        let Event::Servers(event) = serde_json::from_value(params.clone().unwrap()).unwrap() else {
            panic!("an event on another topic");
        };
        (event.server.name, event.server.revision)
    }

    #[test]
    fn frames_are_sent_in_the_order_they_were_queued() {
        let session = subscribed();
        session.reply(reply(1));
        session.publish(status(1, "desk", 2));
        session.reply(reply(2));

        let sent = drain(&session);
        assert_eq!(sent.len(), 3);
        assert_eq!(sent[0], reply(1));
        assert_eq!(carried(&sent[1]), ("desk".to_owned(), 2));
        assert_eq!(sent[2], reply(2));
    }

    #[test]
    fn at_most_one_unsent_event_is_kept_per_topic_and_key() {
        let session = subscribed();
        session.publish(status(1, "desk", 2));
        session.reply(reply(1));
        session.publish(status(1, "attic", 3));
        session.publish(status(1, "cellar", 4));

        // One event, holding the newest copy, in the first one's place.
        let sent = drain(&session);
        assert_eq!(sent.len(), 2);
        assert_eq!(carried(&sent[0]), ("cellar".to_owned(), 4));
        assert_eq!(sent[1], reply(1));

        // Once it is sent, the next change is a new event.
        session.publish(status(1, "loft", 5));
        assert_eq!(drain(&session).len(), 1);
    }

    #[test]
    fn merging_keeps_the_higher_revision_whichever_came_last() {
        let session = subscribed();
        session.publish(status(1, "newer", 7));
        session.publish(status(1, "older", 6));
        let sent = drain(&session);
        assert_eq!(sent.len(), 1);
        assert_eq!(carried(&sent[0]), ("newer".to_owned(), 7));
    }

    #[test]
    fn unsent_player_events_coalesce_and_unsubscribe_within_one_player_only() {
        use ritornel_contract::QueueEvent;
        let session = Session::new(1, SessionGrant::for_role(TokenRole::Player));
        let remote = Some(Id::from_bytes([5; 16]));
        let another = Some(Id::from_bytes([6; 16]));
        for player in [None, remote, another] {
            session
                .change_topics(player, &[Topic::Queue], true)
                .unwrap();
            for queue_revision in [3, 5, 2] {
                session.publish(Event::Queue(QueueEvent {
                    player,
                    queue_revision,
                }));
            }
        }
        assert_eq!(session.unsent(), 3);
        session
            .change_topics(remote, &[Topic::Queue], false)
            .unwrap();
        let events = drain(&session);
        assert_eq!(events.len(), 2);
        for (frame, player) in events.iter().zip([None, another]) {
            let Frame::Notification(note) = frame else {
                panic!("expected an event")
            };
            let event: Event = serde_json::from_value(note.params.clone().unwrap()).unwrap();
            assert_eq!(
                event,
                Event::Queue(QueueEvent {
                    player,
                    queue_revision: 5
                })
            );
        }
    }

    #[test]
    fn another_key_is_another_event() {
        let session = subscribed();
        session.publish(status(1, "desk", 2));
        session.publish(status(2, "attic", 9));
        session.publish(status(1, "desk again", 3));
        let sent: Vec<_> = drain(&session).iter().map(carried).collect();
        assert_eq!(
            sent,
            [("desk again".to_owned(), 3), ("attic".to_owned(), 9)]
        );
    }

    #[test]
    fn a_session_that_did_not_subscribe_is_sent_no_event() {
        let session = Session::new(1, SessionGrant::OWNER);
        session.publish(status(1, "desk", 2));
        assert_eq!(drain(&session), []);
    }

    #[test]
    fn unsubscribing_drops_the_unsent_events_and_keeps_the_responses() {
        let session = subscribed();
        session.publish(status(1, "desk", 2));
        session.reply(reply(1));
        assert_eq!(session.unsubscribe(&[Topic::Servers]), []);
        session.publish(status(1, "desk", 3));
        assert_eq!(drain(&session), [reply(1)]);
        // Subscribing again starts from what happens next.
        session.subscribe(&[Topic::Servers]);
        session.publish(status(1, "desk", 4));
        assert_eq!(drain(&session).len(), 1);
    }

    #[test]
    fn subscribing_twice_and_unsubscribing_what_is_not_there_change_nothing() {
        let session = subscribed();
        assert_eq!(
            session.subscribe(&[Topic::Servers, Topic::Servers]),
            [Topic::Servers]
        );
        assert_eq!(session.unsubscribe(&[]), [Topic::Servers]);
        assert_eq!(session.unsubscribe(&[Topic::Servers]), []);
        assert_eq!(session.unsubscribe(&[Topic::Servers]), []);
    }

    #[test]
    fn a_closed_session_drops_what_was_unsent_and_takes_nothing_more() {
        let session = subscribed();
        session.reply(reply(1));
        session.publish(status(1, "desk", 2));
        session.close();
        assert!(session.take().is_err());
        session.reply(reply(2));
        session.publish(status(1, "desk", 3));
        assert!(session.take().is_err());
    }

    #[tokio::test]
    async fn waiting_for_a_frame_ends_when_one_is_queued_or_the_session_closes() {
        let session = std::sync::Arc::new(subscribed());
        let waiting = tokio::spawn({
            let session = session.clone();
            async move { (session.next().await, session.next().await) }
        });
        session.reply(reply(1));
        session.close();
        // Whichever of the two the waiter saw first, it ends, and it never
        // sees a frame after the close.
        let (first, second) = waiting.await.unwrap();
        assert!(first == Some(reply(1)) || first.is_none());
        assert_eq!(second, None);
    }

    #[tokio::test]
    async fn waiting_for_the_close_ends_when_the_session_closes_and_not_before() {
        let session = std::sync::Arc::new(subscribed());
        let waiting = tokio::spawn({
            let session = session.clone();
            async move { session.closed().await }
        });
        // Frames come and go, and it is still waiting.
        session.reply(reply(1));
        assert_eq!(session.next().await, Some(reply(1)));
        tokio::task::yield_now().await;
        assert!(!waiting.is_finished());

        session.close();
        waiting.await.unwrap();
        // Whoever asks afterwards is told at once.
        session.closed().await;
    }

    fn library(revision: u64, changed: &[ritornel_contract::LibraryChange]) -> Event {
        Event::Library(ritornel_contract::LibraryEvent {
            library_revision: revision,
            changed: changed.to_vec(),
        })
    }

    fn list(view: &str, revision: u64, reorder: bool) -> Event {
        Event::List(ritornel_contract::ListEvent {
            view: view.to_owned(),
            library_revision: revision,
            reorder,
        })
    }

    fn params(frame: &Frame) -> Event {
        let Frame::Notification(Notification { params, .. }) = frame else {
            panic!("{frame:?}");
        };
        serde_json::from_value(params.clone().unwrap()).unwrap()
    }

    #[test]
    fn library_events_merge_into_the_highest_revision_and_every_kind_of_change() {
        use ritornel_contract::LibraryChange::{Membership, Metadata, Stats};
        let session = Session::new(1, SessionGrant::OWNER);
        // Not subscribed: nothing.
        session.publish(library(2, &[Metadata]));
        assert_eq!(drain(&session), []);
        session.subscribe(&[Topic::Library]);
        session.publish(library(3, &[Stats]));
        session.publish(library(5, &[Metadata]));
        session.publish(library(4, &[Membership]));
        let sent = drain(&session);
        assert_eq!(sent.len(), 1);
        assert_eq!(params(&sent[0]), library(5, &[Membership, Metadata, Stats]));
    }

    #[test]
    fn a_list_event_needs_no_subscription_and_a_reorder_is_never_lost_in_a_merge() {
        let session = Session::new(1, SessionGrant::OWNER);
        session.publish(list("a", 2, true));
        session.publish(list("b", 2, false));
        session.publish(list("a", 3, false));
        let sent: Vec<Event> = drain(&session).iter().map(params).collect();
        assert_eq!(sent, [list("a", 3, true), list("b", 2, false)]);
    }

    #[test]
    fn a_session_holds_views_until_it_closes() {
        let session = Session::new(1, SessionGrant::OWNER);
        let resolved = std::sync::Arc::new(ritornel_engine::View {
            kind: ritornel_contract::ListKind::Artists,
            sort: ritornel_contract::SortKey::Name,
            revision: 1,
            ids: vec![1, 2, 3],
            sections: Vec::new(),
        });
        session.hold_view("v".to_owned(), resolved);
        let held: Vec<String> = session.held_views().into_iter().map(|(id, _)| id).collect();
        assert_eq!(held, ["v"]);
        assert!(session.view_bytes() >= 24);
        assert!(session.view("v").is_some());
        session.close();
        assert!(session.view("v").is_none());
    }

    #[test]
    fn a_session_starts_ungreeted_with_the_grant_it_was_given() {
        let grant = SessionGrant::for_role(TokenRole::Guest);
        let session = Session::new(7, grant);
        assert_eq!((session.id(), session.grant()), (7, grant));
        assert!(!session.greeted());
        session.greet();
        assert!(session.greeted());
    }
}
