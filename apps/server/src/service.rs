//! `ritornel-server service`: the optional background service. See
//! "Background service" in docs/design/server.md.
//!
//! ```text
//! ritornel-server service install [--data-dir <dir>]
//! ritornel-server service uninstall
//! ritornel-server service status
//! ```
//!
//! On macOS it is a launchd user agent in the login session, where media
//! keys work; a system daemon would have no media session. It runs `serve`
//! on the data directory, starts at login, and starts again if it fails.
//! The desktop app then finds it and starts nothing, and quitting the app
//! leaves it playing.

use std::io::Write;
use std::path::PathBuf;

use clap::{Args, Subcommand};

use crate::{Env, Error};

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
        // Only macOS installs anything.
        #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
        #[arg(long)]
        data_dir: Option<PathBuf>,
    },
    /// Stops the service and removes it.
    Uninstall,
    /// Says whether it is registered and running.
    Status,
}

/// Runs `ritornel-server service`.
pub(crate) fn run(args: &ServiceArgs, env: Env<'_>, out: &mut dyn Write) -> Result<(), Error> {
    platform::run(&args.action, env, out)
}

/// The launchd agent and how it is driven. It is built on macOS, and for
/// the tests everywhere.
#[cfg(any(target_os = "macos", test))]
mod launchd {
    use std::ffi::OsStr;
    use std::io::ErrorKind;
    use std::path::Path;
    use std::process::Output;
    use std::time::{Duration, Instant};

    use crate::Error;
    use crate::error::io_error;

    /// The service's launchd label, and the name of its plist.
    pub(super) const LABEL: &str = "dev.ritornel.server";

    /// How long launchd waits for the server to stop before it kills it,
    /// as the desktop app does.
    const EXIT_TIMEOUT_SECONDS: u32 = 15;

    /// The exit code of `launchctl bootout` for a job that is not there:
    /// ESRCH, "No such process".
    const BOOTOUT_NO_SUCH_JOB: i32 = 3;

    /// The exit code of `launchctl print` for a job that is not there.
    const PRINT_NO_SUCH_JOB: i32 = 113;

    /// Runs `launchctl` with these arguments. A test passes a fake.
    pub(super) type Launchctl<'a> = &'a dyn Fn(&[&OsStr]) -> Result<Output, Error>;

    /// Escapes text for a plist's XML.
    fn xml(text: &str) -> String {
        text.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    }

