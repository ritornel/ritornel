//! Linux: a Unix, whose background service is a systemd user unit, run by
//! the user's manager with the session bus and the sound server that MPRIS
//! and ALSA's default device reach, and whose media session is MPRIS on
//! that bus (`linux_mpris.rs`), served from the server's own runtime.

pub(crate) use super::unix::*;

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use super::service::{installed, prepare};
use super::systemd::{UNIT, install, status, uninstall, unit};
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
/// The machine's pretty name if one is set, as `hostnamectl` shows it, or
/// else its host name.
pub(crate) fn computer_name() -> Option<String> {
    let pretty = std::fs::read_to_string("/etc/machine-info")
        .ok()
        .and_then(|info| {
            info.lines().find_map(|line| {
                let value = line.strip_prefix("PRETTY_HOSTNAME=")?;
                Some(value.trim().trim_matches('"').to_owned())
            })
        })
        .filter(|name| !name.is_empty());
    pretty.or_else(|| std::fs::read_to_string("/proc/sys/kernel/hostname").ok())
}

/// Holds an idle inhibitor lock through `systemd-inhibit`, which holds it
/// for as long as the `cat` it runs is reading: until this server closes
/// its end of the pipe, or ends. Screen blanking is not inhibited.
pub(crate) fn stay_awake(why: &str) -> std::io::Result<super::Awake> {
    Command::new("systemd-inhibit")
        .args([
            "--what=idle",
            "--mode=block",
            "--who=Ritornel",
            &format!("--why={why}"),
            "cat",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(super::Awake)
}

pub(crate) fn install_service(data_dir: Option<&Path>, env: Env<'_>) -> Result<String, Error> {
    let units = units_dir(env)?;
    let path = units.join(UNIT);
    // The log directory must be there before systemd opens the log in it.
    let (dir, program) = prepare(data_dir, env)?;
    install(&path, &unit(&program, dir.root()), &systemctl)?;
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
