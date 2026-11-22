//! The audio pipeline: a decode thread per ring, a ring, and an output
//! that drives the real-time callback. See "Audio pipeline" in
//! docs/internals/playback.md.
//!
//! ```text
//! control (the player)   load / seek / unload, queue next, play / pause ↓   events, progress ↑
//!    │ messages and rings down, events up, through lock-free queues
//! decode thread          file → Symphonia → gain → channels → rubato → ring (about 0.5 s)
//!    │                   then the next track, joined, with a marker where it begins
//! output callback        ring → fade, volume → device; says when each span began and ended
//! ```
//!
//! The output is the system's default device ([`Output::Default`], where
//! the build has one) or the null output, which hands frames to nothing at
//! real-time speed or a multiple of it, for the tests and for a run that
//! must make no sound.

pub(crate) mod decode;
#[cfg(all(
    feature = "audio-output",
    any(target_os = "macos", target_os = "linux", windows)
))]
mod device;
mod null;
pub(crate) mod output;

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use rtrb::{Consumer, Producer, RingBuffer};

use self::decode::{Opened, OutputSpec, Source};
use self::output::{Callback, EVENTS, Feed, Gate, Marker, Message, Shared};
use crate::playback::renderer::{
    End, Event, FileFacts, Format, Progress, RenderError, Renderer, ReplayGain, ran_on,
};

/// How much audio the ring between the decode thread and the output holds.
const RING_MS: u32 = 500;

/// How long a change waits for the output to take it.
const OUTPUT_WAIT: Duration = Duration::from_secs(2);

/// How long an output told to play may ask for no frames before it counts
/// as lost. A sound server whose device goes away may leave a stream
/// waiting with no error, as PipeWire is expected to when its only sink is
/// unplugged.
/// See "Audio pipeline" in docs/internals/playback.md.
const STALL: Duration = Duration::from_secs(5);

/// What the player is told when the output went away, by a stream error.
const WENT_AWAY: &str = "the output went away";

/// What the player is told when the output stopped asking for sound.
const STALLED: &str = "the output stopped taking sound";

/// Which output a player renders through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Output {
    /// The system's default output device.
    Default,
    /// Nothing: frames are handed to no device, at `speed` times real
    /// time.
    Null {
        /// How many times faster than real time.
        speed: u32,
    },
}

impl Output {
    /// Whether this build can open the system's output device.
    pub const fn has_device() -> bool {
        cfg!(all(
            feature = "audio-output",
            any(target_os = "macos", target_os = "linux", windows)
        ))
    }
}

/// What keeps an output running. Dropping it stops the output.
trait Running: Send + std::fmt::Debug {}

/// Where a decode thread finds the track to join after the one it is
/// decoding. It looks once, as that one ends, and never waits: with none
/// there, it closes the handoff and finishes.
#[derive(Debug, Default)]
struct Handoff(Mutex<Slot>);

#[derive(Default)]
struct Slot {
    next: Option<(u64, Source)>,
    /// Set as the decode thread finishes: nothing more joins its ring.
    closed: bool,
}

impl std::fmt::Debug for Slot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Slot")
            .field("next", &self.next.as_ref().map(|(span, _)| span))
            .field("closed", &self.closed)
            .finish()
    }
}

impl Handoff {
    fn lock(&self) -> MutexGuard<'_, Slot> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Offers the track to join after the one being decoded. Whether it
    /// was taken: a thread that has finished takes nothing more.
    fn offer(&self, span: u64, source: Source) -> bool {
        let mut slot = self.lock();
        if !slot.closed {
            slot.next = Some((span, source));
        }
        !slot.closed
    }

    /// Takes back a track offered, if the thread has not taken it.
    fn withdraw(&self) -> bool {
        self.lock().next.take().is_some()
    }

    /// The track to join, or none and the handoff closed, at once.
    fn take_or_close(&self) -> Option<(u64, Source)> {
        let mut slot = self.lock();
        let next = slot.next.take();
        slot.closed |= next.is_none();
        next
    }

