//! `ritornel-server service`: the optional background service. See
//! "Background service" in docs/internals/server.md.
//!
//! ```text
//! ritornel-server service install [--data-dir <dir>]
//! ritornel-server service uninstall
//! ritornel-server service status
//! ```
//!
//! On macOS it is a launchd user agent in the login session, where media
//! keys work; a system daemon would have no media session. On Linux it is
//! a systemd user unit, for the same reason: the user's manager runs it with
//! the session bus and the sound server that MPRIS and ALSA's default
//! device reach. Either way it runs `serve` on the data directory, starts
//! at login, and starts again if it fails. The desktop app then finds it
//! and starts nothing, and quitting the app leaves it playing.

use std::io::Write;
use std::path::PathBuf;

use clap::{Args, Subcommand};

use crate::{Env, Error, platform};

/// The arguments of `ritornel-server service`.
#[derive(Debug, PartialEq, Eq, Args)]
pub(crate) struct ServiceArgs {
    #[command(subcommand)]
    action: Action,
}

#[derive(Debug, PartialEq, Eq, Subcommand)]
enum Action {
    /// Registers the service and starts it, replacing one registered
    /// before.
    Install {
        #[arg(long)]
        data_dir: Option<PathBuf>,
    },
    /// Stops the service and removes it.
    Uninstall,
    /// Says whether it is registered and running.
    Status,
}

/// Runs `ritornel-server service`. What registers the service is the
/// system's (`platform::install_service`).
pub(crate) fn run(args: &ServiceArgs, env: Env<'_>, out: &mut dyn Write) -> Result<(), Error> {
    let said = match &args.action {
        Action::Install { data_dir } => platform::install_service(data_dir.as_deref(), env)?,
        Action::Uninstall => platform::uninstall_service(env)?,
        Action::Status => platform::service_status(env)?,
    };
    writeln!(out, "{said}").map_err(Error::Output)
}
