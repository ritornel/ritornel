//! The Mac: its window chrome and feel, and the few things done through
//! AppKit directly: GPUI's window, bringing it forward without taking focus
//! from whoever is at the machine, a picture of it for checking by eye, the
//! process's memory, the system's context menu (`mac_menu.rs`), the
//! Settings window's height and folder picker (`mac_window.rs`), and what a
//! touchpad's scroll says that GPUI does not pass on.

// AppKit and the kernel are reached through `unsafe`, each block saying why
// it is sound.
#![allow(unsafe_code)]

use std::path::PathBuf;

use gpui::{AnyWindowHandle, AsyncApp, Pixels, Point};
use objc2::rc::Retained;
use objc2_app_kit::{NSView, NSWindow};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use super::{Chrome, Feel, Keys, ScrollDetails, ScrollPhase, held};

// The app ends only when it is quit, and its server with it, held by the
// launcher's pipe.
pub(crate) use super::fallback::{SERVER_APART, start_server, started_here, stop_server};

/// The three window buttons at the bar's left, 14 points each and 9 apart,
/// are the system's; a press on the bar moves the window, as a title bar's does.
pub(crate) const CHROME: Chrome = Chrome {
    system_buttons: Some(60.),
    app_buttons: false,
    app_moves_window: true,
    padding: (20., 16.),
    menu_bar: true,
};

/// A Mac's keys: ⌘[ and ⌘← are Back, ⌥← and ⌥→ skip, and a field moves by
/// words with ⌥ and to the line's ends with ⌘, as a Mac's own fields do.
pub(crate) const KEYS: Keys = Keys {
    quit: &["cmd-q"],
    hide: &["cmd-h"],
    hide_others: &["alt-cmd-h"],
    minimize: &["cmd-m"],
    back: &["cmd-[", "cmd-left"],
    skip: held(false, true),
    word_left: &["alt-left"],
    word_right: &["alt-right"],
    select_word_left: &["alt-shift-left"],
    select_word_right: &["alt-shift-right"],
    delete_word_left: &["alt-backspace"],
    delete_to_start: &["cmd-backspace"],
    line_start: &["cmd-left"],
    line_end: &["cmd-right"],
    select_line_start: &["cmd-shift-left"],
    select_line_end: &["cmd-shift-right"],
    character_palette: &["ctrl-cmd-space"],
};

/// A key as a Mac's menus show it: `⌘F`.
pub(crate) fn key_text(keystroke: &gpui::KeybindingKeystroke) -> String {
    keystroke.display(gpui::OperatingSystem::Mac).to_string()
}

/// A Mac's pages stretch at their ends under a touchpad.
pub(crate) const FEEL: Feel = Feel { rubber_band: true };

/// Whether System Settings › Accessibility › Display › Reduce motion is on.
pub(crate) fn reduce_motion() -> bool {
    objc2_app_kit::NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion()
}

pub(crate) fn start() {
    watch_scrolls();
}

pub(crate) fn settings_dir() -> Option<PathBuf> {
    super::home().map(|home| home.join("Library/Application Support"))
}

/// The system's context menu, popped up over GPUI's view (`mac_menu.rs`).
pub(crate) async fn pop_up_menu(
    window: AnyWindowHandle,
    items: Vec<super::MenuItem>,
    under: Option<usize>,
    at: Point<Pixels>,
    cx: &mut AsyncApp,
) -> Option<usize> {
    let view = window
        .update(cx, |_, window, _| view(window))
        .ok()
        .flatten()?;
    super::mac_menu::pop_up(view, items, under, f32::from(at.x), f32::from(at.y)).await
}

pub(crate) fn dismiss_menu(_cx: &mut gpui::App) {
    super::mac_menu::dismiss();
}

/// A window opened hidden is shown once it is first fitted (`fit_window`),
/// so it never appears at a height it then leaves.
pub(crate) const FITTING_SHOWS: bool = true;

/// What a button that shows a folder in Finder says.
pub(crate) const SHOW_FOLDER: &str = "Show in Finder";

/// Makes the window's content `height` points tall, keeping its top where
/// it is, and shows it if it is hidden (`mac_window.rs`).
pub(crate) fn fit_window(window: &mut gpui::Window, height: f32, quietly: bool) {
    if let Some(ns) = ns_window(window) {
        super::mac_window::fit(ns, f64::from(height), quietly);
    }
}

