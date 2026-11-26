//! Small marks drawn wherever a song appears: its five stars and the three
//! bars of the song playing.

use std::time::{Duration, Instant};

use gpui::{
    Context, Div, InteractiveElement, ParentElement, SharedString, StatefulInteractiveElement,
    Styled, div, prelude::FluentBuilder, px,
};

use crate::theme;

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

/// GPUI starts its shared sprite clock when it creates the application.
/// Sample that origin so a sprite can follow elapsed playback across pauses.
pub(crate) struct SceneClock(pub Instant);
impl gpui::Global for SceneClock {}

const EQ_PERIOD: Duration = Duration::from_millis(1800);

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
        let start = since.map_or(Duration::ZERO, |start| {
            start.duration_since(cx.global::<SceneClock>().0)
        });
        let phase = (played.as_secs_f64() - start.as_secs_f64()) / EQ_PERIOD.as_secs_f64();
        Self {
            phase: phase.rem_euclid(1.) as f32,
            moving: since.is_some(),
        }
    }

    pub(crate) fn draw(self, color: gpui::Rgba) -> Div {
        let side = 13.;
        // The upper edge of the strip traces our ease-in-out height curve.
        // Each bar clips a narrow slice of it, 300 ms ahead of its neighbour.
        // GPUI can translate this mask in the retained scene between presents.
        const WAVE: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" width="404" height="10">
            <path d="M0 6.5 c42 0 58 -6.5 100 -6.5 s58 6.5 100 6.5
                      s58 -6.5 100 -6.5 s58 6.5 100 6.5 h4 V10 H0 Z"/>
        </svg>"#;
        let unit = side / 16.;
        let cycle = 200. * unit;
        let pan = gpui::TimedTransformation::new(EQ_PERIOD).to(gpui::Transformation::translate(
            gpui::point(px(-cycle), px(0.)),
        ));
        let mut bars = div().relative().size(px(side)).flex_none();
        for (index, x) in [3., 6.9, 10.8].into_iter().enumerate() {
            let phase = (self.phase + index as f32 / 6.).fract();
            bars = bars.child(
                div()
                    .absolute()
                    .left(px(x * unit))
                    .top(px(3. * unit))
                    .w(px(2.2 * unit))
                    .h(px(10. * unit))
                    .overflow_hidden()
                    .child(
                        gpui::svg()
                            .data(WAVE)
                            .absolute()
                            .left(px(-phase * cycle))
                            .top_0()
                            .w(px(404. * unit))
                            .h(px(10. * unit))
                            .text_color(color)
                            .when(self.moving, |wave| wave.with_timed_transformation(pan)),
                    ),
            );
        }
        bars
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
