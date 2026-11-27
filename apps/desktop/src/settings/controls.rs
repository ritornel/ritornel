//! The Settings window's furniture: grouped rows in boxes, as a SwiftUI form
//! has them, and the controls in them.
//!
//! GPUI has no native OS controls (push buttons, switches, pop-ups, date
//! fields, progress bars), so they are drawn here to match AppKit's metrics
//! (sizes, corners, and colours, light and dark, with the window active and
//! inactive).

use std::time::Duration;

use gpui::{
    Animation, AnimationExt, App, ClickEvent, Div, ElementId, FontWeight, InteractiveElement,
    IntoElement, ParentElement, Rgba, SharedString, StatefulInteractiveElement, Styled,
    Transformation, Window, div, percentage, prelude::FluentBuilder, px, rgba,
};

use crate::icons::icon;
use crate::marks::Mark;
use crate::theme::{self, Status};

/// The token for this appearance: dark first, light second.
fn pick(dark: u32, light: u32) -> Rgba {
    if theme::dark() {
        rgba(dark)
    } else {
        rgba(light)
    }
}

/// A box's ground: `--box`.
pub(crate) fn box_bg() -> Rgba {
    pick(0xffffff0b, 0xffffffff)
}
/// A box's edge: `--box-line`.
pub(crate) fn box_line() -> Rgba {
    pick(0xffffff14, 0x1c223017)
}
/// Between two rows: `--row-line`.
pub(crate) fn row_line() -> Rgba {
    pick(0xffffff12, 0x1c223014)
}
/// A row chosen in a list that does not have the keyboard: `--chosen`.
pub(crate) fn chosen() -> Rgba {
    pick(0xffffff1a, 0x1c223017)
}
/// A folder's colour: `color-mix(in srgb, var(--acc) 70%, var(--dim))`.
pub(crate) fn folder() -> Rgba {
    theme::mix(theme::dim(), theme::acc(), 0.7)
}

/// A control's label: AppKit's control text.
fn control_text(enabled: bool) -> Rgba {
    match (theme::dark(), enabled) {
        (true, true) => rgba(0xffffffd9),
        (true, false) => rgba(0xffffff40),
        (false, true) => rgba(0x000000d9),
        (false, false) => rgba(0x00000040),
    }
}

/// The height of a line of the window's text, 13 points on a line of
/// 1.35: WebKit makes a line a whole number of points, 17 rather than 17.55.
pub(crate) const LINE: f32 = 17.;
/// A line of the small text, 11 points on 1.35.
pub(crate) const SMALL_LINE: f32 = 14.;
/// A line of a note, 11 points on 1.4.
const SMALL_NOTE_LINE: f32 = 15.;

// ---- A group: a heading, rows in a box, and a note under it.

/// A group of the section, after the first 22 points below the last.
pub(crate) fn group(first: bool) -> Div {
    div()
        .flex()
        .flex_col()
        .when(!first, |group| group.mt(px(22.)))
}

/// A group's heading: `.group h2`.
pub(crate) fn heading(text: impl Into<SharedString>) -> Div {
    div()
        .mb(px(6.))
        .ml(px(2.))
        .font_weight(FontWeight(600.))
        .child(text.into())
}

/// Rows in a box, with a hairline between each two.
pub(crate) fn boxed(rows: Vec<Div>) -> Div {
    let rows = rows.into_iter().enumerate().map(|(at, row)| {
        if at == 0 {
            row
        } else {
            row.border_t_1().border_color(row_line())
        }
    });
    div()
        .flex()
        .flex_col()
        .border_1()
        .border_color(box_line())
        .rounded(px(8.))
        .bg(box_bg())
        .overflow_hidden()
        .children(rows)
}

/// A row of a box, at least 40 points tall.
pub(crate) fn row() -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(12.))
        .min_h(px(40.))
        .px(px(12.))
        .py(px(7.))
}

/// What takes the row's remaining width.
pub(crate) fn grow() -> Div {
    div().flex_1().min_w_0()
}

/// A second line, small and quiet, cut short with an ellipsis.
pub(crate) fn sub(text: impl Into<SharedString>) -> Div {
    div()
        .mt(px(1.))
        .text_size(px(11.))
        .line_height(px(SMALL_LINE))
        .text_color(theme::dim())
        .truncate()
        .child(text.into())
}

/// The same, wrapping: `.sub.wraps`.
pub(crate) fn sub_wraps(text: impl Into<SharedString>) -> Div {
    div()
        .mt(px(1.))
        .text_size(px(11.))
        .line_height(px(SMALL_LINE))
        .text_color(theme::dim())
        .child(text.into())
}

/// A label and its value at the right.
pub(crate) fn label_value(label: impl Into<SharedString>, value: impl Into<SharedString>) -> Div {
    label_status(label, value, Status::Quiet)
}

