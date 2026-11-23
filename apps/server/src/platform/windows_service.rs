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
//! The supervisor has a hidden window, which `service uninstall` and a
//! reinstall ask to close: ending the task in Task Scheduler ends only the
//! console.
//!
//! Each user has a task of their own, named after them, as each has a
//! launchd agent or a systemd unit of their own.

use std::ffi::OsStr;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use ritornel_windows::window::{self, Window};

use super::service::{installed, prepare};
use super::{close_to_others, create_owner_only};
use crate::data_dir::{self, DataDir};
use crate::error::io_error;
use crate::{Env, Error};

/// The start of each task's name: the user's name follows.
const TASK_PREFIX: &str = "dev.ritornel.server";

/// The title of the supervisor's hidden window, by which it is found in the
/// session.
const SUPERVISOR: &str = "Ritornel service";

/// How long after an exit that was not clean the server is started again.
const RESTART_AFTER: Duration = Duration::from_secs(10);

/// How long the supervisor waits for the server to stop once its input is
/// closed, as the desktop app does, before it ends it.
const STOP_WAIT: Duration = Duration::from_secs(15);

/// How long `install` and `uninstall` wait for a supervisor to stop: its
/// own wait, the end it may then put to the server, and a little more.
const SUPERVISOR_WAIT: Duration = Duration::from_secs(25);

/// How long `install` waits for the supervisor it started to open its
/// window, which it does once the data directory and its log are ready.
const START_WAIT: Duration = Duration::from_secs(15);

/// How long `install` waits for the task to be no longer running after its
/// supervisor stopped: the console it ran in closes a moment later.
const ENDED_WAIT: Duration = Duration::from_secs(5);

/// The task's name for `user`: Task Scheduler's names are the machine's,
/// and a name holds no `\ / : * ? " < > |`.
fn task_name(user: &str) -> String {
    let user: String = user
        .chars()
        .map(|c| match c {
            c if c.is_alphanumeric() || "-_.".contains(c) => c,
            _ => '_',
        })
        .collect();
    format!("{TASK_PREFIX}.{user}")
}

/// The user the service is for: `DOMAIN\name`, and their task's name.
fn user(env: Env<'_>) -> Result<(String, String), Error> {
    let set = |name| env(name).filter(|value| !value.is_empty());
    match (set("USERDOMAIN"), set("USERNAME")) {
        (Some(domain), Some(name)) => {
            let name = name.to_string_lossy();
            Ok((
                format!("{}\\{name}", domain.to_string_lossy()),
                task_name(&name),
            ))
        }
        _ => Err(Error::Service(
            "USERDOMAIN or USERNAME is not set".to_owned(),
        )),
    }
}

/// The task that runs `program service run --data-dir <data_dir>` when
/// `user` signs in, in their session, at normal priority (Task Scheduler's
/// default is below it, which a player cannot have), with no time limit and
/// on battery as on mains. A start asked for while it runs is ignored:
/// `install` starts it only once the supervisor before has gone.
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
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
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

