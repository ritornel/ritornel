//! The player's calls: `playback.*`, `queue.*` and `sources.tracks`, and
//! the events the player sends. See "Commands, playback, and jobs" in
//! docs/internals/contract.md and docs/internals/playback.md.
//!
//! A server with no output has no player, and answers the player's calls
//! with `unsupported` ("Audio pipeline" in docs/internals/playback.md).

use std::path::Path;
use std::sync::PoisonError;
use std::sync::atomic::Ordering;

use ritornel_contract::methods::{
    Method, PlaybackJump, PlaybackPause, PlaybackPlay, PlaybackResume, PlaybackSeek, PlaybackSet,
    PlaybackSkip, PlaybackStop, QueueAdd, QueueClear, QueueMove, QueueRemove,
};
use ritornel_contract::{
    ContextKind, CopyInUse, ErrorCode, Event, Id, ListKind, OutputInfo, OutputMode, OutputProblem,
    PlaySource, PlaybackChanged, PlaybackCommandParams, PlaybackEvent, PlaybackJumpParams,
    PlaybackPlayParams, PlaybackSeekParams, PlaybackSetParams, PlaybackSkipParams, PlaybackState,
    PlaybackStatus, ProgressEvent, QueueAddParams, QueueClearParams, QueueEntry, QueueEvent,
    QueueMoveParams, QueueOrder, QueuePlace, QueueRemoveParams, QueueWindow, QueueWindowParams,
    Receipt, ReceiptStatus, Repeat, RpcError, SkipDirection, SourceTracks, SourcesTracksParams,
    TrackSummary,
};
use ritornel_engine::playback::{
    self as engine, AddTracks, Catalog, Direction, Done, Fetch, Order, Place, PlayTracks, Player,
    PlayerEvent, PlayerState, Request, Settings, Status, TrackDisplay,
};
use ritornel_engine::{Command, LibraryStore, ServerStore};
use tracing::{error, warn};

use crate::hub_catalog::{DEFAULT_CACHE_BYTES, HubCatalog};

use super::lists::expired;
use super::{Core, Stores, fingerprint, lock};
use crate::session::Session;

/// What the player reads the library through: `library.db` on a hub, and
/// the hub on a satellite.
fn catalog<'a>(
    library: Option<&'a LibraryStore>,
    hub: Option<&'a HubCatalog>,
) -> Option<&'a dyn Catalog> {
    match (library, hub) {
        (Some(library), _) => Some(library),
        (None, Some(hub)) => Some(hub),
        (None, None) => None,
    }
}

/// The most rows a view read from the hub to be played may have.
const MAX_VIEW_FROM_HUB: usize = 20_000;

/// The rows one window of a view read from the hub asks for: the most the
/// contract sends at once.
const MAX_WINDOW_ROWS: usize = crate::views::MAX_WINDOW_ROWS as usize;

/// The most queue entries one `queue.window` returns.
const MAX_WINDOW: u32 = 500;

fn unsigned(number: i64) -> u64 {
    u64::try_from(number).unwrap_or(0)
}

fn signed(number: u64) -> i64 {
    i64::try_from(number).unwrap_or(i64::MAX)
}

/// Repeat, as the engine has it.
fn repeat_of(repeat: Repeat) -> engine::Repeat {
    match repeat {
        Repeat::Off => engine::Repeat::Off,
        Repeat::All => engine::Repeat::All,
        Repeat::One => engine::Repeat::One,
    }
}

/// Repeat, as the contract writes it.
fn repeat_to(repeat: engine::Repeat) -> Repeat {
    match repeat {
        engine::Repeat::Off => Repeat::Off,
        engine::Repeat::All => Repeat::All,
        engine::Repeat::One => Repeat::One,
    }
}

/// The tracks a source names, in order, why they are played when the
/// caller does not say, and where the start the caller named is among
/// them.
type Resolved = (Vec<Id>, (ContextKind, Option<Id>), Option<usize>);

/// Why tracks were queued, as the engine writes it.
fn context_kind(kind: ContextKind) -> engine::ContextKind {
    match kind {
        ContextKind::Manual => engine::ContextKind::Manual,
        ContextKind::Album => engine::ContextKind::Album,
        ContextKind::Artist => engine::ContextKind::Artist,
        ContextKind::Playlist => engine::ContextKind::Playlist,
        ContextKind::Query => engine::ContextKind::Query,
        ContextKind::Agent => engine::ContextKind::Agent,
    }
}

