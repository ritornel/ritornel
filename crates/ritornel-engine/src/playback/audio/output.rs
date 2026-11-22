//! The output side of the pipeline: what runs in the output's real-time
//! callback. It never locks, allocates, waits or does I/O. It takes messages
//! and rings from a lock-free queue, pops frames from the ring of the span
//! it was given, ramps the gain in and out over about 20 ms, applies the
//! player's volume, and says what it did on another lock-free queue: when
//! each span's first frame was handed to the output, and when and how each
//! span ended, with its count. See "Audio pipeline" in
//! docs/internals/playback.md.
//!
//! A ring can carry several tracks joined with no gap. A marker says at
//! which frame of the ring the next one begins, and there one span ends
//! into the next, to the frame.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use rtrb::{Consumer, Producer};

use crate::playback::renderer::{End, Event};

/// How long a fade in or out lasts.
const RAMP_MS: u32 = 20;

/// How many events the queue up holds: far more than can come between two
/// reads of it, which are a tick apart at most.
pub(crate) const EVENTS: usize = 1024;

/// Where a joined track begins in a ring, and the span it plays as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Marker {
    /// The frame of the ring, counted from its first.
    pub(crate) at: u64,
    pub(crate) span: u64,
}

/// What a span plays from: its frames, where joined tracks begin, and the
/// gate that says whether the next may be run into.
#[derive(Debug)]
pub(crate) struct Feed {
    pub(crate) ring: Consumer<f32>,
    pub(crate) markers: Consumer<Marker>,
    pub(crate) gate: Arc<Gate>,
}

/// Whether the output may run on into the track queued in a ring. It holds
/// that track's span while it may; the callback swaps it for [`CROSSED`] as
/// it crosses the marker, and the control side for 0 as it lets the track
/// go. Whichever swap comes first wins, so a track is either heard from its
/// first frame or never.
#[derive(Debug, Default)]
pub(crate) struct Gate(AtomicU64);

/// The gate's value once the callback has run into the track it held.
pub(crate) const CROSSED: u64 = u64::MAX;

impl Gate {
    /// The track queued may be run into.
    pub(crate) fn open(&self, span: u64) {
        self.0.store(span, Ordering::SeqCst);
    }

    /// Lets the track go unless the output has run into it. Whether it did.
    pub(crate) fn close(&self, span: u64) -> bool {
        self.swap(span, 0)
    }

    fn cross(&self, span: u64) -> bool {
        self.swap(span, CROSSED)
    }

    fn swap(&self, span: u64, to: u64) -> bool {
        self.0
            .compare_exchange(span, to, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }
}

/// What the control side tells the callback.
pub(crate) enum Message {
    /// Play this feed as span `span` from now on, in place of whatever
    /// plays. A span that starts a track from its beginning starts at full
    /// gain when played; any other fades in.
    Install {
        span: u64,
        feed: Feed,
        fade_in: bool,
    },
    /// Play nothing.
    Clear,
    /// Hand frames to the output, fading in.
    Play,
    /// Fade out, then hand nothing more until `Play`.
    Pause,
    /// The player's volume, 0 to 1, ramped to.
    Volume(f32),
}

/// A time not yet stamped.
const NOT_YET: u64 = u64::MAX;

/// Now, in nanoseconds on the clock every output stamps its events with:
/// one for the process, so a stamp means the same whichever output made it.
pub(crate) fn now() -> u64 {
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    let origin = ORIGIN.get_or_init(Instant::now);
    u64::try_from(origin.elapsed().as_nanos()).unwrap_or(NOT_YET - 1)
}

/// What the callback shows the control side besides its events.
#[derive(Debug, Default)]
pub(crate) struct Shared {
    /// Odd while the callback writes the two after it.
    written: AtomicU64,
    /// The span the callback is playing; 0 for none.
    installed: AtomicU64,
    /// Frames of that span handed to the output.
    consumed: AtomicU64,
    /// Callbacks run, so that the control side can tell the output is
    /// alive.
    pub(crate) callbacks: AtomicU64,
    /// Errors the output's stream reported.
    pub(crate) stream_errors: AtomicU64,
    /// Whether the stream reported an error it cannot go on from: its
    /// device went away or its configuration is no longer valid.
    pub(crate) lost: AtomicBool,
}

impl Shared {
    fn publish(&self, span: u64, frames: u64) {
        self.written.fetch_add(1, Ordering::SeqCst);
        self.installed.store(span, Ordering::SeqCst);
        self.consumed.store(frames, Ordering::SeqCst);
        self.written.fetch_add(1, Ordering::SeqCst);
    }