/// A label and its value at the right, in the value's status's colour, and
/// after the working arc while it is being done.
pub(crate) fn label_status(
    label: impl Into<SharedString>,
    value: impl Into<SharedString>,
    status: Status,
) -> Div {
    row().child(div().flex_1().child(label.into())).child(
        div()
            .flex()
            .items_center()
            .gap(px(6.))
            .text_color(status.text())
            .text_right()
            .when(status == Status::Working, |value| value.child(working()))
            .child(value.into()),
    )
}

/// The row's controls, 8 points apart: `.actions`.
pub(crate) fn actions() -> Div {
    div().flex().flex_none().gap(px(8.))
}

/// A note under a box.
pub(crate) fn note(text: impl Into<SharedString>) -> Div {
    div()
        .mt(px(6.))
        .mx(px(2.))
        .text_size(px(11.))
        .line_height(px(SMALL_NOTE_LINE))
        .text_color(theme::dim())
        .child(text.into())
}

/// What went wrong, under a box.
pub(crate) fn problem(text: impl Into<SharedString>) -> Div {
    div()
        .mt(px(6.))
        .mx(px(2.))
        .text_size(px(11.))
        .line_height(px(SMALL_LINE))
        .text_color(theme::fail_text())
        .child(text.into())
}

/// The system's monospaced face, as `ui-monospace` is.
pub(crate) const MONO: &str = "Menlo";

/// A folder, as the rows of folders have it.
pub(crate) fn folder_icon() -> impl IntoElement {
    gpui::svg()
        .path("icons/settings/folder.svg")
        .w(px(20.))
        .h(px(16.))
        .flex_none()
        .text_color(folder())
}

// ---- The controls.

/// A push button, as AppKit draws one at the regular size on macOS 26: 21
/// points tall, at least 72 wide (`button.push`), a flat capsule of grey
/// in a light window and an edged one in a dark.
pub(crate) fn push(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    enabled: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let dark = theme::dark();
    let face = div()
        .absolute()
        .top(px(if dark { 0. } else { 1. }))
        .bottom(px(if dark { 0. } else { 1. }))
        .left(px(if dark { 0.5 } else { 1. }))
        .right(px(if dark { 0.5 } else { 1. }))
        .rounded(px(if dark { 6. } else { 5. }))
        .bg(pick(0x2f2f2fff, 0xebebebff))
        .when(dark, |face| face.border_1().border_color(rgba(0x1e1e1eff)));
    let base = div()
        .id(id)
        .relative()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .h(px(21.))
        .min_w(px(72.))
        .px(px(8.))
        .whitespace_nowrap()
        .text_size(px(13.))
        .line_height(px(16.))
        .text_color(control_text(enabled))
        .child(face)
        .child(div().relative().child(label.into()));
    if enabled {
        base.active(theme::pressed)
            .on_click(on_click)
            .into_any_element()
    } else {
        base.into_any_element()
    }
}

