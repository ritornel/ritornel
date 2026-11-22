//! The library (`settings/Library.svelte`): whether the listening record
//! advances, the music folders it reads and their scans, the folder of
//! artist pictures, and whether the server watches them for changes. Removing a folder waits on the server: it has
//! no call that forgets one yet.

use gpui::{
    App, Entity, InteractiveElement, IntoElement, ParentElement, Styled, div,
    prelude::FluentBuilder, px,
};
use ritornel_client::record;
use ritornel_contract::{RecordProblem, RecordStatus, WatchState, WatchedFolder, WatchedKind};

use super::act;
use super::controls::{
    actions, boxed, folder_icon, group, grow, heading, note, problem, progress, push, row, spinner,
    sub, sub_wraps, switch_row,
};
use super::model::{Busy, Group, Model};
use super::words::{
    Clock, is_ended, last_auto, pictures_found, scan_ended, scan_progress, unbroken_paths,
    watch_state, when,
};
use crate::theme;
use crate::words::thousands;

/// How the server watches a folder, at the right of its row, while it watches.
fn watching(folder: Option<&WatchedFolder>, on: bool, clock: &Clock) -> Option<impl IntoElement> {
    let folder = folder.filter(|_| on)?;
    Some(
        div()
            .flex_none()
            .text_color(theme::dim())
            .text_right()
            .child(
                div()
                    .when(folder.state == WatchState::Offline, |state| {
                        state.text_color(super::controls::warn())
                    })
                    .child(watch_state(folder)),
            )
            .children(last_auto(folder, clock).map(sub)),
    )
}

/// Whether the listening record advances: a quiet line while it does, and
/// a row for each thing that stops it, with since when, while it does not.
/// Music can go on playing all the while, which is why this is said.
fn record(
    record: &RecordStatus,
    model: &Model,
    entity: &Entity<Model>,
    clock: &Clock,
) -> impl IntoElement {
    let mut rows = Vec::new();
    if record.problems.is_empty() {
        rows.push(
            row()
                .text_color(theme::dim())
                .child("Every listen is being recorded"),
        );
    }
    let dismissing = model.busy.contains(&Busy::Dismissing);
    // The warnings, then the notes: what no one can act on, said quietly.
    let all: Vec<(&RecordProblem, bool)> = record
        .problems
        .iter()
        .map(|problem| (problem, true))
        .chain(record.notes.iter().map(|note| (note, false)))
        .collect();
    // One button sets every such scrobble aside, so one is shown.
    let dismiss_at = all.iter().position(|(problem, _)| problem.dismissable);
    for (n, (problem, warning)) in all.iter().enumerate() {
        // What is wrong or what happened, since when and why, and what to
        // do or what Ritornel does, in the words `ritornelctl status` says
        // too.
        let since = format!("Since {}", when(problem.since, clock));
        let said = match &problem.detail {
            Some(detail) => format!("{since}: {detail}"),
            None => since,
        };
        let then = record::then(problem, WORDS);
        rows.push(
            row()
                .child(
                    grow()
                        .child(
                            div()
                                .when(*warning, |what| what.text_color(super::controls::warn()))
                                .child(record::what(problem)),
                        )
                        .child(sub_wraps(said))
                        .child(sub_wraps(then)),
                )
                .when(dismiss_at == Some(n), |row| {
                    row.child(actions().child(push(
                        "dismiss-record",
                        "Dismiss",
                        !dismissing,
                        act(entity, |model, _, cx| model.dismiss_record(cx)),
                    )))
                }),
        );
    }
    let said = match (record.problems.is_empty(), dismiss_at.is_some()) {
        (false, true) => Some(
            "Music plays on while the record has stopped. One Dismiss sets every scrobble row aside.",
        ),
        (false, false) => Some("Music plays on while the record has stopped."),
        (true, true) => Some("One Dismiss sets every scrobble row aside."),
        (true, false) => None,
    };
    group(true)
        .child(heading("Listening record"))
        .child(boxed(rows))
        .children(model.problems.get(&Group::Record).cloned().map(problem))
        .children(said.map(note))
}

/// How the app says what the person does: its own button, and quitting
/// it, which stops the server it started.
const WORDS: record::Words<'static> = record::Words {
    dismiss: "Dismiss",
    restart: "quit and reopen Ritornel",
    sign_in: "in Settings → Last.fm",
};

