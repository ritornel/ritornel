//! The controls over the wall ("Library views" in
//! docs/internals/desktop.md): the page's name and how many it lists, and
//! one row under it, scrolling away with the wall. At the left the views,
//! each a pill, and a + that adds one; at the right how the rows are drawn,
//! the order, and the button that opens the card that changes the view.

use gpui::{
    AnyElement, Context, FontWeight, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, px,
};

use super::{Wall, name_row};
use crate::icons::icon;
use crate::menu::Entry;
use crate::theme;
use crate::views::{Presentation, TEMPLATES, Views};

/// What an item of the + does to the wall.
type Does = Box<dyn FnOnce(&mut Wall, &mut Context<Wall>)>;

impl Wall {
    /// The page's name and how many there are, and the row of controls
    /// under it, in the `width` between the page's margins.
    pub(super) fn header(
        &self,
        gutter: f32,
        right: f32,
        y: f32,
        width: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let view = self.config();
        let count = (self.view.is_some() && self.total > 0).then(|| view.entity.count(self.total));
        // A narrow page keeps its controls and lets go of their words.
        let roomy = width >= 900.;
        div()
            .absolute()
            .left(px(gutter))
            .right(px(right))
            .top(px(y))
            .flex()
            .flex_col()
            .child(
                div()
                    .relative()
                    .h(px(42.))
                    .mb(px(6.))
                    .child(name_row(0., 0., 0., count)),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .h(px(52.))
                    .child(self.strip(cx))
                    .child(self.plus(cx))
                    .child(div().flex_1().min_w(px(4.)))
                    .child(self.presentations(cx))
                    .children(self.sort(roomy, cx))
                    .child(self.customize_button(roomy, cx)),
            )
            .into_any_element()
    }

    /// The views, each a pill, the one on show raised. More than fit are
    /// scrolled across, never cut off or squeezed.
    fn strip(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self.views.active_index();
        let mut strip = div()
            .id("views")
            .flex()
            .flex_shrink(1.)
            .min_w(px(96.))
            .p(px(2.))
            .rounded(px(10.))
            .bg(theme::fill())
            .text_size(px(13.))
            .overflow_x_scroll()
            .track_scroll(&self.strip);
        for (index, view) in self.views.views.iter().enumerate() {
            let pill = div()
                .id(SharedString::from(format!("view-{}", view.id)))
                .flex()
                .flex_none()
                .items_center()
                .h(px(28.))
                .px(px(13.))
                .rounded(px(8.))
                .whitespace_nowrap()
                .on_click(cx.listener(move |wall, _, _, cx| wall.select(index, cx)));
            strip = strip.child(if index == active {
                pill.bg(theme::raised())
                    .text_color(theme::ink())
                    .font_weight(FontWeight(600.))
                    .shadow(vec![theme::css_shadow(1., 3., gpui::rgba(0x00000024))])
                    .child(view.title().to_owned())
            } else {
                pill.text_color(theme::dim())
                    .font_weight(FontWeight(500.))
                    .hover(theme::hovered_ink)
                    .active(theme::pressed)
                    .child(view.title().to_owned())
            });
        }
        strip
    }

    /// The + that adds a view: a menu of views ready made.
    fn plus(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("add-view")
            .flex_none()
            .size(px(28.))
            .rounded(px(8.))
            .flex()
            .items_center()
            .justify_center()
            .hover(theme::hovered)
            .active(theme::pressed)
            .on_click(cx.listener(|wall, event: &gpui::ClickEvent, window, cx| {
                wall.add_menu(event.position(), window, cx);
            }))
            .child(icon("plus", 11., theme::dim()))
    }

    /// The menu of the +: a view like the one on show to start from, the
    /// views ready made, and a copy of the one on show.
    fn add_menu(
        &mut self,
        at: gpui::Point<gpui::Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let wall = cx.entity().downgrade();
        let does = |text: &str, run: Does| {
            let wall = wall.clone();
            crate::menu::item(text, move |_, cx| {
                let _ = wall.update(cx, |wall, cx| run(wall, cx));
            })
        };
        let mut entries: Vec<Option<Entry>> = vec![
            does("New View…", Box::new(Self::new_view)),
            Some(Entry::Separator),
        ];
        for template in TEMPLATES {
            entries.push(does(
                template.name,
                Box::new(move |wall, cx| wall.add(template.view(), cx)),
            ));
        }
        entries.push(Some(Entry::Separator));
        entries.push(does("Duplicate Current View…", Box::new(Self::duplicate)));
        crate::menu::popup("add-view".to_owned(), entries, at, window, cx);
    }

    /// A new view of what the one on show lists, as that entity's first
    /// view is, with the card open to make it something else.
    fn new_view(&mut self, cx: &mut Context<Self>) {
        let entity = self.config().entity;
        let Some(mut view) = Views::default()
            .views
            .into_iter()
            .find(|view| view.entity == entity)
        else {
            return;
        };
        view.name = format!("New {}", entity.label());
        self.add(view, cx);
        self.open_customize(cx);
    }

