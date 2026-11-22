//! A satellite's link to its hub, while it runs: how it reaches the hub,
//! the connections it holds there, and what it knows of whether the hub
//! answers. See "Hub and satellites" in docs/internals/linking.md and
//! "Satellites" in docs/internals/contract.md.
//!
//! Each client session that makes a library call gets a connection of its
//! own to the hub, opened on its first such call and closed with it. Its
//! calls go through unchanged, with the client's own request IDs, so the
//! views it opens are held at the hub for it, and the hub's library, list
//! and job events come back to it alone. The server keeps one more
//! connection for its own work: what its player reads, and its listens.
//!
//! A connection is made by walking the routes in order: on each, the
//! satellite checks that the server there proves it holds the hub's pinned
//! key before it presents its link session.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
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

/// The most routes a connection tries, so one to a hub that does not
/// answer is bounded however many routes the invite named.
const MAX_ROUTES: usize = 8;

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
    /// Counts the times the hub began or stopped answering, for whoever
    /// tells this server's clients.
    changes: tokio::sync::watch::Sender<u64>,
    /// The server's own connection.
    own: Mutex<Option<Arc<Client>>>,
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
            changes: tokio::sync::watch::Sender::new(0),
            own: Mutex::default(),
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

    /// The route the hub last proved it holds its key on, while it
    /// answers there: where the link session may be sent outside a
    /// connection, for media. `None` until a connection has been made, and
    /// while the hub does not answer.
    pub(crate) fn route(&self) -> Option<SocketAddr> {
        let reach = lock(&self.reach);
        if reach.answers != Some(true) {
            return None;
        }
        reach.route.as_deref().and_then(|route| route.parse().ok())
    }

    /// What changes each time the hub begins or stops answering.
    pub(crate) fn changes(&self) -> tokio::sync::watch::Receiver<u64> {
        self.changes.subscribe()
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

    /// The hub answered: on a connection just opened to `route`, or on one
    /// already open.
    fn answered(&self, route: Option<SocketAddr>) {
        let mut reach = lock(&self.reach);
        if reach.answers != Some(true) {
            info!(hub = %self.link.hub, "the hub answers");
            reach.since_ms = Some(self.clock.now_ms());
            self.changes.send_modify(|count| *count += 1);
        }
        reach.answers = Some(true);
        if let Some(route) = route {
            reach.route = Some(route.to_string());
        }
        reach.problem = None;
        reach.failed_at_ms = None;
    }

    fn lost(&self, why: &str) {
        let mut reach = lock(&self.reach);
        if reach.answers != Some(false) {
            warn!(hub = %self.link.hub, problem = why, "the hub does not answer");
            reach.since_ms = Some(self.clock.now_ms());
            self.changes.send_modify(|count| *count += 1);
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
            .filter(|route| link::is_private_route(route.ip()))
            .take(MAX_ROUTES);
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
                    self.answered(Some(route));
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
            // An answer, even a refusal, is the hub answering.
            Ok(value) => {
                self.answered(None);
                Ok(value)
            }
            Err(ritornel_client::Error::Call(error)) => {
                self.answered(None);
                Err(error)
            }
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
                {
                    // Checked under the lock `forget` takes, so a session
                    // that closed while this connected keeps no connection.
                    let mut sessions = lock(&self.sessions);
                    if session.is_closed() {
                        return Err(RpcError::new(
                            ErrorCode::Unavailable,
                            "the session has closed",
                        ));
                    }
                    sessions.insert(session.id(), Arc::clone(&client));
                }
                self.runtime.spawn(relay(events, Arc::downgrade(session)));
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

    /// Calls the hub on the server's own connection, opening it if it is
    /// not open. It waits, so it runs where waiting is allowed.
    pub(crate) fn call(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        let mut result = None;
        for _ in 0..2 {
            let held = lock(&self.own).clone();
            let client = match held {
                Some(client) => client,
                None => {
                    let client = Arc::new(
                        self.runtime
                            .block_on(self.connect())
                            .map_err(|_| self.unreachable())?,
                    );
                    *lock(&self.own) = Some(Arc::clone(&client));
                    client
                }
            };
            let (sent, again) = self.ask(&client, method, params.clone());
            let lost =
                matches!(&sent, Err(error) if !matches!(error, ritornel_client::Error::Call(_)));
            result = Some(sent);
            if lost {
                // Only the connection that was lost: another thread may
                // have opened a new one meanwhile.
                let mut own = lock(&self.own);
                if own.as_ref().is_some_and(|held| Arc::ptr_eq(held, &client)) {
                    *own = None;
                }
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

    /// Runs `work` on the server's runtime and waits for it: for what goes
    /// to the hub other than calls, such as a master pulled.
    pub(crate) fn block_on<T>(&self, work: impl std::future::Future<Output = T>) -> T {
        self.runtime.block_on(work)
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

/// How often a satellite asks its hub whether it answers, while nothing
/// else does: often enough that its clients hear within seconds that the
/// hub is away, or back.
const CHECK_EVERY: std::time::Duration = std::time::Duration::from_secs(10);

/// Follows a satellite's hub for as long as the server runs: tells its
/// clients, with a `servers` event, each time the hub begins or stops
/// answering, and pings the hub every [`CHECK_EVERY`] so that a hub that
/// went while nothing was asked of it is found out. The ping says whether
/// this server plays, which keeps the hub awake while it does. A server
/// that is no satellite yet is looked at again until it is one.
pub(crate) async fn follow(core: std::sync::Arc<crate::dispatch::Core>) {
    loop {
        let Some(uplink) = core.uplink() else {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            continue;
        };
        let mut changes = uplink.changes();
        // What changed before this began to look is told as well.
        core.hub_changed();
        let pinging = Arc::new(AtomicBool::new(false));
        loop {
            tokio::select! {
                changed = changes.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    core.hub_changed();
                }
                () = tokio::time::sleep(CHECK_EVERY) => ping(&core, &uplink, &pinging),
            }
        }
    }
}

/// Pings the hub, saying whether this server plays, unless a ping is still
/// out. The ping may wait on the hub as long as a call does, so it runs on
/// a thread of its own: news of the hub is told while it waits, and a
/// server that stops does not wait for it.
fn ping(core: &crate::dispatch::Core, uplink: &Arc<Uplink>, pinging: &Arc<AtomicBool>) {
    if pinging.swap(true, Ordering::AcqRel) {
        return;
    }
    let params = serde_json::json!({ "playing": core.playing() });
    let (asked, out) = (Arc::clone(uplink), Arc::clone(pinging));
    let started = std::thread::Builder::new()
        .name("hub ping".to_owned())
        .spawn(move || {
            let _ = asked.call("link.ping", params);
            out.store(false, Ordering::Release);
        });
    if started.is_err() {
        pinging.store(false, Ordering::Release);
    }
}
