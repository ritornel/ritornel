//! The dispatcher: routing from contract calls to engine methods. It takes
//! a decoded frame and a session and returns the frame that answers it, and
//! knows nothing of sockets. See decision 4 in docs/design/contract.md.
//!
//! [`Core`] is what every session of a running server shares: the open
//! database, who this server is, and the list of sessions that events are
//! published to.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};
use std::time::Duration;

use ritornel_contract::methods::{
    self, EventsSubscribe, EventsUnsubscribe, Method, ServerHello, ServerStatus, ServersList,
};
use ritornel_contract::{
    CONTRACT_VERSION, Descriptor, ErrorCode, Event, Frame, FreeDisk, HelloParams, Id, Limits,
    MAX_CLIENT_TEXT, PublicDescriptor, Response, RpcError, ServerInfo, ServerRole, ServersEvent,
    SessionKind, Versions, capability,
};
use ritornel_engine::{Role, SQLITE_VERSION, ServerIdentity, ServerStore, SessionToken};
use serde_json::{Map, Value};
use tokio::task::JoinSet;
use tracing::{debug, error, info, warn};

use crate::credential::Verifier;
use crate::data_dir::DataDir;
use crate::disk;
use crate::session::Session;
use crate::transport::{self, Wire};
use crate::{Bounds, Error};

/// The largest frame the server reads, in bytes: 1 MiB, which keeps bulk
/// out of the control channel (docs/design/linking.md, decision 11).
pub(crate) const MAX_MESSAGE_BYTES: usize = 1 << 20;

/// What became of a frame a transport handed over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Received {
    /// It was read, and whatever answers it is queued.
    Taken,
    /// It was over the limit and was not read. The transport closes.
    Refused,
}

/// What every session of a running server shares.
#[derive(Debug)]
pub(crate) struct Core {
    server_id: Id,
    role: ServerRole,
    schema_version: i64,
    data_dir: PathBuf,
    logs: PathBuf,
    /// What can change about this server while it runs.
    this: Mutex<This>,
    /// `server.db`, until the server stops. A call that comes after that
    /// finds nothing here and is told the server is unavailable.
    store: RwLock<Option<ServerStore>>,
    /// What a presented credential is checked against.
    local_credential: Verifier,
    bounds: Bounds,
    sessions: Mutex<Sessions>,
}

#[derive(Debug)]
struct This {
    name: String,
    /// Counts the changes to this server's status since it started.
    revision: u64,
}

#[derive(Debug, Default)]
struct Sessions {
    next_id: u64,
    open: HashMap<u64, Arc<Session>>,
    /// The task that runs each session's transport. The server owns them,
    /// so that when it stops it can see that each has ended.
    transports: JoinSet<()>,
    /// Set when the server stops: no session opens after it.
    closed: bool,
}

