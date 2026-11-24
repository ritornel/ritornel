// Parts of this file are adapted from WebKit: ScrollingEffectsController
// (Source/WebCore/platform/ScrollingEffectsController.cpp and
// Source/WebCore/platform/mac/ScrollingEffectsController.mm) and
// ScrollAnimationRubberBand
// (Source/WebCore/platform/mac/ScrollAnimationRubberBand.mm), ported to Rust
// for the vertical axis. Those parts stay under WebKit's licence, which
// follows; the rest is under this project's (LICENSE).
//
// Copyright (C) 2011-2017 Apple Inc. All rights reserved.
// Copyright (C) 2021 Apple Inc. All rights reserved.
//
// Redistribution and use in source and binary forms, with or without
// modification, are permitted provided that the following conditions
// are met:
// 1. Redistributions of source code must retain the above copyright
//    notice, this list of conditions and the following disclaimer.
// 2. Redistributions in binary form must reproduce the above copyright
//    notice, this list of conditions and the following disclaimer in the
//    documentation and/or other materials provided with the distribution.
//
// THIS SOFTWARE IS PROVIDED BY APPLE INC. AND ITS CONTRIBUTORS ``AS IS''
// AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO,
// THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR
// PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL APPLE INC. OR ITS CONTRIBUTORS
// BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR
// CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF
// SUBSTITUTE GOODS OR SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS
// INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN
// CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE)
// ARISING IN ANY WAY OUT OF THE USE OF THIS SOFTWARE, EVEN IF ADVISED OF
// THE POSSIBILITY OF SUCH DAMAGE.

//! The stretch at either end of a page on macOS: WebKit's
//! `ScrollingEffectsController` (its Mac half) and
//! `ScrollAnimationRubberBand`, ported for the vertical axis, with the
//! AppKit functions they call written out from what they return here.
//!
//! - A touchpad's gesture that begins where the page cannot move that way
//!   (at its top, pulling down) is not the page's at all: WebKit latches it
//!   to whatever can scroll, and nothing here can. So pulling at the top
//!   from rest does nothing.
//! - A gesture that is the page's pushes it past its end by a twentieth of
//!   the touchpad's travel: AppKit's `_NSElasticDeltaForReboundDelta(f)` is
//!   `f / 20`. Once stretched, the touchpad's unaccelerated travel counts,
//!   as in AppKit; going back takes the stretch in point for point.
//! - When the fingers lift, or when a flick's momentum reaches the end, the
//!   page springs back: `_NSElasticDeltaForTimeDelta(x, v, t)` is
//!   `(x − 0.31·v·t)·e^(−12.5·t)`, which WebKit calls with the stretch and
//!   the momentum's velocity, negated. Outward momentum is let go; momentum
//!   back into the page takes over.
//! - A mouse wheel never stretches.
//! - A scroll with no phase is a wheel's, however precise: many mice send
//!   pixel deltas, but only a touch surface sends a gesture or momentum.
//!
//! It is the Mac's feel, so only where the platform says pages stretch
//! (`platform::FEEL`). GPUI's scroll event leaves out the phases AppKit
//! gives and the unaccelerated delta; the platform reads them from AppKit
//! itself (`platform::scroll_details`).

use std::time::{Duration, Instant};

/// AppKit's rubber band: a stretch is the force over its stiffness.
const STIFFNESS: f32 = 20.;
/// `_NSElasticDeltaForTimeDelta`'s amplitude, and its rate: stiffness 20
/// over a period of 1.6.
const AMPLITUDE: f32 = 0.31;
const RATE: f32 = 12.5;
/// WebKit's `scrollVelocityZeroingTimeout`.
const VELOCITY_ZEROING: f64 = 0.1;

/// A gesture's or its momentum's phase, as AppKit gives it.
pub(crate) use crate::platform::ScrollPhase as Phase;

/// One scroll, as WebKit reads a wheel event.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Wheel {
    /// Down the page is positive.
    pub dy: f32,
    pub unaccelerated_dy: f32,
    pub phase: Phase,
    pub momentum: Phase,
    /// A touchpad's, not a mouse wheel's.
    pub precise: bool,
    /// Seconds, by a steady clock.
    pub time: f64,
}

