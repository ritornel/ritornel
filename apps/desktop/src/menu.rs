//! The system's own context menu on a tile (docs/design/ui.md, "Context
//! menus"): a record's, Play Next, Play Last | Play, Shuffle | Go to
//! Artist, each played through the player as the real `lib/menus.ts` plays
//! it. While it is open the tile wears its ring.
//!
//! GPUI has no context menu of the system's (Zed draws its own); the
//! system's is `platform::pop_up_menu`.
//!
//! The other menus are `lib/menus.ts`'s, built as `lib/menu.ts` builds
//! them: a song's, an entry of Up next's, the song playing's, a record's
//! and an artist's. Each
//! place opens its own with `popup`, and the thing it is on is marked while
//! it is open, the mark fading in and out (`Marked`).

use std::sync::Arc;

use gpui::{App, Context, Global, Pixels, Point, Window};
use ritornel_contract::{AlbumRef, Credit, Id, PlayContext, PlaySource, QueuePlace, TrackSummary};

use crate::platform::MenuItem;

use crate::player::{Doing, Many};
use crate::wall::{Subject, TileRow, Wall};

pub(crate) const RECORD: &[Option<&str>] = &[
    Some("Play Next"),
    Some("Play Last"),
    None,
    Some("Play"),
    Some("Shuffle"),
    None,
    Some("Go to Artist"),
];

/// An artist's menu (`lib/menus.ts`, `artistMenu`).
pub(crate) const ARTIST: &[Option<&str>] = &[
    Some("Play Next"),
    Some("Play Last"),
    None,
    Some("Play"),
    Some("Shuffle"),
];

/// Does what a tile's menu item says.
pub(crate) fn act(wall: &mut Wall, row: &TileRow, item: &str, cx: &mut Context<Wall>) {
    if item == "Go to Artist" {
        if let Some(artist) = row.artist_id {
            cx.emit(crate::wall::OpenArtist {
                artist,
                name: row.artist.clone(),
            });
        }
        return;
    }
    let Some(doing) = Doing::named(item) else {
        return;
    };
    let many = match row.subject {
        Subject::Album(album) => Many::Record {
            album,
            name: row.title.to_string(),
        },
        Subject::Artist(id) => Many::Artist {
            id,
            name: row.title.to_string(),
        },
    };
    wall.player
        .update(cx, |player, cx| player.many(many, doing, cx));
}

pub(crate) fn record_menu(
    row: Arc<TileRow>,
    at: Point<Pixels>,
    window: &mut Window,
    cx: &mut Context<Wall>,
) {
    let wall = cx.entity().downgrade();
    let handle = window.window_handle();
    let items = if row.round { ARTIST } else { RECORD };
    // After the frame that draws the ring.
    window.on_next_frame(move |_, cx| {
        cx.spawn(async move |cx| {
            let chosen = crate::platform::pop_up_menu(handle, plain(items), None, at, cx).await;
            let item = chosen.and_then(|index| items.iter().flatten().nth(index).copied());
            let _ = wall.update(cx, |wall, cx| {
                wall.menu_open = None;
                cx.notify();
                if let Some(item) = item {
                    act(wall, &row, item, cx);
                }
            });
        })
        .detach();
    });
}

/// The system's pop-up of `items` at `at` in the window, `checked` ticked
/// and under the pointer, as a pop-up button's menu opens: the index chosen,
/// or `None`. Not on Windows or Linux yet.
pub(crate) async fn choose(
    window: gpui::AnyWindowHandle,
    items: Vec<String>,
    checked: Option<usize>,
    at: Point<Pixels>,
    cx: &mut gpui::AsyncApp,
) -> Option<usize> {
    let items = items
        .into_iter()
        .enumerate()
        .map(|(index, title)| MenuItem::Action {
            title,
            enabled: true,
            checked: checked == Some(index),
        })
        .collect();
    crate::platform::pop_up_menu(window, items, checked, at, cx).await
}

