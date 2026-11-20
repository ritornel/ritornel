//! Windows: a console program started by one without a console gets a
//! window unless told not to, and a server holds one byte past the end of
//! its lock file.

use std::process::Command;

/// `CREATE_NO_WINDOW`: the process gets no console window.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Starts `command` with no console window. A console program started by
/// one without a console, as the desktop app is, gets a console window of
/// its own unless told not to make one.
pub(crate) fn start_without_console(command: &mut Command) {
    std::os::windows::process::CommandExt::creation_flags(command, CREATE_NO_WINDOW);
}

/// Tries to hold a lock file as a running server does: the one byte past
/// its end that a server holds, which leaves the pid in it readable
/// (`local::WINDOWS_LOCK_BYTE`).
#[cfg(test)]
pub(crate) fn try_hold_as_a_server(file: &std::fs::File) -> bool {
    use ritornel_contract::local;
    ritornel_windows::lock::try_lock(
        file,
        local::WINDOWS_LOCK_BYTE,
        1,
        ritornel_windows::lock::Hold::Exclusive,
    )
    .unwrap_or(false)
}

/// A program beside `path` that is a batch file, `windows`, standing in for
/// another; `unix` is the same script for the shell.
#[cfg(test)]
pub(crate) fn script(path: &std::path::Path, _unix: &str, windows: &str) -> std::path::PathBuf {
    let path = path.with_extension("cmd");
    let lines: Vec<&str> = windows.lines().collect();
    std::fs::write(&path, format!("@echo off\r\n{}\r\n", lines.join("\r\n"))).unwrap();
    path
}