impl Wheel {
    /// GPUI's event, with what AppKit says of it when there is that.
    pub(crate) fn of(event: &gpui::ScrollEvent) -> Self {
        let (dy, precise) = match event.delta {
            gpui::ScrollDelta::Pixels(delta) => (-f32::from(delta.y), true),
            gpui::ScrollDelta::Lines(lines) => (-lines.y * 48., false),
        };
        // Only where the system's pages stretch: elsewhere a touchpad's
        // scroll moves the page as a wheel's does.
        let precise = precise && crate::platform::FEEL.rubber_band;
        if let Some(native) = crate::platform::scroll_details() {
            return Self {
                dy,
                unaccelerated_dy: native.unaccelerated_dy,
                phase: native.phase,
                momentum: native.momentum,
                precise,
                time: native.timestamp,
            };
        }
        let phase = match (precise, event.touch_phase) {
            (false, _) => Phase::None,
            (true, gpui::TouchPhase::Started) => Phase::Began,
            (true, gpui::TouchPhase::Moved) => Phase::Changed,
            (true, gpui::TouchPhase::Ended) => Phase::Ended,
            (true, gpui::TouchPhase::Cancelled) => Phase::Cancelled,
        };
        Self {
            dy,
            unaccelerated_dy: dy,
            phase,
            momentum: Phase::None,
            precise,
            time: clock(),
        }
    }

    /// A scroll that only moves the page, never stretching it: a jump, or
    /// the measure's fling.
    pub(crate) fn plain(dy: f32) -> Self {
        Self {
            dy,
            unaccelerated_dy: dy,
            phase: Phase::None,
            momentum: Phase::None,
            precise: false,
            time: clock(),
        }
    }

    /// A touchpad's scroll in `phase`, for a script.
    pub(crate) fn touch(dy: f32, phase: Phase) -> Self {
        Self {
            precise: true,
            phase,
            ..Self::plain(dy)
        }
    }

    fn is_momentum(&self) -> bool {
        self.phase == Phase::None && self.momentum != Phase::None
    }
}

/// Seconds by a steady clock, for a scroll that comes without AppKit's.
fn clock() -> f64 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64()
}

#[derive(Debug, Clone, Copy)]
struct Spring {
    start: Instant,
    overscroll: f32,
    velocity: f32,
}

/// WebKit's `roundToDevicePixelTowardZero`.
fn toward_zero(value: f32) -> f32 {
    let rounded = value.round();
    let value = if (value - rounded).abs() < 0.125 {
        rounded
    } else {
        value
    };
    if value > 0. {
        (value - 0.5).ceil()
    } else {
        (value + 0.5).floor()
    }
}

/// `_NSElasticDeltaForTimeDelta`.
pub(crate) fn elastic_for_time(position: f32, velocity: f32, seconds: f32) -> f32 {
    (position - AMPLITUDE * velocity * seconds) * (-RATE * seconds).exp()
}

#[derive(Debug, Default)]
pub(crate) struct Elastic {
    /// How far the page is past its end: below zero above its top.
    stretch: f32,
    /// The touchpad's travel the stretch is made of.
    force: f32,
    /// Whether the gesture under way is this page's (WebKit's latching).
    /// `None` until its first movement says which way it goes.
    latched: Option<bool>,
    momentum: bool,
    ignore_momentum: bool,
    /// The last momentum or gesture event's time, and when it came.
    last: Option<(f64, Instant)>,
    velocity: f32,
    spring: Option<Spring>,
}

impl Elastic {
    /// How far the page is drawn past its end: below zero above its top.
    pub(crate) fn shown(&self) -> f32 {
        self.stretch
    }

