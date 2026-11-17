//! A running server: `server.db` open, and `library.db` on a hub, the
//! listener serving, and `server.json` saying where. `ritornel-server serve` runs one, and so
//! does a contract test, inside its own process.

use std::net::SocketAddr;
use std::sync::Arc;

use ritornel_contract::{Id, SessionKind};
use ritornel_engine::{Clock, Entropy, SQLITE_VERSION, ServerStore};
use tokio::sync::mpsc::{Receiver, Sender};
use tracing::{info, warn};

use crate::credential::{self, Credential};
use crate::data_dir::HeldDir;
use crate::dispatch::{Core, Stores};
use crate::jobs::Worker;
use crate::listener::{self, Listening};
use crate::playback;
use crate::server_json::ServerJson;
use crate::ticket::Tickets;
use crate::{Bounds, Error, Listen, Run, library, memory};

/// The name a new server gets. The owner renames it when there is a call to
/// do that with.
pub(crate) const DEFAULT_NAME: &str = "Ritornel server";

/// Ends a server's transports when it is dropped. After a stop there are
/// none, and this does nothing.
#[derive(Debug)]
struct Transports(Arc<Core>);

impl Drop for Transports {
    fn drop(&mut self) {
        self.0.abandon();
    }
}

/// A running server.
#[derive(Debug)]
pub struct Server {
    core: Arc<Core>,
    /// The thread that runs jobs.
    worker: Worker,
    /// The player's control loop and the delivery of listens.
    playback: playback::Workers,
    /// The thread that scrobbles.
    scrobbler: crate::lastfm::Scrobbler,
    /// The thread that watches the hub's folders.
    watcher: crate::watch::Watcher,
    transports: Transports,
    listening: Listening,
    bounds: Bounds,
    server_json: ServerJson,
    // Dropped last: the directory is let go once everything in it is
    // closed.
    _held: HeldDir,
}

impl Server {
    /// Starts a server on a data directory this process holds: opens and
    /// migrates `server.db`, makes sure the server has its identity and the
    /// credential file its token, opens `library.db` if this server is a
    /// library's hub, binds the listener, and only then writes
    /// `server.json`.
    ///
    /// It must be called on a Tokio runtime, and the listener runs on that
    /// runtime.
    pub async fn start(
        held: HeldDir,
        listen: Listen,
        clock: Arc<dyn Clock>,
        entropy: Arc<dyn Entropy>,
    ) -> Result<Self, Error> {
        Self::start_with(held, listen, clock, entropy, Bounds::default()).await
    }

    /// Starts a server as [`start`](Self::start) does, with bounds other
    /// than the usual ones. A test makes them small to reach them.
    pub async fn start_with(
        held: HeldDir,
        listen: Listen,
        clock: Arc<dyn Clock>,
        entropy: Arc<dyn Entropy>,
        bounds: Bounds,
    ) -> Result<Self, Error> {
        Self::start_run(held, listen, clock, entropy, bounds, Run::default()).await
    }