/// A tile's items as the system's menu takes them.
fn plain(items: &[Option<&str>]) -> Vec<MenuItem> {
    items
        .iter()
        .map(|item| match item {
            None => MenuItem::Separator,
            Some(title) => MenuItem::Action {
                title: (*title).to_owned(),
                enabled: true,
                checked: false,
            },
        })
        .collect()
}

/// What an item does, once chosen and the menu has closed.
pub(crate) type Run = Box<dyn FnOnce(&mut Window, &mut App)>;

/// One line of a menu (`MenuEntry`).
pub(crate) enum Entry {
    Separator,
    Action {
        text: String,
        enabled: bool,
        checked: bool,
        run: Run,
    },
    Sub {
        text: String,
        items: Vec<Option<Entry>>,
    },
}

/// An item that does `run`.
pub(crate) fn item(text: &str, run: impl FnOnce(&mut Window, &mut App) + 'static) -> Option<Entry> {
    Some(Entry::Action {
        text: text.to_owned(),
        enabled: true,
        checked: false,
        run: Box::new(run),
    })
}

const SEPARATOR: Option<Entry> = Some(Entry::Separator);

/// The entries that apply, with no separator at either end or two together
/// (`tidy`).
fn tidy(entries: Vec<Option<Entry>>) -> Vec<Entry> {
    let mut kept: Vec<Entry> = Vec::new();
    for entry in entries.into_iter().flatten() {
        if matches!(entry, Entry::Separator)
            && kept
                .last()
                .is_none_or(|last| matches!(last, Entry::Separator))
        {
            continue;
        }
        kept.push(entry);
    }
    if kept
        .last()
        .is_some_and(|last| matches!(last, Entry::Separator))
    {
        kept.pop();
    }
    kept
}

/// The system's items for `entries`, and what each action does, in the
/// order the menu numbers them.
fn native(entries: Vec<Entry>, runs: &mut Vec<Option<Run>>) -> Vec<MenuItem> {
    entries
        .into_iter()
        .map(|entry| match entry {
            Entry::Separator => MenuItem::Separator,
            Entry::Action {
                text,
                enabled,
                checked,
                run,
            } => {
                runs.push(Some(run));
                MenuItem::Action {
                    title: text,
                    enabled,
                    checked,
                }
            }
            Entry::Sub { text, items } => MenuItem::Submenu {
                title: text,
                items: native(tidy(items), runs),
            },
        })
        .collect()
}

/// The thing whose menu is open, or was last: its mark comes in while the
/// menu is open and fades out after (`data-menu-open`, `--menu-in`,
/// `--menu-out`).
#[derive(Default)]
pub(crate) struct Marked {
    key: Option<String>,
    since: Option<std::time::Instant>,
    open: bool,
}

impl Global for Marked {}

const MARK_IN_MS: f32 = 200.;
const MARK_OUT_MS: f32 = 250.;

/// How strongly the thing `key` names is marked now, from 0 to 1. While the
/// mark moves, the window draws again.
pub(crate) fn mark(key: &str, window: &mut Window, cx: &App) -> f32 {
    marked(window, cx)
        .filter(|(marked, _)| marked == key)
        .map_or(0., |(_, shown)| shown)
}

/// The thing marked now and how strongly, if any is.
pub(crate) fn marked(window: &mut Window, cx: &App) -> Option<(String, f32)> {
    let marked = cx.try_global::<Marked>()?;
    let (key, since) = (marked.key.clone()?, marked.since?);
    if crate::platform::reduce_motion() {
        return marked.open.then_some((key, 1.));
    }
    let ms = crate::moves::since(since, std::time::Instant::now());
    let (t, shown) = if marked.open {
        let t = crate::moves::progress(ms, MARK_IN_MS);
        (t, crate::moves::ease_out(t))
    } else {
        let t = crate::moves::progress(ms, MARK_OUT_MS);
        (t, 1. - crate::moves::ease_out(t))
    };
    if t < 1. {
        window.request_animation_frame();
    }
    (shown > 0.).then_some((key, shown))
}

