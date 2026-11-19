//! Small marks the real client draws wherever a song is: its five stars
//! (`frame/Stars.svelte`) and the three bars of the song playing
//! (`frame/Eq.svelte`).

use gpui::{
    Context, Div, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, div, px,
};

use crate::theme;

/// Five stars, as the prototype draws them: in the accent when set, and
/// while the pointer is on them, filled up to the one it is on. `hovered`
/// is the star the pointer is on, which the view holding them keeps, since
/// a star's colour depends on its neighbour's hover. A click sets that
/// many; whether it takes them away is the caller's to decide. The pointer
/// coming onto a star and leaving it are told apart, since leaving one star
/// for the next may be heard after coming onto it.
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

/// CSS's `ease-in-out`, `cubic-bezier(0.42, 0, 0.58, 1)`, at `t`.
fn ease_in_out(t: f32) -> f32 {
    crate::shell::bezier(0.42, 0., 0.58, 1., t)
}

/// The mark of the song playing: three bars that move while it plays and
/// hold while it is paused. `seconds` is how long it has played, which the
/// player keeps; each bar rises and falls over 0.9 seconds, a third of the
/// way behind the one before.
pub(crate) fn eq(seconds: f32, side: f32, color: gpui::Rgba) -> impl IntoElement {
    let unit = side / 16.;
    let mut bars = div().relative().size(px(side)).flex_none();
    for (at, x) in [3., 6.9, 10.8].into_iter().enumerate() {
        let t = (seconds + 0.3 * at as f32) / 0.9;
        // `alternate`: up one cycle, down the next.
        let phase = t.rem_euclid(2.);
        let along = if phase <= 1. { phase } else { 2. - phase };
        let scale = 0.35 + 0.65 * ease_in_out(along);
        let height = 10. * unit * scale;
        bars = bars.child(
            div()
                .absolute()
                .left(px(x * unit))
                .top(px(13. * unit - height))
                .w(px(2.2 * unit))
                .h(px(height))
                .bg(color),
        );
    }
    bars
}
