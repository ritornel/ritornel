//! The dispatcher: routing from contract calls to engine methods. It takes
//! a decoded frame and a session and returns the frame that answers it, and
//! knows nothing of sockets. See "Transport-neutral dispatcher" in
//! docs/internals/contract.md.
//!
//! [`Core`] is what every session of a running server shares: the open
//! databases, who this server is, and the list of sessions that events are
//! published to.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};
use std::time::Duration;

use ritornel_contract::methods::{
    self, AlbumGet, ArtistGet, ArtistsImportPictures, ArtistsPictureFolders, CorrectionsAnswer,
    CorrectionsAnswers, CorrectionsConfirm, CorrectionsDiscs, CorrectionsGet, CorrectionsList,
    CorrectionsLookup, CorrectionsPreview, CorrectionsSummaryCall, CorrectionsUndo,
    EventsSubscribe, EventsUnsubscribe, HistoryAlbum, HistoryAlbums, HistoryArtist, HistoryArtists,
    HistoryExport, HistoryOnThisDay, HistoryRebuild, HistoryRecent, HistorySetZones,
    HistoryStreamCall, HistoryTrack, HistoryZones, IdentityDryRun, IdentityReview, ImportsApply,
    ImportsDryRun, ImportsGet, ImportsList, ImportsStage, ImportsUndo, JobsCancel, JobsGet,
    JobsList, LastfmReportCall, LastfmSetScrobbling, LastfmSignInFinish, LastfmSignInStart,
    LastfmSignOut, LastfmStatusCall, LastfmSync, LibraryCreate, LibrarySearch, LibrarySetSettings,
    LibrarySetWatching, LibrarySettingsCall, LibraryStats, LinkCopiesCall, LinkInvite, LinkJoin,
    LinkListen, LinkPing, LinkStatusCall, LinkTracksCall, ListClose, ListLocate, ListOpen,
    ListSectionsCall, ListWindowCall, MediaTokenCall, Method, PlaybackGet, PlaybackJump,
    PlaybackPause, PlaybackPlay, PlaybackResume, PlaybackSeek, PlaybackSet, PlaybackSkip,
    PlaybackStop, QueueAdd, QueueClear, QueueMove, QueueRemove, QueueWindow, RatingsSet,
    RecordDismiss, ScanStart, ServerBackup, ServerHello, ServerSetSettings, ServerSettingsCall,
    ServerStatus, ServersList, SourcesAdd, SourcesList, SourcesTracks, TracksGet,
};
use ritornel_contract::{
    ART_SIZES, CONTRACT_VERSION, Descriptor, ErrorCode, Event, Frame, FreeDisk, HelloParams, Id,
    LibraryCreateParams, LibraryCreated, LibraryStatus, Limits, ListSupport, MAX_CLIENT_TEXT,
    PublicDescriptor, Receipt, ReceiptStatus, Response, RpcError, ServerInfo, ServerRole,
    ServersEvent, SessionKind, Versions, capability, descriptor_proof, hex,
};
use ritornel_engine::{
    Client, Clock, Command, Entropy, Fingerprint, LibraryStore, Role, SQLITE_VERSION,
    ServerIdentity, ServerStore, SessionToken, SubjectKind, WriteRequest, Written,
};
use serde_json::{Map, Value};
use tokio::task::JoinSet;
use tracing::{debug, error, info, warn};

use crate::credential::Verifier;
use crate::data_dir::DataDir;
use crate::media::{CoverPool, MediaTokens};
use crate::session::Session;
use crate::transport::{self, Wire};
use crate::views::{MAX_VIEWS, MAX_WINDOW_ROWS, VIEW_IDLE};
use crate::{Bounds, Error};

mod backup;
mod calls;
mod churn;
mod corrections;
mod export;
mod history;
mod imports;
mod lastfm;
mod link;
mod lists;
#[cfg(feature = "media-session")]
mod media_keys;
mod pages;
mod playback;
mod ratings;
mod record;
mod settings;
mod watch;

pub(crate) use backup::record_backup;
pub(crate) use calls::job_state;
pub(crate) use corrections::catalogue;
use lists::Fanout;
pub(crate) use settings::StartUp;

/// The largest frame the server reads, in bytes: 1 MiB, which keeps bulk
/// out of the control channel ("Listener and transport boundaries" in
/// docs/internals/linking.md).
pub(crate) const MAX_MESSAGE_BYTES: usize = 1 << 20;

/// What became of a frame a transport handed over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Received {
    /// It was read, and whatever answers it is queued.
    Taken,
    /// It was over the limit and was not read. The transport closes.
    Refused,
}

/// The databases a running server has open.
#[derive(Debug)]
pub(crate) struct Stores {
    /// `server.db`.
    server: ServerStore,
    adding_source: Mutex<()>,
    /// `library.db`, on a server that is a hub. Creating a library puts it
    /// here, and holds this for writing while it does, so that of two calls
    /// to create one the second finds the first's.
    library: RwLock<Option<LibraryStore>>,
}

impl Stores {
    pub(crate) fn new(server: ServerStore, library: Option<LibraryStore>) -> Self {
        Self {
            server,
            adding_source: Mutex::default(),
            library: RwLock::new(library),
        }
    }
}

/// What every session of a running server shares.
#[derive(Debug)]
pub(crate) struct Core {
    server_id: Id,
    schema_version: i64,
    dir: DataDir,
    /// What can change about this server while it runs.
    this: Mutex<This>,
    /// The databases, until the server stops. A call that comes after that
    /// finds nothing here and is told the server is unavailable.
    stores: RwLock<Option<Stores>>,
    /// What a presented credential is checked against.
    local_credential: Verifier,
    /// What the descriptor's proof is signed with.
    signer: ritornel_engine::Signer,
    /// The invites this hub has made and not seen used.
    invites: crate::link::Invites,
    /// The link sessions this hub has issued, checked as the local
    /// credential is.
    link_sessions: crate::link::LinkSessions,
    /// The port other machines reach this server on, once it listens on
    /// the private network. None when it listens on loopback only.
    private_port: std::sync::OnceLock<u16>,
    /// The runtime the server runs on, for the calls that go out to
    /// another server. `None` in the dispatcher's own tests.
    runtime: Option<tokio::runtime::Handle>,
    /// On a satellite, its link to its hub.
    uplink: RwLock<Option<Arc<crate::uplink::Uplink>>>,
    /// Once the server is stopping: when every wait on its hub ends, on
    /// the link it has and on one a join still under way makes.
    hub_stop_by: std::sync::OnceLock<tokio::time::Instant>,
    /// On a hub, its linked servers' activity, and what keeps the machine
    /// from sleeping while one is active.
    awake: Mutex<crate::awake::KeptAwake>,
    /// Held while a satellite's new name is kept, so that two hellos at
    /// once write it one after the other.
    naming: Mutex<()>,
    /// What a library created here takes its times and its IDs from.
    clock: Arc<dyn Clock>,
    entropy: Arc<dyn Entropy>,
    bounds: Bounds,
    sessions: Mutex<Sessions>,
    /// What wakes the worker that runs jobs.
    jobs: crate::jobs::Wake,
    /// Who is told after a library write.
    fanout: Arc<Fanout>,
    /// The media tokens sessions hold.
    media_tokens: MediaTokens,
    /// The cover cache.
    covers: ritornel_engine::covers::Covers,
    /// The threads that make covers.
    cover_pool: CoverPool,
    /// Orders a job's state check and publication against other publishers.
    job_events: Mutex<()>,
    /// The player, on a server with an audio output.
    player: Mutex<Option<ritornel_engine::playback::Player>>,
    /// Whether there is one, read without waiting on it.
    has_player: std::sync::atomic::AtomicBool,
    /// What wakes the thread that delivers finished listens.
    deliveries: crate::playback::Wake,
    /// What wakes the pulling of a master a satellite's player waits for.
    pulls: crate::playback::Wake,
    /// Whether the player is playing, or about to, as the last call or
    /// tick left it: see [`playing`](Self::playing).
    sounding: std::sync::atomic::AtomicBool,
    /// For the tests alone: how long the audio route waits before each
    /// chunk of a master it sends, so that a pull can be held open.
    audio_pace: std::sync::OnceLock<std::time::Duration>,
    /// Last.fm, once the server has started.
    lastfm: std::sync::OnceLock<crate::lastfm::Lastfm>,
    /// The runs of streams staged for a dry run.
    staging: crate::imports::Staging,
    /// The catalogue the library is asked about at, once the server has
    /// started, on a server built with one.
    catalogue: std::sync::OnceLock<Arc<dyn ritornel_source::Catalogue>>,
    /// Held by a lookup from its receipt's check to its commit, so that no
    /// two lookups ask the same thing at once.
    lookup: Mutex<()>,
    /// Held by a yes, an undo or a keep-apart from its check that no scan
    /// runs to the end of its write, and by the job worker while it claims
    /// a job, so that no scan is claimed between the two.
    pub(crate) answering: Mutex<()>,
    /// Watching the hub's folders, once the server has started.
    watch: std::sync::OnceLock<crate::watch::Watching>,
    /// Where backups go, once the server has started.
    backups: std::sync::OnceLock<crate::backup::Backups>,
    /// The most a satellite's cache of pulled masters holds, in bytes,
    /// once the server has started.
    cache_bytes: std::sync::OnceLock<u64>,
    /// The settings given as the server started that it reports and does
    /// not keep elsewhere, once it has started.
    start_up: std::sync::OnceLock<settings::StartUp>,
    /// What the record's sign knows only while the server runs.
    signs: Mutex<record::Signs>,
    /// The system's media session, once `serve` has started one.
    #[cfg(feature = "media-session")]
    media_session: std::sync::OnceLock<crate::media_session::MediaSession>,
}

/// How many threads make covers: enough to keep a screenful coming, and
/// few enough to leave the machine to the page that is drawing them.
const COVER_THREADS: usize = 4;