/// What a client is told when the player refused or failed.
pub(super) fn refused(error: ritornel_engine::Error) -> RpcError {
    use ritornel_engine::Error as Engine;
    match error {
        Engine::NotFound(what) => RpcError::new(ErrorCode::NotFound, what),
        Engine::Invalid(what) => RpcError::new(ErrorCode::Invalid, what),
        Engine::NoLibrary => RpcError::new(
            ErrorCode::Unavailable,
            "this server holds no library: create one with library.create",
        ),
        Engine::CommandMismatch { .. } => RpcError::new(
            ErrorCode::IdempotencyMismatch,
            "this idempotency key was used before with other params",
        ),
        Engine::Playback(what) => RpcError::new(ErrorCode::Unavailable, what),
        Engine::StaleQueue { current } => RpcError::new(
            ErrorCode::Conflict,
            "the queue has changed since that edit was decided: read it again",
        )
        .with_detail("currentRevision", unsigned(current)),
        other => RpcError::new(ErrorCode::Internal, other.to_string()),
    }
}

fn no_player() -> RpcError {
    RpcError::new(
        ErrorCode::Unsupported,
        "this server has no audio output, so it has no player",
    )
}

/// A track as the contract shows it.
pub(super) fn track(display: &TrackDisplay) -> TrackSummary {
    let number = |value: Option<i64>| value.and_then(|value| u32::try_from(value).ok());
    TrackSummary {
        appearance_id: display.appearance,
        recording_id: display.recording,
        release_id: display.release,
        title: display.title.clone(),
        artist: display.artist.clone(),
        album: display.album.clone(),
        disc_number: number(display.disc),
        track_number: number(display.track),
        duration_ms: display.duration_ms.and_then(|ms| u64::try_from(ms).ok()),
        art_id: display.art,
        rating: display.rating.and_then(|value| u32::try_from(value).ok()),
        loved: display.loved,
    }
}

/// An appearance the library no longer shows: its ID and nothing else.
fn bare(appearance: Id) -> TrackSummary {
    TrackSummary {
        appearance_id: appearance,
        recording_id: None,
        release_id: None,
        title: None,
        artist: None,
        album: None,
        disc_number: None,
        track_number: None,
        duration_ms: None,
        art_id: None,
        rating: None,
        loved: false,
    }
}

impl Core {
    /// Gives the server its player.
    pub(crate) fn set_player(&self, player: Player) {
        *lock(&self.player) = Some(player);
        self.has_player.store(true, Ordering::SeqCst);
    }

    /// Whether the server has a player.
    pub(crate) fn has_player(&self) -> bool {
        self.has_player.load(Ordering::SeqCst)
    }

    /// What wakes the thread that delivers listens.
    pub(crate) fn deliveries(&self) -> &crate::playback::Wake {
        &self.deliveries
    }

    /// The player's state as the contract carries it.
    fn playback_state(&self, state: &PlayerState) -> PlaybackState {
        let current = state.current.as_ref();
        PlaybackState {
            revision: state.revision,
            status: match state.status {
                Status::Stopped => PlaybackStatus::Stopped,
                Status::Playing => PlaybackStatus::Playing,
                Status::Paused => PlaybackStatus::Paused,
                Status::Buffering => PlaybackStatus::Buffering,
            },
            entry_id: current.map(|now| now.entry_id),
            index: current.and_then(|now| u32::try_from(now.index).ok()),
            track: current.and_then(|now| now.track.as_ref().map(track)),
            copy: current
                .and_then(|now| now.format.as_ref())
                .map(|format| CopyInUse {
                    server_id: self.server_id,
                    role: "master".to_owned(),
                    codec: format.codec.clone(),
                    sample_rate_hz: format.sample_rate_hz,
                    channels: format.channels,
                    bit_depth: format.bits,
                }),
            position_ms: unsigned(state.position_ms),
            duration_ms: state.duration_ms.map(unsigned),
            volume: state.volume,
            muted: state.muted,
            shuffle: state.shuffle,
            repeat: repeat_to(state.repeat),
            output: OutputInfo {
                name: state.output.0.clone(),
                mode: OutputMode::Shared,
                sample_rate_hz: state.output.1,
                channels: state.output.2,
                problem: state.output_failing.as_ref().map(|failing| OutputProblem {
                    since: failing.since_ms,
                    detail: failing.detail.clone(),
                }),
            },
            queue_revision: unsigned(state.queue_revision),
            queue_length: state.queue_length as u64,
        }
    }

