//! One client's session on one transport: what it may do, whether it has
//! said hello, what it is subscribed to, and what is waiting to be sent to
//! it. See "Events and subscriptions" in docs/design/contract.md.
//!
//! Everything a session is sent goes through its queue, in order: the
//! response to each call, and events. A response is never dropped. Of
//! events the queue keeps at most one unsent per topic and key, and merges
//! a newer one into it, so a client that reads slowly cannot build a
//! backlog.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::sync::{Mutex, MutexGuard, PoisonError};

use ritornel_contract::{EVENT_METHOD, Event, Frame, Notification, SessionKind, Topic};
use tokio::sync::{Notify, watch};
use tracing::error;

/// One client's session on one transport.
#[derive(Debug)]
pub(crate) struct Session {
    id: u64,
    kind: SessionKind,
    state: Mutex<State>,
    /// Wakes the one task that sends this session's frames.
    wake: Notify,
    /// Whether the session has closed, for whoever is waiting on something
    /// else and must stop when it does.
    shut: watch::Sender<bool>,
}

#[derive(Debug, Default)]
struct State {
    /// Whether `server.hello` has succeeded.
    greeted: bool,
    topics: BTreeSet<Topic>,
    /// What is waiting to be sent, oldest first.
    queue: VecDeque<Queued>,
    /// The unsent event behind each [`Queued::Event`].
    events: HashMap<(Topic, String), Event>,
    closed: bool,
}

#[derive(Debug)]
enum Queued {
    Reply(Frame),
    /// The unsent event with this topic and key. It keeps the place in the
    /// queue of the first event it was merged from.
    Event(Topic, String),
}

/// What tells one resource of a topic from another: the key the queue keeps
/// one unsent event for.
fn key(event: &Event) -> String {
    match event {
        Event::Servers(event) => event.server.server_id.to_string(),
    }
}

/// Merges a newer event into the unsent one for the same topic and key. An
/// event is a full copy, so that is keeping the copy with the higher
/// revision.
fn merge(unsent: &mut Event, newer: Event) {
    let (Event::Servers(held), Event::Servers(new)) = (&*unsent, &newer);
    if new.server.revision > held.server.revision {
        *unsent = newer;
    }
}

impl Session {
    pub(crate) fn new(id: u64, kind: SessionKind) -> Self {
        Self {
            id,
            kind,
            state: Mutex::default(),
            wake: Notify::new(),
            shut: watch::Sender::new(false),
        }
    }

    /// What names the session in the log.
    pub(crate) fn id(&self) -> u64 {
        self.id
    }

    pub(crate) fn kind(&self) -> SessionKind {
        self.kind
    }

    fn state(&self) -> MutexGuard<'_, State> {
        // Nothing here can leave the state half changed.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Records that `server.hello` succeeded.
    pub(crate) fn greet(&self) {
        self.state().greeted = true;
    }

    pub(crate) fn greeted(&self) -> bool {
        self.state().greeted
    }

    /// Adds topics, and returns every topic the session then has.
    pub(crate) fn subscribe(&self, topics: &[Topic]) -> Vec<Topic> {
        let mut state = self.state();
        state.topics.extend(topics);
        state.topics.iter().copied().collect()
    }

    /// Removes topics and drops the unsent events on them, and returns
    /// every topic the session then has. Whatever is queued after this is
    /// sent after every event on those topics that the session will ever
    /// get.
    pub(crate) fn unsubscribe(&self, topics: &[Topic]) -> Vec<Topic> {
        let mut state = self.state();
        for topic in topics {
            state.topics.remove(topic);
        }
        state.events.retain(|(topic, _), _| !topics.contains(topic));
        state
            .queue
            .retain(|queued| !matches!(queued, Queued::Event(topic, _) if topics.contains(topic)));
        state.topics.iter().copied().collect()
    }

    /// Queues an event, if the session is subscribed to its topic.
    pub(crate) fn publish(&self, event: Event) {
        let mut state = self.state();
        let topic = event.topic();
        if state.closed || !state.topics.contains(&topic) {
            return;
        }
        let key = key(&event);
        match state.events.get_mut(&(topic, key.clone())) {
            Some(unsent) => merge(unsent, event),
            None => {
                state.events.insert((topic, key.clone()), event);
                state.queue.push_back(Queued::Event(topic, key));
            }
        }
        drop(state);
        self.wake.notify_one();
    }

    /// Queues the response to a call.
    pub(crate) fn reply(&self, frame: Frame) {
        let mut state = self.state();
        if state.closed {
            return;
        }
        state.queue.push_back(Queued::Reply(frame));
        drop(state);
        self.wake.notify_one();
    }

    /// Ends the session: what is unsent is dropped, and [`next`](Self::next)
    /// says so.
    pub(crate) fn close(&self) {
        let mut state = self.state();
        state.closed = true;
        state.queue.clear();
        state.events.clear();
        drop(state);
        self.wake.notify_one();
        self.shut.send_replace(true);
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

    /// Takes the next frame to send without waiting. `Err` when the session
    /// has closed.
    fn take(&self) -> Result<Option<Frame>, Closed> {
        let mut state = self.state();
        if state.closed {
            return Err(Closed);
        }
        while let Some(queued) = state.queue.pop_front() {
            match queued {
                Queued::Reply(frame) => return Ok(Some(frame)),
                Queued::Event(topic, key) => {
                    if let Some(event) = state.events.remove(&(topic, key)) {
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
    use ritornel_contract::{
        Id, RequestId, Response, ServerInfo, ServerRole, ServersEvent, SessionKind,
    };
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
        let session = Session::new(1, SessionKind::Full);
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
        let Event::Servers(event) = serde_json::from_value(params.clone().unwrap()).unwrap();
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
        let session = Session::new(1, SessionKind::Full);
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

    #[test]
    fn a_session_starts_ungreeted_with_the_kind_it_was_given() {
        let session = Session::new(7, SessionKind::Limited);
        assert_eq!((session.id(), session.kind()), (7, SessionKind::Limited));
        assert!(!session.greeted());
        session.greet();
        assert!(session.greeted());
    }
}