/// The system's folder picker, as a sheet on the window.
pub(crate) async fn choose_folder(
    window: AnyWindowHandle,
    prompt: String,
    message: String,
    cx: &mut AsyncApp,
) -> Result<Option<PathBuf>, String> {
    let ns = window
        .update(cx, |_, window, _| ns_window(window))
        .map_err(|err| err.to_string())?
        .ok_or_else(|| "the window closed".to_owned())?;
    super::mac_window::choose_folder(ns, prompt, message).await
}

/// The language the system prefers, as a BCP 47 tag.
pub(crate) fn locale() -> Option<String> {
    super::mac_window::locale()
}

/// Dresses the window light or dark, whatever the system's appearance.
pub(crate) fn hold_appearance(window: &gpui::Window, dark: bool) {
    if let Some(ns) = ns_window(window) {
        super::mac_window::hold_appearance(&ns, dark);
    }
}

/// GPUI's own `NSView` for a window.
pub(crate) fn view(window: &gpui::Window) -> Option<Retained<NSView>> {
    let handle = HasWindowHandle::window_handle(window).ok()?;
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
        return None;
    };
    // SAFETY: GPUI hands out its live content NSView, and windows are only
    // touched on the main thread.
    unsafe { Retained::retain(appkit.ns_view.as_ptr().cast::<NSView>()) }
}

pub(crate) fn ns_window(window: &gpui::Window) -> Option<Retained<NSWindow>> {
    view(window)?.window()
}

/// In front, without becoming the active app: GPUI stops drawing a window
/// that is covered.
/// An accessory app: no icon in the Dock, and the menu bar stays the
/// frontmost app's.
pub(crate) fn keep_out_of_the_dock() {
    if let Some(mtm) = objc2::MainThreadMarker::new() {
        objc2_app_kit::NSApplication::sharedApplication(mtm)
            .setActivationPolicy(objc2_app_kit::NSApplicationActivationPolicy::Accessory);
    }
}

pub(crate) fn bring_forward_quietly(window: &gpui::Window) {
    if let Some(window) = ns_window(window) {
        window.orderFrontRegardless();
    }
}

/// Saves a picture of the window alone, without its shadow.
pub(crate) fn screenshot(window: &gpui::Window, path: &std::path::Path) {
    let Some(ns) = ns_window(window) else { return };
    let number = ns.windowNumber();
    let _ = std::process::Command::new("screencapture")
        .arg("-x")
        .arg("-o")
        .arg(format!("-l{number}"))
        .arg(path)
        .status();
}

/// The process's physical footprint, as Activity Monitor's Memory column
/// shows it, in bytes.
pub(crate) fn footprint() -> Option<u64> {
    let mut info: libc::rusage_info_v2 = unsafe { std::mem::zeroed() };
    let pointer = (&raw mut info).cast::<libc::rusage_info_t>();
    // SAFETY: the buffer is a rusage_info_v2, the flavour asked for.
    let status =
        unsafe { libc::proc_pid_rusage(std::process::id() as i32, libc::RUSAGE_INFO_V2, pointer) };
    (status == 0).then_some(info.ri_phys_footprint)
}

/// Hands freed memory back to the system: macOS's allocator keeps the pages
/// of freed covers otherwise, and they count in the footprint.
pub(crate) fn give_back() {
    unsafe extern "C" {
        fn malloc_zone_pressure_relief(zone: *mut std::ffi::c_void, goal: usize) -> usize;
    }
    // SAFETY: a null zone means every zone; a goal of 0 means as much as can be.
    unsafe {
        malloc_zone_pressure_relief(std::ptr::null_mut(), 0);
    }
}

thread_local! {
    /// What AppKit said of the scroll it last delivered. GPUI's event
    /// leaves out its gesture and momentum phases as they are (GPUI folds a
    /// touch that lifts without scrolling into a move, and momentum into
    /// moves) and its unaccelerated delta, which WebKit stretches a page by.
    static LAST_SCROLL: std::cell::Cell<Option<ScrollDetails>> = const { std::cell::Cell::new(None) };
}