/// The menu's fill, `--menu-fill`, as strong as the mark is.
pub(crate) fn fill(shown: f32) -> gpui::Rgba {
    let fill = crate::theme::fill2();
    gpui::Rgba {
        a: fill.a * shown,
        ..fill
    }
}

/// Pops up `entries` at `at` in the window, the thing `key` names marked
/// while it is open (`popup` and `menu`). Opened after the frame that
/// draws the mark; the item chosen runs once it has closed.
pub(crate) fn popup(
    key: String,
    entries: Vec<Option<Entry>>,
    at: Point<Pixels>,
    window: &mut Window,
    cx: &mut App,
) {
    let entries = tidy(entries);
    if entries.is_empty() {
        return;
    }
    let mut runs = Vec::new();
    let items = native(entries, &mut runs);
    crate::debug(format_args!("menu: {key}: {}", described(&items)));
    cx.set_global(Marked {
        key: Some(key),
        since: Some(std::time::Instant::now()),
        open: true,
    });
    window.refresh();
    let handle = window.window_handle();
    window.on_next_frame(move |_, cx| {
        cx.spawn(async move |cx| {
            let chosen = crate::platform::pop_up_menu(handle, items, None, at, cx).await;
            let run = chosen
                .and_then(|index| runs.get_mut(index))
                .and_then(Option::take);
            let _ = handle.update(cx, |_, window, cx| {
                let marked = cx.global_mut::<Marked>();
                marked.open = false;
                marked.since = Some(std::time::Instant::now());
                window.refresh();
                if let Some(run) = run {
                    run(window, cx);
                }
            });
        })
        .detach();
    });
}

/// The window's shell, for an item that opens a page.
pub(crate) struct Here(pub gpui::WeakEntity<crate::shell::Shell>);

impl Global for Here {}

fn shell(cx: &App) -> Option<gpui::Entity<crate::shell::Shell>> {
    cx.try_global::<Here>()?.0.upgrade()
}

fn player(cx: &App) -> Option<gpui::Entity<crate::player::Player>> {
    Some(shell(cx)?.read(cx).player.clone())
}

/// A record by its release, as a page opens it.
pub(crate) fn release_row(
    release: Id,
    title: Option<String>,
    credit: Option<Credit>,
    art: Option<Id>,
) -> Arc<TileRow> {
    Arc::new(TileRow::of_album(ritornel_contract::AlbumTile {
        album: AlbumRef::Release { id: release },
        title,
        artist_credit: credit,
        year: None,
        track_count: 0,
        duration_ms: 0,
        version_count: 0,
        art_id: art,
        quality: None,
    }))
}

fn go_to_album(release: Option<Id>, title: Option<String>, art: Option<Id>) -> Option<Entry> {
    let release = release?;
    item("Go to Album", move |window, cx| {
        if let Some(shell) = shell(cx) {
            let row = release_row(release, title, None, art);
            shell.update(cx, |shell, cx| {
                shell.open_album(row, None, None, window, cx)
            });
        }
    })
}

fn go_to_artist(credit: Option<&Credit>) -> Option<Entry> {
    let credit = credit?;
    let artist = *credit.artist_ids.first()?;
    let name: gpui::SharedString = credit.text.clone().into();
    item("Go to Artist", move |window, cx| {
        if let Some(shell) = shell(cx) {
            shell.update(cx, |shell, cx| shell.open_artist(artist, name, window, cx));
        }
    })
}

/// A song of a list, as its row has it.
pub(crate) struct Song {
    pub appearance: Id,
    pub title: Option<String>,
    pub release: Option<Id>,
    pub album_title: Option<String>,
    pub credit: Option<Credit>,
    pub art: Option<Id>,
    pub context: Option<PlayContext>,
}