fn role(role: Role) -> ServerRole {
    match role {
        Role::Unassigned => ServerRole::Unassigned,
        Role::Hub => ServerRole::Hub,
        Role::Satellite => ServerRole::Satellite,
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // Nothing done under these locks can leave what they guard half
    // changed.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Core {
    pub(crate) fn new(
        store: ServerStore,
        identity: &ServerIdentity,
        dir: &DataDir,
        local_credential: Verifier,
        bounds: Bounds,
    ) -> Self {
        Self {
            local_credential,
            bounds,
            server_id: identity.server_id,
            role: role(identity.role),
            schema_version: store.schema_version(),
            data_dir: dir.root().to_owned(),
            logs: dir.logs(),
            this: Mutex::new(This {
                name: identity.name.clone(),
                revision: 1,
            }),
            store: RwLock::new(Some(store)),
            sessions: Mutex::default(),
        }
    }

    pub(crate) fn server_id(&self) -> Id {
        self.server_id
    }

    pub(crate) fn name(&self) -> String {
        lock(&self.this).name.clone()
    }

    /// What kind of session `token` holds, if it holds one. The only
    /// credential there is yet is the local one, and it is `full`.
    ///
    /// Every credential check goes through here: the per-call session check
    /// and revocation will too. It compares against what the server loaded
    /// when it started and touches neither a file nor the database, so it
    /// is safe to call on an async worker.
    pub(crate) fn authenticate(&self, token: &SessionToken) -> Option<SessionKind> {
        self.local_credential
            .accepts(token)
            .then_some(SessionKind::Full)
    }

    /// What the server says about itself to anyone who asks.
    pub(crate) fn public_descriptor(&self) -> PublicDescriptor {
        PublicDescriptor {
            server_id: self.server_id,
            name: self.name(),
            role: self.role,
            contract_version: CONTRACT_VERSION,
            capabilities: vec![capability::EVENTS_SERVERS.to_owned()],
            limits: Limits {
                max_message_bytes: MAX_MESSAGE_BYTES as u64,
            },
        }
    }

    fn info(&self, this: &This) -> ServerInfo {
        ServerInfo {
            server_id: self.server_id,
            name: this.name.clone(),
            role: self.role,
            is_this_server: true,
            // A client that reads this has just reached it.
            reachable: true,
            revision: this.revision,
        }
    }

    /// Whether one more session may open now.
    pub(crate) fn has_room(&self) -> bool {
        let sessions = lock(&self.sessions);
        !sessions.closed && sessions.open.len() < self.bounds.max_sessions
    }

    /// Opens a session of this kind. `None` once the server is stopping,
    /// and while it has as many sessions as it takes.
    fn open(&self, sessions: &mut Sessions, kind: SessionKind) -> Option<Arc<Session>> {
        if sessions.closed || sessions.open.len() >= self.bounds.max_sessions {
            return None;
        }
        sessions.next_id += 1;
        let session = Arc::new(Session::new(sessions.next_id, kind));
        sessions.open.insert(session.id(), Arc::clone(&session));
        Some(session)
    }

    /// Opens a session with no transport, for a test to call through.
    #[cfg(test)]
    pub(crate) fn open_session(&self, kind: SessionKind) -> Option<Arc<Session>> {
        self.open(&mut lock(&self.sessions), kind)
    }

    /// The sessions that are open, oldest first.
    #[cfg(test)]
    pub(crate) fn sessions(&self) -> Vec<Arc<Session>> {
        let mut open: Vec<_> = lock(&self.sessions).open.values().cloned().collect();
        open.sort_by_key(|session| session.id());
        open
    }

    /// Opens a session of this kind and runs it over `wire`, as a task
    /// this server owns. It hands the wire back if no session may open:
    /// the server is stopping, or has as many as it takes.
    ///
    /// It must be called on the server's runtime.
    pub(crate) fn run<W: Wire + 'static>(
        self: &Arc<Self>,
        kind: SessionKind,
        mut wire: W,
    ) -> Result<(), W> {
        // One lock for the check and for the task, so that a stop sees
        // either none of this or all of it.
        let mut sessions = lock(&self.sessions);
        let Some(session) = self.open(&mut sessions, kind) else {
            return Err(wire);
        };
        // Tasks that have ended are forgotten as new ones come.
        while sessions.transports.try_join_next().is_some() {}
        let core = Arc::clone(self);
        sessions.transports.spawn(async move {
            debug!(session = session.id(), "a session opened");
            transport::pump(&core, &session, &mut wire).await;
            core.close_session(&session);
            debug!(session = session.id(), "a session closed");
        });
        Ok(())
    }

    /// Ends a session whose transport has gone.
    pub(crate) fn close_session(&self, session: &Session) {
        session.close();
        lock(&self.sessions).open.remove(&session.id());
    }

    /// Stops taking calls: every session is closed, which makes its
    /// transport stop whatever it is waiting on, and no session opens from
    /// here on.
    pub(crate) fn close_sessions(&self) {
        let mut sessions = lock(&self.sessions);
        sessions.closed = true;
        for session in sessions.open.values() {
            session.close();
        }
    }

    /// Waits for every transport to end, for at most `grace`, then ends
    /// whatever is left. When this returns no transport of this server is
    /// running. [`close_sessions`](Self::close_sessions) comes first.
    pub(crate) async fn end_transports(&self, grace: Duration) {
        let mut transports = std::mem::take(&mut lock(&self.sessions).transports);
        if !transport::end_within(&mut transports, grace).await {
            warn!("a transport did not end when its session closed, and was ended");
        }
        lock(&self.sessions).open.clear();
    }

    /// Ends every transport at once, without waiting for any. It is what
    /// is left to do for a server that is dropped and was never stopped.
    pub(crate) fn abandon(&self) {
        self.close_sessions();
        // Dropping the set ends its tasks.
        drop(std::mem::take(&mut lock(&self.sessions).transports));
    }

    /// Closes `server.db`. Whatever calls it afterwards is told the server
    /// is unavailable. This waits for a call that is using the database to
    /// finish.
    pub(crate) fn close_store(&self) -> Result<(), Error> {
        let store = self
            .store
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        Ok(store.map_or(Ok(()), ServerStore::close)?)
    }

    /// Renames the server, and sends its new status to every session that
    /// subscribed to the `servers` topic.
    ///
    /// No call of the contract reaches this yet. It is the one thing in this
    /// milestone that changes a server's status, so it is what the `servers`
    /// topic is tested with.
    pub(crate) fn rename(&self, name: &str) -> Result<(), Error> {
        // The database before the name, as a call takes them: the other
        // order could wait on a call while that call waits on this.
        let store = self.store.read().unwrap_or_else(PoisonError::into_inner);
        let store = store.as_ref().ok_or(Error::Stopped)?;
        // Held from the write to the last publish, so two renames reach
        // every session in the order they were stored.
        let mut this = lock(&self.this);
        let Some(identity) = store.rename(name)? else {
            // A core is made from an identity, so there is always one.
            return Ok(());
        };
        this.name = identity.name;
        this.revision += 1;
        // The event is a hint sent after the commit.
        let event = Event::Servers(ServersEvent {
            server: self.info(&this),
        });
        for session in lock(&self.sessions).open.values() {
            session.publish(event.clone());
        }
        Ok(())
    }

    /// Holds the database as something that is writing to it would, so a
    /// test can see what goes on working meanwhile.
    #[cfg(test)]
    pub(crate) fn stall(&self) -> std::sync::RwLockWriteGuard<'_, Option<ServerStore>> {
        self.store.write().unwrap_or_else(PoisonError::into_inner)
    }

    /// Takes one frame as text from a session's transport, and queues what
    /// answers it on the session.
    ///
    /// A frame over [`MAX_MESSAGE_BYTES`] is not read at all, and the
    /// transport must close. The limit is checked here, before anything is
    /// decoded, so that it holds on every transport. The socket also sets
    /// it on its own codec, so an oversized message is never buffered.
    pub(crate) fn receive(&self, session: &Session, text: &str) -> Received {
        if text.len() > MAX_MESSAGE_BYTES {
            debug!(
                session = session.id(),
                bytes = text.len(),
                "a frame over the limit"
            );
            return Received::Refused;
        }
        let answer = match Frame::decode(text) {
            Ok(frame) => self.dispatch(session, frame),
            Err(malformed) => Some(Frame::Response(malformed.into_response())),
        };
        if let Some(answer) = answer {
            session.reply(answer);
        }
        Received::Taken
    }

    /// Answers one decoded frame from a session.
    ///
    /// It may read the disk and the database and so may wait. A transport
    /// runs it where waiting is allowed, never on an async worker.
    pub(crate) fn dispatch(&self, session: &Session, frame: Frame) -> Option<Frame> {
        match frame {
            Frame::Request(request) => {
                // Held for the whole call: closing the database waits for
                // every call that has started, and a call that starts
                // after it finds none.
                let store = self.store.read().unwrap_or_else(PoisonError::into_inner);
                let outcome = match store.as_ref() {
                    Some(_) => self.call(session, &request.method, request.params),
                    None => Err(RpcError::new(
                        ErrorCode::Unavailable,
                        "the server is stopping",
                    )),
                };
                drop(store);
                if let Err(failed) = &outcome
                    && failed.kind() == ErrorCode::Internal
                {
                    error!(session = session.id(), error = %failed.message, "a call failed");
                }
                Some(Frame::Response(Response {
                    id: Some(request.id),
                    outcome,
                }))
            }
            // No method of this contract is called as a notification, and
            // JSON-RPC answers none. The server calls nothing, so nothing
            // is a response to it.
            Frame::Notification(_) | Frame::Response(_) => None,
        }
    }

    fn call(
        &self,
        session: &Session,
        method: &str,
        params: Option<Value>,
    ) -> Result<Value, RpcError> {
        let Some(row) = methods::method(method) else {
            return Err(RpcError::new(
                ErrorCode::Unsupported,
                "this server has no such method",
            ));
        };
        if !session.greeted() && row.name != ServerHello::NAME {
            return Err(
                RpcError::new(ErrorCode::Invalid, "the first call is server.hello")
                    .with_detail("reason", "hello_required"),
            );
        }
        if row.needs_full && session.kind() != SessionKind::Full {
            return Err(RpcError::new(
                ErrorCode::Forbidden,
                format!("{} needs a full session", row.name),
            ));
        }
        match row.name {
            ServerHello::NAME => run::<ServerHello>(params, |hello| self.hello(session, &hello)),
            ServerStatus::NAME => run::<ServerStatus>(params, |_| Ok(self.status())),
            ServersList::NAME => run::<ServersList>(params, |_| {
                Ok(ritornel_contract::ServersList {
                    servers: vec![self.info(&lock(&self.this))],
                })
            }),
            EventsSubscribe::NAME => run::<EventsSubscribe>(params, |params| {
                Ok(ritornel_contract::Subscriptions {
                    topics: session.subscribe(&params.topics),
                })
            }),
            EventsUnsubscribe::NAME => run::<EventsUnsubscribe>(params, |params| {
                Ok(ritornel_contract::Subscriptions {
                    topics: session.unsubscribe(&params.topics),
                })
            }),
            _ => Err(RpcError::new(
                ErrorCode::Internal,
                format!("{} is in the method table and has no handler", row.name),
            )),
        }
    }

    fn hello(&self, session: &Session, hello: &HelloParams) -> Result<Descriptor, RpcError> {
        if hello.contract_version != CONTRACT_VERSION {
            return Err(RpcError::new(
                ErrorCode::Unsupported,
                "this server speaks another version of the contract",
            )
            .with_detail("contractVersion", CONTRACT_VERSION));
        }
        for (field, text) in [("name", &hello.name), ("version", &hello.version)] {
            if text.is_empty() || text.chars().count() > MAX_CLIENT_TEXT {
                return Err(RpcError::new(
                    ErrorCode::Invalid,
                    format!("{field} must be 1 to {MAX_CLIENT_TEXT} characters"),
                )
                .with_detail("field", field));
            }
        }
        session.greet();
        info!(
            session = session.id(),
            client = %hello.name,
            client_kind = hello.kind.as_str(),
            client_version = %hello.version,
            "hello"
        );
        Ok(Descriptor {
            server: self.public_descriptor(),
            session_kind: session.kind(),
        })
    }

    fn status(&self) -> ritornel_contract::ServerStatus {
        ritornel_contract::ServerStatus {
            server_id: self.server_id,
            name: self.name(),
            role: self.role,
            versions: Versions {
                server: env!("CARGO_PKG_VERSION").to_owned(),
                contract: CONTRACT_VERSION,
                sqlite: SQLITE_VERSION.to_owned(),
                server_schema: self.schema_version,
            },
            free_disk: FreeDisk {
                data_bytes: disk::free_bytes(&self.data_dir),
            },
            log_path: self.logs.to_string_lossy().into_owned(),
        }
    }
}