    /// Sends what the player says changed to the sessions that subscribed,
    /// wakes the delivery of a listen that closed, and hands Last.fm what
    /// began to play.
    pub(crate) fn publish_player(&self, events: Vec<PlayerEvent>, state: Option<PlayerState>) {
        if let Some(state) = &state {
            self.sounding
                .store(state.status.wants_sound(), Ordering::Relaxed);
        }
        for event in events {
            match event {
                PlayerEvent::State => {
                    if let Some(state) = &state {
                        self.publish(&Event::Playback(Box::new(PlaybackEvent {
                            state: self.playback_state(state),
                        })));
                        #[cfg(feature = "media-session")]
                        self.show_in_media_session(state);
                        if state.status != Status::Playing {
                            self.nothing_playing();
                        }
                    }
                }
                PlayerEvent::Began {
                    appearance,
                    duration_ms,
                } => self.now_playing(appearance, duration_ms),
                PlayerEvent::Progress {
                    entry,
                    position_ms,
                    playing,
                } => self.publish(&Event::Progress(ProgressEvent {
                    entry_id: entry,
                    position_ms: unsigned(position_ms),
                    playing,
                })),
                PlayerEvent::Queue(revision) => self.publish(&Event::Queue(QueueEvent {
                    queue_revision: unsigned(revision),
                })),
                PlayerEvent::ListenClosed => self.deliveries.wake(),
                PlayerEvent::Failed { entry, reason } => {
                    warn!(entry_id = %entry, %reason, "a track could not be played");
                }
            }
        }
    }

    /// Runs `work` on the player with the stores, and sends what changed.
    fn with_player<T>(
        &self,
        stores: &Stores,
        work: impl FnOnce(
            &mut Player,
            &ServerStore,
            Option<&dyn Catalog>,
        ) -> Result<(T, Vec<PlayerEvent>), ritornel_engine::Error>,
    ) -> Result<T, RpcError> {
        let library = stores
            .library
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        let hub = self.hub_catalog();
        let mut player = lock(&self.player);
        let player = player.as_mut().ok_or_else(no_player)?;
        let (done, events) = work(
            player,
            &stores.server,
            catalog(library.as_ref(), hub.as_ref()),
        )
        .map_err(refused)?;
        let state = player.state();
        let wants = player.wanted().is_some();
        drop(library);
        self.publish_player(events, Some(state));
        if wants {
            self.pulls.wake();
        }
        Ok(done)
    }

    /// One tick of the player: what the renderer played, checkpoints, and
    /// the end of a track. `Err(FailPoint)` is the crash test's.
    ///
    /// What the tick did is sent before the player is let go, as a
    /// command's is, so that a command carried out just after cannot be
    /// sent first and then be overtaken by the older state.
    pub(crate) fn tick_player(&self) -> Result<(), ritornel_engine::Error> {
        let ticked = self.with_stores(|stores| {
            let library = stores
                .library
                .read()
                .unwrap_or_else(PoisonError::into_inner);
            let hub = self.hub_catalog();
            let mut player = lock(&self.player);
            let Some(player) = player.as_mut() else {
                return Ok(());
            };
            let ticked = player.tick(&stores.server, catalog(library.as_ref(), hub.as_ref()));
            // The record's sign follows the player's saves, and nothing
            // else a tick can fail at.
            match player.save_failing() {
                Some(why) => self.listen_unsaved(why.to_owned()),
                None => self.listen_saved(),
            }
            let events = ticked?;
            let state = (!events.is_empty()).then(|| player.state());
            let wants = player.wanted().is_some();
            drop(library);
            self.publish_player(events, state);
            if wants {
                self.pulls.wake();
            }
            Ok(())
        });
        ticked.unwrap_or(Ok(()))
    }

    /// The most a satellite's cache of pulled masters holds, in bytes: the
    /// `cache_size` setting, or [`DEFAULT_CACHE_BYTES`] without one.
    /// The cache is tidied here, as the server starts and before anything
    /// is pulled.
    pub(crate) fn set_cache_bytes(&self, bytes: Option<u64>) {
        let bytes = bytes.unwrap_or(DEFAULT_CACHE_BYTES);
        let _ = self.cache_bytes.set(bytes);
        crate::hub_catalog::tidy(&self.audio_cache(), bytes);
    }

