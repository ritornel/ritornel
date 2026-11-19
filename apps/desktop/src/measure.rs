//! A floor check, not a real hand: a fling shaped like a recorded one, and
//! what the wall did under it.
//!
//! The fling is a run of flicks. Each one's speed is drawn from a
//! log-normal with the recorded trace's median of about 4,000 points a
//! second and a tenth past about 19,000, capped at its peak of 47,000; the
//! fingers push for 60 ms and then the momentum decays exponentially, with
//! a time constant between 0.35 and 0.6 s, as the systems' momentum does,
//! until it is under 40 points a second. Between flicks the hand rests 120
//! to 500 ms. Mostly down the wall, turning at its ends. The events are
//! made once a frame and go through the same code a real scroll event does;
//! what they do not exercise is the system's own event delivery.

#![allow(
    clippy::print_stderr,
    reason = "a developer's tool: it reports on standard error"
)]

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::TouchPhase;

use crate::covers::{Cache, LoaderCounts};

/// A small, seeded random source, so a run can be repeated.
struct Random(u64);

impl Random {
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }

    fn normal(&mut self) -> f64 {
        let (a, b) = (self.next().max(1e-12), self.next());
        (-2. * a.ln()).sqrt() * (std::f64::consts::TAU * b).cos()
    }
}

#[derive(Debug, Clone, Copy)]
struct Flick {
    /// When the fingers come down, from the fling's start.
    at: f32,
    /// Points a second, signed: positive is down the wall.
    speed: f32,
    tau: f32,
}

const PUSH: f32 = 0.06;
const STOP: f32 = 40.;

impl Flick {
    /// How long the momentum lasts after the push.
    fn coast(&self) -> f32 {
        self.tau * (self.speed.abs() / STOP).max(1.).ln()
    }

    fn end(&self) -> f32 {
        self.at + PUSH + self.coast()
    }

    /// How far the flick has moved the wall by `t` seconds after its start.
    fn distance(&self, t: f32) -> f32 {
        if t <= 0. {
            return 0.;
        }
        if t <= PUSH {
            return self.speed * t;
        }
        let coast = (t - PUSH).min(self.coast());
        self.speed * PUSH + self.speed * self.tau * (1. - (-coast / self.tau).exp())
    }
}

pub(crate) struct Fling {
    flicks: Vec<Flick>,
    started: Option<Instant>,
    last: f32,
    length: f32,
    random: Random,
    /// The direction of the next flick.
    down: bool,
    pending: Option<f32>,
    finished: bool,
}

impl Fling {
    pub(crate) fn new(seconds: f32, seed: u64) -> Self {
        Self {
            flicks: Vec::new(),
            started: None,
            last: 0.,
            length: seconds,
            random: Random(seed.max(1)),
            down: true,
            pending: None,
            finished: false,
        }
    }

    pub(crate) fn done(&self) -> bool {
        self.finished
    }

    fn speed(&mut self) -> f32 {
        let median = 11000f64;
        let sigma = (30000f64 / median).ln() / 1.2816;
        (median * (sigma * self.random.normal()).exp()).clamp(300., 47000.) as f32
    }

    /// Where the wall is (`top` of `max`) at this frame: the movement to
    /// make now, and the phase of the event that carries it.
    pub(crate) fn step_at(
        &mut self,
        now: Instant,
        top: f32,
        max: f32,
    ) -> Option<(f32, TouchPhase)> {
        let started = *self.started.get_or_insert(now);
        let t = now.duration_since(started).as_secs_f32();
        if t > self.length {
            self.finished = true;
            return None;
        }
        // Most flicks come while the last is still coasting, once it has
        // slowed to a quarter of its speed, as a fast hand flicks again; the
        // rest after it has stopped and the hand has rested.
        if self.pending.is_none() {
            match self.flicks.last() {
                None => self.pending = Some(t),
                Some(flick) => {
                    let again = self.random.next() < 0.75;
                    let at = if again {
                        flick.at
                            + PUSH
                            + flick.tau * 2.5f32.ln()
                            + 0.05
                            + self.random.next() as f32 * 0.1
                    } else {
                        flick.end() + 0.12 + self.random.next() as f32 * 0.38
                    };
                    self.pending = Some(at);
                }
            }
        }
        if let Some(at) = self.pending
            && t >= at
        {
            self.pending = None;
            if top > max - 6000. {
                self.down = false;
            } else if top < 6000. {
                self.down = true;
            } else if self.random.next() < 0.25 {
                self.down = !self.down;
            }
            let speed = self.speed();
            let tau = 0.35 + self.random.next() as f32 * 0.25;
            self.flicks.push(Flick {
                at,
                speed: if self.down { speed } else { -speed },
                tau,
            });
        }
        let previous = self.last;
        self.last = t;
        let flick = *self.flicks.last()?;
        if t < flick.at || previous > flick.end() {
            return None;
        }
        let moved = flick.distance(t - flick.at) - flick.distance(previous - flick.at);
        let phase = if previous < flick.at {
            TouchPhase::Started
        } else if previous < flick.at + PUSH && t >= flick.at + PUSH {
            TouchPhase::Ended
        } else {
            TouchPhase::Moved
        };
        Some((moved, phase))
    }

