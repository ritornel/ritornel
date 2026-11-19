//! How things move between pages, as the real client settled it
//! (`lib/motion.ts`, `lib/stage.svelte.ts`): one page leaves before the next
//! arrives with a short hop; a cover that was clicked travels to its place
//! on the page it opens, in a fifth of a second and without changing shape;
//! between an artist and an album the banner holds still and only what it
//! says changes; arriving at an artist, the banner makes a small entrance
//! of its own; back among the walls, the artist you were with settles into
//! place. With the system's Reduce motion on, nothing moves: pages change.
//!
//! The page that arrives waits, unseen, until it is ready (what it shows
//! read, and its picture), for a quarter of a second at most; only then does
//! it arrive and a travelling cover land on it, aimed at where its target
//! will rest.

use std::time::{Duration, Instant};

use gpui::{Bounds, Pixels};

use crate::shell::bezier;

/// The real client's durations, in milliseconds (`MOTION`).
pub(crate) const LEAVE_MS: f32 = 100.;
pub(crate) const ARRIVE_MS: f32 = 300.;
pub(crate) const ARRIVE_DELAY_MS: f32 = 70.;
pub(crate) const FLY_MS: f32 = 200.;
pub(crate) const BANNER_MS: f32 = 200.;
pub(crate) const COLOUR_MS: f32 = 500.;
pub(crate) const PICTURE_MS: f32 = 900.;
pub(crate) const PICTURE_DELAY_MS: f32 = 60.;
pub(crate) const EBB_MS: f32 = 520.;
pub(crate) const TINT_MS: f32 = 900.;
/// A cover with nowhere to land fades where it stands.
pub(crate) const DROP_MS: f32 = 120.;
/// How long a page may take to be ready before it is shown as it is.
pub(crate) const READY_WITHIN: Duration = Duration::from_millis(250);
/// The hop a page arrives with.
pub(crate) const HOP: f32 = 8.;

/// How far `ms` milliseconds into a motion of `duration`, from 0 to 1.
pub(crate) fn progress(ms: f32, duration: f32) -> f32 {
    (ms / duration).clamp(0., 1.)
}

/// Milliseconds since `at`.
pub(crate) fn since(at: Instant, now: Instant) -> f32 {
    now.saturating_duration_since(at).as_secs_f32() * 1000.
}

/// CSS's named curves.
pub(crate) fn ease_out(t: f32) -> f32 {
    bezier(0., 0., 0.58, 1., t)
}

pub(crate) fn ease_in(t: f32) -> f32 {
    bezier(0.42, 0., 1., 1., t)
}

/// CSS's `ease`.
pub(crate) fn ease(t: f32) -> f32 {
    bezier(0.25, 0.1, 0.25, 1., t)
}

/// The prototype's arrival and picture curve.
pub(crate) fn arrive_curve(t: f32) -> f32 {
    bezier(0.2, 0.7, 0.2, 1., t)
}

/// The prototype's flight curve.
pub(crate) fn fly_curve(t: f32) -> f32 {
    bezier(0.3, 0., 0.2, 1., t)
}

/// How a page with a banner shows itself while it moves: the parts the
/// stage moves apart. The page applies it as it draws.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Look {
    /// The banner's opacity: it fades in alone when it holds still.
    pub banner: f32,
    /// The body's opacity, and its hop down from where it rests.
    pub body: f32,
    pub hop: f32,
    /// The banner's picture: its opacity and its scale, for an artist's entrance.
    pub picture: f32,
    pub scale: f32,
}

impl Default for Look {
    fn default() -> Self {
        Self {
            banner: 1.,
            body: 1.,
            hop: 0.,
            picture: 1.,
            scale: 1.,
        }
    }
}

/// How the page that arrives comes in.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Arrival {
    pub started: Instant,
    /// When it was ready and began to arrive.
    pub ready: Option<Instant>,
    /// The banner holds still: from an artist to an album, or back.
    pub hold: bool,
    /// The banner makes an entrance of its own: an artist's page, arrived
    /// at from anywhere but held.
    pub entrance: bool,
    /// With Reduce motion, the page simply shows.
    pub still: bool,
}

impl Arrival {
    pub(crate) fn new(hold: bool, entrance: bool, still: bool) -> Self {
        Self {
            started: Instant::now(),
            ready: None,
            hold,
            entrance,
            still,
        }
    }

    /// The page as it shows now: the whole of it (its opacity and hop),
    /// and the look of its parts.
    pub(crate) fn at(&self, now: Instant) -> (f32, f32, Look) {
        let Some(ready) = self.ready else {
            // Waiting, unseen, to be ready.
            return (0., 0., Look::default());
        };
        if self.still {
            return (1., 0., Look::default());
        }
        let ms = since(ready, now);
        let hopped = arrive_curve(progress(ms - ARRIVE_DELAY_MS, ARRIVE_MS));
        let mut look = Look::default();
        let whole = if self.hold {
            // The banner fades in alone over the one held; the body hops.
            look.banner = ease_out(progress(ms, BANNER_MS));
            look.body = hopped;
            look.hop = HOP * (1. - hopped);
            (1., 0.)
        } else {
            (hopped, HOP * (1. - hopped))
        };
        if self.entrance {
            look.picture = ease_out(progress(ms, COLOUR_MS));
            let eased = arrive_curve(progress(ms - PICTURE_DELAY_MS, PICTURE_MS));
            look.scale = 1.05 - 0.05 * eased;
        }
        (whole.0, whole.1, look)
    }

