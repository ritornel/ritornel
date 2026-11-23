//! How long a tile waits for its picture: from the first frame it is shown
//! to the first that draws its cover or portrait at the size it is shown
//! at, so that a smaller one standing in, blurred, does not count. A
//! developer's measure, kept only with `RITORNEL_GPUI_WAITS=1`, and
//! reported by the script's `waits <label>` step, which starts the count
//! again (`scripts/desktop-memory.py --waits` runs it).
//!
//! Only the page on screen counts, once it can be seen: the shell says
//! which ([`on_screen`]), so a page fading out, or one not yet faded in,
//! is not timed. Each surface, the wall or an artist's page, counts its
//! frames. A tile drawn in a frame that did not follow the last one it was
//! drawn in has just been shown, as one scrolled in is, and when another
//! page comes on screen, or this one comes back, every tile is shown anew.
//! A tile that leaves before its picture comes is counted apart, as never
//! painted, and one still waiting when the count is reported is reported
//! with how long it has waited so far.

#![allow(
    clippy::print_stderr,
    reason = "a developer's tool: it reports on standard error"
)]

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::Instant;

use ritornel_contract::Id;

/// Whether waits are kept, read once.
fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RITORNEL_GPUI_WAITS").is_some_and(|value| value == "1"))
}

/// A tile: its picture, and the size it is shown at in pixels.
type Key = (Id, u32);

/// One tile, on one surface.
struct Tile {
    /// The surface's frame it was last drawn in.
    frame: u64,
    /// When it was shown without its picture, while it still waits.
    since: Option<Instant>,
}

#[derive(Default)]
struct Surface {
    frame: u64,
    tiles: HashMap<Key, Tile>,
    /// Milliseconds each tile shown waited; 0 for one shown with its picture.
    waited: Vec<f32>,
    /// Tiles that left before their picture came.
    unpainted: usize,
}

impl Surface {
    /// A frame begins.
    fn frame(&mut self) {
        self.frame += 1;
        self.forget_gone();
    }

    /// Forgets the tiles not drawn in the frame before: they have left.
    fn forget_gone(&mut self) {
        let frame = self.frame;
        let mut left = 0;
        self.tiles.retain(|_, tile| {
            let stays = tile.frame + 1 >= frame;
            if !stays && tile.since.is_some() {
                left += 1;
            }
            stays
        });
        self.unpainted += left;
    }

    /// Every tile drawn after is shown anew.
    fn moved(&mut self) {
        self.unpainted += self
            .tiles
            .values()
            .filter(|tile| tile.since.is_some())
            .count();
        self.tiles.clear();
    }

    fn drawn(&mut self, key: Key, painted: bool, now: Instant) {
        let frame = self.frame;
        match self.tiles.get_mut(&key) {
            Some(tile) => {
                tile.frame = frame;
                if painted && let Some(since) = tile.since.take() {
                    self.waited
                        .push(now.saturating_duration_since(since).as_secs_f32() * 1000.);
                }
            }
            None => {
                if painted {
                    self.waited.push(0.);
                }
                let since = (!painted).then_some(now);
                self.tiles.insert(key, Tile { frame, since });
            }
        }
    }

    /// What it has counted, and how long each tile on screen still waiting
    /// has waited so far; it starts again, with every tile shown anew.
    fn take(&mut self, now: Instant) -> (Vec<f32>, usize, Vec<f32>) {
        // A tile that left on the last frame drawn has left.
        self.frame += 1;
        self.forget_gone();
        let waiting = self
            .tiles
            .values()
            .filter_map(|tile| tile.since)
            .map(|since| now.saturating_duration_since(since).as_secs_f32() * 1000.)
            .collect();
        self.tiles.clear();
        let waited = std::mem::take(&mut self.waited);
        (waited, std::mem::take(&mut self.unpainted), waiting)
    }
}

#[derive(Default)]
struct Waits {
    /// The surface on screen and seen, if it is one that is timed.
    on_screen: Option<&'static str>,
    surfaces: HashMap<&'static str, Surface>,
}

thread_local! {
    static WAITS: RefCell<Waits> = RefCell::new(Waits::default());
}

/// Which surface is on screen and can be seen now, if one that is timed:
/// the shell says so each frame, before the page draws. Another page, or
/// none, makes every tile shown after count as shown anew.
pub(crate) fn on_screen(surface: Option<&'static str>) {
    if on() {
        WAITS.with_borrow_mut(|waits| {
            if waits.on_screen != surface {
                waits.on_screen = surface;
                waits.surfaces.values_mut().for_each(Surface::moved);
            }
        });
    }
}

/// A frame of `surface` begins.
pub(crate) fn frame(surface: &'static str) {
    if on() {
        WAITS.with_borrow_mut(|waits| {
            if waits.on_screen == Some(surface) {
                waits.surfaces.entry(surface).or_default().frame();
            }
        });
    }
}

/// The tile of `art` at `px` pixels is drawn on `surface` in this frame,
/// with its picture at that size or not.
pub(crate) fn drawn(surface: &'static str, art: Id, px: u32, painted: bool) {
    if on() {
        let now = Instant::now();
        WAITS.with_borrow_mut(|waits| {
            if waits.on_screen == Some(surface) {
                let surface = waits.surfaces.entry(surface).or_default();
                surface.drawn((art, px), painted, now);
            }
        });
    }
}

/// Prints what each surface's tiles waited since the last report, as one
/// line of JSON for each, and starts again; with the label `-`, only starts
/// again.
pub(crate) fn report(label: &str) {
    if !on() {
        return;
    }
    let now = Instant::now();
    WAITS.with_borrow_mut(|waits| {
        for (name, surface) in &mut waits.surfaces {
            let (mut waited, unpainted, waiting) = surface.take(now);
            if label == "-" {
                continue;
            }
            waited.sort_by(f32::total_cmp);
            let at = |share: f32| {
                waited
                    .get(((waited.len().max(1) - 1) as f32 * share).round() as usize)
                    .copied()
                    .unwrap_or(0.)
            };
            let line = serde_json::json!({
                "label": label,
                "surface": name,
                "shown": waited.len(),
                "atOnce": waited.iter().filter(|ms| **ms == 0.).count(),
                "medianMs": at(0.5),
                "p90Ms": at(0.9),
                "slowestMs": at(1.),
                "unpainted": unpainted,
                "waitedMs": waited,
                "stillWaitingMs": waiting,
            });
            eprintln!("waits {line}");
        }
    });
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn a_tile_waits_from_the_frame_it_is_shown_to_the_one_with_its_picture() {
        let [a, b, c, d] = [1, 2, 3, 4].map(|n| (Id::from_bytes([n; 16]), 300));
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let mut wall = Surface::default();

        // Shown: `a` with its picture, the rest without.
        wall.frame();
        wall.drawn(a, true, at(0));
        for tile in [b, c, d] {
            wall.drawn(tile, false, at(0));
        }
        // `b`'s picture comes 40 ms later; `c` scrolls away without one;
        // `d` still waits.
        wall.frame();
        wall.drawn(a, true, at(40));
        wall.drawn(b, true, at(40));
        wall.drawn(d, false, at(40));
        let (waited, unpainted, waiting) = wall.take(at(100));
        assert_eq!(waited, vec![0., 40.]);
        assert_eq!(unpainted, 1);
        assert_eq!(waiting, vec![100.]);

        // Shown again, as on coming back: anew, and painted at once.
        wall.frame();
        wall.drawn(b, true, at(900));
        assert_eq!(wall.take(at(900)).0, vec![0.]);
    }
}
