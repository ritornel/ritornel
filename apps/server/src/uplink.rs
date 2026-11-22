//! A satellite's link to its hub, while it runs: how it reaches the hub,
//! the connections it holds there, and what it knows of whether the hub
//! answers. See "The link between the hub and a satellite" and "When the
//! hub is unreachable" in docs/design/linking.md.
//!
//! Each client session that makes a library call gets a connection of its
//! own to the hub, opened on its first such call and closed with it. Its
//! calls go through unchanged, with the client's own request IDs, so the
//! views it opens are held at the hub for it, and the hub's library, list
//! and job events come back to it alone.
//!
//! A connection is made by walking the routes in order: on each, the
//! satellite checks that the server there proves it holds the hub's pinned
//! key before it presents its link session.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use ritornel_client::contract::{
    CONTRACT_VERSION, ClientKind, ErrorCode, Event, HelloParams, HubStatus, Id, RpcError, Topic,
};
use ritornel_client::{Client, Credential};
use ritornel_engine::{Clock, Entropy, HubLink};
use serde_json::Value;
use tokio::runtime::Handle;
use tracing::{info, warn};

use crate::link;
use crate::session::Session;

/// The topics whose events come from the hub. The rest are this server's
/// own: its player, its queue, and its status.
pub(crate) const HUB_TOPICS: [Topic; 3] = [Topic::Library, Topic::List, Topic::Jobs];

/// Whether the hub answers, as the satellite last found.
#[derive(Debug, Clone, Default)]
struct Reach {
    /// `None` until a connection has been tried.
    answers: Option<bool>,
    /// Since when it has, or has not.
    since_ms: Option<i64>,
    /// The route the last connection was made on.
    route: Option<String>,
    /// Why the last try failed, while the hub does not answer.
    problem: Option<String>,
    /// When a connection was last tried and failed.
    failed_at_ms: Option<i64>,
}

/// The longest a call waits for the hub's answer. The longest call a
/// client makes through a satellite, such as a history page, takes a
/// fraction of this on a hub, and the satellite must say the hub is away
/// before its own clients give up on it: `ritornelctl` waits ten seconds.
const CALL_WAIT: std::time::Duration = std::time::Duration::from_secs(8);

/// The longest a connection to the hub may take on one route, the proof
/// and the hello included. On a home network it takes milliseconds.
const CONNECT_WAIT: std::time::Duration = std::time::Duration::from_secs(4);

/// How long after a failed try a call is answered at once, without trying
/// again: a client that calls ten times a second while the hub is away is
/// not kept waiting on each.
const RETRY_AFTER_MS: i64 = 2_000;