    /// For the tests alone: how long the audio route waits before each
    /// chunk of a master.
    pub(crate) fn set_audio_pace(&self, pace: Option<std::time::Duration>) {
        if let Some(pace) = pace {
            let _ = self.audio_pace.set(pace);
        }
    }

    /// How long the audio route waits before each chunk: none but in tests.
    pub(crate) fn audio_pace(&self) -> Option<std::time::Duration> {
        self.audio_pace.get().copied()
    }

    /// What wakes the pulling of a master the player waits for.
    pub(crate) fn pulls(&self) -> &crate::playback::Wake {
        &self.pulls
    }

    /// The master the player waits for, with the hub to pull it from:
    /// none on a hub, or while the player waits for nothing. The player is
    /// let go before the pull, so it answers meanwhile.
    pub(crate) fn wanted_master(&self) -> Option<(Fetch, HubCatalog)> {
        let hub = self.hub_catalog()?;
        let fetch = lock(&self.player).as_ref()?.wanted()?;
        Some((fetch, hub))
    }

    /// Hands the player a master pulled for it, or why it could not be.
    /// The cache is trimmed here, with the player held, so that nothing it
    /// is opening can be let go meanwhile.
    pub(crate) fn master_fetched(&self, fetch: Fetch, outcome: Result<engine::Copy, String>) {
        let handed = self.with_stores(|stores| {
            self.with_player(stores, |player, server, library| {
                if let Ok(copy) = &outcome {
                    let limit = self
                        .cache_bytes
                        .get()
                        .copied()
                        .unwrap_or(DEFAULT_CACHE_BYTES);
                    let kept = copy.path(&std::collections::HashMap::new());
                    crate::hub_catalog::trim(&self.audio_cache(), limit, kept.as_deref());
                }
                Ok(((), player.fetched(fetch, outcome, server, library)?))
            })
        });
        if let Some(Err(error)) = handed {
            error!(%error, "a pulled master could not be handed to the player");
            // What it came to is said all the same.
            let state = lock(&self.player).as_ref().map(Player::state);
            self.publish_player(vec![PlayerEvent::State], state);
        }
    }

    /// On a satellite, the hub's library as its player reads it.
    pub(crate) fn hub_catalog(&self) -> Option<HubCatalog> {
        self.uplink()
            .map(|uplink| HubCatalog::new(uplink, self.audio_cache()))
    }

    /// Where a satellite keeps the masters it pulls.
    fn audio_cache(&self) -> std::path::PathBuf {
        self.dir.root().join("cache").join("audio")
    }

    /// The songs of a view the session holds at the hub, in order: read
    /// from the hub a window at a time, as the session's own calls are.
    fn view_from_hub(&self, session: &Session, view: &str) -> Result<Vec<Id>, RpcError> {
        let (Some(uplink), Some(session)) = (self.uplink(), self.shared(session)) else {
            return Err(expired(view));
        };
        let (mut ids, mut offset) = (Vec::new(), 0);
        while offset < MAX_VIEW_FROM_HUB {
            let params = serde_json::json!({
                "view": view,
                "offset": offset,
                "limit": MAX_WINDOW_ROWS,
            });
            let window: ritornel_contract::ListWindow =
                serde_json::from_value(uplink.forward(&session, "list.window", Some(params))?)
                    .map_err(|error| RpcError::new(ErrorCode::Internal, error.to_string()))?;
            if window.rows.is_empty() {
                break;
            }
            offset += window.rows.len();
            for row in window.rows {
                match row {
                    ritornel_contract::Row::Song(song) => ids.push(song.appearance_id),
                    // A row the library no longer has is left out.
                    ritornel_contract::Row::Gone(_) => {}
                    _ => {
                        return Err(RpcError::new(
                            ErrorCode::Invalid,
                            "only a list of songs can be played",
                        ));
                    }
                }
            }
        }
        Ok(ids)
    }