    /// Starts a server as [`start_with`](Self::start_with) does, and says
    /// how this run does its jobs: under which boot ID it claims them, and
    /// how a scan runs.
    pub async fn start_run(
        held: HeldDir,
        listen: Listen,
        clock: Arc<dyn Clock>,
        entropy: Arc<dyn Entropy>,
        bounds: Bounds,
        run: Run,
    ) -> Result<Self, Error> {
        let dir = held.dir();
        let tickets = Tickets::new(Arc::clone(&entropy), bounds.ticket_life);
        let store = ServerStore::open(&dir.server_db(), clock.clone(), entropy.clone())?;
        info!(
            sqlite = SQLITE_VERSION,
            schema_version = store.schema_version(),
            migrations_applied = store.migrations_applied(),
            "server.db is open"
        );

        let (identity, issued) = store.ensure_identity(DEFAULT_NAME)?;
        if issued.is_some() {
            info!(server_id = %identity.server_id, "first start: created this server's identity");
        }
        let (credential, verifier) = credential::ensure(&store, &dir.credential(), issued)?;
        match credential {
            Credential::Replaced => warn!(
                "the credential file was missing, open to other users or not the local credential: the old session is \
                 revoked and a new token is in the file"
            ),
            kept_or_issued => info!(credential = kept_or_issued.as_str(), "local credential"),
        }

        let (identity, library) = library::open(dir, &store, identity, &clock, &entropy)?;

        // This run holds the directory's lock, so whatever run claimed a
        // job before it is gone: each such job goes back in the queue, and
        // carries on from what it had committed.
        let boot_id = match run.boot_id {
            Some(boot_id) => boot_id,
            None => ritornel_engine::new_id(clock.now_ms(), &*entropy)?,
        };
        let requeued = store.requeue_jobs_of_other_runs(boot_id)?;
        if requeued > 0 {
            info!(requeued, "jobs that an earlier run left are queued again");
        }

        if let Some(library) = &library {
            history_rule(library)?;
            // A scrobble a dead run was sending may have arrived: it is
            // never sent again.
            let abandoned = library.abandon_sending()?;
            if abandoned > 0 {
                warn!(
                    abandoned,
                    "scrobbles an earlier run was sending are marked unknown"
                );
            }
        }
        let player = playback::restore(
            &store,
            library.as_ref(),
            run.playback,
            identity.server_id,
            boot_id,
            &clock,
            &entropy,
        )?;

        let listener = listener::bind(listen).await?;
        let stores = Stores::new(store, library);
        let core = Arc::new(Core::new(
            stores, &identity, dir, verifier, clock, entropy, bounds,
        )?);
        if let Some(player) = player {
            core.set_player(player);
        }
        core.set_lastfm(crate::lastfm::Lastfm::new(run.lastfm, dir.root()));
        core.set_watch(run.watch);
        let listening = Listening::start(Arc::clone(&core), tickets, listener, bounds)?;
        info!(endpoint = %listening.endpoint(), "listening");
        let worker = Worker::start(Arc::clone(&core), boot_id, run.scan).map_err(Error::Runtime)?;
        let playback = playback::Workers::start(Arc::clone(&core), run.playback, boot_id)
            .map_err(Error::Runtime)?;
        let scrobbler =
            crate::lastfm::Scrobbler::start(Arc::clone(&core)).map_err(Error::Runtime)?;
        let watcher = crate::watch::Watcher::start(Arc::clone(&core)).map_err(Error::Runtime)?;

        // The server answers, so a client may be told where it is.
        let server_json =
            ServerJson::publish(&dir.server_json(), listening.endpoint(), identity.server_id)?;
        Ok(Self {
            transports: Transports(Arc::clone(&core)),
            worker,
            playback,
            scrobbler,
            watcher,
            core,
            listening,
            bounds,
            server_json,
            _held: held,
        })
    }

    /// The server's ID.
    pub fn server_id(&self) -> Id {
        self.core.server_id()
    }

    /// The server's display name.
    pub fn name(&self) -> String {
        self.core.name()
    }

    /// Where the server listens: 127.0.0.1 and the port really bound.
    pub fn endpoint(&self) -> SocketAddr {
        self.listening.endpoint()
    }

    /// Opens a session on the in-memory transport and returns its two ends:
    /// where a client sends frames, as text, and where it receives them.
    /// It is for tests, which join `ritornel-client` to it.
    pub fn connect_in_memory(&self, kind: SessionKind) -> (Sender<String>, Receiver<String>) {
        memory::attach(&self.core, kind)
    }

    /// Renames the server and sends its new status to every session
    /// subscribed to the `servers` topic.
    ///
    /// No call of the contract does this yet. It is here because it is the
    /// one real change to a server's status in this milestone, and so what
    /// the `servers` topic is tested with.
    pub fn rename(&self, name: &str) -> Result<(), Error> {
        self.core.rename(name)
    }

    /// What the server shares, for a test to reach inside.
    #[cfg(test)]
    pub(crate) fn core(&self) -> &Arc<Core> {
        &self.core
    }

    /// Starts the system's media session, for a server with a player.
    /// Only `serve` does, on the main thread that then runs the loop the
    /// session needs; a server in a test has none.
    #[cfg(all(feature = "media-session", target_os = "macos"))]
    pub(crate) fn start_media_session(&self) -> Result<(), Error> {
        if self.core.has_player() {
            let session = crate::media_session::MediaSession::start(Arc::downgrade(&self.core))
                .map_err(Error::Runtime)?;
            self.core.attach_media_session(session);
        }
        Ok(())
    }

