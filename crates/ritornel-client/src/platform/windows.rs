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

/// Asks the process `pid` to stop, as the system asks a program to end:
/// `taskkill` without `/F` closes its windows, and the server's hidden one
/// stops it cleanly. Only a process the user may end can be asked.
pub(crate) fn ask_to_stop(pid: u32) -> std::io::Result<()> {
    if pid == 0 {
        return Err(std::io::Error::other("0 is not a process"));
    }
    // The system's own, not one a folder on the path may hold.
    let program = std::env::var_os("SystemRoot").map_or_else(
        || std::path::PathBuf::from("taskkill"),
        |root| {
            std::path::Path::new(&root)
                .join("System32")
                .join("taskkill.exe")
        },
    );
    let mut command = Command::new(&program);
    start_without_console(&mut command);
    let asked = command
        .args(["/PID", &pid.to_string()])
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|error| std::io::Error::new(error.kind(), format!("taskkill: {error}")))?;
    if asked.status.success() {
        Ok(())
    } else {
        let said = String::from_utf8_lossy(&asked.stderr).trim().to_owned();
        Err(std::io::Error::other(format!("taskkill: {said}")))
    }
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

/// A program in `dir` that is a batch file, `windows`, standing in for
/// another; `unix` is the same script for the shell.
#[cfg(test)]
pub(crate) fn script(dir: &std::path::Path, _unix: &str, windows: &str) -> crate::launch::Program {
    let path = dir.join("fake-server.cmd");
    let lines: Vec<&str> = windows.lines().collect();
    std::fs::write(&path, format!("@echo off\r\n{}\r\n", lines.join("\r\n"))).unwrap();
    crate::launch::Program::new(path)
}
