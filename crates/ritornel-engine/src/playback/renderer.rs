//! The renderer interface the player sits on: "prepare, play, pause, seek"
//! going down, and what was heard coming up. See decision 24 in
//! docs/design/playback.md. The player never sees the decoder, the ring or
//! the output.
//!
//! A track is played in segments: loading it starts one at a position, and
//! each seek ends the one in hand and starts another. A segment counts the
//! frames handed to the output, at the output's rate, and that count is
//! the whole of what the listen recorder learns.

use std::fmt::Debug;
use std::path::Path;

/// What a loaded track is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Format {
    /// The codec, as the decoder names it.
    pub codec: String,
    /// Its sample rate.
    pub sample_rate_hz: u32,
    /// Its channels.
    pub channels: u32,
    /// Its bit depth, where it has one.
    pub bits: Option<u32>,
    /// Its length, where the container says.
    pub duration_ms: Option<u64>,
}

/// What the library last read of a file: the size and time a rescan
/// compares to decide that a file is unchanged. A file is played under the
/// identity the scan gave it only while it still has both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileFacts {
    /// Its length in bytes, tags included.
    pub size_bytes: u64,
    /// Its modification time, as the scan reads it.
    pub mtime_ms: i64,
}

/// What the current segment has played.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Segment {
    /// Frames handed to the output since the segment began.
    pub frames: u64,
    /// Whether the track has played to its end.
    pub ended: bool,
    /// Why the track stopped before its end, if it did.
    pub failed: Option<String>,
    /// Whether the output went away, a device unplugged or taken by the
    /// system: nothing more is heard through it, and the next load opens
    /// the output afresh.
    pub output_lost: bool,
}

/// Why a track could not be opened or moved in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderError {
    /// What the queue entry is marked with: `unsupported_format`,
    /// `unreadable`, `missing`, `file_changed` or `output`.
    pub reason: &'static str,
    /// What happened, for the log.
    pub message: String,
}

impl RenderError {
    pub(crate) fn new(reason: &'static str, message: impl Into<String>) -> Self {
        Self {
            reason,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for RenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.reason, self.message)
    }
}

/// An output the player renders through.
pub trait Renderer: Debug + Send {
    /// Output frames a second.
    fn rate(&self) -> u32;
    /// What the output is, for the player's state: `default`, `null`.
    fn name(&self) -> String;
    /// Its channels.
    fn channels(&self) -> u32;
    /// Opens a track to play from `start_ms`, paused, and returns what it
    /// is. The file opened must still be the one the library read, by
    /// `facts`, or it is refused as `file_changed`, and every segment of
    /// the track reads that file, whatever takes its path later. A track
    /// already loaded must have been unloaded.
    fn load(&mut self, path: &Path, facts: FileFacts, start_ms: u64)
    -> Result<Format, RenderError>;
    /// Plays the loaded track.
    fn play(&mut self);
    /// Pauses it. The frames already on their way are faded out and
    /// counted; nothing more is handed to the output until `play`.
    fn pause(&mut self);
    /// Ends the current segment and starts one at `position_ms`, and
    /// returns what the ended segment played, whole. A seek that fails
    /// leaves the segment in hand to `poll` and `unload`, which go on
    /// counting what it plays.
    fn seek(&mut self, position_ms: u64) -> Result<Segment, RenderError>;
    /// Lets the loaded track go, and returns what its last segment played,
    /// whole.
    fn unload(&mut self) -> Segment;
    /// What the current segment has played so far.
    fn poll(&mut self) -> Segment;
}

