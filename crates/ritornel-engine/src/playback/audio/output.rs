//! The output side of the pipeline: what runs in the output's real-time
//! callback. It never locks, allocates or does I/O. It takes messages and
//! rings from a lock-free queue, pops frames from the ring of the segment
//! it was given, ramps the gain in and out over about 20 ms, and counts the
//! frames it hands to the output. Position and listening time come from
//! that count. See "The pipeline" in docs/design/playback.md.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use rtrb::{Consumer, Producer};

/// How long a fade in or out lasts.
const RAMP_MS: u32 = 20;

/// What the control side tells the callback.
pub(crate) enum Message {
    /// Play this ring as segment `segment` from now on. The ring before it
    /// is handed back.
    Install { segment: u64, ring: Consumer<f32> },
    /// Play nothing.
    Clear,
    /// Hand frames to the output, fading in.
    Play,
    /// Fade out, then hand nothing more until `Play`.
    Pause,
}

/// What the callback tells the control side.
#[derive(Debug, Default)]
pub(crate) struct Shared {
    /// The segment the callback is playing; 0 for none.
    pub(crate) installed: AtomicU64,
    /// Frames of that segment handed to the output.
    pub(crate) consumed: AtomicU64,
    /// Frames the segment before it had been handed when it was replaced.
    pub(crate) previous_final: AtomicU64,
    /// The last segment whose ring ran empty after its producer finished.
    pub(crate) drained: AtomicU64,
    /// Callbacks run, so that the control side can tell the output is
    /// alive.
    pub(crate) callbacks: AtomicU64,
    /// Errors the output's stream reported.
    pub(crate) stream_errors: AtomicU64,
    /// Whether the stream reported an error it cannot go on from: its
    /// device went away or its configuration is no longer valid.
    pub(crate) lost: AtomicBool,
}

/// The state that lives in the callback.
pub(crate) struct Callback {
    inbox: Consumer<Message>,
    retired: Producer<Consumer<f32>>,
    shared: Arc<Shared>,
    channels: usize,
    ring: Option<Consumer<f32>>,
    segment: u64,
    consumed: u64,
    gain: f32,
    target: f32,
    step: f32,
}

impl std::fmt::Debug for Callback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Callback")
            .field("segment", &self.segment)
            .field("consumed", &self.consumed)
            .finish_non_exhaustive()
    }
}

impl Callback {
    pub(crate) fn new(
        inbox: Consumer<Message>,
        retired: Producer<Consumer<f32>>,
        shared: Arc<Shared>,
        rate: u32,
        channels: usize,
    ) -> Self {
        let ramp_frames = (rate.max(1) * RAMP_MS / 1000).max(1);
        Self {
            inbox,
            retired,
            shared,
            channels: channels.max(1),
            ring: None,
            segment: 0,
            consumed: 0,
            gain: 0.0,
            target: 0.0,
            step: 1.0 / ramp_frames as f32,
        }
    }

    /// Hands the old ring back to the control side, which frees it.
    fn retire(&mut self, ring: Consumer<f32>) {
        // The queue back holds far more rings than can be in flight, and
        // the control side empties it after each change. Were it full, the
        // ring would be freed here, which is slow and not wrong.
        let _ = self.retired.push(ring);
    }

    fn take_messages(&mut self) {
        while let Ok(message) = self.inbox.pop() {
            match message {
                Message::Install { segment, ring } => {
                    self.shared
                        .previous_final
                        .store(self.consumed, Ordering::SeqCst);
                    if let Some(old) = self.ring.replace(ring) {
                        self.retire(old);
                    }
                    self.segment = segment;
                    self.consumed = 0;
                    self.gain = 0.0;
                    self.shared.consumed.store(0, Ordering::SeqCst);
                    self.shared.installed.store(segment, Ordering::SeqCst);
                }
                Message::Clear => {
                    self.shared
                        .previous_final
                        .store(self.consumed, Ordering::SeqCst);
                    if let Some(old) = self.ring.take() {
                        self.retire(old);
                    }
                    self.segment = 0;
                    self.consumed = 0;
                    self.shared.consumed.store(0, Ordering::SeqCst);
                    self.shared.installed.store(0, Ordering::SeqCst);
                }
                Message::Play => self.target = 1.0,
                Message::Pause => self.target = 0.0,
            }
        }
    }