    /// The span the callback plays and its frames so far, read together.
    pub(crate) fn progress(&self) -> (u64, u64) {
        loop {
            let before = self.written.load(Ordering::SeqCst);
            let read = (
                self.installed.load(Ordering::SeqCst),
                self.consumed.load(Ordering::SeqCst),
            );
            if before.is_multiple_of(2) && self.written.load(Ordering::SeqCst) == before {
                return read;
            }
            // The callback is between its two writes, a moment from done.
            std::thread::yield_now();
        }
    }
}

/// The state that lives in the callback.
pub(crate) struct Callback {
    inbox: Consumer<Message>,
    retired: Producer<Feed>,
    events: Producer<Event>,
    shared: Arc<Shared>,
    /// What times are read from: [`now`], but for a test.
    clock: fn() -> u64,
    channels: usize,
    rate: u64,
    feed: Option<Feed>,
    /// Frames read from the feed's ring so far.
    read: u64,
    /// The span in hand, 0 for none, and its frames handed over.
    span: u64,
    consumed: u64,
    /// Whether its `Began` and its `Ended` have been said.
    began: bool,
    ended: bool,
    /// When its last frame handed over was done.
    last: u64,
    /// Where the span before ended into this one, while nothing but its
    /// frames has gone out since: its first frame starts there.
    seamless: Option<u64>,
    /// Whether the ring stops at its next marker, the track there let
    /// go: nothing past it is played.
    cut: bool,
    gain: f32,
    target: f32,
    step: f32,
    /// Whether the span starts a track, so its first frame needs no fade.
    hard_start: bool,
    volume: f32,
    volume_target: f32,
    /// What a test's decoder does just after the callback looks at the
    /// markers for a frame.
    #[cfg(test)]
    meanwhile: Option<Box<dyn FnOnce() + Send>>,
}

impl std::fmt::Debug for Callback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Callback")
            .field("span", &self.span)
            .field("consumed", &self.consumed)
            .finish_non_exhaustive()
    }
}

impl Callback {
    pub(crate) fn new(
        inbox: Consumer<Message>,
        retired: Producer<Feed>,
        events: Producer<Event>,
        shared: Arc<Shared>,
        rate: u32,
        channels: usize,
    ) -> Self {
        // The clock starts here, on the control side, so that the callback
        // never waits for it to.
        now();
        let ramp_frames = (rate.max(1) * RAMP_MS / 1000).max(1);
        Self {
            inbox,
            retired,
            events,
            shared,
            clock: now,
            channels: channels.max(1),
            rate: u64::from(rate.max(1)),
            feed: None,
            read: 0,
            span: 0,
            consumed: 0,
            began: false,
            ended: false,
            last: NOT_YET,
            seamless: None,
            cut: false,
            gain: 0.0,
            target: 0.0,
            step: 1.0 / ramp_frames as f32,
            hard_start: false,
            volume: 1.0,
            volume_target: 1.0,
            #[cfg(test)]
            meanwhile: None,
        }
    }

    /// The samples waiting in the ring it plays, for a test's hold on the
    /// null output.
    pub(crate) fn waiting(&self) -> usize {
        self.feed.as_ref().map_or(0, |feed| feed.ring.slots())
    }

    fn say(&mut self, event: Event) {
        // The queue holds far more than can be in flight.
        let _ = self.events.push(event);
    }

    /// Ends the span in hand, at `at`, unless it has ended already.
    fn end(&mut self, at: u64, end: End) {
        if self.span != 0 && !self.ended {
            self.ended = true;
            let (span, frames) = (self.span, self.consumed);
            self.say(Event::Ended {
                span,
                frames,
                at,
                end,
            });
        }
    }

    /// Takes up `span`, nothing of it handed over yet.
    fn start(&mut self, span: u64) {
        self.span = span;
        self.consumed = 0;
        (self.began, self.ended, self.last) = (false, false, NOT_YET);
    }

    /// Hands the feed back to the control side, which frees it, and ends
    /// the spans joined in it that were never reached, but one let go.
    fn let_go(&mut self, at: u64) {
        self.cut = false;
        let Some(mut old) = self.feed.take() else {
            return;
        };
        while let Ok(marker) = old.markers.pop() {
            if !old.gate.cross(marker.span) {
                continue;
            }
            self.say(Event::Ended {
                span: marker.span,
                frames: 0,
                at,
                end: End::Replaced,
            });
        }
        // The queue back holds far more feeds than can be in flight. Were
        // it full, the feed would be freed here, which is slow, not wrong.
        let _ = self.retired.push(old);
    }

    /// Ends the span in hand, and any joined in its ring, as something is
    /// put in its place now, though never before its last frame was done.
    fn replace(&mut self, now: u64) {
        let at = if self.last == NOT_YET {
            now
        } else {
            self.last.max(now)
        };
        self.end(at, End::Replaced);
        self.let_go(at);
        self.seamless = None;
    }

    fn take_messages(&mut self, now: u64) {
        while let Ok(message) = self.inbox.pop() {
            match message {
                Message::Install {
                    span,
                    feed,
                    fade_in,
                } => {
                    self.replace(now);
                    self.feed = Some(feed);
                    self.read = 0;
                    self.gain = 0.0;
                    self.hard_start = !fade_in;
                    self.start(span);
                }
                Message::Clear => {
                    self.replace(now);
                    self.start(0);
                }
                Message::Play => {
                    self.target = 1.0;
                }
                Message::Pause => {
                    self.target = 0.0;
                }
                Message::Volume(volume) => {
                    self.volume_target = volume.clamp(0.0, 1.0);
                    // Nothing is heard yet, so there is nothing to ramp.
                    if self.gain <= 0.0 {
                        self.volume = self.volume_target;
                    }
                }
            }
        }
    }

    /// Ends the span in hand into the next, if the frame about to be read
    /// is the first of a track joined in this ring, and on through any
    /// joined there with nothing in them. Each ended with its last frame,
    /// or at `at` if it had none. A track let go before the output got
    /// there cuts the ring: the one in hand ends there by itself.
    fn cross(&mut self, at: u64) -> bool {
        let mut crossed = false;
        while let Some(feed) = &mut self.feed {
            let next = match feed.markers.peek() {
                Ok(marker) if marker.at <= self.read && !self.cut => marker.span,
                _ => break,
            };
            if !feed.gate.cross(next) {
                self.cut = true;
                break;
            }
            let _ = feed.markers.pop();
            let ended = if self.last == NOT_YET { at } else { self.last };
            self.end(ended, End::Joined { next });
            self.start(next);
            self.seamless = Some(ended);
            crossed = true;
        }
        crossed
    }