/// Runs one method: reads its params as the type the table gives them,
/// and writes its result. The handler cannot take or return another type.
fn run<M: Method>(
    params: Option<Value>,
    handler: impl FnOnce(M::Params) -> Result<M::Result, RpcError>,
) -> Result<Value, RpcError> {
    // Params are by name. A frame may leave out an empty object.
    let params = match params {
        None => Value::Object(Map::new()),
        Some(params @ Value::Object(_)) => params,
        Some(_) => {
            return Err(RpcError::new(
                ErrorCode::Invalid,
                format!("the params of {} are an object", M::NAME),
            ));
        }
    };
    let params = serde_json::from_value(params).map_err(|error| {
        RpcError::new(
            ErrorCode::Invalid,
            format!("the params of {} are wrong: {error}", M::NAME),
        )
    })?;
    serde_json::to_value(handler(params)?).map_err(|error| {
        RpcError::new(
            ErrorCode::Internal,
            format!("the result of {} could not be written: {error}", M::NAME),
        )
    })
}

#[cfg(test)]
mod tests {
    use ritornel_contract::{METHODS, Request, RequestId, Topic};
    use ritornel_engine::{OsEntropy, SystemClock};
    use serde_json::json;

    use super::*;
    use crate::testing::Scratch;

    /// A core on a database in memory, and the directory it reports on.
    fn core() -> (Core, Scratch) {
        let scratch = Scratch::new();
        let dir = DataDir::prepare(scratch.path().join("data")).unwrap();
        let store =
            ServerStore::open_in_memory(Arc::new(SystemClock), Arc::new(OsEntropy)).unwrap();
        let (identity, issued) = store.ensure_identity("desk").unwrap();
        let (_, verifier) = crate::credential::ensure(&store, &dir.credential(), issued).unwrap();
        let bounds = Bounds {
            max_sessions: 4,
            ..Bounds::default()
        };
        (Core::new(store, &identity, &dir, verifier, bounds), scratch)
    }

