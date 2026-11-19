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

//! The stretch at either end of a page, as the Tauri app's WebKit page has
//! it on a Mac: WebKit's `ScrollingEffectsController` (its Mac half) and
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
//!   the momentum's velocity, negated. The rest of the flick is let go.
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
    in_gesture: bool,
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
        let now = Instant::now();
        // A touch surface sends a gesture or momentum phase; a wheel, however
        // fine its steps, sends neither, and never stretches the page.
        let touch = wheel.precise && (wheel.phase != Phase::None || wheel.momentum != Phase::None);
        // `updateGestureInProgressState`.
        self.in_gesture = touch
            && wheel.phase != Phase::Ended
            && !(wheel.phase == Phase::None && wheel.momentum == Phase::Began);
        if matches!(wheel.phase, Phase::MayBegin | Phase::Cancelled) {
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
        // Latching: a gesture and its momentum go to the page only if, when
        // it first moves, the page can move that way, or is stretched.
        let gesture = wheel.phase != Phase::None || wheel.is_momentum();
        if gesture {
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
        if self.ignore_momentum && (wheel.is_momentum() || self.spring.is_some()) {
            if wheel.momentum == Phase::Ended {
                self.ignore_momentum = false;
            }
            return;
        }
        if self.spring.is_some() && wheel.is_momentum() {
            // The flick after a pull let go, which would fight the spring.
            return;
        }
        let stretched = self.stretch != 0.;
        let event_delta = if stretched {
            wheel.unaccelerated_dy
        } else {
            wheel.dy
        };
        let mut delta = event_delta;
        if !self.momentum && matches!(wheel.momentum, Phase::Began | Phase::Changed) {
            self.momentum = true;
        }
        let mut should_stretch = false;
        if self.in_gesture || self.momentum {
            match self.last {
                Some((at, _)) if wheel.time - at > 0. && wheel.time - at < VELOCITY_ZEROING => {
                    self.velocity = event_delta / (wheel.time - at) as f32;
                }
                _ => self.velocity = 0.,
            }
            self.last = Some((wheel.time, now));
            // `modifyScrollDeltaForStretching`: towards an end the page is at.
            should_stretch = !stretched && pinned(*top, max, delta);
        }
        if delta != 0. {
            if should_stretch || stretched {
                let can_start = self.stretch_by(top, &mut delta, stretched, max);
                if self.momentum && can_start && self.last.is_some() {
                    self.ignore_momentum = true;
                    self.momentum = false;
                    self.start_spring_if_needed(now);
                }
            } else {
                *top = (*top + delta).clamp(0., max);
            }
        }
        if self.momentum && wheel.momentum == Phase::Ended {
            self.momentum = false;
            self.ignore_momentum = false;
            self.last = None;
        }
    }

    /// `applyScrollDeltaWithStretching`. Whether a spring may start: the
    /// page is at the end the event goes towards.
    fn stretch_by(&mut self, top: &mut f32, delta: &mut f32, stretched: bool, max: f32) -> bool {
        let opposes = stretched && self.stretch.signum() != delta.signum();
        let can_start = self.momentum && pinned(*top, max, *delta);
        if opposes {
            // Going back takes the stretch in point for point; the force stays.
            let to = self.stretch + *delta;
            if to == 0. || to.signum() == self.stretch.signum() {
                self.stretch = to;
            } else {
                // Past the end and back into the page.
                self.stretch = 0.;
                *top = (*top + to).clamp(0., max);
            }
        } else {
            self.force += *delta;
            self.stretch = (self.force / STIFFNESS).ceil();
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
        self.spring = Some(Spring {
            start: now,
            overscroll: self.stretch,
            // WebKit passes the momentum's velocity negated.
            velocity: -self.velocity,
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
        let mut elastic = Elastic::default();
        let mut top = 10.;
        elastic.scroll(&mut top, wheel(0., Phase::Began, Phase::None, 0.), 1000.);
        for at in 1..40 {
            let event = wheel(-20., Phase::Changed, Phase::None, f64::from(at) * 0.01);
            elastic.scroll(&mut top, event, 1000.);
        }
        let pulled = elastic.shown();
        assert!(pulled < -30., "{pulled}");
        elastic.scroll(&mut top, wheel(0., Phase::Ended, Phase::None, 0.4), 1000.);
        let start = Instant::now();
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
        let mut elastic = Elastic::default();
        let mut top = 900.;
        elastic.scroll(&mut top, wheel(0., Phase::Began, Phase::None, 0.), 1000.);
        elastic.scroll(
            &mut top,
            wheel(10., Phase::Changed, Phase::None, 0.01),
            1000.,
        );
        elastic.scroll(&mut top, wheel(0., Phase::Ended, Phase::None, 0.02), 1000.);
        let mut time = 0.03;
        elastic.scroll(&mut top, wheel(30., Phase::None, Phase::Began, time), 1000.);
        while top < 1000. {
            time += 0.01;
            elastic.scroll(
                &mut top,
                wheel(30., Phase::None, Phase::Changed, time),
                1000.,
            );
        }
        time += 0.01;
        elastic.scroll(
            &mut top,
            wheel(30., Phase::None, Phase::Changed, time),
            1000.,
        );
        // 3,000 points a second: a bounce of about 27 points at 80 ms.
        let start = Instant::now();
        let mut most: f32 = 0.;
        let mut now = start;
        while elastic.step(now) {
            most = most.max(elastic.shown());
            now += Duration::from_millis(4);
        }
        assert!((24. ..=30.).contains(&most), "{most}");
        // The rest of the flick is let go.
        elastic.scroll(
            &mut top,
            wheel(30., Phase::None, Phase::Changed, time + 0.01),
            1000.,
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
}