#[derive(Debug)]
struct This {
    name: String,
    role: ServerRole,
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

pub(super) fn role(role: Role) -> ServerRole {
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
        stores: Stores,
        identity: &ServerIdentity,
        dir: &DataDir,
        local_credential: Verifier,
        clock: Arc<dyn Clock>,
        entropy: Arc<dyn Entropy>,
        bounds: Bounds,
    ) -> Result<Self, Error> {
        let fanout = Arc::new(Fanout::default());
        if let Some(library) = stores
            .library
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
        {
            lists::hook(library, &fanout);
        }
        let signer = stores
            .server
            .signer()
            .map_err(Error::Store)?
            .ok_or(Error::Store(ritornel_engine::Error::Corrupt(
                "a running server with no identity",
            )))?;
        let link_sessions =
            crate::link::LinkSessions::new(stores.server.link_sessions().map_err(Error::Store)?);
        let runtime = tokio::runtime::Handle::try_current().ok();
        let uplink = match (stores.server.hub_link().map_err(Error::Store)?, &runtime) {
            (Some(link), Some(runtime)) => Some(Arc::new(crate::uplink::Uplink::new(
                link,
                identity.name.clone(),
                runtime.clone(),
                Arc::clone(&clock),
                Arc::clone(&entropy),
                bounds.hub_call_wait,
            ))),
            _ => None,
        };
        let staging = crate::imports::Staging::start(dir.imports())?;
        let cache = dir.cache()?;
        let covers = ritornel_engine::covers::Covers::new(&cache).map_err(|error| Error::Io {
            doing: "prepare",
            path: cache.clone(),
            source: std::io::Error::other(error.to_string()),
        })?;
        Ok(Self {
            fanout,
            media_tokens: MediaTokens::default(),
            covers,
            cover_pool: CoverPool::start(COVER_THREADS),
            job_events: Mutex::default(),
            local_credential,
            signer,
            invites: crate::link::Invites::default(),
            link_sessions,
            private_port: std::sync::OnceLock::new(),
            runtime,
            uplink: RwLock::new(uplink),
            hub_stop_by: std::sync::OnceLock::new(),
            awake: Mutex::default(),
            naming: Mutex::default(),
            clock,
            entropy,
            bounds,
            server_id: identity.server_id,
            schema_version: stores.server.schema_version(),
            dir: dir.clone(),
            this: Mutex::new(This {
                name: identity.name.clone(),
                role: role(identity.role),
                revision: 1,
            }),
            stores: RwLock::new(Some(stores)),
            sessions: Mutex::default(),
            jobs: crate::jobs::Wake::default(),
            player: Mutex::new(None),
            has_player: std::sync::atomic::AtomicBool::new(false),
            deliveries: crate::playback::Wake::default(),
            pulls: crate::playback::Wake::default(),
            sounding: std::sync::atomic::AtomicBool::new(false),
            audio_pace: std::sync::OnceLock::new(),
            lastfm: std::sync::OnceLock::new(),
            staging,
            catalogue: std::sync::OnceLock::new(),
            lookup: Mutex::new(()),
            answering: Mutex::new(()),
            watch: std::sync::OnceLock::new(),
            backups: std::sync::OnceLock::new(),
            cache_bytes: std::sync::OnceLock::new(),
            start_up: std::sync::OnceLock::new(),
            signs: Mutex::default(),
            #[cfg(feature = "media-session")]
            media_session: std::sync::OnceLock::new(),
        })
    }

    pub(crate) fn server_id(&self) -> Id {
        self.server_id
    }

    /// This server's public key, as 64 hex digits.
    pub(crate) fn public_key(&self) -> String {
        hex::encode(&self.signer.public_key())
    }

    pub(crate) fn name(&self) -> String {
        lock(&self.this).name.clone()
    }

    /// Whether this server is its library's hub, the one server that
    /// writes it and runs Last.fm.
    #[cfg_attr(not(feature = "lastfm"), allow(dead_code))]
    pub(crate) fn is_hub(&self) -> bool {
        lock(&self.this).role == ServerRole::Hub
    }

    /// What kind of session `token` holds, if it holds one, and the
    /// satellite it is the link of, if it is a link session. The local
    /// credential and a link session are both `full`.
    ///
    /// Every credential check goes through here: the per-call session check
    /// and revocation will too. It compares against what the server holds
    /// in memory and touches neither a file nor the database, so it is safe
    /// to call on an async worker.
    pub(crate) fn authenticate(
        &self,
        token: &SessionToken,
    ) -> Option<(SessionKind, Option<crate::link::Peer>)> {
        if self.local_credential.accepts(token) {
            return Some((SessionKind::Full, None));
        }
        self.link_sessions
            .peer(token)
            .map(|peer| (SessionKind::Full, Some(peer)))
    }