    fn hello() -> Value {
        json!({ "name": "test", "kind": "cli", "version": "1.2.3", "contractVersion": CONTRACT_VERSION })
    }

    /// Calls a method and returns its result, or the contract's code and
    /// the details of its error.
    fn call(
        core: &Core,
        session: &Session,
        method: &str,
        params: Option<Value>,
    ) -> Result<Value, (ErrorCode, Value)> {
        let request = Frame::Request(Request {
            id: RequestId::Number(1),
            method: method.to_owned(),
            params,
        });
        let Some(Frame::Response(response)) = core.dispatch(session, request) else {
            panic!("a request gets a response");
        };
        assert_eq!(response.id, Some(RequestId::Number(1)));
        response
            .outcome
            .map_err(|error| (error.data.code, Value::Object(error.data.details)))
    }

    fn greeted(core: &Core, kind: SessionKind) -> Arc<Session> {
        let session = core.open_session(kind).unwrap();
        call(core, &session, "server.hello", Some(hello())).unwrap();
        session
    }

    #[test]
    fn hello_answers_with_the_descriptor_and_the_sessions_kind() {
        let (core, _scratch) = core();
        let session = core.open_session(SessionKind::Limited).unwrap();
        let descriptor = call(&core, &session, "server.hello", Some(hello())).unwrap();
        assert_eq!(
            descriptor,
            json!({
                "serverId": core.server_id().to_string(),
                "name": "desk",
                "role": "unassigned",
                "contractVersion": CONTRACT_VERSION,
                "capabilities": ["events.servers"],
                "limits": { "maxMessageBytes": 1_048_576 },
                "sessionKind": "limited",
            })
        );
        assert!(session.greeted());
    }

