//! The card that changes the view on show ("Library views" in
//! docs/internals/desktop.md).
//!
//! It floats over the wall's top right corner, under the button that opens
//! it, and every choice on it changes the wall behind it at once and is
//! kept (`Wall::edit`). A press outside it puts it away, and so does Esc.
//! The wall is not laid out around it, so opening and closing it never
//! moves what is on show.
//!
//! Its fields take the keys while one is focused (`Wall::typing`): the
//! shell leaves them alone meanwhile, as it does for Search, and gets them
//! back through its own focus handle when the card closes.

use std::time::{Duration, Instant};

use gpui::{
    AnyElement, AppContext as _, Context, Div, Entity, FontWeight, InteractiveElement, IntoElement,
    MouseButton, ParentElement, SharedString, Stateful, StatefulInteractiveElement, Styled,
    Subscription, Task, Window, div, prelude::FluentBuilder, px,
};

use super::{HEADER, Wall};
use crate::icons::icon;
use crate::input::{Field, FieldEvent};
use crate::theme;
use crate::views::{self, Density, Entity as Listed, LibraryView, Presentation};

/// The card's width, and what is inside its edge.
const WIDTH: f32 = 348.;
const INSET: f32 = 16.;
/// The column of the labels that stand beside what they name.
const BESIDE: f32 = 62.;
/// How long typing rests before what was typed narrows the view.
const RESTS: Duration = Duration::from_millis(280);
/// How long Reset waits for a second click.
const SURE: Duration = Duration::from_secs(4);

/// One of the card's fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Which {
    Name,
    Text,
    From,
    To,
}

/// The card's state: whether it is open, and its fields.
pub(super) struct Customize {
    pub open: bool,
    name: Entity<Field>,
    text: Entity<Field>,
    from: Entity<Field>,
    to: Entity<Field>,
    /// Narrows the view by what was typed, once the typing rests.
    resting: Option<Task<()>>,
    /// The keys go back to the shell at the next frame, which has the
    /// window to give them with.
    hand_back: bool,
    /// When Reset was first clicked: a second click within `SURE` resets.
    sure: Option<Instant>,
    _fields: [Subscription; 4],
}

impl Customize {
    pub(super) fn new(cx: &mut Context<Wall>) -> Self {
        let field = |placeholder: &'static str, which: Which, cx: &mut Context<Wall>| {
            let field = cx.new(|cx| Field::new(placeholder, cx));
            let heard = cx.subscribe(&field, move |wall, _, event: &FieldEvent, cx| {
                wall.field_said(which, event, cx);
            });
            (field, heard)
        };
        let (name, name_heard) = field("Name", Which::Name, cx);
        let (text, text_heard) = field("Title, artist or album", Which::Text, cx);
        let (from, from_heard) = field("From", Which::From, cx);
        let (to, to_heard) = field("To", Which::To, cx);
        Self {
            open: false,
            name,
            text,
            from,
            to,
            resting: None,
            hand_back: false,
            sure: None,
            _fields: [name_heard, text_heard, from_heard, to_heard],
        }
    }

    fn fields(&self) -> [&Entity<Field>; 4] {
        [&self.name, &self.text, &self.from, &self.to]
    }

    /// Puts what `view` says into the fields: another view came on show,
    /// or this one changed some other way. A field that already says it,
    /// give or take the spaces at its ends, is left as it is, so that what
    /// is being typed is not moved under the cursor.
    pub(super) fn fill(&self, view: &LibraryView, cx: &mut Context<Wall>) {
        let year = |year: Option<i64>| year.map(|year| year.to_string()).unwrap_or_default();
        let said = [
            (&self.name, view.name.clone()),
            (&self.text, view.filter.text.clone().unwrap_or_default()),
            (&self.from, year(view.filter.year_from)),
            (&self.to, year(view.filter.year_to)),
        ];
        for (field, text) in said {
            field.update(cx, |field, cx| {
                if field.text().trim() != text.trim() {
                    field.set(&text, cx);
                }
            });
        }
    }
}

impl Wall {
    /// Whether a field of the card has the keys: they are the field's, not
    /// the wall's or the shell's.
    pub(crate) fn typing(&self, window: &Window, cx: &gpui::App) -> bool {
        self.customize.open
            && self
                .customize
                .fields()
                .iter()
                .any(|field| field.read(cx).focused(window))
    }

