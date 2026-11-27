//! Small marks: a status's dot, ring or turning arc, and, wherever a song
//! appears, its five stars and the three bars of the song playing.

use std::time::{Duration, Instant};

use gpui::{
    Bounds, ContentMask, Context, Div, InteractiveElement, IntoElement, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, canvas, div, point, prelude::FluentBuilder,
    px, size,
};

use crate::theme::{self, Status};

/// A status's mark ("Status" in docs/internals/desktop.md). What is being
/// done is an open arc that turns, whether or not it is here; anything else
/// is a dot in its colour if it is here, or a ring if it is not, as a hub
/// that does not answer is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Mark {
    pub status: Status,
    pub here: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    Dot,
    Ring,
    Arc,
}

/// How long the working arc takes to turn once.
const TURN: Duration = Duration::from_millis(1000);

impl Mark {
    pub(crate) fn here(status: Status) -> Self {
        Self { status, here: true }
    }

    pub(crate) fn away(status: Status) -> Self {
        Self {
            status,
            here: false,
        }
    }

    fn shape(self) -> Shape {
        match (self.status, self.here) {
            (Status::Working, _) => Shape::Arc,
            (_, true) => Shape::Dot,
            (_, false) => Shape::Ring,
        }
    }

    /// The mark, `side` points square. The arc is turned by the renderer,
    /// so nothing is laid out again while it turns, and holds still where
    /// motion is reduced.
    pub(crate) fn draw(self, side: f32) -> gpui::AnyElement {
        let color = self.status.mark();
        let round = div().size(px(side)).flex_none().rounded_full();
        match self.shape() {
            Shape::Dot => round.bg(color).into_any_element(),
            Shape::Ring => round.border(px(1.5)).border_color(color).into_any_element(),
            Shape::Arc => gpui::svg()
                .path("icons/marks/working.svg")
                .size(px(side))
                .flex_none()
                .text_color(color)
                .when(!crate::platform::reduce_motion(), |arc| {
                    arc.with_timed_transformation(
                        gpui::TimedTransformation::rotation(TURN, gpui::Transformation::default())
                            .with_max_fps(30.),
                    )
                })
                .into_any_element(),
        }
    }
}

/// Five stars: in the accent when set, and while the pointer is on them,
/// filled up to the one it is on. `hovered` is the star the pointer is on,
/// which the view holding them keeps since a star's colour depends on its
/// neighbour's hover. A click sets that many; whether it takes them away is
/// the caller's to decide. The pointer coming onto a star and leaving it are
/// told apart, since leaving one star for the next may be heard after coming
/// onto it.
#[allow(clippy::too_many_arguments)]
pub(crate) fn stars<V: 'static>(
    key: SharedString,
    shown: u32,
    hovered: Option<u32>,
    enabled: bool,
    size: f32,
    cx: &mut Context<V>,
    on_hover: impl Fn(&mut V, u32, bool, &mut Context<V>) + Clone + 'static,
    on_click: impl Fn(&mut V, u32, &mut Context<V>) + Clone + 'static,
) -> Div {
    let off = gpui::Rgba {
        a: 0.2,
        ..theme::ink()
    };
    let mut row = div()
        .flex()
        .flex_none()
        .items_center()
        .text_size(px(size))
        .line_height(px(size));
    for star in 1..=5u32 {
        let color = match hovered.filter(|_| enabled) {
            Some(at) if star <= at => theme::ink(),
            Some(_) => off,
            None if star <= shown => theme::acc(),
            None => off,
        };
        let mut cell = div()
            .id(SharedString::from(format!("{key}-star-{star}")))
            // The real stars have a point of letter-spacing after each:
            // GPUI's advance for the glyph is already that point wider.
            .text_color(color)
            .child("★");
        if enabled {
            let hover = on_hover.clone();
            let click = on_click.clone();
            cell = cell
                .cursor_pointer()
                .on_hover(cx.listener(move |view, over: &bool, _, cx| {
                    hover(view, star, *over, cx);
                    cx.notify();
                }))
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(move |view, _, _, cx| {
                        // Not a double-click on the row it is in.
                        cx.stop_propagation();
                        click(view, star, cx);
                    }),
                );
        }
        row = row.child(cell);
    }
    row
}