    #[test]
    fn every_call_but_hello_is_refused_until_hello_has_succeeded() {
        let (core, _scratch) = core();
        let session = core.open_session(SessionKind::Full).unwrap();
        for method in METHODS.iter().filter(|row| row.name != "server.hello") {
            assert_eq!(
                call(&core, &session, method.name, None),
                Err((ErrorCode::Invalid, json!({ "reason": "hello_required" }))),
                "{}",
                method.name
            );
        }
        // A hello that is refused does not count.
        let mut wrong = hello();
        wrong["name"] = json!("");
        call(&core, &session, "server.hello", Some(wrong)).unwrap_err();
        call(&core, &session, "servers.list", None).unwrap_err();

        call(&core, &session, "server.hello", Some(hello())).unwrap();
        call(&core, &session, "servers.list", None).unwrap();
        // Saying hello again is answered again.
        call(&core, &session, "server.hello", Some(hello())).unwrap();
    }

    #[test]
    fn a_client_built_for_another_contract_version_is_refused() {
        let (core, _scratch) = core();
        let session = core.open_session(SessionKind::Full).unwrap();
        let mut other = hello();
        other["contractVersion"] = json!(CONTRACT_VERSION + 1);
        assert_eq!(
            call(&core, &session, "server.hello", Some(other)),
            Err((
                ErrorCode::Unsupported,
                json!({ "contractVersion": CONTRACT_VERSION })
            ))
        );
        assert!(!session.greeted());
    }

