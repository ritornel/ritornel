//! What the tests that start a `ritornel-server` share.

use std::process::Child;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

/// Says, while a test is failing, why its server is gone if it is: how it
/// ended, which names the signal when one ended it, and what it wrote to
/// stderr, from `stderr` when the test reads it: the pipe ends with the
/// process, so what is still on its way is waited for, five seconds at
/// most in all. A command that found no server running then fails beside the
/// reason. It says nothing of a server that is running or that ended
/// cleanly.
// What it says is for the failure's output, which nextest shows.
#[allow(clippy::print_stderr)]
pub(crate) fn say_if_gone(child: &mut Child, stderr: Option<&Receiver<String>>) {
    if !std::thread::panicking() {
        return;
    }
    let Ok(Some(status)) = child.try_wait() else {
        return;
    };
    if status.success() {
        return;
    }
    eprintln!(
        "the test's ritornel-server, pid {}, had ended: {status}",
        child.id()
    );
    if let Some(stderr) = stderr {
        let until = Instant::now() + Duration::from_secs(5);
        while let Ok(line) = stderr.recv_timeout(until.saturating_duration_since(Instant::now())) {
            eprintln!("ritornel-server: {line}");
        }
    }
}
