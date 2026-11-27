//! The frame: the title bar with its tabs (the player bar is `bar.rs`).
//! And the app's menus and keys: the menus are the system's menu bar's
//! where it has one, and otherwise opened from a button at the title bar's
//! left (`drawn.rs`), from the same definition.

use std::rc::Rc;

use gpui::{
    Action, App, FontWeight, InteractiveElement, IntoElement, KeyBinding,
    KeyBindingContextPredicate, Menu, MenuItem, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, WindowControlArea, actions, div, prelude::FluentBuilder,
    px,
};

use crate::icons::icon;
use crate::platform;
use crate::stage::Tab;
use crate::theme;
use crate::wall::TITLE_BAR;

actions!(
    ritornel,
    [
        Quit,
        Back,
        Hide,
        HideOthers,
        ShowAll,
        Minimize,
        Zoom,
        PlayNext,
        PlayLast,
        Play,
        Shuffle,
        GoToArtist,
        Find,
        LeanBack,
        ToggleUpNext
    ]
);

/// A binding that may hold `secondary-`, the system's own modifier: ⌘ on a
/// Mac, Ctrl elsewhere.
pub(crate) fn key<A: Action>(
    keys: &str,
    action: A,
    context: Option<&str>,
    cx: &App,
) -> Option<KeyBinding> {
    let context = match context {
        Some(context) => Some(Rc::new(KeyBindingContextPredicate::parse(context).ok()?)),
        None => None,
    };
    let binding = KeyBinding::load(keys, Box::new(action), context, false, None, cx);
    if binding.is_err() {
        crate::debug(format_args!("keys: {keys} is not a key"));
    }
    binding.ok()
}

/// Each of the system's keys in `keys` for `action`.
pub(crate) fn keys<A: Action + Clone>(
    keys: &'static [&'static str],
    action: A,
    context: Option<&str>,
) -> impl Iterator<Item = KeyBinding> {
    keys.iter()
        .map(move |keys| KeyBinding::new(keys, action.clone(), context))
}

