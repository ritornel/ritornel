//! Everything that differs from one system to another, behind one set of
//! plain functions and values. The rest of the app never names a system:
//! there is no `cfg` of an operating system outside this module.
//!
//! Each system has a file of its own (`mac.rs`, `windows.rs`), and each
//! offers the same names. What a system has no code of its own for, it takes
//! from `fallback`, the plain behaviour: menus the app draws itself
//! (`drawn.rs`), Ctrl's keys, a server held by the launcher's pipe, no
//! stretch at a page's ends, no picture of the window. Linux (`linux.rs`)
//! takes all of it but the server, which outlives the app there, as it
//! does on Windows.
//!
//! What is decided here is the system's feel as well as its plumbing: a Mac
//! page stretches at its ends because a Mac's do, not because the app's do
//! (`FEEL`), and each system's window keeps its own buttons (`CHROME`).
//! Behaviour is ported from the system's own code where it matters: the
//! stretch is WebKit's, its constants read from AppKit (`elastic.rs`).

use std::path::PathBuf;

#[cfg(target_os = "macos")]
mod mac;
#[cfg(target_os = "macos")]
mod mac_menu;
#[cfg(target_os = "macos")]
mod mac_window;
#[cfg(target_os = "macos")]
use mac as system;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as system;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux as system;

#[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
use fallback as system;

pub(crate) use system::{
    CHROME, FEEL, FITTING_SHOWS, KEYS, SERVER_APART, bring_forward_quietly, choose_folder,
    dismiss_menu, fit_window, footprint, give_back, hold_appearance, keep_out_of_the_dock,
    key_text, locale, memory_line, pop_up_menu, screenshot, scroll_details, settings_dir, start,
    start_server, started_here, stop_server, swiped_back,
};

/// One line of the system's context menu. Its actions are numbered in
/// order, those in a submenu where the submenu stands, and the number of
/// the one chosen is what the menu answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MenuItem {
    Separator,
    Action {
        title: String,
        enabled: bool,
        checked: bool,
    },
    Submenu {
        title: String,
        items: Vec<MenuItem>,
    },
}

/// How the system's window is dressed around the app's title bar.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Chrome {
    /// The width the system's own window buttons take at the bar's left
    /// (a Mac's three), which the app leaves empty.
    pub system_buttons: Option<f32>,
    /// Whether the app draws the window's buttons at the bar's right, for
    /// the system to treat as its own (Windows' minimize, maximize, close).
    pub app_buttons: bool,
    /// Whether a press on the bar moves the window by the app's hand: on a
    /// Mac it must, on Windows the system's hit test does it.
    pub app_moves_window: bool,
    /// The bar's padding at its left and right.
    pub padding: (f32, f32),
    /// Whether the system shows the app's menus in a menu bar of its own (a
    /// Mac's), with its own items in the app's menu (Services, Hide, Show
    /// All). Without one, a button at the bar's left opens them.
    pub menu_bar: bool,
}

/// The keys that differ from one system to another. A key that is the same
/// idea everywhere under the system's own modifier is bound once, as
/// `secondary-` (⌘ on a Mac, Ctrl elsewhere), and is not here. Each list may
/// be empty: the system has no key for it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Keys {
    pub quit: &'static [&'static str],
    pub hide: &'static [&'static str],
    pub hide_others: &'static [&'static str],
    pub minimize: &'static [&'static str],
    /// Back, besides the mouse's back button.
    pub back: &'static [&'static str],
    /// What is held with ← or → to go to the previous or next song.
    pub skip: gpui::Modifiers,
    /// A text field's: a word at a time, to the line's ends, and what
    /// deletes as far. Home and End are the same everywhere.
    pub word_left: &'static [&'static str],
    pub word_right: &'static [&'static str],
    pub select_word_left: &'static [&'static str],
    pub select_word_right: &'static [&'static str],
    pub delete_word_left: &'static [&'static str],
    pub delete_to_start: &'static [&'static str],
    pub line_start: &'static [&'static str],
    pub line_end: &'static [&'static str],
    pub select_line_start: &'static [&'static str],
    pub select_line_end: &'static [&'static str],
    pub character_palette: &'static [&'static str],
}

/// Modifiers as a constant: `Modifiers::default()` is not one.
#[allow(dead_code, reason = "each system names its own")]
const fn held(control: bool, alt: bool) -> gpui::Modifiers {
    gpui::Modifiers {
        control,
        alt,
        shift: false,
        platform: false,
        function: false,
    }
}

