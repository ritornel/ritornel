//! Linux: everything is the fallback's but the server the app starts.
//!
//! Closing the window ends the app here, as it does on Linux, and must not
//! stop what is playing. So the server is not held by a pipe, as the
//! launcher's is (`ritornel_client::launch`): it is started apart from the
//! app, in a session of its own with nothing of the app's open in it, and
//! outlives it however it ends. It carries a mark in its environment, by
//! which a later run of the app knows it started it: Quit stops a server
//! with the mark, and leaves any other running.

// The session and the signals are reached through `unsafe`, each block
// saying why it is sound.
#![allow(unsafe_code)]

use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use ritornel_client::launch::Program;

pub(crate) use super::fallback::{
    CHROME, FEEL, FITTING_SHOWS, KEYS, SHOW_FOLDER, bring_forward_quietly, choose_folder,
    dismiss_menu, fit_window, footprint, give_back, hold_appearance, keep_out_of_the_dock,
    key_text, locale, memory_line, pop_up_menu, reduce_motion, screenshot, scroll_details,
    settings_dir, start, swiped_back,
};

/// The server outlives the app.
pub(crate) const SERVER_APART: bool = true;

/// The mark in a server's environment that says the app started it.
const MARK: (&str, &str) = ("RITORNEL_STARTED_BY_APP", "1");

/// Starts `program` serving `data_dir`, apart from the app: in a session of
/// its own, with its standard streams on nothing, so that no hangup, signal
/// to the app's group or closed pipe reaches it. It is found once it is up.
pub(crate) fn start_server(program: &Program, data_dir: &Path) -> std::io::Result<Child> {
    let mut command = Command::new(&program.path);
    command
        .arg("serve")
        .arg("--data-dir")
        .arg(data_dir)
        .args(&program.args)
        .envs(program.env.iter().map(|(name, value)| (name, value)))
        .env(MARK.0, MARK.1)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: `setsid` is async-signal-safe and touches no memory; it runs
    // in the child between fork and exec, where nothing else does.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command.spawn()
}

/// Whether the process `pid` is a server the app started.
pub(crate) fn started_here(pid: u32) -> bool {
    let mark = format!("{}={}", MARK.0, MARK.1);
    std::fs::read(format!("/proc/{pid}/environ")).is_ok_and(|environ| {
        environ
            .split(|byte| *byte == 0)
            .any(|each| each == mark.as_bytes())
    })
}

/// Asks the server `pid` to stop, as a signal to it does, waits up to
/// `wait` for it to end, and kills it if it has not. What it keeps is
/// durable, so a kill loses only what a crash would.
pub(crate) fn stop_server(pid: u32, wait: Duration) {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return;
    };
    // SAFETY: `kill` takes plain integers and touches no memory.
    let signal = |signal| unsafe { libc::kill(pid, signal) };
    if signal(libc::SIGTERM) != 0 {
        return;
    }
    let deadline = Instant::now() + wait;
    // Signal 0 asks only whether it is there.
    while signal(0) == 0 {
        if Instant::now() >= deadline {
            signal(libc::SIGKILL);
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}
