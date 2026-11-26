//! Appearance: how the app looks on this computer. It is kept here and not
//! on the server, and the main window follows a change at once
//! (`remember::Looks`).

use gpui::{App, InteractiveElement, IntoElement, ParentElement, Styled, div};

use super::controls::{boxed, group, grow, heading, pop_up, row, sub_wraps, switch_row};
use crate::places::VolumeControl;
use crate::remember;

pub(super) fn render(active: bool, cx: &App) -> impl IntoElement {
    let looks = remember::looks(cx);
    let letters = looks.letters;
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
        .child(group(false).child(heading("Player bar")).child(boxed(vec![
            row()
                .child(grow().child("Volume slider").child(sub_wraps(
                    "Shown for another place you play on. Music here keeps the system’s volume \
                     unless you choose Always.",
                )))
                .child(pop_up(
                    "volume-control",
                    looks.volume.label(),
                    move |event, window, cx| {
                        let handle = window.window_handle();
                        let at = event.position();
                        let checked = VolumeControl::ALL
                            .iter()
                            .position(|each| *each == remember::looks(cx).volume);
                        cx.spawn(async move |cx| {
                            let labels = VolumeControl::ALL
                                .iter()
                                .map(|each| each.label().to_owned())
                                .collect();
                            let chosen = crate::menu::choose(handle, labels, checked, at, cx).await;
                            if let Some(volume) =
                                chosen.and_then(|index| VolumeControl::ALL.get(index).copied())
                            {
                                cx.update(|cx| remember::set_volume_control(volume, cx));
                            }
                        })
                        .detach();
                    },
                )),
        ])))
}