    /// Lets go of any stretch at once: the page has moved by other means.
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }

    /// Scrolls `top`, between 0 and `max`, as WebKit scrolls a page.
    pub(crate) fn scroll(&mut self, top: &mut f32, wheel: Wheel, max: f32) {
        self.scroll_at(top, wheel, max, Instant::now());
    }

    /// As [`scroll`](Self::scroll), with the wheel's event come at `now`.
    fn scroll_at(&mut self, top: &mut f32, wheel: Wheel, max: f32, now: Instant) {
        // A touch surface sends a gesture or momentum phase; a wheel, however
        // fine its steps, sends neither, and never stretches the page.
        let touch = wheel.precise && (wheel.phase != Phase::None || wheel.momentum != Phase::None);
        let in_gesture = touch && matches!(wheel.phase, Phase::Began | Phase::Changed);
        if wheel.phase == Phase::MayBegin {
            return;
        }
        if wheel.phase == Phase::Cancelled {
            self.velocity = 0.;
            self.start_spring_if_needed(now);
            return;
        }
        if wheel.phase == Phase::Began {
            self.latched = None;
            self.momentum = false;
            self.ignore_momentum = false;
            self.last = None;
            self.velocity = 0.;
            self.force = self.stretch * STIFFNESS;
            self.spring = None;
        }
        if !touch {
            // A non-gesture wheel interrupts any active spring or stretch and
            // ignores trailing momentum from an interrupted flick.
            if self.momentum || self.spring.is_some() {
                self.ignore_momentum = true;
            }
            self.momentum = false;
            self.latched = None;
            self.spring = None;
            self.stretch = 0.;
            self.force = 0.;
        }
        // Latching: a gesture and its momentum go to the page only if, when
        // it first moves, the page can move that way, or is stretched.
        if touch {
            if self.latched.is_none() && wheel.dy != 0. {
                let can = (wheel.dy < 0. && *top > 0.)
                    || (wheel.dy > 0. && *top < max)
                    || self.stretch != 0.;
                self.latched = Some(can);
            }
            if self.latched == Some(false) {
                return;
            }
        }
        if wheel.phase == Phase::Ended {
            self.start_spring_if_needed(now);
            return;
        }
        if self.ignore_momentum && wheel.is_momentum() {
            if wheel.momentum == Phase::Ended {
                self.ignore_momentum = false;
            }
            return;
        }
        if self.spring.is_some() && wheel.is_momentum() {
            let delta = if self.stretch != 0. {
                wheel.unaccelerated_dy
            } else {
                wheel.dy
            };
            let opposes = delta != 0.
                && if self.stretch != 0. {
                    self.stretch.signum() != delta.signum()
                } else {
                    !pinned(*top, max, delta)
                };
            if opposes {
                // Opposing momentum (such as a quick flick back before the
                // fingers clear the bounce, or during the 24 ms spring settle
                // window) pulls the stretch back and takes over from the spring.
                self.spring = None;
                if self.stretch == 0. {
                    self.force = 0.;
                }
            } else {
                // Same-direction momentum after a pull let go would fight the
                // spring; ignore the rest of that flick once it moves.
                if delta != 0. {
                    self.ignore_momentum = true;
                    self.momentum = false;
                }
                if wheel.momentum == Phase::Ended {
                    self.ignore_momentum = false;
                    self.momentum = false;
                }
                return;
            }
        }
        let stretched = self.stretch != 0.;
        let delta = if stretched {
            wheel.unaccelerated_dy
        } else {
            wheel.dy
        };
        if !self.momentum && matches!(wheel.momentum, Phase::Began | Phase::Changed) {
            self.momentum = true;
        }
        let mut should_stretch = false;
        if in_gesture || self.momentum {
            match self.last {
                Some((at, _)) if wheel.time - at > 0. && wheel.time - at < VELOCITY_ZEROING => {
                    self.velocity = delta / (wheel.time - at) as f32;
                }
                _ => self.velocity = 0.,
            }
            self.last = Some((wheel.time, now));
            // `modifyScrollDeltaForStretching`: towards an end the page is at.
            should_stretch = !stretched && pinned(*top, max, delta);
        }
        if delta != 0. {
            if should_stretch || stretched {
                let can_start = self.stretch_by(top, delta, stretched, max);
                if self.momentum && can_start && self.last.is_some() {
                    self.ignore_momentum = true;
                    self.momentum = false;
                    self.start_spring_if_needed(now);
                }
            } else {
                *top = (*top + delta).clamp(0., max);
                self.force = 0.;
            }
        }
        if self.momentum && wheel.momentum == Phase::Ended {
            self.momentum = false;
            self.ignore_momentum = false;
            self.last = None;
            self.start_spring_if_needed(now);
        }
    }

    /// `applyScrollDeltaWithStretching`. Whether a spring may start: the
    /// page is at the end the event goes towards.
    fn stretch_by(&mut self, top: &mut f32, delta: f32, stretched: bool, max: f32) -> bool {
        let opposes = stretched && self.stretch.signum() != delta.signum();
        let can_start = self.momentum && pinned(*top, max, delta);
        if opposes {
            // Going back takes the stretch in point for point; keep force
            // matched to the remaining stretch so leaving the end clears it.
            let to = self.stretch + delta;
            if to == 0. || to.signum() != self.stretch.signum() {
                // Back to the end, or past it into the page.
                self.stretch = 0.;
                self.force = 0.;
                if to != 0. {
                    *top = (*top + to).clamp(0., max);
                }
            } else {
                self.stretch = to;
                self.force = to * STIFFNESS;
            }
        } else {
            self.force += delta;
            let next = (self.force / STIFFNESS).ceil();
            // Never let a small outward push round a fractional stretch inward.
            self.stretch = if delta < 0. {
                self.stretch.min(next)
            } else {
                self.stretch.max(next)
            };
        }
        can_start
    }

    /// `startRubberBandAnimationIfNecessary`.
    fn start_spring_if_needed(&mut self, now: Instant) {
        if let Some((_, at)) = self.last
            && now.duration_since(at).as_secs_f64() >= VELOCITY_ZEROING
        {
            self.velocity = 0.;
        }
        if self.spring.is_some() || (self.stretch == 0. && self.force == 0.) {
            return;
        }
        let velocity = if self.stretch != 0.
            && self.velocity != 0.
            && self.stretch.signum() != self.velocity.signum()
        {
            0.
        } else {
            -self.velocity
        };
        self.spring = Some(Spring {
            start: now,
            overscroll: self.stretch,
            // WebKit passes the momentum's velocity negated.
            velocity,
        });
    }

    /// Moves the spring on. Whether it is still moving, for another frame.
    pub(crate) fn step(&mut self, now: Instant) -> bool {
        let Some(spring) = self.spring else {
            return false;
        };
        let elapsed = now.saturating_duration_since(spring.start);
        let offset = toward_zero(elastic_for_time(
            spring.overscroll,
            spring.velocity,
            elapsed.as_secs_f32(),
        ));
        self.stretch = offset;
        if offset == 0. && elapsed > Duration::from_millis(24) {
            self.spring = None;
            self.force = 0.;
            return false;
        }
        true
    }
}

