//! The audio pipeline: a decode thread per segment, a ring, and an output
//! that drives the real-time callback. See "The pipeline" in
//! docs/design/playback.md.
//!
//! ```text
//! control (the player)   load / seek / unload, play / pause, poll
//!    │ messages and rings, through a lock-free queue
//! decode thread          file → Symphonia → channels → rubato → ring (about 0.5 s)
//!    │
//! output callback        ring → fade → device; counts frames handed over
//! ```
//!
//! The output is the system's default device ([`Output::Default`], where
//! the build has one) or the null output, which hands frames to nothing at
//! real-time speed or a multiple of it, for the tests and for a run that
//! must make no sound.

pub(crate) mod decode;
#[cfg(all(feature = "audio-output", target_os = "macos"))]
mod device;
mod null;
pub(crate) mod output;

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use rtrb::{Consumer, Producer, RingBuffer};

use self::decode::{Opened, OutputSpec, Source};
use self::output::{Callback, Message, Shared};
use crate::playback::renderer::{FileFacts, Format, RenderError, Renderer, Segment};

/// How much audio the ring between the decode thread and the output holds.
const RING_MS: u32 = 500;

/// How long a change waits for the output to take it.
const OUTPUT_WAIT: Duration = Duration::from_secs(2);

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
        cfg!(all(feature = "audio-output", target_os = "macos"))
    }
}

/// What keeps an output running. Dropping it stops the output.
trait Running: Send + std::fmt::Debug {}

/// The segment the decode thread is filling.
#[derive(Debug)]
struct Decoding {
    segment: u64,
    /// The track's file, held open for every segment.
    opened: Arc<Opened>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<Result<(), RenderError>>>,
    failed: Option<String>,
}

impl Decoding {
    /// Stops the decode thread and waits for it.
    fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.join();
    }

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

/// A renderer that plays through an output.
#[derive(Debug)]
pub struct AudioRenderer {
    name: String,
    spec: OutputSpec,
    control: Producer<Message>,
    retired: Consumer<Consumer<f32>>,
    shared: Arc<Shared>,
    next_segment: u64,
    decoding: Option<Decoding>,
    // Dropped last: the output stops once nothing feeds it.
    _running: Box<dyn Running>,
}

impl std::fmt::Debug for Message {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Install { .. } => "Install",
            Self::Clear => "Clear",
            Self::Play => "Play",
            Self::Pause => "Pause",
        })
    }
}

/// The two queues between the control side and a callback.
fn queues(
    spec: OutputSpec,
    shared: &Arc<Shared>,
) -> (Producer<Message>, Consumer<Consumer<f32>>, Callback) {
    let (control, inbox) = RingBuffer::new(64);
    let (retire, retired) = RingBuffer::new(64);
    let callback = Callback::new(inbox, retire, Arc::clone(shared), spec.rate, spec.channels);
    (control, retired, callback)
}

impl AudioRenderer {
    /// Opens an output.
    pub fn open(output: Output) -> Result<Self, RenderError> {
        let shared = Arc::new(Shared::default());
        match output {
            Output::Null { speed } => {
                let spec = OutputSpec {
                    rate: 48_000,
                    channels: 2,
                };
                let (control, retired, callback) = queues(spec, &shared);
                let running = null::start(callback, spec, speed.max(1))?;
                Ok(Self::new(
                    "null".to_owned(),
                    spec,
                    control,
                    retired,
                    shared,
                    Box::new(running),
                ))
            }
            Output::Default => Self::open_device(shared),
        }
    }

    #[cfg(all(feature = "audio-output", target_os = "macos"))]
    fn open_device(shared: Arc<Shared>) -> Result<Self, RenderError> {
        let (running, spec, control, retired) = device::start(&shared, queues)?;
        Ok(Self::new(
            running.name(),
            spec,
            control,
            retired,
            shared,
            Box::new(running),
        ))
    }