/// A song of a list: what its row's double-click and + do, and the way to
/// its album and artist (`songMenu`). A double-click already plays a song,
/// so queueing it leads.
pub(crate) fn song_menu(song: Song, play: Run) -> Vec<Option<Entry>> {
    let add = |place: QueuePlace| {
        let (id, title, context) = (song.appearance, song.title.clone(), song.context);
        move |_: &mut Window, cx: &mut App| {
            if let Some(player) = player(cx) {
                let name = format!("“{}”", title.as_deref().unwrap_or("Untitled"));
                player
                    .update(cx, |player, cx| {
                        player.add(
                            PlaySource::Tracks { ids: vec![id] },
                            place,
                            name,
                            context,
                            cx,
                        )
                    })
                    .detach();
            }
        }
    };
    let context = song.context;
    let on_album = context.is_some_and(|context| {
        context.kind == ritornel_contract::ContextKind::Album
            && context.reference.is_some()
            && context.reference == song.release
    });
    let first_artist = song
        .credit
        .as_ref()
        .and_then(|credit| credit.artist_ids.first().copied());
    let on_artist = context.is_some_and(|context| {
        context.kind == ritornel_contract::ContextKind::Artist
            && context.reference.is_some()
            && context.reference == first_artist
    });
    vec![
        item("Play Next", add(QueuePlace::Next)),
        item("Play Last", add(QueuePlace::End)),
        SEPARATOR,
        Some(Entry::Action {
            text: "Play".to_owned(),
            enabled: true,
            checked: false,
            run: play,
        }),
        SEPARATOR,
        (!on_album)
            .then(|| go_to_album(song.release, song.album_title.clone(), song.art))
            .flatten(),
        (!on_artist)
            .then(|| go_to_artist(song.credit.as_ref()))
            .flatten(),
    ]
}

/// Opens the page of the artist of a track the queue holds. The queue
/// names them only in words, so the track is found on its album, which
/// credits it (`openArtistOf`).
fn open_artist_of(track: &TrackSummary, window: &mut Window, cx: &mut App) {
    let (Some(release), Some(shell)) = (track.release_id, shell(cx)) else {
        return;
    };
    let Some(net) = shell.read(cx).net(cx) else {
        return;
    };
    let appearance = track.appearance_id;
    let named = track.artist.clone();
    let handle = window.window_handle();
    let client = Arc::clone(&net.client);
    let asked = net.rt().spawn(async move {
        let params = ritornel_contract::AlbumGetParams {
            album: AlbumRef::Release { id: release },
        };
        client
            .call::<ritornel_contract::methods::AlbumGet>(&params)
            .await
    });
    cx.spawn(async move |cx| {
        // The album has gone since the song was queued: there is nowhere to go.
        let Ok(Ok(album)) = asked.await else { return };
        let credit = album
            .tracks
            .iter()
            .find(|song| song.appearance_id == appearance)
            .and_then(|song| song.artist_credit.clone())
            .or(album.tile.artist_credit);
        let Some(artist) = credit
            .as_ref()
            .and_then(|credit| credit.artist_ids.first().copied())
        else {
            return;
        };
        let name = credit
            .map(|credit| credit.text)
            .or(named)
            .unwrap_or_default();
        let _ = handle.update(cx, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                shell.open_artist(artist, name.into(), window, cx)
            });
        });
    })
    .detach();
}

fn track_places(track: &TrackSummary) -> [Option<Entry>; 2] {
    let of = track.clone();
    [
        go_to_album(track.release_id, track.album.clone(), track.art_id),
        track.release_id.and_then(|_| {
            item("Go to Artist", move |window, cx| {
                open_artist_of(&of, window, cx)
            })
        }),
    ]
}