    /// Developer-only: writes to the invented library `per_second` times a
    /// second until the server stops, as a scan running would. See
    /// `ritornel_engine::invent::Churn`.
    pub fn invent_churn(&self, per_second: u32) -> Result<(), Error> {
        self.core.invent_churn(per_second)
    }

    /// Developer-only, and for tests: one write to the invented library, of
    /// this kind, now.
    pub fn invent_step(
        &self,
        kind: ritornel_engine::invent::ChurnKind,
    ) -> Result<ritornel_engine::invent::Churned, Error> {
        self.core.invent_step(kind)
    }

    /// Stops the server cleanly: it stops taking calls and closes its
    /// sockets, pauses the player, lets each job finish its step, then
    /// closes its databases, which empties their logs, then removes
    /// `server.json`.
    ///
    /// When this returns nothing of the server is running. A connection or
    /// a transport that has not ended by itself within the stop grace is
    /// ended, and waited for until it is gone. The one thing waited for
    /// without a bound is a call that has started: closing the database
    /// waits for it to finish.
    pub async fn stop(self) -> Result<(), Error> {
        self.core.close_sessions();
        self.listening.stop().await;
        self.core.end_transports(self.bounds.stop_grace).await;
        drop(self.transports);
        // The player pauses, saves where it is and closes its listen as a
        // shutdown, and what that closed is delivered. It goes before the
        // jobs, so that a quit silences the music at once.
        let playback = self.playback;
        tokio::task::spawn_blocking(move || playback.stop())
            .await
            .map_err(|failed| Error::Runtime(std::io::Error::other(failed)))?;
        // The watcher first, so that it queues no scan the worker would
        // leave behind.
        let watcher = self.watcher;
        tokio::task::spawn_blocking(move || watcher.stop())
            .await
            .map_err(|failed| Error::Runtime(std::io::Error::other(failed)))?;
        // The worker ends after the file it is reading or the write it is
        // making. A job it leaves is taken up again by the next run.
        let worker = self.worker;
        tokio::task::spawn_blocking(move || worker.stop())
            .await
            .map_err(|failed| Error::Runtime(std::io::Error::other(failed)))?;
        // The scrobbler ends after the batch it is sending.
        let scrobbler = self.scrobbler;
        tokio::task::spawn_blocking(move || scrobbler.stop())
            .await
            .map_err(|failed| Error::Runtime(std::io::Error::other(failed)))?;
        // Closing waits for calls that are using the database, and then
        // empties its log, so it is not done on an async worker either.
        let core = Arc::clone(&self.core);
        tokio::task::spawn_blocking(move || core.close_store())
            .await
            .map_err(|failed| Error::Runtime(std::io::Error::other(failed)))??;
        self.server_json.remove()
    }
}

