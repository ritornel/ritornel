//! How fast the wall is moving, and where a flick will come to rest.
//!
//! macOS and Windows both send a touchpad's momentum as a stream of scroll
//! events after the fingers lift, slowing roughly exponentially. The wall
//! reads the velocity from those events and estimates the decay's time
//! constant from how the velocity falls, so it can say where the flick
//! will stop: `top + v·τ`. That point is where covers are wanted first.

use std::time::{Duration, Instant};

use gpui::TouchPhase;

/// The time constant to assume before one has been measured. Apple's
/// "normal" deceleration (0.998 a millisecond) is about half a second.
const DEFAULT_TAU: f32 = 0.45;

#[derive(Debug)]
pub(crate) struct Motion {
    velocity: f32,
    last: Option<Instant>,
    /// Whether the fingers have lifted, so what comes now is momentum.
    pub momentum: bool,
    pub tau: f32,
    sample: Option<(Instant, f32)>,
    pub moved_at: Option<Instant>,
}

impl Default for Motion {
    fn default() -> Self {
        Self {
            velocity: 0.,
            last: None,
            momentum: false,
            tau: DEFAULT_TAU,
            sample: None,
            moved_at: None,
        }
    }
}

impl Motion {
    /// One scroll event that moved the wall `dy` points (positive is down
    /// the wall).
    pub(crate) fn event(&mut self, dy: f32, phase: TouchPhase, now: Instant) {
        match phase {
            TouchPhase::Started => {
                self.momentum = false;
                self.sample = None;
            }
            TouchPhase::Ended => self.momentum = true,
            _ => {}
        }
        if dy == 0. {
            return;
        }
        self.moved_at = Some(now);
        let dt = self
            .last
            .map_or(f32::INFINITY, |last| now.duration_since(last).as_secs_f32());
        self.last = Some(now);
        if dt > 0.1 {
            // A new movement: what it says is all there is to go on.
            self.velocity = dy / (1. / 120.);
            self.sample = None;
            return;
        }
        let dt = dt.max(0.001);
        let instant = dy / dt;
        let weight = 1. - (-dt / 0.025).exp();
        self.velocity += (instant - self.velocity) * weight;
        if self.momentum && self.velocity.abs() > 300. {
            match self.sample {
                Some((at, before)) if now.duration_since(at) > Duration::from_millis(50) => {
                    if before.signum() == self.velocity.signum()
                        && self.velocity.abs() < before.abs()
                    {
                        let span = now.duration_since(at).as_secs_f32();
                        let estimate = span / (before.abs() / self.velocity.abs()).ln();
                        self.tau = self.tau * 0.7 + estimate.clamp(0.1, 1.5) * 0.3;
                    }
                    self.sample = Some((now, self.velocity));
                }
                None => self.sample = Some((now, self.velocity)),
                _ => {}
            }
        }
    }

    /// Points a second, down the wall; nothing once events have stopped.
    pub(crate) fn velocity(&self, now: Instant) -> f32 {
        match self.last {
            Some(last) if now.duration_since(last) < Duration::from_millis(80) => self.velocity,
            _ => 0.,
        }
    }

    /// How far on from here the wall is expected to come to rest.
    pub(crate) fn throw(&self, now: Instant) -> f32 {
        self.velocity(now) * self.tau
    }

    /// Whether the wall is moving, or stopped less than `within` ago.
    pub(crate) fn moving(&self, now: Instant, within: Duration) -> bool {
        self.moved_at
            .is_some_and(|at| now.duration_since(at) < within)
    }
}
