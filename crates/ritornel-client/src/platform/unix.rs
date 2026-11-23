//! Unix: a program started from one has no window to hide, and a server
//! holds the whole of its lock file.

use std::process::Command;

/// Starts `command` with no console window: on Unix there is none to make.
pub(crate) fn start_without_console(_command: &mut Command) {}

/// Asks the process `pid` to stop, as the system does at shutdown: a
/// SIGTERM, which a server answers by stopping cleanly. Only a process of
/// the same user can be asked. A pid no process can have, such as one past
/// what `kill` takes, which it would read as every process, is refused.
pub(crate) fn ask_to_stop(pid: u32) -> std::io::Result<()> {
    if pid == 0 || i32::try_from(pid).is_err() {
        return Err(std::io::Error::other(format!("{pid} is not a process")));
    }
    let program = ["/bin/kill", "/usr/bin/kill"]
        .into_iter()
        .find(|path| std::path::Path::new(path).exists())
        .unwrap_or("kill");
    let asked = Command::new(program)
        .args(["-TERM", &pid.to_string()])
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|error| std::io::Error::new(error.kind(), format!("{program}: {error}")))?;
    if asked.status.success() {
        Ok(())
    } else {
        let said = String::from_utf8_lossy(&asked.stderr).trim().to_owned();
        Err(std::io::Error::other(format!("{program}: {said}")))
    }
}

/// Tries to hold a lock file as a running server does: the whole file.
#[cfg(test)]
pub(crate) fn try_hold_as_a_server(file: &std::fs::File) -> bool {
    file.try_lock().is_ok()
}

/// A program that runs `unix`, a script for the shell, standing in for
/// another; `windows` is the same script for Windows. It is `fake-server`
/// beside this file, which runs the script it is given in its environment.
/// A test that wrote an executable and ran it at once could find it busy
/// (ETXTBSY) on Linux: a child that another test's thread forked meanwhile
/// holds it open for writing until that child runs its own program.
#[cfg(test)]
pub(crate) fn script(_dir: &std::path::Path, unix: &str, _windows: &str) -> crate::launch::Program {
    let mut program = crate::launch::Program::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/platform/fake-server"
    ));
    program
        .env
        .push(("RITORNEL_FAKE_SERVER".into(), unix.into()));
    program
}