    /// Opens the card on the view on show.
    pub(crate) fn open_customize(&mut self, cx: &mut Context<Self>) {
        self.customize.open = true;
        self.customize.sure = None;
        self.customize.fill(self.views.active(), cx);
        cx.notify();
    }

    /// Puts the card away, the keys going back to the shell.
    pub(crate) fn close_customize(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_customize_soon(cx);
        self.hand_back(window, cx);
    }

    /// Puts the card away from where there is no window to give the keys
    /// back with: they go back as the next frame is drawn.
    fn close_customize_soon(&mut self, cx: &mut Context<Self>) {
        self.customize.open = false;
        self.customize.hand_back = true;
        self.flush_typed(cx);
        cx.notify();
    }

    /// Gives the keys back to the shell, if a field has them or the card
    /// closed owing them.
    pub(super) fn hand_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let held = self
            .customize
            .fields()
            .iter()
            .any(|field| field.read(cx).focused(window));
        if (std::mem::take(&mut self.customize.hand_back) || !self.customize.open)
            && held
            && let Some(home) = &self.home
        {
            window.focus(home, cx);
        }
    }

    /// What a field of the card says: its text changed, or Return or Esc.
    fn field_said(&mut self, which: Which, event: &FieldEvent, cx: &mut Context<Self>) {
        match event {
            FieldEvent::Changed(text) => {
                let text = text.to_string();
                match which {
                    // A name changes nothing the server is asked, so it is
                    // kept as it is typed.
                    Which::Name => self.edit(|view| view.name = text, cx),
                    // The rest open the view again: once the typing rests.
                    Which::Text | Which::From | Which::To => {
                        self.customize.resting = Some(cx.spawn(async move |wall, cx| {
                            cx.background_executor().timer(RESTS).await;
                            let _ = wall.update(cx, |wall, cx| wall.flush_typed(cx));
                        }));
                    }
                }
            }
            // Return leaves the field; Esc puts the card away.
            FieldEvent::Entered => {
                self.flush_typed(cx);
                self.customize.hand_back = true;
                cx.notify();
            }
            FieldEvent::Escaped => self.close_customize_soon(cx),
            FieldEvent::Moved(_) => {}
        }
    }

    /// Narrows the view by what its fields say now, without waiting for
    /// the typing to rest.
    fn flush_typed(&mut self, cx: &mut Context<Self>) {
        self.customize.resting = None;
        let read = |field: &Entity<Field>| field.read(cx).text().trim().to_owned();
        let text = read(&self.customize.text);
        // A year half typed, or not a year, narrows nothing.
        let year = |field: &Entity<Field>| read(field).parse::<i64>().ok().filter(|year| *year > 0);
        let (from, to) = (year(&self.customize.from), year(&self.customize.to));
        self.edit(
            |view| {
                view.filter.text = (!text.is_empty()).then_some(text);
                if view.entity.has_years() {
                    view.filter.year_from = from;
                    view.filter.year_to = to;
                }
            },
            cx,
        );
    }

    /// Takes the view on show out of the Library, and shows the one before.
    fn delete_view(&mut self, cx: &mut Context<Self>) {
        if self.views.remove_active() {
            self.views.save();
            self.customize.fill(self.views.active(), cx);
            self.show(cx);
        }
    }

    /// Moves the view on show a place left or right among the views.
    fn move_view(&mut self, by: isize, cx: &mut Context<Self>) {
        if self.views.move_active(by) {
            self.views.save();
            cx.notify();
        }
    }

    /// Reset, clicked: the first click asks, and a second soon after puts
    /// the Library's views back as they come.
    fn reset_views(&mut self, cx: &mut Context<Self>) {
        if self.customize.sure.is_none_or(|at| at.elapsed() > SURE) {
            self.customize.sure = Some(Instant::now());
            cx.spawn(async move |wall, cx| {
                cx.background_executor().timer(SURE).await;
                let _ = wall.update(cx, |wall, cx| {
                    wall.customize.sure = None;
                    cx.notify();
                });
            })
            .detach();
            cx.notify();
            return;
        }
        self.customize.sure = None;
        self.views = views::Views::default();
        self.views.save();
        self.customize.fill(self.views.active(), cx);
        self.show(cx);
    }

    /// The card and the veil under it, when it is open: what is drawn over
    /// the wall. `top` is how far the page is scrolled, and `right` the
    /// page's right margin.
    pub(super) fn customize_card(
        &mut self,
        right: f32,
        top: f32,
        viewport: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        self.hand_back(window, cx);
        if !self.customize.open {
            return Vec::new();
        }
        // A press anywhere else puts it away and is not passed on; a
        // scroll is, so the wall can be read while it is changed.
        let veil = div()
            .id("customize-veil")
            .absolute()
            .size_full()
            .occlude()
            .on_scroll(cx.listener(Self::on_scroll))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|wall, _, window, cx| {
                    cx.stop_propagation();
                    wall.close_customize(window, cx);
                }),
            )
            .into_any_element();
        // Under the controls, or at the top once they have scrolled away.
        let y = (HEADER - 16. - top).max(10.);
        let view = self.config().clone();
        let entity = view.entity;
        let inner = WIDTH - 2. * INSET;
        // Its name and its way out stay at the top, and what is done with
        // the view at the foot; what is between scrolls when the window is
        // too short for it all.
        let top_row = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(10.))
            .px(px(INSET))
            .pt(px(14.))
            .pb(px(10.))
            .child(
                div()
                    .text_size(px(15.))
                    .font_weight(FontWeight(650.))
                    .text_color(theme::ink())
                    .child("Customize view"),
            )
            .child(div().flex_1())
            .child(
                div()
                    .text_size(px(11.5))
                    .text_color(theme::faint())
                    .child(format!("Kept in {}", views::FILE)),
            )
            .child(
                div()
                    .id("customize-close")
                    .size(px(24.))
                    .mr(px(-6.))
                    .rounded_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .hover(theme::hovered)
                    .active(theme::pressed)
                    .on_click(cx.listener(|wall, _, window, cx| {
                        wall.close_customize(window, cx);
                    }))
                    .child(icon("close", 10., theme::dim())),
            );
        let mut body = div()
            .id("customize-body")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .gap(px(12.))
            .px(px(INSET))
            .pb(px(14.))
            .overflow_y_scroll();
        body = body.child(beside(
            "Name",
            self.boxed(&self.customize.name, inner - BESIDE, window, cx),
        ));
        body = body.child(beside(
            "Show",
            segmented(
                "entity",
                Listed::ALL.map(|each| (each, each.label())),
                entity,
                |view, entity| view.entity = entity,
                cx,
            ),
        ));
        body = body.child(beside(
            "As",
            segmented(
                "presentation",
                Presentation::ALL.map(|each| (each, each.label())),
                view.presentation,
                |view, presentation| view.presentation = presentation,
                cx,
            ),
        ));
        body = body.child(beside(
            "Density",
            segmented(
                "density",
                Density::ALL.map(|each| (each, each.label())),
                view.density,
                |view, density| view.density = density,
                cx,
            ),
        ));
        body = body.child(labelled(
            "Group by",
            chips(
                "grouping",
                entity
                    .groupings()
                    .iter()
                    .map(|each| (*each, each.label(), *each == view.grouping)),
                |view, grouping| view.grouping = grouping,
                cx,
            ),
        ));
        body = body.child(labelled(
            "Sort by",
            chips(
                "sort",
                entity
                    .sorts()
                    .iter()
                    .map(|each| (*each, each.label(), *each == view.sort)),
                |view, sort| view.sort = sort,
                cx,
            ),
        ));
        let mut only = div().flex().flex_col().gap(px(8.)).child(chips(
            "narrow",
            entity
                .narrows()
                .iter()
                .map(|each| (*each, each.label(), view.filter.has(*each))),
            |view, narrow| view.filter.toggle(narrow),
            cx,
        ));
        if entity.has_years() {
            // The two years share what the word and the dash leave.
            let half = ((inner - BESIDE - 26.) / 2.).floor();
            only = only.child(
                div()
                    .flex()
                    .items_center()
                    .text_size(px(13.))
                    .text_color(theme::dim())
                    .child(div().w(px(BESIDE)).flex_none().child("Years"))
                    .child(self.boxed(&self.customize.from, half, window, cx))
                    .child(div().w(px(26.)).flex().justify_center().child("–"))
                    .child(self.boxed(&self.customize.to, half, window, cx)),
            );
        }
        only = only.child(self.boxed(&self.customize.text, inner, window, cx));
        body = body.child(labelled("Only", only));

        // The view's place among the others, a copy of it, and its end.
        let index = self.views.active_index();
        let last = self.views.views.len() - 1;
        let sure = self.customize.sure.is_some_and(|at| at.elapsed() <= SURE);
        let foot = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(4.))
            .px(px(INSET - 6.))
            .py(px(9.))
            .border_t_1()
            .border_color(theme::line())
            .child(act(
                "move-left",
                Some("back"),
                "",
                index > 0,
                false,
                |wall, cx| wall.move_view(-1, cx),
                cx,
            ))
            .child(act(
                "move-right",
                Some("chevronRight"),
                "",
                index < last,
                false,
                |wall, cx| wall.move_view(1, cx),
                cx,
            ))
            .child(act(
                "duplicate",
                None,
                "Duplicate",
                true,
                false,
                Self::duplicate,
                cx,
            ))
            .child(act(
                "delete",
                None,
                "Delete",
                last > 0,
                false,
                Self::delete_view,
                cx,
            ))
            .child(div().flex_1())
            .child(act(
                "reset",
                None,
                if sure { "Reset all views?" } else { "Reset" },
                true,
                sure,
                Self::reset_views,
                cx,
            ));
        let card = div()
            .absolute()
            .right(px(right))
            .top(px(y))
            .w(px(WIDTH))
            .max_h(px((viewport - y - 12.).max(200.)))
            .flex()
            .flex_col()
            .rounded(px(14.))
            .overflow_hidden()
            .bg(theme::surf())
            .border_1()
            .border_color(theme::line())
            .shadow(vec![theme::css_shadow(14., 40., gpui::rgba(0x00000073))])
            .occlude()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(top_row)
            .child(body)
            .child(foot)
            .into_any_element();
        vec![veil, card]
    }

    /// A field in its box, `width` wide: lit while it has the keys, which a
    /// press anywhere on the box gives it.
    fn boxed(
        &self,
        field: &Entity<Field>,
        width: f32,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let focused = field.read(cx).focused(window);
        let pressed = field.clone();
        div()
            .w(px(width))
            .flex_none()
            .h(px(30.))
            .px(px(10.))
            .rounded(px(8.))
            .flex()
            .items_center()
            .text_size(px(13.))
            .text_color(theme::faint())
            .bg(if focused {
                theme::raised()
            } else {
                theme::fill()
            })
            .when(focused, |boxed| {
                boxed.shadow(vec![
                    gpui::BoxShadow::new(
                        px(0.),
                        px(0.),
                        gpui::Hsla::from(gpui::Rgba {
                            a: 0.55,
                            ..theme::acc()
                        }),
                    )
                    .spread_radius(px(2.)),
                ])
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |_, _, window, cx| {
                    cx.stop_propagation();
                    pressed.update(cx, |field, cx| {
                        if !field.focused(window) {
                            field.focus_all(window, cx);
                        }
                    });
                }),
            )
            .child(field.clone())
    }
}

