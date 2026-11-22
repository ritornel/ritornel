//! The `ritornel-server` binary. Everything it does is in the library next to
//! it.
//!
//! `main` is not async, on purpose. The async runtime runs on threads of its
//! own and the main thread only waits: on macOS it blocks in the platform
//! run loop, which the media session needs (docs/internals/server.md,
//! "Shutdown and bounds").

use std::process::ExitCode;

fn main() -> ExitCode {
    let args = std::env::args_os().skip(1);
    let (mut out, mut err) = (std::io::stdout(), std::io::stderr());
    ExitCode::from(ritornel_server::run(
        args,
        &|name| std::env::var_os(name),
        &mut out,
        &mut err,
    ))
}