/// Derives history again if any play was judged by a rule other than the
/// one this program has: the rule changes by rebuilding (decision 7 in
/// docs/design/history.md).
fn history_rule(library: &ritornel_engine::LibraryStore) -> Result<(), Error> {
    if !library.history_needs_rebuild().map_err(Error::Library)? {
        return Ok(());
    }
    let request = ritornel_engine::WriteRequest {
        method: "history.rebuild",
        command: None,
        client: None,
    };
    library
        .rebuild_history(request, false, |rebuilt| {
            format!("{} plays, {} changed", rebuilt.after, rebuilt.changed)
        })
        .map_err(Error::Library)
        .map(|done| info!(%done, "history is derived again under the play rule in force"))
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::{self, RecvTimeoutError};
    use std::time::{Duration, Instant};

    use ritornel_contract::{Frame, Notification, route};
    use ritornel_engine::{OsEntropy, SystemClock};
    use serde_json::{Value, json};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpSocket, TcpStream};
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    use super::*;
    use crate::session::Session;
    use crate::testing::Scratch;

    /// How long a test may take before it is failed.
    const WAIT: Duration = Duration::from_secs(60);

    /// Runs a test on a thread and a one-threaded runtime of its own, and
    /// fails it if it has not finished in [`WAIT`]. The tests here are
    /// about the runtime not being held up, and a runtime that is held up
    /// cannot fail its own test.
    fn watched<Test: Future<Output = ()>>(test: impl FnOnce() -> Test + Send + 'static) {
        let (done, finished) = mpsc::channel();
        std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(test());
            let _ = done.send(());
        });
        match finished.recv_timeout(WAIT) {
            Ok(()) => {}
            Err(RecvTimeoutError::Timeout) => {
                panic!("the runtime was held up: the test did not finish within {WAIT:?}")
            }
            Err(RecvTimeoutError::Disconnected) => panic!("the test failed on its own thread"),
        }
    }

    async fn start(scratch: &Scratch) -> Server {
        start_with(scratch, Bounds::default()).await
    }

    async fn start_with(scratch: &Scratch, bounds: Bounds) -> Server {
        Server::start_with(
            HeldDir::take(scratch.path().join("data")).unwrap(),
            Listen::on_port(0),
            Arc::new(SystemClock),
            Arc::new(OsEntropy),
            bounds,
        )
        .await
        .unwrap()
    }

    /// How many tasks are alive on this test's runtime. Before a server
    /// starts it is the test's own, and after a stop it must be that again.
    fn alive() -> usize {
        tokio::runtime::Handle::current()
            .metrics()
            .num_alive_tasks()
    }

    /// A frame of about this many bytes, for the server to send.
    fn frame(bytes: usize) -> Frame {
        Frame::Notification(Notification {
            method: "test".to_owned(),
            params: Some(Value::String("x".repeat(bytes))),
        })
    }

    /// Waits until the session's transport has taken every queued frame in
    /// hand to send it.
    async fn taken(session: &Session) {
        while session.unsent() > 0 {
            tokio::task::yield_now().await;
        }
    }

    /// The one session a server has, once its transport has opened it.
    async fn only_session(server: &Server) -> Arc<Session> {
        loop {
            if let [session] = &server.core.sessions()[..] {
                return Arc::clone(session);
            }
            tokio::task::yield_now().await;
        }
    }

    fn credential(scratch: &Scratch) -> ritornel_client::Credential {
        ritornel_client::Credential::new(
            std::fs::read_to_string(scratch.path().join("data/credential")).unwrap(),
        )
    }

    fn request(id: u32, method: &str, params: &Value) -> String {
        json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }).to_string()
    }

    fn hello(id: u32) -> String {
        let params = json!({ "name": "test", "kind": "cli", "version": "1", "contractVersion": 0 });
        request(id, "server.hello", &params)
    }

    #[test]
    #[allow(clippy::await_holding_lock)]
    fn a_call_waiting_on_the_database_holds_up_nothing_else() {
        watched(|| async {
            let scratch = Scratch::new();
            let server = start(&scratch).await;
            let (to_server, mut from_server) = server.connect_in_memory(SessionKind::Full);

            // The database is busy, as under a long write, and a call
            // arrives that has to wait for it.
            let busy = server.core.stall();
            to_server.send(hello(1)).await.unwrap();

            // The runtime's one thread is not what waits. A credential is
            // still checked and a route still answered, more than once.
            for _ in 0..3 {
                ritornel_client::health(server.endpoint(), &credential(&scratch))
                    .await
                    .unwrap();
            }
            ritornel_client::descriptor(server.endpoint())
                .await
                .unwrap();
            assert!(from_server.try_recv().is_err());

            drop(busy);
            let answer: Value = serde_json::from_str(&from_server.recv().await.unwrap()).unwrap();
            assert_eq!(answer["id"], 1);
            assert_eq!(answer["result"]["sessionKind"], "full");
            server.stop().await.unwrap();
        });
    }

    /// A grace no passing test comes near. A stop that takes this long has
    /// ended a transport by force, where closing its session should have.
    const LONG_GRACE: Duration = Duration::from_secs(30);

    #[test]
    fn a_stop_ends_a_transport_in_memory_whose_client_has_stopped_reading() {
        watched(|| async {
            let scratch = Scratch::new();
            let before = alive();
            let bounds = Bounds {
                stop_grace: LONG_GRACE,
                ..Bounds::default()
            };
            let server = start_with(&scratch, bounds).await;
            let (_to_server, from_server) = server.connect_in_memory(SessionKind::Full);
            let session = only_session(&server).await;

            // The client reads nothing. The channel holds as many frames as
            // it holds, and the transport has taken one more in hand: it is
            // waiting to send it, and nothing will let it.
            for _ in 0..=memory::WAITING {
                session.reply(frame(8));
            }
            taken(&session).await;
            assert_eq!(from_server.len(), memory::WAITING);

            let stopping = Instant::now();
            server.stop().await.unwrap();
            assert!(stopping.elapsed() < LONG_GRACE, "{:?}", stopping.elapsed());
            // The transport is gone, and so is everything else of the
            // server's.
            assert!(from_server.is_closed());
            assert_eq!(alive(), before);
        });
    }

    #[test]
    fn a_stop_ends_a_transport_over_a_socket_whose_client_has_stopped_reading() {
        watched(|| async {
            let scratch = Scratch::new();
            let before = alive();
            let bounds = Bounds {
                stop_grace: LONG_GRACE,
                ..Bounds::default()
            };
            let server = start_with(&scratch, bounds).await;

            // A client that takes in as little as the system lets it, and
            // after the upgrade reads nothing at all.
            let socket = TcpSocket::new_v4().unwrap();
            socket.set_recv_buffer_size(4096).unwrap();
            let stream = socket.connect(server.endpoint()).await.unwrap();
            let token = std::fs::read_to_string(scratch.path().join("data/credential")).unwrap();
            let mut request = format!("ws://{}{}", server.endpoint(), route::WS)
                .into_client_request()
                .unwrap();
            request.headers_mut().insert(
                "authorization",
                format!("Bearer {}", token.trim()).parse().unwrap(),
            );
            let (client, _) = tokio_tungstenite::client_async(request, stream)
                .await
                .unwrap();
            let session = only_session(&server).await;

            // One frame, larger than everything the system will hold for a
            // connection in both directions. Once the transport has taken
            // it in hand, it is waiting on a write that cannot finish.
            session.reply(frame(32 << 20));
            taken(&session).await;

            let stopping = Instant::now();
            server.stop().await.unwrap();
            assert!(stopping.elapsed() < LONG_GRACE, "{:?}", stopping.elapsed());
            assert_eq!(alive(), before);
            drop(client);
        });
    }

    #[test]
    fn a_stop_does_not_wait_for_a_connection_that_is_still_sending_its_request() {
        watched(|| async {
            let scratch = Scratch::new();
            let before = alive();
            // It would be given a minute to finish its request. A stop
            // gives it a twentieth of a second at most, and whether it is
            // closed at once or cut off then, it is gone when the stop
            // returns.
            let bounds = Bounds {
                header_wait: Duration::from_secs(60),
                stop_grace: Duration::from_millis(50),
                ..Bounds::default()
            };
            let server = start_with(&scratch, bounds).await;

            // A whole request, so the connection is one the server is
            // serving, and then half of another.
            let mut connection = TcpStream::connect(server.endpoint()).await.unwrap();
            connection
                .write_all(b"GET /v1/descriptor HTTP/1.1\r\nHost: test\r\n\r\nGET /v1/descr")
                .await
                .unwrap();
            let mut answer = [0; 15];
            connection.read_exact(&mut answer).await.unwrap();
            assert_eq!(&answer, b"HTTP/1.1 200 OK");

            server.stop().await.unwrap();
            assert_eq!(alive(), before);
            // The server's end is closed, with or without a reset.
            let mut rest = Vec::new();
            let _ = connection.read_to_end(&mut rest).await;
        });
    }

    #[test]
    fn a_server_that_is_dropped_without_a_stop_leaves_no_transport_running() {
        watched(|| async {
            let scratch = Scratch::new();
            let server = start(&scratch).await;
            let (_to_server, from_server) = server.connect_in_memory(SessionKind::Full);
            only_session(&server).await;

            drop(server);
            // The transport is told to go, and goes when it next runs.
            while !from_server.is_closed() {
                tokio::task::yield_now().await;
            }
        });
    }
}