/// How things feel on the system.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Feel {
    /// Whether a page stretches past its ends under a touchpad.
    pub rubber_band: bool,
}

/// A scroll's phase, as AppKit gives a gesture's and its momentum's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScrollPhase {
    None,
    #[allow(dead_code, reason = "only a Mac's touchpad says it")]
    MayBegin,
    Began,
    Changed,
    Ended,
    Cancelled,
}

/// What the system says of a scroll that GPUI's event leaves out.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ScrollDetails {
    pub phase: ScrollPhase,
    pub momentum: ScrollPhase,
    /// Down the page is positive.
    pub unaccelerated_dy: f32,
    /// Seconds, by the system's steady clock.
    pub timestamp: f64,
}

/// The plain behaviour, for what a system has no code of its own for.
#[allow(dead_code, reason = "each system takes only what it lacks")]
mod fallback {
    use std::path::{Path, PathBuf};

    use gpui::{AnyWindowHandle, AsyncApp, PathPromptOptions, Pixels, Point, px, size};

    use super::{Chrome, Feel, Keys, ScrollDetails, held};

    pub(crate) const CHROME: Chrome = Chrome {
        system_buttons: None,
        app_buttons: false,
        app_moves_window: false,
        padding: (12., 16.),
        menu_bar: false,
    };

    /// Ctrl's keys, as Linux's and Windows' own fields and apps have them:
    /// Alt+← is Back, as in a browser, and Ctrl+← and Ctrl+→ skip, as in
    /// the players there; a field moves by words with Ctrl. Nothing hides
    /// the app or minimizes the window from the keyboard but the system.
    pub(crate) const KEYS: Keys = Keys {
        quit: &["ctrl-q"],
        hide: &[],
        hide_others: &[],
        minimize: &[],
        back: &["alt-left"],
        skip: held(true, false),
        word_left: &["ctrl-left"],
        word_right: &["ctrl-right"],
        select_word_left: &["ctrl-shift-left"],
        select_word_right: &["ctrl-shift-right"],
        delete_word_left: &["ctrl-backspace"],
        delete_to_start: &[],
        line_start: &[],
        line_end: &[],
        select_line_start: &["shift-home"],
        select_line_end: &["shift-end"],
        character_palette: &[],
    };

    /// A key as a menu shows it: `Ctrl+Shift+F`.
    pub(crate) fn key_text(keystroke: &gpui::KeybindingKeystroke) -> String {
        let held = keystroke.modifiers();
        let mut parts: Vec<String> = [
            (held.control, "Ctrl"),
            (held.platform, "Super"),
            (held.alt, "Alt"),
            (held.shift, "Shift"),
        ]
        .into_iter()
        .filter(|(on, _)| *on)
        .map(|(_, name)| name.to_owned())
        .collect();
        let key = keystroke.key();
        let mut letters = key.chars();
        parts.push(match letters.next() {
            Some(first) => first.to_uppercase().chain(letters).collect(),
            None => String::new(),
        });
        parts.join("+")
    }

    pub(crate) const FEEL: Feel = Feel { rubber_band: false };

    /// Whatever the system needs set up once the app has started.
    pub(crate) fn start() {}

    pub(crate) fn scroll_details() -> Option<ScrollDetails> {
        None
    }

    /// Whether a swipe across has asked to go back since this was last asked.
    pub(crate) fn swiped_back() -> bool {
        false
    }

    /// Whether the system asks for motion to be reduced.
    pub(crate) fn reduce_motion() -> bool {
        false
    }

    /// A context menu of `items` at `at` in the window, opened with the
    /// action numbered `under` beneath the pointer, as a pop-up button's
    /// is: the number of the action chosen, or `None`. The app draws it
    /// (`drawn.rs`).
    pub(crate) async fn pop_up_menu(
        window: AnyWindowHandle,
        items: Vec<super::MenuItem>,
        under: Option<usize>,
        at: Point<Pixels>,
        cx: &mut AsyncApp,
    ) -> Option<usize> {
        crate::drawn::pop_up(window, items, under, at, cx).await
    }

    /// Puts away the menu open, for a script.
    pub(crate) fn dismiss_menu(cx: &mut gpui::App) {
        crate::drawn::dismiss(cx);
    }

    /// Whether the server the app starts outlives it. Here it does not:
    /// the launcher holds it by a pipe and it stops when the app ends,
    /// however it ends (`ritornel_client::launch`), and the app ends only
    /// when it is quit.
    pub(crate) const SERVER_APART: bool = false;

