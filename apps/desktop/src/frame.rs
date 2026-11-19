//! The frame: the title bar with its tabs (the player bar is `bar.rs`).
//! And the menu bar, which is the system's.

use gpui::{
    App, FontWeight, InteractiveElement, IntoElement, KeyBinding, Menu, MenuItem, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, WindowControlArea, actions, div,
    prelude::FluentBuilder, px,
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

pub(crate) fn menus(cx: &mut App) {
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-h", Hide, None),
        KeyBinding::new("alt-cmd-h", HideOthers, None),
        KeyBinding::new("cmd-[", Back, None),
        KeyBinding::new("cmd-left", Back, None),
        KeyBinding::new("cmd-m", Minimize, None),
        KeyBinding::new("cmd-f", Find, None),
        KeyBinding::new("shift-cmd-f", LeanBack, None),
        KeyBinding::new("alt-cmd-u", ToggleUpNext, None),
    ]);
    cx.set_menus([
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
        ]),
        // Up Next and Lean Back at Music's keys, as the real app's View menu has them.
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

/// The title bar, as the real `TitleBar.svelte` has it: a 52-point row in
/// three columns, the window's buttons and Back and the tabs at the left,
/// the server's name at the right of the middle, and the search field in a
/// column of its own, 320 points wide and giving way to 180 in a narrow
/// window.
/// Goes to a tab, as clicking it does.
pub(crate) type OnTab = std::rc::Rc<dyn Fn(Tab, &mut gpui::Window, &mut App)>;

pub(crate) fn title_bar(
    can_go_back: bool,
    lit: Tab,
    // The server's name, or `None` when there is no connection.
    server: Option<SharedString>,
    search: gpui::AnyElement,
    on_back: impl Fn(&gpui::ClickEvent, &mut gpui::Window, &mut App) + 'static,
    on_tab: OnTab,
) -> impl IntoElement {
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
                base.hover(|style| style.text_color(theme::ink()))
            })
            .px(px(11.))
            .py(px(5.))
            .rounded(px(8.))
            .whitespace_nowrap();
        let base = if on { base.bg(theme::fill()) } else { base };
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
                    .when(on, |label| label.text_color(theme::ink()))
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
                            back.hover(|style| style.text_color(theme::ink()))
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
                .child(
                    div()
                        .size(px(7.))
                        .flex_none()
                        .rounded_full()
                        .bg(if server.is_some() {
                            gpui::rgb(0x3fae6a)
                        } else {
                            theme::fill2()
                        }),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .child(server.unwrap_or_else(|| "Not connected".into())),
                ),
        )
        .child(search)
        .when(platform::CHROME.app_buttons, |bar| {
            bar.child(caption_buttons())
        })
}

/// Windows draws no caption buttons on a window whose title bar is the
/// app's: GPUI leaves them to the app. These answer the system's hit test
/// as its own minimize, maximize and close (`WindowControlArea`), so the
/// system does the rest, Maximize's snap layouts included. The glyphs are
/// Segoe Fluent Icons', as Windows 11's own buttons have them.
fn caption_buttons() -> impl IntoElement {
    let button = |id: &'static str, glyph: &'static str, area: WindowControlArea, close: bool| {
        div()
            .id(id)
            .window_control_area(area)
            .w(px(46.))
            .h(px(TITLE_BAR))
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
                    style.bg(theme::fill()).text_color(theme::ink())
                }
            })
            .child(glyph)
    };
    div()
        .flex()
        .ml(px(8.))
        .child(button(
            "minimize",
            "\u{E921}",
            WindowControlArea::Min,
            false,
        ))
        .child(button(
            "maximize",
            "\u{E922}",
            WindowControlArea::Max,
            false,
        ))
        .child(button("close", "\u{E8BB}", WindowControlArea::Close, true))
}
