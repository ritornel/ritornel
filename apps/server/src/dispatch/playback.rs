//! The player's calls: `playback.*`, `queue.window` and `sources.tracks`,
//! and the events the player sends. See "Playback and jobs" in
//! docs/design/contract.md and docs/design/playback.md.
//!
//! A server with no output has no player, and answers the player's calls
//! with `unsupported` (docs/design/playback.md, decision 2).

use std::path::Path;
use std::sync::PoisonError;
use std::sync::atomic::Ordering;

use ritornel_contract::methods::{
    Method, PlaybackPause, PlaybackPlay, PlaybackResume, PlaybackSeek, PlaybackSkip, PlaybackStop,
};
use ritornel_contract::{
    ContextKind, CopyInUse, ErrorCode, Event, Id, OutputInfo, OutputMode, PlaySource,
    PlaybackChanged, PlaybackCommandParams, PlaybackEvent, PlaybackPlayParams, PlaybackSeekParams,
    PlaybackSkipParams, PlaybackState, PlaybackStatus, ProgressEvent, QueueEntry, QueueEvent,
    QueueWindow, QueueWindowParams, Receipt, ReceiptStatus, Repeat, RpcError, SkipDirection,
    SourceTracks, SourcesTracksParams, TrackSummary,
};
use ritornel_engine::playback::{
    self as engine, Direction, Done, PlayTracks, Player, PlayerEvent, PlayerState, Request, Status,
    TrackDisplay,
};
use ritornel_engine::{Command, LibraryStore, ServerStore};
use tracing::{error, warn};

use super::{Core, Stores, fingerprint, lock};

/// The most queue entries one `queue.window` returns.
const MAX_WINDOW: u32 = 500;

fn unsigned(number: i64) -> u64 {
    u64::try_from(number).unwrap_or(0)
}

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
            volume: 1.0,
            muted: false,
            shuffle: false,
            repeat: Repeat::Off,
            output: OutputInfo {
                name: state.output.0.clone(),
                mode: OutputMode::Shared,
                sample_rate_hz: state.output.1,
                channels: state.output.2,
            },
            queue_revision: unsigned(state.queue_revision),
            queue_length: state.queue_length as u64,
        }
    }

    /// Sends what the player says changed to the sessions that subscribed,
    /// and wakes the delivery of a listen that closed.
    pub(crate) fn publish_player(&self, events: Vec<PlayerEvent>, state: Option<PlayerState>) {
        for event in events {
            match event {
                PlayerEvent::State => {
                    if let Some(state) = &state {
                        self.publish(&Event::Playback(Box::new(PlaybackEvent {
                            state: self.playback_state(state),
                        })));
                    }
                }
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
            Option<&LibraryStore>,
        ) -> Result<(T, Vec<PlayerEvent>), ritornel_engine::Error>,
    ) -> Result<T, RpcError> {
        let library = stores
            .library
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        let mut player = lock(&self.player);
        let player = player.as_mut().ok_or_else(no_player)?;
        let (done, events) = work(player, &stores.server, library.as_ref()).map_err(refused)?;
        let state = player.state();
        drop(library);
        self.publish_player(events, Some(state));
        Ok(done)
    }

    /// One tick of the player: what the renderer played, checkpoints, and
    /// the end of a track. `Err(FailPoint)` is the crash test's.
    pub(crate) fn tick_player(&self) -> Result<(), ritornel_engine::Error> {
        let ticked = self.with_stores(|stores| {
            let library = stores
                .library
                .read()
                .unwrap_or_else(PoisonError::into_inner);
            let mut player = lock(&self.player);
            let Some(player) = player.as_mut() else {
                return Ok((Vec::new(), None));
            };
            let events = player.tick(&stores.server, library.as_ref())?;
            let state = (!events.is_empty()).then(|| player.state());
            Ok((events, state))
        });
        match ticked {
            Some(Ok((events, state))) => {
                self.publish_player(events, state);
                Ok(())
            }
            Some(Err(error)) => Err(error),
            None => Ok(()),
        }
    }

    /// Stops the player for a clean shutdown: the open listen is closed as
    /// `shutdown` and the position saved.
    pub(crate) fn shutdown_player(&self) {
        let done = self.with_stores(|stores| {
            let mut player = lock(&self.player);
            player
                .as_mut()
                .map(|player| player.shutdown(&stores.server))
        });
        if let Some(Some(Err(failed))) = done {
            error!(error = %failed, "the player could not save where it was");
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

    pub(super) fn playback_play(
        &self,
        stores: &Stores,
        params: &PlaybackPlayParams,
    ) -> Result<PlaybackChanged, RpcError> {
        if !self.has_player() {
            return Err(no_player());
        }
        let (tracks, default_context) = match &params.source {
            PlaySource::Tracks { ids } => (ids.clone(), (ContextKind::Manual, None)),
            PlaySource::Release { id } => {
                let tracks = Self::library(stores, |library| {
                    library
                        .release_tracks(*id)
                        .map_err(refused)?
                        .ok_or_else(|| RpcError::new(ErrorCode::NotFound, "no such release"))
                })?;
                (
                    tracks.iter().map(|track| track.appearance).collect(),
                    (ContextKind::Album, Some(*id)),
                )
            }
        };
        let context = params
            .context
            .map_or(default_context, |context| (context.kind, context.reference));
        let what = PlayTracks {
            tracks,
            start: usize::try_from(params.start.unwrap_or(0)).unwrap_or(usize::MAX),
            context: (context_kind(context.0), context.1),
            actor: self.local_credential.session(),
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
            Option<&LibraryStore>,
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
        let (queue, state) = {
            let player = lock(&self.player);
            let player = player.as_ref().ok_or_else(no_player)?;
            (player.queue(), player.state())
        };
        let limit = params.limit.unwrap_or(100).min(MAX_WINDOW);
        let offset = usize::try_from(params.offset).unwrap_or(usize::MAX);
        let library = stores
            .library
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        let entries = queue
            .iter()
            .skip(offset)
            .take(usize::try_from(limit).unwrap_or(0))
            .map(|(entry_id, appearance, failed)| {
                let shown = library
                    .as_ref()
                    .and_then(|library| library.track(*appearance).ok().flatten());
                QueueEntry {
                    entry_id: *entry_id,
                    track: shown.as_ref().map_or_else(|| bare(*appearance), track),
                    failed_reason: failed.clone(),
                }
            })
            .collect();
        Ok(QueueWindow {
            queue_revision: unsigned(state.queue_revision),
            total: queue.len() as u64,
            current_index: state.current.and_then(|now| u32::try_from(now.index).ok()),
            entries,
        })
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
        let roots = stores.server.roots().map_err(refused)?;
        let found = roots.iter().find_map(|(name, root)| {
            let rel = path.strip_prefix(root).ok()?;
            Some((name.clone(), rel.to_string_lossy().into_owned()))
        });
        let Some((root, rel)) = found else {
            return Err(RpcError::new(
                ErrorCode::NotFound,
                "that path is in none of this server's roots",
            ));
        };
        let server_id = self.server_id;
        let tracks = Self::library(stores, |library| {
            library
                .folder_tracks(&root, &rel, server_id)
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