    /// Stops the player for a clean shutdown: the open listen is closed as
    /// `shutdown` and the position saved.
    pub(crate) fn shutdown_player(&self) {
        let done = self.with_stores(|stores| {
            let mut player = lock(&self.player);
            player.as_mut().map(|player| {
                player
                    .shutdown(&stores.server)
                    .map(|_| player.save_failing().map(str::to_owned))
            })
        });
        match done {
            Some(Some(Err(failed))) => {
                error!(error = %failed, "the player could not save where it was");
            }
            Some(Some(Ok(Some(why)))) => {
                error!(error = %why, "the player could not save its listens");
            }
            _ => {}
        }
    }

    /// A request for the player: the command, and how its answer is
    /// written.
    fn request<M: Method>(&self, key: Id, params: &M::Params) -> Result<Request, RpcError> {
        Ok(Request {
            command: Command {
                id: key,
                fingerprint: fingerprint::<M>(params)?,
            },
            method: M::NAME,
            answer: Box::new(move |summary| {
                let changed = PlaybackChanged {
                    receipt: Receipt {
                        command_id: key,
                        status: ReceiptStatus::Done,
                        revision: None,
                    },
                    queue_revision: unsigned(summary.queue_revision),
                    entry_id: summary.entry,
                };
                // These few fields always write.
                serde_json::to_string(&changed).unwrap_or_default()
            }),
        })
    }

    fn answer_of<M: Method>(done: &Done) -> Result<PlaybackChanged, RpcError> {
        serde_json::from_str(&done.answer).map_err(|error| {
            RpcError::new(
                ErrorCode::Internal,
                format!(
                    "the stored result of {} could not be read: {error}",
                    M::NAME
                ),
            )
        })
    }

    pub(super) fn playback_get(&self, _stores: &Stores) -> Result<PlaybackState, RpcError> {
        let player = lock(&self.player);
        let player = player.as_ref().ok_or_else(no_player)?;
        Ok(self.playback_state(&player.state()))
    }

    /// What a command carried out before answered, read before anything it
    /// names is resolved, as the view it played from may have ended since.
    /// The write checks again, for one sent twice at once.
    fn answered_before<M: Method>(
        stores: &Stores,
        key: Id,
        params: &M::Params,
    ) -> Result<Option<PlaybackChanged>, RpcError> {
        let command = Command {
            id: key,
            fingerprint: fingerprint::<M>(params)?,
        };
        let Some(stored) = stores.server.stored_result(command).map_err(refused)? else {
            return Ok(None);
        };
        serde_json::from_str(&stored).map(Some).map_err(|error| {
            RpcError::new(
                ErrorCode::Internal,
                format!(
                    "the stored result of {} could not be read: {error}",
                    M::NAME
                ),
            )
        })
    }

    /// The tracks a source names. A view's rows the library no longer has
    /// are left out, and the start moves with them.
    fn source_tracks(
        &self,
        stores: &Stores,
        session: &Session,
        source: &PlaySource,
        start: Option<u32>,
    ) -> Result<Resolved, RpcError> {
        let start = start.map(|start| usize::try_from(start).unwrap_or(usize::MAX));
        match source {
            PlaySource::Tracks { ids } => Ok((ids.clone(), (ContextKind::Manual, None), start)),
            PlaySource::Release { id } => {
                let library = stores
                    .library
                    .read()
                    .unwrap_or_else(PoisonError::into_inner);
                let hub = self.hub_catalog();
                let tracks = catalog(library.as_ref(), hub.as_ref())
                    .ok_or_else(|| refused(ritornel_engine::Error::NoLibrary))?
                    .release_tracks(*id)
                    .map_err(refused)?
                    .ok_or_else(|| RpcError::new(ErrorCode::NotFound, "no such release"))?;
                Ok((
                    tracks.iter().map(|track| track.appearance).collect(),
                    (ContextKind::Album, Some(*id)),
                    start,
                ))
            }
            // On a satellite the view is held at the hub, and read from it.
            PlaySource::View { view } if self.uplink().is_some() => {
                let ids = self.view_from_hub(session, view)?;
                let moved = start.map(|start| start.min(ids.len()));
                Ok((ids, (ContextKind::Query, None), moved))
            }
            PlaySource::View { view } => {
                let resolved = session.view(view).ok_or_else(|| expired(view))?;
                if resolved.kind != ListKind::Songs {
                    return Err(RpcError::new(
                        ErrorCode::Invalid,
                        "only a list of songs can be played",
                    ));
                }
                let ids = Self::library(stores, |library| {
                    library.song_ids(&resolved.ids).map_err(refused)
                })?;
                if let Some(start) = start
                    && ids.get(start).is_some_and(Option::is_none)
                {
                    return Err(RpcError::new(ErrorCode::NotFound, "that song is gone"));
                }
                let moved = start.map(|start| {
                    ids.iter()
                        .take(start)
                        .filter(|found| found.is_some())
                        .count()
                });
                let tracks = ids.into_iter().flatten().collect();
                Ok((tracks, (ContextKind::Query, None), moved))
            }
        }
    }

