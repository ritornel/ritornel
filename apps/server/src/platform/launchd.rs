//! The launchd agent and how it is driven. It is built on macOS, and for
//! the tests on every Unix: they make an exit status from a raw wait status.

use std::ffi::OsStr;
use std::io::ErrorKind;
use std::path::Path;
use std::process::Output;
use std::time::{Duration, Instant};

use crate::Error;
use crate::error::io_error;

/// The service's launchd label, and the name of its plist.
pub(crate) const LABEL: &str = "dev.ritornel.server";

/// How long launchd waits for the server to stop before it kills it,
/// as the desktop app does.
const EXIT_TIMEOUT_SECONDS: u32 = 15;

/// The exit code of `launchctl bootout` for a job that is not there:
/// ESRCH, "No such process".
const BOOTOUT_NO_SUCH_JOB: i32 = 3;

/// The exit code of `launchctl print` for a job that is not there.
const PRINT_NO_SUCH_JOB: i32 = 113;

/// Runs `launchctl` with these arguments. A test passes a fake.
pub(crate) type Launchctl<'a> = &'a dyn Fn(&[&OsStr]) -> Result<Output, Error>;

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
pub(crate) fn plist(program: &Path, data_dir: &Path) -> String {
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
pub(crate) fn served_dir(plist: &str) -> Option<String> {
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
pub(crate) fn running_pid(printed: &str) -> Option<u32> {
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
pub(crate) fn bootout(job: &str, launchctl: Launchctl<'_>) -> Result<bool, Error> {
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
pub(crate) fn uninstall(
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
