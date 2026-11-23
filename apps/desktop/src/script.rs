//! A short script for checking by eye, run inside the app with no input
//! from the system: `--script "wait 1500; shot a.png; jump 40000; wait 400;
//! shot b.png; quit"`. Steps:
//!
//! - `wait <ms>`
//! - `shot <path>`: a picture of the window alone
//! - `jump <points>`: scroll the page by that much at once
//! - `pull <points>`: pull the wall with a touchpad, held; `lift`: let go
//! - `open <index>`: click the tile of that row (it must be drawn)
//! - `type <text>`: each character a key; `key <name>`: one key, as `down` or `cmd-down`
//! - `wall artists` or `wall albums <artist|title|year|added>`: as the controls choose
//! - `tab <name>`: go to a tab, as clicking it does
//! - `back`; `up`: the banner's way back, on an album's page or an artist's;
//!   `record <n>`: a click on the nth record of the page open
//! - `goto <title>`: open the record of that title exactly, as a click on it would;
//!   `artist <name>`: the artist of that name
//! - `album play`, `album shuffle`, `album row <n>` (a double-click on it),
//!   `album next <n>` (its +) or `album rate <n> <stars>`: on the album page open
//! - `point <across> <down>`: hold the pointer over History's picture there,
//!   as shares of the window; `hold <n>`: click its nth layer (newest first)
//! - `size <w>x<h>`: resize the window
//! - `menu <index>`: open that tile's context menu; `dismiss` puts it away;
//!   `do <index> <item>`: what an item of it does, as `do 3 Play Next`
//! - `settings <section>`: open Settings (as ⌘, does) on that section, `library`,
//!   `appearance`, `lastfm` or `about`; `settings shot <path>`: a picture of it;
//!   `settings letters on|off`, `settings watching on|off`: as its switches do;
//!   `settings add <path>`: add that music folder, as the folder chooser does;
//!   `settings zone`: open the form for a zone; `settings choose <n>`: choose the
//!   nth zone of the list; `settings focus`: make it the key window, as a
//!   click on it would; `settings close`: as ⌘W does
//! - `lean`: open Lean back, or close it, as the player bar's button does
//! - `connection`: hold the pointer on the title bar's connection, as hovering it does; `connection leave` lets go
//! - `upnext`: open or close Up next's card, as ⌥⌘U does; `upnext open` or `upnext close`
//! - `press <x> <y>`, `move <x> <y>`, `release`: the left button held down
//!   there, the pointer moved there in steps while it is held, and let go
//!   where it is, as a drag by hand; in points from the window's top left
//! - `mem <label>`: print the memory footprint
//! - `waits <label>`: print how long tiles waited for their pictures since
//!   the last, with `RITORNEL_GPUI_WAITS=1` (`waits.rs`); `waits -` starts
//!   the count again
//! - `close`: close the window, as its close button does; the app runs on
//! - `quit`

#![allow(
    clippy::print_stderr,
    reason = "a developer's tool: it reports on standard error"
)]

use std::time::Duration;

use gpui::{AnyWindowHandle, AsyncApp, TouchPhase, px, size};

use crate::shell::Shell;