#[cfg(test)]
pub(crate) mod fake {
    //! A renderer with no audio: tracks have the lengths a test gives
    //! them, and time passes when the test says so.

    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};

    use super::{FileFacts, Format, RenderError, Renderer, Segment};

    /// What the catalog in the tests says of every file.
    pub(crate) const FACTS: FileFacts = FileFacts {
        size_bytes: 1000,
        mtime_ms: 0,
    };

    /// The output rate the fake counts frames at.
    pub(crate) const RATE: u32 = 1000;

    #[derive(Debug, Default)]
    struct State {
        lengths: HashMap<PathBuf, Result<u64, &'static str>>,
        /// Whether the output went away.
        lost: bool,
        /// Files that are no longer what the catalog says, by path.
        replaced: HashMap<PathBuf, FileFacts>,
        /// Whether the file loaded was written to where it is.
        rewritten: bool,
        loaded: Option<(PathBuf, u64)>,
        playing: bool,
        /// Where in the track the current segment is.
        at_ms: u64,
        frames: u64,
        loads: Vec<(PathBuf, u64)>,
    }

    /// The test's handle on the fake.
    #[derive(Debug, Clone, Default)]
    pub(crate) struct Fake(Arc<Mutex<State>>);

    impl Fake {
        fn state(&self) -> std::sync::MutexGuard<'_, State> {
            self.0.lock().unwrap()
        }

        /// A track at `path` that is `length_ms` long.
        pub(crate) fn track(&self, path: impl Into<PathBuf>, length_ms: u64) {
            self.state().lengths.insert(path.into(), Ok(length_ms));
        }

        /// Another file takes the place of the one at `path`.
        pub(crate) fn replace(&self, path: impl Into<PathBuf>, facts: FileFacts) {
            self.state().replaced.insert(path.into(), facts);
        }

        /// The output goes away: nothing more is heard until a track is
        /// loaded again.
        pub(crate) fn lose_output(&self) {
            let mut state = self.state();
            state.lost = true;
            state.playing = false;
        }

        /// The file loaded is written to where it is.
        pub(crate) fn rewrite_loaded(&self) {
            self.state().rewritten = true;
        }

        /// A file at `path` that cannot be opened, for `reason`.
        pub(crate) fn broken(&self, path: impl Into<PathBuf>, reason: &'static str) {
            self.state().lengths.insert(path.into(), Err(reason));
        }

        /// Lets `ms` of time pass: a playing track is heard for that long,
        /// up to its end.
        pub(crate) fn advance(&self, ms: u64) {
            let mut state = self.state();
            if !state.playing || state.lost {
                return;
            }
            let Some((_, length)) = state.loaded.clone() else {
                return;
            };
            let heard = ms.min(length.saturating_sub(state.at_ms));
            state.at_ms += heard;
            state.frames += heard * u64::from(RATE) / 1000;
        }

        /// Whether the output is playing.
        pub(crate) fn playing(&self) -> bool {
            let state = self.state();
            state.playing && state.loaded.is_some()
        }

        /// Every load, with the position it started at.
        pub(crate) fn loads(&self) -> Vec<(PathBuf, u64)> {
            self.state().loads.clone()
        }

        /// The renderer the player is given.
        pub(crate) fn renderer(&self) -> Box<dyn Renderer> {
            Box::new(self.clone())
        }
    }

    impl Renderer for Fake {
        fn rate(&self) -> u32 {
            RATE
        }

        fn name(&self) -> String {
            "fake".to_owned()
        }

        fn channels(&self) -> u32 {
            2
        }

        fn load(
            &mut self,
            path: &Path,
            facts: FileFacts,
            start_ms: u64,
        ) -> Result<Format, RenderError> {
            let mut state = self.state();
            let length = match state.lengths.get(path) {
                Some(Ok(length)) => *length,
                Some(Err(reason)) => return Err(RenderError::new(reason, "the fake says so")),
                None => return Err(RenderError::new("missing", "no such fake track")),
            };
            if state.replaced.get(path).copied().unwrap_or(FACTS) != facts {
                return Err(RenderError::new("file_changed", "another file is there"));
            }
            state.rewritten = false;
            state.lost = false;
            state.loaded = Some((path.to_owned(), length));
            state.at_ms = start_ms.min(length);
            state.frames = 0;
            state.playing = false;
            state.loads.push((path.to_owned(), start_ms));
            Ok(Format {
                codec: "fake".to_owned(),
                sample_rate_hz: 44_100,
                channels: 2,
                bits: Some(16),
                duration_ms: Some(length),
            })
        }

        fn play(&mut self) {
            self.state().playing = true;
        }

        fn pause(&mut self) {
            self.state().playing = false;
        }

        fn seek(&mut self, position_ms: u64) -> Result<Segment, RenderError> {
            if self.state().rewritten {
                return Err(RenderError::new("file_changed", "it was written to"));
            }
            let ended = self.poll();
            let mut state = self.state();
            let length = state.loaded.as_ref().map_or(0, |(_, length)| *length);
            state.at_ms = position_ms.min(length);
            state.frames = 0;
            Ok(ended)
        }

        fn unload(&mut self) -> Segment {
            let ended = self.poll();
            let mut state = self.state();
            state.loaded = None;
            state.frames = 0;
            state.playing = false;
            ended
        }

        fn poll(&mut self) -> Segment {
            let state = self.state();
            let ended = state
                .loaded
                .as_ref()
                .is_some_and(|(_, length)| state.at_ms >= *length);
            Segment {
                frames: state.frames,
                ended,
                failed: None,
                output_lost: state.lost,
            }
        }
    }
}
