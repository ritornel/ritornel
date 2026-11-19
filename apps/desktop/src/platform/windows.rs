//! Windows: its window chrome and feel, and the few things done through
//! Win32 directly: the process's memory as Task Manager counts it, and a
//! picture of the window for checking by eye. Its context menu would be a
//! `TrackPopupMenu` over the window's HWND, not written yet.

// Win32 is reached through `unsafe`, each block saying why it is sound.
#![allow(unsafe_code)]

use std::path::PathBuf;

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows_sys::Win32::Foundation::{HWND, RECT};
use windows_sys::Win32::System::ProcessStatus::{
    GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
};
use windows_sys::Win32::System::Threading::GetCurrentProcess;
use windows_sys::Win32::UI::WindowsAndMessaging::GetWindowRect;

use super::{Chrome, Feel};

pub(crate) use super::fallback::{
    FITTING_SHOWS, bring_forward_quietly, choose_folder, dismiss_menu, fit_window, give_back,
    hold_appearance, keep_out_of_the_dock, locale, pop_up_menu, reduce_motion, scroll_details,
    start, swiped_back,
};

/// The bar is the caption, by the system's hit test, and the app draws the
/// window's buttons at its right for the system to treat as its own.
pub(crate) const CHROME: Chrome = Chrome {
    system_buttons: None,
    app_buttons: true,
    app_moves_window: false,
    padding: (12., 0.),
};

/// Windows' pages stop at their ends.
pub(crate) const FEEL: Feel = Feel { rubber_band: false };

pub(crate) fn settings_dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(PathBuf::from)
}

/// The working set, which is what the bakeoff's Windows report compared.
pub(crate) fn footprint() -> Option<u64> {
    memory().map(|(working_set, _)| working_set)
}

pub(crate) fn memory_line() -> String {
    let mb = |bytes: u64| bytes as f64 / 1024. / 1024.;
    match memory() {
        Some((working_set, private)) => format!(
            "working set {:.0} MB, private bytes {:.0} MB",
            mb(working_set),
            mb(private)
        ),
        None => "unknown".to_owned(),
    }
}

/// The working set and the private bytes, in bytes: the counters the
/// bakeoff's Windows report used.
fn memory() -> Option<(u64, u64)> {
    let mut counters: PROCESS_MEMORY_COUNTERS_EX = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
    counters.cb = size;
    // SAFETY: the buffer is a PROCESS_MEMORY_COUNTERS_EX of the size given.
    let ok = unsafe {
        GetProcessMemoryInfo(
            GetCurrentProcess(),
            (&raw mut counters).cast::<PROCESS_MEMORY_COUNTERS>(),
            size,
        )
    };
    (ok != 0).then_some((counters.WorkingSetSize as u64, counters.PrivateUsage as u64))
}

/// Saves a picture of the window's rectangle on screen. The window must be
/// in front: what covers it is in the picture.
pub(crate) fn screenshot(window: &gpui::Window, path: &std::path::Path) {
    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::Win32(win32) = handle.as_raw() else {
        return;
    };
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    // SAFETY: GPUI's live HWND, and a RECT to fill.
    if unsafe { GetWindowRect(win32.hwnd.get() as HWND, &mut rect) } == 0 {
        return;
    }
    let (w, h) = (rect.right - rect.left, rect.bottom - rect.top);
    let script = format!(
        "Add-Type -AssemblyName System.Drawing; $b = New-Object System.Drawing.Bitmap {w},{h}; \
         $g = [System.Drawing.Graphics]::FromImage($b); $g.CopyFromScreen({},{},0,0,$b.Size); \
         $b.Save('{}', [System.Drawing.Imaging.ImageFormat]::Png)",
        rect.left,
        rect.top,
        path.display()
    );
    let _ = std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", &script])
        .status();
}
