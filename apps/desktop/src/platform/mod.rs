//! Everything that differs from one system to another, behind one set of
//! plain functions and values. The rest of the app never names a system:
//! there is no `cfg` of an operating system outside this module.
//!
//! Each system has a file of its own (`mac.rs`, `windows.rs`), and each
//! offers the same names. What a system has no code of its own for, it takes
//! from `fallback`, the plain behaviour: no system menu, no stretch at a
//! page's ends, no picture of the window. A system with no file at all
//! (Linux, for now) takes the fallback whole.
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

#[cfg(not(any(target_os = "macos", windows)))]
use fallback as system;

pub(crate) use system::{
    CHROME, FEEL, FITTING_SHOWS, bring_forward_quietly, choose_folder, dismiss_menu, fit_window,
    footprint, give_back, hold_appearance, keep_out_of_the_dock, locale, memory_line, pop_up_menu,
    screenshot, scroll_details, settings_dir, start, swiped_back,
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

    use super::{Chrome, Feel, ScrollDetails};

    pub(crate) const CHROME: Chrome = Chrome {
        system_buttons: None,
        app_buttons: false,
        app_moves_window: false,
        padding: (12., 16.),
    };

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

    /// The system's context menu of `items` at `at` in the window, opened
    /// with the action numbered `under` beneath the pointer, as a pop-up
    /// button's is: the number of the action chosen, or `None`.
    pub(crate) async fn pop_up_menu(
        _window: AnyWindowHandle,
        _items: Vec<super::MenuItem>,
        _under: Option<usize>,
        _at: Point<Pixels>,
        _cx: &mut AsyncApp,
    ) -> Option<usize> {
        None
    }

    /// Puts away the menu open, for a script.
    pub(crate) fn dismiss_menu() {}

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
    ) -> Option<PathBuf> {
        let chosen = cx.update(|cx| {
            cx.prompt_for_paths(PathPromptOptions {
                files: false,
                directories: true,
                multiple: false,
                prompt: Some(prompt.into()),
            })
        });
        chosen.await.ok()?.ok()??.into_iter().next()
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
