//! The systemd user unit and how it is driven. It is built on Linux, and
//! for the tests on every Unix: they make an exit status from a raw wait
//! status.

use std::ffi::OsStr;
use std::io::ErrorKind;
use std::path::Path;
use std::process::Output;

use crate::Error;
use crate::error::io_error;

/// The unit's name, and the name of its file.
pub(crate) const UNIT: &str = "dev.ritornel.server.service";

/// How long systemd waits for the server to stop before it kills it,
/// as the desktop app does.
const STOP_TIMEOUT_SECONDS: u32 = 15;

/// The exit code of `systemctl stop` for a unit that is not loaded.
const NOT_LOADED: i32 = 5;

/// Runs `systemctl --user` with these arguments. A test passes a fake.
pub(crate) type Systemctl<'a> = &'a dyn Fn(&[&OsStr]) -> Result<Output, Error>;

/// Escapes text for a unit file, where `%` starts a specifier.
fn specifiers(text: &str) -> String {
    text.replace('%', "%%")
}

/// The program of an `ExecStart=` line, quoted: a path may hold
/// spaces, quotes and `%`. systemd takes the program before it expands
/// variables, so a `$` in it stays as it is.
fn exec_program(text: &str) -> String {
    let escaped = specifiers(text).replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

/// An argument of an `ExecStart=` line, quoted as the program is, and
/// with `$`, which would start a variable, doubled.
fn exec_argument(text: &str) -> String {
    exec_program(text).replace('$', "$$")
}

/// The user unit that runs `program serve --data-dir <data_dir>`.
///
/// It starts at login (`default.target`), and again ten seconds after
/// an exit that was not clean: a crash, or a start refused because
/// another server held the directory, until it is free. Its standard
/// output and error, which hold only the ready line and a failed
/// start's reason, go to `service.log` beside the server's own log.
pub(crate) fn unit(program: &Path, data_dir: &Path) -> String {
    let program = exec_program(&program.display().to_string());
    let data = exec_argument(&data_dir.display().to_string());
    let log = specifiers(&data_dir.join("logs/service.log").display().to_string());
    format!(
        "[Unit]
Description=Ritornel server

[Service]
Type=exec
ExecStart={program} serve --data-dir {data}
Restart=on-failure
RestartSec=10
TimeoutStopSec={STOP_TIMEOUT_SECONDS}
StandardOutput=append:{log}
StandardError=append:{log}

[Install]
WantedBy=default.target
"
    )
}

/// The data directory a unit written by [`unit()`] serves.
pub(crate) fn served_dir(unit: &str) -> Option<String> {
    let exec = unit
        .lines()
        .find_map(|line| line.strip_prefix("ExecStart="))?;
    let after = exec.split_once(" --data-dir ")?.1;
    let mut quoted = after.strip_prefix('"')?.chars();
    let mut value = String::new();
    loop {
        match quoted.next()? {
            '"' => break,
            '\\' => value.push(quoted.next()?),
            c => value.push(c),
        }
    }
    Some(value.replace("$$", "$").replace("%%", "%"))
}

/// What `systemctl show` says of the unit.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Shown {
    /// Whether systemd has read the unit file.
    pub(crate) loaded: bool,
    /// `active`, `inactive`, `failed`, `activating` and so on.
    pub(crate) active: String,
    /// The server's pid, while it runs.
    pub(crate) pid: Option<u32>,
}

/// Reads what `systemctl show --property=LoadState,ActiveState,MainPID`
/// prints: `None` if it says no load state, which it says of every
/// unit, one it has no file for included.
pub(crate) fn shown(printed: &str) -> Option<Shown> {
    let value = |key: &str| {
        printed
            .lines()
            .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
    };
    Some(Shown {
        loaded: value("LoadState").filter(|state| !state.is_empty())? == "loaded",
        active: value("ActiveState").unwrap_or("").to_owned(),
        pid: value("MainPID")
            .and_then(|pid| pid.parse().ok())
            .filter(|pid| *pid != 0),
    })
}

/// Why systemctl failed, in its own words.
pub(crate) fn failed(doing: &str, output: &Output) -> Error {
    let said = String::from_utf8_lossy(&output.stderr);
    let said = said.trim();
    Error::Service(match (said.is_empty(), output.status.code()) {
        (false, _) => format!("systemctl {doing} failed: {said}"),
        (true, Some(code)) => format!("systemctl {doing} failed with exit code {code}"),
        (true, None) => format!("systemctl {doing} was killed"),
    })
}

/// Runs systemctl and fails unless it succeeded.
pub(crate) fn must(systemctl: Systemctl<'_>, args: &[&OsStr]) -> Result<(), Error> {
    let output = systemctl(args)?;
    if output.status.success() {
        return Ok(());
    }
    let doing = args
        .first()
        .map_or_else(String::new, |verb| verb.to_string_lossy().into_owned());
    Err(failed(&doing, &output))
}

