//! A running server: `server.db` open, and `library.db` on a hub, the
//! listener serving, and `server.json` saying where. `ritornel-server serve` runs one, and so
//! does a contract test, inside its own process.

use std::net::SocketAddr;
use std::sync::Arc;

use ritornel_contract::local::Preparing;
use ritornel_contract::{Id, SessionKind};
use ritornel_engine::transcode::{Encoder, EncoderId, LameEncoder, OpusEncoder};
use ritornel_engine::{Clock, Entropy, SQLITE_VERSION, ServerStore};
use tokio::sync::mpsc::{Receiver, Sender};
use tracing::{info, warn};

use crate::credential::{self, Credential};
use crate::data_dir::HeldDir;
use crate::dispatch::{Core, Stores};
use crate::jobs::Worker;
use crate::listener::{self, Listening};
use crate::playback;
use crate::preparing::Steps;
use crate::server_json::ServerJson;
use crate::ticket::Tickets;
use crate::{Bounds, Error, Listen, Run, library, memory};

async fn probe_encoders() -> Vec<EncoderId> {
    let (lame, opus) = tokio::join!(
        tokio::task::spawn_blocking(|| LameEncoder.probe()),
        tokio::task::spawn_blocking(|| OpusEncoder.probe())
    );
    [("lame", lame), ("opusenc", opus)]
        .into_iter()
        .filter_map(|(family, result)| {
            match result
                .map_err(std::io::Error::other)
                .and_then(|probe| probe)
            {
                Ok(id) => id,
                Err(error) => {
                    warn!(family, %error, "audio encoder probe failed");
                    None
                }
            }
        })
        .collect()
}

/// The name a new server gets. The owner renames it when there is a call to
/// do that with.
pub(crate) const DEFAULT_NAME: &str = "Ritornel server";

/// Whether a server called `current` takes its computer's name, `computer`:
/// when it still has the first-start name, or the name an older build took
/// from the computer, `legacy`, such as `STUDIO-PC` from `COMPUTERNAME` for
/// a Windows machine set up as `Studio-PC`. Any other name was chosen, and
/// is kept; one chosen to equal the old name exactly cannot be told apart.
fn takes_computer_name(current: &str, computer: &str, legacy: Option<&str>) -> bool {
    current == DEFAULT_NAME || (legacy == Some(current) && current != computer)
}

/// Ends a server's transports when it is dropped. After a stop there are
/// none, and this does nothing.
#[derive(Debug)]
struct Transports(Arc<Core>);

impl Drop for Transports {
    fn drop(&mut self) {
        self.0.abandon();
    }
}

/// A task of the server's own, ended when the server stops or is dropped,
/// so that it never outlives the server it holds.
#[derive(Debug)]
struct Task(tokio::task::JoinHandle<()>);

impl Drop for Task {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// A running server.
#[derive(Debug)]
pub struct Server {
    core: Arc<Core>,
    encoders: Vec<EncoderId>,
    /// The thread that runs jobs.
    worker: Worker,
    /// The player's control loop and the delivery of listens.
    playback: playback::Workers,
    /// The thread that scrobbles.
    scrobbler: crate::lastfm::Scrobbler,
    /// The thread that watches the hub's folders.
    watcher: crate::watch::Watcher,
    /// What follows a satellite's hub and tells its clients when the hub
    /// is away, or back.
    hub: Task,
    /// What lets a hub's machine sleep again once no linked server is
    /// active.
    awake: Task,
    /// Removes the staged imports that have expired.
    sweep: Task,
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
        // What takes more than a moment before the server listens is said
        // in `preparing.json`, until `server.json` replaces it.
        let steps = Steps::new(&dir.preparing_json());
        // Nothing is migrated without a copy of it.
        let started_ms = clock.now_ms();
        let before =
            crate::backup::before_migrating(dir, run.backup_dir.as_deref(), started_ms, &|step| {
                steps.say(step)
            })?;
        let store = ServerStore::open(&dir.server_db(), clock.clone(), entropy.clone())?;
        crate::dispatch::record_backup(
            &store,
            started_ms,
            before.taken.as_ref(),
            before.failed.as_deref(),
        );
        info!(
            sqlite = SQLITE_VERSION,
            schema_version = store.schema_version(),
            migrations_applied = store.migrations_applied(),
            "server.db is open"
        );