    /// How far the gain falls at the next frame as it fades out: by the
    /// usual step, or faster where a track joined in the ring begins
    /// sooner than the fade would end, so that it is out by then. A fade
    /// the join comes too soon for, as when its marker comes late, stops
    /// there.
    fn fade_step(&self) -> f32 {
        let left = (self.feed.as_ref())
            .and_then(|feed| feed.markers.peek().ok())
            .map(|marker| marker.at.saturating_sub(self.read));
        match left {
            Some(left) => self
                .step
                .max((self.gain - self.target) / left.saturating_add(1) as f32),
            None => self.step,
        }
    }

    /// When the frame `frame` of this buffer is handed over, in
    /// nanoseconds on the output's clock.
    fn time_of(&self, now: u64, frame: usize) -> u64 {
        now.saturating_add(frame as u64 * 1_000_000_000 / self.rate)
    }

    /// Fills `out`, interleaved in the output's channels, and returns how
    /// many frames came from the ring: they are the first of `out`, and the
    /// rest is silence.
    pub(crate) fn render(&mut self, out: &mut [f32]) -> usize {
        let now = (self.clock)();
        self.take_messages(now);
        self.shared.callbacks.fetch_add(1, Ordering::Relaxed);
        let channels = self.channels;
        let mut written = 0;
        for frame in out.chunks_exact_mut(channels) {
            let at = self.time_of(now, written);
            // The frame before its marker: a decoder writes a marker before
            // the frames past it, so a frame seen here has its marker seen.
            let ready = (self.feed.as_ref()).is_some_and(|feed| feed.ring.slots() >= channels);
            self.cross(at);
            #[cfg(test)]
            if let Some(meanwhile) = self.meanwhile.take() {
                meanwhile();
            }
            // Paused, a span is never begun: what is left of a fade out
            // goes no further than the span heard.
            if self.target <= 0.0 && !self.began {
                self.gain = 0.0;
            }
            let silent = self.gain <= 0.0 && self.target <= 0.0;
            let feed = match &mut self.feed {
                Some(feed) if ready && !silent && !self.cut => feed,
                _ => break,
            };
            let Ok(chunk) = feed.ring.read_chunk(channels) else {
                break;
            };
            // A track played from its beginning starts at full gain, from
            // its first frame.
            if self.hard_start {
                self.hard_start = false;
                self.gain = self.target;
            }
            let scale = self.gain * self.volume;
            let (first, second) = chunk.as_slices();
            for (at, sample) in first.iter().chain(second).enumerate() {
                // Nothing goes over full scale. A sample within it, as
                // every decoded one is, passes unchanged.
                if let Some(slot) = frame.get_mut(at) {
                    *slot = (sample * scale).clamp(-1.0, 1.0);
                }
            }
            chunk.commit_all();
            if !self.began {
                self.began = true;
                let (span, at) = (self.span, self.seamless.unwrap_or(at));
                self.say(Event::Began { span, at });
            }
            written += 1;
            self.last = self.time_of(now, written);
            self.read += 1;
            self.consumed += 1;
            if self.gain < self.target {
                self.gain = (self.gain + self.step).min(self.target);
            } else if self.gain > self.target {
                self.gain = (self.gain - self.fade_step()).max(self.target);
            }
            if self.volume < self.volume_target {
                self.volume = (self.volume + self.step).min(self.volume_target);
            } else if self.volume > self.volume_target {
                self.volume = (self.volume - self.step).max(self.volume_target);
            }
        }
        let at = self.time_of(now, written);
        // A track that ends where this buffer does ends into the next now.
        self.cross(at);
        if let Some(rest) = out.get_mut(written * channels..)
            && !rest.is_empty()
        {
            rest.fill(0.0);
            self.seamless = None;
        }
        self.settle(at);
        self.shared.publish(self.span, self.consumed);
        written
    }