    /// Whether it is still moving.
    pub(crate) fn moving(&self, now: Instant) -> bool {
        match self.ready {
            None => true,
            Some(_) if self.still => false,
            Some(ready) => {
                let ms = since(ready, now);
                let longest = if self.entrance {
                    PICTURE_DELAY_MS + PICTURE_MS
                } else {
                    ARRIVE_DELAY_MS + ARRIVE_MS
                };
                ms < longest
            }
        }
    }

    /// When nothing that carries a cover moves any more: the hop is done.
    pub(crate) fn settled_at(&self) -> Option<Instant> {
        let ready = self.ready?;
        Some(ready + Duration::from_millis((ARRIVE_DELAY_MS + ARRIVE_MS) as u64))
    }
}

/// The page left, fading out under the one that arrives: all of it, or
/// all but its banner when the banner holds still.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Leaving {
    pub started: Instant,
    pub hold: bool,
}

impl Leaving {
    /// Its opacity now; `None` once it is gone. With the banner held, the
    /// old banner stays under the new one while that fades in over it.
    pub(crate) fn at(&self, now: Instant) -> Option<f32> {
        let ms = since(self.started, now);
        let gone = if self.hold {
            ARRIVE_DELAY_MS + BANNER_MS + 20.
        } else {
            LEAVE_MS + 20.
        };
        (ms < gone).then(|| 1. - ease_in(progress(ms, LEAVE_MS)))
    }
}

/// Where a cover flies, worked out when the page is ready.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Phase {
    /// Waiting where it was clicked for the page to be ready.
    Waiting,
    /// Flying from `from` to `to`, since `start`.
    Flying { start: Instant, to: Bounds<Pixels> },
    /// Holding where it lands until the page has stopped moving.
    Holding { to: Bounds<Pixels> },
    /// Fading where it stands: there is nowhere to land.
    Dropping { start: Instant },
}

/// Where a flying cover is and how it shows at `now`, from `from` to `to`:
/// its bounds, and its opacity.
pub(crate) fn flight_at(
    phase: Phase,
    from: Bounds<Pixels>,
    now: Instant,
) -> (Bounds<Pixels>, f32, bool) {
    match phase {
        Phase::Waiting => (from, 1., false),
        Phase::Flying { start, to } => {
            let t = progress(since(start, now), FLY_MS);
            let p = fly_curve(t);
            let lerp = |a: Pixels, b: Pixels| a + (b - a) * p;
            let bounds = Bounds::new(
                gpui::point(
                    lerp(from.origin.x, to.origin.x),
                    lerp(from.origin.y, to.origin.y),
                ),
                gpui::size(
                    lerp(from.size.width, to.size.width),
                    lerp(from.size.height, to.size.height),
                ),
            );
            (bounds, 1., t >= 1.)
        }
        Phase::Holding { to } => (to, 1., false),
        Phase::Dropping { start } => {
            let t = progress(since(start, now), DROP_MS);
            (from, 1. - ease_out(t), t >= 1.)
        }
    }
}

/// Whether a target can be seen in the page's area: a cover lands only
/// where it can be seen.
pub(crate) fn seen(target: Bounds<Pixels>, top: f32, bottom: f32) -> bool {
    let y = f32::from(target.origin.y);
    let height = f32::from(target.size.height);
    y + height > top && y < bottom && height > 0.
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_waits_unseen_then_hops_into_place() {
        let mut arrival = Arrival::new(false, false, false);
        let start = arrival.started;
        assert_eq!(arrival.at(start).0, 0.);
        arrival.ready = Some(start);
        // Still before the delay, in place after it and the hop.
        let (opacity, hop, _) = arrival.at(start + Duration::from_millis(60));
        assert!(
            opacity.abs() < 1e-3 && (hop - HOP).abs() < 1e-3,
            "{opacity} {hop}"
        );
        let (opacity, hop, _) = arrival.at(start + Duration::from_millis(400));
        assert!(
            (opacity - 1.).abs() < 1e-3 && hop.abs() < 1e-2,
            "{opacity} {hop}"
        );
    }

    #[test]
    fn held_the_banner_fades_in_alone_and_the_body_hops() {
        let mut arrival = Arrival::new(true, false, false);
        let start = arrival.started;
        arrival.ready = Some(start);
        let (opacity, hop, look) = arrival.at(start + Duration::from_millis(100));
        assert_eq!((opacity, hop), (1., 0.));
        assert!(look.banner > 0.3 && look.banner < 1.);
        assert!(look.hop > 0.);
    }

    #[test]
    fn an_artists_picture_comes_up_and_eases_into_place() {
        let mut arrival = Arrival::new(false, true, false);
        let start = arrival.started;
        arrival.ready = Some(start);
        let (_, _, look) = arrival.at(start + Duration::from_millis(50));
        assert!(look.picture < 0.5 && (look.scale - 1.05).abs() < 1e-3);
        let (_, _, look) = arrival.at(start + Duration::from_millis(1000));
        assert!((look.picture - 1.).abs() < 1e-3 && (look.scale - 1.).abs() < 1e-3);
    }

    #[test]
    fn with_reduce_motion_a_page_simply_shows() {
        let mut arrival = Arrival::new(true, true, true);
        arrival.ready = Some(arrival.started);
        assert_eq!(arrival.at(arrival.started).0, 1.);
        assert!(!arrival.moving(arrival.started));
    }

    #[test]
    fn the_page_left_fades_and_goes_later_when_the_banner_holds() {
        let start = Instant::now();
        let plain = Leaving {
            started: start,
            hold: false,
        };
        let held = Leaving {
            started: start,
            hold: true,
        };
        let at = |ms| start + Duration::from_millis(ms);
        assert!(plain.at(at(130)).is_none());
        assert!(held.at(at(130)).is_some());
        assert!(held.at(at(300)).is_none());
    }
}