    /// A copy of the view on show, with the card open to change it.
    pub(super) fn duplicate(&mut self, cx: &mut Context<Self>) {
        let mut copy = self.config().clone();
        copy.name = format!("{} copy", copy.name);
        self.add(copy, cx);
        self.open_customize(cx);
    }

    /// Adds the view ready made that a script names, and shows it. Whether
    /// there is one of that name.
    pub(crate) fn add_template(&mut self, id: &str, cx: &mut Context<Self>) -> bool {
        let Some(template) = TEMPLATES.iter().find(|template| template.id == id) else {
            return false;
        };
        self.add(template.view(), cx);
        true
    }

    /// How the rows are drawn: three icons, the one in use raised. A click
    /// changes it at once.
    fn presentations(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let current = self.config().presentation;
        let mut toggle = div()
            .flex()
            .flex_none()
            .p(px(2.))
            .rounded(px(10.))
            .bg(theme::fill());
        for presentation in Presentation::ALL {
            let on = presentation == current;
            let button = div()
                .id(presentation.name())
                .w(px(32.))
                .h(px(28.))
                .rounded(px(8.))
                .flex()
                .items_center()
                .justify_center()
                .on_click(cx.listener(move |wall, _, _, cx| {
                    wall.edit(|view| view.presentation = presentation, cx);
                }));
            toggle = toggle.child(if on {
                button
                    .bg(theme::raised())
                    .shadow(vec![theme::css_shadow(1., 3., gpui::rgba(0x00000024))])
                    .child(icon(presentation.icon(), 15., theme::ink()))
            } else {
                button
                    .hover(theme::hovered_on_fill)
                    .active(theme::pressed)
                    .child(icon(presentation.icon(), 15., theme::dim()))
            });
        }
        toggle
    }

    /// The order, in a pop-up menu of the orders the view's entity takes.
    /// An entity with one order has none to choose, and neither has an
    /// empty view.
    fn sort(&self, roomy: bool, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let view = self.config();
        let choices = view.entity.sorts();
        let order = view.sort;
        let empty = self.view.is_some() && self.total == 0;
        if choices.len() < 2 || empty {
            return None;
        }
        // As wide as its longest order, so it does not move as they change.
        let longest = choices
            .iter()
            .map(|each| each.label().len())
            .max()
            .unwrap_or(0);
        Some(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(px(8.))
                .text_size(px(13.))
                .text_color(theme::dim())
                .when(roomy, |sort| sort.child("Sort by"))
                .child(
                    div()
                        .id("sort")
                        .relative()
                        .flex()
                        .items_center()
                        .w(px(40. + longest as f32 * 7.2))
                        .h(px(30.))
                        .pl(px(12.))
                        .rounded(px(8.))
                        .bg(theme::fill())
                        .hover(theme::hovered_on_fill)
                        .active(theme::pressed)
                        .text_color(theme::ink())
                        .font_weight(FontWeight(600.))
                        .whitespace_nowrap()
                        .on_click(cx.listener(move |_, event: &gpui::ClickEvent, window, cx| {
                            let window = window.window_handle();
                            let at = event.position();
                            let checked = choices.iter().position(|each| *each == order);
                            let labels =
                                choices.iter().map(|each| each.label().to_owned()).collect();
                            cx.spawn(async move |wall, cx| {
                                let chosen =
                                    crate::menu::choose(window, labels, checked, at, cx).await;
                                if let Some(order) =
                                    chosen.and_then(|index| choices.get(index)).copied()
                                {
                                    let _ = wall.update(cx, |wall, cx| {
                                        wall.edit(|view| view.sort = order, cx)
                                    });
                                }
                            })
                            .detach();
                        }))
                        .child(order.label())
                        .child(
                            div()
                                .absolute()
                                .right(px(9.))
                                .top(px(8.))
                                // In the accent, as the system's pop-ups
                                // have theirs.
                                .child(icon("chevron", 14., theme::acc())),
                        ),
                ),
        )
    }

    /// The button that opens the card that changes the view on show, lit
    /// while the card is open.
    fn customize_button(&self, roomy: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let open = self.customize.open;
        let ink = if open {
            theme::lit_words()
        } else {
            theme::ink()
        };
        div()
            .id("customize")
            .flex()
            .flex_none()
            .items_center()
            .gap(px(7.))
            .h(px(30.))
            .px(px(11.))
            .rounded(px(8.))
            .text_size(px(13.))
            .font_weight(FontWeight(600.))
            .text_color(ink)
            .bg(if open { theme::lit() } else { theme::fill() })
            .when(!open, |button| button.hover(theme::hovered_on_fill))
            .active(theme::pressed)
            .on_click(cx.listener(|wall, _, window, cx| {
                if wall.customize.open {
                    wall.close_customize(window, cx);
                } else {
                    wall.open_customize(cx);
                }
            }))
            .child(icon("sliders", 14., ink))
            .when(roomy, |button| button.child("Customize"))
    }
}
