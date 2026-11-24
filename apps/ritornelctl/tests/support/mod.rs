//! What the tests that start a `ritornel-server` share.

use std::process::Child;
use std::sync::mpsc::Receiver;

/// Says, while a test is failing, why its server is gone if it is: how it
/// ended, which names the signal when one ended it, and what it wrote to
/// stderr that the test kept, from `stderr` when the test reads it. A
/// command that found no server running then fails beside the reason.
/// It says nothing of a server that is running or that ended cleanly.
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
    for line in stderr.into_iter().flat_map(Receiver::try_iter) {
        eprintln!("ritornel-server: {line}");
    }
}