/// An entry of Up next, and whether it already plays next, or last
/// (`entryMenu`). It is already in Up next, so playing it now leads.
pub(crate) fn entry_menu(
    entry: Id,
    track: &TrackSummary,
    first: bool,
    last: bool,
    cx: &App,
) -> Vec<Option<Entry>> {
    let Some(player) = player(cx) else {
        return Vec::new();
    };
    let doing = |doing: fn(&mut crate::player::Player, Id, &mut Context<crate::player::Player>)| {
        let player = player.clone();
        move |_: &mut Window, cx: &mut App| player.update(cx, |player, cx| doing(player, entry, cx))
    };
    let moving = |place: QueuePlace, enabled: bool, text: &str| {
        let player = player.clone();
        Some(Entry::Action {
            text: text.to_owned(),
            enabled,
            checked: false,
            run: Box::new(move |_, cx| {
                player.update(cx, |player, cx| player.move_entries(vec![entry], place, cx));
            }),
        })
    };
    let mut entries = vec![
        item(
            "Play Now",
            doing(|player, entry, cx| player.jump(entry, cx)),
        ),
        SEPARATOR,
        moving(QueuePlace::Next, !first, "Play Next"),
        moving(QueuePlace::End, !last, "Play Last"),
        item(
            "Remove from Up Next",
            doing(|player, entry, cx| player.remove(entry, cx)),
        ),
        SEPARATOR,
    ];
    entries.extend(track_places(track));
    entries
}

/// The song playing, in the player bar and at the head of Up next: where
/// it is from, and its stars (`nowMenu`).
pub(crate) fn now_menu(track: &TrackSummary, cx: &App) -> Vec<Option<Entry>> {
    let mut entries: Vec<Option<Entry>> = track_places(track).into_iter().collect();
    entries.push(SEPARATOR);
    if let (Some(recording), Some(player)) = (track.recording_id, player(cx)) {
        let own = player.read(cx).own(recording, track.rating);
        let stars = crate::player::stars_of(own, false);
        let rate = |to: Option<u32>| {
            let checked = match to {
                None => own.is_none(),
                Some(to) => own.is_some() && stars == to / 20,
            };
            let text = to.map_or_else(|| "None".to_owned(), |to| "★".repeat((to / 20) as usize));
            let player = player.clone();
            Some(Entry::Action {
                text,
                enabled: true,
                checked,
                run: Box::new(move |_, cx| {
                    player.update(cx, |player, cx| player.rate(recording, to, cx));
                }),
            })
        };
        let mut items: Vec<Option<Entry>> = [100, 80, 60, 40, 20].map(|to| rate(Some(to))).into();
        items.push(SEPARATOR);
        items.push(rate(None));
        entries.push(Some(Entry::Sub {
            text: "Rating".to_owned(),
            items,
        }));
    }
    entries
}

/// What a thing of many songs is played and queued through (`manyMenu`).
/// Play and Shuffle are a click away on the page it opens, so queueing it
/// leads.
fn many_menu(many: Many) -> Vec<Option<Entry>> {
    let doing = |doing: Doing| {
        let many = many.clone();
        move |_: &mut Window, cx: &mut App| {
            if let Some(player) = player(cx) {
                player.update(cx, |player, cx| player.many(many, doing, cx));
            }
        }
    };
    vec![
        item("Play Next", doing(Doing::Next)),
        item("Play Last", doing(Doing::Last)),
        SEPARATOR,
        item("Play", doing(Doing::Play)),
        item("Shuffle", doing(Doing::Shuffle)),
    ]
}

/// A record, on a page or as the album's own cover (`recordMenu`). Go to
/// Artist is left out on that artist's page.
pub(crate) fn record_entries(
    album: AlbumRef,
    title: &str,
    credit: Option<&Credit>,
    cx: &App,
) -> Vec<Option<Entry>> {
    let here = shell(cx).and_then(|shell| shell.read(cx).artist_here(cx));
    let first = credit.and_then(|credit| credit.artist_ids.first().copied());
    let mut entries = many_menu(Many::Record {
        album,
        name: title.to_owned(),
    });
    entries.push(SEPARATOR);
    if here.is_none() || here != first {
        entries.push(go_to_artist(credit));
    }
    entries
}

/// How strongly `key` is marked, from what a page read as it began to draw.
pub(crate) fn strength(menued: Option<&(String, f32)>, key: &str) -> f32 {
    menued
        .filter(|(marked, _)| marked == key)
        .map_or(0., |(_, shown)| *shown)
}

