//! macOS: a Unix, whose background service is a launchd user agent in the
//! login session, where the media keys work, and whose media session is
//! Now Playing (`mac_now_playing.rs`), which needs the main thread to block
//! in the run loop.

pub(crate) use super::unix::*;

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use super::launchd::{LABEL, bootout, plist, running_pid, served_dir, uninstall};
use super::service::{installed, prepare};
use crate::error::io_error;
use crate::{Env, Error};

#[cfg(not(feature = "media-session"))]
pub(crate) use super::fallback::wait;
#[cfg(feature = "media-session")]
pub(crate) use super::mac_now_playing::{Controls as MediaControls, wait};

/// Now Playing is served from the main thread's run loop. The session's
/// thread is left to end by itself when the server stops, as souvlaki may
/// need that loop, which has stopped by then.
#[cfg(feature = "media-session")]
pub(crate) const HAS_MEDIA_SESSION: bool = true;
#[cfg(feature = "media-session")]
pub(crate) const MEDIA_SESSION_ON_RUNTIME: bool = false;

/// Where the agent's plist is, and the job launchd knows it as.
struct Agent {
    agents: PathBuf,
    path: PathBuf,
    domain: String,
    job: String,
}

impl Agent {
    fn of(env: Env<'_>) -> Result<Self, Error> {
        let agents = env("HOME")
            .filter(|home| !home.is_empty())
            .map(|home| PathBuf::from(home).join("Library/LaunchAgents"))
            .ok_or_else(|| Error::Service("HOME is not set".to_owned()))?;
        let path = agents.join(format!("{LABEL}.plist"));
        let domain = format!("gui/{}", rustix::process::getuid().as_raw());
        let job = format!("{domain}/{LABEL}");
        Ok(Self {
            agents,
            path,
            domain,
            job,
        })
    }
}

/// Registers the agent that serves `data_dir` and starts it, replacing one
/// registered before, and says what was done.
pub(crate) fn install_service(data_dir: Option<&Path>, env: Env<'_>) -> Result<String, Error> {
    let Agent {
        agents,
        path,
        domain,
        job,
    } = Agent::of(env)?;
    // The log directory must be there before launchd opens the log in it.
    let (dir, program) = prepare(data_dir, env)?;
    // One registered before goes first, and its server stops.
    bootout(&job, &launchctl)?;
    std::fs::create_dir_all(&agents).map_err(io_error("create", &agents))?;
    let staged = path.with_extension("plist.new");
    std::fs::write(&staged, plist(&program, dir.root())).map_err(io_error("write", &staged))?;
    std::fs::rename(&staged, &path).map_err(io_error("write", &path))?;
    let booted = launchctl(&["bootstrap".as_ref(), domain.as_ref(), path.as_os_str()])?;
    if !booted.status.success() {
        return Err(Error::Service(format!(
            "launchctl bootstrap failed: {}",
            String::from_utf8_lossy(&booted.stderr).trim()
        )));
    }
    Ok(installed(&program, &dir))
}

/// Stops the agent and removes it, and says what was done.
pub(crate) fn uninstall_service(env: Env<'_>) -> Result<String, Error> {
    let agent = Agent::of(env)?;
    uninstall(&agent.job, &agent.path, &launchctl).map(str::to_owned)
}

/// Says whether the agent is registered and running.
pub(crate) fn service_status(env: Env<'_>) -> Result<String, Error> {
    let Agent { path, job, .. } = Agent::of(env)?;
    let installed = std::fs::read_to_string(&path).ok();
    let printed = launchctl(&["print".as_ref(), job.as_ref()])?;
    let loaded = printed.status.success();
    let pid = loaded
        .then(|| running_pid(&String::from_utf8_lossy(&printed.stdout)))
        .flatten();
    let mut lines = match installed.as_deref() {
        Some(plist) => vec![
            format!("Installed: {}", path.display()),
            format!(
                "Data directory: {}",
                served_dir(plist).unwrap_or_else(|| "unknown".to_owned())
            ),
        ],
        None => vec!["Not installed".to_owned()],
    };
    match (loaded, pid) {
        (true, Some(pid)) => lines.push(format!("Running: pid {pid}")),
        (true, None) => lines.push("Registered, not running".to_owned()),
        (false, _) if installed.is_some() => {
            lines.push("Not registered with launchd".to_owned());
        }
        (false, _) => {}
    }
    Ok(lines.join("\n"))
}

fn launchctl(args: &[&OsStr]) -> Result<Output, Error> {
    Command::new("/bin/launchctl")
        .args(args)
        .output()
        .map_err(|error| Error::Service(format!("cannot run launchctl: {error}")))
}