/// A row a click on which does `on_click`, anywhere along it, as a
/// `<label class="row">` or a list's option is: an unseen layer over it
/// takes the click, so that the row stays a plain one in its box.
pub(crate) fn clickable(
    row: Div,
    id: impl Into<ElementId>,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Div {
    row.relative().child(
        div()
            .id(id)
            .absolute()
            .top_0()
            .bottom_0()
            .left_0()
            .right_0()
            // Pressed, the row takes the ground a row takes under the
            // pointer elsewhere: the layer is all that is pressed.
            .active(theme::hovered)
            .on_click(on_click),
    )
}

/// A row with a switch at its right, the whole row its label.
pub(crate) fn switch_row(
    id: impl Into<ElementId>,
    label: Div,
    on: bool,
    enabled: bool,
    window_active: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Div {
    let row = row().child(label).child(switch(on, enabled, window_active));
    if enabled {
        clickable(row, id, on_click)
    } else {
        row
    }
}

/// A switch, as AppKit draws one on macOS 26: a capsule 41 points by 18
/// with a knob of 24.5 by 15, in the accent when on, grey when off or when
/// the window is behind.
fn switch(on: bool, enabled: bool, window_active: bool) -> impl IntoElement {
    let dark = theme::dark();
    let track = match (on && window_active, dark) {
        (true, _) => theme::system_accent(),
        (false, true) if on => rgba(0x3d3d3dff),
        (false, true) => rgba(0xffffff26),
        (false, false) if on => rgba(0xd9d9d9ff),
        (false, false) => rgba(0x00000017),
    };
    let knob = if dark && !(on && window_active) {
        rgba(0xe2e2e2ff)
    } else {
        rgba(0xffffffff)
    };
    div()
        .flex_none()
        .relative()
        .w(px(41.))
        .h(px(18.))
        .rounded(px(9.))
        .bg(track)
        .when(!enabled, |base| base.opacity(0.5))
        .child(
            div()
                .absolute()
                .top(px(1.5))
                .left(px(if on { 41. - 1.5 - 24.5 } else { 1.5 }))
                .w(px(24.5))
                .h(px(15.))
                .rounded(px(7.5))
                .bg(knob)
                .shadow(vec![gpui::BoxShadow {
                    color: gpui::hsla(0., 0., 0., 0.18),
                    offset: gpui::point(px(0.), px(0.5)),
                    blur_radius: px(1.),
                    spread_radius: px(0.),
                    inset: false,
                }]),
        )
}

/// A progress bar across its row, as AppKit draws one: `done` of `total`,
/// or, with no total, a stretch that runs along it.
pub(crate) fn progress(id: impl Into<ElementId>, of: Option<(u64, u64)>) -> impl IntoElement {
    let track = pick(0xffffff26, 0x0000001a);
    let bar = div()
        .absolute()
        .top_0()
        .bottom_0()
        .rounded(px(2.5))
        .bg(theme::system_accent());
    let inner = match of {
        Some((done, total)) => {
            let share = if total == 0 {
                0.
            } else {
                (done as f32 / total as f32).clamp(0., 1.)
            };
            bar.left_0().w(gpui::relative(share)).into_any_element()
        }
        None if crate::platform::reduce_motion() => {
            bar.left_0().w(gpui::relative(0.3)).into_any_element()
        }
        None => bar
            .w(gpui::relative(0.3))
            .with_animation(
                "running",
                Animation::new(Duration::from_millis(1400)).repeat(),
                |bar, t| bar.left(gpui::relative(t * 1.3 - 0.3)),
            )
            .into_any_element(),
    };
    div()
        .id(id)
        .w_full()
        .h(px(16.))
        .flex()
        .items_center()
        .child(
            div()
                .relative()
                .w_full()
                .h(px(5.))
                .rounded(px(2.5))
                .bg(track)
                .overflow_hidden()
                .child(inner),
        )
}

/// What is being done, for a wait with no end to show: the app's working
/// arc (`marks::Mark`), as everywhere else. The scan's progress bar keeps
/// the system's accent, as AppKit's does.
pub(crate) fn working() -> gpui::AnyElement {
    Mark::here(Status::Working).draw(WORKING)
}

/// How big the working arc is beside a line of the window's text.
const WORKING: f32 = 12.;

/// A pop-up button, as AppKit draws one: a push button's face with its
/// choice at the left and the up and down chevrons at the right. The menu
/// it opens is the system's (`platform::pop_up_menu`).
pub(crate) fn pop_up(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let dark = theme::dark();
    div()
        .id(id)
        .relative()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(6.))
        .h(px(21.))
        .pl(px(9.))
        .pr(px(5.))
        .text_size(px(13.))
        .line_height(px(16.))
        .text_color(control_text(true))
        .child(
            div()
                .absolute()
                .top(px(if dark { 0. } else { 1. }))
                .bottom(px(if dark { 0. } else { 1. }))
                .left(px(if dark { 0.5 } else { 1. }))
                .right(px(if dark { 0.5 } else { 1. }))
                .rounded(px(if dark { 6. } else { 5. }))
                .bg(pick(0x2f2f2fff, 0xebebebff))
                .when(dark, |face| face.border_1().border_color(rgba(0x1e1e1eff))),
        )
        .child(div().relative().whitespace_nowrap().child(label.into()))
        .child(
            div()
                .relative()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(1.))
                .child(
                    icon("chevron", 9., control_text(true))
                        .with_transformation(Transformation::rotate(percentage(0.5))),
                )
                .child(icon("chevron", 9., control_text(true))),
        )
        .active(theme::pressed)
        .on_click(on_click)
}

/// A date field, as WebKit draws `<input type="date">`: month, day and year
/// in a field. A stand-in: it shows the date but takes no typing yet, which
/// waits for the app's text input.
pub(crate) fn date_field(date: &str) -> impl IntoElement {
    // `YYYY-MM-DD`, as an American field shows it: 11/18/2026.
    let mut parts = date.split('-');
    let shown = match (parts.next(), parts.next(), parts.next()) {
        (Some(year), Some(month), Some(day)) => format!(
            "{}/{}/{year}",
            month.trim_start_matches('0'),
            day.trim_start_matches('0')
        ),
        _ => "mm/dd/yyyy".to_owned(),
    };
    div()
        .flex()
        .flex_none()
        .items_center()
        .h(px(21.))
        .px(px(5.))
        .rounded(px(5.))
        .border_1()
        .border_color(pick(0xffffff26, 0x00000026))
        .bg(pick(0xffffff0d, 0xffffffff))
        .text_size(px(13.))
        .line_height(px(16.))
        .text_color(if date.is_empty() {
            theme::faint()
        } else {
            control_text(true)
        })
        .child(shown)
}

/// The list's own + and −, as a Mac list has them at its foot.
pub(crate) fn edit(
    id: impl Into<ElementId>,
    glyph: &'static str,
    enabled: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let base = div()
        .id(id)
        .w(px(24.))
        .h(px(20.))
        .rounded(px(4.))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(15.))
        .line_height(px(15.));
    if enabled {
        base.hover(theme::hovered_on_fill)
            .active(theme::pressed)
            .on_click(on_click)
            .child(glyph)
            .into_any_element()
    } else {
        base.opacity(0.35).child(glyph).into_any_element()
    }
}
