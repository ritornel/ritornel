//! Last.fm: the account the server is signed in to, scrobbling, the import
//! of the history, and where you listened, which dates what is imported. The
//! server holds the key, the secret, and the session; none of them is ever
//! shown here.

use gpui::{
    Div, Entity, FocusHandle, FontWeight, InteractiveElement, IntoElement, ParentElement, Styled,
    div, prelude::FluentBuilder, px,
};

use super::act;
use super::controls::{
    MONO, actions, boxed, chosen, clickable, date_field, edit, group, grow, heading, label_value,
    note, pop_up, problem, push, row, spinner, sub, switch_row,
};
use super::model::{Busy, Group, Model, SigningIn};
use super::words::{Clock, all_zones, last_import, next_import, when, zone_name, zone_span};
use crate::theme;
use crate::words::thousands;

/// Text that wraps at its spaces, with the words in `mono` set in the
/// monospaced face at 11 points (such as `lastfm.env` inside a sentence).
fn sentence(text: &str, mono: &str) -> Div {
    div()
        .flex()
        .flex_wrap()
        .items_baseline()
        .children(text.split(' ').map(|word| {
            let tail = word.trim_end_matches(['.', ',']);
            let (word, mark) = word.split_at(tail.len());
            let set = if word == mono {
                div()
                    .flex()
                    .items_baseline()
                    .child(
                        div()
                            .font_family(MONO)
                            .text_size(px(11.))
                            .child(word.to_owned()),
                    )
                    .child(mark.to_owned())
            } else {
                div().child(format!("{word}{mark}"))
            };
            set.mr(px(3.2))
        }))
}