/// A satellite's link to its hub.
#[derive(Debug)]
pub(crate) struct Uplink {
    link: HubLink,
    name: String,
    runtime: Handle,
    clock: Arc<dyn Clock>,
    entropy: Arc<dyn Entropy>,
    reach: Mutex<Reach>,
    /// Each client session's connection, by the session's ID.
    sessions: Mutex<HashMap<u64, Arc<Client>>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Uplink {
    pub(crate) fn new(
        link: HubLink,
        name: String,
        runtime: Handle,
        clock: Arc<dyn Clock>,
        entropy: Arc<dyn Entropy>,
    ) -> Self {
        Self {
            link,
            name,
            runtime,
            clock,
            entropy,
            reach: Mutex::default(),
            sessions: Mutex::default(),
        }
    }

    /// The library the hub holds.
    pub(crate) fn library_id(&self) -> Id {
        self.link.library_id
    }

    /// The link session's token, for the media routes.
    pub(crate) fn credential(&self) -> Credential {
        Credential::new(self.link.token.expose())
    }

    /// The route the last connection was made on, or else the first.
    pub(crate) fn route(&self) -> Option<SocketAddr> {
        lock(&self.reach)
            .route
            .as_deref()
            .or(self.link.routes.first().map(String::as_str))
            .and_then(|route| route.parse().ok())
    }

    /// The hub, as `link.status` and `server.status` show it.
    pub(crate) fn status(&self) -> HubStatus {
        let reach = lock(&self.reach).clone();
        HubStatus {
            server_id: self.link.hub,
            name: self.link.hub_name.clone(),
            library_id: self.link.library_id,
            epoch: self.link.hub_epoch,
            routes: self.link.routes.clone(),
            linked_at_ms: self.link.linked_at_ms,
            answers: reach.answers,
            since_ms: reach.since_ms,
            route: reach.route,
            problem: reach.problem,
        }
    }

    /// What a library call is answered with while the hub does not: what
    /// is wrong, since when, and what still works.
    pub(crate) fn unreachable(&self) -> RpcError {
        let reach = lock(&self.reach).clone();
        let mut error = RpcError::new(
            ErrorCode::Unavailable,
            format!(
                "{} does not answer, so the library cannot be read from here. What is playing \
                 plays on, and listens wait on this server until it is back.",
                self.link.hub_name
            ),
        )
        .with_detail("reason", "hub_unreachable")
        .with_detail("hub", self.link.hub_name.clone());
        if let Some(since) = reach.since_ms {
            error = error.with_detail("sinceMs", since);
        }
        if let Some(problem) = reach.problem {
            error = error.with_detail("problem", problem);
        }
        error
    }

    fn answered(&self, route: SocketAddr) {
        let mut reach = lock(&self.reach);
        if reach.answers != Some(true) {
            info!(hub = %self.link.hub, %route, "the hub answers");
            reach.since_ms = Some(self.clock.now_ms());
        }
        reach.answers = Some(true);
        reach.route = Some(route.to_string());
        reach.problem = None;
        reach.failed_at_ms = None;
    }

    fn lost(&self, why: &str) {
        let mut reach = lock(&self.reach);
        if reach.answers != Some(false) {
            warn!(hub = %self.link.hub, problem = why, "the hub does not answer");
            reach.since_ms = Some(self.clock.now_ms());
        }
        reach.answers = Some(false);
        reach.problem = Some(why.to_owned());
    }

    /// Opens a connection to the hub on the first route that works, and
    /// says hello.
    async fn connect(&self) -> Result<Client, String> {
        {
            let reach = lock(&self.reach);
            if let (Some(failed), Some(problem)) = (reach.failed_at_ms, &reach.problem)
                && self.clock.now_ms().saturating_sub(failed) < RETRY_AFTER_MS
            {
                return Err(problem.clone());
            }
        }
        let mut tried = Vec::new();
        let routes = self
            .link
            .routes
            .iter()
            .filter_map(|route| route.parse::<SocketAddr>().ok())
            .filter(|route| link::is_private_route(route.ip()));
        for route in routes {
            let opened = async {
                link::reach(
                    route,
                    self.link.hub,
                    &self.link.hub_public_key,
                    &*self.entropy,
                )
                .await?;
                let client = Client::connect(route, &self.credential())
                    .await
                    .map_err(|error| error.to_string())?;
                client
                    .hello(&HelloParams {
                        name: self.name.clone(),
                        kind: ClientKind::Server,
                        version: env!("CARGO_PKG_VERSION").to_owned(),
                        contract_version: CONTRACT_VERSION,
                    })
                    .await
                    .map_err(|error| error.to_string())?;
                Ok::<_, String>(client)
            };
            let opened = match tokio::time::timeout(CONNECT_WAIT, opened).await {
                Ok(opened) => opened,
                Err(_) => Err(format!("no answer in {} seconds", CONNECT_WAIT.as_secs())),
            };
            match opened {
                Ok(client) => {
                    self.answered(route);
                    return Ok(client);
                }
                Err(why) => tried.push(format!("{route}: {why}")),
            }
        }
        let why = if tried.is_empty() {
            "the link has no route to the hub".to_owned()
        } else {
            tried.join("; ")
        };
        self.lost(&why);
        lock(&self.reach).failed_at_ms = Some(self.clock.now_ms());
        Err(why)
    }

    /// What became of a call: the hub's own answer, or the hub lost.
    fn answer(&self, result: Result<Value, ritornel_client::Error>) -> Result<Value, RpcError> {
        match result {
            Ok(value) => Ok(value),
            Err(ritornel_client::Error::Call(error)) => Err(error),
            Err(other) => {
                self.lost(&other.to_string());
                Err(self.unreachable())
            }
        }
    }

    /// Sends a client's library call to the hub, unchanged, on the
    /// session's own connection, and returns the hub's answer. It waits,
    /// so it runs where waiting is allowed.
    pub(crate) fn forward(
        &self,
        session: &Arc<Session>,
        method: &str,
        params: Option<Value>,
    ) -> Result<Value, RpcError> {
        let params = params.unwrap_or(Value::Object(serde_json::Map::new()));
        // A connection that was lost is found out by using it. The call
        // is sent once more on a new one: a command carries its
        // idempotency key, so the hub never carries it out twice, and a
        // view that went with the old connection is reported expired.
        let mut result = None;
        for _ in 0..2 {
            let client = self.session_client(session, false)?;
            let (sent, again) = self.ask(&client, method, params.clone());
            let lost =
                matches!(&sent, Err(error) if !matches!(error, ritornel_client::Error::Call(_)));
            result = Some(sent);
            if lost {
                lock(&self.sessions).remove(&session.id());
            }
            if !(lost && again) {
                break;
            }
        }
        match result {
            Some(result) => self.answer(result),
            None => Err(self.unreachable()),
        }
    }

    /// Makes one call on a connection and waits for its answer, for at
    /// most [`CALL_WAIT`]. A connection whose hub has gone without closing
    /// it, as one does when the network drops, would otherwise be waited on
    /// for good. Also says whether the call may be sent again at once on a
    /// new connection: not after a wait that ran out, which would double it.
    fn ask(
        &self,
        client: &Client,
        method: &str,
        params: Value,
    ) -> (Result<Value, ritornel_client::Error>, bool) {
        // The timer is made inside the runtime: a caller may be a thread
        // of the server's own, outside it, such as the outbox's.
        let asked = self.runtime.block_on(async {
            tokio::time::timeout(CALL_WAIT, client.call_raw(method, params)).await
        });
        match asked {
            Ok(answer) => (answer, true),
            Err(_) => (
                Err(ritornel_client::Error::Transport(format!(
                    "no answer in {} seconds",
                    CALL_WAIT.as_secs()
                ))),
                false,
            ),
        }
    }

    /// Asks the hub for these topics on the session's connection, opening
    /// it if it is not open: a client that subscribes is waiting for the
    /// hub's events, and need not call first. The hub not answering is
    /// not an error here: the next call says so.
    pub(crate) fn subscribe(&self, session: &Arc<Session>) {
        let _ = self.session_client(session, true);
    }

    /// The session's connection, opened if it is not. A new one, or one
    /// asked to, is subscribed to the hub's topics the session has.
    fn session_client(
        &self,
        session: &Arc<Session>,
        subscribe: bool,
    ) -> Result<Arc<Client>, RpcError> {
        let held = lock(&self.sessions).get(&session.id()).cloned();
        let topics: Vec<Topic> = session
            .topics()
            .into_iter()
            .filter(|topic| HUB_TOPICS.contains(topic) && *topic != Topic::List)
            .collect();
        let (client, subscribe) = match held {
            Some(client) => (client, subscribe),
            None => {
                let mut client = self
                    .runtime
                    .block_on(self.connect())
                    .map_err(|_| self.unreachable())?;
                let events = client.take_events();
                let client = Arc::new(client);
                self.runtime.spawn(relay(events, Arc::downgrade(session)));
                lock(&self.sessions).insert(session.id(), Arc::clone(&client));
                (client, true)
            }
        };
        if subscribe && !topics.is_empty() {
            let params = serde_json::json!({ "topics": topics });
            if let (Err(error), _) = self.ask(&client, "events.subscribe", params) {
                warn!(%error, "the hub's events could not be subscribed to");
            }
        }
        Ok(client)
    }

    /// Lets go of a session's connection: the session has closed.
    pub(crate) fn forget(&self, session: u64) {
        lock(&self.sessions).remove(&session);
    }
}

/// Hands the hub's events on one connection to the session it is for,
/// until the connection closes or the session goes.
async fn relay(mut events: tokio::sync::mpsc::UnboundedReceiver<Event>, session: Weak<Session>) {
    while let Some(event) = events.recv().await {
        let Some(session) = session.upgrade() else {
            return;
        };
        if HUB_TOPICS.contains(&event.topic()) {
            session.publish(event);
        }
    }
}