pub(crate) fn menus(cx: &mut App) {
    // Quit is `main.rs`'s, which knows the server.
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    // The window in front, as Window › Minimize and Zoom say.
    cx.on_action(|_: &Minimize, cx| {
        if let Some(window) = cx.active_window() {
            let _ = window.update(cx, |_, window, _| window.minimize_window());
        }
    });
    cx.on_action(|_: &Zoom, cx| {
        if let Some(window) = cx.active_window() {
            let _ = window.update(cx, |_, window, _| window.zoom_window());
        }
    });
    // Controls › Play and Shuffle, as the player bar's buttons do, from
    // whichever window is in front.
    cx.on_action(|_: &Play, cx| {
        if let Some(player) = crate::menu::player(cx) {
            player.update(cx, |player, cx| player.toggle(cx));
        }
    });
    cx.on_action(|_: &Shuffle, cx| {
        if let Some(player) = crate::menu::player(cx) {
            player.update(cx, |player, cx| {
                if player.on() {
                    let shuffle = player.state.as_ref().is_some_and(|state| state.shuffle);
                    player.set_shuffle(!shuffle, cx);
                }
            });
        }
    });
    let system = platform::KEYS;
    let mut bindings: Vec<KeyBinding> = Vec::new();
    bindings.extend(keys(system.quit, Quit, None));
    bindings.extend(keys(system.hide, Hide, None));
    bindings.extend(keys(system.hide_others, HideOthers, None));
    bindings.extend(keys(system.back, Back, None));
    bindings.extend(keys(system.minimize, Minimize, None));
    bindings.extend(key("secondary-f", Find, None, cx));
    bindings.extend(key("shift-secondary-f", LeanBack, None, cx));
    bindings.extend(key("alt-secondary-u", ToggleUpNext, None, cx));
    cx.bind_keys(bindings);
    // A menu bar's app menu has the system's own items; a menu opened from
    // the title bar has only the app's.
    let app = if platform::CHROME.menu_bar {
        Menu::new("Ritornel").items([
            MenuItem::action("Settings…", crate::settings::OpenSettings),
            MenuItem::separator(),
            MenuItem::os_submenu("Services", gpui::SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action("Hide Ritornel", Hide),
            MenuItem::action("Hide Others", HideOthers),
            MenuItem::action("Show All", ShowAll),
            MenuItem::separator(),
            MenuItem::action("Quit Ritornel", Quit),
        ])
    } else {
        Menu::new("Ritornel").items([
            MenuItem::action("Settings…", crate::settings::OpenSettings),
            MenuItem::separator(),
            MenuItem::action("Quit Ritornel", Quit),
        ])
    };
    cx.set_menus([
        app,
        // Up Next and Lean Back in the View menu.
        Menu::new("View").items([
            MenuItem::action("Up Next", ToggleUpNext),
            MenuItem::action("Lean Back", LeanBack),
            MenuItem::separator(),
            MenuItem::action("Back", Back),
        ]),
        Menu::new("Controls").items([
            MenuItem::action("Play", Play),
            MenuItem::action("Shuffle", Shuffle),
        ]),
        Menu::new("Window").items([
            MenuItem::action("Minimize", Minimize),
            MenuItem::action("Zoom", Zoom),
        ]),
    ]);
}

/// Goes to a tab, as clicking it does.
pub(crate) type OnTab = std::rc::Rc<dyn Fn(Tab, &mut gpui::Window, &mut App)>;

/// The connection, as the title bar shows it at the right of the middle.
pub(crate) struct Server {
    /// The name it is shown by.
    pub name: SharedString,
    /// Whether the computer the library is on has gone: the dot is then an
    /// amber ring.
    pub away: bool,
    /// What it says while the pointer is on it: a first line and a second,
    /// which may be empty.
    pub card: Option<(SharedString, SharedString)>,
    /// Told when the pointer comes onto it, or leaves.
    pub on_hover: OnHover,
}

/// Told whether the pointer is on something.
pub(crate) type OnHover = Rc<dyn Fn(bool, &mut gpui::Window, &mut App)>;

#[allow(
    clippy::too_many_arguments,
    reason = "the title bar's parts, as the shell has them"
)]
pub(crate) fn title_bar(
    can_go_back: bool,
    lit: Tab,
    // Whether the bar lies over a banner's picture, not the page's ground.
    over_picture: bool,
    // The connection, or `None` when there is none.
    server: Option<Server>,
    // What is said while there is none.
    unconnected: &'static str,
    search: gpui::AnyElement,
    on_back: impl Fn(&gpui::ClickEvent, &mut gpui::Window, &mut App) + 'static,
    on_tab: OnTab,
) -> impl IntoElement {
    // The lit tab wears the accent on its own tint, as a lit button of the
    // player bar does, and the ink where the accent is a grey. Over a
    // banner's picture, whose colours the accent was never worked out
    // against, it keeps the ink on `fill`.
    let (lit_ground, lit_words) = if over_picture {
        (theme::fill(), theme::ink())
    } else {
        (theme::lit(), theme::lit_words())
    };
    // Each tab keeps room for its name at the lit weight, so lighting one
    // does not nudge the others: the name at 600, unseen, sets the width.
    let tab = |tab: Tab| {
        let (label, on, go) = (tab.label(), tab == lit, on_tab.clone());
        let base = div()
            .id(label)
            .relative()
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(move |_, window, cx| go(tab, window, cx))
            .when(!on, |base| {
                base.hover(theme::hovered_ink).active(theme::pressed)
            })
            .px(px(11.))
            .py(px(5.))
            .rounded(px(8.))
            .whitespace_nowrap();
        let base = if on { base.bg(lit_ground) } else { base };
        base.text_color(theme::dim())
            .child(div().font_weight(FontWeight(600.)).invisible().child(label))
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left_0()
                    .right_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .font_weight(FontWeight(if on { 600. } else { 500. }))
                    .when(on, |label| label.text_color(lit_words))
                    .child(label),
            )
    };
    div()
        .id("title-bar")
        // On Windows the area answers the system's hit test as the caption.
        .window_control_area(WindowControlArea::Drag)
        // On a Mac a press here moves the window, as a title bar's does.
        .when(platform::CHROME.app_moves_window, |bar| {
            bar.on_mouse_down(gpui::MouseButton::Left, |event, window, _| {
                if event.click_count == 2 {
                    window.titlebar_double_click();
                } else {
                    window.start_window_move();
                }
            })
        })
        .h(px(TITLE_BAR))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(16.))
        .pl(px(platform::CHROME.padding.0))
        .pr(px(platform::CHROME.padding.1))
        .text_size(px(13.5))
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(px(14.))
                // Room for the system's own window buttons.
                .when_some(platform::CHROME.system_buttons, |left, width| {
                    left.child(div().w(px(width)).h(px(14.)).mr(px(4.)))
                })
                // With no menu bar, the app's menus are a button away.
                .when(!platform::CHROME.menu_bar, |left| left.child(menu_button()))
                .child(
                    div()
                        .id("back")
                        .w(px(24.))
                        .h(px(28.))
                        .mx(px(-6.))
                        .rounded(px(6.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_color(theme::dim())
                        .when(!can_go_back, |back| back.opacity(0.35))
                        .when(can_go_back, |back| {
                            back.hover(theme::hovered).active(theme::pressed)
                        })
                        // The press must not start a window drag.
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(on_back)
                        .child(icon("back", 16., theme::dim())),
                )
                .child(div().flex().gap(px(2.)).children(Tab::ALL.map(tab))),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .justify_end()
                .gap(px(6.))
                .text_size(px(12.5))
                .text_color(theme::dim())
                .whitespace_nowrap()
                .child(connection(server, unconnected)),
        )
        .child(search)
        .when(platform::CHROME.app_buttons, |bar| {
            bar.child(caption_buttons())
        })
}