    fn close(&self) {
        self.lock().closed = true;
    }
}

/// A decode thread and the ring it fills: a track and those joined after
/// it. It starts when the output first plays, so a track loaded paused
/// takes the next one queued before its decoder can reach its end.
struct Decoder {
    spec: OutputSpec,
    stop: Arc<AtomicBool>,
    handoff: Arc<Handoff>,
    gate: Arc<Gate>,
    unstarted: Option<(Source, RingStream)>,
    thread: Option<JoinHandle<Result<(), RenderError>>>,
    failed: Option<String>,
}

impl std::fmt::Debug for Decoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Decoder")
            .field("started", &self.unstarted.is_none())
            .field("handoff", &self.handoff)
            .field("failed", &self.failed)
            .finish_non_exhaustive()
    }
}

impl Decoder {
    /// A decoder of `source`, not started, and the feed the callback plays
    /// its ring from.
    fn new(source: Source, spec: OutputSpec) -> (Self, Feed) {
        let capacity =
            usize::try_from(spec.rate * RING_MS / 1000).unwrap_or(24_000) * spec.channels;
        let (producer, ring) = RingBuffer::<f32>::new(capacity);
        let (marks, markers) = RingBuffer::<Marker>::new(4);
        let stop = Arc::new(AtomicBool::new(false));
        let handoff = Arc::new(Handoff::default());
        let gate = Arc::new(Gate::default());
        let stream = RingStream {
            producer,
            marks,
            handoff: Arc::clone(&handoff),
            stop: Arc::clone(&stop),
            channels: spec.channels,
        };
        let decoder = Self {
            spec,
            stop,
            handoff,
            gate: Arc::clone(&gate),
            unstarted: Some((source, stream)),
            thread: None,
            failed: None,
        };
        (
            decoder,
            Feed {
                ring,
                markers,
                gate,
            },
        )
    }

    /// Starts decoding, if it has not started.
    fn start(&mut self) {
        let Some((source, mut stream)) = self.unstarted.take() else {
            return;
        };
        let spec = self.spec;
        let spawned = std::thread::Builder::new()
            .name("ritornel-decode".to_owned())
            .spawn(move || {
                let stop = Arc::clone(&stream.stop);
                let decoded = decode::run_joined(source, spec, &stop, &mut stream);
                stream.handoff.close();
                decoded
                // The producer goes with the thread: the callback sees the
                // ring abandoned, and once it is empty the track has ended.
            });
        match spawned {
            Ok(thread) => self.thread = Some(thread),
            Err(error) => {
                self.handoff.close();
                self.failed = Some(error.to_string());
            }
        }
    }

    /// Stops the thread and waits for it.
    fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.unstarted = None;
        self.handoff.close();
        self.join();
    }

    /// Waits for the thread, keeping why it failed, if it did.
    fn join(&mut self) {
        if let Some(thread) = self.thread.take() {
            match thread.join() {
                Ok(Ok(())) => {}
                Ok(Err(error)) => self.failed = Some(error.to_string()),
                Err(_) => self.failed = Some("the decoder stopped unexpectedly".to_owned()),
            }
        }
    }
}

/// A track queued to follow the one in hand.
#[derive(Debug)]
struct Queued {
    span: u64,
    opened: Arc<Opened>,
    gain: ReplayGain,
}

/// A ring, the decoder filling it, and the tracks in it.
#[derive(Debug)]
struct Decoding {
    /// The span in hand, last, and the one that ran into it, if one did:
    /// each with its track's file, held open for a seek, and its gain.
    spans: Vec<(u64, Arc<Opened>, ReplayGain)>,
    decoder: Decoder,
    next: Option<Queued>,
    /// Whether the ring was cut where a track let go began: its decoder
    /// runs on into that track, and has not failed by stopping there.
    cut: bool,
}

