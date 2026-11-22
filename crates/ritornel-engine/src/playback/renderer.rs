//! The renderer interface the player sits on: "prepare, play, pause, seek"
//! going down, and "started, ended" coming up. See "Audio pipeline" in
//! docs/internals/playback.md. The player never sees the decoder, the ring or
//! the output.
//!
//! A track is played in spans: an unbroken run of one track through the
//! output, from a position. Loading a track starts one, each seek replaces
//! it with another, and a track queued to follow is a span the output runs
//! into with no gap. The player numbers each span as it hands it down, and
//! the output says, in order, when each span's first frame was handed over
//! and when and how it ended, with its count of frames. Every span the
//! renderer takes gets exactly one `Ended`, after its `Began` if it had one.
//! That is the whole of what the listen recorder learns.

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

/// How a span ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum End {
    /// It ran into span `next`, joined to it with no gap: its track played
    /// to its end.
    Joined {
        /// The span that plays now.
        next: u64,
    },
    /// Its audio ran out: its track played to its end.
    Drained,
    /// Its audio ran out early, because the file could not be read on.
    Failed(String),
    /// Something was put in its place: a seek, a skip, a stop.
    Replaced,
}

/// What the output did, in the order it did it. Times are nanoseconds on
/// the renderer's own clock, [`Renderer::now`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The span's first frame was handed to the output.
    Began {
        /// The span.
        span: u64,
        /// When.
        at: u64,
    },
    /// The span ended, having handed `frames` frames to the output, at the
    /// output's rate.
    Ended {
        /// The span.
        span: u64,
        /// Every frame of it handed over.
        frames: u64,
        /// When its last frame was done, or when it was replaced.
        at: u64,
        /// How.
        end: End,
    },
}

/// Whether the output ran into a next track in what it said: once it has,
/// letting that track go is too late, though the player has not heard.
pub(crate) fn ran_on(said: &[Event]) -> bool {
    said.iter().any(|event| {
        matches!(
            event,
            Event::Ended {
                end: End::Joined { .. },
                ..
            }
        )
    })
}

/// How far the span in hand has played: for the position, and for a
/// listen saved while it plays. Its count is final only in its `Ended`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Progress {
    /// The span the output is playing; 0 for none.
    pub span: u64,
    /// Frames of it handed over so far.
    pub frames: u64,
    /// Whether the output went away, a device unplugged or taken by the
    /// system: nothing more is heard through it, and the next load opens
    /// the output afresh.
    pub lost: bool,
}

/// Which ReplayGain tag a track plays at. See "Audio pipeline" in
/// docs/internals/playback.md.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ReplayGain {
    /// None: the samples as they are.
    #[default]
    Off,
    /// The track's gain.
    Track,
    /// The album's gain, or the track's where the album has none.
    Album,
}

/// Why a track could not be opened or moved in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderError {
    /// What the queue entry is marked with: `unsupported_format`,
    /// `unreadable`, `missing` or `file_changed`; or `output`, where the
    /// output failed and no track is to blame, which marks nothing.
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

    /// Whether the output failed, rather than the track.
    pub(crate) fn is_output(&self) -> bool {
        self.reason == "output"
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
    /// Opens a track as span `span`, to play from `start_ms`, paused, and
    /// returns what it is. The file opened must still be the one the
    /// library read, by `facts`, or it is refused as `file_changed`, and
    /// every span of the track reads that file, whatever takes its path
    /// later. A track already loaded must have been unloaded.
    fn load(
        &mut self,
        path: &Path,
        facts: FileFacts,
        start_ms: u64,
        span: u64,
    ) -> Result<Format, RenderError>;
    /// Plays the loaded track.
    fn play(&mut self);
    /// Pauses it. The frames already on their way are faded out and
    /// counted; nothing more is handed to the output until `play`.
    fn pause(&mut self);
    /// Plays the track of span `of` from `position_ms`, as span `span`, in
    /// place of whatever plays now, and lets a track queued to follow go.
    /// Naming the track keeps the player's choice even if the output has
    /// just run on into the next. An error changes nothing.
    fn seek(&mut self, of: u64, position_ms: u64, span: u64) -> Result<(), RenderError>;
    /// Lets the loaded track go, and a track queued to follow it.
    fn unload(&mut self);
    /// What the output did since it was last asked, in order.
    fn drain(&mut self) -> Vec<Event>;
    /// How far the span in hand has played.
    fn progress(&self) -> Progress;
    /// Now, on the clock events are stamped with.
    fn now(&self) -> u64;

    /// The ReplayGain the tracks loaded or queued from now on play at.
    fn set_replay_gain(&mut self, _gain: ReplayGain) {}

    /// The player's own volume, 0 to 1, taken up at once with a short
    /// ramp.
    fn set_volume(&mut self, _volume: f32) {}

    /// Opens the track to play straight after the loaded one as span
    /// `span`, with no gap, held to `facts` as `load` is. `Ok(None)` when it
    /// cannot be joined: this renderer cannot join tracks, one is queued
    /// already, or the decoder has read past the loaded track's end. It is
    /// then loaded once the loaded track's audio runs out.
    fn queue_next(
        &mut self,
        _path: &Path,
        _facts: FileFacts,
        _span: u64,
    ) -> Result<Option<Format>, RenderError> {
        Ok(None)
    }

    /// Lets the queued track go, even once its decoder has it. `false` only
    /// when the output has run into it: until then the loaded track ends
    /// where it would have, and nothing of the queued one is heard.
    fn cancel_next(&mut self) -> bool {
        true
    }
}

