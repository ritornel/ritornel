//! macOS: a Unix, whose background service is a launchd user agent in the
//! login session, where the media keys work, and whose media session is
//! Now Playing (`mac_now_playing.rs`), which needs the main thread to block
//! in the run loop.

pub(crate) use super::unix::*;

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use super::launchd::{LABEL, install, plist, running_pid, served_dir, uninstall};
use super::service::{installed, prepare};
use crate::{Env, Error};

pub(crate) use super::fallback::run_service;
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
        Ok(Self { path, domain, job })
    }
}

/// The host name macOS answers to by Bonjour, which it always does: the
/// `studio-mac` of `studio-mac.local`.
pub(crate) fn mdns_host() -> Option<String> {
    let found = Command::new("/usr/sbin/scutil")
        .args(["--get", "LocalHostName"])
        .output()
        .ok()?;
    found
        .status
        .success()
        .then(|| String::from_utf8_lossy(&found.stdout).into_owned())
}

/// The name an older build took from this computer, if it differs from
/// how the computer's name is read now: on this system, none.
pub(crate) fn legacy_computer_name() -> Option<String> {
    None
}

/// The name in System Settings › General › Sharing, as the person wrote
/// it: "Studio Mac", not the network's `studio-mac.local`.
pub(crate) fn computer_name() -> Option<String> {
    let found = Command::new("/usr/sbin/scutil")
        .args(["--get", "ComputerName"])
        .output()
        .ok()?;
    found
        .status
        .success()
        .then(|| String::from_utf8_lossy(&found.stdout).into_owned())
}

/// Holds the system's assertion against idle sleep, which lets the display
/// sleep, through `caffeinate`, which holds it for as long as this server's
/// process lives and no longer.
pub(crate) fn stay_awake(_why: &str) -> std::io::Result<super::Awake> {
    Command::new("/usr/bin/caffeinate")
        .args(["-i", "-w", &std::process::id().to_string()])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(super::Awake)
}

/// Registers the agent that serves `data_dir` and starts it, replacing one
/// registered before, and says what was done.
pub(crate) fn install_service(data_dir: Option<&Path>, env: Env<'_>) -> Result<String, Error> {
    let Agent { path, domain, job } = Agent::of(env)?;
    // The log directory must be there before launchd opens the log in it.
    let (dir, program) = prepare(data_dir, env)?;
    install(
        &job,
        &domain,
        &path,
        &plist(&program, dir.root()),
        &launchctl,
    )?;
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