    pub(super) fn playback_play(
        &self,
        stores: &Stores,
        session: &Session,
        params: &PlaybackPlayParams,
    ) -> Result<PlaybackChanged, RpcError> {
        if !self.has_player() {
            return Err(no_player());
        }
        if let Some(answer) =
            Self::answered_before::<PlaybackPlay>(stores, params.idempotency_key, params)?
        {
            return Ok(answer);
        }
        let (tracks, default_context, start) =
            self.source_tracks(stores, session, &params.source, params.start)?;
        let context = params
            .context
            .map_or(default_context, |context| (context.kind, context.reference));
        let what = PlayTracks {
            tracks,
            start,
            context: (context_kind(context.0), context.1),
            actor: self.local_credential.session(),
            shuffle: params.shuffle,
            known: matches!(params.source, PlaySource::View { .. }),
        };
        let request = self.request::<PlaybackPlay>(params.idempotency_key, params)?;
        let done = self.with_player(stores, |player, server, library| {
            let done = player.play(request, what, server, library)?;
            let events = done.events.clone();
            Ok((done, events))
        })?;
        Self::answer_of::<PlaybackPlay>(&done)
    }

    fn transport<M>(
        &self,
        stores: &Stores,
        params: &PlaybackCommandParams,
        act: impl FnOnce(
            &mut Player,
            Request,
            &ServerStore,
            Option<&dyn Catalog>,
        ) -> Result<Done, ritornel_engine::Error>,
    ) -> Result<PlaybackChanged, RpcError>
    where
        M: Method<Params = PlaybackCommandParams>,
    {
        let request = self.request::<M>(params.idempotency_key, params)?;
        let done = self.with_player(stores, |player, server, library| {
            let done = act(player, request, server, library)?;
            let events = done.events.clone();
            Ok((done, events))
        })?;
        Self::answer_of::<M>(&done)
    }

    pub(super) fn playback_pause(
        &self,
        stores: &Stores,
        params: &PlaybackCommandParams,
    ) -> Result<PlaybackChanged, RpcError> {
        self.transport::<PlaybackPause>(stores, params, |player, request, server, _| {
            player.pause(request, server)
        })
    }

    pub(super) fn playback_resume(
        &self,
        stores: &Stores,
        params: &PlaybackCommandParams,
    ) -> Result<PlaybackChanged, RpcError> {
        self.transport::<PlaybackResume>(stores, params, |player, request, server, library| {
            player.resume(request, server, library)
        })
    }

    pub(super) fn playback_stop(
        &self,
        stores: &Stores,
        params: &PlaybackCommandParams,
    ) -> Result<PlaybackChanged, RpcError> {
        self.transport::<PlaybackStop>(stores, params, |player, request, server, _| {
            player.stop(request, server)
        })
    }

    pub(super) fn playback_seek(
        &self,
        stores: &Stores,
        params: &PlaybackSeekParams,
    ) -> Result<PlaybackChanged, RpcError> {
        let request = self.request::<PlaybackSeek>(params.idempotency_key, params)?;
        let position = i64::try_from(params.position_ms).unwrap_or(i64::MAX);
        let entry = params.entry_id;
        let done = self.with_player(stores, |player, server, library| {
            let done = player.seek(request, entry, position, server, library)?;
            let events = done.events.clone();
            Ok((done, events))
        })?;
        Self::answer_of::<PlaybackSeek>(&done)
    }

    pub(super) fn playback_skip(
        &self,
        stores: &Stores,
        params: &PlaybackSkipParams,
    ) -> Result<PlaybackChanged, RpcError> {
        let request = self.request::<PlaybackSkip>(params.idempotency_key, params)?;
        let direction = match params.direction {
            SkipDirection::Next => Direction::Next,
            SkipDirection::Previous => Direction::Previous,
        };
        let from = params.from_entry_id;
        let done = self.with_player(stores, |player, server, library| {
            let done = player.skip(request, from, direction, server, library)?;
            let events = done.events.clone();
            Ok((done, events))
        })?;
        Self::answer_of::<PlaybackSkip>(&done)
    }

