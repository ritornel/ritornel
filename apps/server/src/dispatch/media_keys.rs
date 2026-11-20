//! What the system's media session asks of the player and shows of it.
//! A key takes the path a contract call takes, with a command ID of its
//! own. See "OS integration" in docs/design/playback.md.

use std::path::PathBuf;

use ritornel_contract::{
    Id, PlaybackCommandParams, PlaybackSeekParams, PlaybackSetParams, PlaybackSkipParams,
    PlaybackStatus, SkipDirection,
};
use tracing::warn;

use super::Core;
use crate::media_session::{Key, MediaSession};

/// The size of the cover Now Playing is given.
const COVER_SIZE: u32 = 512;

impl Core {
    /// Shows the player's state in `session` from now on.
    pub(crate) fn attach_media_session(&self, session: MediaSession) {
        let _ = self.media_session.set(session);
    }

    /// Takes the media session off the system, if the server has one.
    pub(crate) fn stop_media_session(&self) {
        if let Some(session) = self.media_session.get() {
            session.stop();
        }
    }

    /// Shows the player's state, if the server has a media session.
    pub(super) fn show_in_media_session(&self, state: &ritornel_engine::playback::PlayerState) {
        if let Some(session) = self.media_session.get() {
            session.show(state);
        }
    }

    /// Carries out a media key. Play and pause name nothing, so a toggle
    /// becomes whichever the player's state asks for; the others act on
    /// the entry the player is on, but for a seek that names its own,
    /// which the player ignores once it has moved on from it.
    pub(crate) fn media_key(&self, key: Key) {
        let done = self.with_stores(|stores| {
            let state = self.playback_get(stores)?;
            let idempotency_key = ritornel_engine::new_id(self.clock.now_ms(), &*self.entropy)
                .map_err(super::playback::refused)?;
            let command = PlaybackCommandParams { idempotency_key };
            let playing = state.status == PlaybackStatus::Playing;
            match (key, state.entry_id) {
                (Key::Toggle, _) if playing => self.playback_pause(stores, &command),
                (Key::Toggle | Key::Play, _) => self.playback_resume(stores, &command),
                (Key::Pause, _) => self.playback_pause(stores, &command),
                (Key::Volume(volume), _) => self.playback_set(
                    stores,
                    &PlaybackSetParams {
                        idempotency_key,
                        volume: Some(volume),
                        muted: Some(false),
                        shuffle: None,
                        repeat: None,
                    },
                ),
                (Key::Next | Key::Previous, Some(from_entry_id)) => self.playback_skip(
                    stores,
                    &PlaybackSkipParams {
                        idempotency_key,
                        from_entry_id,
                        direction: if key == Key::Next {
                            SkipDirection::Next
                        } else {
                            SkipDirection::Previous
                        },
                    },
                ),
                (Key::Seek(position_ms), Some(entry_id))
                | (
                    Key::SeekIn {
                        entry: entry_id,
                        position_ms,
                    },
                    _,
                ) => self.playback_seek(
                    stores,
                    &PlaybackSeekParams {
                        idempotency_key,
                        entry_id,
                        position_ms,
                    },
                ),
                (_, None) => return Ok(()),
            }
            .map(|_| ())
        });
        if let Some(Err(error)) = done {
            warn!(?key, %error, "a media key could not be carried out");
        }
    }

    /// The cover of the release an appearance is on, as a file for Now
    /// Playing, made in the cover cache if it is not there yet.
    pub(crate) fn now_playing_cover(&self, appearance: Id) -> Option<PathBuf> {
        self.with_library(|server, library| {
            let art = library.track_art(appearance).ok()??;
            let path = self.covers.path(art, COVER_SIZE);
            if !path.exists() {
                self.covers
                    .render(library, server, art, COVER_SIZE)
                    .map_err(|error| warn!(%error, "no cover for Now Playing"))
                    .ok()?;
            }
            Some(path)
        })
        .flatten()
    }
}
