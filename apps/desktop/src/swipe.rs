//! A two-finger swipe to the right is Back, as in the system's own apps:
//! the Tauri shell's `swipe.rs`, whose rule this is.
//!
//! Each scroll event carries the phase of the fingers' gesture, and
//! momentum has no phase of its own, so where one swipe ends and the next
//! begins is known. A gesture goes back once at most, when the fingers have
//! gone far enough across and more across than down. It does so whatever
//! Trackpad › Swipe between pages is set to, so a two-finger swipe works
//! with it set to three. The page does not follow the fingers; it
//! goes back when the swipe is far enough.

/// How far across, in points, the fingers must go.
const SWIPE_POINTS: f64 = 140.0;

/// Where in its gesture a scroll event is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) enum Phase {
    /// The fingers have just touched.
    Began,
    /// The fingers are moving.
    Changed,
    /// Momentum after the fingers left, the gesture's end, or a mouse wheel.
    Other,
}

/// One gesture's sweep across, which goes back once at most.
#[derive(Debug, Default)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) struct Gesture {
    swept: f64,
    spent: bool,
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
impl Gesture {
    /// Takes a scroll event: `right` is how far the fingers moved to the
    /// right, `down` how far down. True when it is time to go back.
    pub(crate) fn scroll(&mut self, phase: Phase, right: f64, down: f64) -> bool {
        match phase {
            Phase::Began => *self = Self::default(),
            Phase::Changed => {}
            Phase::Other => return false,
        }
        if self.spent {
            return false;
        }
        if right.abs() <= down.abs() * 2.0 {
            self.swept = 0.0;
            return false;
        }
        self.swept += right;
        if self.swept > SWIPE_POINTS {
            self.spent = true;
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::{Gesture, Phase};

    fn swipe(gesture: &mut Gesture, steps: usize, right: f64) -> usize {
        let mut backs = usize::from(gesture.scroll(Phase::Began, right, 0.0));
        for _ in 1..steps {
            backs += usize::from(gesture.scroll(Phase::Changed, right, 0.0));
        }
        backs
    }

    #[test]
    fn one_swipe_goes_back_once_and_its_momentum_not_at_all() {
        let mut gesture = Gesture::default();
        assert_eq!(swipe(&mut gesture, 60, 20.0), 1);
        assert!(!(0..60).any(|_| gesture.scroll(Phase::Other, 20.0, 0.0)));
    }

    #[test]
    fn a_second_swipe_during_the_first_ones_momentum_goes_back_again() {
        let mut gesture = Gesture::default();
        assert_eq!(swipe(&mut gesture, 20, 20.0), 1);
        assert!(!gesture.scroll(Phase::Other, 15.0, 0.0));
        assert_eq!(swipe(&mut gesture, 20, 20.0), 1);
    }

    #[test]
    fn a_short_swipe_a_swipe_left_and_a_scroll_go_nowhere() {
        let mut gesture = Gesture::default();
        assert_eq!(swipe(&mut gesture, 5, 20.0), 0);
        assert_eq!(swipe(&mut gesture, 30, -20.0), 0);
        let mut scrolled = usize::from(gesture.scroll(Phase::Began, 10.0, 30.0));
        for _ in 0..30 {
            scrolled += usize::from(gesture.scroll(Phase::Changed, 10.0, 30.0));
        }
        assert_eq!(scrolled, 0);
    }
}