pub(crate) fn run(script: String, window: AnyWindowHandle, cx: &mut AsyncApp) -> gpui::Task<()> {
    cx.spawn(async move |cx| {
        // Where the pointer is, for a drag by hand.
        let mut pointer = gpui::point(px(0.), px(0.));
        for step in script
            .split(';')
            .map(str::trim)
            .filter(|step| !step.is_empty())
        {
            let (verb, rest) = step.split_once(' ').unwrap_or((step, ""));
            match verb {
                "wait" => {
                    let ms = rest.trim().parse().unwrap_or(500);
                    cx.background_executor()
                        .timer(Duration::from_millis(ms))
                        .await;
                }
                "shot" => {
                    // Run on a display no one sees (`RITORNEL_TEST_DISPLAY`), the
                    // window is never covered, and so never stale.
                    let path = std::path::PathBuf::from(rest.trim());
                    let _ = window.update(cx, |_, window, _| {
                        crate::platform::screenshot(window, &path);
                    });
                }
                "pull" | "lift" => {
                    // A touchpad's pull of the wall, held, or the fingers lifting.
                    let by: f32 = rest.trim().parse().unwrap_or(0.);
                    let lifting = verb == "lift";
                    let _ = window.update(cx, |root, window, cx| {
                        if let Ok(shell) = root.downcast::<Shell>() {
                            let viewport = crate::wall::viewport_of(window).1;
                            shell.read(cx).wall.clone().update(cx, |wall, cx| {
                                use crate::elastic::{Phase, Wheel};
                                let now = std::time::Instant::now();
                                if lifting {
                                    let wheel = Wheel::touch(0., Phase::Ended);
                                    wall.scroll_by(wheel, TouchPhase::Ended, viewport, now);
                                } else {
                                    let wheel = Wheel::touch(0., Phase::Began);
                                    wall.scroll_by(wheel, TouchPhase::Started, viewport, now);
                                    for _ in 0..10 {
                                        let wheel = Wheel::touch(by / 10., Phase::Changed);
                                        wall.scroll_by(wheel, TouchPhase::Moved, viewport, now);
                                    }
                                }
                                cx.notify();
                            });
                        }
                    });
                }
                "jump" => {
                    let by: f32 = rest.trim().parse().unwrap_or(0.);
                    let _ = window.update(cx, |root, window, cx| {
                        if let Ok(shell) = root.downcast::<Shell>() {
                            shell.update(cx, |shell, cx| {
                                let viewport = crate::wall::viewport_of(window).1;
                                if let Some(album) = shell.album_page() {
                                    album.update(cx, |album, cx| {
                                        album.jump(by, viewport);
                                        cx.notify();
                                    });
                                    return;
                                }
                                if let Some(history) = shell.history_page() {
                                    history.update(cx, |history, cx| {
                                        history.jump(by, viewport);
                                        cx.notify();
                                    });
                                    return;
                                }
                                if let Some(artist) = shell.artist_page() {
                                    artist.update(cx, |artist, cx| {
                                        artist.jump(by, viewport);
                                        cx.notify();
                                    });
                                    return;
                                }
                                shell.wall.update(cx, |wall, cx| {
                                    wall.scroll_by(
                                        crate::elastic::Wheel::plain(by),
                                        TouchPhase::Started,
                                        viewport,
                                        std::time::Instant::now(),
                                    );
                                    cx.notify();
                                });
                            });
                        }
                    });
                }
                "point" | "hold" => {
                    let numbers: Vec<f32> = rest
                        .split_whitespace()
                        .filter_map(|word| word.parse().ok())
                        .collect();
                    let _ = window.update(cx, |root, window, cx| {
                        let size = window.viewport_size();
                        if let Ok(shell) = root.downcast::<Shell>()
                            && let Some(history) = shell.read(cx).history_page()
                        {
                            history.update(cx, |history, cx| {
                                match (verb, numbers.as_slice()) {
                                    ("point", [across, down]) => history.point(gpui::point(
                                        size.width * *across,
                                        size.height * *down,
                                    )),
                                    ("hold", [n]) => history.hold(*n as usize),
                                    _ => {}
                                }
                                cx.notify();
                            });
                        }
                    });
                }
                "open" => {
                    let index: usize = rest.trim().parse().unwrap_or(0);
                    let _ = window.update(cx, |root, window, cx| {
                        if let Ok(shell) = root.downcast::<Shell>() {
                            shell.update(cx, |shell, cx| shell.open_index(index, window, cx));
                        }
                    });
                }
                "type" => {
                    // Characters, each a key, as typed on the window.
                    for character in rest.chars() {
                        let key = if character == ' ' {
                            "space".to_owned()
                        } else {
                            character.to_string()
                        };
                        let keystroke = gpui::Keystroke {
                            modifiers: gpui::Modifiers::default(),
                            key,
                            key_char: Some(character.to_string()),
                        };
                        let _ = window
                            .update(cx, |_, window, cx| window.dispatch_keystroke(keystroke, cx));
                        cx.background_executor()
                            .timer(Duration::from_millis(80))
                            .await;
                    }
                }
                "key" => {
                    // One key by name, as GPUI writes it: `down`, `enter`, `cmd-down`, `escape`.
                    let name: &'static str = Box::leak(rest.trim().to_owned().into_boxed_str());
                    if let Ok(keystroke) = gpui::Keystroke::parse(name) {
                        let _ = window
                            .update(cx, |_, window, cx| window.dispatch_keystroke(keystroke, cx));
                    }
                }
                "wall" => {
                    // `wall artists` or `wall albums year`: as the controls choose.
                    let mut words = rest.split_whitespace();
                    let kind = match words.next() {
                        Some("albums") => crate::wall::WallKind::Albums,
                        _ => crate::wall::WallKind::Artists,
                    };
                    let order = words.next().and_then(crate::wall::order_named);
                    let _ = window.update(cx, |root, _, cx| {
                        if let Ok(shell) = root.downcast::<Shell>() {
                            let wall = shell.read(cx).wall.clone();
                            wall.update(cx, |wall, cx| wall.choose(kind, order, cx));
                        }
                    });
                }
                "tab" => {
                    let wanted = rest.trim().to_lowercase();
                    let tab = crate::stage::Tab::ALL
                        .into_iter()
                        .find(|tab| tab.label().to_lowercase() == wanted);
                    let _ = window.update(cx, |root, _, cx| {
                        if let (Ok(shell), Some(tab)) = (root.downcast::<Shell>(), tab) {
                            shell.update(cx, |shell, cx| shell.go(tab, cx));
                        }
                    });
                }
                "album" => {
                    let words: Vec<&str> = rest.split_whitespace().collect();
                    let _ = window.update(cx, |root, _, cx| {
                        let Ok(shell) = root.downcast::<Shell>() else {
                            return;
                        };
                        let Some(album) = shell.read(cx).album_page() else {
                            return;
                        };
                        let number = |at: usize| {
                            words
                                .get(at)
                                .and_then(|word| word.parse::<usize>().ok())
                                .unwrap_or(0)
                        };
                        album.update(cx, |album, cx| match words.first().copied() {
                            Some("play") => album.play_all(false, cx),
                            Some("shuffle") => album.play_all(true, cx),
                            Some("row") => album.play_from(number(1), cx),
                            Some("next") => album.play_next(number(1), cx),
                            Some("rate") => album.rate(number(1), number(2) as u32, cx),
                            _ => {}
                        });
                    });
                }
                "artist" => {
                    let wanted = rest.trim().to_owned();
                    let net = window
                        .update(cx, |root, _, cx| {
                            root.downcast::<Shell>()
                                .ok()
                                .and_then(|shell| shell.read(cx).net(cx))
                        })
                        .ok()
                        .flatten();
                    let Some(net) = net else { continue };
                    let client = std::sync::Arc::clone(&net.client);
                    let text = wanted.clone();
                    let found = net
                        .rt()
                        .spawn(async move {
                            let opened = client
                                .list_open(&ritornel_contract::ListOpenParams {
                                    kind: ritornel_contract::ListKind::Artists,
                                    query: ritornel_contract::Query {
                                        text: Some(text.clone()),
                                        ..ritornel_contract::Query::default()
                                    },
                                    window: Some(ritornel_contract::Window {
                                        offset: 0,
                                        limit: 50,
                                    }),
                                    replaces: None,
                                })
                                .await
                                .ok()?;
                            let _ = client.list_close(&opened.view).await;
                            opened.rows.into_iter().find_map(|row| match row {
                                ritornel_contract::Row::Artist(artist)
                                    if artist.name.as_deref() == Some(text.as_str()) =>
                                {
                                    Some(artist.artist_id)
                                }
                                _ => None,
                            })
                        })
                        .await
                        .ok()
                        .flatten();
                    if let Some(id) = found {
                        let _ = window.update(cx, |root, window, cx| {
                            if let Ok(shell) = root.downcast::<Shell>() {
                                shell.update(cx, |shell, cx| {
                                    shell.open_artist(id, wanted.clone().into(), window, cx);
                                });
                            }
                        });
                    } else {
                        eprintln!("artist: no artist named {wanted}");
                    }
                }
                "goto" => {
                    let wanted = rest.trim().to_owned();
                    let net = window
                        .update(cx, |root, _, cx| {
                            root.downcast::<Shell>()
                                .ok()
                                .and_then(|shell| shell.read(cx).net(cx))
                        })
                        .ok()
                        .flatten();
                    let Some(net) = net else { continue };
                    let client = std::sync::Arc::clone(&net.client);
                    let text = wanted.clone();
                    let found = net
                        .rt()
                        .spawn(async move {
                            let opened = client
                                .list_open(&ritornel_contract::ListOpenParams {
                                    kind: ritornel_contract::ListKind::Albums,
                                    query: ritornel_contract::Query {
                                        text: Some(text.clone()),
                                        ..ritornel_contract::Query::default()
                                    },
                                    window: Some(ritornel_contract::Window {
                                        offset: 0,
                                        limit: 50,
                                    }),
                                    replaces: None,
                                })
                                .await
                                .ok()?;
                            let _ = client.list_close(&opened.view).await;
                            opened.rows.into_iter().find_map(|row| match row {
                                ritornel_contract::Row::Album(tile)
                                    if tile.title.as_deref() == Some(text.as_str()) =>
                                {
                                    Some(tile)
                                }
                                _ => None,
                            })
                        })
                        .await
                        .ok()
                        .flatten();
                    if let Some(tile) = found {
                        let _ = window.update(cx, |root, window, cx| {
                            if let Ok(shell) = root.downcast::<Shell>() {
                                shell.update(cx, |shell, cx| {
                                    let row =
                                        std::sync::Arc::new(crate::wall::TileRow::of_album(tile));
                                    shell.open_album(row, None, None, window, cx);
                                });
                            }
                        });
                    } else {
                        eprintln!("goto: no record titled {wanted}");
                    }
                }
                "record" => {
                    // A click on the nth record of the page open.
                    let n: usize = rest.trim().parse().unwrap_or(0);
                    let _ = window.update(cx, |root, _, cx| {
                        if let Ok(shell) = root.downcast::<Shell>() {
                            let (album, artist) = {
                                let shell = shell.read(cx);
                                (shell.album_page(), shell.artist_page())
                            };
                            if let Some(album) = album {
                                album.update(cx, |album, cx| album.open_record(n, cx));
                            } else if let Some(artist) = artist {
                                artist.update(cx, |artist, cx| artist.open_record(n, cx));
                            }
                        }
                    });
                }
                "up" => {
                    // The banner's way back, on the page open.
                    let _ = window.update(cx, |root, _, cx| {
                        if let Ok(shell) = root.downcast::<Shell>() {
                            let (album, artist) = {
                                let shell = shell.read(cx);
                                (shell.album_page(), shell.artist_page())
                            };
                            if let Some(album) = album {
                                album.update(cx, |album, cx| album.up_to_artist(cx));
                            } else if let Some(artist) = artist {
                                artist.update(cx, |artist, cx| artist.up(cx));
                            }
                        }
                    });
                }
                "lean" => {
                    let _ = window.update(cx, |root, window, cx| {
                        if let Ok(shell) = root.downcast::<Shell>() {
                            shell.update(cx, |shell, cx| shell.toggle_lean(window, cx));
                        }
                    });
                }
                "back" => {
                    let _ = window.update(cx, |root, window, cx| {
                        if let Ok(shell) = root.downcast::<Shell>() {
                            shell.update(cx, |shell, cx| shell.back(window, cx));
                        }
                    });
                }
                "size" => {
                    if let Some((w, h)) = rest.trim().split_once('x')
                        && let (Ok(w), Ok(h)) = (w.parse::<f32>(), h.parse::<f32>())
                    {
                        let _ = window.update(cx, |_, window, _| window.resize(size(px(w), px(h))));
                    }
                }
                "do" => {
                    // `do <index> <item>`: what that tile's menu item does.
                    let (index, item) = rest.trim().split_once(' ').unwrap_or((rest, ""));
                    let index: usize = index.parse().unwrap_or(0);
                    let item = item.to_owned();
                    let _ = window.update(cx, |root, _, cx| {
                        if let Ok(shell) = root.downcast::<Shell>() {
                            let wall = shell.read(cx).wall.clone();
                            wall.update(cx, |wall, cx| {
                                if let Some(row) = wall.row(index) {
                                    crate::menu::act(wall, &row, &item, cx);
                                }
                            });
                        }
                    });
                }
                "menu" => {
                    let index: usize = rest.trim().parse().unwrap_or(0);
                    let _ = window.update(cx, |root, window, cx| {
                        if let Ok(shell) = root.downcast::<Shell>() {
                            let wall = shell.read(cx).wall.clone();
                            wall.update(cx, |wall, cx| {
                                if let (Some(row), Some(art)) =
                                    (wall.row(index), wall.art_bounds(index))
                                {
                                    wall.menu_open = Some(index);
                                    cx.notify();
                                    let at =
                                        gpui::point(art.origin.x + px(60.), art.origin.y + px(60.));
                                    crate::menu::record_menu(row, at, window, cx);
                                }
                            });
                        }
                    });
                }
                "dismiss" => {
                    cx.update(crate::platform::dismiss_menu);
                }
                "rclick" | "click" => {
                    // `rclick <x> <y>`: a right-click there, in points from
                    // the window's top left; `click` a left one.
                    let mut words = rest.split_whitespace().map(str::parse::<f32>);
                    if let (Some(Ok(x)), Some(Ok(y))) = (words.next(), words.next()) {
                        let button = if verb == "rclick" {
                            gpui::MouseButton::Right
                        } else {
                            gpui::MouseButton::Left
                        };
                        let position = gpui::point(px(x), px(y));
                        // The pointer goes there first: what is under it is
                        // found as it moves.
                        let _ = window.update(cx, |_, window, cx| {
                            window.dispatch_event(
                                gpui::PlatformInput::MouseMove(gpui::MouseMoveEvent {
                                    position,
                                    pressed_button: None,
                                    modifiers: gpui::Modifiers::default(),
                                }),
                                cx,
                            );
                        });
                        cx.background_executor()
                            .timer(Duration::from_millis(50))
                            .await;
                        let _ = window.update(cx, |_, window, cx| {
                            window.dispatch_event(
                                gpui::PlatformInput::MouseDown(gpui::MouseDownEvent {
                                    button,
                                    position,
                                    modifiers: gpui::Modifiers::default(),
                                    click_count: 1,
                                    first_mouse: false,
                                }),
                                cx,
                            );
                            window.dispatch_event(
                                gpui::PlatformInput::MouseUp(gpui::MouseUpEvent {
                                    button,
                                    position,
                                    modifiers: gpui::Modifiers::default(),
                                    click_count: 1,
                                }),
                                cx,
                            );
                        });
                    }
                }
                "connection" => {
                    let over = rest.trim() != "leave";
                    let _ = window.update(cx, |root, _, cx| {
                        if let Ok(shell) = root.downcast::<Shell>() {
                            shell.update(cx, |shell, cx| shell.hover_connection(over, cx));
                        }
                    });
                }
                "upnext" => {
                    let open = match rest.trim() {
                        "open" => Some(true),
                        "close" => Some(false),
                        _ => None,
                    };
                    let _ = window.update(cx, |root, _, cx| {
                        if let Ok(shell) = root.downcast::<Shell>() {
                            shell.update(cx, |shell, cx| shell.toggle_card(open, cx));
                        }
                    });
                }
                "press" | "move" | "release" => {
                    let mut words = rest.split_whitespace().map(str::parse::<f32>);
                    let to = match (words.next(), words.next()) {
                        (Some(Ok(x)), Some(Ok(y))) => gpui::point(px(x), px(y)),
                        _ => pointer,
                    };
                    let left = gpui::MouseButton::Left;
                    let steps = if verb == "move" { 12 } else { 1 };
                    let from = pointer;
                    for step in 1..=steps {
                        let t = step as f32 / steps as f32;
                        let position = from + (to - from) * t;
                        let input = match verb {
                            "press" => gpui::PlatformInput::MouseDown(gpui::MouseDownEvent {
                                button: left,
                                position,
                                modifiers: gpui::Modifiers::default(),
                                click_count: 1,
                                first_mouse: false,
                            }),
                            "move" => gpui::PlatformInput::MouseMove(gpui::MouseMoveEvent {
                                position,
                                pressed_button: Some(left),
                                modifiers: gpui::Modifiers::default(),
                            }),
                            _ => gpui::PlatformInput::MouseUp(gpui::MouseUpEvent {
                                button: left,
                                position,
                                modifiers: gpui::Modifiers::default(),
                                click_count: 1,
                            }),
                        };
                        if verb == "press" {
                            // The pointer comes there first, as a hand's would.
                            let _ = window.update(cx, |_, window, cx| {
                                window.dispatch_event(
                                    gpui::PlatformInput::MouseMove(gpui::MouseMoveEvent {
                                        position,
                                        pressed_button: None,
                                        modifiers: gpui::Modifiers::default(),
                                    }),
                                    cx,
                                );
                            });
                        }
                        let _ = window.update(cx, |_, window, cx| {
                            window.dispatch_event(input, cx);
                        });
                        cx.background_executor()
                            .timer(Duration::from_millis(16))
                            .await;
                    }
                    pointer = to;
                }
                "settings" => settings(rest, cx),
                "close" => {
                    let _ = window.update(cx, |_, window, _| window.remove_window());
                }
                "mem" => eprintln!("mem {}: {}", rest.trim(), crate::platform::memory_line()),
                "waits" => crate::waits::report(rest.trim()),
                "quit" => {
                    cx.update(|cx| cx.quit());
                }
                _ => eprintln!("script: unknown step {step}"),
            }
        }
    })
}

