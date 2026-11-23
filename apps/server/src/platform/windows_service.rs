//! The background service on Windows: a task in Task Scheduler that runs at
//! sign-in, in the user's own session, where the default audio device and
//! the system's media controls are, and the supervisor it runs.
//!
//! Task Scheduler would open a console window for the server, so the task
//! runs it under `conhost.exe --headless`, which gives it a console no one
//! sees. That console does not pass on its program's exit code, so Task
//! Scheduler cannot tell a crash from a clean stop. Starting the server
//! again is the supervisor's job instead (`ritornel-server service run`):
//! it runs `serve --owned`, and starts it again ten seconds after an exit
//! that was not clean, as the systemd unit does. The server stops cleanly
//! when the supervisor ends, however it ends, since that closes its input.
//! The supervisor has a hidden window, which `service uninstall` asks to
//! close.

use std::ffi::OsStr;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use ritornel_windows::window::{self, Window};

use super::service::{installed, prepare};
use crate::data_dir::{self, DataDir};
use crate::error::io_error;
use crate::{Env, Error};

/// The task's name in Task Scheduler.
pub(crate) const TASK: &str = "dev.ritornel.server";

/// The title of the supervisor's hidden window, by which it is found.
const SUPERVISOR: &str = "Ritornel service";

/// How long after an exit that was not clean the server is started again.
const RESTART_AFTER: Duration = Duration::from_secs(10);

/// How long the supervisor waits for the server to stop once its input is
/// closed, as the desktop app does.
const STOP_WAIT: Duration = Duration::from_secs(15);

/// How long `install` and `uninstall` wait for a supervisor to stop: its
/// own wait, and a little more.
const SUPERVISOR_WAIT: Duration = Duration::from_secs(20);

/// The task that runs `program service run --data-dir <data_dir>` when
/// `user` signs in, in their session, at normal priority (Task Scheduler's
/// default is below it, which a player cannot have), with no time limit and
/// on battery as on mains. A start asked for while it runs waits for it to
/// end, so a reinstall does not race the supervisor it replaces.
///
/// Task Scheduler expands `%NAME%` in what a task runs, with no way to
/// escape it, so a path holding `%` is refused.
pub(crate) fn definition(user: &str, program: &Path, data_dir: &Path) -> Result<String, Error> {
    let [program, data_dir] = [program, data_dir].map(|path| plain(path).display().to_string());
    if program.contains('%') || data_dir.contains('%') {
        return Err(Error::Service(
            "Task Scheduler would read the % in the program's or the data directory's path as a \
             variable. Choose a path without %."
                .to_owned(),
        ));
    }
    let arguments = xml(&format!(
        "--headless {} service run --data-dir {}",
        quote(&program),
        quote(&data_dir)
    ));
    let user = xml(user);
    let about = xml(&format!("Ritornel server on {data_dir}"));
    Ok(format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>{about}</Description>
  </RegistrationInfo>
  <Triggers>
    <LogonTrigger>
      <UserId>{user}</UserId>
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>{user}</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>Queue</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>5</Priority>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>%SystemRoot%\System32\conhost.exe</Command>
      <Arguments>{arguments}</Arguments>
    </Exec>
  </Actions>
</Task>
"#
    ))
}

/// The data directory a task written by [`definition`] serves, from its
/// definition as Task Scheduler gives it back.
pub(crate) fn served_dir(definition: &str) -> Option<String> {
    let arguments = definition.split_once("<Arguments>")?.1;
    let arguments = unxml(arguments.split_once("</Arguments>")?.0);
    let quoted = arguments
        .split_once(" --data-dir \"")?
        .1
        .strip_suffix('"')?;
    // Only backslashes before the closing quote were doubled.
    let kept = quoted.trim_end_matches('\\');
    let doubled = quoted.len() - kept.len();
    Some(format!("{kept}{}", "\\".repeat(doubled / 2)))
}

/// A path as `CommandLineToArgvW` reads it back: in quotes, with the
/// backslashes before the closing one doubled. A Windows path cannot hold
/// a quote.
fn quote(path: &str) -> String {
    let kept = path.trim_end_matches('\\');
    let trailing = path.len() - kept.len();
    format!("\"{kept}{}\"", "\\".repeat(trailing * 2))
}