    #[test]
    fn a_method_the_server_does_not_have_is_unsupported() {
        let (core, _scratch) = core();
        for session in [
            core.open_session(SessionKind::Full).unwrap(),
            greeted(&core, SessionKind::Full),
        ] {
            assert_eq!(
                call(&core, &session, "playback.play", None),
                Err((ErrorCode::Unsupported, json!({})))
            );
        }
    }

    #[test]
    fn params_of_the_wrong_shape_are_invalid() {
        let (core, _scratch) = core();
        let session = greeted(&core, SessionKind::Full);
        for (method, params) in [
            ("server.hello", None),
            ("server.hello", Some(json!({ "name": "test" }))),
            (
                "server.hello",
                Some(
                    json!({ "name": "test", "kind": "toaster", "version": "1", "contractVersion": 0 }),
                ),
            ),
            ("server.status", Some(json!([]))),
            ("server.status", Some(json!(null))),
            ("servers.list", Some(json!("all"))),
            ("events.subscribe", None),
            ("events.subscribe", Some(json!({ "topics": "servers" }))),
            ("events.subscribe", Some(json!({ "topics": ["weather"] }))),
            ("events.subscribe", Some(json!([["servers"]]))),
            ("events.unsubscribe", Some(json!({}))),
        ] {
            let refused = call(&core, &session, method, params.clone());
            assert!(
                matches!(refused, Err((ErrorCode::Invalid, _))),
                "{method} {params:?}: {refused:?}"
            );
        }
        // A topic the server does not have subscribes to nothing, even
        // beside one it has.
        call(
            &core,
            &session,
            "events.subscribe",
            Some(json!({ "topics": ["servers", "weather"] })),
        )
        .unwrap_err();
        assert_eq!(session.subscribe(&[]), []);
    }

    #[test]
    fn a_name_or_a_version_that_is_empty_or_too_long_is_invalid() {
        let (core, _scratch) = core();
        let session = core.open_session(SessionKind::Full).unwrap();
        for (field, text) in [
            ("name", String::new()),
            ("version", String::new()),
            ("name", "n".repeat(MAX_CLIENT_TEXT + 1)),
            ("version", "v".repeat(MAX_CLIENT_TEXT + 1)),
        ] {
            let mut wrong = hello();
            wrong[field] = json!(text);
            assert_eq!(
                call(&core, &session, "server.hello", Some(wrong)),
                Err((ErrorCode::Invalid, json!({ "field": field })))
            );
        }
        let mut longest = hello();
        longest["name"] = json!("\u{e9}".repeat(MAX_CLIENT_TEXT));
        call(&core, &session, "server.hello", Some(longest)).unwrap();
    }

    #[test]
    fn the_status_needs_a_full_session_and_the_rest_do_not() {
        let (core, _scratch) = core();
        let limited = greeted(&core, SessionKind::Limited);
        assert_eq!(
            call(&core, &limited, "server.status", None),
            Err((ErrorCode::Forbidden, json!({})))
        );
        call(&core, &limited, "servers.list", None).unwrap();
        call(
            &core,
            &limited,
            "events.subscribe",
            Some(json!({ "topics": ["servers"] })),
        )
        .unwrap();
    }