/// What `service status` says of the unit, whose file is at `path`
/// and holds `installed` if it is there. A systemctl that cannot say
/// (no user manager, say) is an error, not a unit that is not there.
pub(crate) fn status(
    path: &Path,
    installed: Option<&str>,
    systemctl: Systemctl<'_>,
) -> Result<String, Error> {
    let printed = systemctl(&[
        "show".as_ref(),
        "--property=LoadState,ActiveState,MainPID".as_ref(),
        UNIT.as_ref(),
    ])?;
    if !printed.status.success() {
        return Err(failed("show", &printed));
    }
    let state = shown(&String::from_utf8_lossy(&printed.stdout))
        .ok_or_else(|| Error::Service("systemctl show said nothing of the unit".to_owned()))?;
    let mut lines = match installed {
        Some(unit) => vec![
            format!("Installed: {}", path.display()),
            format!(
                "Data directory: {}",
                served_dir(unit).unwrap_or_else(|| "unknown".to_owned())
            ),
        ],
        None => vec!["Not installed".to_owned()],
    };
    match (state.loaded, state.pid) {
        (true, Some(pid)) => lines.push(format!("Running: pid {pid}")),
        (true, None) => lines.push(format!("Registered, not running ({})", state.active)),
        (false, _) if installed.is_some() => {
            lines.push("Not registered with systemd".to_owned());
        }
        (false, _) => {}
    }
    Ok(lines.join("\n"))
}

/// Stops the unit, waiting for its server to stop as `systemctl stop`
/// does. `Ok(false)` when systemd has no such unit.
pub(crate) fn stop(systemctl: Systemctl<'_>) -> Result<bool, Error> {
    let stopped = systemctl(&["stop".as_ref(), UNIT.as_ref()])?;
    match stopped.status.code() {
        Some(0) => Ok(true),
        Some(NOT_LOADED) => Ok(false),
        _ => Err(failed("stop", &stopped)),
    }
}

pub(crate) fn install(path: &Path, contents: &str, systemctl: Systemctl<'_>) -> Result<(), Error> {
    let replacement = super::service::Replacement::install(path, contents)?;
    let query = systemctl(&[
        "show".as_ref(),
        "--property=ActiveState,UnitFileState".as_ref(),
        UNIT.as_ref(),
    ]);
    let (active, enabled) = match query {
        Ok(out) if out.status.success() => {
            let state = String::from_utf8_lossy(&out.stdout);
            let Some(active) = state
                .lines()
                .find_map(|line| line.strip_prefix("ActiveState="))
                .filter(|value| !value.is_empty())
            else {
                let error = Error::Service("systemd did not report the service state. Check systemctl --user status before installing.".into());
                return Err(replacement.rollback(error, || Ok(())));
            };
            (
                matches!(active, "active" | "activating" | "reloading"),
                state.lines().any(|line| line == "UnitFileState=enabled"),
            )
        }
        Ok(out) if out.status.code() == Some(4) => (false, false),
        Ok(out) => return Err(replacement.rollback(failed("show", &out), || Ok(()))),
        Err(err) => return Err(replacement.rollback(err, || Ok(()))),
    };
    let replacement = replacement.protect_running(active)?;
    let reload = || must(systemctl, &["daemon-reload".as_ref()]);
    let run = |verb: &str| must(systemctl, &[verb.as_ref(), UNIT.as_ref()]);
    if let Err(error) = reload() {
        return Err(replacement.rollback(error, || Ok(())));
    }
    let start = run("restart").and_then(|()| {
        if enabled {
            return Ok(());
        }
        run("enable").map_err(|error| match run("disable") {
            Ok(()) => error,
            Err(cleanup) => Error::Service(format!(
                "{error}. Removing the new login setting failed ({cleanup}). \
                 Check it with systemctl --user is-enabled {UNIT}."
            )),
        })
    });
    if let Err(error) = start {
        return Err(replacement.rollback(error, || {
            if !active {
                stop(systemctl)?;
            }
            reload()?;
            if active {
                run("restart")?;
            }
            Ok(())
        }));
    }
    Ok(())
}