/// A path without the `\\?\` that `canonicalize` puts before it, which a
/// person reading the task need not see.
fn plain(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(share) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{share}"));
    }
    match text.strip_prefix(r"\\?\") {
        Some(local) if local.as_bytes().get(1) == Some(&b':') => PathBuf::from(local),
        _ => path.to_owned(),
    }
}

/// Text as XML holds it.
fn xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Text as XML held it.
fn unxml(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// Runs `schtasks.exe` from the system's own folder with these arguments.
fn schtasks(args: &[&OsStr], env: Env<'_>) -> Result<Output, Error> {
    let root = env("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
    Command::new(Path::new(&root).join(r"System32\schtasks.exe"))
        .args(args)
        .output()
        .map_err(|error| Error::Service(format!("cannot run schtasks: {error}")))
}

/// Runs schtasks and fails unless it succeeded, in its own words.
fn must(args: &[&OsStr], env: Env<'_>) -> Result<(), Error> {
    let output = schtasks(args, env)?;
    if output.status.success() {
        return Ok(());
    }
    let said = String::from_utf8_lossy(&output.stderr);
    let verb = args.first().map(|verb| verb.to_string_lossy());
    Err(Error::Service(format!(
        "schtasks {} failed: {}",
        verb.unwrap_or_default(),
        said.trim()
    )))
}

/// The task's definition, if it is registered.
fn registered(env: Env<'_>) -> Result<Option<String>, Error> {
    let output = schtasks(
        &[
            "/Query".as_ref(),
            "/TN".as_ref(),
            TASK.as_ref(),
            "/XML".as_ref(),
            "ONE".as_ref(),
        ],
        env,
    )?;
    // schtasks says no more than exit code 1 for a task that is not there,
    // and that for little else.
    match output.status.code() {
        Some(0) => Ok(Some(String::from_utf8_lossy(&output.stdout).into_owned())),
        Some(1) => Ok(None),
        _ => Err(Error::Service(format!(
            "schtasks /Query failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))),
    }
}

/// Asks a running supervisor to stop, and waits for it to. Whether one was
/// running.
fn stop_supervisor() -> Result<bool, Error> {
    if !window::ask_to_close(SUPERVISOR) {
        return Ok(false);
    }
    let asked = Instant::now();
    while window::owner(SUPERVISOR).is_some() {
        if asked.elapsed() >= SUPERVISOR_WAIT {
            return Err(Error::Service(format!(
                "the service did not stop within {} seconds. Run service status, and try again",
                SUPERVISOR_WAIT.as_secs()
            )));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(true)
}

/// Registers the task that serves `data_dir`, replacing one registered
/// before, and starts it, stopping the supervisor it replaces.
pub(crate) fn install_service(data_dir: Option<&Path>, env: Env<'_>) -> Result<String, Error> {
    let user = match (env("USERDOMAIN"), env("USERNAME")) {
        (Some(domain), Some(name)) => format!("{}\\{}", domain.display(), name.display()),
        _ => {
            return Err(Error::Service(
                "USERDOMAIN or USERNAME is not set".to_owned(),
            ));
        }
    };
    let (dir, program) = prepare(data_dir, env)?;
    let definition = definition(&user, &program, dir.root())?;
    // schtasks reads a definition from a file, in UTF-16 as it declares.
    let mut file = tempfile::NamedTempFile::new().map_err(io_error("stage", Path::new(TASK)))?;
    let bytes: Vec<u8> = [0xFEFF]
        .into_iter()
        .chain(definition.encode_utf16())
        .flat_map(u16::to_le_bytes)
        .collect();
    file.write_all(&bytes)
        .and_then(|()| file.flush())
        .map_err(io_error("write", file.path()))?;
    // Closed, so that schtasks may open it; it is removed when dropped.
    let file = file.into_temp_path();
    must(
        &[
            "/Create".as_ref(),
            "/TN".as_ref(),
            TASK.as_ref(),
            "/XML".as_ref(),
            file.as_os_str(),
            "/F".as_ref(),
        ],
        env,
    )?;
    stop_supervisor()?;
    must(&["/Run".as_ref(), "/TN".as_ref(), TASK.as_ref()], env)?;
    Ok(installed(&plain(&program), &dir))
}

/// Stops the supervisor and removes the task, and says what was done. The
/// task stays if the supervisor could not be stopped, so that it can be
/// tried again.
pub(crate) fn uninstall_service(env: Env<'_>) -> Result<String, Error> {
    let stopped = stop_supervisor()?;
    let removed = registered(env)?.is_some();
    if removed {
        must(
            &[
                "/Delete".as_ref(),
                "/TN".as_ref(),
                TASK.as_ref(),
                "/F".as_ref(),
            ],
            env,
        )?;
    }
    Ok(if stopped || removed {
        "Removed the background service"
    } else {
        "The background service is not installed"
    }
    .to_owned())
}

/// Says whether the task is registered and its supervisor running.
pub(crate) fn service_status(env: Env<'_>) -> Result<String, Error> {
    let definition = registered(env)?;
    let mut lines = match &definition {
        Some(definition) => vec![
            format!("Installed: the task {TASK} in Task Scheduler"),
            format!(
                "Data directory: {}",
                served_dir(definition).unwrap_or_else(|| "unknown".to_owned())
            ),
        ],
        None => vec!["Not installed".to_owned()],
    };
    match window::owner(SUPERVISOR) {
        Some(pid) => lines.push(format!("Running: pid {pid}")),
        None if definition.is_some() => lines.push("Registered, not running".to_owned()),
        None => {}
    }
    Ok(lines.join("\n"))
}

/// What the supervisor hears.
#[derive(Debug)]
enum Heard {
    /// It was asked to stop: by `service uninstall`, a reinstall,
    /// `taskkill /PID`, or the session ending.
    Stop,
    /// The server ended.
    Ended(std::io::Result<ExitStatus>),
}

/// Runs `serve --owned` on `data_dir` until it stops cleanly or this is
/// asked to stop, starting it again ten seconds after each exit that was
/// not clean. What it says, and what the server writes to its standard
/// output and error, go to `service.log` beside the server's own log.
pub(crate) fn run_service(data_dir: &Path, env: Env<'_>) -> Result<(), Error> {
    let dir = DataDir::prepare(data_dir::resolve(Some(data_dir), env)?)?;
    let program = std::env::current_exe()
        .map_err(|error| Error::Service(format!("cannot find this program: {error}")))?;
    let log_path = dir.logs().join("service.log");
    let log = File::options()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(io_error("open", &log_path))?;
    let (heard, hearing) = mpsc::channel();
    let window = Window::open(SUPERVISOR, {
        let heard = heard.clone();
        move |_| {
            let _ = heard.send(Heard::Stop);
        }
    })
    .map_err(|error| Error::Service(format!("cannot open the service's window: {error}")))?;
    let start = || {
        let output = log.try_clone()?;
        Command::new(&program)
            .arg("serve")
            .arg("--owned")
            .arg("--data-dir")
            .arg(dir.root())
            .stdin(Stdio::piped())
            .stderr(output.try_clone()?)
            .stdout(output)
            .spawn()
    };
    let mut notes = log.try_clone().map_err(io_error("open", &log_path))?;
    let supervised = supervise(start, &heard, &hearing, RESTART_AFTER, &mut notes);
    window.stopped();
    supervised
}

/// Starts the server with `start` and starts it again `restart_after` each
/// time it ends uncleanly, until it ends cleanly or [`Heard::Stop`] comes.
/// A stop closes the server's input and waits for it to end, for up to
/// [`STOP_WAIT`].
fn supervise(
    mut start: impl FnMut() -> std::io::Result<Child>,
    heard: &Sender<Heard>,
    hearing: &Receiver<Heard>,
    restart_after: Duration,
    log: &mut dyn Write,
) -> Result<(), Error> {
    loop {
        let mut child =
            start().map_err(|error| Error::Service(format!("cannot start the server: {error}")))?;
        let input = child.stdin.take();
        let ended = heard.clone();
        std::thread::spawn(move || {
            let _ = ended.send(Heard::Ended(child.wait()));
        });
        let status = match hearing.recv() {
            Ok(Heard::Ended(status)) => status,
            Ok(Heard::Stop) | Err(_) => {
                drop(input);
                let until = Instant::now() + STOP_WAIT;
                while let Some(left) = until.checked_duration_since(Instant::now()) {
                    if let Ok(Heard::Ended(_)) | Err(_) = hearing.recv_timeout(left) {
                        break;
                    }
                }
                return Ok(());
            }
        };
        match status {
            Ok(status) if status.success() => return Ok(()),
            Ok(status) => {
                let _ = writeln!(
                    log,
                    "ritornel-server service: the server ended ({status}); starting it again in {} seconds",
                    restart_after.as_secs()
                );
            }
            Err(error) => {
                let _ = writeln!(
                    log,
                    "ritornel-server service: lost the server ({error}); starting it again in {} seconds",
                    restart_after.as_secs()
                );
            }
        }
        match hearing.recv_timeout(restart_after) {
            Err(RecvTimeoutError::Timeout) | Ok(Heard::Ended(_)) => {}
            Ok(Heard::Stop) | Err(RecvTimeoutError::Disconnected) => return Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_task_runs_the_supervisor_hidden_at_sign_in_and_gives_its_directory_back() {
        let written = definition(
            r"TALLY\pat",
            Path::new(r"\\?\C:\Program Files\Ritornel & Co\ritornel-server.exe"),
            Path::new(r"D:\"),
        )
        .unwrap();
        assert!(written.contains("<UserId>TALLY\\pat</UserId>\n    </LogonTrigger>"));
        assert!(written.contains("<LogonType>InteractiveToken</LogonType>"));
        assert!(written.contains("<Priority>5</Priority>"));
        assert!(written.contains("<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>"));
        assert!(written.contains(
            "<Arguments>--headless &quot;C:\\Program Files\\Ritornel &amp; Co\\ritornel-server.exe&quot; \
             service run --data-dir &quot;D:\\\\&quot;</Arguments>"
        ));
        assert_eq!(served_dir(&written).as_deref(), Some(r"D:\"));

        let share = definition(
            "TALLY\\pat",
            Path::new(r"\\?\UNC\nas\tools\ritornel-server.exe"),
            Path::new(r"E:\Music <one>"),
        )
        .unwrap();
        assert!(share.contains(r"&quot;\\nas\tools\ritornel-server.exe&quot;"));
        assert_eq!(served_dir(&share).as_deref(), Some("E:\\Music <one>"));
    }

    #[test]
    fn a_path_task_scheduler_would_expand_is_refused() {
        assert!(
            definition(
                "TALLY\\pat",
                Path::new(r"C:\Tools\ritornel-server.exe"),
                Path::new(r"D:\100%\data")
            )
            .is_err()
        );
    }

    /// A program that ends as `code` says.
    fn ends(code: u8) -> std::io::Result<Child> {
        Command::new("cmd")
            .args(["/d", "/c", &format!("exit {code}")])
            .stdin(Stdio::piped())
            .spawn()
    }

    #[test]
    fn an_unclean_exit_is_started_again_and_a_clean_one_ends_the_supervisor() {
        let (heard, hearing) = mpsc::channel();
        let mut codes = [1, 3, 0].into_iter();
        let mut starts = 0;
        let mut log = Vec::new();
        supervise(
            || {
                starts += 1;
                ends(codes.next().unwrap())
            },
            &heard,
            &hearing,
            Duration::from_millis(10),
            &mut log,
        )
        .unwrap();
        assert_eq!(starts, 3);
        let log = String::from_utf8(log).unwrap();
        assert_eq!(log.matches("starting it again").count(), 2, "{log}");
    }

    #[test]
    fn a_stop_closes_the_servers_input_and_is_not_followed_by_a_start() {
        let (heard, hearing) = mpsc::channel();
        heard.send(Heard::Stop).unwrap();
        let mut starts = 0;
        let begun = Instant::now();
        supervise(
            || {
                starts += 1;
                // Reads its input until it is closed, as `serve --owned` does.
                Command::new("cmd")
                    .args(["/d", "/c", "more"])
                    .stdin(Stdio::piped())
                    .stdout(Stdio::null())
                    .spawn()
            },
            &heard,
            &hearing,
            Duration::from_millis(10),
            &mut Vec::new(),
        )
        .unwrap();
        assert_eq!(starts, 1);
        assert!(begun.elapsed() < STOP_WAIT);
    }
}