    pub(super) fn queue_window(
        &self,
        stores: &Stores,
        params: &QueueWindowParams,
    ) -> Result<QueueWindow, RpcError> {
        let order = match params.order.unwrap_or(QueueOrder::Queue) {
            QueueOrder::Queue => Order::Queue,
            QueueOrder::Play => Order::Play,
            QueueOrder::UpNext => Order::UpNext,
        };
        let limit = usize::try_from(params.limit.unwrap_or(100).min(MAX_WINDOW)).unwrap_or(0);
        let offset = usize::try_from(params.offset).unwrap_or(usize::MAX);
        let (page, revision) = {
            let player = lock(&self.player);
            let player = player.as_ref().ok_or_else(no_player)?;
            (
                player.window(order, offset, limit),
                player.state().queue_revision,
            )
        };
        let appearances: Vec<Id> = page
            .entries
            .iter()
            .map(|(_, appearance, _)| *appearance)
            .collect();
        let library = stores
            .library
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        let hub = self.hub_catalog();
        let shown = match catalog(library.as_ref(), hub.as_ref()) {
            Some(catalog) => catalog.tracks(&appearances).map_err(refused)?,
            None => Vec::new(),
        };
        let entries = page
            .entries
            .into_iter()
            .enumerate()
            .map(|(at, (entry_id, appearance, failed))| QueueEntry {
                entry_id,
                track: shown
                    .get(at)
                    .and_then(Option::as_ref)
                    .map_or_else(|| bare(appearance), track),
                failed_reason: failed,
            })
            .collect();
        Ok(QueueWindow {
            queue_revision: unsigned(revision),
            total: page.total as u64,
            current_index: page.current.and_then(|index| u32::try_from(index).ok()),
            entries,
        })
    }

    /// Runs a command of the player's and answers with where it is after.
    fn command<M: Method>(
        &self,
        stores: &Stores,
        key: Id,
        params: &M::Params,
        act: impl FnOnce(
            &mut Player,
            Request,
            &ServerStore,
            Option<&dyn Catalog>,
        ) -> Result<Done, ritornel_engine::Error>,
    ) -> Result<PlaybackChanged, RpcError> {
        if !self.has_player() {
            return Err(no_player());
        }
        let request = self.request::<M>(key, params)?;
        let done = self.with_player(stores, |player, server, library| {
            let done = act(player, request, server, library)?;
            let events = done.events.clone();
            Ok((done, events))
        })?;
        Self::answer_of::<M>(&done)
    }

    pub(super) fn playback_set(
        &self,
        stores: &Stores,
        params: &PlaybackSetParams,
    ) -> Result<PlaybackChanged, RpcError> {
        let settings = Settings {
            volume: params.volume,
            muted: params.muted,
            shuffle: params.shuffle,
            repeat: params.repeat.map(repeat_of),
        };
        self.command::<PlaybackSet>(
            stores,
            params.idempotency_key,
            params,
            |player, request, server, _| player.set(request, settings, server),
        )
    }

    pub(super) fn playback_jump(
        &self,
        stores: &Stores,
        params: &PlaybackJumpParams,
    ) -> Result<PlaybackChanged, RpcError> {
        let entry = params.entry_id;
        self.command::<PlaybackJump>(
            stores,
            params.idempotency_key,
            params,
            |player, request, server, library| player.jump(request, entry, server, library),
        )
    }

    pub(super) fn queue_add(
        &self,
        stores: &Stores,
        session: &Session,
        params: &QueueAddParams,
    ) -> Result<PlaybackChanged, RpcError> {
        if !self.has_player() {
            return Err(no_player());
        }
        if let Some(answer) =
            Self::answered_before::<QueueAdd>(stores, params.idempotency_key, params)?
        {
            return Ok(answer);
        }
        let (tracks, default_context, _) =
            self.source_tracks(stores, session, &params.source, None)?;
        let context = params
            .context
            .map_or(default_context, |context| (context.kind, context.reference));
        let what = AddTracks {
            tracks,
            place: match params.place {
                QueuePlace::Next => Place::Next,
                QueuePlace::End => Place::End,
                QueuePlace::After { entry_id } => Place::After(entry_id),
            },
            context: (context_kind(context.0), context.1),
            actor: self.local_credential.session(),
            play_if_empty: params.play_if_empty.unwrap_or(false),
        };
        let expected = params.expected_revision.map(signed);
        self.command::<QueueAdd>(
            stores,
            params.idempotency_key,
            params,
            |player, request, server, library| player.add(request, expected, what, server, library),
        )
    }