pub(super) fn render(
    model: &Model,
    entity: &Entity<Model>,
    clock: &Clock,
    active: bool,
    _cx: &App,
) -> impl IntoElement {
    let sources = model.sources.as_deref().unwrap_or_default();
    let here = sources
        .iter()
        .filter(|source| source.path.is_some())
        .count();
    let running: Vec<_> = model.scans.values().filter(|job| !is_ended(job)).collect();
    // The scan that ended last, of any folder.
    let last = model
        .scans
        .values()
        .filter(|job| job.finished_at.is_some())
        .max_by_key(|job| job.finished_at);
    let total: u64 = running.iter().filter_map(|job| job.total).sum();
    let done: u64 = running.iter().map(|job| job.done).sum();
    let known = running.iter().all(|job| job.total.is_some()) && total > 0;
    let picture = model.picture_folder();
    let pictures = model.pictures.as_ref();
    let portraits = picture
        .as_ref()
        .and_then(|(name, _)| {
            pictures?
                .folders
                .iter()
                .find(|folder| &folder.name == name)
                .map(|folder| folder.portraits)
        })
        .unwrap_or(0);
    let watch = model
        .status
        .as_ref()
        .and_then(|status| status.watch.as_ref());
    let watch_on = watch.is_some_and(|watch| watch.on);
    let watched = |name: &str, kind: WatchedKind| {
        watch.and_then(|watch| {
            watch
                .folders
                .iter()
                .find(|folder| folder.name == name && folder.kind == kind)
        })
    };
    let busy = |what: Busy| model.busy.contains(&what);

    // ---- Music folders
    let mut rows = Vec::new();
    if model.sources.is_none() {
        rows.push(row().text_color(theme::dim()).child("Reading…"));
    } else if sources.is_empty() {
        rows.push(
            row()
                .text_color(theme::dim())
                .child("No folders yet. Add the folder your music is in."),
        );
    }
    for source in sources {
        rows.push(
            row()
                .child(folder_icon())
                .child(
                    grow().child(source.name.clone()).child(sub(source
                        .path
                        .clone()
                        .unwrap_or_else(|| "On another server".to_owned()))),
                )
                .children(watching(
                    watched(&source.name, WatchedKind::Music),
                    watch_on,
                    clock,
                )),
        );
    }
    let status = if !running.is_empty() {
        let said = match running.as_slice() {
            [one] => scan_progress(one),
            many => format!("Scanning {} folders", many.len()),
        };
        Some(
            div()
                .child(progress("scanning", known.then_some((done, total))))
                .child(sub(said))
                .into_any_element(),
        )
    } else if let Some(last) = last {
        // A scan's failure can be long: two lines of it.
        Some(
            div()
                .text_color(theme::dim())
                .line_clamp(2)
                .text_ellipsis()
                .child(unbroken_paths(&scan_ended(last, clock)))
                .into_any_element(),
        )
    } else if here > 0 {
        Some(
            div()
                .text_color(theme::dim())
                .child("Not scanned yet")
                .into_any_element(),
        )
    } else {
        None
    };
    let scan_button = if running.is_empty() {
        push(
            "scan",
            "Scan Now",
            here > 0,
            act(entity, |model, _, cx| model.scan_all(cx)),
        )
        .into_any_element()
    } else {
        push(
            "stop",
            "Stop",
            true,
            act(entity, |model, _, cx| model.stop_scans(cx)),
        )
        .into_any_element()
    };
    rows.push(
        row().child(grow().children(status)).child(
            actions()
                .child(push(
                    "add-folder",
                    "Add Folder…",
                    !busy(Busy::Adding),
                    act(entity, |model, window, cx| {
                        model.choose_folder(window.window_handle(), cx);
                    }),
                ))
                .child(scan_button),
        ),
    );
    if let Some(watch) = watch {
        let enabled = !busy(Busy::Watching);
        let to = !watch.enabled;
        let toggle = act(entity, move |model, _, cx| model.set_watching(to, cx));
        rows.push(switch_row(
            "watching",
            grow().child("Watch for changes").child(sub_wraps(
                "Picks up added, changed and removed files, and new artist pictures, by itself. A disk that is unplugged is never taken as deleted.",
            )),
            watch.enabled,
            enabled,
            active,
            toggle,
        ));
    }
    let record = model
        .status
        .as_ref()
        .and_then(|status| status.record.as_ref())
        .map(|found| record(found, model, entity, clock));
    let music = group(record.is_none())
        .child(heading("Music folders"))
        .child(boxed(rows))
        .when_some(
            watch.filter(|watch| watch.enabled && !watch.on),
            |group, watch| {
                group.children(
                    watch
                        .off
                        .as_ref()
                        .map(|off| super::controls::note(format!("Not watching now: {off}."))),
                )
            },
        )
        .children(model.problems.get(&Group::Library).cloned().map(problem))
        .child(note(
            "Ritornel reads these folders and never changes the files in them.",
        ));

    // ---- Artist pictures
    let mut rows = vec![
        row()
            .child(folder_icon())
            .child(match &picture {
                Some((name, path)) => grow().child(name.clone()).child(sub(path.clone())),
                None => grow().child(div().text_color(theme::dim()).child(if pictures.is_none() {
                    "Reading…"
                } else {
                    "No picture folder"
                })),
            })
            .children(picture.as_ref().and_then(|(name, _)| {
                watching(watched(name, WatchedKind::Pictures), watch_on, clock)
            }))
            .child(actions().child(push(
                "choose-pictures",
                "Choose…",
                !busy(Busy::Pictures),
                act(entity, |model, window, cx| {
                    model.choose_pictures(window.window_handle(), cx);
                }),
            ))),
    ];
    if picture.is_some() {
        let said = if busy(Busy::Pictures) {
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .text_color(theme::dim())
                .child(spinner("importing"))
                .child("Importing pictures…")
        } else {
            // An import started on its own keeps no counts: what the folder
            // holds now is said instead.
            let counted = pictures
                .and_then(|pictures| pictures.last_import.as_ref())
                .map_or_else(
                    || {
                        format!(
                            "{} {} a picture",
                            thousands(portraits),
                            if portraits == 1 {
                                "artist has"
                            } else {
                                "artists have"
                            }
                        )
                    },
                    pictures_found,
                );
            div().child(counted).children(
                pictures
                    .and_then(|pictures| pictures.last_import_at)
                    .map(|at| sub(format!("Imported {}", when(at, clock)))),
            )
        };
        rows.push(row().child(grow().child(said)).child(actions().child(push(
            "import-pictures",
            "Import Now",
            !busy(Busy::Pictures),
            act(entity, |model, _, cx| model.import_pictures(None, cx)),
        ))));
    }
    let artists = group(false)
        .child(heading("Artist pictures"))
        .child(boxed(rows))
        .children(model.problems.get(&Group::Pictures).cloned().map(problem))
        .child(note(
            "A folder of portraits kept apart from the music, one folder for each artist. Import again after it changes.",
        ));

    div()
        .id("library")
        .flex()
        .flex_col()
        .children(record)
        .child(music)
        .child(artists)
}
