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
//! A session's connection that the hub closes, as it does when it stops,
//! is let go at once. While the session follows the hub's library or job
//! events, a new one is opened and subscribed when the hub begins to answer
//! again, or at the next check while it answers, so that the session hears
//! the hub's events again without having to call.
//!
//! A connection is made at the best address the routes give now, this
//! network before a tailnet (`link::candidates`): at each, the satellite
//! checks that the server there proves it holds the hub's pinned key
//! before it presents its link session.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use ritornel_client::contract::{
    CONTRACT_VERSION, ClientKind, ErrorCode, Event, HelloParams, HubStatus, Id, PresenceHello,
    RpcError, Topic,
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
pub(crate) const HUB_TOPICS: [Topic; 4] =
    [Topic::Library, Topic::List, Topic::Jobs, Topic::Players];

/// Whether the hub answers, as the satellite last found.
#[derive(Debug, Clone, Default)]
struct Reach {
    /// `None` until a connection has been tried.
    answers: Option<bool>,
    /// Since when it has, or has not.
    since_ms: Option<i64>,
    /// The address the last connection was made at.
    route: Option<String>,
    /// The route, as the link names it, that gave that address.
    via: Option<String>,
    /// Why the last try failed, while the hub does not answer.
    problem: Option<String>,
    /// When a connection was last tried and failed.
    failed_at_ms: Option<i64>,
}

/// How long after a failed try a call is answered at once, without trying
/// again: a client that calls ten times a second while the hub is away is
/// not kept waiting on each.
const RETRY_AFTER_MS: i64 = 2_000;

/// How long a call waits for the hub's answer: a lookup of an album's
/// discs as long as it can take
/// ([`HUB_LOOKUP_WAIT`](crate::bounds::HUB_LOOKUP_WAIT)), any other `call_wait`.
fn wait_for(method: &str, call_wait: std::time::Duration) -> std::time::Duration {
    if method == "corrections.lookup" {
        call_wait.max(crate::bounds::HUB_LOOKUP_WAIT)
    } else {
        call_wait
    }
}