/// The control side's ends of the queues to and from a callback.
#[derive(Debug)]
struct Ends {
    control: Producer<Message>,
    retired: Consumer<Feed>,
    events: Consumer<Event>,
}

/// The queues between the control side and a callback.
fn queues(spec: OutputSpec, shared: &Arc<Shared>) -> (Ends, Callback) {
    let (control, inbox) = RingBuffer::new(64);
    let (retire, retired) = RingBuffer::new(64);
    let (say, events) = RingBuffer::new(EVENTS);
    let callback = Callback::new(
        inbox,
        retire,
        say,
        Arc::clone(shared),
        spec.rate,
        spec.channels,
    );
    let ends = Ends {
        control,
        retired,
        events,
    };
    (ends, callback)
}

/// A renderer that plays through an output.
#[derive(Debug)]
pub struct AudioRenderer {
    name: String,
    spec: OutputSpec,
    ends: Ends,
    shared: Arc<Shared>,
    decoding: Option<Decoding>,
    gain: ReplayGain,
    /// Whether the output was last told to play.
    playing: bool,
    /// When it was, on the output's clock.
    played_at: u64,
    /// How long it may ask for no frames while playing: [`STALL`], but for
    /// a test.
    stall_after: Duration,
    /// What the output said, as the renderer has read it, and what the
    /// renderer says itself, in order, until drained.
    said: Vec<Event>,
    /// Spans the renderer took that have not ended.
    given: Vec<u64>,
    /// Why the last ring let go had stopped short, for an end of it the
    /// output says late.
    failed: Option<(u64, String)>,
    // Dropped last: the output stops once nothing feeds it. `None` once it
    // stopped answering and was let go.
    running: Option<Box<dyn Running>>,
}

impl std::fmt::Debug for Message {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Install { .. } => "Install",
            Self::Clear => "Clear",
            Self::Play => "Play",
            Self::Pause => "Pause",
            Self::Volume(_) => "Volume",
        })
    }
}

impl AudioRenderer {
    /// Opens an output.
    pub fn open(output: Output) -> Result<Self, RenderError> {
        match output {
            Output::Null { speed } => {
                let spec = OutputSpec {
                    rate: 48_000,
                    channels: 2,
                };
                Self::null(spec, speed, None, None)
            }
            Output::Default => Self::open_device(Arc::new(Shared::default())),
        }
    }

    fn null(
        spec: OutputSpec,
        speed: u32,
        tap: Option<null::Tap>,
        hold: Option<Arc<null::Hold>>,
    ) -> Result<Self, RenderError> {
        let shared = Arc::new(Shared::default());
        let (ends, callback) = queues(spec, &shared);
        let running = null::start(callback, spec, speed.max(1), tap, hold)?;
        Ok(Self::new(
            "null".to_owned(),
            spec,
            ends,
            shared,
            Box::new(running),
        ))
    }

    /// A null output of this rate and these channels that keeps every
    /// frame it is handed, for a test to read.
    #[cfg(test)]
    pub(crate) fn tapped(spec: OutputSpec, speed: u32) -> Result<(Self, null::Tap), RenderError> {
        let tap = null::Tap::default();
        Ok((Self::null(spec, speed, Some(Arc::clone(&tap)), None)?, tap))
    }

    /// As [`tapped`](Self::tapped), and the output holds once it has
    /// handed over `at` frames, until the test lets it go.
    #[cfg(test)]
    fn held(
        spec: OutputSpec,
        speed: u32,
        at: u64,
    ) -> Result<(Self, null::Tap, Arc<null::Hold>), RenderError> {
        let (tap, hold) = (null::Tap::default(), null::Hold::at(at));
        let renderer = Self::null(spec, speed, Some(Arc::clone(&tap)), Some(Arc::clone(&hold)))?;
        Ok((renderer, tap, hold))
    }

