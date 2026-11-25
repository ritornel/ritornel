//! A page with nothing on it yet says so, in the middle and quietly: a
//! heading, a sentence, perhaps a button and a quiet line of facts, 520
//! points wide at most, 14% of the window down.

use gpui::{
    AnyElement, App, ClickEvent, FontWeight, HighlightStyle, InteractiveElement, IntoElement,
    ParentElement, SharedString, StatefulInteractiveElement, Styled, StyledText, Window, div,
    prelude::FluentBuilder, px,
};

use crate::theme;

/// What a button on a blank page does.
pub(crate) type OnClick = Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// What a blank page says. `bold` is a part of `text` set in the ink (for
/// example a server's name).
pub(crate) struct Blank {
    pub title: SharedString,
    pub text: SharedString,
    pub bold: Option<SharedString>,
    pub button: Option<(SharedString, OnClick)>,
    pub facts: Option<SharedString>,
}

impl Blank {
    pub(crate) fn new(title: impl Into<SharedString>, text: impl Into<SharedString>) -> Self {
        Self {
            title: title.into(),
            text: text.into(),
            bold: None,
            button: None,
            facts: None,
        }
    }

    /// Draws it in a page area `height` points tall.
    pub(crate) fn render(self, height: f32) -> AnyElement {
        let text = self.text.to_string();
        let highlights: Vec<(std::ops::Range<usize>, HighlightStyle)> = self
            .bold
            .as_ref()
            .and_then(|bold| text.find(bold.as_ref()).map(|at| at..at + bold.len()))
            .map(|range| {
                vec![(
                    range,
                    HighlightStyle {
                        color: Some(theme::ink().into()),
                        font_weight: Some(FontWeight(600.)),
                        ..HighlightStyle::default()
                    },
                )]
            })
            .unwrap_or_default();
        div()
            .w_full()
            .pt(px(height * 0.14))
            .flex()
            .justify_center()
            .child(
                div()
                    .max_w(px(520.))
                    .flex()
                    .flex_col()
                    .items_center()
                    .text_center()
                    .child(
                        div()
                            .mb(px(10.))
                            .text_size(px(30.))
                            .line_height(px(36.))
                            .font_weight(FontWeight::BOLD)
                            .child(self.title),
                    )
                    .child(
                        div()
                            .text_size(px(15.))
                            .line_height(gpui::relative(1.5))
                            .text_color(theme::dim())
                            .child(StyledText::new(self.text).with_highlights(highlights)),
                    )
                    .when_some(self.button, |blank, (label, on_click)| {
                        blank.child(
                            div().mt(px(22.)).flex().justify_center().child(
                                div()
                                    .id("blank-button")
                                    .h(px(36.))
                                    .px(px(18.))
                                    .rounded_full()
                                    .flex()
                                    .items_center()
                                    .bg(theme::fill())
                                    .hover(theme::hovered_on_fill)
                                    .active(theme::pressed)
                                    .text_size(px(14.))
                                    .font_weight(FontWeight(600.))
                                    .on_click(on_click)
                                    .child(label),
                            ),
                        )
                    })
                    .when_some(self.facts, |blank, facts| {
                        blank.child(
                            div()
                                .mt(px(22.))
                                .text_size(px(12.5))
                                .text_color(theme::faint())
                                .child(facts),
                        )
                    }),
            )
            .into_any_element()
    }
}

/// A tab whose page is not built yet: its name, and what will be there.
pub(crate) fn not_built(tab: crate::stage::Tab, height: f32) -> AnyElement {
    use crate::stage::Tab;
    let (title, text) = match tab {
        Tab::Today => (
            "Today is not built yet.",
            "It will have something to say each day: what you were playing years ago, a record worth going back to.",
        ),
        Tab::Playlists => (
            "Playlists are not built yet.",
            "Your playlists will be here.",
        ),
        Tab::Devices => (
            "Devices are not built yet.",
            "The players you sync music to will be here.",
        ),
        // These have pages of their own.
        Tab::Library | Tab::History => ("", ""),
    };
    div()
        .relative()
        .size_full()
        .child(
            div()
                .absolute()
                .top(px(20.))
                .left(px(40.))
                .child(crate::tracked::tracked(
                    tab.label(),
                    30.,
                    42.,
                    FontWeight::BOLD,
                    theme::ink(),
                    -0.022,
                )),
        )
        .child(
            div()
                .absolute()
                .top(px(20. + 42. + 22.))
                .left_0()
                .right_0()
                .child(Blank::new(title, text).render(height)),
        )
        .into_any_element()
}