/// A step of the Settings window's.
fn settings(rest: &str, cx: &mut AsyncApp) {
    use crate::settings::{self, Section};
    let words: Vec<&str> = rest.split_whitespace().collect();
    let on = words.get(1) == Some(&"on");
    let open = cx.update(|cx| settings::window(cx));
    let model = open.and_then(|open| {
        open.update(cx, |settings, _, _| settings.model().clone())
            .ok()
    });
    match words.first().copied() {
        Some("shot") => {
            let path = std::path::PathBuf::from(words.get(1).copied().unwrap_or("settings.png"));
            if let Some(open) = open {
                let _ = open.update(cx, |_, window, _| {
                    crate::platform::screenshot(window, &path)
                });
            }
        }
        Some("focus") => {
            if let Some(open) = open {
                let _ = open.update(cx, |_, window, _| window.activate_window());
            }
        }
        Some("close") => {
            if let Some(open) = open {
                let _ = open.update(cx, |_, window, _| window.remove_window());
            }
        }
        Some("letters") => cx.update(|cx| crate::remember::set_letters(on, cx)),
        Some("add") => {
            let path = rest.trim_start()["add".len()..].trim().to_owned();
            if let Some(model) = model {
                model.update(cx, |model, cx| model.add_folder(path, cx).detach());
            }
        }
        Some("watching") => {
            if let Some(model) = model {
                model.update(cx, |model, cx| model.set_watching(on, cx));
            }
        }
        Some("zone") => {
            if let Some(open) = open {
                let _ = open.update(cx, |settings, _, cx| settings.start_adding(cx));
            }
        }
        Some("choose") => {
            let index = words.get(1).and_then(|word| word.parse().ok());
            if let Some(model) = model {
                model.update(cx, |model, cx| {
                    model.chosen = index;
                    cx.notify();
                });
            }
        }
        Some(name) => {
            let Some(section) = Section::named(name) else {
                eprintln!("script: no section of Settings named {name}");
                return;
            };
            cx.update(|cx| settings::open(Some(section), cx));
        }
        None => cx.update(|cx| settings::open(None, cx)),
    }
}