pub(super) fn render(
    model: &Model,
    entity: &Entity<Model>,
    clock: &Clock,
    active: bool,
    zones_focus: &FocusHandle,
    focused: bool,
) -> impl IntoElement {
    let base = div().id("lastfm").flex().flex_col();
    let Some(status) = &model.lastfm else {
        return base.child(div().text_color(theme::dim()).child("Reading…"));
    };
    if !status.available {
        return base.child(group(true).child(boxed(vec![
            row().child("This server was built without Last.fm."),
        ])));
    }
    let busy = |what: Busy| model.busy.contains(&what);
    let data_dir = model.data_dir.as_ref().map(|dir| dir.display().to_string());
    if !status.configured {
        let reveal = model.data_dir.clone();
        return base.child(
            group(true).child(heading("Last.fm is not set up")).child(boxed(vec![
                row().child(
                    grow()
                        .child(sentence(
                            "Ritornel needs a Last.fm API key and secret before it can use your account. Put them in a file named lastfm.env in the data folder, readable only by you, then quit Ritornel and open it again.",
                            "lastfm.env",
                        ))
                        .child(
                            div()
                                .mt(px(8.))
                                .px(px(10.))
                                .py(px(8.))
                                .rounded(px(6.))
                                .bg(theme::fill())
                                .font_family(MONO)
                                .text_size(px(11.))
                                // Use the monospaced face's own tighter line
                                // height rather than the body's 1.4 ratio.
                                .line_height(px(13.))
                                .child("SCROBL_API_KEY=…")
                                .child("SCROBL_API_SECRET=…"),
                        ),
                ),
                row()
                    .child(grow().child(sub(data_dir.clone().unwrap_or_else(|| {
                        "The data folder could not be worked out.".to_owned()
                    }))))
                    .child(actions().child(push(
                        "show-data",
                        crate::platform::SHOW_FOLDER,
                        reveal.is_some(),
                        move |_, _, cx| {
                            if let Some(dir) = &reveal {
                                cx.reveal_path(dir);
                            }
                        },
                    ))),
            ])),
        );
    }
    let signed_in = status.account.is_some() && !status.sign_in_again;
    let pending = status
        .deliveries
        .iter()
        .find(|count| count.state == "pending")
        .map_or(0, |count| count.count);

    // ---- Account
    let account = match &model.signing_in {
        SigningIn::Waiting { .. } => row()
            .child(spinner("signing-in"))
            .child(
                grow()
                    .child("Waiting for you to allow Ritornel on Last.fm…")
                    .child(sub("Allow access on the page that opened in your browser.")),
            )
            .child(
                actions()
                    .child(push(
                        "open-again",
                        "Open Again",
                        true,
                        act(entity, |model, _, cx| model.open_approval_again(cx)),
                    ))
                    .child(push(
                        "cancel-sign-in",
                        "Cancel",
                        true,
                        act(entity, |model, _, cx| model.cancel_sign_in(cx)),
                    )),
            ),
        _ if signed_in => row()
            .child(
                grow().flex().child("Signed in as ").child(
                    div()
                        .font_weight(FontWeight(700.))
                        .child(status.account.clone().unwrap_or_default()),
                ),
            )
            .child(actions().child(push(
                "sign-out",
                "Sign Out",
                !busy(Busy::SigningOut),
                act(entity, |model, _, cx| model.sign_out(cx)),
            ))),
        _ => row()
            .child(grow().child(if status.sign_in_again {
                "Last.fm turned the session down. Sign in again."
            } else {
                "Not signed in"
            }))
            .child(actions().child(push(
                "sign-in",
                "Sign In…",
                true,
                act(entity, |model, _, cx| model.sign_in(cx)),
            ))),
    };
    let failed = match &model.signing_in {
        SigningIn::Failed { message } => Some(problem(message.clone())),
        _ => None,
    };
    let account = group(true)
        .child(heading("Account"))
        .child(boxed(vec![account]))
        .children(failed)
        .children(model.problems.get(&Group::Account).cloned().map(problem));

    // ---- Scrobbling
    let on = status.scrobbling_since.is_some();
    let scrobbling_said = match status.scrobbling_since {
        Some(since) => format!(
            "On since {}{}",
            when(since, clock),
            if pending > 0 {
                format!(", {} waiting to send", thousands(pending))
            } else {
                String::new()
            }
        ),
        None => "Off".to_owned(),
    };
    let scrobbling = group(false)
        .child(heading("Scrobbling"))
        .child(boxed(vec![
            switch_row(
                "scrobbling",
                grow().child("Scrobble what you play").child(sub(scrobbling_said)),
                on,
                signed_in && !busy(Busy::Scrobbling),
                active,
                act(entity, move |model, _, cx| model.set_scrobbling(!on, cx)),
            ),
        ]))
        .children(model.problems.get(&Group::Scrobbling).cloned().map(problem))
        .children(status.waiting_for_import_since.map(|since| {
            note(format!(
                "Nothing has been sent since {}, when this library was restored from a backup: \
                 Last.fm may already have plays sent after it was taken. {}",
                when(since, clock),
                if status.zones_set {
                    "The next import checks, and the plays waiting go once it has. Import Now \
                     runs it straight away."
                } else {
                    "An import checks, and the plays waiting go once it has: add where you \
                     listened below, then Import Now."
                }
            ))
        }))
        .child(note(
            "Sends each play to Last.fm from the moment you turn this on, and shows there what is playing now. Plays from before are never sent.",
        ));

    // ---- History
    let imported = status.imported;
    let history = group(false)
        .child(heading("History"))
        .child(boxed(vec![
            label_value("Last import", last_import(&status.import, clock)),
            label_value("Next import", next_import(&status.import, clock)),
            row()
                .child(grow().child(div().text_color(theme::dim()).child(format!(
                    "{} {} imported",
                    thousands(imported),
                    if imported == 1 { "play" } else { "plays" }
                ))))
                .child(actions().child(push(
                    "import-now",
                    "Import Now",
                    signed_in && status.zones_set && !status.import.running && !busy(Busy::Syncing),
                    act(entity, |model, _, cx| model.sync_now(cx)),
                ))),
        ]))
        .children(model.problems.get(&Group::Import).cloned().map(problem));

    // ---- Where you listened
    let zones = model.zones.as_deref().unwrap_or_default();
    let mut rows: Vec<Div> = zones
        .iter()
        .enumerate()
        .map(|(index, period)| {
            let is_chosen = model.chosen == Some(index);
            let model = entity.clone();
            let focus = zones_focus.clone();
            // In a plain row of its own, for the line above it.
            let option = row()
                .min_h(px(34.))
                .when(is_chosen, |row| {
                    row.bg(if focused {
                        // `color-mix(in srgb, var(--acc) 26%, transparent)`.
                        gpui::Rgba {
                            a: 0.26,
                            ..theme::acc()
                        }
                    } else {
                        chosen()
                    })
                    .when(index == 0, |row| row.rounded_t(px(7.)))
                })
                .child(div().flex_1().child(zone_name(&period.zone)))
                .child(
                    div()
                        .text_color(theme::dim())
                        .text_right()
                        .child(zone_span(period, clock.style)),
                );
            clickable(option, ("zone", index), move |_, window, cx| {
                window.focus(&focus, cx);
                model.update(cx, |model, cx| {
                    model.chosen = if model.chosen == Some(index) {
                        None
                    } else {
                        Some(index)
                    };
                    cx.notify();
                });
            })
        })
        .collect();
    if zones.is_empty() {
        rows.push(
            row()
                .text_color(theme::dim())
                .child("No zones yet. Imported plays wait for one."),
        );
    }
    if let Some(adding) = &model.adding {
        let zone = adding.zone.clone();
        let choose = entity.clone();
        rows.push(
            row()
                .flex_wrap()
                .child(pop_up(
                    "zone",
                    zone_name(&adding.zone),
                    move |event, window, cx| {
                        let handle = window.window_handle();
                        let at = event.position();
                        let model = choose.clone();
                        let zone = zone.clone();
                        cx.spawn(async move |cx| {
                            let zones = all_zones();
                            let names = zones.iter().map(|zone| zone_name(zone)).collect();
                            let checked = zones.iter().position(|each| *each == zone);
                            let chosen = crate::menu::choose(handle, names, checked, at, cx).await;
                            if let Some(zone) = chosen.and_then(|index| zones.get(index).cloned()) {
                                model.update(cx, |model, cx| {
                                    if let Some(adding) = &mut model.adding {
                                        adding.zone = zone;
                                    }
                                    cx.notify();
                                });
                            }
                        })
                        .detach();
                    },
                ))
                .child(div().text_color(theme::dim()).child("from"))
                .child(date_field(&adding.from))
                .child(div().flex_1())
                .child(push(
                    "cancel-zone",
                    "Cancel",
                    true,
                    act(entity, |model, _, cx| {
                        model.adding = None;
                        cx.notify();
                    }),
                ))
                .child(push(
                    "add-zone",
                    "Add",
                    !busy(Busy::Zones),
                    act(entity, |model, _, cx| model.add(cx)),
                )),
        );
    }
    let start = clock.clone();
    rows.push(
        div()
            .flex()
            .items_center()
            .gap(px(2.))
            .px(px(6.))
            .py(px(3.))
            .bg(theme::fill())
            .rounded_b(px(7.))
            .child(edit(
                "add-a-zone",
                "+",
                model.adding.is_none(),
                act(entity, move |model, _, cx| model.start_adding(&start, cx)),
            ))
            .child(edit(
                "remove-the-zone",
                "−",
                model.chosen.is_some() && !busy(Busy::Zones),
                act(entity, |model, _, cx| model.remove(cx)),
            ))
            .when(busy(Busy::Zones), |foot| {
                foot.child(div().ml(px(6.)).child(spinner("zones")))
            }),
    );
    let removing = entity.clone();
    let list = boxed(rows)
        .id("zones")
        .track_focus(zones_focus)
        .on_key_down(move |event, _, cx| {
            if matches!(event.keystroke.key.as_str(), "backspace" | "delete") {
                removing.update(cx, |model, cx| {
                    if model.chosen.is_some() {
                        model.remove(cx);
                    }
                });
                cx.stop_propagation();
            }
        });
    let listened = group(false)
        .child(heading("Where you listened"))
        .child(list)
        .children(model.problems.get(&Group::Zones).cloned().map(problem))
        .child(note(
            "Each play Last.fm keeps is dated in the zone you were in then. Add one for every place you lived, from the day you moved; leave the date empty for the first.",
        ));

    base.child(account)
        .child(scrobbling)
        .child(history)
        .child(listened)
}