/// Stops the unit, disables it and removes its file, and says what was
/// done. The file stays if the unit could not be stopped, so that it
/// can be tried again.
pub(crate) fn uninstall(unit: &Path, systemctl: Systemctl<'_>) -> Result<&'static str, Error> {
    let stopped = stop(systemctl)?;
    let removed = if unit.exists() {
        must(systemctl, &["disable".as_ref(), UNIT.as_ref()])?;
        match std::fs::remove_file(unit) {
            Ok(()) => true,
            Err(error) if error.kind() == ErrorKind::NotFound => false,
            Err(error) => return Err(io_error("remove", unit)(error)),
        }
    } else {
        false
    };
    if removed {
        must(systemctl, &["daemon-reload".as_ref()])?;
    }
    Ok(if stopped || removed {
        "Removed the background service"
    } else {
        "The background service is not installed"
    })
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::os::unix::process::ExitStatusExt;
    use std::process::ExitStatus;

    use super::*;

    #[test]
    fn the_unit_runs_serve_on_the_directory_and_logs_beside_the_server() {
        let written = unit(
            Path::new("/home/pat/$tools/100%/ritornel-server"),
            Path::new("/home/pat/Music \"100%\" $HOME\\data"),
        );
        assert_eq!(
            written,
            "[Unit]
Description=Ritornel server

[Service]
Type=exec
ExecStart=\"/home/pat/$tools/100%%/ritornel-server\" serve --data-dir \"/home/pat/Music \\\"100%%\\\" $$HOME\\\\data\"
Restart=on-failure
RestartSec=10
TimeoutStopSec=15
StandardOutput=append:/home/pat/Music \"100%%\" $HOME\\data/logs/service.log
StandardError=append:/home/pat/Music \"100%%\" $HOME\\data/logs/service.log

[Install]
WantedBy=default.target
"
        );
        assert!(!written.contains("--owned"));
        assert_eq!(
            served_dir(&written).as_deref(),
            Some("/home/pat/Music \"100%\" $HOME\\data")
        );
    }

    #[test]
    fn the_state_is_read_from_what_systemctl_shows() {
        let running = "LoadState=loaded\nActiveState=active\nMainPID=4312\n";
        assert_eq!(
            shown(running),
            Some(Shown {
                loaded: true,
                active: "active".to_owned(),
                pid: Some(4312),
            })
        );
        let absent = "MainPID=0\nLoadState=not-found\nActiveState=inactive\n";
        assert_eq!(
            shown(absent),
            Some(Shown {
                loaded: false,
                active: "inactive".to_owned(),
                pid: None,
            })
        );
        // Nothing is not a unit that is not there.
        assert_eq!(shown(""), None);
    }

    fn printed(stdout: &str) -> Output {
        Output {
            stdout: stdout.as_bytes().to_vec(),
            ..output(0, "")
        }
    }

    #[test]
    fn a_unit_systemd_has_no_file_for_is_not_registered() {
        let file = UnitFile::new("status-absent");
        let fake = Fake::new(vec![(
            "show",
            printed("LoadState=not-found\nActiveState=inactive\nMainPID=0\n"),
        )]);
        let said = status(&file.path, Some("[Unit]\n"), &|args| fake.run(args)).unwrap();
        assert!(said.ends_with("Not registered with systemd"), "{said}");
        let fake = Fake::new(vec![(
            "show",
            printed("LoadState=loaded\nActiveState=active\nMainPID=4312\n"),
        )]);
        let said = status(&file.path, Some("[Unit]\n"), &|args| fake.run(args)).unwrap();
        assert!(said.ends_with("Running: pid 4312"), "{said}");
    }

    #[test]
    fn a_status_systemctl_cannot_give_is_an_error() {
        let file = UnitFile::new("status-failed");
        let fake = Fake::new(vec![(
            "show",
            output(1, "Failed to connect to user scope bus via local transport"),
        )]);
        let failed = status(&file.path, Some("[Unit]\n"), &|args| fake.run(args)).unwrap_err();
        assert_eq!(
            failed.to_string(),
            "the background service: systemctl show failed: Failed to connect to user scope \
             bus via local transport"
        );
        // Nor is a show that says nothing of the unit.
        let fake = Fake::new(vec![("show", printed(""))]);
        assert!(status(&file.path, None, &|args| fake.run(args)).is_err());
    }

    fn output(code: i32, stderr: &str) -> Output {
        Output {
            status: ExitStatus::from_raw(code << 8),
            stdout: Vec::new(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }

    /// A fake systemctl that answers each call with the next of
    /// `answers`, by its first argument, and records the calls.
    struct Fake {
        answers: RefCell<Vec<(&'static str, Output)>>,
        calls: RefCell<Vec<String>>,
    }

    impl Fake {
        fn new(answers: Vec<(&'static str, Output)>) -> Self {
            Self {
                answers: RefCell::new(answers),
                calls: RefCell::new(Vec::new()),
            }
        }

        fn run(&self, args: &[&OsStr]) -> Result<Output, Error> {
            let verb = args[0].to_string_lossy().into_owned();
            self.calls.borrow_mut().push(verb.clone());
            let (expected, answer) = self.answers.borrow_mut().remove(0);
            assert_eq!(verb, expected);
            Ok(answer)
        }
    }

    struct UnitFile {
        path: std::path::PathBuf,
        _dir: tempfile::TempDir,
    }

    impl UnitFile {
        fn new(name: &str) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(format!("{name}.service"));
            std::fs::write(&path, "[Unit]\n").unwrap();
            Self { path, _dir: dir }
        }
    }

    #[test]
    fn failed_registration_puts_the_running_definition_back() {
        let file = UnitFile::new("rollback");
        let fake = Fake::new(vec![
            ("show", printed("ActiveState=active")),
            ("daemon-reload", output(0, "")),
            ("restart", output(1, "bad unit")),
            ("daemon-reload", output(0, "")),
            ("restart", output(0, "")),
        ]);
        assert!(install(&file.path, "new unit", &|args| fake.run(args)).is_err());
        assert_eq!(std::fs::read_to_string(&file.path).unwrap(), "[Unit]\n");
        assert!(fake.answers.borrow().is_empty());
    }

    #[test]
    fn an_install_requires_the_current_service_state() {
        let file = UnitFile::new("unknown-state");
        let fake = Fake::new(vec![("show", printed(""))]);
        assert!(install(&file.path, "new unit", &|args| fake.run(args)).is_err());
        assert_eq!(std::fs::read_to_string(&file.path).unwrap(), "[Unit]\n");
        assert!(fake.answers.borrow().is_empty());
    }

    #[test]
    fn a_failed_initial_reload_keeps_the_original_without_restarting_it() {
        let file = UnitFile::new("reload-error");
        let fake = Fake::new(vec![
            ("show", printed("ActiveState=active")),
            ("daemon-reload", output(1, "manager unavailable")),
        ]);
        let error = install(&file.path, "new unit", &|args| fake.run(args)).unwrap_err();
        assert!(error.to_string().contains("manager unavailable"));
        assert_eq!(std::fs::read_to_string(&file.path).unwrap(), "[Unit]\n");
        assert!(fake.answers.borrow().is_empty());
    }

    #[test]
    fn failed_enable_restores_an_inactive_disabled_unit() {
        let file = UnitFile::new("enable-error");
        let fake = Fake::new(vec![
            (
                "show",
                printed("ActiveState=inactive\nUnitFileState=disabled"),
            ),
            ("daemon-reload", output(0, "")),
            ("restart", output(0, "")),
            ("enable", output(1, "cannot enable")),
            ("disable", output(0, "")),
            ("stop", output(0, "")),
            ("daemon-reload", output(0, "")),
        ]);
        assert!(install(&file.path, "new unit", &|args| fake.run(args)).is_err());
        assert_eq!(std::fs::read_to_string(&file.path).unwrap(), "[Unit]\n");
        assert!(fake.answers.borrow().is_empty());
    }

    #[test]
    fn a_running_service_with_no_saved_definition_is_left_running() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing");
        let fake = Fake::new(vec![("show", printed("ActiveState=active"))]);
        let error = install(&path, "new unit", &|args| fake.run(args)).unwrap_err();
        assert!(error.to_string().contains("no saved definition"));
        assert!(!path.exists());
        assert!(fake.answers.borrow().is_empty());
    }

    #[test]
    fn a_failed_stage_leaves_the_manager_alone() {
        let file = UnitFile::new("stage-error");
        let path = file.path.join("not-a-directory");
        let fake = Fake::new(vec![]);
        assert!(install(&path, "new unit", &|args| fake.run(args)).is_err());
    }

    #[test]
    fn a_running_service_is_stopped_disabled_and_its_unit_removed() {
        let file = UnitFile::new("running");
        let fake = Fake::new(vec![
            ("stop", output(0, "")),
            ("disable", output(0, "")),
            ("daemon-reload", output(0, "")),
        ]);
        let said = uninstall(&file.path, &|args| fake.run(args)).unwrap();
        assert_eq!(said, "Removed the background service");
        assert!(!file.path.exists());
        assert_eq!(*fake.calls.borrow(), ["stop", "disable", "daemon-reload"]);
    }

    #[test]
    fn a_unit_that_is_not_there_is_not_an_error() {
        let file = UnitFile::new("absent");
        std::fs::remove_file(&file.path).unwrap();
        let not_loaded = || output(NOT_LOADED, "Unit dev.ritornel.server.service not loaded.");
        let fake = Fake::new(vec![("stop", not_loaded())]);
        let said = uninstall(&file.path, &|args| fake.run(args)).unwrap();
        assert_eq!(said, "The background service is not installed");
    }

    #[test]
    fn a_stop_that_fails_otherwise_is_reported_and_keeps_the_unit() {
        let file = UnitFile::new("failed");
        let fake = Fake::new(vec![(
            "stop",
            output(1, "Failed to connect to bus: No medium found"),
        )]);
        let failed = uninstall(&file.path, &|args| fake.run(args)).unwrap_err();
        assert_eq!(
            failed.to_string(),
            "the background service: systemctl stop failed: Failed to connect to bus: No \
             medium found"
        );
        assert!(file.path.exists());
    }
}