    /// Starts a server apart from the app, where `SERVER_APART` says one is.
    pub(crate) fn start_server(
        _program: &ritornel_client::launch::Program,
        _data_dir: &Path,
    ) -> std::io::Result<std::process::Child> {
        Err(std::io::ErrorKind::Unsupported.into())
    }

    /// Whether the process `pid` is a server the app started apart from it.
    pub(crate) fn started_here(_pid: u32) -> bool {
        false
    }

    /// Stops a server the app started apart from it.
    pub(crate) fn stop_server(_pid: u32, _wait: std::time::Duration) {}

    /// Whether a window opened hidden is shown by its first fit. Here it
    /// is opened shown, and fitted after.
    pub(crate) const FITTING_SHOWS: bool = false;

    /// Makes the window's content `height` points tall.
    pub(crate) fn fit_window(window: &mut gpui::Window, height: f32, _quietly: bool) {
        let width = window.viewport_size().width;
        window.resize(size(width, px(height)));
    }

    /// A folder, by the picker GPUI has for the system: not a sheet, and
    /// with no message, only the prompt.
    pub(crate) async fn choose_folder(
        _window: AnyWindowHandle,
        prompt: String,
        _message: String,
        cx: &mut AsyncApp,
    ) -> Result<Option<PathBuf>, String> {
        let chosen = cx.update(|cx| {
            cx.prompt_for_paths(PathPromptOptions {
                files: false,
                directories: true,
                multiple: false,
                prompt: Some(prompt.into()),
            })
        });
        picked_folder(chosen.await.map_err(|err| err.to_string())?)
    }

    fn picked_folder(
        chosen: gpui::Result<Option<Vec<PathBuf>>>,
    ) -> Result<Option<PathBuf>, String> {
        chosen
            .map(|paths| paths.and_then(|paths| paths.into_iter().next()))
            .map_err(|err| err.to_string())
    }

    /// The language the system prefers, as a BCP 47 tag, from the
    /// environment's locale: `en_GB.UTF-8` is `en-GB`.
    pub(crate) fn locale() -> Option<String> {
        let lang = std::env::var("LC_ALL")
            .ok()
            .filter(|value| !value.is_empty())
            .or_else(|| std::env::var("LANG").ok())?;
        let tag = lang.split('.').next()?.replace('_', "-");
        (!tag.is_empty() && tag != "C" && tag != "POSIX").then_some(tag)
    }

    /// Dresses the window light or dark: nothing here, where the app's
    /// palette is the whole of the window.
    pub(crate) fn hold_appearance(_window: &gpui::Window, _dark: bool) {}

    /// A picture of the window alone, for checking by eye.
    pub(crate) fn screenshot(_window: &gpui::Window, _path: &Path) {}

    /// The window in front without taking the keyboard from whoever is at the machine.
    pub(crate) fn bring_forward_quietly(_window: &gpui::Window) {}

    /// The app runs without a place in the Dock or the menu bar, for a test
    /// that must not be seen by whoever is at the machine.
    pub(crate) fn keep_out_of_the_dock() {}

    /// The process's memory as the system's own monitor counts it, in bytes.
    pub(crate) fn footprint() -> Option<u64> {
        None
    }

    /// The memory, in words, for the measure.
    pub(crate) fn memory_line() -> String {
        "unknown".to_owned()
    }

    /// Hands freed memory back to the system, where its allocator keeps it.
    pub(crate) fn give_back() {}

    /// Where the system keeps an app's settings.
    pub(crate) fn settings_dir() -> Option<PathBuf> {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
    }
    #[cfg(test)]
    mod picker_tests {
        use super::*;

        #[test]
        fn cancelling_is_quiet_and_a_failed_picker_keeps_its_reason() {
            assert_eq!(picked_folder(Ok(None)), Ok(None));
            assert_eq!(
                picked_folder(Err(std::io::Error::other("portal unavailable").into())),
                Err("portal unavailable".into())
            );
        }
    }
}

/// Whether the system asks for motion to be reduced: then pages simply
/// change. `RITORNEL_GPUI_REDUCE_MOTION` asks for it too, for checking
/// without changing the system's setting (developer-only).
pub(crate) fn reduce_motion() -> bool {
    std::env::var_os("RITORNEL_GPUI_REDUCE_MOTION").is_some() || system::reduce_motion()
}

/// The program of the server that goes beside the app's.
pub(crate) fn server_program() -> String {
    format!("ritornel-server{}", std::env::consts::EXE_SUFFIX)
}

#[allow(dead_code, reason = "used where a system needs it")]
fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}