    /// The launchd agent that runs `program serve --data-dir <data_dir>`.
    ///
    /// It runs at load and at each login, and again after an exit that was
    /// not clean: a crash, or a start refused because another server held
    /// the directory, which launchd retries every ten seconds until it is
    /// free. Its standard output and error, which hold only the ready line
    /// and a failed start's reason, go to `service.log` beside the
    /// server's own log.
    pub(super) fn plist(program: &Path, data_dir: &Path) -> String {
        let program = xml(&program.display().to_string());
        let data = xml(&data_dir.display().to_string());
        let log = xml(&data_dir.join("logs/service.log").display().to_string());
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{LABEL}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{program}</string>
		<string>serve</string>
		<string>--data-dir</string>
		<string>{data}</string>
	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<dict>
		<key>SuccessfulExit</key>
		<false/>
	</dict>
	<key>ProcessType</key>
	<string>Interactive</string>
	<key>ExitTimeOut</key>
	<integer>{EXIT_TIMEOUT_SECONDS}</integer>
	<key>StandardOutPath</key>
	<string>{log}</string>
	<key>StandardErrorPath</key>
	<string>{log}</string>
</dict>
</plist>
"#
        )
    }

    /// The data directory a plist written by [`plist`] serves.
    pub(super) fn served_dir(plist: &str) -> Option<String> {
        let after = plist.split("<string>--data-dir</string>").nth(1)?;
        let value = after.split("<string>").nth(1)?.split("</string>").next()?;
        Some(
            value
                .replace("&lt;", "<")
                .replace("&gt;", ">")
                .replace("&amp;", "&"),
        )
    }

    /// The pid in what `launchctl print` says of a running job.
    pub(super) fn running_pid(printed: &str) -> Option<u32> {
        printed
            .lines()
            .find_map(|line| line.trim().strip_prefix("pid = "))
            .and_then(|pid| pid.trim().parse().ok())
    }

    /// Why launchctl failed, in its own words.
    fn failed(doing: &str, output: &Output) -> Error {
        let said = String::from_utf8_lossy(&output.stderr);
        let said = said.trim();
        Error::Service(match (said.is_empty(), output.status.code()) {
            (false, _) => format!("launchctl {doing} failed: {said}"),
            (true, Some(code)) => format!("launchctl {doing} failed with exit code {code}"),
            (true, None) => format!("launchctl {doing} was killed"),
        })
    }

    /// Removes the job from launchd and waits for its server to stop,
    /// which launchd does not. `Ok(false)` when there was no job; any other
    /// failure is an error, and the job may still be running.
    pub(super) fn bootout(job: &str, launchctl: Launchctl<'_>) -> Result<bool, Error> {
        let booted_out = launchctl(&["bootout".as_ref(), job.as_ref()])?;
        match booted_out.status.code() {
            Some(0) => {}
            Some(BOOTOUT_NO_SUCH_JOB) => return Ok(false),
            _ => return Err(failed("bootout", &booted_out)),
        }
        // launchd kills it after its exit timeout; this waits a little
        // longer.
        let deadline = Instant::now() + Duration::from_secs(u64::from(EXIT_TIMEOUT_SECONDS) + 5);
        loop {
            let printed = launchctl(&["print".as_ref(), job.as_ref()])?;
            match printed.status.code() {
                Some(PRINT_NO_SUCH_JOB) => return Ok(true),
                Some(0) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(100));
                }
                Some(0) => return Err(Error::Service(format!("{job} has not stopped"))),
                _ => return Err(failed("print", &printed)),
            }
        }
    }

    /// Stops the job and removes its plist, and says what was done. The
    /// plist stays if the job could not be removed, so that it can be tried
    /// again.
    pub(super) fn uninstall(
        job: &str,
        plist: &Path,
        launchctl: Launchctl<'_>,
    ) -> Result<&'static str, Error> {
        let booted_out = bootout(job, launchctl)?;
        let removed = match std::fs::remove_file(plist) {
            Ok(()) => true,
            Err(error) if error.kind() == ErrorKind::NotFound => false,
            Err(error) => return Err(io_error("remove", plist)(error)),
        };
        Ok(if booted_out || removed {
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
        fn the_agent_runs_serve_on_the_directory_and_logs_beside_the_server() {
            let written = plist(
                Path::new("/Users/pat/.local/bin/ritornel-server"),
                Path::new("/Users/pat/Music & <Things>/data"),
            );
            assert!(written.contains("<string>dev.ritornel.server</string>"));
            assert!(written.contains(
                "<string>/Users/pat/.local/bin/ritornel-server</string>\n\t\t<string>serve</string>\n\t\t<string>--data-dir</string>\n\t\t<string>/Users/pat/Music &amp; &lt;Things&gt;/data</string>"
            ));
            assert!(written.contains(
                "<string>/Users/pat/Music &amp; &lt;Things&gt;/data/logs/service.log</string>"
            ));
            assert!(!written.contains("--owned"));
            assert_eq!(
                served_dir(&written).as_deref(),
                Some("/Users/pat/Music & <Things>/data")
            );
        }

        #[test]
        fn the_pid_is_read_from_what_launchctl_prints() {
            let printed = "gui/501/dev.ritornel.server = {\n\tactive count = 1\n\tstate = running\n\n\tprogram = /x\n\tpid = 4312\n}\n";
            assert_eq!(running_pid(printed), Some(4312));
            assert_eq!(running_pid("\tstate = not running\n"), None);
        }

        fn output(code: i32, stderr: &str) -> Output {
            Output {
                status: ExitStatus::from_raw(code << 8),
                stdout: Vec::new(),
                stderr: stderr.as_bytes().to_vec(),
            }
        }

        /// A fake launchctl that answers each call with the next of
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

        struct Plist(std::path::PathBuf);

        impl Plist {
            fn new(name: &str) -> Self {
                let path = std::env::temp_dir().join(format!(
                    "ritornel-service-{}-{name}.plist",
                    std::process::id()
                ));
                std::fs::write(&path, "<plist/>").unwrap();
                Self(path)
            }
        }

        impl Drop for Plist {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }

        const JOB: &str = "gui/501/dev.ritornel.server";

        #[test]
        fn a_running_service_is_stopped_waited_for_and_its_plist_removed() {
            let plist = Plist::new("running");
            let fake = Fake::new(vec![
                ("bootout", output(0, "")),
                ("print", output(0, "state = running")),
                ("print", output(PRINT_NO_SUCH_JOB, "Could not find service")),
            ]);
            let said = uninstall(JOB, &plist.0, &|args| fake.run(args)).unwrap();
            assert_eq!(said, "Removed the background service");
            assert!(!plist.0.exists());
            assert_eq!(*fake.calls.borrow(), ["bootout", "print", "print"]);
        }

        #[test]
        fn a_job_that_is_not_there_is_not_an_error_and_a_left_plist_is_removed() {
            let plist = Plist::new("absent");
            let absent = || output(BOOTOUT_NO_SUCH_JOB, "Boot-out failed: 3: No such process");
            let fake = Fake::new(vec![("bootout", absent()), ("bootout", absent())]);
            let said = uninstall(JOB, &plist.0, &|args| fake.run(args)).unwrap();
            assert_eq!(said, "Removed the background service");
            assert!(!plist.0.exists());
            let said = uninstall(JOB, &plist.0, &|args| fake.run(args)).unwrap();
            assert_eq!(said, "The background service is not installed");
        }

        #[test]
        fn a_bootout_that_fails_otherwise_is_reported_and_keeps_the_plist() {
            let plist = Plist::new("failed");
            let fake = Fake::new(vec![(
                "bootout",
                output(5, "Boot-out failed: 5: Input/output error"),
            )]);
            let failed = uninstall(JOB, &plist.0, &|args| fake.run(args)).unwrap_err();
            assert_eq!(
                failed.to_string(),
                "the background service: launchctl bootout failed: Boot-out failed: 5: \
                 Input/output error"
            );
            assert!(plist.0.exists());

            // A print that fails for another reason is no proof it stopped.
            let fake = Fake::new(vec![("bootout", output(0, "")), ("print", output(1, ""))]);
            let failed = uninstall(JOB, &plist.0, &|args| fake.run(args)).unwrap_err();
            assert_eq!(
                failed.to_string(),
                "the background service: launchctl print failed with exit code 1"
            );
            assert!(plist.0.exists());
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::ffi::OsStr;
    use std::io::Write;
    use std::path::PathBuf;
    use std::process::{Command, Output};

    use super::Action;
    use super::launchd::{LABEL, bootout, plist, running_pid, served_dir, uninstall};
    use crate::data_dir::{self, DataDir};
    use crate::error::io_error;
    use crate::{Env, Error};

    pub(super) fn run(action: &Action, env: Env<'_>, out: &mut dyn Write) -> Result<(), Error> {
        let agents = env("HOME")
            .filter(|home| !home.is_empty())
            .map(|home| PathBuf::from(home).join("Library/LaunchAgents"))
            .ok_or_else(|| Error::Service("HOME is not set".to_owned()))?;
        let path = agents.join(format!("{LABEL}.plist"));
        let domain = format!("gui/{}", rustix::process::getuid().as_raw());
        let job = format!("{domain}/{LABEL}");
        match action {
            Action::Install { data_dir } => {
                let root = data_dir::resolve(data_dir.as_deref(), env)?;
                // The log directory must be there before launchd opens the
                // log in it.
                let dir = DataDir::prepare(root)?;
                let program = std::env::current_exe()
                    .and_then(std::fs::canonicalize)
                    .map_err(|error| {
                        Error::Service(format!("cannot find this program: {error}"))
                    })?;
                // One registered before goes first, and its server stops.
                bootout(&job, &launchctl)?;
                std::fs::create_dir_all(&agents).map_err(io_error("create", &agents))?;
                let staged = path.with_extension("plist.new");
                std::fs::write(&staged, plist(&program, dir.root()))
                    .map_err(io_error("write", &staged))?;
                std::fs::rename(&staged, &path).map_err(io_error("write", &path))?;
                let booted = launchctl(&["bootstrap".as_ref(), domain.as_ref(), path.as_os_str()])?;
                if !booted.status.success() {
                    return Err(Error::Service(format!(
                        "launchctl bootstrap failed: {}",
                        String::from_utf8_lossy(&booted.stderr).trim()
                    )));
                }
                writeln!(
                    out,
                    "Installed the background service: {} serve --data-dir {}\n\
                     It runs now and at each login, and the desktop app uses it. If another \
                     server is running on that directory, the service starts once it stops.\n\
                     Its log is in {}",
                    program.display(),
                    dir.root().display(),
                    dir.logs().display()
                )
                .map_err(Error::Output)
            }
            Action::Uninstall => {
                let said = uninstall(&job, &path, &launchctl)?;
                writeln!(out, "{said}").map_err(Error::Output)
            }
            Action::Status => {
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
                writeln!(out, "{}", lines.join("\n")).map_err(Error::Output)
            }
        }
    }

    fn launchctl(args: &[&OsStr]) -> Result<Output, Error> {
        Command::new("/bin/launchctl")
            .args(args)
            .output()
            .map_err(|error| Error::Service(format!("cannot run launchctl: {error}")))
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use std::io::Write;

    use super::Action;
    use crate::{Env, Error};

    pub(super) fn run(_: &Action, _: Env<'_>, _: &mut dyn Write) -> Result<(), Error> {
        Err(Error::Service(
            "it is built for macOS only; elsewhere run `ritornel-server serve` under the \
             system's service manager"
                .to_owned(),
        ))
    }
}