/// A path without the `\\?\` that `canonicalize` puts before it, where the
/// path means the same without it: within `MAX_PATH`, and with no name
/// ending in a dot or a space, which Windows would otherwise drop.
fn plain(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    let (plain, rest) = if let Some(share) = text.strip_prefix(r"\\?\UNC\") {
        (format!(r"\\{share}"), share)
    } else if let Some(local) = text
        .strip_prefix(r"\\?\")
        .filter(|local| local.as_bytes().get(1) == Some(&b':'))
    {
        (local.to_owned(), local)
    } else {
        return path.to_owned();
    };
    let same = plain.len() < 260
        && rest
            .split('\\')
            .all(|name| !name.ends_with('.') && !name.ends_with(' '));
    if same {
        PathBuf::from(plain)
    } else {
        path.to_owned()
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

/// A program from the system's own folder.
fn system_program(name: &str, env: Env<'_>) -> PathBuf {
    let root = env("SystemRoot")
        .filter(|root| !root.is_empty())
        .unwrap_or_else(|| r"C:\Windows".into());
    Path::new(&root).join("System32").join(name)
}

/// Runs `schtasks.exe` with these arguments, and fails unless it
/// succeeded, in its own words.
fn schtasks(args: &[&OsStr], env: Env<'_>) -> Result<(), Error> {
    let output = Command::new(system_program("schtasks.exe", env))
        .args(args)
        .output()
        .map_err(|error| Error::Service(format!("cannot run schtasks: {error}")))?;
    if output.status.success() {
        return Ok(());
    }
    let verb = args.first().map(|verb| verb.to_string_lossy());
    let verb = verb.unwrap_or_default();
    let said = String::from_utf8_lossy(&output.stderr);
    Err(Error::Service(match (said.trim(), output.status.code()) {
        ("", Some(code)) => format!("schtasks {verb} failed with exit code {code}"),
        ("", None) => format!("schtasks {verb} failed"),
        (said, _) => format!("schtasks {verb} failed: {said}"),
    }))
}

/// What Task Scheduler says of a task.
#[derive(Debug, PartialEq, Eq)]
enum Task {
    /// There is no task of that name.
    Absent,
    /// The task, its state (`Ready`, `Running`, `Disabled` and so on), and
    /// its definition.
    Present { state: String, definition: String },
}

/// Asks Task Scheduler for the task `name`, through PowerShell, which tells
/// a task that is not there from a question that failed and names the state
/// in English whatever the system's language; schtasks does neither.
fn task(name: &str, env: Env<'_>) -> Result<Task, Error> {
    let name = name.replace('\'', "''");
    let script = format!(
        "[Console]::OutputEncoding = [Text.Encoding]::UTF8
try {{ $task = Get-ScheduledTask -TaskPath '\\' -TaskName '{name}' -ErrorAction Stop }}
catch {{
  if ($_.CategoryInfo.Category -eq 'ObjectNotFound') {{ 'Absent'; exit 0 }}
  [Console]::Error.WriteLine($_.Exception.Message); exit 1
}}
$task.State.ToString()
Export-ScheduledTask -TaskPath '\\' -TaskName '{name}'"
    );
    let output = Command::new(system_program(
        r"WindowsPowerShell\v1.0\powershell.exe",
        env,
    ))
    .args(["-NoProfile", "-NonInteractive", "-Command", &script])
    .stdin(Stdio::null())
    .output()
    .map_err(|error| Error::Service(format!("cannot run PowerShell: {error}")))?;
    if !output.status.success() {
        return Err(Error::Service(format!(
            "Task Scheduler could not be asked for the task: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(read_task(&String::from_utf8_lossy(&output.stdout)))
}

/// What [`task`]'s script printed: `Absent`, or the state and then the
/// definition.
fn read_task(printed: &str) -> Task {
    let printed = printed.trim_start_matches('\u{feff}');
    let (state, definition) = printed.split_once('\n').unwrap_or((printed, ""));
    match state.trim() {
        "Absent" => Task::Absent,
        state => Task::Present {
            state: state.to_owned(),
            definition: definition.to_owned(),
        },
    }
}

/// Asks the supervisor running in this session to stop, and waits for it
/// to. Whether one was running.
fn stop_supervisor() -> Result<bool, Error> {
    let asked = window::ask_to_close(SUPERVISOR).map_err(|error| {
        Error::Service(format!("the service could not be asked to stop: {error}"))
    })?;
    if !asked {
        return Ok(false);
    }
    let since = Instant::now();
    while window::owner(SUPERVISOR).is_some() {
        if since.elapsed() >= SUPERVISOR_WAIT {
            return Err(Error::Service(format!(
                "the service did not stop within {} seconds. Run service status, and try again",
                SUPERVISOR_WAIT.as_secs()
            )));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(true)
}

/// Waits up to `longest` for `done`, looking every tenth of a second.
fn wait_for(
    longest: Duration,
    mut done: impl FnMut() -> Result<bool, Error>,
) -> Result<bool, Error> {
    let since = Instant::now();
    loop {
        if done()? {
            return Ok(true);
        }
        if since.elapsed() >= longest {
            return Ok(false);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// What to do about a task still running once no supervisor answers in
/// this session: it runs in another, where this one cannot reach it.
fn elsewhere(name: &str) -> Error {
    Error::Service(format!(
        "the service is running in another session, where it cannot be asked to stop from here. \
         Run this there, or sign that session out. The task is {name} in Task Scheduler"
    ))
}

/// Registers the task that serves `data_dir` and starts it, once the
/// supervisor of the one it replaces has stopped, and returns once the new
/// supervisor is running.
pub(crate) fn install_service(data_dir: Option<&Path>, env: Env<'_>) -> Result<String, Error> {
    let (user, name) = user(env)?;
    let (dir, program) = prepare(data_dir, env)?;
    let definition = definition(&user, &program, dir.root())?;
    stop_supervisor()?;
    let running =
        |env| Ok(matches!(task(&name, env)?, Task::Present { state, .. } if state == "Running"));
    if !wait_for(ENDED_WAIT, || running(env).map(|running| !running))? {
        return Err(elsewhere(&name));
    }
    // schtasks reads a definition from a file, in UTF-16 as it declares.
    let mut file = tempfile::NamedTempFile::new().map_err(io_error("stage", Path::new(&name)))?;
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
    let name_arg: &OsStr = name.as_ref();
    schtasks(
        &[
            "/Create".as_ref(),
            "/TN".as_ref(),
            name_arg,
            "/XML".as_ref(),
            file.as_os_str(),
            "/F".as_ref(),
        ],
        env,
    )?;
    schtasks(&["/Run".as_ref(), "/TN".as_ref(), name_arg], env)?;
    let started = wait_for(START_WAIT, || Ok(window::owner(SUPERVISOR).is_some()))?;
    let mut said = installed(&plain(&program), &dir);
    if !started {
        if !running(env)? {
            return Err(Error::Service(format!(
                "the task {name} was registered and started, but the service did not start. \
                 Run `ritornel-server service run --data-dir \"{}\"` in a terminal to see why",
                dir.root().display()
            )));
        }
        said.push_str("\nIt started in another session, where this one cannot see it.");
    }
    Ok(said)
}

/// Removes the task, so that nothing starts it again, and stops its
/// supervisor, and says what was done. A task that runs in another session
/// is left, with its supervisor, to be removed from there.
pub(crate) fn uninstall_service(env: Env<'_>) -> Result<String, Error> {
    let (_, name) = user(env)?;
    let present = match task(&name, env)? {
        Task::Absent => None,
        Task::Present { state, .. } => Some(state),
    };
    if present.as_deref() == Some("Running") && window::owner(SUPERVISOR).is_none() {
        return Err(elsewhere(&name));
    }
    if present.is_some() {
        let name: &OsStr = name.as_ref();
        schtasks(
            &["/Delete".as_ref(), "/TN".as_ref(), name, "/F".as_ref()],
            env,
        )?;
    }
    let stopped = stop_supervisor()?;
    Ok(if stopped || present.is_some() {
        "Removed the background service"
    } else {
        "The background service is not installed"
    }
    .to_owned())
}

/// Says whether the task is registered, and whether its supervisor runs.
pub(crate) fn service_status(env: Env<'_>) -> Result<String, Error> {
    let (_, name) = user(env)?;
    let task = task(&name, env)?;
    let mut lines = match &task {
        Task::Present { definition, .. } => vec![
            format!("Installed: the task {name} in Task Scheduler"),
            format!(
                "Data directory: {}",
                served_dir(definition).unwrap_or_else(|| "unknown".to_owned())
            ),
        ],
        Task::Absent => vec!["Not installed".to_owned()],
    };
    match (window::owner(SUPERVISOR), &task) {
        (Some(pid), _) => lines.push(format!("Running: pid {pid}")),
        (None, Task::Present { state, .. }) if state == "Running" => {
            lines.push("Running in another session".to_owned());
        }
        (None, Task::Present { state, .. }) => {
            lines.push(format!("Registered, not running ({state})"));
        }
        (None, Task::Absent) => {}
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
/// output and error, go to `service.log` beside the server's own log, which
/// only the OS user may read. Its window opens once all that is ready, so
/// that `install` knows it started.
pub(crate) fn run_service(data_dir: &Path, env: Env<'_>) -> Result<(), Error> {
    let dir = DataDir::prepare(data_dir::resolve(Some(data_dir), env)?)?;
    let program = std::env::current_exe()
        .map_err(|error| Error::Service(format!("cannot find this program: {error}")))?;
    let log_path = dir.logs().join("service.log");
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    create_owner_only(&mut options);
    let log = options
        .open(&log_path)
        .map_err(io_error("open", &log_path))?;
    close_to_others(&log_path).map_err(io_error("close", &log_path))?;
    let mut notes = log.try_clone().map_err(io_error("open", &log_path))?;
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
    let taskkill = system_program("taskkill.exe", env);
    let end = |pid: u32| {
        let _ = Command::new(&taskkill)
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .output();
    };
    let supervised = supervise(
        start,
        end,
        (&heard, &hearing),
        (RESTART_AFTER, STOP_WAIT),
        &mut notes,
    );
    window.stopped();
    supervised
}

/// Starts the server with `start`, and starts it again `restart_after` each
/// time it ends uncleanly, until it ends cleanly or [`Heard::Stop`] comes.
/// A stop closes the server's input and waits `stop_wait` for it to end;
/// one still running then is ended with `end`, given its pid.
fn supervise(
    mut start: impl FnMut() -> std::io::Result<Child>,
    end: impl Fn(u32),
    (heard, hearing): (&Sender<Heard>, &Receiver<Heard>),
    (restart_after, stop_wait): (Duration, Duration),
    log: &mut dyn Write,
) -> Result<(), Error> {
    loop {
        let mut child = start().map_err(|error| {
            // Under a console no one sees, the log is the only place to say so.
            let _ = writeln!(
                log,
                "ritornel-server service: cannot start the server: {error}"
            );
            Error::Service(format!("cannot start the server: {error}"))
        })?;
        let pid = child.id();
        let input = child.stdin.take();
        let ended = heard.clone();
        std::thread::spawn(move || {
            let _ = ended.send(Heard::Ended(child.wait()));
        });
        let status = match hearing.recv() {
            Ok(Heard::Ended(status)) => status,
            Ok(Heard::Stop) | Err(_) => {
                drop(input);
                if !ends_within(hearing, stop_wait) {
                    let _ = writeln!(
                        log,
                        "ritornel-server service: the server did not stop within {} seconds of \
                         being asked, and was ended",
                        stop_wait.as_secs()
                    );
                    end(pid);
                    ends_within(hearing, stop_wait);
                }
                return Ok(());
            }
        };
        let why = match status {
            Ok(status) if status.success() => return Ok(()),
            Ok(status) => format!("the server ended ({status})"),
            Err(error) => format!("lost the server ({error})"),
        };
        let _ = writeln!(
            log,
            "ritornel-server service: {why}; starting it again in {} seconds",
            restart_after.as_secs()
        );
        match hearing.recv_timeout(restart_after) {
            Err(RecvTimeoutError::Timeout) | Ok(Heard::Ended(_)) => {}
            Ok(Heard::Stop) | Err(RecvTimeoutError::Disconnected) => return Ok(()),
        }
    }
}

/// Whether the server ends within `longest`. A stop heard meanwhile changes
/// nothing.
fn ends_within(hearing: &Receiver<Heard>, longest: Duration) -> bool {
    let until = Instant::now() + longest;
    while let Some(left) = until.checked_duration_since(Instant::now()) {
        match hearing.recv_timeout(left) {
            Ok(Heard::Ended(_)) | Err(RecvTimeoutError::Disconnected) => return true,
            Ok(Heard::Stop) => {}
            Err(RecvTimeoutError::Timeout) => return false,
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

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
    fn the_directory_is_read_back_from_the_definition_task_scheduler_exports() {
        // As `Export-ScheduledTask` prints it: its own declaration and
        // layout, and quotes left bare inside an element.
        let exported = "<?xml version=\"1.0\" encoding=\"UTF-16\"?>\r\n\
            <Task version=\"1.2\" xmlns=\"http://schemas.microsoft.com/windows/2004/02/mit/task\">\r\n\
            \x20 <Actions Context=\"Author\">\r\n\
            \x20   <Exec>\r\n\
            \x20     <Command>%SystemRoot%\\System32\\conhost.exe</Command>\r\n\
            \x20     <Arguments>--headless \"C:\\Tools\\ritornel-server.exe\" service run --data-dir \"D:\\Música &amp; Bücher\"</Arguments>\r\n\
            \x20   </Exec>\r\n\
            \x20 </Actions>\r\n\
            </Task>";
        assert_eq!(
            read_task(&format!("\u{feff}Running\r\n{exported}")),
            Task::Present {
                state: "Running\r".trim().to_owned(),
                definition: exported.to_owned(),
            }
        );
        assert_eq!(served_dir(exported).as_deref(), Some("D:\\Música & Bücher"));
        assert_eq!(read_task("Absent\r\n"), Task::Absent);
    }

    #[test]
    fn each_user_has_a_task_of_their_own_with_a_name_task_scheduler_takes() {
        assert_eq!(task_name("pat"), "dev.ritornel.server.pat");
        assert_eq!(
            task_name("Pat Q: \"two\""),
            "dev.ritornel.server.Pat_Q___two_"
        );
    }

    #[test]
    fn a_path_keeps_its_verbatim_form_where_it_would_mean_another_without_it() {
        assert_eq!(
            plain(Path::new(r"\\?\C:\Tools\ritornel-server.exe")),
            Path::new(r"C:\Tools\ritornel-server.exe")
        );
        for kept in [r"\\?\C:\Tools.\ritornel-server.exe", r"\\?\C:\Tools \x.exe"] {
            assert_eq!(plain(Path::new(kept)), Path::new(kept));
        }
        let long = format!(r"\\?\C:\{}\ritornel-server.exe", "a".repeat(260));
        assert_eq!(plain(Path::new(&long)), Path::new(&long));
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

    /// A program that reads its input until it closes, as `serve --owned`
    /// does, and then ends cleanly.
    fn reads_until_closed() -> std::io::Result<Child> {
        Command::new("findstr")
            .arg("^")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
    }

    /// A program that pays its input no heed and runs for a minute.
    fn hangs() -> std::io::Result<Child> {
        Command::new("ping")
            .args(["-n", "60", "127.0.0.1"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
    }

    fn never_ended(pid: u32) {
        panic!("{pid} was ended");
    }

    /// Asks the supervisor to stop after `after`.
    fn stop_after(heard: &Sender<Heard>, after: Duration) {
        let heard = heard.clone();
        std::thread::spawn(move || {
            std::thread::sleep(after);
            let _ = heard.send(Heard::Stop);
        });
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
            never_ended,
            (&heard, &hearing),
            (Duration::from_millis(10), STOP_WAIT),
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
        stop_after(&heard, Duration::from_millis(500));
        let mut starts = 0;
        let begun = Instant::now();
        supervise(
            || {
                starts += 1;
                reads_until_closed()
            },
            never_ended,
            (&heard, &hearing),
            (Duration::from_millis(10), STOP_WAIT),
            &mut Vec::new(),
        )
        .unwrap();
        assert_eq!(starts, 1);
        // It ran until asked, and stopped once its input closed.
        assert!(begun.elapsed() >= Duration::from_millis(500));
        assert!(begun.elapsed() < STOP_WAIT);
    }

    #[test]
    fn a_server_that_does_not_stop_when_asked_is_ended() {
        let (heard, hearing) = mpsc::channel();
        stop_after(&heard, Duration::from_millis(200));
        let ended = Mutex::new(None);
        let mut log = Vec::new();
        let begun = Instant::now();
        supervise(
            hangs,
            |pid| {
                *ended.lock().unwrap() = Some(pid);
                let _ = Command::new("taskkill")
                    .args(["/F", "/PID", &pid.to_string()])
                    .output();
            },
            (&heard, &hearing),
            (Duration::from_millis(10), Duration::from_millis(500)),
            &mut log,
        )
        .unwrap();
        assert!(ended.lock().unwrap().is_some());
        // Ended, not waited on for ever: a minute is far below a hang.
        assert!(begun.elapsed() < Duration::from_secs(60));
        assert!(String::from_utf8(log).unwrap().contains("was ended"));
    }

    #[test]
    fn a_stop_while_waiting_to_start_again_ends_the_supervisor() {
        let (heard, hearing) = mpsc::channel();
        stop_after(&heard, Duration::from_millis(500));
        let mut starts = 0;
        let begun = Instant::now();
        supervise(
            || {
                starts += 1;
                ends(1)
            },
            never_ended,
            (&heard, &hearing),
            (Duration::from_secs(60), STOP_WAIT),
            &mut Vec::new(),
        )
        .unwrap();
        assert_eq!(starts, 1);
        // Ended, not waited on for ever: a minute is far below a hang.
        assert!(begun.elapsed() < Duration::from_secs(60));
    }
}