/// The connection's dot and name, and what hovering them says.
fn connection(server: Option<Server>, unconnected: &'static str) -> impl IntoElement {
    let dot = div().size(px(7.)).flex_none().rounded_full();
    let dot = match &server {
        Some(server) if server.away => dot.border(px(1.5)).border_color(theme::warn()),
        Some(_) => dot.bg(gpui::rgb(0x3fae6a)),
        None => dot.bg(theme::fill2()),
    };
    let Some(Server {
        name,
        card,
        on_hover,
        ..
    }) = server
    else {
        return div()
            .flex()
            .items_center()
            .gap(px(6.))
            .min_w_0()
            .child(dot)
            .child(div().min_w_0().truncate().child(unconnected))
            .into_any_element();
    };
    div()
        .id("connection")
        .relative()
        .flex()
        .items_center()
        .gap(px(6.))
        .min_w_0()
        .on_hover(move |over, window, cx| on_hover(*over, window, cx))
        .child(dot)
        .child(div().min_w_0().truncate().child(name))
        .children(card.map(|(first, then)| {
            // Over the page, as the search's results are, under the name
            // and ending where it ends.
            gpui::deferred(
                div()
                    .absolute()
                    .top(px(24.))
                    .right_0()
                    .w(px(300.))
                    .p(px(12.))
                    .rounded(px(12.))
                    .bg(theme::raised())
                    .shadow(crate::search::shadow())
                    .whitespace_normal()
                    .text_size(px(13.))
                    .line_height(px(18.))
                    .child(
                        div()
                            .font_weight(FontWeight(600.))
                            .text_color(theme::ink())
                            .child(first),
                    )
                    .when(!then.is_empty(), |card| {
                        card.child(div().mt(px(3.)).text_color(theme::dim()).child(then))
                    }),
            )
            .with_priority(1)
        }))
        .into_any_element()
}

/// The button that opens the app's menus where the system has no menu
/// bar, as Back is drawn; they open below it on a press, as a menu bar's do.
fn menu_button() -> impl IntoElement {
    div()
        .id("menus")
        .w(px(28.))
        .h(px(28.))
        .mx(px(-6.))
        .rounded(px(6.))
        .flex()
        .items_center()
        .justify_center()
        .hover(theme::hovered)
        .active(theme::pressed)
        .on_mouse_down(gpui::MouseButton::Left, |_, window, cx| {
            // The press must not start a window drag.
            cx.stop_propagation();
            let at = gpui::point(px(platform::CHROME.padding.0 - 6.), px(TITLE_BAR - 10.));
            crate::drawn::app_menus(at, window, cx);
        })
        .child(icon("menu", 16., theme::dim()))
}

/// Standard Windows 11 caption button width and compact title-bar height, in points.
pub(crate) const CAPTION_BUTTON_WIDTH: f32 = 46.;
pub(crate) const CAPTION_HEIGHT: f32 = 32.;

/// Windows draws no caption buttons on a window whose title bar is the
/// app's: GPUI leaves them to the app. These answer the system's hit test
/// as its own minimize, maximize and close (`WindowControlArea`), so the
/// system does the rest, Maximize's snap layouts included. The glyphs are
/// Segoe Fluent Icons', as Windows 11's own buttons have them.
fn caption_buttons() -> impl IntoElement {
    div()
        .flex()
        .ml(px(8.))
        .child(caption_button(
            "minimize",
            "\u{E921}",
            WindowControlArea::Min,
            TITLE_BAR,
        ))
        .child(caption_button(
            "maximize",
            "\u{E922}",
            WindowControlArea::Max,
            TITLE_BAR,
        ))
        .child(close_button(TITLE_BAR))
}

/// A single close caption button for a non-resizable, non-minimizable
/// window whose title bar is the app's (such as Settings on Windows).
pub(crate) fn close_button(height: f32) -> impl IntoElement {
    caption_button("close", "\u{E8BB}", WindowControlArea::Close, height)
}

fn caption_button(
    id: &'static str,
    glyph: &'static str,
    area: WindowControlArea,
    height: f32,
) -> impl IntoElement {
    let close = area == WindowControlArea::Close;
    div()
        .id(id)
        .window_control_area(area)
        // GPUI answers the system's hit test with the first area under
        // the pointer, outermost first: the bar's caption, unless each
        // button hides what is behind it.
        .occlude()
        .w(px(CAPTION_BUTTON_WIDTH))
        .h(px(height))
        .flex()
        .items_center()
        .justify_center()
        .font_family("Segoe Fluent Icons")
        .text_size(px(10.))
        .text_color(theme::dim())
        .hover(move |style| {
            if close {
                style
                    .bg(gpui::rgb(0xc42b1c))
                    .text_color(gpui::rgb(0xffffff))
            } else {
                theme::hovered_ink(theme::hovered(style))
            }
        })
        .active(theme::pressed)
        .child(glyph)
}