/// The menu's ring round a cover `side` points with corners of `radius`
/// (`--menu-ring`: the page's colour for 2 points, then the accent at 55%
/// for 1.5), as strong as the mark is.
pub(crate) fn ring(side: f32, radius: f32, shown: f32) -> gpui::Div {
    use gpui::{Styled, px};
    gpui::div()
        .absolute()
        .top(px(-3.5))
        .left(px(-3.5))
        .size(px(side + 7.))
        .rounded(px(radius + 3.5))
        .border(px(1.5))
        .border_color(crate::theme::mix(
            crate::theme::acc(),
            crate::theme::bg(),
            0.45,
        ))
        .opacity(shown)
}

/// The key a record's mark goes by.
pub(crate) fn record_key(album: AlbumRef) -> String {
    format!("record:{}", album.id())
}

/// Opens a record's menu on a right-click on `element`.
pub(crate) fn on_record<E: gpui::InteractiveElement>(
    element: E,
    tile: &ritornel_contract::AlbumTile,
) -> E {
    let album = tile.album;
    let title = tile.title.clone().unwrap_or_else(|| "Untitled".to_owned());
    let credit = tile.artist_credit.clone();
    element.on_mouse_down(gpui::MouseButton::Right, move |event, window, cx| {
        cx.stop_propagation();
        let entries = record_entries(album, &title, credit.as_ref(), cx);
        popup(record_key(album), entries, event.position, window, cx);
    })
}

/// A menu's items in a line, for the trace: `Play Next, Play Last | Rating (★★★★★, …)`.
fn described(items: &[MenuItem]) -> String {
    items
        .iter()
        .map(|item| match item {
            MenuItem::Separator => "|".to_owned(),
            MenuItem::Action {
                title,
                enabled,
                checked,
            } => format!(
                "{}{title}{}",
                if *checked { "✓" } else { "" },
                if *enabled { "" } else { " (off)" }
            ),
            MenuItem::Submenu { title, items } => format!("{title} ({})", described(items)),
        })
        .collect::<Vec<_>>()
        .join(", ")
        .replace(", |, ", " | ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn titles(entries: Vec<Option<Entry>>) -> Vec<String> {
        let mut runs = Vec::new();
        native(tidy(entries), &mut runs)
            .iter()
            .map(|item| match item {
                MenuItem::Separator => "|".to_owned(),
                MenuItem::Action { title, .. } | MenuItem::Submenu { title, .. } => title.clone(),
            })
            .collect()
    }

    #[test]
    fn what_does_not_apply_is_left_out() {
        let menu = vec![
            item("Play", |_, _| {}),
            None,
            item("Go to Album", |_, _| {}),
        ];
        assert_eq!(titles(menu), ["Play", "Go to Album"]);
    }

    #[test]
    fn no_separator_at_either_end_or_two_together() {
        let menu = vec![
            SEPARATOR,
            item("Play", |_, _| {}),
            SEPARATOR,
            None,
            SEPARATOR,
            item("Go to Album", |_, _| {}),
            SEPARATOR,
        ];
        assert_eq!(titles(menu), ["Play", "|", "Go to Album"]);
        assert_eq!(
            titles(vec![item("Play", |_, _| {}), SEPARATOR, None]),
            ["Play"]
        );
        assert!(titles(vec![SEPARATOR, None]).is_empty());
    }

    #[test]
    fn a_submenus_actions_are_numbered_where_it_stands() {
        let menu = vec![
            item("Go to Album", |_, _| {}),
            Some(Entry::Sub {
                text: "Rating".to_owned(),
                items: vec![item("★★", |_, _| {}), SEPARATOR, item("None", |_, _| {})],
            }),
            item("After", |_, _| {}),
        ];
        let mut runs = Vec::new();
        let items = native(tidy(menu), &mut runs);
        assert_eq!(runs.len(), 4);
        assert_eq!(described(&items), "Go to Album, Rating (★★ | None), After");
    }
}