    /// At the end of a ring its decoder has let go of, a track joined there
    /// with nothing in the ring is reached all the same, and the last one's
    /// audio has run out; so has the track in hand where the ring is cut.
    fn settle(&mut self, at: u64) {
        let Some(feed) = &self.feed else {
            return;
        };
        // Abandoned first: a ring its decoder has let go of gets no more.
        let ran_out = feed.ring.is_abandoned() && feed.ring.slots() < self.channels;
        if !ran_out && !self.cut {
            return;
        }
        if ran_out {
            // Nothing more comes: a track joined where it ran out is reached.
            self.read = u64::MAX;
            self.cross(at);
        }
        let ended = if self.last == NOT_YET { at } else { self.last };
        self.end(ended, End::Drained);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Draws;

    /// A callback at a rate of 1000, on a clock that moves as a device
    /// plays its buffers.
    struct Rig {
        callback: Callback,
        control: Producer<Message>,
        events: Consumer<Event>,
        shared: Arc<Shared>,
        channels: usize,
    }

    thread_local! {
        static CLOCK: std::cell::Cell<u64> = const { std::cell::Cell::new(1) };
    }

    fn clock() -> u64 {
        CLOCK.with(std::cell::Cell::get)
    }

    impl Rig {
        fn new(channels: usize) -> Self {
            let (control, inbox) = rtrb::RingBuffer::new(64);
            let (retired, _) = rtrb::RingBuffer::new(64);
            let (said, events) = rtrb::RingBuffer::new(EVENTS);
            let shared = Arc::new(Shared::default());
            let mut callback =
                Callback::new(inbox, retired, said, Arc::clone(&shared), 1000, channels);
            callback.clock = clock;
            CLOCK.with(|now| now.set(1));
            Self {
                callback,
                control,
                events,
                shared,
                channels,
            }
        }

        /// The device plays a buffer: its time passes.
        fn render(&mut self, out: &mut [f32]) -> usize {
            let written = self.callback.render(out);
            self.wait(out.len() / self.channels);
            written
        }

        fn wait(&mut self, ms: usize) {
            CLOCK.with(|now| now.set(now.get() + ms as u64 * 1_000_000));
        }

        fn send(&mut self, message: Message) {
            assert!(self.control.push(message).is_ok());
        }

        fn install(&mut self, span: u64, feed: Feed, fade_in: bool) {
            self.send(Message::Install {
                span,
                feed,
                fade_in,
            });
        }

        fn said(&mut self) -> Vec<Event> {
            std::iter::from_fn(|| self.events.pop().ok()).collect()
        }
    }

    /// A feed of `samples`, with these markers, the first of them offered,
    /// and its producer.
    fn feed(samples: &[f32], markers: &[Marker]) -> (Feed, Producer<f32>) {
        let (mut producer, ring) = rtrb::RingBuffer::new(samples.len().max(1) + 64);
        for sample in samples {
            producer.push(*sample).unwrap();
        }
        let (mut marks, markers_ring) = rtrb::RingBuffer::new(8);
        for marker in markers {
            marks.push(*marker).unwrap();
        }
        let gate = Arc::new(Gate::default());
        if let Some(first) = markers.first() {
            gate.open(first.span);
        }
        let feed = Feed {
            ring,
            markers: markers_ring,
            gate,
        };
        (feed, producer)
    }

    #[test]
    fn a_track_let_go_before_the_output_reaches_it_is_never_heard_and_the_one_before_ends_whole() {
        let mut rig = Rig::new(1);
        // Seven frames of one track, then five of the next, both decoded.
        let samples = [0.1, 0.1, 0.1, 0.1, 0.1, 0.1, 0.1, 0.9, 0.9, 0.9, 0.9, 0.9];
        let (ring, _producer) = feed(&samples, &[Marker { at: 7, span: 4 }]);
        let gate = Arc::clone(&ring.gate);
        rig.install(3, ring, false);
        rig.send(Message::Play);
        let mut out = [0.0; 5];
        assert_eq!(rig.render(&mut out), 5);
        // Let go with the output two frames short of it.
        assert!(gate.close(4));
        assert_eq!(rig.render(&mut out), 2);
        assert_eq!(out, [0.1, 0.1, 0.0, 0.0, 0.0]);
        assert_eq!(rig.render(&mut out), 0);
        let said = rig.said();
        assert_eq!(ended(&said), [(3, 7, End::Drained)]);
        assert!(
            !said
                .iter()
                .any(|event| matches!(event, Event::Began { span: 4, .. }))
        );
        // Once the output has run into a track, it can no longer be let go.
        let (ring, _producer) = feed(&samples, &[Marker { at: 7, span: 6 }]);
        let gate = Arc::clone(&ring.gate);
        rig.install(5, ring, false);
        rig.send(Message::Play);
        assert_eq!(rig.render(&mut [0.0; 8]), 8);
        assert!(!gate.close(6));
    }

    /// Seven frames of span 3 in a ring, with `next` queued after them and
    /// nothing of it written yet: its decoder is about to write the marker,
    /// then its frames.
    fn about_to_join(next: u64) -> (Feed, Producer<f32>, Producer<Marker>, Arc<Gate>) {
        let (mut producer, ring) = rtrb::RingBuffer::new(64);
        for _ in 0..7 {
            producer.push(0.1).unwrap();
        }
        let (marks, markers) = rtrb::RingBuffer::new(4);
        let gate = Arc::new(Gate::default());
        gate.open(next);
        let feed = Feed {
            ring,
            markers,
            gate: Arc::clone(&gate),
        };
        (feed, producer, marks, gate)
    }

    #[test]
    fn a_track_written_just_after_the_callback_looks_for_its_marker_is_never_heard_once_let_go() {
        let mut rig = Rig::new(1);
        let (ring, mut producer, mut marks, gate) = about_to_join(4);
        rig.install(3, ring, false);
        rig.send(Message::Play);
        assert_eq!(rig.render(&mut [0.0; 7]), 7);
        assert!(gate.close(4));
        rig.callback.meanwhile = Some(Box::new(move || {
            marks.push(Marker { at: 7, span: 4 }).unwrap();
            producer.push(0.9).unwrap();
        }));
        let mut out = [0.0; 2];
        assert_eq!(rig.render(&mut out), 0);
        assert_eq!(rig.render(&mut out), 0);
        assert_eq!(out, [0.0; 2]);
        let said = rig.said();
        assert_eq!(ended(&said), [(3, 7, End::Drained)]);
        assert!(
            !said
                .iter()
                .any(|event| matches!(event, Event::Began { span: 4, .. }))
        );

        // Not let go, that frame is the next track's, and joins to the frame.
        let (ring, mut producer, mut marks, _gate) = about_to_join(6);
        rig.install(5, ring, false);
        rig.send(Message::Play);
        assert_eq!(rig.render(&mut [0.0; 7]), 7);
        rig.callback.meanwhile = Some(Box::new(move || {
            marks.push(Marker { at: 7, span: 6 }).unwrap();
            producer.push(0.9).unwrap();
        }));
        rig.render(&mut out);
        assert_eq!(rig.render(&mut out), 1);
        assert_eq!(out, [0.9, 0.0]);
        // Its decoder let the ring go after that frame.
        assert_eq!(
            ended(&rig.said()),
            [(5, 7, End::Joined { next: 6 }), (6, 1, End::Drained)]
        );
    }

    #[test]
    fn a_track_let_go_is_never_heard_however_its_decoder_races_the_callback() {
        const RACES: usize = 50_000;
        type Job = (Producer<f32>, Producer<Marker>, Arc<AtomicBool>);
        let (jobs, work) = std::sync::mpsc::channel::<Job>();
        let decoder = std::thread::spawn(move || {
            while let Ok((mut producer, mut marks, done)) = work.recv() {
                marks.push(Marker { at: 7, span: 4 }).unwrap();
                producer.push(0.9).unwrap();
                done.store(true, Ordering::Release);
            }
        });
        let mut rig = Rig::new(1);
        let mut wrong = 0;
        for _ in 0..RACES {
            let (ring, producer, marks, gate) = about_to_join(4);
            rig.install(3, ring, false);
            rig.send(Message::Play);
            rig.render(&mut [0.0; 7]);
            assert!(gate.close(4));
            let done = Arc::new(AtomicBool::new(false));
            jobs.send((producer, marks, Arc::clone(&done))).unwrap();
            let mut heard = false;
            loop {
                let written = done.load(Ordering::Acquire);
                let mut out = [0.0];
                rig.render(&mut out);
                heard |= out[0] != 0.0;
                if written {
                    break;
                }
            }
            if heard || ended(&rig.said()) != [(3, 7, End::Drained)] {
                wrong += 1;
            }
        }
        drop(jobs);
        decoder.join().unwrap();
        assert_eq!(wrong, 0, "{wrong} of {RACES} races");
    }

    fn ended(events: &[Event]) -> Vec<(u64, u64, End)> {
        events
            .iter()
            .filter_map(|event| match event {
                Event::Ended {
                    span, frames, end, ..
                } => Some((*span, *frames, end.clone())),
                Event::Began { .. } => None,
            })
            .collect()
    }

    #[test]
    fn frames_are_counted_as_they_are_handed_to_the_output_and_not_while_paused() {
        let mut rig = Rig::new(2);
        let (ring, producer) = feed(&[0.5; 40], &[]);
        rig.install(7, ring, true);
        let mut out = [1.0; 8];
        // Installed and not yet playing: silence, and nothing counted.
        assert_eq!(rig.render(&mut out), 0);
        assert_eq!(out, [0.0; 8]);
        assert_eq!(rig.shared.progress(), (7, 0));
        assert!(rig.said().is_empty());

        rig.send(Message::Play);
        assert_eq!(rig.render(&mut out), 4);
        assert_eq!(rig.shared.progress(), (7, 4));
        assert!(matches!(rig.said()[..], [Event::Began { span: 7, .. }]));
        // The fade in starts from silence.
        assert_eq!(out[0], 0.0);
        assert!(out[6] > 0.0);

        // The producer finishes: the ring drains and the span says so, once.
        drop(producer);
        for _ in 0..6 {
            rig.render(&mut out);
        }
        assert_eq!(ended(&rig.said()), [(7, 20, End::Drained)]);
    }

    #[test]
    fn a_track_played_from_its_beginning_starts_at_full_gain_and_samples_go_through_unchanged() {
        let mut rig = Rig::new(1);
        let samples: Vec<f32> = (0..30).map(|at| at as f32 / 64.0 - 0.2).collect();
        let (ring, _producer) = feed(&samples, &[]);
        rig.install(1, ring, false);
        rig.send(Message::Play);
        let mut out = [0.0; 30];
        assert_eq!(rig.render(&mut out), 30);
        assert_eq!(out.to_vec(), samples);
    }

    #[test]
    fn a_span_replaced_ends_with_its_own_count_and_one_joined_in_its_ring_never_reached_too() {
        let mut rig = Rig::new(1);
        let (ring, _producer) = feed(&[0.25; 30], &[Marker { at: 25, span: 2 }]);
        rig.install(1, ring, true);
        rig.send(Message::Play);
        let mut out = [0.0; 10];
        rig.render(&mut out);
        rig.render(&mut out);
        let (next, _next_producer) = feed(&[], &[]);
        rig.install(3, next, true);
        rig.render(&mut out);
        assert_eq!(
            ended(&rig.said()),
            [(1, 20, End::Replaced), (2, 0, End::Replaced)]
        );
        assert_eq!(rig.shared.progress(), (3, 0));
    }

    #[test]
    fn a_joined_track_starts_at_its_marker_to_the_frame_with_no_fade_and_no_gap() {
        let mut rig = Rig::new(1);
        // Seven frames of one track, then five of the next, in one ring.
        let samples = [0.1, 0.1, 0.1, 0.1, 0.1, 0.1, 0.1, 0.9, 0.9, 0.9, 0.9, 0.9];
        let (ring, producer) = feed(&samples, &[Marker { at: 7, span: 4 }]);
        rig.install(3, ring, false);
        rig.send(Message::Play);
        drop(producer);
        let mut out = [0.0; 5];
        assert_eq!(rig.render(&mut out), 5);
        assert_eq!(rig.render(&mut out), 5);
        // The join fell in this buffer: the first track ended on 7 frames,
        // and the next began where it ended.
        assert_eq!(out, [0.1, 0.1, 0.9, 0.9, 0.9]);
        let said = rig.said();
        assert_eq!(ended(&said), [(3, 7, End::Joined { next: 4 })]);
        let (joined, began) = match said[..] {
            [
                _,
                Event::Ended { at: joined, .. },
                Event::Began { span: 4, at: began },
            ] => (joined, began),
            _ => panic!("{said:?}"),
        };
        assert_eq!(joined, began);
        assert_eq!(rig.shared.progress(), (4, 3));
        assert_eq!(rig.render(&mut out), 2);
        assert_eq!(ended(&rig.said()), [(4, 5, End::Drained)]);
    }

    #[test]
    fn a_track_that_ends_with_the_buffer_begins_the_next_where_it_ended() {
        let mut rig = Rig::new(1);
        let (ring, _producer) = feed(&[0.5; 8], &[Marker { at: 4, span: 9 }]);
        rig.install(8, ring, false);
        rig.send(Message::Play);
        rig.render(&mut [0.0; 4]);
        rig.wait(5);
        rig.render(&mut [0.0; 4]);
        let said = rig.said();
        let [
            Event::Began { .. },
            Event::Ended { at: ended, .. },
            Event::Began { span: 9, at: began },
        ] = said[..]
        else {
            panic!("{said:?}");
        };
        // Nothing but its frames went out between them: no gap, whatever
        // the callback's own timing.
        assert_eq!(ended, began);
    }

    #[test]
    fn a_join_reached_as_a_pause_fades_out_ends_there_and_the_next_begins_when_play_resumes() {
        let mut rig = Rig::new(1);
        let (ring, _producer) = feed(&[0.1; 100], &[Marker { at: 40, span: 2 }]);
        rig.install(1, ring, false);
        rig.send(Message::Play);
        assert_eq!(rig.render(&mut [0.0; 20]), 20);
        // The fade out takes the last twenty frames before the join.
        rig.send(Message::Pause);
        assert_eq!(rig.render(&mut [0.0; 20]), 20);
        assert_eq!(rig.render(&mut [0.0; 20]), 0);
        let said = rig.said();
        let Event::Ended { at: joined, .. } = said[1] else {
            panic!("{said:?}");
        };
        assert_eq!(ended(&said), [(1, 40, End::Joined { next: 2 })]);
        rig.wait(5);
        rig.send(Message::Play);
        assert_eq!(rig.render(&mut [0.0; 20]), 20);
        let [Event::Began { span: 2, at: began }] = rig.said()[..] else {
            panic!("the next began once");
        };
        assert!(began >= joined + 5_000_000, "{joined} {began}");
    }

    #[test]
    fn a_pause_just_before_a_join_fades_out_by_it_and_the_next_waits_for_play() {
        let mut rig = Rig::new(1);
        let (ring, _producer) = feed(&[0.5; 100], &[Marker { at: 40, span: 2 }]);
        rig.install(1, ring, false);
        rig.send(Message::Play);
        assert_eq!(rig.render(&mut [0.0; 30]), 30);
        // Ten frames before the join: too few for the whole fade.
        rig.send(Message::Pause);
        let mut out = [1.0; 20];
        assert_eq!(rig.render(&mut out), 10);
        // It fades all the way out over those ten, and nothing of the next
        // track is heard.
        assert!(out[0] > 0.45, "{out:?}");
        assert!(
            out[..10].windows(2).all(|pair| pair[1] < pair[0]),
            "{out:?}"
        );
        assert!(out[9] <= 0.05, "{out:?}");
        assert_eq!(out[10..], [0.0; 10]);
        let said = rig.said();
        assert_eq!(ended(&said), [(1, 40, End::Joined { next: 2 })]);
        assert!(
            !said
                .iter()
                .any(|event| matches!(event, Event::Began { span: 2, .. })),
            "{said:?}"
        );
        assert_eq!(rig.render(&mut [0.0; 20]), 0);
        rig.send(Message::Play);
        assert_eq!(rig.render(&mut [0.0; 20]), 20);
        assert!(matches!(rig.said()[..], [Event::Began { span: 2, .. }]));
    }

    #[test]
    fn a_track_joined_after_a_pause_began_is_never_begun_while_paused() {
        let mut rig = Rig::new(1);
        // Its marker comes late, once the fade has half gone, as from a
        // decoder that fell behind.
        let (ring, mut producer, mut marks, _gate) = about_to_join(2);
        for _ in 0..33 {
            producer.push(0.1).unwrap();
        }
        rig.install(1, ring, false);
        rig.send(Message::Play);
        assert_eq!(rig.render(&mut [0.0; 30]), 30);
        rig.send(Message::Pause);
        assert_eq!(rig.render(&mut [0.0; 10]), 10);
        marks.push(Marker { at: 40, span: 2 }).unwrap();
        for _ in 0..20 {
            producer.push(0.9).unwrap();
        }
        let mut out = [1.0; 20];
        assert_eq!(rig.render(&mut out), 0);
        assert_eq!(out, [0.0; 20]);
        // Or the join is the first frame of the buffer after the pause.
        let (ring, _producer) = feed(&[0.5; 100], &[Marker { at: 40, span: 4 }]);
        rig.install(3, ring, false);
        rig.send(Message::Play);
        assert_eq!(rig.render(&mut [0.0; 40]), 40);
        rig.send(Message::Pause);
        assert_eq!(rig.render(&mut out), 0);
        let said = rig.said();
        assert!(
            !said
                .iter()
                .any(|event| matches!(event, Event::Began { span: 2 | 4, .. })),
            "{said:?}"
        );
        assert_eq!(
            ended(&said),
            [
                (1, 40, End::Joined { next: 2 }),
                (2, 0, End::Replaced),
                (3, 40, End::Joined { next: 4 })
            ]
        );
        // Played again, the next begins from its first frame.
        rig.send(Message::Play);
        assert_eq!(rig.render(&mut [0.0; 20]), 20);
        assert!(matches!(rig.said()[..], [Event::Began { span: 4, .. }]));
        assert_eq!(rig.shared.progress(), (4, 20));
    }

    #[test]
    fn a_pause_before_a_track_from_its_beginning_is_heard_hands_nothing_over() {
        let mut rig = Rig::new(1);
        // Its decoder has nothing in the ring yet when play and pause come.
        let (ring, mut producer) = feed(&[], &[]);
        rig.install(1, ring, false);
        rig.send(Message::Play);
        assert_eq!(rig.render(&mut [0.0; 5]), 0);
        rig.send(Message::Pause);
        for _ in 0..50 {
            producer.push(0.5).unwrap();
        }
        let mut out = [1.0; 20];
        assert_eq!(rig.render(&mut out), 0);
        assert_eq!(out, [0.0; 20]);
        // Or both come before the output's next buffer.
        let (ring, _producer) = feed(&[0.5; 50], &[]);
        rig.install(2, ring, false);
        rig.send(Message::Play);
        rig.send(Message::Pause);
        assert_eq!(rig.render(&mut out), 0);
        assert!(
            rig.said()
                .iter()
                .all(|event| !matches!(event, Event::Began { .. }))
        );
        // Played, each starts at full gain from its first frame.
        rig.send(Message::Play);
        assert_eq!(rig.render(&mut out), 20);
        assert_eq!(out, [0.5; 20]);
    }

    #[test]
    fn a_volume_set_after_play_and_before_the_first_frame_is_where_that_frame_starts() {
        // Set with play, or once the output has found nothing yet to play.
        for later in [false, true] {
            let mut rig = Rig::new(1);
            let (ring, mut producer) = feed(&[], &[]);
            rig.install(1, ring, false);
            rig.send(Message::Play);
            if !later {
                rig.send(Message::Volume(0.5));
            }
            assert_eq!(rig.render(&mut [0.0; 5]), 0);
            if later {
                rig.send(Message::Volume(0.5));
            }
            for _ in 0..20 {
                producer.push(1.0).unwrap();
            }
            let mut out = [0.0; 20];
            assert_eq!(rig.render(&mut out), 20);
            assert_eq!(out, [0.5; 20], "later: {later}");
        }
    }

    #[test]
    fn the_volume_ramps_to_where_it_is_set_and_scales_every_sample() {
        let mut rig = Rig::new(1);
        let (ring, _producer) = feed(&[1.0; 200], &[]);
        rig.install(1, ring, false);
        rig.send(Message::Play);
        assert_eq!(rig.render(&mut [0.0; 5]), 5);
        rig.send(Message::Volume(0.5));
        let mut out = [0.0; 40];
        rig.render(&mut out);
        // Twenty frames of ramp at a rate of 1000, no step bigger than one
        // twentieth, and then the new level.
        assert!(out[0] > 0.9);
        assert!(
            out.windows(2)
                .all(|pair| (pair[0] - pair[1]).abs() <= 0.051)
        );
        assert_eq!(out[39], 0.5);
    }

    #[test]
    fn a_pause_fades_out_and_then_holds_the_rest_of_the_ring() {
        let mut rig = Rig::new(1);
        let (ring, _producer) = feed(&[1.0; 200], &[]);
        rig.install(1, ring, true);
        rig.send(Message::Play);
        let mut out = [0.0; 40];
        rig.render(&mut out);
        assert_eq!(out[39], 1.0);
        rig.send(Message::Pause);
        rig.render(&mut out);
        // Twenty frames of fade at a rate of 1000, then nothing.
        assert_eq!(rig.shared.progress(), (1, 40 + 20));
        assert_eq!(out[39], 0.0);
        rig.render(&mut out);
        assert_eq!(rig.shared.progress(), (1, 60));
    }

    /// One ring of a few joined tracks as a decoder writes it, its marks
    /// written up to `lead` frames before the frames they mark, as the
    /// resampler can. A track is taken, and marked, only once the renderer
    /// has offered it through the gate; a decoder slower than the renderer
    /// waits for it.
    struct Written {
        producer: Option<Producer<f32>>,
        marks: Producer<Marker>,
        gate: Arc<Gate>,
        /// Where each joined track begins, and its span.
        edges: Vec<(u64, u64)>,
        /// How many have been offered, and marked.
        opened: usize,
        marked: usize,
        /// The track let go, after which nothing more is offered.
        cancelled: Option<u64>,
        lead: u64,
        done: u64,
        total: u64,
    }

    impl Written {
        /// Writes up to `frames` more, and lets the ring go, when all are
        /// in, if `finish` says so. A span marked reaches the callback.
        fn write(&mut self, frames: u64, channels: usize, finish: bool, reached: &mut Vec<u64>) {
            let Some(producer) = &mut self.producer else {
                return;
            };
            let mut left = frames;
            loop {
                while let Some(&(at, span)) = self.edges.get(self.marked) {
                    if at > self.done + self.lead || self.marked >= self.opened {
                        break;
                    }
                    self.marks.push(Marker { at, span }).unwrap();
                    self.marked += 1;
                    reached.push(span);
                }
                let waiting = self.cancelled.is_none()
                    && self.marked >= self.opened
                    && self
                        .edges
                        .get(self.marked)
                        .is_some_and(|(at, _)| *at <= self.done);
                if left == 0 || waiting || self.done == self.total || producer.slots() < channels {
                    break;
                }
                for _ in 0..channels {
                    producer.push(0.5).unwrap();
                }
                self.done += 1;
                left -= 1;
            }
            if finish && self.done == self.total && self.marked == self.edges.len() {
                self.producer = None;
            }
        }

        /// What the renderer does between callbacks: offers the next track
        /// once the one before has been run into, or now and then lets the
        /// one offered go. The track let go, if it was.
        fn offer(&mut self, draws: &mut Draws) -> Option<u64> {
            if self.cancelled.is_some() {
                return None;
            }
            let crossed = self.opened == 0 || self.gate.0.load(Ordering::SeqCst) == CROSSED;
            if crossed && let Some(&(_, span)) = self.edges.get(self.opened) {
                self.gate.open(span);
                self.opened += 1;
            } else if !crossed
                && draws.below(6) == 0
                && let Some(&(_, span)) = self.edges.get(self.opened - 1)
                && self.gate.close(span)
            {
                self.cancelled = Some(span);
            }
            self.cancelled
        }
    }

    #[test]
    fn whatever_the_messages_markers_and_tracks_let_go_every_span_ends_once_in_order_and_the_counts_add_up()
     {
        for seed in 1..=400 {
            let mut draws = Draws::seeded(seed);
            let channels = usize::try_from(draws.below(2)).unwrap() + 1;
            let mut rig = Rig::new(channels);
            let (mut next_span, mut handed) = (1_u64, 0_u64);
            let mut ring: Option<Written> = None;
            let mut lengths = std::collections::HashMap::new();
            let (mut reached, mut cancelled) = (Vec::new(), Vec::new());
            for _ in 0..80 {
                match draws.below(10) {
                    0 | 1 => {
                        let mut edges = Vec::new();
                        let mut total = 0;
                        let first = next_span;
                        for joined in 0..draws.below(3) + 1 {
                            let length = draws.below(40);
                            if joined > 0 {
                                edges.push((total, next_span));
                            }
                            lengths.insert(next_span, length);
                            total += length;
                            next_span += 1;
                        }
                        let (producer, feed) = rtrb::RingBuffer::new(64 * channels);
                        let (marks, markers) = rtrb::RingBuffer::new(8);
                        let gate = Arc::new(Gate::default());
                        let feed = Feed {
                            ring: feed,
                            markers,
                            gate: Arc::clone(&gate),
                        };
                        rig.install(first, feed, draws.below(2) == 0);
                        reached.push(first);
                        ring = Some(Written {
                            producer: Some(producer),
                            marks,
                            gate,
                            edges,
                            opened: 0,
                            marked: 0,
                            cancelled: None,
                            lead: draws.below(20),
                            done: 0,
                            total,
                        });
                    }
                    2 => {
                        // The renderer stops a ring's decoder before it lets it go.
                        ring = None;
                        rig.send(Message::Clear);
                    }
                    3 => rig.send(Message::Play),
                    4 => rig.send(Message::Pause),
                    5 => rig.send(Message::Volume(0.5)),
                    _ => {}
                }
                if let Some(ring) = &mut ring {
                    if let Some(span) = ring.offer(&mut draws)
                        && !cancelled.contains(&span)
                    {
                        cancelled.push(span);
                    }
                    ring.write(draws.below(30), channels, draws.below(3) == 0, &mut reached);
                }
                let frames = draws.below(25);
                let mut out = vec![0.0; usize::try_from(frames).unwrap() * channels];
                handed += rig.render(&mut out) as u64;
            }
            // Everything is let go at the end.
            rig.send(Message::Clear);
            rig.render(&mut []);
            let mut seen: std::collections::HashMap<u64, (bool, bool)> = Default::default();
            let (mut counted, mut last) = (0, 0);
            for event in rig.said() {
                let (span, at) = match event {
                    Event::Began { span, at } | Event::Ended { span, at, .. } => (span, at),
                };
                assert!(at >= last, "seed {seed}: time went back at {event:?}");
                last = at;
                let (began, ended) = seen.entry(span).or_default();
                assert!(!*ended, "seed {seed}: {event:?} after its span ended");
                match event {
                    Event::Began { .. } => {
                        assert!(!*began, "seed {seed}: span {span} began twice");
                        *began = true;
                    }
                    Event::Ended { frames, end, .. } => {
                        *ended = true;
                        counted += frames;
                        let whole = lengths[&span];
                        assert!(frames <= whole, "seed {seed}: {span} of {whole}");
                        if matches!(end, End::Joined { .. } | End::Drained) {
                            assert_eq!(frames, whole, "seed {seed}: {span} {end:?}");
                        }
                        assert_eq!(frames > 0, *began, "seed {seed}: span {span}");
                    }
                }
            }
            // Every span that reached the callback ended, but none let go
            // was heard or said anything; and the counts are what the output
            // was handed, to the frame.
            let mut ended: Vec<u64> = seen.keys().copied().collect();
            reached.retain(|span| !cancelled.contains(span));
            ended.sort_unstable();
            reached.sort_unstable();
            assert_eq!(ended, reached, "seed {seed}");
            assert!(seen.values().all(|(_, ended)| *ended), "seed {seed}");
            assert_eq!(counted, handed, "seed {seed}");
        }
    }
}