#[cfg(test)]
impl Wall {
    fn field(&self, which: Which) -> &Entity<Field> {
        match which {
            Which::Name => &self.customize.name,
            Which::Text => &self.customize.text,
            Which::From => &self.customize.from,
            Which::To => &self.customize.to,
        }
    }

    /// Types `text` into one of the card's fields, as the keys would.
    pub(super) fn type_into(&mut self, which: Which, text: &str, cx: &mut Context<Self>) {
        self.field(which)
            .update(cx, |field, cx| field.set(text, cx));
        self.field_said(which, &FieldEvent::Changed(text.to_owned().into()), cx);
    }

    /// Gives one of the card's fields the keys, as a press on it does.
    pub(super) fn focus_field(&self, which: Which, window: &mut Window, cx: &mut Context<Self>) {
        self.field(which)
            .update(cx, |field, cx| field.focus_all(window, cx));
    }

    /// What one of the card's fields says.
    pub(super) fn field_text(&self, which: Which, cx: &gpui::App) -> String {
        self.field(which).read(cx).text().to_owned()
    }
}

/// A part of the card beside its label, in a column `BESIDE` wide.
fn beside(label: &'static str, part: Div) -> Div {
    div()
        .flex()
        .items_center()
        .child(
            div()
                .w(px(BESIDE))
                .flex_none()
                .text_size(px(11.5))
                .font_weight(FontWeight(600.))
                .text_color(theme::faint())
                .child(label),
        )
        .child(part)
}