    pub(crate) fn speeds(&self) -> Vec<f32> {
        self.flicks.iter().map(|flick| flick.speed.abs()).collect()
    }
}

/// What the wall did under the fling.
#[derive(Default)]
pub(crate) struct Stats {
    pub last: Option<Instant>,
    pub intervals: Vec<f32>,
    pub work: Vec<f32>,
    pub tile_frames: u64,
    pub cover_frames: u64,
    pub entered: u64,
    pub entered_unfinished: u64,
    pub pending_paint: Option<(Instant, Rc<Cell<Option<Instant>>>)>,
    pub footprint_start: Option<u64>,
    pub out: Option<PathBuf>,
    pub flicks: Vec<f32>,
    /// Points a second of each frame that moved, as the recorded trace counted updates.
    pub speeds: Vec<f32>,
    pub label: String,
    /// Whether the app quits when the fling is over: not when a script runs.
    pub stay: bool,
}

impl Stats {
    pub(crate) fn frame(&mut self, now: Instant, tiles: usize, covers: usize) {
        if self.footprint_start.is_none() {
            self.footprint_start = crate::platform::footprint();
        }
        if let Some(last) = self.last {
            self.intervals
                .push(now.duration_since(last).as_secs_f32() * 1000.);
        }
        self.last = Some(now);
        if let Some((started, painted)) = self.pending_paint.take()
            && let Some(painted) = painted.get()
        {
            self.work
                .push(painted.duration_since(started).as_secs_f32() * 1000.);
        }
        self.tile_frames += tiles as u64;
        self.cover_frames += covers as u64;
    }

    pub(crate) fn report(&self, cache: &Cache, loader: LoaderCounts) {
        let percentile = |values: &[f32], p: f32| -> f32 {
            if values.is_empty() {
                return 0.;
            }
            let mut sorted = values.to_vec();
            sorted.sort_by(f32::total_cmp);
            sorted
                .get(((sorted.len() - 1) as f32 * p).round() as usize)
                .copied()
                .unwrap_or(0.)
        };
        let frames = self.intervals.len();
        let long = |limit: f32| self.intervals.iter().filter(|ms| **ms > limit).count();
        let mean = self.intervals.iter().sum::<f32>() / frames.max(1) as f32;
        let footprint_end = crate::platform::footprint();
        let mb = |bytes: Option<u64>| bytes.map_or(-1., |bytes| bytes as f64 / 1024. / 1024.);
        let report = serde_json::json!({
            "label": self.label,
            "frames": frames,
            "meanIntervalMs": mean,
            "p50IntervalMs": percentile(&self.intervals, 0.5),
            "p95IntervalMs": percentile(&self.intervals, 0.95),
            "p99IntervalMs": percentile(&self.intervals, 0.99),
            "maxIntervalMs": percentile(&self.intervals, 1.0),
            "framesOver12ms": long(12.),
            "framesOver20ms": long(20.),
            "framesOver34ms": long(34.),
            "p50WorkMs": percentile(&self.work, 0.5),
            "p95WorkMs": percentile(&self.work, 0.95),
            "maxWorkMs": percentile(&self.work, 1.0),
            "tileFramesWithCover": self.cover_frames as f64 / self.tile_frames.max(1) as f64,
            "tilesEntered": self.entered,
            "tilesEnteredWithoutCover": self.entered_unfinished,
            "enteredUnfinishedShare": self.entered_unfinished as f64 / self.entered.max(1) as f64,
            "flicks": self.flicks.len(),
            "flickSpeedsMedian": percentile(&self.flicks, 0.5),
            "flickSpeedsP90": percentile(&self.flicks, 0.9),
            "flickSpeedsMax": percentile(&self.flicks, 1.0),
            "updateSpeedsMedian": percentile(&self.speeds, 0.5),
            "updateSpeedsP90": percentile(&self.speeds, 0.9),
            "updateSpeedsMax": percentile(&self.speeds, 1.0),
            "coversHeld": cache.len(),
            "coverBytesMb": cache.bytes() as f64 / 1024. / 1024.,
            "evicted": cache.evicted,
            "fetched": loader.fetched,
            "aborted": loader.aborted,
            "decoded": loader.decoded,
            "skippedDecode": loader.skipped_decode,
            "footprintStartMb": mb(self.footprint_start),
            "footprintEndMb": mb(footprint_end),
        });
        let mut report = report;
        if let Some(fields) = report.as_object_mut() {
            fields.insert(
                "memoryEnd".to_owned(),
                serde_json::Value::String(crate::platform::memory_line()),
            );
        }
        let text = serde_json::to_string_pretty(&report).unwrap_or_default();
        eprintln!("{text}");
        if let Some(out) = &self.out {
            let _ = std::fs::write(out, text);
        }
    }
}

pub(crate) fn wait() -> Duration {
    Duration::from_millis(1500)
}