/// Whether the page is at the end that `delta` goes towards.
fn pinned(top: f32, max: f32, delta: f32) -> bool {
    (delta < 0. && top <= 0.) || (delta > 0. && top >= max)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wheel(dy: f32, phase: Phase, momentum: Phase, time: f64) -> Wheel {
        Wheel {
            dy,
            unaccelerated_dy: dy,
            phase,
            momentum,
            precise: true,
            time,
        }
    }

    /// Scrolls as if `wheel` came when its time says, counted from `t0`,
    /// so that the spring's clock and the wheel's agree however long the
    /// test takes.
    fn scroll(elastic: &mut Elastic, top: &mut f32, wheel: Wheel, t0: Instant) {
        let now = t0 + Duration::from_secs_f64(wheel.time);
        elastic.scroll_at(top, wheel, 1000., now);
    }

    #[test]
    fn the_spring_is_appkits() {
        // Read from `_NSElasticDeltaForTimeDelta` on macOS 26.
        assert!((elastic_for_time(100., 0., 0.1) - 28.650_48).abs() < 0.001);
        assert!((elastic_for_time(0., 1000., 0.1) + 8.881_649).abs() < 0.001);
        assert!((elastic_for_time(50., 1000., 0.05) - 18.466_52).abs() < 0.001);
    }

    #[test]
    fn a_pull_at_the_top_from_rest_does_nothing() {
        let mut elastic = Elastic::default();
        let mut top = 0.;
        elastic.scroll(&mut top, wheel(0., Phase::Began, Phase::None, 0.), 1000.);
        for at in 1..20 {
            let event = wheel(-10., Phase::Changed, Phase::None, f64::from(at) * 0.01);
            elastic.scroll(&mut top, event, 1000.);
        }
        assert_eq!((top, elastic.shown()), (0., 0.));
    }

    #[test]
    fn a_pull_that_reaches_the_top_stretches_by_a_twentieth() {
        let mut elastic = Elastic::default();
        let mut top = 50.;
        elastic.scroll(&mut top, wheel(0., Phase::Began, Phase::None, 0.), 1000.);
        elastic.scroll(
            &mut top,
            wheel(-50., Phase::Changed, Phase::None, 0.01),
            1000.,
        );
        assert_eq!(top, 0.);
        for at in 2..12 {
            let event = wheel(-20., Phase::Changed, Phase::None, f64::from(at) * 0.01);
            elastic.scroll(&mut top, event, 1000.);
        }
        // 200 points of travel past the top: a stretch of 10.
        assert_eq!(elastic.shown(), -10.);
        // Going back takes it in point for point.
        elastic.scroll(
            &mut top,
            wheel(4., Phase::Changed, Phase::None, 0.12),
            1000.,
        );
        assert_eq!(elastic.shown(), -6.);
    }

    #[test]
    fn it_springs_back_when_the_fingers_lift() {
        let t0 = Instant::now();
        let mut elastic = Elastic::default();
        let mut top = 10.;
        scroll(
            &mut elastic,
            &mut top,
            wheel(0., Phase::Began, Phase::None, 0.),
            t0,
        );
        for at in 1..40 {
            let event = wheel(-20., Phase::Changed, Phase::None, f64::from(at) * 0.01);
            scroll(&mut elastic, &mut top, event, t0);
        }
        let pulled = elastic.shown();
        assert!(pulled < -30., "{pulled}");
        scroll(
            &mut elastic,
            &mut top,
            wheel(0., Phase::Ended, Phase::None, 0.4),
            t0,
        );
        let start = t0 + Duration::from_secs_f64(0.4);
        let mut now = start;
        while elastic.step(now) {
            now += Duration::from_millis(8);
            assert!(now - start < Duration::from_secs(1), "it settles");
        }
        assert_eq!(elastic.shown(), 0.);
        assert!(now - start > Duration::from_millis(200));
    }

    #[test]
    fn a_flick_that_reaches_the_end_bounces_by_its_speed() {
        let t0 = Instant::now();
        let mut elastic = Elastic::default();
        let mut top = 900.;
        scroll(
            &mut elastic,
            &mut top,
            wheel(0., Phase::Began, Phase::None, 0.),
            t0,
        );
        scroll(
            &mut elastic,
            &mut top,
            wheel(10., Phase::Changed, Phase::None, 0.01),
            t0,
        );
        scroll(
            &mut elastic,
            &mut top,
            wheel(0., Phase::Ended, Phase::None, 0.02),
            t0,
        );
        let mut time = 0.03;
        scroll(
            &mut elastic,
            &mut top,
            wheel(30., Phase::None, Phase::Began, time),
            t0,
        );
        while top < 1000. {
            time += 0.01;
            scroll(
                &mut elastic,
                &mut top,
                wheel(30., Phase::None, Phase::Changed, time),
                t0,
            );
        }
        time += 0.01;
        scroll(
            &mut elastic,
            &mut top,
            wheel(30., Phase::None, Phase::Changed, time),
            t0,
        );
        // 3,000 points a second: a bounce of about 27 points at 80 ms.
        let start = t0 + Duration::from_secs_f64(time);
        let mut most: f32 = 0.;
        let mut now = start;
        while elastic.step(now) {
            most = most.max(elastic.shown());
            now += Duration::from_millis(4);
        }
        assert!((24. ..=30.).contains(&most), "{most}");
        // The rest of the flick is let go.
        scroll(
            &mut elastic,
            &mut top,
            wheel(30., Phase::None, Phase::Changed, time + 0.01),
            t0,
        );
        assert_eq!(elastic.shown(), 0.);
    }

    #[test]
    fn a_mouse_wheel_never_stretches() {
        let mut elastic = Elastic::default();
        let mut top = 0.;
        let mut notch = wheel(-48., Phase::None, Phase::None, 0.);
        notch.precise = false;
        elastic.scroll(&mut top, notch, 1000.);
        assert_eq!((top, elastic.shown()), (0., 0.));
    }

    #[test]
    fn a_precise_wheel_never_stretches() {
        let mut elastic = Elastic::default();
        let mut top = 0.;
        elastic.scroll(&mut top, wheel(-10., Phase::None, Phase::None, 0.), 1000.);
        assert_eq!((top, elastic.shown()), (0., 0.));
        assert!(!elastic.step(Instant::now()));

        let mut top = 1000.;
        for at in 0..10 {
            let event = wheel(10., Phase::None, Phase::None, f64::from(at) * 0.01);
            elastic.scroll(&mut top, event, 1000.);
        }
        assert_eq!((top, elastic.shown()), (1000., 0.));
        assert!(!elastic.step(Instant::now()));
    }

    #[test]
    fn a_flick_back_up_while_stretched_keeps_its_momentum() {
        let t0 = Instant::now();
        let mut elastic = Elastic::default();
        let mut top = 900.;

        // 1. A fast flick down hits the bottom and starts bouncing.
        scroll(
            &mut elastic,
            &mut top,
            wheel(0., Phase::Began, Phase::None, 0.),
            t0,
        );
        scroll(
            &mut elastic,
            &mut top,
            wheel(10., Phase::Changed, Phase::None, 0.01),
            t0,
        );
        scroll(
            &mut elastic,
            &mut top,
            wheel(0., Phase::Ended, Phase::None, 0.02),
            t0,
        );
        let mut time = 0.03;
        scroll(
            &mut elastic,
            &mut top,
            wheel(30., Phase::None, Phase::Began, time),
            t0,
        );
        while top < 1000. {
            time += 0.01;
            scroll(
                &mut elastic,
                &mut top,
                wheel(30., Phase::None, Phase::Changed, time),
                t0,
            );
        }
        time += 0.01;
        scroll(
            &mut elastic,
            &mut top,
            wheel(30., Phase::None, Phase::Changed, time),
            t0,
        );
        // Step the spring into its bounce so the page is stretched past the bottom.
        time += 0.04;
        assert!(elastic.step(t0 + Duration::from_secs_f64(time)));
        let stretched = elastic.shown();
        assert!(stretched > 5., "expected bottom stretch, got {stretched}");

        // 2. While still stretched, start a new gesture upward past the stretch.
        time += 0.01;
        scroll(
            &mut elastic,
            &mut top,
            wheel(0., Phase::Began, Phase::None, time),
            t0,
        );
        for _ in 0..3 {
            time += 0.01;
            scroll(
                &mut elastic,
                &mut top,
                wheel(-20., Phase::Changed, Phase::None, time),
                t0,
            );
        }
        assert_eq!(elastic.shown(), 0.);
        let after_drag = top;
        assert!(
            after_drag < 1000.,
            "drag should move back into page: {after_drag}"
        );

        // 3. Lift fingers: no spring should start because stretch is back to zero.
        time += 0.01;
        scroll(
            &mut elastic,
            &mut top,
            wheel(0., Phase::Ended, Phase::None, time),
            t0,
        );
        assert!(!elastic.step(t0 + Duration::from_secs_f64(time + 0.016)));

        // 4. Upward momentum must keep scrolling the page up.
        time += 0.01;
        scroll(
            &mut elastic,
            &mut top,
            wheel(-30., Phase::None, Phase::Began, time),
            t0,
        );
        for _ in 0..5 {
            time += 0.01;
            scroll(
                &mut elastic,
                &mut top,
                wheel(-30., Phase::None, Phase::Changed, time),
                t0,
            );
        }
        assert!(
            top <= after_drag - 150.,
            "upward momentum was swallowed: after_drag={after_drag}, top={top}, stretch={}",
            elastic.shown()
        );
        assert_eq!(elastic.shown(), 0.);
    }

    #[test]
    fn a_small_pull_at_the_top_that_reverses_into_the_page_keeps_its_momentum() {
        let t0 = Instant::now();
        let mut elastic = Elastic::default();
        let mut top = 10.;
        scroll(
            &mut elastic,
            &mut top,
            wheel(0., Phase::Began, Phase::None, 0.),
            t0,
        );
        // Reach the top and pull by less than STIFFNESS: stretch stays 0 while force is -5.
        scroll(
            &mut elastic,
            &mut top,
            wheel(-10., Phase::Changed, Phase::None, 0.01),
            t0,
        );
        scroll(
            &mut elastic,
            &mut top,
            wheel(-5., Phase::Changed, Phase::None, 0.02),
            t0,
        );
        assert_eq!((top, elastic.shown()), (0., 0.));
        // Reverse back into the page, lift, and flick downward.
        scroll(
            &mut elastic,
            &mut top,
            wheel(20., Phase::Changed, Phase::None, 0.03),
            t0,
        );
        scroll(
            &mut elastic,
            &mut top,
            wheel(0., Phase::Ended, Phase::None, 0.04),
            t0,
        );
        assert!(!elastic.step(t0 + Duration::from_secs_f64(0.056)));
        scroll(
            &mut elastic,
            &mut top,
            wheel(30., Phase::None, Phase::Began, 0.05),
            t0,
        );
        scroll(
            &mut elastic,
            &mut top,
            wheel(30., Phase::None, Phase::Changed, 0.06),
            t0,
        );
        assert_eq!(top, 80.);
        assert_eq!(elastic.shown(), 0.);
    }

    #[test]
    fn pulling_back_partway_and_pushing_again_continues_from_remaining_stretch() {
        let mut elastic = Elastic::default();
        let mut top = 950.;
        elastic.scroll(&mut top, wheel(0., Phase::Began, Phase::None, 0.), 1000.);
        elastic.scroll(
            &mut top,
            wheel(50., Phase::Changed, Phase::None, 0.01),
            1000.,
        );
        for at in 2..12 {
            elastic.scroll(
                &mut top,
                wheel(20., Phase::Changed, Phase::None, f64::from(at) * 0.01),
                1000.,
            );
        }
        assert_eq!(elastic.shown(), 10.);
        // Pull back 4 points to +6, then push 20 points further: must step to +7, not jump to +11.
        elastic.scroll(
            &mut top,
            wheel(-4., Phase::Changed, Phase::None, 0.12),
            1000.,
        );
        assert_eq!(elastic.shown(), 6.);
        elastic.scroll(
            &mut top,
            wheel(20., Phase::Changed, Phase::None, 0.13),
            1000.,
        );
        assert_eq!(elastic.shown(), 7.);
    }

    #[test]
    fn a_mouse_wheel_interrupts_an_active_spring() {
        let t0 = Instant::now();
        let mut elastic = Elastic::default();
        let mut top = 970.;
        scroll(
            &mut elastic,
            &mut top,
            wheel(0., Phase::Began, Phase::None, 0.),
            t0,
        );
        scroll(
            &mut elastic,
            &mut top,
            wheel(10., Phase::Changed, Phase::None, 0.01),
            t0,
        );
        scroll(
            &mut elastic,
            &mut top,
            wheel(0., Phase::Ended, Phase::None, 0.02),
            t0,
        );
        scroll(
            &mut elastic,
            &mut top,
            wheel(30., Phase::None, Phase::Began, 0.03),
            t0,
        );
        scroll(
            &mut elastic,
            &mut top,
            wheel(30., Phase::None, Phase::Changed, 0.04),
            t0,
        );
        assert!(elastic.step(t0 + Duration::from_secs_f64(0.08)));
        assert!(elastic.shown() > 0.);
        // A discrete mouse wheel scrolls back into the page instead of being ignored.
        let mut notch = wheel(-48., Phase::None, Phase::None, 0.09);
        notch.precise = false;
        scroll(&mut elastic, &mut top, notch, t0);
        assert_eq!(elastic.shown(), 0.);
        assert_eq!(top, 952.);
        assert!(!elastic.step(t0 + Duration::from_secs_f64(0.11)));
        // Trailing momentum from the interrupted trackpad flick stays ignored.
        scroll(
            &mut elastic,
            &mut top,
            wheel(30., Phase::None, Phase::Changed, 0.12),
            t0,
        );
        assert_eq!((top, elastic.shown()), (952., 0.));
    }

    #[test]
    fn a_quick_flick_back_that_lifts_while_still_stretched_keeps_its_momentum() {
        let t0 = Instant::now();
        let mut elastic = Elastic::default();
        let mut top = 900.;

        // 1. Flick down into the bottom so the page bounces to a noticeable stretch.
        scroll(
            &mut elastic,
            &mut top,
            wheel(0., Phase::Began, Phase::None, 0.),
            t0,
        );
        scroll(
            &mut elastic,
            &mut top,
            wheel(10., Phase::Changed, Phase::None, 0.01),
            t0,
        );
        scroll(
            &mut elastic,
            &mut top,
            wheel(0., Phase::Ended, Phase::None, 0.02),
            t0,
        );
        let mut time = 0.03;
        scroll(
            &mut elastic,
            &mut top,
            wheel(30., Phase::None, Phase::Began, time),
            t0,
        );
        while top < 1000. {
            time += 0.01;
            scroll(
                &mut elastic,
                &mut top,
                wheel(30., Phase::None, Phase::Changed, time),
                t0,
            );
        }
        time += 0.01;
        scroll(
            &mut elastic,
            &mut top,
            wheel(30., Phase::None, Phase::Changed, time),
            t0,
        );
        time += 0.06;
        assert!(elastic.step(t0 + Duration::from_secs_f64(time)));
        let stretched = elastic.shown();
        assert!(stretched > 15., "expected >15 pt stretch, got {stretched}");

        // 2. Quick flick back up: fingers only move -10 pt before lifting, so the
        //    page is still stretched when Phase::Ended arrives.
        time += 0.01;
        scroll(
            &mut elastic,
            &mut top,
            wheel(0., Phase::Began, Phase::None, time),
            t0,
        );
        time += 0.01;
        scroll(
            &mut elastic,
            &mut top,
            wheel(-10., Phase::Changed, Phase::None, time),
            t0,
        );
        assert!(elastic.shown() > 0., "still stretched before lift");
        time += 0.01;
        scroll(
            &mut elastic,
            &mut top,
            wheel(0., Phase::Ended, Phase::None, time),
            t0,
        );
        // One display tick before momentum arrives keeps the stretch positive.
        time += 0.008;
        assert!(elastic.step(t0 + Duration::from_secs_f64(time)));
        assert!(elastic.shown() > 0.);

        // 3. Upward momentum arrives and pulls the page back cleanly.
        time += 0.008;
        scroll(
            &mut elastic,
            &mut top,
            wheel(-30., Phase::None, Phase::Began, time),
            t0,
        );
        for _ in 0..5 {
            time += 0.01;
            scroll(
                &mut elastic,
                &mut top,
                wheel(-30., Phase::None, Phase::Changed, time),
                t0,
            );
        }
        assert_eq!(elastic.shown(), 0.);
        assert!(
            top < 860.,
            "expected upward momentum to scroll page, got top={top}"
        );
        assert!(!elastic.step(t0 + Duration::from_secs_f64(time + 0.016)));
    }

    #[test]
    fn same_direction_momentum_after_a_pull_still_springs_back() {
        let t0 = Instant::now();
        let mut elastic = Elastic::default();
        let mut top = 990.;
        scroll(
            &mut elastic,
            &mut top,
            wheel(0., Phase::Began, Phase::None, 0.),
            t0,
        );
        scroll(
            &mut elastic,
            &mut top,
            wheel(10., Phase::Changed, Phase::None, 0.01),
            t0,
        );
        for at in 2..12 {
            scroll(
                &mut elastic,
                &mut top,
                wheel(20., Phase::Changed, Phase::None, f64::from(at) * 0.01),
                t0,
            );
        }
        assert_eq!(elastic.shown(), 10.);
        scroll(
            &mut elastic,
            &mut top,
            wheel(0., Phase::Ended, Phase::None, 0.12),
            t0,
        );
        // Same-direction downward momentum after lifting should not fight the spring.
        scroll(
            &mut elastic,
            &mut top,
            wheel(30., Phase::None, Phase::Began, 0.13),
            t0,
        );
        scroll(
            &mut elastic,
            &mut top,
            wheel(30., Phase::None, Phase::Changed, 0.14),
            t0,
        );
        let start = t0 + Duration::from_secs_f64(0.12);
        let mut now = t0 + Duration::from_secs_f64(0.15);
        while elastic.step(now) {
            now += Duration::from_millis(8);
            assert!(now - start < Duration::from_secs(1), "it settles");
        }
        assert_eq!((top, elastic.shown()), (1000., 0.));
    }

    #[test]
    fn a_cancelled_stretched_gesture_springs_back() {
        let t0 = Instant::now();
        let mut elastic = Elastic::default();
        let mut top = 990.;
        scroll(
            &mut elastic,
            &mut top,
            wheel(0., Phase::Began, Phase::None, 0.),
            t0,
        );
        scroll(
            &mut elastic,
            &mut top,
            wheel(10., Phase::Changed, Phase::None, 0.01),
            t0,
        );
        for at in 2..12 {
            scroll(
                &mut elastic,
                &mut top,
                wheel(20., Phase::Changed, Phase::None, f64::from(at) * 0.01),
                t0,
            );
        }
        assert_eq!(elastic.shown(), 10.);
        scroll(
            &mut elastic,
            &mut top,
            wheel(0., Phase::Cancelled, Phase::None, 0.12),
            t0,
        );
        let start = t0 + Duration::from_secs_f64(0.12);
        let mut now = start;
        while elastic.step(now) {
            assert!(
                elastic.shown() <= 10.,
                "cancellation should not deepen stretch: {}",
                elastic.shown()
            );
            now += Duration::from_millis(8);
            assert!(now - start < Duration::from_secs(1), "it settles");
        }
        assert_eq!((top, elastic.shown()), (1000., 0.));
    }

    #[test]
    fn inward_momentum_takes_over_when_spring_rounds_to_zero_in_grace_period() {
        let t0 = Instant::now();
        let mut elastic = Elastic::default();
        let mut top = 990.;

        // Pull past the bottom to +10, then reverse until +0.25 remains and lift.
        scroll(
            &mut elastic,
            &mut top,
            wheel(0., Phase::Began, Phase::None, 0.),
            t0,
        );
        scroll(
            &mut elastic,
            &mut top,
            wheel(10., Phase::Changed, Phase::None, 0.01),
            t0,
        );
        for at in 2..12 {
            scroll(
                &mut elastic,
                &mut top,
                wheel(20., Phase::Changed, Phase::None, f64::from(at) * 0.01),
                t0,
            );
        }
        assert_eq!(elastic.shown(), 10.);
        scroll(
            &mut elastic,
            &mut top,
            wheel(-9.75, Phase::Changed, Phase::None, 0.12),
            t0,
        );
        assert_eq!(elastic.shown(), 0.25);
        scroll(
            &mut elastic,
            &mut top,
            wheel(0., Phase::Ended, Phase::None, 0.13),
            t0,
        );

        // An 8 ms display frame rounds the displayed stretch to 0 while keeping
        // the spring alive during its 24 ms completion grace window.
        assert!(elastic.step(t0 + Duration::from_secs_f64(0.138)));
        assert_eq!(elastic.shown(), 0.);

        // Inward upward momentum must still take over and scroll back into the page.
        scroll(
            &mut elastic,
            &mut top,
            wheel(-30., Phase::None, Phase::Began, 0.14),
            t0,
        );
        scroll(
            &mut elastic,
            &mut top,
            wheel(-30., Phase::None, Phase::Changed, 0.15),
            t0,
        );
        assert_eq!((top, elastic.shown()), (940., 0.));
        assert!(!elastic.step(t0 + Duration::from_secs_f64(0.16)));
    }

    #[test]
    fn small_outward_pull_at_top_does_not_move_fractional_stretch_inward() {
        let mut elastic = Elastic::default();
        let mut top = 10.;
        elastic.scroll(&mut top, wheel(0., Phase::Began, Phase::None, 0.), 1000.);
        elastic.scroll(
            &mut top,
            wheel(-10., Phase::Changed, Phase::None, 0.01),
            1000.,
        );
        elastic.scroll(
            &mut top,
            wheel(-20., Phase::Changed, Phase::None, 0.02),
            1000.,
        );
        assert_eq!(elastic.shown(), -1.);
        // Reverse partially to -0.75, then push outward by -0.1: must not jump to 0.
        elastic.scroll(
            &mut top,
            wheel(0.25, Phase::Changed, Phase::None, 0.03),
            1000.,
        );
        assert_eq!(elastic.shown(), -0.75);
        elastic.scroll(
            &mut top,
            wheel(-0.1, Phase::Changed, Phase::None, 0.04),
            1000.,
        );
        assert_eq!(elastic.shown(), -0.75);
    }
}