#[cfg(test)]
pub(crate) mod fake {
    //! A renderer with no audio: tracks have the lengths a test gives
    //! them, and time passes when the test says so. It joins a queued
    //! track to the one before it only when the test asks it to, and, as
    //! the real decoder does, takes the next once it has read to within
    //! half a second of a track's end. It stamps events on the test's
    //! monotonic clock, and keeps them all for a test to hold listens to.

    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};

    use super::{
        End, Event, FileFacts, Format, Progress, RenderError, Renderer, ReplayGain, ran_on,
    };
    use crate::timekeeping::Timekeeper;

    /// What the catalog in the tests says of every file.
    pub(crate) const FACTS: FileFacts = FileFacts {
        size_bytes: 1000,
        mtime_ms: 0,
    };

    /// The output rate the fake counts frames at: one a millisecond.
    pub(crate) const RATE: u32 = 1000;

    /// What moves a test's clocks on by so many milliseconds.
    type Clocks = Arc<dyn Fn(i64) + Send + Sync>;

    /// How far ahead of the output the decoder reads: the real ring.
    const LEAD_MS: u64 = 500;

    /// The span in hand.
    #[derive(Debug, Clone)]
    struct Span {
        span: u64,
        path: PathBuf,
        /// Where its audio runs out: its end, or where its file fails.
        end: u64,
        failed: bool,
        at_ms: u64,
        frames: u64,
        began: bool,
        ended: bool,
        /// Whether its decoder has started reading ahead.
        started: bool,
    }

    impl Span {
        /// Whether the decoder has read to its end: a next queued by then
        /// is taken, though it can still be let go, and none can be queued
        /// after.
        fn read_out(&self) -> bool {
            self.ended || (self.started && self.at_ms + LEAD_MS >= self.end)
        }
    }

    /// A span the fake was given.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) struct Given {
        pub(crate) span: u64,
        pub(crate) path: PathBuf,
        pub(crate) from_ms: u64,
        /// For a seek, the span whose track it plays.
        pub(crate) seek_of: Option<u64>,
        /// When, on the test's monotonic clock.
        pub(crate) at_ms: i64,
    }

    #[derive(Default)]
    struct State {
        lengths: HashMap<PathBuf, Result<u64, &'static str>>,
        cuts: HashMap<PathBuf, u64>,
        /// Whether the output went away.
        lost: bool,
        /// Whether the output cannot be opened.
        refused: bool,
        /// How long the output plays on during the next seek, before it
        /// stops answering.
        stalls: Option<u64>,
        /// Files that are no longer what the catalog says, by path.
        replaced: HashMap<PathBuf, FileFacts>,
        /// Whether the file loaded was written to where it is.
        rewritten: bool,
        loaded: Option<Span>,
        playing: bool,
        loads: Vec<(PathBuf, u64)>,
        /// Whether a queued track is joined to the one before it.
        gapless: bool,
        /// The span queued to follow, and its track.
        next: Option<(u64, PathBuf)>,
        gain: ReplayGain,
        /// The gain each track was opened at, loaded or queued.
        gains: Vec<(PathBuf, ReplayGain)>,
        volume: Option<f32>,
        /// The clock time passes on.
        clock: Option<Arc<dyn Timekeeper>>,
        /// How long the output plays on during each call the player makes,
        /// as a callback runs while the player is busy, and what moves the
        /// test's clocks by as much.
        meanwhile: Option<(u64, Clocks)>,
        /// Whether the output went away and was let go: it then has no rate,
        /// as an output not yet opened again has none.
        gone: bool,
        /// What the player has not drained yet, and all that was ever said.
        said: Vec<Event>,
        history: Vec<Event>,
        given: Vec<Given>,
    }

    impl std::fmt::Debug for State {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("State")
                .field("loaded", &self.loaded)
                .field("next", &self.next)
                .finish_non_exhaustive()
        }
    }

    fn stamp(ms: i64) -> u64 {
        u64::try_from(ms).unwrap_or(0) * 1_000_000
    }

    impl State {
        fn now_ms(&self) -> i64 {
            self.clock.as_ref().map_or(0, |clock| clock.mono_ms())
        }

        fn say(&mut self, event: Event) {
            self.history.push(event.clone());
            self.said.push(event);
        }

        fn give(&mut self, span: u64, path: &Path, from_ms: u64, seek_of: Option<u64>) {
            let at_ms = self.now_ms();
            self.given.push(Given {
                span,
                path: path.to_owned(),
                from_ms,
                seek_of,
                at_ms,
            });
        }

        /// Ends the span in hand, and the one queued, as replaced at `at`.
        fn let_go(&mut self, at: u64) {
            if let Some(span) = self.loaded.take().filter(|span| !span.ended) {
                self.say(Event::Ended {
                    span: span.span,
                    frames: span.frames,
                    at,
                    end: End::Replaced,
                });
            }
            if let Some((span, _)) = self.next.take() {
                self.say(Event::Ended {
                    span,
                    frames: 0,
                    at,
                    end: End::Replaced,
                });
            }
        }

        fn install(&mut self, span: u64, path: &Path, from_ms: u64, started: bool) {
            let length = match self.lengths.get(path) {
                Some(Ok(length)) => *length,
                _ => 0,
            };
            let cut = self.cuts.get(path).copied().filter(|cut| *cut < length);
            self.loaded = Some(Span {
                span,
                path: path.to_owned(),
                end: cut.unwrap_or(length),
                failed: cut.is_some(),
                at_ms: from_ms.min(length),
                frames: 0,
                began: false,
                ended: false,
                started,
            });
        }

        /// The span in hand reaches its end at `at`: on into the one
        /// queued, or its audio runs out.
        fn reach_end(&mut self, mut span: Span, at: u64) {
            let next = self.next.take();
            let end = match next {
                Some((next, path)) if !span.failed => {
                    self.install(next, &path, 0, true);
                    End::Joined { next }
                }
                next => {
                    if let Some((next, _)) = next {
                        self.say(Event::Ended {
                            span: next,
                            frames: 0,
                            at,
                            end: End::Replaced,
                        });
                    }
                    span.ended = true;
                    self.loaded = Some(span.clone());
                    if span.failed {
                        End::Failed("the fake cut it".to_owned())
                    } else {
                        End::Drained
                    }
                }
            };
            self.say(Event::Ended {
                span: span.span,
                frames: span.frames,
                at,
                end,
            });
        }
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

        /// A track whose file cannot be read past `at_ms`.
        pub(crate) fn cut(&self, path: impl Into<PathBuf>, at_ms: u64) {
            self.state().cuts.insert(path.into(), at_ms);
        }

        /// Joins a queued track to the one before it, as the real output
        /// does.
        pub(crate) fn gapless(&self) {
            self.state().gapless = true;
        }

        /// Stamps what is heard on `clock`, which a test moves on after
        /// each `advance`.
        pub(crate) fn clocked(&self, clock: Arc<dyn Timekeeper>) {
            self.state().clock = Some(clock);
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

        /// The output cannot be opened while `refused`: every load fails
        /// as the output's fault.
        pub(crate) fn refuse_output(&self, refused: bool) {
            self.state().refused = refused;
        }

        /// The next seek plays on for `ms`, as the output did before it
        /// stopped answering, and then fails as the output's fault.
        pub(crate) fn stall_at_seek(&self, ms: u64) {
            self.state().stalls = Some(ms);
        }

        /// The file loaded is written to where it is.
        pub(crate) fn rewrite_loaded(&self) {
            self.state().rewritten = true;
        }

        /// A file at `path` that cannot be opened, for `reason`.
        pub(crate) fn broken(&self, path: impl Into<PathBuf>, reason: &'static str) {
            self.state().lengths.insert(path.into(), Err(reason));
        }

        /// Lets `ms` of time pass from the clock's now: a playing track is
        /// heard for that long, up to its end, and on into a queued track
        /// joined to it.
        pub(crate) fn advance(&self, ms: u64) {
            let mut state = self.state();
            let base = state.now_ms();
            let at = |elapsed: u64| stamp(base + i64::try_from(elapsed).unwrap_or(0));
            let mut elapsed = 0;
            while state.playing && !state.lost && elapsed < ms {
                let Some(mut span) = state.loaded.clone().filter(|span| !span.ended) else {
                    break;
                };
                let heard = (ms - elapsed).min(span.end.saturating_sub(span.at_ms));
                if heard > 0 && !span.began {
                    span.began = true;
                    state.say(Event::Began {
                        span: span.span,
                        at: at(elapsed),
                    });
                }
                span.at_ms += heard;
                span.frames += heard * u64::from(RATE) / 1000;
                elapsed += heard;
                if span.at_ms < span.end {
                    state.loaded = Some(span);
                    break;
                }
                state.reach_end(span, at(elapsed));
            }
        }

        /// The output plays on for `ms` during each call the player makes,
        /// as its callback does while the player is busy; `clocks` moves the
        /// test's clocks on by as much.
        pub(crate) fn meanwhile(&self, ms: u64, clocks: impl Fn(i64) + Send + Sync + 'static) {
            self.state().meanwhile = Some((ms, Arc::new(clocks)));
        }

        fn plays_on(&self) {
            let meanwhile = self.state().meanwhile.clone();
            if let Some((ms, clocks)) = meanwhile {
                self.advance(ms);
                clocks(i64::try_from(ms).unwrap_or(0));
            }
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

        /// The gain each track was opened at, loaded or queued, in order.
        pub(crate) fn gains(&self) -> Vec<(PathBuf, ReplayGain)> {
            self.state().gains.clone()
        }

        /// The volume the output was last given.
        pub(crate) fn volume(&self) -> Option<f32> {
            self.state().volume
        }

        /// The track queued to follow, if one is.
        pub(crate) fn queued(&self) -> Option<PathBuf> {
            self.state().next.as_ref().map(|(_, path)| path.clone())
        }

        /// How long the span in hand has still to play, while it plays.
        pub(crate) fn left(&self) -> Option<u64> {
            let state = self.state();
            let span = state.loaded.as_ref().filter(|span| !span.ended)?;
            state.playing.then(|| span.end.saturating_sub(span.at_ms))
        }

        /// The track the output plays, while its audio lasts.
        pub(crate) fn in_hand(&self) -> Option<PathBuf> {
            let state = self.state();
            let span = state.loaded.as_ref().filter(|span| !span.ended)?;
            Some(span.path.clone())
        }

        /// All the fake has said, and every span it was given.
        pub(crate) fn timeline(&self) -> (Vec<Event>, Vec<Given>) {
            let state = self.state();
            (state.history.clone(), state.given.clone())
        }

        /// The renderer the player is given.
        pub(crate) fn renderer(&self) -> Box<dyn Renderer> {
            Box::new(self.clone())
        }

        fn length_of(state: &State, path: &Path, facts: FileFacts) -> Result<u64, RenderError> {
            let length = match state.lengths.get(path) {
                Some(Ok(length)) => *length,
                Some(Err(reason)) => return Err(RenderError::new(reason, "the fake says so")),
                None => return Err(RenderError::new("missing", "no such fake track")),
            };
            if state.replaced.get(path).copied().unwrap_or(FACTS) != facts {
                return Err(RenderError::new("file_changed", "another file is there"));
            }
            Ok(length)
        }
    }

    fn format(length: u64) -> Format {
        Format {
            codec: "fake".to_owned(),
            sample_rate_hz: 44_100,
            channels: 2,
            bits: Some(16),
            duration_ms: Some(length),
        }
    }

    impl Renderer for Fake {
        fn rate(&self) -> u32 {
            if self.state().gone { 0 } else { RATE }
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
            span: u64,
        ) -> Result<Format, RenderError> {
            self.plays_on();
            let mut state = self.state();
            // A file that cannot be read says so first, as one whose copy is
            // nowhere to be reached does before any output is opened.
            let length = Self::length_of(&state, path, facts)?;
            if state.refused {
                return Err(RenderError::new("output", "no output device is there"));
            }
            let now = stamp(state.now_ms());
            state.let_go(now);
            state.rewritten = false;
            (state.lost, state.gone) = (false, false);
            state.playing = false;
            state.give(span, path, start_ms, None);
            state.install(span, path, start_ms, false);
            state.loads.push((path.to_owned(), start_ms));
            let gain = state.gain;
            state.gains.push((path.to_owned(), gain));
            Ok(format(length))
        }

        fn play(&mut self) {
            self.plays_on();
            let mut state = self.state();
            state.playing = true;
            if let Some(span) = &mut state.loaded {
                span.started = true;
            }
        }

        fn pause(&mut self) {
            self.plays_on();
            self.state().playing = false;
        }

        fn seek(&mut self, of: u64, position_ms: u64, span: u64) -> Result<(), RenderError> {
            self.plays_on();
            let stalls = self.state().stalls.take();
            if let Some(ms) = stalls {
                self.advance(ms);
                return Err(RenderError::new("output", "the output stopped answering"));
            }
            let mut state = self.state();
            if state.rewritten {
                return Err(RenderError::new("file_changed", "it was written to"));
            }
            let Some(track) = state.given.iter().find(|given| given.span == of) else {
                return Err(RenderError::new("output", "no such span"));
            };
            let path = track.path.clone();
            let (now, playing) = (stamp(state.now_ms()), state.playing);
            state.let_go(now);
            state.give(span, &path, position_ms, Some(of));
            state.install(span, &path, position_ms, playing);
            Ok(())
        }

        fn unload(&mut self) {
            self.plays_on();
            let mut state = self.state();
            let now = stamp(state.now_ms());
            state.let_go(now);
            state.playing = false;
            // A lost output is let go, as `OnDemand` lets it go.
            state.gone |= state.lost;
        }

        fn drain(&mut self) -> Vec<Event> {
            std::mem::take(&mut self.state().said)
        }

        fn progress(&self) -> Progress {
            let state = self.state();
            let (span, frames) = state
                .loaded
                .as_ref()
                .filter(|span| !span.ended)
                .map_or((0, 0), |span| (span.span, span.frames));
            Progress {
                span,
                frames,
                lost: state.lost,
            }
        }

        fn now(&self) -> u64 {
            stamp(self.state().now_ms())
        }

        fn set_replay_gain(&mut self, gain: ReplayGain) {
            self.state().gain = gain;
        }

        fn set_volume(&mut self, volume: f32) {
            self.state().volume = Some(volume);
        }

        fn queue_next(
            &mut self,
            path: &Path,
            facts: FileFacts,
            span: u64,
        ) -> Result<Option<Format>, RenderError> {
            self.plays_on();
            let mut state = self.state();
            let open = state
                .loaded
                .as_ref()
                .is_some_and(|loaded| !loaded.read_out());
            if !state.gapless || state.next.is_some() || !open {
                return Ok(None);
            }
            let length = Self::length_of(&state, path, facts)?;
            state.give(span, path, 0, None);
            state.next = Some((span, path.to_owned()));
            let gain = state.gain;
            state.gains.push((path.to_owned(), gain));
            Ok(Some(format(length)))
        }

        fn cancel_next(&mut self) -> bool {
            self.plays_on();
            let mut state = self.state();
            if ran_on(&state.said) {
                return false;
            }
            if let Some((span, _)) = state.next.take() {
                let at = stamp(state.now_ms());
                state.say(Event::Ended {
                    span,
                    frames: 0,
                    at,
                    end: End::Replaced,
                });
            }
            true
        }
    }
}