    #[cfg(not(all(feature = "audio-output", target_os = "macos")))]
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
        control: Producer<Message>,
        retired: Consumer<Consumer<f32>>,
        shared: Arc<Shared>,
        running: Box<dyn Running>,
    ) -> Self {
        Self {
            name,
            spec,
            control,
            retired,
            shared,
            next_segment: 1,
            decoding: None,
            _running: running,
        }
    }

    /// Sends a message to the callback. The queue is far larger than the
    /// messages in flight; a full one waits for the callback to read.
    fn send(&mut self, mut message: Message) {
        let deadline = Instant::now() + OUTPUT_WAIT;
        loop {
            match self.control.push(message) {
                Ok(()) => return,
                Err(rtrb::PushError::Full(back)) if Instant::now() < deadline => {
                    message = back;
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(_) => return,
            }
        }
    }

    /// Waits until the callback has taken segment `segment` in.
    fn wait_for(&mut self, segment: u64) -> Result<(), RenderError> {
        let deadline = Instant::now() + OUTPUT_WAIT;
        while self.shared.installed.load(Ordering::SeqCst) != segment {
            if Instant::now() >= deadline {
                return Err(RenderError::new(
                    "output",
                    format!(
                        "the output is not running: {} callbacks so far, {} stream errors",
                        self.shared.callbacks.load(Ordering::Relaxed),
                        self.shared.stream_errors.load(Ordering::Relaxed)
                    ),
                ));
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        // The rings the callback has let go of are freed here, not there.
        while self.retired.pop().is_ok() {}
        Ok(())
    }

    /// Starts a segment of the track's file, already open at its position,
    /// and hands its ring to the callback.
    fn begin(&mut self, opened: Arc<Opened>, source: Source) -> Result<(), RenderError> {
        let segment = self.next_segment;
        self.next_segment += 1;
        let spec = self.spec;
        let capacity =
            usize::try_from(spec.rate * RING_MS / 1000).unwrap_or(24_000) * spec.channels;
        let (mut producer, ring) = RingBuffer::<f32>::new(capacity);
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let stop = Arc::clone(&stop);
            std::thread::Builder::new()
                .name("ritornel-decode".to_owned())
                .spawn(move || {
                    let mut sink =
                        |frames: &[f32]| fill(&mut producer, frames, spec.channels, &stop);
                    decode::run(source, spec, &stop, &mut sink).map(|_| ())
                    // The producer goes with the thread: the callback sees the
                    // ring abandoned, and once it is empty the track has ended.
                })
                .map_err(|error| RenderError::new("output", error.to_string()))?
        };
        self.decoding = Some(Decoding {
            segment,
            opened,
            stop,
            thread: Some(thread),
            failed: None,
        });
        self.send(Message::Install { segment, ring });
        self.wait_for(segment)
    }

    fn stop_decoding(&mut self) -> Option<Decoding> {
        let mut decoding = self.decoding.take()?;
        decoding.stop();
        Some(decoding)
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
    ) -> Result<Format, RenderError> {
        if self.decoding.is_some() {
            self.unload();
        }
        let opened = Arc::new(Opened::open(path, Some(facts))?);
        let source = Source::of(&opened, start_ms)?;
        let format = source.info.clone();
        // A track is loaded paused. A seek keeps whatever the output was
        // doing, so the pause is said here and not in the ring's change.
        self.send(Message::Pause);
        self.begin(opened, source)?;
        Ok(format)
    }

    fn play(&mut self) {
        self.send(Message::Play);
    }

    fn pause(&mut self) {
        self.send(Message::Pause);
    }

    fn seek(&mut self, position_ms: u64) -> Result<Segment, RenderError> {
        let Some(opened) = self
            .decoding
            .as_ref()
            .map(|decoding| Arc::clone(&decoding.opened))
        else {
            return Ok(Segment::default());
        };
        // The new position is opened before the segment in hand is
        // touched, so a seek that cannot open it leaves that segment
        // playing on, and poll and unload count it as before. It reads the
        // file the track was loaded from, which must not have been written
        // to since.
        opened.unchanged()?;
        let source = Source::of(&opened, position_ms)?;
        let Some(mut old) = self.stop_decoding() else {
            return Ok(Segment::default());
        };
        if let Err(error) = self.begin(opened, source) {
            // The output did not take the new segment. The old one, its
            // decoding stopped, is still the one poll and unload count,
            // and it fails once what it had buffered has been heard.
            old.failed = Some(error.to_string());
            if let Some(mut new) = self.decoding.replace(old) {
                new.stop();
            }
            return Err(error);
        }
        Ok(Segment {
            frames: self.shared.previous_final.load(Ordering::SeqCst),
            ended: false,
            failed: None,
            output_lost: self.shared.lost.load(Ordering::SeqCst),
        })
    }

    fn unload(&mut self) -> Segment {
        let Some(old) = self.stop_decoding() else {
            return Segment::default();
        };
        self.send(Message::Clear);
        let cleared = self.wait_for(0);
        let frames = if cleared.is_ok() {
            self.shared.previous_final.load(Ordering::SeqCst)
        } else {
            self.shared.consumed.load(Ordering::SeqCst)
        };
        Segment {
            frames,
            ended: self.shared.drained.load(Ordering::SeqCst) == old.segment,
            failed: old.failed,
            output_lost: self.shared.lost.load(Ordering::SeqCst),
        }
    }

    fn poll(&mut self) -> Segment {
        let Some(decoding) = &mut self.decoding else {
            return Segment::default();
        };
        let segment = decoding.segment;
        if decoding
            .thread
            .as_ref()
            .is_some_and(std::thread::JoinHandle::is_finished)
        {
            decoding.join();
        }
        let installed = self.shared.installed.load(Ordering::SeqCst) == segment;
        let drained = self.shared.drained.load(Ordering::SeqCst) == segment;
        Segment {
            frames: if installed {
                self.shared.consumed.load(Ordering::SeqCst)
            } else {
                0
            },
            // A track that failed is reported once what was decoded of it
            // has been heard.
            ended: drained && decoding.failed.is_none(),
            failed: decoding.failed.clone().filter(|_| drained),
            output_lost: self.shared.lost.load(Ordering::SeqCst),
        }
    }
}

impl Drop for AudioRenderer {
    fn drop(&mut self) {
        self.stop_decoding();
    }
}

/// An output that is opened the first time a track is loaded, so that a
/// server restored paused, or one that is never asked to play, never opens
/// a device.
#[derive(Debug)]
pub struct OnDemand {
    output: Output,
    opened: Option<AudioRenderer>,
}

impl OnDemand {
    /// The output, not yet open.
    pub fn new(output: Output) -> Self {
        Self {
            output,
            opened: None,
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
    ) -> Result<Format, RenderError> {
        let opened = match &mut self.opened {
            Some(opened) => opened,
            None => self.opened.insert(AudioRenderer::open(self.output)?),
        };
        opened.load(path, facts, start_ms)
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

    fn seek(&mut self, position_ms: u64) -> Result<Segment, RenderError> {
        match &mut self.opened {
            Some(opened) => opened.seek(position_ms),
            None => Ok(Segment::default()),
        }
    }

    fn unload(&mut self) -> Segment {
        let Some(opened) = &mut self.opened else {
            return Segment::default();
        };
        let last = opened.unload();
        // An output that went away is let go, and the next load opens the
        // system's output again.
        if last.output_lost {
            self.opened = None;
        }
        last
    }

    fn poll(&mut self) -> Segment {
        self.opened
            .as_mut()
            .map_or_else(Segment::default, Renderer::poll)
    }
}

#[cfg(test)]
mod tests;
