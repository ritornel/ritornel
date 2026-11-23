//! Windows: its window chrome and feel, and the few things done through
//! Win32 directly: the server the app starts, the process's memory as Task
//! Manager counts it, and a picture of the window for checking by eye. Its
//! menus, the app's and the context menus, are the ones the app draws
//! (`drawn.rs`), as its keys are the fallback's.
//!
//! Closing the window ends the app here, as it does on Linux, and must not
//! stop what is playing. So the server is not held by a pipe, as the
//! launcher's is (`ritornel_client::launch`): it is started apart from the
//! app, with a console of its own that no one sees, out of the app's job
//! where the job allows it, and with nothing of the app's open in it, and
//! outlives it however it ends. It carries a mark, by which a later run of
//! the app knows it started it: Quit stops a server with the mark, and
//! leaves any other running.

// Win32 is reached through `unsafe`, each block saying why it is sound.
#![allow(unsafe_code)]

use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use ritornel_client::launch::Program;
use windows_sys::Win32::Foundation::{
    CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE, HWND, RECT, WAIT_OBJECT_0,
};
use windows_sys::Win32::System::ProcessStatus::{
    GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
};
use windows_sys::Win32::System::Threading::{
    CREATE_BREAKAWAY_FROM_JOB, CREATE_NO_WINDOW, CreateEventW, GetCurrentProcess, OpenEventW,
    OpenProcess, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, SYNCHRONIZATION_SYNCHRONIZE,
    TerminateProcess, WaitForSingleObject,
};
use windows_sys::Win32::UI::WindowsAndMessaging::GetWindowRect;

use super::{Chrome, Feel};

pub(crate) use super::fallback::{
    FITTING_SHOWS, KEYS, bring_forward_quietly, choose_folder, dismiss_menu, fit_window, give_back,
    hold_appearance, keep_out_of_the_dock, key_text, locale, pop_up_menu, reduce_motion,
    scroll_details, start, swiped_back,
};

/// The bar is the caption, by the system's hit test, and the app draws the
/// window's buttons at its right for the system to treat as its own.
pub(crate) const CHROME: Chrome = Chrome {
    system_buttons: None,
    app_buttons: true,
    app_moves_window: false,
    padding: (12., 0.),
    menu_bar: false,
};

/// Windows' pages stop at their ends.
pub(crate) const FEEL: Feel = Feel { rubber_band: false };

/// The server outlives the app.
pub(crate) const SERVER_APART: bool = true;

/// What a button that shows a folder in File Explorer says.
pub(crate) const SHOW_FOLDER: &str = "Show in File Explorer";

/// The mark in a server's environment that says the app started it, as on
/// Linux. Windows offers no way to read another process's environment, so
/// the mark that is read is a named event the server holds (`mark_name`),
/// which lasts exactly as long as the server does.
const MARK: (&str, &str) = ("RITORNEL_STARTED_BY_APP", "1");

/// The name of the event that marks the server `pid` as the app's, in the
/// session's namespace.
fn mark_name(pid: u32) -> Vec<u16> {
    format!("Local\\ritornel-started-by-app-{pid}")
        .encode_utf16()
        .chain([0])
        .collect()
}

/// Starts `program` serving `data_dir`, apart from the app: with a console
/// of its own that no one sees, so that nothing done to the app's console
/// reaches it; out of the app's job where the job allows it, so that a job
/// that ends its processes with the app's launcher leaves it; and with its
/// standard streams on nothing, so that no closed pipe ends it. It is found
/// once it is up.
pub(crate) fn start_server(program: &Program, data_dir: &Path) -> std::io::Result<Child> {
    let spawn = |flags| {
        Command::new(&program.path)
            .creation_flags(flags)
            .arg("serve")
            .arg("--data-dir")
            .arg(data_dir)
            .args(&program.args)
            .envs(program.env.iter().map(|(name, value)| (name, value)))
            .env(MARK.0, MARK.1)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
    };
    let child =
        spawn(CREATE_NO_WINDOW | CREATE_BREAKAWAY_FROM_JOB).or_else(|_| spawn(CREATE_NO_WINDOW))?;
    let name = mark_name(child.id());
    // SAFETY: a name that ends in a nul, the live handle of the child just
    // spawned, and the event's own handle, closed here once the child holds
    // a copy of it, which it keeps until it ends.
    unsafe {
        let event = CreateEventW(std::ptr::null(), 1, 0, name.as_ptr());
        if !event.is_null() {
            let mut held: HANDLE = std::ptr::null_mut();
            DuplicateHandle(
                GetCurrentProcess(),
                event,
                child.as_raw_handle() as HANDLE,
                &mut held,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            );
            CloseHandle(event);
        }
    }
    Ok(child)
}

/// Whether the process `pid` is a server the app started: whether the
/// event that marks it is there, which only that server holds.
pub(crate) fn started_here(pid: u32) -> bool {
    let name = mark_name(pid);
    // SAFETY: a name that ends in a nul; the handle is closed at once.
    unsafe {
        let event = OpenEventW(SYNCHRONIZATION_SYNCHRONIZE, 0, name.as_ptr());
        if event.is_null() {
            return false;
        }
        CloseHandle(event);
    }
    true
}

/// Asks the server `pid` to stop, as the system asks a program to end
/// (`taskkill` without `/F` closes its windows, and the server's hidden one
/// stops it), waits up to `wait` for it to end, and kills it if it has not.
/// What it keeps is durable, so a kill loses only what a crash would.
pub(crate) fn stop_server(pid: u32, wait: Duration) {
    // SAFETY: a plain pid; the handle is checked here and closed below.
    let process = unsafe { OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_TERMINATE, 0, pid) };
    if process.is_null() {
        return;
    }
    let _ = Command::new("taskkill")
        .creation_flags(CREATE_NO_WINDOW)
        .args(["/PID", &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let wait = u32::try_from(wait.as_millis()).unwrap_or(u32::MAX);
    // SAFETY: the process handle opened above, with the rights these need.
    unsafe {
        if WaitForSingleObject(process, wait) != WAIT_OBJECT_0 {
            TerminateProcess(process, 1);
        }
        CloseHandle(process);
    }
}

pub(crate) fn settings_dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(PathBuf::from)
}

/// The working set, in bytes.
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

/// The working set and the private bytes, in bytes.
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
