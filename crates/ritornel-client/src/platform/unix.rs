//! Unix: a program started from one has no window to hide, and a server
//! holds the whole of its lock file.

use std::process::Command;

/// Starts `command` with no console window: on Unix there is none to make.
pub(crate) fn start_without_console(_command: &mut Command) {}

/// Asks the process `pid` to stop, as the system does at shutdown: a
/// SIGTERM, which a server answers by stopping cleanly. Only a process of
/// the same user can be asked.
pub(crate) fn ask_to_stop(pid: u32) -> std::io::Result<()> {
    let asked = Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?;
    if asked.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "kill could not signal process {pid}"
        )))
    }
}

/// Tries to hold a lock file as a running server does: the whole file.
#[cfg(test)]
pub(crate) fn try_hold_as_a_server(file: &std::fs::File) -> bool {
    file.try_lock().is_ok()
}

/// A program at `path` that is a script for the shell, `unix`, standing in
/// for another; `windows` is the same script for Windows.
#[cfg(test)]
pub(crate) fn script(path: &std::path::Path, unix: &str, _windows: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, format!("#!/bin/sh\n{unix}\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path.to_owned()
}