    #[cfg(all(
        feature = "audio-output",
        any(target_os = "macos", target_os = "linux", windows)
    ))]
    fn open_device(shared: Arc<Shared>) -> Result<Self, RenderError> {
        let (running, spec, ends) = device::start(&shared, queues)?;
        Ok(Self::new(
            running.name(),
            spec,
            ends,
            shared,
            Box::new(running),
        ))
    }

    #[cfg(not(all(
        feature = "audio-output",
        any(target_os = "macos", target_os = "linux", windows)
    )))]
    fn open_device(_shared: Arc<Shared>) -> Result<Self, RenderError> {
        Err(RenderError::new(
            "output",
            "this build has no audio output: it was built without the audio-output feature, or \
             for a system it does not cover",
        ))
    }

    fn new(
        name: String,
        spec: OutputSpec,
        ends: Ends,
        shared: Arc<Shared>,
        running: Box<dyn Running>,
    ) -> Self {
        Self {
            name,
            spec,
            ends,
            shared,
            decoding: None,
            gain: ReplayGain::Off,
            playing: false,
            played_at: 0,
            stall_after: STALL,
            said: Vec::new(),
            given: Vec::new(),
            failed: None,
            running: Some(running),
        }
    }

    /// Why the output can play no more, if it cannot: it went away, it
    /// stopped answering, or it stopped asking for frames while told to
    /// play. A stall is marked lost here, for good, as a stream error is
    /// marked where it is heard, so a device that comes back is not played
    /// through until the output is opened again.
    fn lost(&self) -> Option<&'static str> {
        if self.running.is_none() {
            return Some("the output stopped answering");
        }
        if self.stalled() {
            self.shared.stalled.store(true, Ordering::SeqCst);
        }
        if self.shared.stalled.load(Ordering::SeqCst) {
            Some(STALLED)
        } else if self.shared.lost.load(Ordering::SeqCst) {
            Some(WENT_AWAY)
        } else {
            None
        }
    }

    /// Whether the output, told to play, has asked for no frames for
    /// [`stall_after`](Self::stall_after): since it last asked, or since it
    /// was told to play if that was later.
    fn stalled(&self) -> bool {
        if !self.playing {
            return false;
        }
        // Now first: a callback that runs between the two reads then counts.
        let now = output::now();
        let asked = self.shared.asked_until.load(Ordering::SeqCst);
        let since = asked.max(self.played_at);
        Duration::from_nanos(now.saturating_sub(since)) >= self.stall_after
    }

    /// When the output last took frames, on its clock: when the last it
    /// asked for were done, or now if it never asked or they are not done
    /// yet. `heard` is when the last frame of a span was done, where that is
    /// known, which leaves out any silence the output took after it.
    fn last_taken(&self, heard: u64) -> u64 {
        let now = output::now();
        let asked = self.shared.asked_until.load(Ordering::SeqCst);
        match (heard, asked) {
            (0, 0) => now,
            (0, at) | (at, _) => at.min(now),
        }
    }

    /// Sends a message to the callback, and says whether it went. The
    /// queue is far larger than the messages in flight; a full one waits
    /// for the callback to read, and one that stays full is an output that
    /// stopped answering.
    fn send(&mut self, mut message: Message) -> bool {
        let deadline = Instant::now() + OUTPUT_WAIT;
        loop {
            match self.ends.control.push(message) {
                Ok(()) => return true,
                Err(rtrb::PushError::Full(back)) if Instant::now() < deadline => {
                    message = back;
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(_) => {
                    self.abandon();
                    return false;
                }
            }
        }
    }

    /// Waits until the callback has taken span `span` in.
    fn wait_for(&mut self, span: u64) -> Result<(), RenderError> {
        let deadline = Instant::now() + OUTPUT_WAIT;
        while self.shared.progress().0 != span {
            if self.lost().is_some() || Instant::now() >= deadline {
                let error = RenderError::new(
                    "output",
                    format!(
                        "the output is not running: {} callbacks so far, {} stream errors",
                        self.shared.callbacks.load(Ordering::Relaxed),
                        self.shared.stream_errors.load(Ordering::Relaxed)
                    ),
                );
                self.abandon();
                return Err(error);
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        Ok(())
    }

    /// The output stopped answering: it is let go, so that it says nothing
    /// more, and each span it had ends here with what it was seen to play.
    fn abandon(&mut self) {
        if let Some(decoding) = self.decoding.take() {
            self.retire(decoding);
        }
        self.running = None;
        self.collect();
        let (installed, consumed, heard) = self.shared.heard();
        // Its spans end when it last took frames, as its own ends do: for
        // one that stalled, seconds before it was let go.
        for span in self.given.clone() {
            let (frames, at) = if span == installed {
                (consumed, self.last_taken(heard))
            } else {
                (0, self.last_taken(0))
            };
            self.ended_at(span, frames, at);
        }
    }

    /// Reads what the output said, in order: a span that ran into the one
    /// queued makes that the track in hand, and one whose audio ran out
    /// because its decoder failed says so.
    fn collect(&mut self) {
        while let Ok(mut feed) = self.ends.retired.pop() {
            // Marked by a decoder after the output let its ring go.
            while let Ok(marker) = feed.markers.pop() {
                self.ended_here(marker.span, 0);
            }
        }
        while let Ok(event) = self.ends.events.pop() {
            let event = match event {
                Event::Ended {
                    span,
                    frames,
                    at,
                    end,
                } => {
                    self.given.retain(|given| *given != span);
                    let end = match end {
                        End::Joined { next } => {
                            self.joined(next);
                            End::Joined { next }
                        }
                        End::Drained => self.failure(span).map_or(End::Drained, End::Failed),
                        end => end,
                    };
                    Event::Ended {
                        span,
                        frames,
                        at,
                        end,
                    }
                }
                began @ Event::Began { .. } => began,
            };
            self.said.push(event);
        }
    }

    fn joined(&mut self, next: u64) {
        let Some(decoding) = &mut self.decoding else {
            return;
        };
        if let Some(queued) = decoding.next.take_if(|queued| queued.span == next) {
            decoding
                .spans
                .push((queued.span, queued.opened, queued.gain));
            if decoding.spans.len() > 2 {
                decoding.spans.remove(0);
            }
        }
    }

    /// Why the span's decoder stopped short, if it did. Its audio has run
    /// out, so its thread has ended.
    fn failure(&mut self, span: u64) -> Option<String> {
        if let Some((_, why)) = self.failed.take_if(|(failed, _)| *failed == span) {
            return Some(why);
        }
        let decoding = self.decoding.as_mut()?;
        if decoding.cut || decoding.spans.last().is_none_or(|(last, ..)| *last != span) {
            return None;
        }
        decoding.decoder.join();
        decoding.decoder.failed.clone()
    }

    /// A span the output will say nothing more of ends here, if it has not
    /// ended already.
    fn ended_here(&mut self, span: u64, frames: u64) {
        self.ended_at(span, frames, output::now());
    }

    /// As [`ended_here`](Self::ended_here), with its last frame done at
    /// `at`.
    fn ended_at(&mut self, span: u64, frames: u64, at: u64) {
        if !self.given.contains(&span) {
            return;
        }
        self.given.retain(|given| *given != span);
        self.said.push(Event::Ended {
            span,
            frames,
            at,
            end: End::Replaced,
        });
    }

    /// Stops a ring's decoder once the output has let the ring go, so that
    /// it never runs dry while it plays, and lets a track queued after it
    /// go: one the decoder never took ends here.
    fn retire(&mut self, mut decoding: Decoding) {
        decoding.decoder.stop();
        if let Some(queued) = decoding.next.take()
            && decoding.decoder.handoff.withdraw()
        {
            self.ended_here(queued.span, 0);
        }
        if let (Some(why), Some((span, ..))) = (decoding.decoder.failed, decoding.spans.last()) {
            self.failed = Some((*span, why));
        }
        self.collect();
    }
    /// Opens a track's file at `start_ms`, at the ReplayGain `gain` asks
    /// for.
    fn source(opened: &Opened, start_ms: u64, gain: ReplayGain) -> Result<Source, RenderError> {
        let mut source = Source::of(opened, start_ms)?;
        source.set_replay_gain(gain);
        Ok(source)
    }

    /// Starts span `span` of a track's file, already open at its position,
    /// and hands its ring to the callback.
    fn begin(
        &mut self,
        span: u64,
        opened: Arc<Opened>,
        gain: ReplayGain,
        source: Source,
        fade_in: bool,
    ) -> Result<(), RenderError> {
        let (mut decoder, feed) = Decoder::new(source, self.spec);
        if self.playing {
            decoder.start();
        }
        self.decoding = Some(Decoding {
            spans: vec![(span, opened, gain)],
            decoder,
            next: None,
            cut: false,
        });
        self.given.push(span);
        if !self.send(Message::Install {
            span,
            feed,
            fade_in,
        }) {
            return Err(RenderError::new("output", "the output stopped answering"));
        }
        self.wait_for(span)
    }
}

/// What the decode thread writes into: the ring, its markers, and where a
/// next track is handed over.
struct RingStream {
    producer: Producer<f32>,
    marks: Producer<Marker>,
    handoff: Arc<Handoff>,
    stop: Arc<AtomicBool>,
    channels: usize,
}

impl decode::Stream for RingStream {
    fn write(&mut self, frames: &[f32]) -> bool {
        fill(&mut self.producer, frames, self.channels, &self.stop)
    }

    fn next(&mut self) -> Option<(u64, Source)> {
        if self.stop.load(Ordering::SeqCst) {
            return None;
        }
        self.handoff.take_or_close()
    }

    fn mark(&mut self, at: u64, span: u64) {
        // One track is queued at a time, so there is always room.
        let _ = self.marks.push(Marker { at, span });
    }
}

/// Writes frames into the ring, waiting while it is full, until all are in
/// or `stop` is set. Only whole frames are written.
fn fill(producer: &mut Producer<f32>, frames: &[f32], channels: usize, stop: &AtomicBool) -> bool {
    let mut rest = frames;
    while !rest.is_empty() {
        if stop.load(Ordering::SeqCst) {
            return false;
        }
        let room = producer.slots().min(rest.len());
        let room = room - room % channels.max(1);
        if room == 0 {
            std::thread::sleep(Duration::from_millis(5));
            continue;
        }
        let Ok(mut chunk) = producer.write_chunk(room) else {
            continue;
        };
        let (first, second) = chunk.as_mut_slices();
        let (head, tail) = rest.split_at(room);
        let (into_first, into_second) = head.split_at(first.len().min(head.len()));
        first.copy_from_slice(into_first);
        if let Some(second) = second.get_mut(..into_second.len()) {
            second.copy_from_slice(into_second);
        }
        chunk.commit_all();
        rest = tail;
    }
    true
}

impl Renderer for AudioRenderer {
    fn rate(&self) -> u32 {
        self.spec.rate
    }

    fn name(&self) -> String {
        self.name.clone()
    }

    fn channels(&self) -> u32 {
        u32::try_from(self.spec.channels).unwrap_or(2)
    }

    fn load(
        &mut self,
        path: &Path,
        facts: FileFacts,
        start_ms: u64,
        span: u64,
    ) -> Result<Format, RenderError> {
        if self.decoding.is_some() {
            self.unload();
        }
        if self.running.is_none() {
            return Err(RenderError::new("output", "the output stopped answering"));
        }
        let opened = Arc::new(Opened::open(path, Some(facts))?);
        let source = Self::source(&opened, start_ms, self.gain)?;
        let format = source.info.clone();
        // A track is loaded paused. A seek keeps whatever the output was
        // doing, so the pause is said here and not in the ring's change.
        self.pause();
        self.begin(span, opened, self.gain, source, start_ms > 0)?;
        Ok(format)
    }

    fn play(&mut self) {
        self.playing = true;
        self.played_at = output::now();
        if let Some(decoding) = &mut self.decoding {
            decoding.decoder.start();
        }
        self.send(Message::Play);
    }

    fn pause(&mut self) {
        self.playing = false;
        self.send(Message::Pause);
    }

    fn seek(&mut self, of: u64, position_ms: u64, span: u64) -> Result<(), RenderError> {
        self.collect();
        let track = self.decoding.as_ref().and_then(|decoding| {
            let (_, opened, gain) = decoding.spans.iter().find(|(span, ..)| *span == of)?;
            Some((Arc::clone(opened), *gain))
        });
        let Some((opened, gain)) = track else {
            return Err(RenderError::new("output", "that track is not loaded"));
        };
        // The new position is opened before the span in hand is touched,
        // so a seek that cannot open it leaves that span playing on. It
        // reads the file the track was loaded from, which must not have
        // been written to since.
        opened.unchanged()?;
        let source = Self::source(&opened, position_ms, gain)?;
        let old = self.decoding.take();
        let begun = self.begin(span, opened, gain, source, true);
        if let Some(old) = old {
            self.retire(old);
        }
        begun
    }

    fn unload(&mut self) {
        self.collect();
        let Some(old) = self.decoding.take() else {
            return;
        };
        if self.lost().is_some() {
            self.decoding = Some(old);
            self.abandon();
            return;
        }
        if self.send(Message::Clear) {
            // On a timeout the output is let go, and says no more.
            let _ = self.wait_for(0);
        }
        self.retire(old);
    }

    fn drain(&mut self) -> Vec<Event> {
        self.collect();
        std::mem::take(&mut self.said)
    }

    fn progress(&self) -> Progress {
        let (span, frames) = self.shared.progress();
        Progress {
            span,
            frames,
            lost: self.lost(),
        }
    }

    fn now(&self) -> u64 {
        output::now()
    }

    fn set_replay_gain(&mut self, gain: ReplayGain) {
        self.gain = gain;
    }

    fn set_volume(&mut self, volume: f32) {
        self.send(Message::Volume(volume));
    }

    fn queue_next(
        &mut self,
        path: &Path,
        facts: FileFacts,
        span: u64,
    ) -> Result<Option<Format>, RenderError> {
        self.collect();
        if self
            .decoding
            .as_ref()
            .is_none_or(|decoding| decoding.next.is_some())
        {
            return Ok(None);
        }
        let opened = Arc::new(Opened::open(path, Some(facts))?);
        let source = Self::source(&opened, 0, self.gain)?;
        let format = source.info.clone();
        let gain = self.gain;
        let Some(decoding) = &mut self.decoding else {
            return Ok(None);
        };
        // A decoder that has finished takes nothing more: the track loads
        // once this one's audio runs out.
        decoding.decoder.gate.open(span);
        if !decoding.decoder.handoff.offer(span, source) {
            return Ok(None);
        }
        decoding.next = Some(Queued { span, opened, gain });
        self.given.push(span);
        Ok(Some(format))
    }

    fn cancel_next(&mut self) -> bool {
        self.collect();
        // The output has run into the next, and the player has not heard.
        if ran_on(&self.said) {
            return false;
        }
        let Some(decoding) = &mut self.decoding else {
            return true;
        };
        let Some(span) = decoding.next.as_ref().map(|queued| queued.span) else {
            return true;
        };
        // Its decoder may have it, and still the output has not run into
        // it: the ring is cut there, and its decoder takes nothing more.
        let taken = !decoding.decoder.handoff.withdraw();
        if !decoding.decoder.gate.close(span) {
            return false;
        }
        if taken {
            decoding.decoder.handoff.close();
            decoding.cut = true;
        }
        decoding.next = None;
        self.ended_here(span, 0);
        true
    }
}

impl Drop for AudioRenderer {
    fn drop(&mut self) {
        if let Some(mut decoding) = self.decoding.take() {
            decoding.decoder.stop();
        }
    }
}

/// An output that is opened the first time a track is loaded, so that a
/// server restored paused, or one that is never asked to play, never opens
/// a device. One that goes away is let go, and the next load opens the
/// system's output again.
#[derive(Debug)]
pub struct OnDemand {
    output: Output,
    opened: Option<AudioRenderer>,
    gain: ReplayGain,
    volume: f32,
    /// What an output let go of said last.
    said: Vec<Event>,
}

impl OnDemand {
    /// The output, not yet open.
    pub fn new(output: Output) -> Self {
        Self {
            output,
            opened: None,
            gain: ReplayGain::Off,
            volume: 1.0,
            said: Vec::new(),
        }
    }

    fn let_go_if_lost(&mut self) {
        if let Some(mut lost) = self.opened.take_if(|opened| opened.lost().is_some()) {
            lost.unload();
            self.said.extend(lost.drain());
        }
    }
}

impl Renderer for OnDemand {
    fn rate(&self) -> u32 {
        self.opened.as_ref().map_or(0, Renderer::rate)
    }

    fn name(&self) -> String {
        match (&self.opened, self.output) {
            (Some(opened), _) => opened.name(),
            (None, Output::Default) => "default".to_owned(),
            (None, Output::Null { .. }) => "null".to_owned(),
        }
    }

    fn channels(&self) -> u32 {
        self.opened.as_ref().map_or(0, Renderer::channels)
    }

    fn load(
        &mut self,
        path: &Path,
        facts: FileFacts,
        start_ms: u64,
        span: u64,
    ) -> Result<Format, RenderError> {
        self.let_go_if_lost();
        let opened = match &mut self.opened {
            Some(opened) => opened,
            None => {
                let mut opened = AudioRenderer::open(self.output)?;
                opened.set_volume(self.volume);
                self.opened.insert(opened)
            }
        };
        opened.set_replay_gain(self.gain);
        opened.load(path, facts, start_ms, span)
    }

    fn play(&mut self) {
        if let Some(opened) = &mut self.opened {
            opened.play();
        }
    }

    fn pause(&mut self) {
        if let Some(opened) = &mut self.opened {
            opened.pause();
        }
    }

    fn seek(&mut self, of: u64, position_ms: u64, span: u64) -> Result<(), RenderError> {
        match &mut self.opened {
            Some(opened) => opened.seek(of, position_ms, span),
            None => Err(RenderError::new("output", "no output is open")),
        }
    }

    fn unload(&mut self) {
        if let Some(opened) = &mut self.opened {
            opened.unload();
        }
        self.let_go_if_lost();
    }

    fn drain(&mut self) -> Vec<Event> {
        let mut said = std::mem::take(&mut self.said);
        if let Some(opened) = &mut self.opened {
            said.extend(opened.drain());
        }
        said
    }

    fn progress(&self) -> Progress {
        self.opened
            .as_ref()
            .map_or_else(Progress::default, Renderer::progress)
    }

    fn now(&self) -> u64 {
        output::now()
    }

    fn set_replay_gain(&mut self, gain: ReplayGain) {
        self.gain = gain;
    }

    fn set_volume(&mut self, volume: f32) {
        self.volume = volume;
        if let Some(opened) = &mut self.opened {
            opened.set_volume(volume);
        }
    }

    fn queue_next(
        &mut self,
        path: &Path,
        facts: FileFacts,
        span: u64,
    ) -> Result<Option<Format>, RenderError> {
        match &mut self.opened {
            Some(opened) => {
                opened.set_replay_gain(self.gain);
                opened.queue_next(path, facts, span)
            }
            None => Ok(None),
        }
    }

    fn cancel_next(&mut self) -> bool {
        !ran_on(&self.said) && self.opened.as_mut().is_none_or(Renderer::cancel_next)
    }
}

#[cfg(test)]
mod tests;