    pub(super) fn queue_remove(
        &self,
        stores: &Stores,
        params: &QueueRemoveParams,
    ) -> Result<PlaybackChanged, RpcError> {
        let expected = params.expected_revision.map(signed);
        let entries = params.entry_ids.clone();
        self.command::<QueueRemove>(
            stores,
            params.idempotency_key,
            params,
            |player, request, server, library| {
                player.remove(request, expected, &entries, server, library)
            },
        )
    }

    pub(super) fn queue_move(
        &self,
        stores: &Stores,
        params: &QueueMoveParams,
    ) -> Result<PlaybackChanged, RpcError> {
        let expected = params.expected_revision.map(signed);
        let entries = params.entry_ids.clone();
        let place = match params.place {
            QueuePlace::Next => Place::Next,
            QueuePlace::End => Place::End,
            QueuePlace::After { entry_id } => Place::After(entry_id),
        };
        self.command::<QueueMove>(
            stores,
            params.idempotency_key,
            params,
            |player, request, server, _| {
                player.move_entries(request, expected, &entries, place, server)
            },
        )
    }

    pub(super) fn queue_clear(
        &self,
        stores: &Stores,
        params: &QueueClearParams,
    ) -> Result<PlaybackChanged, RpcError> {
        let expected = params.expected_revision.map(signed);
        self.command::<QueueClear>(
            stores,
            params.idempotency_key,
            params,
            |player, request, server, _| player.clear(request, expected, server),
        )
    }

    /// Reads again what the listener said of the playing song, after a
    /// rating was set, and tells the clients if it changed.
    pub(super) fn said_again(&self, stores: &Stores) {
        let library = stores
            .library
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        let hub = self.hub_catalog();
        let Some(library) = catalog(library.as_ref(), hub.as_ref()) else {
            return;
        };
        let mut player = lock(&self.player);
        let Some(player) = player.as_mut() else {
            return;
        };
        match player.said_again(library) {
            Ok(true) => self.publish_player(vec![PlayerEvent::State], Some(player.state())),
            Ok(false) => {}
            Err(failed) => warn!(error = %failed, "the playing song's rating could not be read"),
        }
    }

    /// `sources.tracks`: the tracks of the files under a path in one of
    /// this server's roots.
    pub(super) fn sources_tracks(
        &self,
        stores: &Stores,
        params: &SourcesTracksParams,
    ) -> Result<SourceTracks, RpcError> {
        let path = Path::new(&params.path);
        if !path.is_absolute() {
            return Err(RpcError::new(ErrorCode::Invalid, "the path is absolute")
                .with_detail("field", "path"));
        }
        let sources = self.sources_list(stores)?;
        let found = sources
            .sources
            .iter()
            .filter_map(|source| {
                let root = Path::new(source.path.as_ref()?);
                Some((
                    root.components().count(),
                    &source.name,
                    path.strip_prefix(root).ok()?,
                ))
            })
            .max_by_key(|(depth, _, _)| *depth);
        let Some((_, root, relative)) = found else {
            return Err(RpcError::new(
                ErrorCode::NotFound,
                "This path is outside the music folders. Choose a file or folder inside one.",
            ));
        };
        let rel = relative
            .components()
            .map(|part| part.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        let server_id = self.server_id;
        let tracks = Self::library(stores, |library| {
            library
                .folder_tracks(root, &rel, server_id)
                .map_err(refused)
        })?;
        Ok(SourceTracks {
            tracks: tracks.iter().map(track).collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_context_kind_is_one_the_engine_has() {
        for kind in [
            ContextKind::Manual,
            ContextKind::Album,
            ContextKind::Artist,
            ContextKind::Playlist,
            ContextKind::Query,
            ContextKind::Agent,
        ] {
            let wire = serde_json::to_value(kind).unwrap();
            assert_eq!(wire, context_kind(kind).as_str());
        }
    }
}