    /// Fills `out`, interleaved in the output's channels.
    pub(crate) fn render(&mut self, out: &mut [f32]) {
        self.take_messages();
        self.shared.callbacks.fetch_add(1, Ordering::Relaxed);
        let channels = self.channels;
        let mut written = 0;
        for frame in out.chunks_exact_mut(channels) {
            let silent = self.gain <= 0.0 && self.target <= 0.0;
            let ring = match &mut self.ring {
                Some(ring) if !silent && ring.slots() >= channels => ring,
                _ => {
                    frame.fill(0.0);
                    continue;
                }
            };
            let Ok(chunk) = ring.read_chunk(channels) else {
                frame.fill(0.0);
                continue;
            };
            let (first, second) = chunk.as_slices();
            for (at, sample) in first.iter().chain(second).enumerate() {
                if let Some(slot) = frame.get_mut(at) {
                    *slot = sample * self.gain;
                }
            }
            chunk.commit_all();
            written += 1;
            if self.gain < self.target {
                self.gain = (self.gain + self.step).min(self.target);
            } else if self.gain > self.target {
                self.gain = (self.gain - self.step).max(self.target);
            }
        }
        // A remainder shorter than a frame is never asked for, and is left
        // silent if it is.
        let whole = out.len() / channels * channels;
        if let Some(rest) = out.get_mut(whole..) {
            rest.fill(0.0);
        }
        self.consumed += written;
        self.shared.consumed.store(self.consumed, Ordering::SeqCst);
        if let Some(ring) = &self.ring
            && ring.is_abandoned()
            && ring.slots() < channels
        {
            self.shared.drained.store(self.segment, Ordering::SeqCst);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn callback(channels: usize) -> (Callback, Producer<Message>, Arc<Shared>) {
        let (to_callback, inbox) = rtrb::RingBuffer::new(8);
        let (retired, _) = rtrb::RingBuffer::new(8);
        let shared = Arc::new(Shared::default());
        let callback = Callback::new(inbox, retired, Arc::clone(&shared), 1000, channels);
        (callback, to_callback, shared)
    }

    #[test]
    fn frames_are_counted_as_they_are_handed_to_the_output_and_not_while_paused() {
        let (mut callback, mut control, shared) = callback(2);
        let (mut producer, ring) = rtrb::RingBuffer::new(64);
        for _ in 0..40 {
            producer.push(0.5).unwrap();
        }
        let _ = control.push(Message::Install { segment: 7, ring });
        let mut out = [1.0; 8];
        // Installed and not yet playing: silence, and nothing counted.
        callback.render(&mut out);
        assert_eq!(out, [0.0; 8]);
        assert_eq!(shared.installed.load(Ordering::SeqCst), 7);
        assert_eq!(shared.consumed.load(Ordering::SeqCst), 0);

        let _ = control.push(Message::Play);
        callback.render(&mut out);
        assert_eq!(shared.consumed.load(Ordering::SeqCst), 4);
        // The fade in starts from silence.
        assert_eq!(out[0], 0.0);
        assert!(out[6] > 0.0);

        // The producer finishes: the ring drains and the segment says so.
        drop(producer);
        for _ in 0..4 {
            callback.render(&mut out);
        }
        assert_eq!(shared.consumed.load(Ordering::SeqCst), 20);
        assert_eq!(shared.drained.load(Ordering::SeqCst), 7);
    }

    #[test]
    fn a_new_segment_says_how_many_frames_the_one_before_it_ended_on() {
        let (mut callback, mut control, shared) = callback(1);
        let (mut producer, ring) = rtrb::RingBuffer::new(64);
        for _ in 0..30 {
            producer.push(0.25).unwrap();
        }
        let _ = control.push(Message::Install { segment: 1, ring });
        let _ = control.push(Message::Play);
        let mut out = [0.0; 10];
        callback.render(&mut out);
        callback.render(&mut out);
        let (_, next) = rtrb::RingBuffer::new(4);
        let _ = control.push(Message::Install {
            segment: 2,
            ring: next,
        });
        callback.render(&mut out);
        assert_eq!(shared.previous_final.load(Ordering::SeqCst), 20);
        assert_eq!(shared.installed.load(Ordering::SeqCst), 2);
        assert_eq!(shared.consumed.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_pause_fades_out_and_then_holds_the_rest_of_the_ring() {
        let (mut callback, mut control, shared) = callback(1);
        let (mut producer, ring) = rtrb::RingBuffer::new(256);
        for _ in 0..200 {
            producer.push(1.0).unwrap();
        }
        let _ = control.push(Message::Install { segment: 1, ring });
        let _ = control.push(Message::Play);
        let mut out = [0.0; 40];
        callback.render(&mut out);
        assert_eq!(out[39], 1.0);
        let _ = control.push(Message::Pause);
        callback.render(&mut out);
        // Twenty frames of fade at a rate of 1000, then nothing.
        let faded = shared.consumed.load(Ordering::SeqCst);
        assert_eq!(faded, 40 + 20);
        assert_eq!(out[39], 0.0);
        callback.render(&mut out);
        assert_eq!(shared.consumed.load(Ordering::SeqCst), faded);
    }
}