        let first_name = run.name.as_deref().unwrap_or(DEFAULT_NAME);
        let (mut identity, issued) = store.ensure_identity(first_name)?;
        if issued.is_some() {
            info!(server_id = %identity.server_id, "first start: created this server's identity");
        }
        // A name given in the settings wins over the stored one at every
        // start, so a machine nobody sits at is called what its settings
        // say. Without one, a server is named after its computer while it
        // still has the default name or the name an older build took from
        // the computer (`takes_computer_name`), so that a hub's satellites
        // and a satellite's hub say which machine each is; a name someone
        // chose is kept.
        if let Some(name) = run.name.as_deref() {
            if name != identity.name
                && let Some(renamed) = store.rename(name)?
            {
                info!(name, "renamed by the name setting");
                identity = renamed;
            }
        } else if let Some(name) = run.computer_name.as_deref()
            && name != identity.name
            && takes_computer_name(&identity.name, name, run.legacy_name.as_deref())
            && let Some(renamed) = store.rename(name)?
        {
            info!(name = %renamed.name, "this server is named after its computer");
            identity = renamed;
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
        // A hub renamed at start, as after its computer's name, keeps the
        // library's registry of servers in step, and one a crash or a
        // failed write left behind catches up.
        if let Some(library) = &library
            && let Err(error) = library::keep_registry_name(library, &identity)
        {
            warn!(%error, "the library's registry of servers could not be kept in step with this server's name");
        }

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
            history_rule(library, &steps)?;
            corrections_rule(library)?;
            // A scrobble a dead run was sending may have arrived: it is
            // never sent again.
            let abandoned = library.abandon_sending()?;
            if abandoned > 0 {
                warn!(
                    abandoned,
                    "scrobbles an earlier run was sending are marked unknown"
                );
            }
            // A library put back from a backup: what Last.fm took after it
            // is not known until an import has run.
            if let Some(held) = library.opened_restored()? {
                info!(
                    backup_taken_at = %crate::logs::utc(held.taken_at_ms),
                    "the library was restored from a backup: nothing is scrobbled until an \
                     import from Last.fm has run"
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
        core.follow_awake_at_start(run.keep_awake);
        core.set_lastfm(crate::lastfm::Lastfm::new(run.lastfm, dir.root()));
        if let Some(catalogue) = crate::dispatch::catalogue(run.musicbrainz_url.as_deref()) {
            core.set_catalogue(catalogue);
        }
        core.set_watch(run.watch);
        core.set_backups(crate::backup::Backups::new(run.backup_dir));
        core.set_cache_bytes(run.cache_bytes);
        core.set_audio_pace(run.audio_pace);
        core.set_start_up(crate::dispatch::StartUp {
            listen,
            log_level: run.log_level.unwrap_or(tracing::Level::INFO),
            origins: run.origins,
        });
        let encoders = probe_encoders().await;
        let listening = Listening::start(Arc::clone(&core), tickets, listener, listen, bounds)?;
        info!(endpoint = %listening.endpoint(), "listening");
        if listen.is_private() {
            core.set_private_port(listening.endpoint().port());
        }
        if listen.is_private() {
            warn!(
                "listening on the private network: what is sent is plain HTTP, which anyone on \
                 the same network can read"
            );
        }
        let worker = Worker::start(Arc::clone(&core), boot_id, run.scan).map_err(Error::Runtime)?;
        let playback = playback::Workers::start(Arc::clone(&core), run.playback, boot_id)
            .map_err(Error::Runtime)?;
        let scrobbler =
            crate::lastfm::Scrobbler::start(Arc::clone(&core)).map_err(Error::Runtime)?;
        let watcher = crate::watch::Watcher::start(Arc::clone(&core)).map_err(Error::Runtime)?;
        let hub = Task(tokio::spawn(crate::uplink::follow(Arc::clone(&core))));
        let awake = Task(tokio::spawn(crate::awake::check(Arc::clone(&core))));
        let sweep = Task(tokio::spawn(crate::imports::sweep(Arc::clone(&core))));

        // The server answers, so a client may be told where it is.
        let server_json = ServerJson::publish(
            &dir.server_json(),
            listening.local_endpoint(),
            identity.server_id,
        )?;
        drop(steps);
        Ok(Self {
            encoders,
            transports: Transports(Arc::clone(&core)),
            worker,
            playback,
            scrobbler,
            watcher,
            hub,
            awake,
            sweep,
            core,
            listening,
            bounds,
            server_json,
            _held: held,
        })
    }

    /// Encoder programs discovered on this machine at startup.
    pub fn encoders(&self) -> &[EncoderId] {
        &self.encoders
    }

    /// The server's ID.
    pub fn server_id(&self) -> Id {
        self.core.server_id()
    }

    /// The server's public key, as 64 hex digits: what a client or a peer
    /// pins.
    pub fn public_key(&self) -> String {
        self.core.public_key()
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

    /// Renames the server, keeps its row in the library's registry of
    /// servers in step on a hub, and sends its new status to every session
    /// subscribed to the `servers` topic. If the registry cannot be written,
    /// the new name is still stored and sent, and the error is returned;
    /// the registry is caught up at the next start or rename.
    ///
    /// No call of the contract does this yet; it is exposed so tests can
    /// change a server's status and exercise the `servers` topic.
    pub fn rename(&self, name: &str) -> Result<(), Error> {
        self.core.rename(name)
    }

    /// What the server shares, for a test to reach inside.
    #[cfg(test)]
    pub(crate) fn core(&self) -> &Arc<Core> {
        &self.core
    }

    /// Starts the system's media session, for a server with a player on a
    /// system that has one. Only `serve` does, on the main thread that then
    /// runs the loop the session needs on macOS; a server in a test has
    /// none. MPRIS on Linux is served from `runtime`.
    #[cfg(feature = "media-session")]
    pub(crate) fn start_media_session(
        &self,
        runtime: &tokio::runtime::Handle,
    ) -> Result<(), Error> {
        if self.core.has_player() && crate::platform::HAS_MEDIA_SESSION {
            let session = crate::media_session::MediaSession::start(
                Arc::downgrade(&self.core),
                runtime.clone(),
            )
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
        // From here on every wait on a satellite's hub ends within the
        // grace: a hub that does not answer holds neither the player, nor
        // a call, nor the outbox, past it. A listen not sent by then stays
        // staged for the next run.
        self.core.stop_hub_within(self.bounds.stop_grace);
        drop(self.hub);
        drop(self.awake);
        drop(self.sweep);
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
        // A media session served from the runtime (MPRIS) leaves the
        // session bus while the runtime still runs, and Windows' controls
        // are let go while the hidden window they are tied to is open. On
        // a Mac the session's thread is left to end by itself, as souvlaki
        // may need the main run loop, which has stopped by now.
        #[cfg(feature = "media-session")]
        if crate::platform::STOP_MEDIA_SESSION_WITH_SERVER {
            let core = Arc::clone(&self.core);
            tokio::task::spawn_blocking(move || core.stop_media_session())
                .await
                .map_err(|failed| Error::Runtime(std::io::Error::other(failed)))?;
        }
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
        // Before the stores close: see `lastfm::Turn`.
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
/// one this program has: the rule changes by rebuilding ("Incremental
/// updates and rebuilds" in docs/internals/history.md).
fn history_rule(library: &ritornel_engine::LibraryStore, steps: &Steps) -> Result<(), Error> {
    if !library.history_needs_rebuild().map_err(Error::Library)? {
        return Ok(());
    }
    steps.say(Preparing::History);
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

/// Works out again, from stored rows and with no network, whatever this
/// build works out otherwise: the kept MusicBrainz replies' facts, the disc
/// layouts and the proposals (docs/internals/corrections.md).
fn corrections_rule(library: &ritornel_engine::LibraryStore) -> Result<(), Error> {
    let request = ritornel_engine::WriteRequest {
        method: ritornel_engine::FACTS_METHOD,
        command: None,
        client: None,
    };
    if library
        .refresh_corrections(request)
        .map_err(Error::Library)?
        .is_some()
    {
        info!("the corrections evidence and proposals are worked out again by this build");
    }
    Ok(())
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

    #[test]
    fn whether_the_player_plays_is_read_without_waiting_on_the_player() {
        watched(|| async {
            let scratch = Scratch::new();
            let run = Run {
                playback: crate::PlaybackRun {
                    output: Some(crate::Output::Null { speed: 1 }),
                    ..crate::PlaybackRun::default()
                },
                ..Run::default()
            };
            let server = Server::start_run(
                HeldDir::take(scratch.path().join("data")).unwrap(),
                Listen::on_port(0),
                Arc::new(SystemClock),
                Arc::new(OsEntropy),
                Bounds::default(),
                run,
            )
            .await
            .unwrap();
            // A call holds the player, as one that waits on the hub does,
            // on a thread of its own.
            let core = Arc::clone(server.core());
            let (held, holding) = mpsc::channel();
            let (release, released) = mpsc::channel::<()>();
            let holder = {
                let core = Arc::clone(&core);
                std::thread::spawn(move || {
                    let _player = core.hold_player();
                    held.send(()).unwrap();
                    let _ = released.recv();
                })
            };
            holding.recv().unwrap();
            // What the hub's ping reads, on the runtime, answers meanwhile:
            // waiting there would hold up the runtime the call waits on.
            assert!(!core.playing());
            release.send(()).unwrap();
            holder.join().unwrap();
            server.stop().await.unwrap();
        });
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
        let params = json!({
            "name": "test",
            "kind": "cli",
            "version": "1",
            "contractVersion": ritornel_contract::CONTRACT_VERSION,
        });
        request(id, "server.hello", &params)
    }

    #[test]
    fn a_server_takes_its_computers_name_over_the_default_or_the_name_an_older_build_took() {
        let legacy = Some("STUDIO-WORKSTAT");
        assert!(takes_computer_name(
            DEFAULT_NAME,
            "Studio-Workstation",
            None
        ));
        // Taken before from COMPUTERNAME, upper-cased and cut short.
        assert!(takes_computer_name(
            "STUDIO-WORKSTAT",
            "Studio-Workstation",
            legacy
        ));
        assert!(!takes_computer_name(
            "Studio-Workstation",
            "Studio-Workstation",
            legacy
        ));
        // A name someone chose is kept, in any case, and where no older
        // build named it otherwise.
        assert!(!takes_computer_name(
            "studio-workstation",
            "Studio-Workstation",
            legacy
        ));
        assert!(!takes_computer_name(
            "Living room",
            "Studio-Workstation",
            legacy
        ));
        assert!(!takes_computer_name("STUDIO-PC", "Studio-PC", None));
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
