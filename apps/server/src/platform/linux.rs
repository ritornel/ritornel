//! Linux: a Unix, whose background service is a systemd user unit, run by
//! the user's manager with the session bus and the sound server that MPRIS
//! and ALSA's default device reach, and whose media session is MPRIS on
//! that bus (`linux_mpris.rs`), served from the server's own runtime.

pub(crate) use super::unix::*;

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use super::service::{installed, prepare};
use super::systemd::{UNIT, must, status, stop, uninstall, unit};
use crate::error::io_error;
use crate::{Env, Error};

pub(crate) use super::fallback::wait;
#[cfg(feature = "media-session")]
pub(crate) use super::linux_mpris::Controls as MediaControls;

/// MPRIS is served from the server's runtime, so it leaves the session bus
/// while the runtime still runs.
#[cfg(feature = "media-session")]
pub(crate) const HAS_MEDIA_SESSION: bool = true;
#[cfg(feature = "media-session")]
pub(crate) const MEDIA_SESSION_ON_RUNTIME: bool = true;

/// Where systemd looks for a user's own units.
fn units_dir(env: Env<'_>) -> Result<PathBuf, Error> {
    let config = env("XDG_CONFIG_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .or_else(|| {
            env("HOME")
                .filter(|home| !home.is_empty())
                .map(|home| PathBuf::from(home).join(".config"))
        })
        .ok_or_else(|| Error::Service("HOME is not set".to_owned()))?;
    Ok(config.join("systemd/user"))
}

/// Writes the unit that serves `data_dir`, enables it and starts it,
/// replacing one registered before, and says what was done.
pub(crate) fn install_service(data_dir: Option<&Path>, env: Env<'_>) -> Result<String, Error> {
    let units = units_dir(env)?;
    let path = units.join(UNIT);
    // The log directory must be there before systemd opens the log in it.
    let (dir, program) = prepare(data_dir, env)?;
    // One registered before goes first, and its server stops.
    stop(&systemctl)?;
    std::fs::create_dir_all(&units).map_err(io_error("create", &units))?;
    let staged = path.with_extension("service.new");
    std::fs::write(&staged, unit(&program, dir.root())).map_err(io_error("write", &staged))?;
    std::fs::rename(&staged, &path).map_err(io_error("write", &path))?;
    must(&systemctl, &["daemon-reload".as_ref()])?;
    must(
        &systemctl,
        &["enable".as_ref(), "--now".as_ref(), UNIT.as_ref()],
    )?;
    Ok(installed(&program, &dir))
}

/// Stops the unit and removes it, and says what was done.
pub(crate) fn uninstall_service(env: Env<'_>) -> Result<String, Error> {
    let path = units_dir(env)?.join(UNIT);
    uninstall(&path, &systemctl).map(str::to_owned)
}

/// Says whether the unit is installed and running.
pub(crate) fn service_status(env: Env<'_>) -> Result<String, Error> {
    let path = units_dir(env)?.join(UNIT);
    let installed = std::fs::read_to_string(&path).ok();
    status(&path, installed.as_deref(), &systemctl)
}

fn systemctl(args: &[&OsStr]) -> Result<Output, Error> {
    Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()
        .map_err(|error| Error::Service(format!("cannot run systemctl: {error}")))
}
