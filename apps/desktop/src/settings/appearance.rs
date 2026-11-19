//! Appearance (`settings/Appearance.svelte`): how the app looks on this
//! computer. It is kept here and not on the server, and the main window
//! follows a change at once (`remember::Looks`).

use gpui::{App, InteractiveElement, IntoElement, ParentElement, Styled, div};

use super::controls::{boxed, group, grow, heading, sub_wraps, switch_row};
use crate::remember;

pub(super) fn render(active: bool, cx: &App) -> impl IntoElement {
    let letters = remember::looks(cx).letters;
    div()
        .id("appearance")
        .flex()
        .flex_col()
        .child(group(true).child(heading("Library")).child(boxed(vec![
            switch_row(
                "letters",
                grow().child("Large letters beside the walls").child(sub_wraps(
                    "When off, a small heading marks each letter and the covers fill the margin.",
                )),
                letters,
                true,
                active,
                move |_, _, cx| remember::set_letters(!letters, cx),
            ),
        ])))
}