    /// On a satellite, its link to its hub.
    pub(crate) fn uplink(&self) -> Option<Arc<crate::uplink::Uplink>> {
        self.uplink
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The server is stopping: every wait on its hub ends within `within`
    /// from now, on the link it has and on one a join still under way
    /// makes ([`Uplink::stop_by`](crate::uplink::Uplink::stop_by)).
    pub(crate) fn stop_hub_within(&self, within: std::time::Duration) {
        let by = *self
            .hub_stop_by
            .get_or_init(|| tokio::time::Instant::now() + within);
        if let Some(uplink) = self.uplink() {
            uplink.stop_by(by);
        }
    }

    /// The open session `session` is, shared: what a forwarded call hands
    /// the hub's events to.
    fn shared(&self, session: &Session) -> Option<Arc<Session>> {
        lock(&self.sessions).open.get(&session.id()).cloned()
    }

    /// Tells every session that follows this server's status that its hub
    /// began or stopped answering: their status, which carries the hub's,
    /// has changed.
    pub(crate) fn hub_changed(&self) {
        let mut this = lock(&self.this);
        self.changed(&mut this);
    }

    /// Follows the wake setting `server.db` keeps: at start, and after
    /// `server.setSettings`. It is read while the wake state is held, so of two
    /// settings made at once, the one the database kept last is the one
    /// followed. The machine is held, or let go, at once.
    pub(crate) fn follow_stored_awake(&self, stores: &Stores) {
        let mut awake = lock(&self.awake);
        (awake.setting, awake.set_by_call) = match stores.server().awake() {
            Ok(Some(setting)) if setting.is_valid() => (setting, true),
            Ok(None) => (ritornel_contract::Awake::default(), false),
            Ok(Some(setting)) => {
                warn!(
                    ?setting,
                    "the stored wake setting is out of range: the default holds until it is set again"
                );
                (ritornel_contract::Awake::default(), false)
            }
            Err(error) => {
                warn!(%error, "the stored wake setting cannot be read: the default holds until it is set again");
                (ritornel_contract::Awake::default(), false)
            }
        };
        awake.follow_the_rule(self.clock.now_ms());
    }

    /// Follows the wake setting `server.db` keeps, as a server starts:
    /// asking the system to keep the machine awake only if `asks`.
    pub(crate) fn follow_awake_at_start(&self, asks: bool) {
        lock(&self.awake).asks = asks;
        let stores = self.stores.read().unwrap_or_else(PoisonError::into_inner);
        if let Some(stores) = stores.as_ref() {
            self.follow_stored_awake(stores);
        }
    }

    /// This server's wake setting, and whether the machine is kept awake
    /// now.
    pub(crate) fn awake(&self) -> (ritornel_contract::Awake, bool) {
        let awake = lock(&self.awake);
        (awake.setting, awake.held())
    }

    /// The linked server `peer` made a call: the machine stays awake for
    /// it, its screen free to sleep.
    pub(crate) fn linked_server_called(&self, peer: Id) {
        let now = self.clock.now_ms();
        let mut awake = lock(&self.awake);
        awake.activity.called(peer, now);
        awake.follow_the_rule(now);
    }

    /// The linked server `peer` said whether it plays. Says whether the
    /// machine is kept awake now.
    pub(crate) fn linked_server_pinged(&self, peer: Id, playing: bool) -> bool {
        let now = self.clock.now_ms();
        let mut awake = lock(&self.awake);
        awake.activity.pinged(peer, playing, now);
        awake.follow_the_rule(now)
    }

    /// Lets the machine sleep again once no linked server is active.
    pub(crate) fn check_awake(&self) {
        let now = self.clock.now_ms();
        lock(&self.awake).follow_the_rule(now);
    }

    /// Whether this server's player is playing, or about to, as the last
    /// call or tick left it. It is read without waiting on the player, which a
    /// call may hold while it waits on the hub: the hub's ping reads it on
    /// the async runtime, which must never wait on a lock held by what
    /// waits on the runtime.
    pub(crate) fn playing(&self) -> bool {
        self.sounding.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// For a test: holds the player, as a call waiting on the hub does.
    #[cfg(test)]
    pub(crate) fn hold_player(&self) -> MutexGuard<'_, Option<ritornel_engine::playback::Player>> {
        lock(&self.player)
    }

    /// Says which port other machines reach this server on: once, when it
    /// starts listening on the private network.
    pub(crate) fn set_private_port(&self, port: u16) {
        let _ = self.private_port.set(port);
    }

    /// The routes an invite offers when it is not told which, found as it
    /// is made, so that a tailnet brought up since the server started is
    /// offered too. None when this server listens on loopback only.
    fn offered_routes(&self) -> Vec<String> {
        self.private_port
            .get()
            .map(|port| crate::link::found_routes(*port))
            .unwrap_or_default()
    }

    /// What the server says about itself to anyone who asks. With a
    /// nonce, it proves it holds its key by signing it.
    pub(crate) fn public_descriptor(&self, nonce: Option<&[u8]>) -> PublicDescriptor {
        let this = lock(&self.this);
        let mut capabilities = [
            capability::EVENTS_SERVERS,
            capability::EVENTS_LIBRARY,
            capability::LISTS,
            capability::MEDIA_ART,
            capability::LINK,
        ]
        .map(str::to_owned)
        .to_vec();
        if self.has_player() {
            capabilities.push(capability::PLAYBACK.to_owned());
        }
        if self.catalogue().is_some() {
            capabilities.push(capability::CORRECTIONS_LOOKUP.to_owned());
        }
        PublicDescriptor {
            server_id: self.server_id,
            name: this.name.clone(),
            role: this.role,
            contract_version: CONTRACT_VERSION,
            capabilities,
            limits: Limits {
                max_message_bytes: MAX_MESSAGE_BYTES as u64,
                max_views: MAX_VIEWS as u32,
                max_window_rows: MAX_WINDOW_ROWS as u32,
                view_idle_ms: VIEW_IDLE.as_millis() as u64,
            },
            lists: ListSupport::all(),
            art_sizes: ART_SIZES.to_vec(),
            public_key: Some(self.public_key()),
            proof: nonce.map(|nonce| {
                hex::encode(&self.signer.sign(&descriptor_proof(self.server_id, nonce)))
            }),
        }
    }

    fn info(&self, this: &This) -> ServerInfo {
        ServerInfo {
            server_id: self.server_id,
            name: this.name.clone(),
            role: this.role,
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
    fn open(
        &self,
        sessions: &mut Sessions,
        kind: SessionKind,
        peer: Option<Id>,
    ) -> Option<Arc<Session>> {
        if sessions.closed || sessions.open.len() >= self.bounds.max_sessions {
            return None;
        }
        sessions.next_id += 1;
        let session = Arc::new(Session::linked(sessions.next_id, kind, peer));
        sessions.open.insert(session.id(), Arc::clone(&session));
        self.fanout.add(&session);
        Some(session)
    }

    /// Opens a session with no transport, for a test to call through.
    #[cfg(test)]
    pub(crate) fn open_session(&self, kind: SessionKind) -> Option<Arc<Session>> {
        self.open(&mut lock(&self.sessions), kind, None)
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
        peer: Option<Id>,
        mut wire: W,
    ) -> Result<(), W> {
        // One lock for the check and for the task, so that a stop sees
        // either none of this or all of it.
        let mut sessions = lock(&self.sessions);
        let Some(session) = self.open(&mut sessions, kind, peer) else {
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

    /// Closes every session that is the link of the satellite `peer`.
    pub(crate) fn close_linked(&self, peer: Id) {
        for session in lock(&self.sessions).open.values() {
            if session.peer() == Some(peer) {
                session.close();
            }
        }
    }

    /// Ends a session whose transport has gone.
    pub(crate) fn close_session(&self, session: &Session) {
        session.close();
        self.fanout.remove(session.id());
        self.media_tokens.revoke(session.id());
        lock(&self.sessions).open.remove(&session.id());
        if let Some(uplink) = self.uplink() {
            uplink.forget(session.id());
        }
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

    /// Closes the databases. Whatever calls the server afterwards is told
    /// it is unavailable. This waits for a call that is using a database to
    /// finish.
    pub(crate) fn close_store(&self) -> Result<(), Error> {
        // The cover threads read the databases, so they stop first.
        self.cover_pool.stop();
        let stores = self
            .stores
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        let Some(stores) = stores else {
            return Ok(());
        };
        // Both are closed, whatever closing the first one says.
        let library = stores
            .library
            .into_inner()
            .unwrap_or_else(PoisonError::into_inner)
            .map_or(Ok(()), |library| library.close().map_err(Error::Library));
        stores.server.close()?;
        library
    }

    /// Renames the server, keeps its row in the library's registry of
    /// servers in step on a hub, and sends its new status to every session
    /// that subscribed to the `servers` topic. An error from the registry
    /// comes after the new name is stored and sent.
    ///
    /// No call of the contract reaches this yet; it is used by tests to
    /// change a server's status and exercise the `servers` topic.
    pub(crate) fn rename(&self, name: &str) -> Result<(), Error> {
        // The databases before the name, as a call takes them: the other
        // order could wait on a call while that call waits on this.
        let stores = self.stores.read().unwrap_or_else(PoisonError::into_inner);
        let stores = stores.as_ref().ok_or(Error::Stopped)?;
        let library = stores
            .library
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        // Held from the write to the last publish, so two renames reach
        // every session, and the registry, in the order they were stored.
        let mut this = lock(&self.this);
        let Some(identity) = stores.server.rename(name)? else {
            // A core is made from an identity, so there is always one.
            return Ok(());
        };
        // The name stored is the name, so it is sent whether or not the
        // registry took it; a registry left behind is said, and caught up
        // by the next start or rename.
        let registry = library.as_ref().map_or(Ok(()), |library| {
            crate::library::keep_registry_name(library, &identity)
        });
        this.name = identity.name;
        self.changed(&mut this);
        registry.map_err(Error::Library)
    }

    /// Counts a change to this server's status and sends the new status to
    /// every session that subscribed to the `servers` topic. The event is a
    /// hint sent after the commit.
    fn changed(&self, this: &mut This) {
        this.revision += 1;
        let event = Event::Servers(ServersEvent {
            server: self.info(this),
        });
        for session in lock(&self.sessions).open.values() {
            session.publish(event.clone());
        }
    }

    /// Holds the database as something that is writing to it would, so a
    /// test can see what goes on working meanwhile.
    #[cfg(test)]
    pub(crate) fn stall(&self) -> std::sync::RwLockWriteGuard<'_, Option<Stores>> {
        self.stores.write().unwrap_or_else(PoisonError::into_inner)
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
                let stores = self.stores.read().unwrap_or_else(PoisonError::into_inner);
                let outcome = match stores.as_ref() {
                    Some(stores) => self.call(stores, session, &request.method, request.params),
                    None => Err(RpcError::new(
                        ErrorCode::Unavailable,
                        "the server is stopping",
                    )),
                };
                drop(stores);
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
        stores: &Stores,
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
        if session.peer().is_some() && link::kept_from_a_link(row.name) {
            return Err(RpcError::new(
                ErrorCode::Forbidden,
                format!(
                    "{} is for this server's own machine, not a satellite",
                    row.name
                ),
            ));
        }
        // What a linked server asks keeps this hub awake for a while.
        if let Some(peer) = session.peer()
            && crate::awake::is_activity(row.name)
        {
            self.linked_server_called(peer);
        }
        if let Some(uplink) = self.uplink() {
            match link::on_a_satellite(row.name) {
                link::Route::Here => {}
                link::Route::Hub => {
                    let session = self.shared(session).ok_or_else(|| {
                        RpcError::new(ErrorCode::Unavailable, "the session has closed")
                    })?;
                    return uplink.forward(&session, row.name, params);
                }
                link::Route::Refused(why) => {
                    return Err(RpcError::new(ErrorCode::Unsupported, why)
                        .with_detail("reason", "on_the_hub"));
                }
            }
        }
        match row.name {
            ServerHello::NAME => {
                run::<ServerHello>(params, |hello| self.hello(stores, session, &hello))
            }
            ServerStatus::NAME => run::<ServerStatus>(params, |_| self.status(stores)),
            ServersList::NAME => run::<ServersList>(params, |_| {
                Ok(ritornel_contract::ServersList {
                    servers: vec![self.info(&lock(&self.this))],
                })
            }),
            EventsSubscribe::NAME => run::<EventsSubscribe>(params, |params| {
                if session.kind() != SessionKind::Full
                    && params.topics.contains(&ritornel_contract::Topic::Jobs)
                {
                    return Err(RpcError::new(
                        ErrorCode::Forbidden,
                        "Job events need a full session. Subscribe without the jobs topic.",
                    ));
                }
                let topics = session.subscribe(&params.topics);
                if let Some(uplink) = self.uplink()
                    && params
                        .topics
                        .iter()
                        .any(|topic| crate::uplink::HUB_TOPICS.contains(topic))
                    && let Some(session) = self.shared(session)
                {
                    uplink.subscribe(&session);
                }
                Ok(ritornel_contract::Subscriptions { topics })
            }),
            EventsUnsubscribe::NAME => run::<EventsUnsubscribe>(params, |params| {
                let topics = session.unsubscribe(&params.topics);
                // The hub's events it no longer follows are not sent to it.
                if let Some(uplink) = self.uplink()
                    && let Some(session) = self.shared(session)
                {
                    uplink.unsubscribe(&session, &params.topics);
                }
                Ok(ritornel_contract::Subscriptions { topics })
            }),
            // Each of these may give the hub a folder to watch.
            LibraryCreate::NAME => run::<LibraryCreate>(params, |params| {
                let created = self.create_library(stores, &params);
                self.wake_watch();
                created
            }),
            SourcesList::NAME => run::<SourcesList>(params, |_| self.sources_list(stores)),
            SourcesAdd::NAME => run::<SourcesAdd>(params, |params| {
                let added = self.sources_add(stores, &params);
                self.wake_watch();
                added
            }),
            ScanStart::NAME => run::<ScanStart>(params, |params| self.scan_start(stores, &params)),
            LibrarySetWatching::NAME => run::<LibrarySetWatching>(params, |params| {
                self.library_set_watching(stores, &params)
            }),
            ServerBackup::NAME => {
                run::<ServerBackup>(params, |params| self.server_backup(stores, &params))
            }
            ArtistsPictureFolders::NAME => {
                run::<ArtistsPictureFolders>(params, |_| self.artists_picture_folders(stores))
            }
            ArtistsImportPictures::NAME => run::<ArtistsImportPictures>(params, |params| {
                let imported = self.artists_import_pictures(stores, &params);
                self.wake_watch();
                imported
            }),
            JobsList::NAME => run::<JobsList>(params, |_| self.jobs_list(stores)),
            JobsGet::NAME => run::<JobsGet>(params, |params| self.jobs_get(stores, &params)),
            JobsCancel::NAME => {
                run::<JobsCancel>(params, |params| self.jobs_cancel(stores, &params))
            }
            LibraryStats::NAME => run::<LibraryStats>(params, |_| self.library_stats(stores)),
            IdentityDryRun::NAME => {
                run::<IdentityDryRun>(params, |params| self.identity_dry_run(stores, &params))
            }
            IdentityReview::NAME => run::<IdentityReview>(params, |_| self.identity_review(stores)),
            CorrectionsDiscs::NAME => {
                run::<CorrectionsDiscs>(params, |params| self.corrections_discs(stores, &params))
            }
            LibrarySettingsCall::NAME => {
                run::<LibrarySettingsCall>(params, |_| self.library_settings(stores))
            }
            LibrarySetSettings::NAME => run::<LibrarySetSettings>(params, |params| {
                self.library_set_settings(stores, &params)
            }),
            CorrectionsLookup::NAME => {
                run::<CorrectionsLookup>(params, |params| self.corrections_lookup(stores, &params))
            }
            CorrectionsSummaryCall::NAME => {
                run::<CorrectionsSummaryCall>(params, |_| self.corrections_summary(stores))
            }
            CorrectionsList::NAME => {
                run::<CorrectionsList>(params, |_| self.corrections_list(stores))
            }
            CorrectionsGet::NAME => {
                run::<CorrectionsGet>(params, |params| self.corrections_get(stores, &params))
            }
            CorrectionsPreview::NAME => run::<CorrectionsPreview>(params, |params| {
                self.corrections_preview(stores, &params)
            }),
            CorrectionsConfirm::NAME => run::<CorrectionsConfirm>(params, |params| {
                self.corrections_confirm(stores, &params)
            }),
            CorrectionsUndo::NAME => {
                run::<CorrectionsUndo>(params, |params| self.corrections_undo(stores, &params))
            }
            CorrectionsAnswers::NAME => {
                run::<CorrectionsAnswers>(params, |params| self.corrections_answers(stores, params))
            }
            CorrectionsAnswer::NAME => {
                run::<CorrectionsAnswer>(params, |params| self.corrections_answer(stores, &params))
            }
            ListOpen::NAME => {
                run::<ListOpen>(params, |params| self.list_open(stores, session, params))
            }
            ListWindowCall::NAME => {
                run::<ListWindowCall>(params, |params| Self::list_window(stores, session, params))
            }
            ListSectionsCall::NAME => {
                run::<ListSectionsCall>(params, |params| Self::list_sections(session, &params))
            }
            ListLocate::NAME => {
                run::<ListLocate>(params, |params| Self::list_locate(stores, session, &params))
            }
            ListClose::NAME => {
                run::<ListClose>(params, |params| Ok(Self::list_close(session, &params)))
            }
            LibrarySearch::NAME => {
                run::<LibrarySearch>(params, |params| Self::library_search(stores, &params))
            }
            AlbumGet::NAME => run::<AlbumGet>(params, |params| self.album_get(stores, &params)),
            ArtistGet::NAME => run::<ArtistGet>(params, |params| self.artist_get(stores, &params)),
            MediaTokenCall::NAME => run::<MediaTokenCall>(params, |_| self.media_token(session)),
            PlaybackGet::NAME => run::<PlaybackGet>(params, |_| self.playback_get(stores)),
            PlaybackPlay::NAME => run::<PlaybackPlay>(params, |params| {
                self.playback_play(stores, session, &params)
            }),
            PlaybackPause::NAME => {
                run::<PlaybackPause>(params, |params| self.playback_pause(stores, &params))
            }
            PlaybackResume::NAME => {
                run::<PlaybackResume>(params, |params| self.playback_resume(stores, &params))
            }
            PlaybackStop::NAME => {
                run::<PlaybackStop>(params, |params| self.playback_stop(stores, &params))
            }
            PlaybackSeek::NAME => {
                run::<PlaybackSeek>(params, |params| self.playback_seek(stores, &params))
            }
            PlaybackSkip::NAME => {
                run::<PlaybackSkip>(params, |params| self.playback_skip(stores, &params))
            }
            PlaybackJump::NAME => {
                run::<PlaybackJump>(params, |params| self.playback_jump(stores, &params))
            }
            PlaybackSet::NAME => {
                run::<PlaybackSet>(params, |params| self.playback_set(stores, &params))
            }
            QueueWindow::NAME => {
                run::<QueueWindow>(params, |params| self.queue_window(stores, &params))
            }
            QueueAdd::NAME => {
                run::<QueueAdd>(params, |params| self.queue_add(stores, session, &params))
            }
            QueueRemove::NAME => {
                run::<QueueRemove>(params, |params| self.queue_remove(stores, &params))
            }
            QueueMove::NAME => run::<QueueMove>(params, |params| self.queue_move(stores, &params)),
            QueueClear::NAME => {
                run::<QueueClear>(params, |params| self.queue_clear(stores, &params))
            }
            RatingsSet::NAME => {
                run::<RatingsSet>(params, |params| self.ratings_set(stores, &params))
            }
            SourcesTracks::NAME => {
                run::<SourcesTracks>(params, |params| self.sources_tracks(stores, &params))
            }
            HistoryRecent::NAME => {
                run::<HistoryRecent>(params, |params| self.history_recent(stores, &params))
            }
            HistoryOnThisDay::NAME => {
                run::<HistoryOnThisDay>(params, |params| self.history_on_this_day(stores, &params))
            }
            HistoryArtist::NAME => {
                run::<HistoryArtist>(params, |params| self.history_artist(stores, &params))
            }
            HistoryAlbum::NAME => {
                run::<HistoryAlbum>(params, |params| self.history_album(stores, &params))
            }
            HistoryArtists::NAME => {
                run::<HistoryArtists>(params, |params| self.history_artists(stores, &params))
            }
            HistoryAlbums::NAME => {
                run::<HistoryAlbums>(params, |params| self.history_albums(stores, &params))
            }
            HistoryTrack::NAME => {
                run::<HistoryTrack>(params, |params| self.history_track(stores, &params))
            }
            HistoryStreamCall::NAME => {
                run::<HistoryStreamCall>(params, |_| self.history_stream(stores))
            }
            HistoryRebuild::NAME => {
                run::<HistoryRebuild>(params, |params| self.history_rebuild(stores, &params))
            }
            HistoryExport::NAME => {
                run::<HistoryExport>(params, |params| self.history_export(stores, &params))
            }
            RecordDismiss::NAME => {
                run::<RecordDismiss>(params, |params| self.record_dismiss(stores, &params))
            }
            LinkInvite::NAME => {
                run::<LinkInvite>(params, |params| self.link_invite(stores, session, &params))
            }
            LinkJoin::NAME => {
                run::<LinkJoin>(params, |params| self.link_join(stores, session, &params))
            }
            LinkStatusCall::NAME => run::<LinkStatusCall>(params, |_| self.link_status(stores)),
            LinkTracksCall::NAME => {
                run::<LinkTracksCall>(params, |params| self.link_tracks(stores, &params))
            }
            LinkCopiesCall::NAME => {
                run::<LinkCopiesCall>(params, |params| self.link_copies(stores, &params))
            }
            LinkListen::NAME => {
                run::<LinkListen>(params, |params| self.link_listen(stores, session, params))
            }
            LinkPing::NAME => run::<LinkPing>(params, |params| self.link_ping(session, params)),
            ServerSettingsCall::NAME => {
                run::<ServerSettingsCall>(params, |_| Ok(self.server_settings()))
            }
            ServerSetSettings::NAME => {
                run::<ServerSetSettings>(params, |params| self.server_set_settings(stores, &params))
            }
            HistoryZones::NAME => run::<HistoryZones>(params, |_| self.history_zones(stores)),
            HistorySetZones::NAME => {
                run::<HistorySetZones>(params, |params| self.history_set_zones(stores, &params))
            }
            TracksGet::NAME => run::<TracksGet>(params, |params| self.tracks_get(stores, &params)),
            LastfmStatusCall::NAME => {
                run::<LastfmStatusCall>(params, |_| self.lastfm_status(stores))
            }
            LastfmSignInStart::NAME => {
                run::<LastfmSignInStart>(params, |_| self.lastfm_sign_in_start())
            }
            LastfmSignInFinish::NAME => run::<LastfmSignInFinish>(params, |params| {
                self.lastfm_sign_in_finish(stores, &params)
            }),
            LastfmSignOut::NAME => {
                run::<LastfmSignOut>(params, |params| self.lastfm_sign_out(stores, &params))
            }
            LastfmSync::NAME => {
                run::<LastfmSync>(params, |params| self.lastfm_sync(stores, &params))
            }
            LastfmSetScrobbling::NAME => run::<LastfmSetScrobbling>(params, |params| {
                self.lastfm_set_scrobbling(stores, &params)
            }),
            ImportsStage::NAME => {
                run::<ImportsStage>(params, |params| self.imports_stage(stores, &params))
            }
            ImportsDryRun::NAME => {
                run::<ImportsDryRun>(params, |params| self.imports_dry_run(stores, &params))
            }
            ImportsApply::NAME => {
                run::<ImportsApply>(params, |params| self.imports_apply(stores, &params))
            }
            ImportsUndo::NAME => {
                run::<ImportsUndo>(params, |params| self.imports_undo(stores, &params))
            }
            ImportsGet::NAME => {
                run::<ImportsGet>(params, |params| self.imports_get(stores, &params))
            }
            ImportsList::NAME => run::<ImportsList>(params, |_| self.imports_list(stores)),
            LastfmReportCall::NAME => {
                run::<LastfmReportCall>(params, |params| self.lastfm_report(stores, &params))
            }
            _ => Err(RpcError::new(
                ErrorCode::Internal,
                format!("{} is in the method table and has no handler", row.name),
            )),
        }
    }

    fn hello(
        &self,
        stores: &Stores,
        session: &Session,
        hello: &HelloParams,
    ) -> Result<Descriptor, RpcError> {
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
        if let Some(peer) = session.peer()
            && hello.kind == ritornel_contract::ClientKind::Server
        {
            self.satellite_named(stores, peer, &hello.name);
        }
        info!(
            session = session.id(),
            client = %hello.name,
            client_kind = hello.kind.as_str(),
            client_version = %hello.version,
            "hello"
        );
        // A person opening an app may want their latest plays: the
        // Last.fm thread decides whether that starts an import.
        if !matches!(
            hello.kind,
            ritornel_contract::ClientKind::Cli | ritornel_contract::ClientKind::Agent
        ) && let Some(lastfm) = self.lastfm()
        {
            lastfm.client_opened();
        }
        Ok(Descriptor {
            server: self.public_descriptor(None),
            session_kind: session.kind(),
        })
    }

    /// `server.status`. The library's metadata and the server's name and
    /// role are copied under one read of the library, taking the two in the
    /// order `library.create` does, so they agree. That read is let go
    /// before the record's health reads the library again: a read kept
    /// while another is taken would wait behind a writer queued between
    /// them, which waits on the first.
    fn status(&self, stores: &Stores) -> Result<ritornel_contract::ServerStatus, RpcError> {
        let read = stores
            .library
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        let (library_schema, library) = match read.as_ref() {
            Some(library) => {
                let meta = library.meta().map_err(Error::Library).map_err(failed)?;
                let status = LibraryStatus {
                    library_id: meta.library_id,
                    revision: revision(meta.revision),
                };
                (Some(library.schema_version()), Some(status))
            }
            // A satellite reads the hub's library. Its revision is the
            // hub's to say, and is not asked for here: a status must
            // answer at once, whether the hub does or not.
            None => (
                None,
                self.uplink().map(|uplink| LibraryStatus {
                    library_id: uplink.library_id(),
                    revision: 0,
                }),
            ),
        };
        let (name, role) = {
            let this = lock(&self.this);
            (this.name.clone(), this.role)
        };
        drop(read);
        let watch = self.watch_status(stores);
        let backup = self.backup_status(stores);
        let record = self.record_status(stores);
        Ok(ritornel_contract::ServerStatus {
            server_id: self.server_id,
            name,
            role,
            versions: Versions {
                server: env!("CARGO_PKG_VERSION").to_owned(),
                contract: CONTRACT_VERSION,
                sqlite: SQLITE_VERSION.to_owned(),
                server_schema: self.schema_version,
                library_schema,
            },
            library,
            free_disk: FreeDisk {
                data_bytes: crate::platform::free_bytes(self.dir.root()),
            },
            log_path: self.dir.logs().to_string_lossy().into_owned(),
            watch: Some(watch),
            backup: Some(backup),
            record: Some(record),
            hub: self.uplink().map(|uplink| uplink.status()),
        })
    }

    /// `library.create`: makes this server a hub with a library of its own.
    /// See "Server identity and hub role" in docs/internals/linking.md.
    ///
    /// Two files are written, the library first. `library.db` gets the
    /// library, with its hub pointer naming this server and the command's
    /// receipt, in one transaction. Then `server.db` records that this
    /// server is its hub. A server that stops between the two finishes the
    /// second when it next starts: the pointer names it, so the library is
    /// its own.
    fn create_library(
        &self,
        stores: &Stores,
        params: &LibraryCreateParams,
    ) -> Result<LibraryCreated, RpcError> {
        let request = WriteRequest {
            method: LibraryCreate::NAME,
            command: Some(Command {
                id: params.idempotency_key,
                fingerprint: fingerprint::<LibraryCreate>(params)?,
            }),
            // The only credential there is yet is the local one, so that is
            // the session every command comes from.
            client: self.local_credential.session().map(|session| Client {
                session,
                kind: SubjectKind::Person,
            }),
        };
        let mut library = stores
            .library
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        let written = match library.as_ref() {
            // The command that made this library, sent again, is answered
            // as it was the first time. Any other is refused, and told
            // which library is in its way.
            Some(existing) => existing.created_by(request).map_err(|error| {
                let refused = failed(Error::Library(error));
                if refused.kind() == ErrorCode::Conflict {
                    refused.with_detail("libraryId", existing.library_id().to_string())
                } else {
                    refused
                }
            })?,
            None => {
                let (created, written) = self
                    .new_library(stores, request, params.idempotency_key)
                    .map_err(failed)?;
                info!(library_id = %created.library_id(), "created a library: this server is its hub");
                lists::hook(&created, &self.fanout);
                *library = Some(created);
                let mut this = lock(&self.this);
                this.role = ServerRole::Hub;
                self.changed(&mut this);
                written
            }
        };
        serde_json::from_str(&written.result_json).map_err(|error| {
            RpcError::new(
                ErrorCode::Internal,
                format!(
                    "the stored result of {} could not be read: {error}",
                    LibraryCreate::NAME
                ),
            )
        })
    }

    /// Creates the library in `library.db`, then records in `server.db`
    /// that this server is its hub.
    fn new_library(
        &self,
        stores: &Stores,
        request: WriteRequest,
        command_id: Id,
    ) -> Result<(LibraryStore, Written), Error> {
        // A core is made from an identity, so there is always one. It is
        // read again here for the name the server has now.
        let identity = stores.server.identity()?.ok_or(Error::Stopped)?;
        let path = self.dir.private_library_db()?;
        let receipt = move |library: &ritornel_engine::LibraryMeta| {
            let created = LibraryCreated {
                receipt: Receipt {
                    command_id,
                    status: ReceiptStatus::Done,
                    revision: Some(revision(library.revision)),
                },
                library_id: library.library_id,
            };
            // Writing these few fields cannot fail. If it did, the result
            // would not read back, and the call would say so.
            serde_json::to_string(&created).unwrap_or_default()
        };
        let (library, written) = LibraryStore::create(
            &path,
            &identity,
            request,
            receipt,
            Arc::clone(&self.clock),
            Arc::clone(&self.entropy),
        )
        .map_err(Error::Library)?;
        stores.server.claim_library(library.library_id())?;
        Ok((library, written))
    }
}

/// A library revision as the contract carries it. It counts up from 1.
fn revision(revision: i64) -> u64 {
    u64::try_from(revision).unwrap_or(0)
}

/// What tells a request from another that reuses its idempotency key: the
/// method and the params, as this server writes them from their type. Two
/// requests that differ only in white space or in the order of their fields
/// are then the same request.
fn fingerprint<M: Method>(params: &M::Params) -> Result<Fingerprint, RpcError> {
    let written = serde_json::to_vec(params).map_err(|error| {
        RpcError::new(
            ErrorCode::Internal,
            format!("the params of {} could not be written: {error}", M::NAME),
        )
    })?;
    Ok(Fingerprint::of(M::NAME, &written))
}

/// What a client is told when a command failed in the server.
fn failed(error: Error) -> RpcError {
    use ritornel_engine::Error as Engine;
    match &error {
        // Its own, or one in its data directory that another server holds.
        Error::Library(Engine::LibraryExists) => RpcError::new(
            ErrorCode::Conflict,
            "there is a library on this server already",
        ),
        Error::Library(Engine::CommandMismatch { .. })
        | Error::Store(Engine::CommandMismatch { .. }) => RpcError::new(
            ErrorCode::IdempotencyMismatch,
            "this idempotency key was used before with other params",
        ),
        Error::Library(Engine::SourceExists { source }) => RpcError::new(
            ErrorCode::Conflict,
            "the library has a source of that name on this server already",
        )
        .with_detail("sourceId", source.to_string()),
        Error::Store(Engine::RootTaken { .. }) => RpcError::new(
            ErrorCode::Conflict,
            "this server has a root of that name at another path",
        ),
        Error::Library(Engine::Invalid(why)) => RpcError::new(ErrorCode::Invalid, *why),
        _ => RpcError::new(ErrorCode::Internal, error.to_string()),
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
        // A staged stream's values are a listener's history, and serde's
        // message can quote one: that call is told only that one is wrong.
        let why = if M::NAME == ImportsStage::NAME {
            "a field is unknown or holds a value of another kind".to_owned()
        } else {
            error.to_string()
        };
        RpcError::new(
            ErrorCode::Invalid,
            format!("the params of {} are wrong: {why}", M::NAME),
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
    use std::path::Path;
    use std::time::Instant;

    use ritornel_contract::{METHODS, Request, RequestId, Topic};
    use ritornel_engine::{OsEntropy, SystemClock};
    use serde_json::json;

    use super::*;
    use crate::testing::core;

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
                "capabilities": ["events.servers", "events.library", "lists", "media.art", "link"],
                "limits": {
                    "maxMessageBytes": 1_048_576,
                    "maxViews": 16,
                    "maxWindowRows": 500,
                    "viewIdleMs": 600_000,
                },
                "lists": serde_json::to_value(ListSupport::all()).unwrap(),
                "artSizes": ART_SIZES,
                "publicKey": core.public_key(),
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
                call(&core, &session, "weather.get", None),
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
            ("library.create", None),
            ("library.create", Some(json!({}))),
            ("library.create", Some(json!({ "idempotencyKey": "again" }))),
            ("library.create", Some(json!({ "idempotencyKey": 7 }))),
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
    fn the_status_and_a_command_need_a_full_session_and_the_rest_do_not() {
        let (core, scratch) = core();
        let limited = greeted(&core, SessionKind::Limited);
        assert_eq!(
            call(&core, &limited, "server.status", None),
            Err((ErrorCode::Forbidden, json!({})))
        );
        // A limited session may change nothing, and it is refused before
        // its params are looked at.
        for params in [None, Some(key(0xc0))] {
            assert_eq!(
                call(&core, &limited, "library.create", params),
                Err((ErrorCode::Forbidden, json!({})))
            );
        }
        assert_eq!(
            call(&core, &limited, "jobs.list", None),
            Err((ErrorCode::Forbidden, json!({})))
        );
        assert_eq!(
            call(
                &core,
                &limited,
                "events.subscribe",
                Some(json!({"topics":["library","jobs"]}))
            ),
            Err((ErrorCode::Forbidden, json!({})))
        );
        assert!(limited.subscribe(&[]).is_empty());
        assert!(!scratch.path().join("data/library.db").exists());
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
                    "serverSchema": ritornel_engine::SERVER_SCHEMA_VERSION,
                },
                // There is no library, so nothing is said of one.
                "freeDisk": { "dataBytes": free },
                "logPath": scratch.path().join("data").join("logs").to_str().unwrap(),
                "watch": {
                    "on": false,
                    "enabled": true,
                    "off": "this run of the server does not watch",
                    "folders": [],
                },
                // No backup folder is named, so there is no backup.
                "backup": {},
                // Nothing has stopped the record.
                "record": { "problems": [], "notes": [] },
            })
        );
    }

    #[test]
    fn a_backup_that_failed_is_said_until_one_works_and_one_since_removed_is_no_backup() {
        let (core, scratch) = core();
        // This core's server.db is in memory; a backup copies the file.
        ServerStore::open(
            &scratch.path().join("data").join("server.db"),
            Arc::new(SystemClock),
            Arc::new(OsEntropy),
        )
        .unwrap()
        .ensure_identity("desk")
        .unwrap();
        let place = scratch.path().join("elsewhere");
        core.set_backups(crate::backup::Backups::new(Some(place.clone())));
        let session = greeted(&core, SessionKind::Full);
        let status = || call(&core, &session, "server.status", Some(json!({}))).unwrap();

        // Its disk is not connected: refused, and said.
        let refused = call(&core, &session, "server.backup", Some(key(0xb1))).unwrap_err();
        assert_eq!(refused.0, ErrorCode::Unavailable);
        let backup = status()["backup"].clone();
        assert!(
            backup["failed"]["message"]
                .as_str()
                .unwrap()
                .contains("is not there"),
            "{backup}"
        );
        assert!(backup.get("last").is_none(), "{backup}");

        // Connected: taken, and the failure is over.
        std::fs::create_dir(&place).unwrap();
        let taken = call(&core, &session, "server.backup", Some(key(0xb2))).unwrap();
        let backup = status()["backup"].clone();
        assert!(backup.get("failed").is_none(), "{backup}");
        assert_eq!(backup["last"]["path"], taken["backup"]["path"]);
        assert_eq!(backup["last"]["present"], true);

        // Removed since: there is no backup.
        std::fs::remove_dir_all(taken["backup"]["path"].as_str().unwrap()).unwrap();
        assert_eq!(status()["backup"]["last"]["present"], false);
    }

    #[test]
    fn dismissing_with_a_library_answers_the_record_and_again_changes_nothing() {
        let (core, _scratch) = core();
        let session = greeted(&core, SessionKind::Full);
        call(&core, &session, "library.create", Some(key(0xc1))).unwrap();
        for _ in 0..2 {
            let dismissed = call(&core, &session, "record.dismiss", Some(key(0xd2))).unwrap();
            assert_eq!(dismissed["record"], json!({ "problems": [], "notes": [] }));
        }
    }

    #[test]
    fn dismissing_with_no_library_changes_nothing_and_answers_the_record() {
        let (core, _scratch) = core();
        let session = greeted(&core, SessionKind::Full);
        let dismissed = call(&core, &session, "record.dismiss", Some(key(0xd1))).unwrap();
        assert_eq!(dismissed["record"], json!({ "problems": [], "notes": [] }));
    }

    /// The params of a command with this byte for every byte of its key.
    fn key(byte: u8) -> Value {
        json!({ "idempotencyKey": Id::from_bytes([byte; 16]).to_string() })
    }

    #[test]
    fn creating_a_library_makes_this_server_its_hub() {
        let (core, scratch) = core();
        let session = greeted(&core, SessionKind::Full);
        let created = call(&core, &session, "library.create", Some(key(0xc0))).unwrap();
        let library_id = created["libraryId"].as_str().unwrap().to_owned();
        assert_eq!(
            created,
            json!({
                "commandId": "c0c0c0c0-c0c0-c0c0-c0c0-c0c0c0c0c0c0",
                "status": "done",
                "revision": 1,
                "libraryId": library_id,
            })
        );
        assert_eq!(library_id.len(), 36);

        // The status says what now exists, and the descriptor and the list
        // of servers say the role.
        let status = call(&core, &session, "server.status", None).unwrap();
        assert_eq!(status["role"], "hub");
        assert_eq!(
            status["versions"],
            json!({
                "server": env!("CARGO_PKG_VERSION"),
                "contract": CONTRACT_VERSION,
                "sqlite": "3.54.1",
                "serverSchema": ritornel_engine::SERVER_SCHEMA_VERSION,
                "librarySchema": ritornel_engine::LIBRARY_SCHEMA_VERSION,
            })
        );
        assert_eq!(
            status["library"],
            json!({ "libraryId": library_id, "revision": 1 })
        );
        assert_eq!(core.public_descriptor(None).role, ServerRole::Hub);
        let list = call(&core, &session, "servers.list", None).unwrap();
        assert_eq!(list["servers"][0]["role"], "hub");
        assert_eq!(list["servers"][0]["revision"], 2);

        // The library is a file only the OS user can read.
        let library_db = scratch.path().join("data/library.db");
        assert!(crate::platform::path_is_private(&library_db));
    }

    #[test]
    fn a_hub_renamed_while_it_runs_keeps_the_librarys_registry_in_step() {
        let (core, scratch) = core();
        let session = greeted(&core, SessionKind::Full);
        call(&core, &session, "library.create", Some(key(0xc0))).unwrap();
        let registry = || -> (String, i64) {
            rusqlite::Connection::open(scratch.path().join("data/library.db"))
                .unwrap()
                .query_row(
                    "SELECT name, (SELECT rev FROM library_meta) FROM server WHERE public_id = ?1",
                    [core.server_id().as_bytes()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap()
        };
        assert_eq!(registry(), ("desk".to_owned(), 1));

        core.rename("attic").unwrap();
        assert_eq!(registry(), ("attic".to_owned(), 2));
        // The same name again leaves the library's revision as it was.
        core.rename("attic").unwrap();
        assert_eq!(registry(), ("attic".to_owned(), 2));

        // A registry that cannot be written: the rename says so, and the
        // name is stored and sent all the same. The same name again, once
        // it can be written, catches the registry up.
        let library_db = || rusqlite::Connection::open(scratch.path().join("data/library.db"));
        library_db()
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER refuse_rename BEFORE UPDATE ON server
                 BEGIN SELECT RAISE(ABORT, 'refused'); END",
            )
            .unwrap();
        assert!(matches!(core.rename("cellar"), Err(Error::Library(_))));
        let list = call(&core, &session, "servers.list", None).unwrap();
        assert_eq!(list["servers"][0]["name"], "cellar");
        assert_eq!(registry(), ("attic".to_owned(), 2));
        library_db()
            .unwrap()
            .execute_batch("DROP TRIGGER refuse_rename")
            .unwrap();
        core.rename("cellar").unwrap();
        assert_eq!(registry(), ("cellar".to_owned(), 3));
    }

    #[test]
    fn a_command_sent_again_gets_its_first_answer_and_another_is_refused() {
        let (core, _scratch) = core();
        let session = greeted(&core, SessionKind::Full);
        let created = call(&core, &session, "library.create", Some(key(0xc0))).unwrap();

        // The same key: the same receipt, from another session too, and
        // nothing is created a second time.
        let other = greeted(&core, SessionKind::Full);
        for session in [&session, &other] {
            assert_eq!(
                call(&core, session, "library.create", Some(key(0xc0))).unwrap(),
                created
            );
        }

        // Another key is another command, and this server has its library.
        assert_eq!(
            call(&core, &session, "library.create", Some(key(0xc1))),
            Err((
                ErrorCode::Conflict,
                json!({ "libraryId": created["libraryId"] })
            ))
        );
        let status = call(&core, &session, "server.status", None).unwrap();
        assert_eq!(status["library"]["libraryId"], created["libraryId"]);
        assert_eq!(status["library"]["revision"], 1);
    }

    #[test]
    fn adding_a_folder_again_answers_from_its_receipt_and_another_folder_under_its_key_is_refused()
    {
        let (core, scratch) = core();
        let session = greeted(&core, SessionKind::Full);
        call(&core, &session, "library.create", Some(key(0xc0))).unwrap();
        let music = scratch.path().join("music");
        std::fs::create_dir(&music).unwrap();
        let add = |name: &str| {
            let mut params = key(0xc1);
            params["name"] = json!(name);
            params["path"] = json!(music.to_str().unwrap());
            call(&core, &session, "sources.add", Some(params))
        };
        let added = add("music").unwrap();

        // Sent again once the disk is gone, as after an answer that was
        // lost: the answer it had.
        std::fs::remove_dir(&music).unwrap();
        assert_eq!(add("music"), Ok(added));

        // The same key for another root is refused as that, and leaves no
        // root behind.
        std::fs::create_dir(&music).unwrap();
        let (code, _) = add("records").unwrap_err();
        assert_eq!(code, ErrorCode::IdempotencyMismatch);
        let roots = core
            .with_stores(|stores| stores.server().roots().unwrap())
            .unwrap();
        assert_eq!(roots.len(), 1, "{roots:?}");
    }

    /// What an app does with the first folder on a server with no library:
    /// refused as having none, with nothing written, it makes the library
    /// and sends the same command again.
    #[test]
    fn a_folder_refused_for_want_of_a_library_is_added_once_there_is_one() {
        let (core, scratch) = core();
        let session = greeted(&core, SessionKind::Full);
        let music = scratch.path().join("music");
        std::fs::create_dir(&music).unwrap();
        let mut params = key(0xc1);
        params["name"] = json!("music");
        params["path"] = json!(music.to_str().unwrap());
        let refused = call(&core, &session, "sources.add", Some(params.clone())).unwrap_err();
        assert_eq!(
            refused,
            (ErrorCode::Unavailable, json!({ "reason": "no_library" }))
        );
        let roots = core
            .with_stores(|stores| stores.server().roots().unwrap())
            .unwrap();
        assert!(roots.is_empty(), "{roots:?}");

        call(&core, &session, "library.create", Some(key(0xc0))).unwrap();
        let added = call(&core, &session, "sources.add", Some(params)).unwrap();
        assert_eq!(added["source"]["name"], "music");
        assert_eq!(added["source"]["path"], music.to_str().unwrap());
        // The library is in the server's data folder, and nothing was
        // written in the music folder, which is only read.
        assert!(scratch.path().join("data/library.db").exists());
        assert_eq!(std::fs::read_dir(&music).unwrap().count(), 0);
    }

    #[test]
    fn becoming_a_hub_is_sent_to_the_sessions_that_subscribed() {
        let (core, _scratch) = core();
        let subscribed = greeted(&core, SessionKind::Full);
        let subscribe = Some(json!({ "topics": ["servers"] }));
        call(&core, &subscribed, "events.subscribe", subscribe).unwrap();

        let creator = greeted(&core, SessionKind::Full);
        call(&core, &creator, "library.create", Some(key(0xc0))).unwrap();
        let Some(Frame::Notification(sent)) = now(subscribed.next()) else {
            panic!("an event was queued");
        };
        let event = sent.params.unwrap();
        assert_eq!(
            (
                &event["topic"],
                &event["server"]["role"],
                &event["server"]["revision"]
            ),
            (&json!("servers"), &json!("hub"), &json!(2))
        );

        // A command that is answered from its receipt changed nothing, so
        // it sends nothing.
        call(&core, &creator, "library.create", Some(key(0xc0))).unwrap();
        subscribed.reply(marker());
        assert_eq!(now(subscribed.next()), Some(marker()));
    }

    #[test]
    fn a_jobs_end_is_not_undone_by_an_older_copy_sent_after_it() {
        let (core, _scratch) = core();
        let subscribed = greeted(&core, SessionKind::Full);
        let subscribe = Some(json!({ "topics": ["jobs"] }));
        call(&core, &subscribed, "events.subscribe", subscribe).unwrap();
        let (queued, running) = core
            .with_stores(|stores| {
                let cmd = Command {
                    id: Id::from_bytes([0xd1; 16]),
                    fingerprint: Fingerprint::of("test.scan", b"{}"),
                };
                let new = ritornel_engine::NewJob {
                    kind: "scan",
                    dedupe_key: "music".into(),
                    target: Id::from_bytes([1; 16]),
                    params_json: "{}".into(),
                    session: None,
                };
                let (queued, _) = stores.server.start_job(cmd, "test.scan", new).unwrap();
                let running = stores
                    .server
                    .claim_job(Id::from_bytes([2; 16]))
                    .unwrap()
                    .unwrap();
                (queued, running)
            })
            .unwrap();
        let sent = |session: &Session| {
            let Some(Frame::Notification(sent)) = now(session.next()) else {
                panic!("an event was queued");
            };
            sent.params.unwrap()["job"]["state"].clone()
        };

        core.publish_job(&running);
        core.publish_job(&queued);
        assert_eq!(sent(&subscribed), json!("running"));

        let ended = core
            .with_stores(|stores| {
                stores
                    .server
                    .finish_job(queued.id, ritornel_engine::JobState::Cancelled, None)
                    .unwrap()
            })
            .unwrap();
        // A late snapshot is already stale before the worker sends the end.
        core.publish_job(&queued);
        subscribed.reply(marker());
        assert_eq!(now(subscribed.next()), Some(marker()));

        core.publish_job(&ended);
        core.publish_job(&queued);
        assert_eq!(sent(&subscribed), json!("cancelled"));
        core.publish_job(&queued);
        subscribed.reply(marker());
        assert_eq!(now(subscribed.next()), Some(marker()));
    }

    #[test]
    fn a_library_file_that_is_another_servers_stops_one_being_created_over_it() {
        let (core, scratch) = core();
        let session = greeted(&core, SessionKind::Full);
        // A library.db that names some other server as its hub.
        let library_db = scratch.path().join("data/library.db");
        let other = ServerIdentity {
            server_id: Id::from_bytes([0x22; 16]),
            public_key: [0; 32],
            name: "attic".to_owned(),
            role: Role::Hub,
            library_id: None,
            created_at_ms: 0,
        };
        let request = WriteRequest {
            method: "library.create",
            command: None,
            client: None,
        };
        let (theirs, _) = LibraryStore::create(
            &library_db,
            &other,
            request,
            |_| String::new(),
            Arc::new(SystemClock),
            Arc::new(OsEntropy),
        )
        .unwrap();
        theirs.close().unwrap();
        let before = std::fs::read(&library_db).unwrap();

        assert_eq!(
            call(&core, &session, "library.create", Some(key(0xc0))),
            Err((ErrorCode::Conflict, json!({})))
        );
        assert_eq!(std::fs::read(&library_db).unwrap(), before);
        let status = call(&core, &session, "server.status", None).unwrap();
        assert_eq!(status["role"], "unassigned");
        assert_eq!(status.get("library"), None);
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

    /// Gives the core a player on the null output, which opens nothing
    /// until a track is played.
    fn with_player(core: &Core) {
        use ritornel_engine::playback::{OnDemand, Output, Player, PlayerSetup};
        let player = core
            .with_stores(|stores| {
                let setup = PlayerSetup {
                    server_id: core.server_id(),
                    run: Id::from_bytes([0xb0; 16]),
                    clock: Arc::new(SystemClock),
                    timekeeper: Arc::new(ritornel_engine::SystemTimekeeper),
                    entropy: Arc::new(OsEntropy),
                    checkpoint_every_ms: ritornel_engine::playback::CHECKPOINT_EVERY_MS,
                    fail_at_position_ms: None,
                    fail_saves_after_ms: None,
                };
                let renderer = Box::new(OnDemand::new(Output::Null { speed: 1 }));
                Player::restore(setup, renderer, stores.server(), None)
            })
            .unwrap()
            .unwrap()
            .0;
        core.set_player(player);
    }

    #[test]
    fn a_server_with_a_player_says_so_and_one_without_refuses_the_players_calls() {
        let (core, _scratch) = core();
        let session = greeted(&core, SessionKind::Limited);
        assert_eq!(
            call(&core, &session, "playback.get", None),
            Err((ErrorCode::Unsupported, json!({})))
        );
        with_player(&core);
        assert_eq!(
            core.public_descriptor(None).capabilities,
            [
                "events.servers",
                "events.library",
                "lists",
                "media.art",
                "link",
                "playback"
            ]
        );
        // Controlling playback is open to a limited session.
        let state = call(&core, &session, "playback.get", None).unwrap();
        assert_eq!(
            (&state["status"], &state["output"]["name"]),
            (&json!("stopped"), &json!("null"))
        );
        let paused = call(&core, &session, "playback.pause", Some(key(0xc5))).unwrap();
        assert_eq!(paused["commandId"], key(0xc5)["idempotencyKey"]);
    }

    /// Gives the core a library of these fixtures, scanned, and a player
    /// playing them. Their entries, in queue order.
    fn playing(core: &Core, session: &Session, scratch: &Path, files: &[&str]) -> Vec<Id> {
        call(core, session, "library.create", Some(key(0xd0))).unwrap();
        let music = scratch.join("music");
        std::fs::create_dir_all(&music).unwrap();
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/audio");
        for file in files {
            std::fs::copy(fixtures.join(file), music.join(file)).unwrap();
        }
        let added = call(
            core,
            session,
            "sources.add",
            Some(json!({
                "idempotencyKey": Id::from_bytes([0xd1; 16]).to_string(),
                "name": "Music",
                "path": music.display().to_string(),
            })),
        )
        .unwrap();
        let source = added["source"]["sourceId"].as_str().unwrap();
        let scan = json!({
            "idempotencyKey": Id::from_bytes([0xd2; 16]).to_string(),
            "sourceId": source,
        });
        call(core, session, "scan.start", Some(scan)).unwrap();
        core.with_library(|server, library| {
            let job = server
                .claim_job(Id::from_bytes([0xd3; 16]))
                .unwrap()
                .unwrap();
            let row = job.id;
            ritornel_engine::run_scan(
                job,
                server,
                library,
                ritornel_engine::ScanSettings::default(),
            )
            .unwrap();
            server
                .finish_job(row, ritornel_engine::JobState::Succeeded, None)
                .unwrap();
        });
        with_player(core);
        let path = json!({ "path": music.display().to_string() });
        let found = call(core, session, "sources.tracks", Some(path)).unwrap();
        let ids: Vec<Value> = found["tracks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|track| track["appearanceId"].clone())
            .collect();
        assert_eq!(ids.len(), files.len(), "{found}");
        let play = json!({
            "idempotencyKey": Id::from_bytes([0xd4; 16]).to_string(),
            "source": { "type": "tracks", "ids": ids },
        });
        call(core, session, "playback.play", Some(play)).unwrap();
        let window = json!({ "limit": 10 });
        let queue = call(core, session, "queue.window", Some(window)).unwrap();
        queue["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["entryId"].as_str().unwrap().parse().unwrap())
            .collect()
    }

    /// A next that names the entry it was asked from does nothing once the
    /// player has moved on from it, so a late one cannot skip two.
    #[cfg(feature = "media-session")]
    #[test]
    fn a_media_next_from_an_entry_the_player_has_left_skips_nothing() {
        let (core, scratch) = core();
        let session = greeted(&core, SessionKind::Full);
        let files = ["a.flac", "b.flac", "b.compilation.flac"];
        let entries = playing(&core, &session, scratch.path(), &files);
        let on = || {
            let state = call(&core, &session, "playback.get", None).unwrap();
            let entry = state["entryId"].as_str().unwrap().parse::<Id>().unwrap();
            (entry, state["status"].clone())
        };
        assert_eq!(on(), (entries[0], json!("playing")));
        core.media_key(crate::media_session::Key::NextFrom(entries[0]));
        assert_eq!(on(), (entries[1], json!("playing")));
        core.media_key(crate::media_session::Key::NextFrom(entries[0]));
        assert_eq!(on(), (entries[1], json!("playing")));
    }

    /// `playIfEmpty` plays an empty queue, decided as the server adds, and
    /// never replaces one that has anything in it.
    #[test]
    fn an_add_that_plays_if_empty_plays_an_empty_queue_and_adds_to_any_other() {
        let (core, scratch) = core();
        let session = greeted(&core, SessionKind::Full);
        playing(&core, &session, scratch.path(), &["a.flac", "b.flac"]);
        let queue = call(&core, &session, "queue.window", None).unwrap();
        let tracks: Vec<Value> = queue["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["track"]["appearanceId"].clone())
            .collect();
        call(&core, &session, "queue.clear", Some(key(0xe0))).unwrap();
        let add = |byte: u8, track: &Value| {
            let params = json!({
                "idempotencyKey": Id::from_bytes([byte; 16]).to_string(),
                "source": { "type": "tracks", "ids": [track] },
                "place": { "type": "next" },
                "playIfEmpty": true,
            });
            call(&core, &session, "queue.add", Some(params)).unwrap();
            call(&core, &session, "playback.get", None).unwrap()
        };
        let first = add(0xe1, &tracks[0]);
        assert_eq!(
            (&first["status"], &first["queueLength"]),
            (&json!("playing"), &json!(1))
        );
        let second = add(0xe2, &tracks[1]);
        assert_eq!(
            (
                &second["status"],
                &second["queueLength"],
                &second["entryId"]
            ),
            (&json!("playing"), &json!(2), &first["entryId"])
        );
    }

    /// A tick sends what it did while it still holds the player, so that
    /// a command carried out just after it cannot be sent first and then be
    /// overtaken by the tick's older state.
    #[test]
    fn a_tick_sends_what_it_did_before_a_command_can_change_the_player() {
        let (core, scratch) = core();
        let session = greeted(&core, SessionKind::Full);
        playing(&core, &session, scratch.path(), &["a.flac"]);
        let topics = json!({ "topics": ["playback", "progress"] });
        call(&core, &session, "events.subscribe", Some(topics)).unwrap();
        // sleep: long enough for the tick to have something to say. Longer
        // on a slow machine only gives it more.
        std::thread::sleep(Duration::from_millis(1_100));

        std::thread::scope(|threads| {
            // Nothing can be sent while this is held: the tick waits to send.
            let sending = lock(&core.sessions);
            let ticking = threads.spawn(|| core.tick_player().unwrap());
            let deadline = Instant::now() + Duration::from_secs(60);
            while core.player.try_lock().is_ok() {
                assert!(Instant::now() < deadline, "the tick never took the player");
                std::thread::yield_now();
            }
            // sleep: a chance for the tick to let go of the player, which it
            // must not take.
            std::thread::sleep(Duration::from_millis(200));
            // It holds the player while it waits, so no command comes between.
            assert!(core.player.try_lock().is_err());
            let pausing = threads.spawn(|| {
                call(&core, &session, "playback.pause", Some(key(0xd5))).unwrap();
            });
            drop(sending);
            ticking.join().unwrap();
            pausing.join().unwrap();
        });
        // The newest state is the last one sent.
        let newest = call(&core, &session, "playback.get", None).unwrap();
        let last = session
            .drain()
            .into_iter()
            .filter_map(|frame| match frame {
                Frame::Notification(note) => note.params,
                _ => None,
            })
            .rfind(|event| event["topic"] == "playback")
            .unwrap();
        assert_eq!(last["state"], newest);
    }

    #[test]
    fn every_method_in_the_table_has_a_handler() {
        let (core, _scratch) = core();
        with_player(&core);
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
        assert_eq!(core.authenticate(&token), Some((SessionKind::Full, None)));
        assert_eq!(core.authenticate(&SessionToken::new("anything")), None);
        assert_eq!(core.authenticate(&SessionToken::new("")), None);

        // With the database busy, and then gone, the answer is the same.
        let busy = core.stall();
        assert_eq!(core.authenticate(&token), Some((SessionKind::Full, None)));
        drop(busy);
        core.close_store().unwrap();
        assert_eq!(core.authenticate(&token), Some((SessionKind::Full, None)));
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
        assert_eq!(
            call(&core, &session, "library.create", Some(key(0xc0)))
                .unwrap_err()
                .0,
            ErrorCode::Unavailable
        );
        // A call that comes now is told to come back, and not that it was
        // wrong.
        let (code, _) = call(&core, &session, "servers.list", None).unwrap_err();
        assert_eq!(code, ErrorCode::Unavailable);
        // Closing twice is fine.
        core.close_store().unwrap();
    }

    #[test]
    fn turning_watching_off_while_the_server_stops_does_not_hang() {
        use std::sync::mpsc;
        use std::time::Duration;
        // Leaked, so that a call that hangs does not hold the test up.
        let (core, scratch) = core();
        let core: &'static Core = Box::leak(Box::new(core));
        Box::leak(Box::new(scratch));
        let session = greeted(core, SessionKind::Full);
        let (reading, read) = mpsc::channel();
        let (go, gone) = mpsc::channel::<()>();
        let (done, answered) = mpsc::channel();
        std::thread::spawn(move || {
            // A call holds the databases for as long as it runs, as
            // `dispatch` does.
            let stores = core.stores.read().unwrap();
            reading.send(()).unwrap();
            gone.recv().unwrap();
            let params = ritornel_contract::WatchingParams {
                idempotency_key: Id::from_bytes([0xd0; 16]),
                on: false,
            };
            let set = core.library_set_watching(stores.as_ref().unwrap(), &params);
            done.send(set.is_ok()).unwrap();
        });
        read.recv().unwrap();
        // The server stops while the call is running: closing waits for it,
        // and a new read waits behind the close.
        std::thread::spawn(move || core.close_store());
        let queued = std::time::Instant::now();
        while core.stores.try_read().is_ok() {
            assert!(
                queued.elapsed() < Duration::from_secs(60),
                "the close never waited"
            );
            // sleep: between looks; the deadline bounds them.
            std::thread::sleep(Duration::from_millis(1));
        }
        go.send(()).unwrap();
        assert_eq!(answered.recv_timeout(Duration::from_secs(60)), Ok(true));
        drop(session);
    }

    /// A status asked for while a library is being made: neither waits on
    /// the other. The status holds its read of the library while it waits
    /// for the name, and a writer queues behind that read. A status that
    /// then read the library again, before letting go of the first read,
    /// would wait behind the writer while the writer waited on it.
    #[test]
    fn a_status_and_a_library_made_meanwhile_both_answer() {
        use std::sync::mpsc;
        use std::time::Duration;
        // Leaked, so that a call that hangs does not hold the test up.
        let (core, scratch) = core();
        let core: &'static Core = Box::leak(Box::new(core));
        Box::leak(Box::new(scratch));
        let (status, create) = (
            greeted(core, SessionKind::Full),
            greeted(core, SessionKind::Full),
        );
        // Waits until `seen` says the library's lock is as wanted.
        let until = |seen: fn(&RwLock<Option<LibraryStore>>) -> bool, what: &str| {
            let started = std::time::Instant::now();
            loop {
                let stores = core.stores.read().unwrap();
                if seen(&stores.as_ref().unwrap().library) {
                    return;
                }
                drop(stores);
                assert!(started.elapsed() < Duration::from_secs(60), "{what}");
                // sleep: between looks; the deadline bounds them.
                std::thread::sleep(Duration::from_millis(1));
            }
        };
        let (answered, answers) = mpsc::channel();
        // The name is held, as a rename holds it, so the status waits on
        // it with its read of the library.
        let held = lock(&core.this);
        let send = answered.clone();
        std::thread::spawn(move || {
            send.send(("status", call(core, &status, "server.status", None).is_ok()))
                .unwrap();
        });
        until(
            |library| library.try_write().is_err(),
            "the status never read",
        );
        // The library is made meanwhile: its write queues behind the read.
        std::thread::spawn(move || {
            let created = call(core, &create, "library.create", Some(key(0xc0)));
            answered.send(("create", created.is_ok())).unwrap();
        });
        until(
            |library| library.try_read().is_err(),
            "the write never queued",
        );
        drop(held);
        let mut got: Vec<_> = (0..2)
            .map_while(|_| answers.recv_timeout(Duration::from_secs(60)).ok())
            .collect();
        got.sort_unstable();
        assert_eq!(got, [("create", true), ("status", true)]);
    }

    #[test]
    fn a_staged_stream_of_the_wrong_kind_is_refused_without_its_value() {
        let params = json!({
            "idempotencyKey": Id::from_bytes([1; 16]),
            "runId": Id::from_bytes([2; 16]),
            "piece": 0,
            "pieces": 1,
            "streams": [{
                "endedAt": 1, "msPlayed": 1, "trackUri": "spotify:track:1",
                "title": "Low Tide", "artist": "Calla Brandt", "shuffle": "MARKED_VALUE",
            }],
        });
        let refused = run::<ImportsStage>(Some(params.clone()), |_| unreachable!()).unwrap_err();
        assert!(!refused.message.contains("MARKED"), "{}", refused.message);
        let mut unknown = params;
        unknown["archivePath"] = json!("MARKED_PATH");
        let refused = run::<ImportsStage>(Some(unknown), |_| unreachable!()).unwrap_err();
        assert!(!refused.message.contains("MARKED"), "{}", refused.message);
    }
}