    #[test]
    fn the_status_reports_what_exists() {
        let (core, scratch) = core();
        let session = greeted(&core, SessionKind::Full);
        let status = call(&core, &session, "server.status", Some(json!({}))).unwrap();
        let free = status["freeDisk"]["dataBytes"].as_u64().unwrap();
        assert!(free > 0);
        assert_eq!(
            status,
            json!({
                "serverId": core.server_id().to_string(),
                "name": "desk",
                "role": "unassigned",
                "versions": {
                    "server": env!("CARGO_PKG_VERSION"),
                    "contract": CONTRACT_VERSION,
                    "sqlite": "3.54.1",
                    "serverSchema": 1,
                },
                "freeDisk": { "dataBytes": free },
                "logPath": scratch.path().join("data/logs").to_str().unwrap(),
            })
        );
    }

    #[test]
    fn the_list_of_servers_is_this_server() {
        let (core, _scratch) = core();
        let session = greeted(&core, SessionKind::Full);
        assert_eq!(
            call(&core, &session, "servers.list", None).unwrap(),
            json!({ "servers": [{
                "serverId": core.server_id().to_string(),
                "name": "desk",
                "role": "unassigned",
                "isThisServer": true,
                "reachable": true,
                "revision": 1,
            }] })
        );
    }

    #[test]
    fn every_method_in_the_table_has_a_handler() {
        let (core, _scratch) = core();
        let session = greeted(&core, SessionKind::Full);
        for method in METHODS {
            let outcome = call(&core, &session, method.name, None);
            assert!(
                !matches!(
                    outcome,
                    Err((ErrorCode::Internal | ErrorCode::Unsupported, _))
                ),
                "{}: {outcome:?}",
                method.name
            );
        }
    }

    #[test]
    fn a_rename_is_sent_to_the_sessions_that_subscribed_and_to_no_other() {
        let (core, _scratch) = core();
        let subscribed = greeted(&core, SessionKind::Full);
        let other = greeted(&core, SessionKind::Full);
        let subscribe = Some(json!({ "topics": ["servers"] }));
        assert_eq!(
            call(&core, &subscribed, "events.subscribe", subscribe.clone()).unwrap(),
            json!({ "topics": ["servers"] })
        );

        core.rename("attic").unwrap();
        other.reply(marker());
        let sent = now(subscribed.next()).unwrap();
        assert_eq!(
            sent.encode(),
            Frame::Notification(ritornel_contract::Notification {
                method: "event".to_owned(),
                params: Some(json!({
                    "topic": "servers",
                    "server": {
                        "serverId": core.server_id().to_string(),
                        "name": "attic",
                        "role": "unassigned",
                        "isThisServer": true,
                        "reachable": true,
                        "revision": 2,
                    },
                })),
            })
            .encode()
        );
        // The other session's next frame is the marker, not an event.
        assert_eq!(now(other.next()), Some(marker()));

        // What a call reads afterwards agrees with the event.
        let list = call(&core, &other, "servers.list", None).unwrap();
        assert_eq!(list["servers"][0]["name"], "attic");
        assert_eq!(list["servers"][0]["revision"], 2);

        assert_eq!(
            call(&core, &subscribed, "events.unsubscribe", subscribe).unwrap(),
            json!({ "topics": [] })
        );
        core.rename("cellar").unwrap();
        subscribed.reply(marker());
        assert_eq!(now(subscribed.next()), Some(marker()));
        assert_eq!(subscribed.subscribe(&[]), [Topic::Servers; 0]);
    }

    /// A frame to tell the end of what a session was sent by.
    fn marker() -> Frame {
        Frame::Notification(ritornel_contract::Notification {
            method: "marker".to_owned(),
            params: None,
        })
    }

    /// Runs a future that is ready at once.
    fn now<T>(future: impl Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(future)
    }