/// Sampled just before GPUI constructs the application, as an approximation
/// of the private epoch it uses to advance timed sprite transformations.
pub(crate) struct SceneClock(pub Instant);
impl gpui::Global for SceneClock {}

const EQ_PERIOD: Duration = Duration::from_millis(1800);
// The 16-unit artwork occupies a 13-point square in a row.
const EQ_SIDE: f32 = 13.;
const UNIT: f32 = EQ_SIDE / 16.;
// Each half of the Bézier wave runs for 100 SVG units. Two full cycles
// plus four units cover a moving bar at every starting phase.
const CYCLE_WIDTH: f32 = 200.;
const STRIP_WIDTH: f32 = 2. * CYCLE_WIDTH + 4.;
const WAVE: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" width="404" height="10">
    <path d="M0 6.5 c42 0 58 -6.5 100 -6.5 s58 6.5 100 6.5
              s58 -6.5 100 -6.5 s58 6.5 100 6.5 h4 V10 H0 Z"/>
</svg>"#;

/// Three staggered bars that follow playback and hold their height on pause.
pub(crate) struct Equalizer {
    /// At rest, the held phase. In motion, the offset from GPUI's clock.
    phase: f32,
    moving: bool,
}

impl Equalizer {
    pub(crate) fn playback(played: Duration, since: Option<Instant>, cx: &gpui::App) -> Self {
        if cx.reduce_motion() {
            return Self {
                phase: 0.,
                moving: false,
            };
        }
        let start = since
            .zip(cx.try_global::<SceneClock>())
            .map_or(Duration::ZERO, |(start, clock)| {
                start.saturating_duration_since(clock.0)
            });
        let phase = (played.as_secs_f64() - start.as_secs_f64()) / EQ_PERIOD.as_secs_f64();
        Self {
            phase: phase.rem_euclid(1.) as f32,
            moving: since.is_some(),
        }
    }

    pub(crate) fn for_picker(opened: Option<Instant>, cx: &gpui::App) -> Self {
        Self::playback(Duration::ZERO, opened, cx)
    }

