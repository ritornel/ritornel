//! `ritornelctl`: the command-line client, and the first consumer of the
//! contract. It talks to a running server through `ritornel-client` and
//! never opens a database (docs/internals/server.md, "Decisions").

// Tests may unwrap and index. Everything else is held to the workspace's
// lints, which CI denies.
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

mod artists;
mod backup;
mod cli;
mod export;
mod history;
mod identity;
mod jobs;
mod lastfm;
mod library;
mod link;
mod playback;
mod scan;
mod server;
mod settings;
mod status;

use std::process::ExitCode;

fn main() -> ExitCode {
    let args = std::env::args_os().skip(1);
    let (mut out, mut err) = (std::io::stdout(), std::io::stderr());
    ExitCode::from(cli::run(
        args,
        &|name| std::env::var_os(name),
        &mut out,
        &mut err,
    ))
}