    #[test]
    fn text_that_is_not_a_frame_is_answered_with_an_error_and_a_notification_with_nothing() {
        let (core, _scratch) = core();
        let session = core.open_session(SessionKind::Full).unwrap();
        core.receive(&session, "[]");
        core.receive(&session, r#"{"jsonrpc":"2.0","method":"servers.list"}"#);
        core.receive(&session, r#"{"jsonrpc":"2.0","id":4,"result":{}}"#);
        core.receive(&session, "not json");
        let answers: Vec<_> = (0..2)
            .map(|_| {
                let Some(Frame::Response(response)) = now(session.next()) else {
                    panic!("an answer was queued");
                };
                assert_eq!(response.id, None);
                response.outcome.unwrap_err().code
            })
            .collect();
        assert_eq!(answers, [-32600, -32700]);
    }

    #[test]
    fn no_more_sessions_open_than_the_server_takes_until_one_closes() {
        let (core, _scratch) = core();
        let open: Vec<_> = (0..4)
            .map(|_| core.open_session(SessionKind::Full).unwrap())
            .collect();
        assert!(!core.has_room());
        assert!(core.open_session(SessionKind::Full).is_none());

        core.close_session(&open[0]);
        assert!(core.has_room());
        core.open_session(SessionKind::Limited).unwrap();
        assert!(core.open_session(SessionKind::Full).is_none());
    }

    #[test]
    fn checking_a_credential_needs_no_database() {
        let (core, scratch) = core();
        let token = std::fs::read_to_string(scratch.path().join("data/credential")).unwrap();
        let token = SessionToken::new(token.trim());
        assert_eq!(core.authenticate(&token), Some(SessionKind::Full));
        assert_eq!(core.authenticate(&SessionToken::new("anything")), None);
        assert_eq!(core.authenticate(&SessionToken::new("")), None);

        // With the database busy, and then gone, the answer is the same.
        let busy = core.stall();
        assert_eq!(core.authenticate(&token), Some(SessionKind::Full));
        drop(busy);
        core.close_store().unwrap();
        assert_eq!(core.authenticate(&token), Some(SessionKind::Full));
        assert_eq!(core.authenticate(&SessionToken::new("anything")), None);
    }

    #[test]
    fn a_frame_over_the_limit_is_refused_unread_and_one_at_the_limit_is_read() {
        let (core, _scratch) = core();
        let session = core.open_session(SessionKind::Full).unwrap();
        // Valid JSON, so only its length can be what refuses it.
        let frame = |bytes: usize| {
            let padding = bytes - r#"{"jsonrpc":"2.0","id":1,"method":""}"#.len();
            format!(
                r#"{{"jsonrpc":"2.0","id":1,"method":"{}"}}"#,
                "x".repeat(padding)
            )
        };

        assert_eq!(
            core.receive(&session, &frame(MAX_MESSAGE_BYTES + 1)),
            Received::Refused
        );
        session.reply(marker());
        assert_eq!(now(session.next()), Some(marker()));

        assert_eq!(
            core.receive(&session, &frame(MAX_MESSAGE_BYTES)),
            Received::Taken
        );
        let Some(Frame::Response(response)) = now(session.next()) else {
            panic!("an answer was queued");
        };
        assert_eq!(response.outcome.unwrap_err().kind(), ErrorCode::Unsupported);
    }

    #[test]
    fn once_the_server_stops_no_session_opens_and_the_database_is_gone() {
        let (core, _scratch) = core();
        let session = greeted(&core, SessionKind::Full);
        core.close_sessions();
        assert_eq!(now(session.next()), None);
        assert!(core.open_session(SessionKind::Full).is_none());

        core.close_session(&session);
        assert_eq!(core.sessions().len(), 0);
        core.close_store().unwrap();
        assert!(matches!(core.rename("too late"), Err(Error::Stopped)));
        // A call that comes now is told to come back, and not that it was
        // wrong.
        let (code, _) = call(&core, &session, "servers.list", None).unwrap_err();
        assert_eq!(code, ErrorCode::Unavailable);
        // Closing twice is fine.
        core.close_store().unwrap();
    }
}