    pub(crate) fn draw(self, color: gpui::Rgba) -> Div {
        let cycle = px(CYCLE_WIDTH * UNIT);
        let pan = gpui::TimedTransformation::new(EQ_PERIOD)
            .to(gpui::Transformation::translate(point(-cycle, px(0.))))
            .with_max_fps(30.);
        let strip = size(px(STRIP_WIDTH * UNIT), px(10. * UNIT));
        let mut bars = div().relative().size(px(EQ_SIDE)).flex_none();
        // Each bar reveals a slice of the same ease-in-out curve,
        // 300 ms ahead of its neighbour.
        for (x, stagger) in [(3., 0.), (6.9, 1. / 6.), (10.8, 2. / 6.)] {
            let phase = (self.phase + stagger).fract();
            let wave = gpui::svg()
                .data(WAVE)
                .w(strip.width)
                .h(strip.height)
                .text_color(color)
                .when(self.moving, |wave| wave.with_timed_transformation(pan));
            let bar = canvas(
                move |bounds, window, cx| {
                    // Layout rounds the bar's width and position independently.
                    // A scroll offset can put it between pixels again. Round its
                    // absolute corners as a quad would before GPUI expands the clip.
                    let clip = ContentMask {
                        bounds: Bounds::from_corners(
                            window.pixel_snap_point(bounds.origin),
                            window.pixel_snap_point(bounds.bottom_right()),
                        ),
                    };
                    let origin = clip.bounds.origin - point(cycle * phase, px(0.));
                    let mut wave = wave.into_any_element();
                    window.with_content_mask(Some(clip), |window| {
                        wave.prepaint_as_root(origin, strip.into(), window, cx);
                    });
                    (clip, wave)
                },
                |_, (clip, mut wave), window, cx| {
                    window.with_content_mask(Some(clip), |window| wave.paint(window, cx));
                },
            );
            bars = bars.child(
                bar.absolute()
                    .left(px(x * UNIT))
                    .top(px(3. * UNIT))
                    .w(px(2.2 * UNIT))
                    .h(px(10. * UNIT)),
            );
        }
        bars
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn working_is_an_arc_and_anything_else_a_dot_here_or_a_ring_away() {
        assert_eq!(Mark::here(Status::Working).shape(), Shape::Arc);
        assert_eq!(Mark::away(Status::Working).shape(), Shape::Arc);
        for status in [Status::Quiet, Status::Ok, Status::Need, Status::Fail] {
            assert_eq!(Mark::here(status).shape(), Shape::Dot);
            assert_eq!(Mark::away(status).shape(), Shape::Ring);
        }
    }

    #[test]
    fn the_working_arc_s_picture_is_there() -> gpui::Result<()> {
        use gpui::AssetSource;
        assert!(
            crate::icons::Icons
                .load("icons/marks/working.svg")?
                .is_some()
        );
        Ok(())
    }

    #[gpui::test]
    fn bar_clips_keep_the_original_widths_and_gaps_at_both_scales(cx: &mut gpui::TestAppContext) {
        struct Marks;
        impl gpui::Render for Marks {
            fn render(&mut self, _: &mut gpui::Window, _: &mut Context<Self>) -> impl IntoElement {
                canvas(
                    |bounds, window, cx| {
                        let mut mark = Equalizer::for_picker(Some(Instant::now()), cx)
                            .draw(theme::ink())
                            .into_any_element();
                        // A subpixel scroll after layout: outward rounding would
                        // make each clip a device pixel wider.
                        let origin = bounds.origin + point(px(0.375), px(0.375));
                        mark.prepaint_as_root(
                            origin,
                            size(px(EQ_SIDE), px(EQ_SIDE)).into(),
                            window,
                            cx,
                        );
                        mark
                    },
                    |_, mut mark, window, cx| mark.paint(window, cx),
                )
                .size(px(EQ_SIDE))
            }
        }
        let (_, cx) = cx.add_window_view(|_, _| Marks);
        let mut edges = |scale| {
            cx.simulate_scale_factor_change(scale);
            cx.update(|window, cx| {
                window.draw(cx).clear(cx);
                let mut edges: Vec<_> = window
                    .rendered_scene()
                    .monochrome_sprites
                    .iter()
                    .map(|sprite| {
                        assert_ne!(sprite.animation, gpui::SpriteAnimationId::NONE);
                        let b = sprite.content_mask.bounds;
                        (
                            b.left().0 as i32,
                            b.right().0 as i32,
                            b.top().0 as i32,
                            b.bottom().0 as i32,
                        )
                    })
                    .collect();
                edges.sort_unstable();
                edges
            })
        };
        // (left, right, top, bottom) in device pixels.
        assert_eq!(edges(1.), [(2, 4, 2, 10), (6, 8, 2, 10), (9, 11, 2, 10)]);
        assert_eq!(
            edges(2.),
            [(6, 10, 6, 22), (12, 16, 6, 22), (19, 23, 6, 22)]
        );
    }

    #[gpui::test]
    fn resuming_starts_at_the_held_phase_on_the_shared_clock(cx: &mut gpui::TestAppContext) {
        let origin = Instant::now();
        cx.update(|cx| {
            cx.set_global(SceneClock(origin));
            let played = Duration::from_millis(450);
            let held = Equalizer::playback(played, None, cx);
            assert!(!held.moving);
            assert_eq!(held.phase, 0.25);

            let paused_until = Duration::from_millis(3150);
            let resumed = Equalizer::playback(played, Some(origin + paused_until), cx);
            let scene_phase = (paused_until.as_secs_f64() / EQ_PERIOD.as_secs_f64()) as f32;
            assert!(resumed.moving);
            assert_eq!((scene_phase + resumed.phase).fract(), held.phase);
        });
    }
}