/// A part of the card under its label.
fn labelled(label: &'static str, part: impl IntoElement) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(
            div()
                .text_size(px(11.5))
                .font_weight(FontWeight(600.))
                .text_color(theme::faint())
                .child(label),
        )
        .child(part)
}

/// One choice of a few, side by side in a track, the one chosen raised.
fn segmented<T: Copy + PartialEq + 'static, const N: usize>(
    id: &'static str,
    choices: [(T, &'static str); N],
    chosen: T,
    set: fn(&mut LibraryView, T),
    cx: &mut Context<Wall>,
) -> Div {
    let mut track = div()
        .flex()
        .flex_1()
        .p(px(2.))
        .rounded(px(9.))
        .bg(theme::fill())
        .text_size(px(13.));
    for (choice, label) in choices {
        let segment = div()
            .id(SharedString::from(format!("{id}-{label}")))
            .flex_1()
            .h(px(26.))
            .rounded(px(7.))
            .flex()
            .items_center()
            .justify_center()
            .on_click(cx.listener(move |wall, _, _, cx| {
                wall.edit(|view| set(view, choice), cx);
            }));
        track = track.child(if choice == chosen {
            segment
                .bg(theme::raised())
                .text_color(theme::ink())
                .font_weight(FontWeight(600.))
                .shadow(vec![theme::css_shadow(1., 3., gpui::rgba(0x00000024))])
                .child(label)
        } else {
            segment
                .text_color(theme::dim())
                .font_weight(FontWeight(500.))
                .hover(theme::hovered_ink)
                .active(theme::pressed)
                .child(label)
        });
    }
    track
}

/// Choices that wrap onto as many lines as they need, each a chip, those
/// that hold lit.
fn chips<T: Copy + 'static>(
    id: &'static str,
    choices: impl Iterator<Item = (T, &'static str, bool)>,
    set: fn(&mut LibraryView, T),
    cx: &mut Context<Wall>,
) -> Div {
    let mut row = div().flex().flex_wrap().gap(px(6.));
    for (choice, label, on) in choices {
        let chip = div()
            .id(SharedString::from(format!("{id}-{label}")))
            .h(px(26.))
            .px(px(10.))
            .rounded(px(13.))
            .flex()
            .items_center()
            .text_size(px(12.5))
            .whitespace_nowrap()
            .on_click(cx.listener(move |wall, _, _, cx| {
                wall.edit(|view| set(view, choice), cx);
            }));
        row = row.child(if on {
            chip.bg(theme::lit())
                .text_color(theme::lit_words())
                .font_weight(FontWeight(600.))
                .active(theme::pressed)
                .child(label)
        } else {
            chip.bg(theme::fill())
                .text_color(theme::dim())
                .font_weight(FontWeight(500.))
                .hover(|style| theme::hovered_ink(theme::hovered_on_fill(style)))
                .active(theme::pressed)
                .child(label)
        });
    }
    row
}

/// A quiet button at the card's foot: an icon or a word. One that cannot
/// be used is faded and answers nothing, and one that asks to be sure
/// wears the warning's colour.
fn act(
    id: &'static str,
    picture: Option<&'static str>,
    label: &'static str,
    enabled: bool,
    warns: bool,
    run: fn(&mut Wall, &mut Context<Wall>),
    cx: &mut Context<Wall>,
) -> Stateful<Div> {
    let ink = if warns { theme::warn() } else { theme::dim() };
    let button = div()
        .id(id)
        .h(px(26.))
        .px(px(if picture.is_some() { 7. } else { 9. }))
        .rounded(px(7.))
        .flex()
        .flex_none()
        .items_center()
        .text_size(px(12.5))
        .font_weight(FontWeight(500.))
        .text_color(ink)
        .whitespace_nowrap()
        .children(picture.map(|picture| icon(picture, 12., ink)))
        .when(!label.is_empty(), |button| button.child(label));
    if !enabled {
        return button.opacity(0.35);
    }
    button
        .hover(|style| theme::hovered_ink(theme::hovered(style)))
        .active(theme::pressed)
        .on_click(cx.listener(move |wall, _, _, cx| run(wall, cx)))
}
