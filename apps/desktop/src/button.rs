//! The app's buttons, by what each is for ("Buttons" in
//! docs/internals/desktop.md): a capsule 36 points tall, its label at 14
//! points and 600. Play's ink is kept for starting music and nothing else,
//! so what a page is for wears the accent instead, and a button is never
//! drawn in a status's colour but to take something away.
//!
//! A blank page's button is `Secondary`, and the Devices tab uses every
//! role but Play. The Settings
//! window's push buttons are AppKit's (`settings/controls.rs`), and the
//! banner's Play and Shuffle are drawn over its picture (`banner.rs`).

use gpui::{
    Div, ElementId, FontWeight, InteractiveElement, ParentElement, Rgba, SharedString, Stateful,
    StatefulInteractiveElement, Styled, div, px,
};

use crate::theme;

/// What a button is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(
    dead_code,
    reason = "`Play` has no caller yet: the banner draws its own"
)]
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
    /// Takes something away: red words, edged in red, with a little red
    /// under the pointer.
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
        Role::Primary => plain(theme::acc(), theme::on_accent()),
        Role::Secondary => plain(theme::fill(), theme::ink()),
        Role::Quiet => Look {
            ground: None,
            words: theme::acc(),
            edge: None,
        },
        // No neutral ground: on a raised surface in the dark, the red words
        // would fall under 4.5:1 on one.
        Role::Destructive => Look {
            ground: None,
            words: theme::fail_text(),
            edge: Some(theme::fail()),
        },
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
            Role::Play => theme::hovered_solid(style),
            // Darker in the light and lighter in the dark, away from its
            // words, rather than fading toward the page.
            Role::Primary => style.bg(theme::primary_hover()),
            Role::Secondary => theme::hovered_on_fill(style),
            Role::Quiet => theme::hovered(style),
            Role::Destructive => style.bg(theme::fail_hover()),
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
        assert_eq!(secondary.ground, Some(theme::fill()));
        assert_eq!((quiet.ground, destructive.ground), (None, None));
        assert_eq!(destructive.words, theme::fail_text());
        for look in [play, primary, secondary, quiet] {
            assert_eq!(look.edge, None);
            assert!(look.words != theme::fail_text() && look.ground != Some(theme::fail()));
        }
    }
}
