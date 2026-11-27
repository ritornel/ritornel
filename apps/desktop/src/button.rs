//! The app's buttons, by what each is for ("Buttons" in
//! docs/internals/desktop.md): a capsule 36 points tall, its label at 14
//! points and 600. Play's ink is kept for starting music and nothing else,
//! so what a page is for wears the accent instead, and a button is never
//! drawn in a status's colour but to take something away.
//!
//! The Settings window's push buttons are AppKit's (`settings/controls.rs`),
//! and the banner's Play and Shuffle are drawn over its picture
//! (`banner.rs`).

use gpui::{
    Div, ElementId, FontWeight, InteractiveElement, ParentElement, Rgba, SharedString, Stateful,
    StatefulInteractiveElement, Styled, div, px,
};

use crate::theme;

/// What a button is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code, reason = "each is drawn where a page needs it")]
pub(crate) enum Role {
    /// Starts music on a record, an artist or a playlist: the ink.
    Play,
    /// What the page is for, at most one in view: the accent.
    Primary,
    /// Anything else, as Not now, Stop or Cancel: a neutral ground.
    Secondary,
    /// More of the same thing, at a row's end or in a sentence: the
    /// accent's words alone.
    Quiet,
    /// Takes something away: red words on a neutral ground, edged in red.
    Destructive,
}

/// How a button of a role is drawn.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Look {
    ground: Option<Rgba>,
    words: Rgba,
    edge: Option<Rgba>,
}

fn look(role: Role) -> Look {
    let plain = |ground, words| Look {
        ground: Some(ground),
        words,
        edge: None,
    };
    match role {
        Role::Play => plain(theme::ink(), theme::bg()),
        Role::Primary => plain(theme::acc(), on_accent()),
        Role::Secondary => plain(theme::fill(), theme::ink()),
        Role::Quiet => Look {
            ground: None,
            words: theme::acc(),
            edge: None,
        },
        Role::Destructive => Look {
            ground: Some(theme::fill()),
            words: theme::fail_text(),
            edge: Some(theme::fail()),
        },
    }
}

/// Words on the accent: the page's ground in the dark, where the accent is
/// light, and white in the light, where it is deep. The accent is worked out
/// to 4.5:1 on both (`accent::derive`).
fn on_accent() -> Rgba {
    if theme::dark() {
        theme::bg()
    } else {
        theme::raised()
    }
}

/// A button for `role`, saying `label`; the caller gives it its click.
pub(crate) fn button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    role: Role,
) -> Stateful<Div> {
    let Look {
        ground,
        words,
        edge,
    } = look(role);
    let mut button = div()
        .id(id)
        .h(px(36.))
        .px(px(18.))
        .rounded_full()
        .flex()
        .flex_none()
        .items_center()
        .whitespace_nowrap()
        .text_size(px(14.))
        .font_weight(FontWeight(600.))
        .text_color(words);
    if let Some(ground) = ground {
        button = button.bg(ground);
    }
    if let Some(edge) = edge {
        button = button.border_1().border_color(edge);
    }
    button
        .hover(move |style| match role {
            Role::Play | Role::Primary => theme::hovered_solid(style),
            Role::Secondary => theme::hovered_on_fill(style),
            Role::Quiet => theme::hovered(style),
            Role::Destructive => style.bg(theme::fail_tint()),
        })
        .active(theme::pressed)
        .child(label.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn play_and_primary_are_told_apart_and_only_taking_away_is_red() {
        let looks = [
            Role::Play,
            Role::Primary,
            Role::Secondary,
            Role::Quiet,
            Role::Destructive,
        ]
        .map(look);
        let [play, primary, secondary, quiet, destructive] = looks;
        assert_ne!(play.ground, primary.ground);
        assert_eq!(primary.ground, Some(theme::acc()));
        assert_eq!(secondary.ground, destructive.ground);
        assert_eq!(quiet.ground, None);
        assert_eq!(destructive.words, theme::fail_text());
        for look in [play, primary, secondary, quiet] {
            assert_eq!(look.edge, None);
            assert!(look.words != theme::fail_text() && look.ground != Some(theme::fail()));
        }
    }

    #[test]
    fn a_primary_button_s_words_read_on_the_accent() {
        let ratio = crate::accent::contrast(on_accent(), theme::acc());
        assert!(ratio >= crate::accent::CONTRAST, "{ratio:.2}");
    }
}