thread_local! {
    /// The swipe across under way, and whether the scroll being handled now
    /// asks to go back.
    static SWIPE: std::cell::RefCell<crate::swipe::Gesture> = std::cell::RefCell::default();
    static SWIPED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether the scroll being handled asks to go back. The monitor sees every
/// window's scrolls, and the main window hears only its own, so the answer
/// holds for that one scroll: a swipe in Settings, or over something that
/// keeps its scroll, does not wait to send the main window back on the next.
pub(crate) fn swiped_back() -> bool {
    SWIPED.with(|swiped| swiped.replace(false))
}

/// From `NSEventPhase`'s bits.
fn phase(bits: usize) -> ScrollPhase {
    match bits {
        1 => ScrollPhase::Began,
        2 | 4 => ScrollPhase::Changed,
        8 => ScrollPhase::Ended,
        16 => ScrollPhase::Cancelled,
        32 => ScrollPhase::MayBegin,
        _ => ScrollPhase::None,
    }
}

/// Reads every scroll as AppKit has it, before GPUI is given it. Once.
fn watch_scrolls() {
    use objc2::runtime::AnyObject;
    use objc2::{msg_send, sel};
    use objc2_app_kit::{NSEvent, NSEventMask};

    let block = block2::RcBlock::new(|event: std::ptr::NonNull<NSEvent>| -> *mut NSEvent {
        // SAFETY: AppKit hands the monitor a live event, on the main thread.
        let ns = unsafe { event.as_ref() };
        let object: &AnyObject = ns;
        // SAFETY: `respondsToSelector:` is NSObject's; the private getter is
        // only called when the event answers to it, and returns a CGFloat.
        let answers: bool =
            unsafe { msg_send![object, respondsToSelector: sel!(_unacceleratedScrollingDeltaY)] };
        let unaccelerated: f64 = if answers {
            unsafe { msg_send![object, _unacceleratedScrollingDeltaY] }
        } else {
            ns.scrollingDeltaY()
        };
        LAST_SCROLL.with(|last| {
            last.set(Some(ScrollDetails {
                phase: phase(ns.phase().0),
                momentum: phase(ns.momentumPhase().0),
                unaccelerated_dy: -(unaccelerated as f32),
                timestamp: ns.timestamp(),
            }));
        });
        // A swipe across, for Back (`swipe.rs`): with natural scrolling the
        // delta is the way the fingers went; without it, the other way.
        let gesture = if !ns.momentumPhase().is_empty() {
            crate::swipe::Phase::Other
        } else if ns.phase().contains(objc2_app_kit::NSEventPhase::Began) {
            crate::swipe::Phase::Began
        } else if ns.phase().contains(objc2_app_kit::NSEventPhase::Changed) {
            crate::swipe::Phase::Changed
        } else {
            crate::swipe::Phase::Other
        };
        let across = ns.scrollingDeltaX();
        let right = if ns.isDirectionInvertedFromDevice() {
            across
        } else {
            -across
        };
        let back = SWIPE.with(|swipe| {
            swipe
                .borrow_mut()
                .scroll(gesture, right, ns.scrollingDeltaY())
        });
        SWIPED.with(|swiped| swiped.set(back));
        event.as_ptr()
    });
    // SAFETY: the block returns the event it was given, a valid pointer.
    let monitor = unsafe {
        NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::ScrollWheel, &block)
    };
    // The monitor lives as long as the app.
    std::mem::forget(monitor);
}

/// The scroll AppKit last delivered: the one GPUI is handling now.
pub(crate) fn scroll_details() -> Option<ScrollDetails> {
    LAST_SCROLL.with(std::cell::Cell::get)
}

/// The footprint two ways: the kernel's `ri_phys_footprint`, which counts
/// graphics memory the system could reclaim, and the `footprint` tool's
/// total, which Activity Monitor's figure is closer to.
pub(crate) fn memory_line() -> String {
    let kernel = footprint().map_or(-1., |bytes| bytes as f64 / 1024. / 1024.);
    let tool = std::process::Command::new("footprint")
        .arg("-p")
        .arg(std::process::id().to_string())
        .output()
        .ok()
        .and_then(|out| {
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .find(|line| line.contains("Footprint:"))
                .and_then(|line| line.split("Footprint:").nth(1))
                .map(|rest| rest.split('(').next().unwrap_or("").trim().to_owned())
        })
        .unwrap_or_default();
    format!("phys_footprint {kernel:.0} MB, footprint tool {tool}")
}