/// A satellite's link to its hub.
#[derive(Debug)]
pub(crate) struct Uplink {
    link: HubLink,
    name: tokio::sync::watch::Sender<String>,
    runtime: Handle,
    clock: Arc<dyn Clock>,
    entropy: Arc<dyn Entropy>,
    /// The longest a call waits for the hub's answer:
    /// [`HUB_CALL_WAIT`](crate::bounds::HUB_CALL_WAIT), but for a test.
    call_wait: std::time::Duration,
    reach: Mutex<Reach>,
    /// Each client session's connection, by the session's ID.
    sessions: Arc<Mutex<HashMap<u64, Arc<Client>>>>,
    /// The sessions whose connection the hub closed while they followed
    /// its events: subscribed again on a new one once the hub answers.
    cut: Arc<Cut>,
    /// Counts the times the hub began or stopped answering, for whoever
    /// tells this server's clients.
    changes: tokio::sync::watch::Sender<u64>,
    /// The server's own connection.
    own: Mutex<Option<Arc<Client>>>,
    /// Once the server is stopping: when every wait on the hub ends,
    /// those already waiting too.
    stop_by: tokio::sync::watch::Sender<Option<tokio::time::Instant>>,
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
        call_wait: std::time::Duration,
    ) -> Self {
        Self {
            link,
            name: tokio::sync::watch::Sender::new(name),
            runtime,
            clock,
            entropy,
            call_wait,
            reach: Mutex::default(),
            sessions: Arc::default(),
            cut: Arc::default(),
            changes: tokio::sync::watch::Sender::new(0),
            own: Mutex::default(),
            stop_by: tokio::sync::watch::Sender::new(None),
        }
    }

    /// The server is stopping: every wait on the hub ends by `by`, and no
    /// call is made after that, so that a hub that does not answer cannot
    /// hold the stop. What was not sent waits for the next run.
    pub(crate) fn stop_by(&self, by: tokio::time::Instant) {
        self.stop_by.send_if_modified(|stop_by| {
            let first = stop_by.is_none();
            stop_by.get_or_insert(by);
            first
        });
    }

    /// Whether the time a stop gave the hub has run out.
    pub(crate) fn stopped(&self) -> bool {
        self.stop_by
            .borrow()
            .is_some_and(|by| tokio::time::Instant::now() >= by)
    }

    /// Resolves once the time a stop gave the hub has run out, and never
    /// while the server is not stopping.
    async fn stop_passed(&self) {
        let mut stop_by = self.stop_by.subscribe();
        let by = match stop_by.wait_for(Option::is_some).await {
            Ok(by) => *by,
            // The sender is this uplink's own, so it outlives the wait.
            Err(_) => None,
        };
        match by {
            Some(by) => tokio::time::sleep_until(by).await,
            None => std::future::pending().await,
        }
    }

    /// What a call is answered with once the stop's time has run out.
    fn stopping() -> RpcError {
        RpcError::new(ErrorCode::Unavailable, "the server is stopping")
    }

    /// Runs `work` on the server's runtime and waits for it: for at most
    /// `wait`, if given, and never past the stop's deadline, though the
    /// stop began after the wait did. `None` if the wait ran out first.
    /// Past the deadline `work` is not begun, so no call is sent then.
    fn wait_on<T>(
        &self,
        wait: Option<std::time::Duration>,
        work: impl std::future::Future<Output = T>,
    ) -> Option<T> {
        // The timers are made inside the runtime: a caller may be a thread
        // of the server's own, outside it, such as the outbox's.
        self.runtime.block_on(async {
            let waited = async {
                match wait {
                    Some(wait) => tokio::time::sleep(wait).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                biased;
                () = self.stop_passed() => None,
                done = work => Some(done),
                () = waited => None,
            }
        })
    }

    pub(crate) fn renamed(&self, name: &str) {
        self.name.send_if_modified(|held| {
            if held == name {
                return false;
            }
            name.clone_into(held);
            true
        });
    }

    /// Whether a connection attempt has already found the hub away.
    pub(crate) fn away(&self) -> bool {
        lock(&self.reach).answers == Some(false)
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
            route: reach.route.map(|address| match reach.via {
                Some(via) if via != address => format!("{via} ({address})"),
                _ => address,
            }),
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
    fn answered(&self, route: Option<(String, SocketAddr)>) {
        let mut reach = lock(&self.reach);
        if reach.answers != Some(true) {
            info!(hub = %self.link.hub, "the hub answers");
            reach.since_ms = Some(self.clock.now_ms());
            self.changes.send_modify(|count| *count += 1);
        }
        reach.answers = Some(true);
        if let Some((via, address)) = route {
            reach.route = Some(address.to_string());
            reach.via = Some(via);
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

    /// Opens a connection to the hub at `address`, once it has proved it
    /// holds the hub's key.
    async fn open(&self, address: SocketAddr) -> Result<Client, String> {
        link::reach(
            address,
            self.link.hub,
            &self.link.hub_public_key,
            &*self.entropy,
        )
        .await?;
        Client::connect(address, &self.credential())
            .await
            .map_err(|error| error.to_string())
    }

    async fn hello(&self, client: &Client, presence: Option<PresenceHello>) -> Result<(), String> {
        let name = self.name.borrow().clone();
        client
            .hello(&HelloParams {
                presence,
                name,
                kind: ClientKind::Server,
                version: env!("CARGO_PKG_VERSION").to_owned(),
                contract_version: CONTRACT_VERSION,
            })
            .await
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    /// Opens a connection to the hub on the first route that works, and
    /// says hello.
    async fn connect(&self, presence: Option<PresenceHello>) -> Result<Client, String> {
        {
            let reach = lock(&self.reach);
            if let (Some(failed), Some(problem)) = (reach.failed_at_ms, &reach.problem)
                && self.clock.now_ms().saturating_sub(failed) < RETRY_AFTER_MS
            {
                return Err(problem.clone());
            }
        }
        // Each route is resolved again at each dial, so a hub found by its
        // name is found wherever its address has moved, and the best way
        // to it is chosen again: this network before a tailnet, and the
        // next tried soon when one does not answer (`link::first_answer`).
        let (candidates, mut tried) = link::candidates(&self.link.routes).await;
        let ordinary = presence.is_none();
        match link::first_answer(candidates, |address| async move {
            let client = self.open(address).await?;
            if ordinary {
                self.hello(&client, None).await?;
            }
            Ok(client)
        })
        .await
        {
            Ok((via, address, client)) => {
                // Only the winning route may register a presence socket.
                if let Some(presence) = presence {
                    let hello = tokio::time::timeout(
                        link::CONNECT_WAIT,
                        self.hello(&client, Some(presence)),
                    )
                    .await;
                    if let Err(why) = hello.map_err(|e| e.to_string()).and_then(|answer| answer) {
                        self.lost(&why);
                        return Err(why);
                    }
                }
                self.answered(Some((via, address)));
                return Ok(client);
            }
            Err(failed) => tried.extend(failed),
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
            // Cut short by the stop: the hub was not given the time to
            // answer, so nothing is known of it.
            Err(_) if self.stopped() => Err(Self::stopping()),
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
            if self.stopped() {
                return Err(Self::stopping());
            }
            let client = self.session_client(session, false)?;
            let (sent, again) = self.ask(&client, method, params.clone());
            let lost =
                matches!(&sent, Err(error) if !matches!(error, ritornel_client::Error::Call(_)));
            result = Some(sent);
            if lost {
                // Only the connection that was lost: another call may
                // have opened a new one meanwhile. Its relay marks the
                // session to be subscribed again.
                let mut sessions = lock(&self.sessions);
                if sessions
                    .get(&session.id())
                    .is_some_and(|held| Arc::ptr_eq(held, &client))
                {
                    sessions.remove(&session.id());
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

    /// Makes one call on a connection and waits for its answer, for at
    /// most [`call_wait`](Self::call_wait), and never past a stop's deadline. A connection
    /// whose hub has gone without closing it, as one does when the network
    /// drops, would otherwise be waited on for good. Also says whether the
    /// call may be sent again at once on a new connection: not after a
    /// wait that ran out, which would double it.
    fn ask(
        &self,
        client: &Client,
        method: &str,
        params: Value,
    ) -> (Result<Value, ritornel_client::Error>, bool) {
        let wait = wait_for(method, self.call_wait);
        match self.wait_on(Some(wait), client.call_raw(method, params)) {
            Some(answer) => (answer, true),
            None => (
                Err(ritornel_client::Error::Transport(if self.stopped() {
                    "the server is stopping".to_owned()
                } else {
                    format!("no answer in {} seconds", wait.as_secs())
                })),
                false,
            ),
        }
    }

    /// Asks the hub for these topics on the session's connection, opening
    /// it if it is not open: a client that subscribes is waiting for the
    /// hub's events, and need not call first. The hub not answering is
    /// not an error here: the next call says so, and the session is
    /// subscribed once the hub answers.
    pub(crate) fn subscribe(&self, session: &Arc<Session>) {
        if self.session_client(session, true).is_err() {
            self.cut.mark(session);
        }
    }

    /// The session's connection, opened if it is not. A new one, or one
    /// asked to, is subscribed to the hub's topics the session has.
    fn session_client(
        &self,
        session: &Arc<Session>,
        subscribe: bool,
    ) -> Result<Arc<Client>, RpcError> {
        let held = lock(&self.sessions).get(&session.id()).cloned();
        let (client, subscribe) = match held {
            Some(client) => (client, subscribe),
            None => {
                let mut client = self
                    .wait_on(None, self.connect(None))
                    .ok_or_else(Self::stopping)?
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
                self.runtime.spawn(relay(
                    events,
                    Arc::downgrade(session),
                    Arc::downgrade(&client),
                    Arc::clone(&self.sessions),
                    Arc::clone(&self.cut),
                ));
                (client, true)
            }
        };
        // The topics as they are once connected: one unsubscribed from
        // meanwhile is not asked for.
        let topics = followed(&session.topics());
        if subscribe && !topics.is_empty() {
            let params = serde_json::json!({ "topics": topics });
            match self.ask(&client, "events.subscribe", params) {
                (Ok(_), _) => {}
                // The hub refused: asking again would be refused again.
                (Err(ritornel_client::Error::Call(error)), _) => {
                    warn!(error = %error.message, "the hub refused to send its events");
                }
                // The connection failed: it is let go, if it is still this
                // one, and the session is subscribed on another later.
                (Err(error), _) => {
                    warn!(%error, "the hub's events could not be subscribed to");
                    let mut sessions = lock(&self.sessions);
                    if sessions
                        .get(&session.id())
                        .is_some_and(|held| Arc::ptr_eq(held, &client))
                    {
                        sessions.remove(&session.id());
                    }
                    drop(sessions);
                    self.cut.mark(session);
                }
            }
        }
        Ok(client)
    }

    /// Calls the hub on the server's own connection, opening it if it is
    /// not open. It waits, so it runs where waiting is allowed.
    pub(crate) fn call(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        let mut result = None;
        for _ in 0..2 {
            if self.stopped() {
                return Err(Self::stopping());
            }
            let held = lock(&self.own).clone();
            let client = match held {
                Some(client) => client,
                None => {
                    let client = Arc::new(
                        self.wait_on(None, self.connect(None))
                            .ok_or_else(Self::stopping)?
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

    /// Runs `work` on the server's runtime and waits for it, for what goes
    /// to the hub other than calls, such as a master pulled: never past a
    /// stop's deadline. `None` if that ran out first, and `work` was
    /// dropped.
    pub(crate) fn block_on<T>(&self, work: impl std::future::Future<Output = T>) -> Option<T> {
        self.wait_on(None, work)
    }

    /// Tells the hub that the session no longer follows these topics, on
    /// its connection, if it has one: the hub stops sending what would
    /// only be dropped here.
    pub(crate) fn unsubscribe(&self, session: &Arc<Session>, topics: &[Topic]) {
        let topics = followed(topics);
        if topics.is_empty() {
            return;
        }
        if followed(&session.topics()).is_empty() {
            lock(&self.cut.sessions).remove(&session.id());
        }
        let Some(client) = lock(&self.sessions).get(&session.id()).cloned() else {
            return;
        };
        let params = serde_json::json!({ "topics": topics });
        if let (Err(error), _) = self.ask(&client, "events.unsubscribe", params) {
            warn!(%error, "the hub's events could not be unsubscribed from");
        }
    }

    /// Subscribes each session that follows the hub's events but is not
    /// subscribed at the hub: its connection closed, or none could be
    /// opened or subscribed. A connection is opened where there is none.
    /// One the hub does not answer for yet waits for the next try. It
    /// waits, so it runs where waiting is allowed.
    pub(crate) fn restore(&self) {
        let cut: Vec<Weak<Session>> = lock(&self.cut.sessions)
            .drain()
            .map(|(_, held)| held)
            .collect();
        for session in cut.iter().filter_map(Weak::upgrade) {
            // Closed, or no longer following the hub, since it was marked.
            if session.is_closed() || followed(&session.topics()).is_empty() {
                continue;
            }
            if self.session_client(&session, true).is_err() {
                self.cut.mark(&session);
            }
        }
    }

    /// Lets go of a session's connection: the session has closed.
    pub(crate) fn forget(&self, session: u64) {
        lock(&self.sessions).remove(&session);
        lock(&self.cut.sessions).remove(&session);
    }
}

/// The sessions that follow the hub's events but are not subscribed at the
/// hub. They are subscribed when the hub begins to answer again, or at the
/// next check while it answers, never at once: a hub that closes each new
/// connection is not dialled in a loop.
#[derive(Debug, Default)]
struct Cut {
    sessions: Mutex<HashMap<u64, Weak<Session>>>,
}

impl Cut {
    /// Marks `session` to be subscribed when the hub next answers, if it
    /// is open and follows the hub's events.
    fn mark(&self, session: &Arc<Session>) {
        if !session.is_closed() && !followed(&session.topics()).is_empty() {
            lock(&self.sessions).insert(session.id(), Arc::downgrade(session));
        }
    }
}

/// Of `topics`, those the hub is asked for: its library, job and player events. A
/// view's list events come on the connection that holds it, unasked.
fn followed(topics: &[Topic]) -> Vec<Topic> {
    topics
        .iter()
        .copied()
        .filter(|topic| HUB_TOPICS.contains(topic) && *topic != Topic::List)
        .collect()
}

/// Hands the hub's events on one connection to the session it is for,
/// until the connection closes or the session goes. A connection that
/// closes is let go, and a session that follows the hub's events is
/// marked to be subscribed again on a new one.
async fn relay(
    mut events: tokio::sync::mpsc::UnboundedReceiver<Event>,
    session: Weak<Session>,
    client: Weak<Client>,
    sessions: Arc<Mutex<HashMap<u64, Arc<Client>>>>,
    cut: Arc<Cut>,
) {
    while let Some(event) = events.recv().await {
        let Some(session) = session.upgrade() else {
            return;
        };
        if HUB_TOPICS.contains(&event.topic()) {
            session.publish(event);
        }
    }
    let Some(session) = session.upgrade() else {
        return;
    };
    {
        // Let go if it is still held. Another in its place was opened, and
        // subscribed, by a call meanwhile; none means a call let this one
        // go and could not open another.
        let mut sessions = lock(&sessions);
        if let Some(held) = sessions.get(&session.id()) {
            if !client
                .upgrade()
                .is_some_and(|ours| Arc::ptr_eq(held, &ours))
            {
                return;
            }
            sessions.remove(&session.id());
        }
    }
    cut.mark(&session);
}

/// How often a satellite asks its hub whether it answers, while nothing
/// else does: often enough that its clients hear within seconds that the
/// hub is away, or back.
const CHECK_EVERY: std::time::Duration = std::time::Duration::from_secs(10);

/// Maintains satellite presence and restores subscriptions when the hub returns.
/// A server can join a library after startup, so wait until it has an uplink.
pub(crate) async fn follow(core: std::sync::Arc<crate::dispatch::Core>) {
    loop {
        let Some(uplink) = core.uplink() else {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            continue;
        };
        let mut changes = uplink.changes();
        // What changed before this began to look is told as well.
        core.hub_changed();
        let monitor = async {
            loop {
                tokio::select! {
                    changed = changes.changed() => {
                        if changed.is_err() { return; }
                        // Subscribe before telling clients the hub is back.
                        restore(&uplink).await;
                        core.hub_changed();
                    }
                    () = tokio::time::sleep(CHECK_EVERY) => restore(&uplink).await,
                }
            }
        };
        tokio::select! {
            () = monitor => (),
            () = presence(&core, &uplink) => return,
        }
    }
}

/// Subscribes again, on a thread where waiting is allowed, the sessions
/// whose connection the hub closed, if there are any and the hub answers
/// as last found: a hub that does not is tried by the ping alone.
async fn restore(uplink: &Arc<Uplink>) {
    let answers = lock(&uplink.reach).answers == Some(true);
    if !answers || lock(&uplink.cut.sessions).is_empty() || uplink.stopped() {
        return;
    }
    let uplink = Arc::clone(uplink);
    let _ = tokio::task::spawn_blocking(move || uplink.restore()).await;
}

/// Owns the satellite's presence socket through reconnects. A separate socket
/// carries library reads so a slow query cannot delay a heartbeat.
async fn presence(core: &crate::dispatch::Core, uplink: &Uplink) {
    let run = async {
        let mut backoff = std::time::Duration::from_millis(250);
        let mut name = uplink.name.subscribe();
        loop {
            name.borrow_and_update();
            let mut updates = core.player_updates();
            let player = updates.borrow().as_ref().map(|(_, state)| state.clone());
            let hello = PresenceHello {
                has_player: player.is_some(),
                player,
                // Only a library hub sends Now Playing in this version.
                sends_now_playing: false,
            };
            if let Ok(mut client) = uplink.connect(Some(hello)).await {
                backoff = std::time::Duration::from_millis(250);
                let mut events = client.take_events();
                loop {
                    let player = updates
                        .borrow_and_update()
                        .as_ref()
                        .map(|(_, state)| state.clone());
                    let params = serde_json::json!({"playing":core.playing(), "player":player});
                    match tokio::time::timeout(
                        uplink.call_wait,
                        client.call_raw("link.ping", params),
                    )
                    .await
                    {
                        Ok(Ok(_)) => uplink.answered(None),
                        Ok(Err(ritornel_client::Error::Call(_))) => {
                            // Even a refusal is an answer. Register again on a fresh socket.
                            uplink.answered(None);
                            break;
                        }
                        _ => {
                            uplink.lost("the presence ping was not answered");
                            break;
                        }
                    }
                    tokio::select! {
                        _ = name.changed() => {
                            // A hello on the live link updates the name without interrupting playback presence.
                            if !matches!(tokio::time::timeout(uplink.call_wait, uplink.hello(&client, None)).await, Ok(Ok(()))) {
                                break;
                            }
                        }
                        _ = events.recv() => {
                            uplink.lost("the presence connection closed");
                            break;
                        }
                        _ = updates.changed() => (),
                        () = tokio::time::sleep(CHECK_EVERY) => (),
                    }
                }
            }
            tokio::time::sleep(backoff).await;
            backoff = (2 * backoff).min(std::time::Duration::from_secs(5));
        }
    };
    tokio::select! {
        () = uplink.stop_passed() => (),
        () = run => (),
    }
}

#[cfg(test)]
mod wait_tests {
    use std::time::Duration;

    #[test]
    fn a_lookup_waits_as_long_as_it_can_take_and_any_other_call_the_usual_wait() {
        let usual = Duration::from_secs(8);
        assert_eq!(super::wait_for("library.status", usual), usual);
        assert_eq!(
            super::wait_for("corrections.lookup", usual),
            crate::bounds::HUB_LOOKUP_WAIT
        );
    }
}
