//! The command line.
//!
//! ```text
//! ritornelctl status [--data-dir <dir>] [--json]
//! ```

use std::ffi::OsString;
use std::fmt;
use std::io::Write;
use std::path::PathBuf;

use ritornel_client::contract::local::Env;

use crate::status;

const USAGE: &str = "\
The command-line client of a Ritornel server. It talks to the server that is
running on a data directory, and never starts one.

Usage:
  ritornelctl status [--data-dir <dir>] [--json]

Commands:
  status   Show the running server's ID, name and role, the versions it is
           built from, the free disk space and where its logs are

Options:
  --data-dir <dir>      The data directory of the server to talk to
  --json                Print the status as JSON, in the shape the server
                        sends it in
  -h, --help            Show this help
  -V, --version         Show the version

Environment:
  RITORNEL_DATA_DIR     The data directory, when --data-dir is not given.
                        With neither it is
                        ~/Library/Application Support/Ritornel on macOS,
                        and $XDG_DATA_HOME/ritornel elsewhere, which is
                        ~/.local/share/ritornel unless you moved it

Exit codes:
  0    Done
  1    Failed
  2    The command line was wrong
  4    No server is running on the data directory
";

/// The exit code of a command that failed for a reason with no code of its
/// own.
pub(crate) const EXIT_FAILED: u8 = 1;

/// The exit code when the command line was wrong.
pub(crate) const EXIT_USAGE: u8 = 2;

/// The exit code reserved for one case: no server is running on the data
/// directory. It is not 3, which `ritornel-server` uses for the opposite
/// case, a data directory that a server has.
pub(crate) const EXIT_NO_SERVER: u8 = 4;

/// Why a command did not do what it was asked.
///
/// `Display` is one line that is safe to print.
#[derive(Debug)]
pub(crate) enum Error {
    /// The command line was wrong. The text is fixed, so it repeats nothing
    /// it was given.
    Usage(&'static str),
    /// The server could not be found or reached, or it answered with an
    /// error.
    Client(ritornel_client::Error),
    /// The server took too long to answer.
    TimedOut,
    /// The async runtime could not be started.
    Runtime(std::io::Error),
    /// The answer could not be written to standard output.
    Output(std::io::Error),
}

impl Error {
    /// The exit code a process ends with after this error.
    pub(crate) fn exit_code(&self) -> u8 {
        match self {
            Self::Usage(_) => EXIT_USAGE,
            Self::Client(ritornel_client::Error::NotRunning { .. }) => EXIT_NO_SERVER,
            _ => EXIT_FAILED,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage(message) => f.write_str(message),
            Self::Client(error) => write!(f, "{error}"),
            Self::TimedOut => f.write_str("the server did not answer in time"),
            Self::Runtime(error) => write!(f, "cannot start: {error}"),
            Self::Output(error) => write!(f, "cannot write to standard output: {error}"),
        }
    }
}

impl From<ritornel_client::Error> for Error {
    fn from(error: ritornel_client::Error) -> Self {
        Self::Client(error)
    }
}

/// The arguments of `ritornelctl status`.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct StatusArgs {
    pub(crate) data_dir: Option<PathBuf>,
    pub(crate) json: bool,
}

/// A parsed command line.
#[derive(Debug, PartialEq, Eq)]
enum Command {
    Help,
    Version,
    Status(StatusArgs),
}

/// Parses the arguments after the program name. The messages are fixed and
/// repeat no argument.
fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Command, Error> {
    let mut args = args.into_iter();
    let (mut status, mut parsed) = (false, StatusArgs::default());
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("-h" | "--help") => return Ok(Command::Help),
            Some("-V" | "--version") => return Ok(Command::Version),
            Some("status") if !status => status = true,
            // A token that looks like an option is not a value: `--data-dir
            // --json` is a missing directory, not a directory named
            // `--json`. One whose name does begin with a hyphen is written
            // `./-name`.
            Some("--data-dir") if status => match args
                .next()
                .filter(|dir| !dir.is_empty() && !dir.as_encoded_bytes().starts_with(b"-"))
            {
                Some(dir) => parsed.data_dir = Some(PathBuf::from(dir)),
                None => return Err(Error::Usage("--data-dir needs a directory")),
            },
            Some("--json") if status => parsed.json = true,
            _ => {
                return Err(Error::Usage("unknown argument; try `ritornelctl --help`"));
            }
        }
    }
    if status {
        Ok(Command::Status(parsed))
    } else {
        Err(Error::Usage(
            "expected `ritornelctl status`; try `ritornelctl --help`",
        ))
    }
}

/// Runs the program and returns its exit code. What was asked for goes to
/// `out`, and an error to `err`.
pub(crate) fn run(
    args: impl IntoIterator<Item = OsString>,
    env: Env<'_>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> u8 {
    match try_run(args, env, out) {
        Ok(()) => 0,
        Err(error) => {
            // Nothing useful can be done if the terminal is gone.
            let _ = writeln!(err, "ritornelctl: {error}");
            error.exit_code()
        }
    }
}

fn try_run(
    args: impl IntoIterator<Item = OsString>,
    env: Env<'_>,
    out: &mut dyn Write,
) -> Result<(), Error> {
    match parse(args)? {
        Command::Help => out.write_all(USAGE.as_bytes()).map_err(Error::Output),
        Command::Version => writeln!(
            out,
            "{} {}",
            env!("CARGO_PKG_NAME"),
            env!("CARGO_PKG_VERSION")
        )
        .map_err(Error::Output),
        Command::Status(args) => status::status(&args, env, out),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(line: &[&str]) -> Vec<OsString> {
        line.iter().map(OsString::from).collect()
    }

    /// Runs the program with an empty environment and returns its exit
    /// code, standard output and standard error.
    fn program(line: &[&str]) -> (u8, String, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run(args(line), &|_| None, &mut out, &mut err);
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    #[test]
    fn status_is_parsed_with_its_options_in_any_order() {
        assert_eq!(
            parse(args(&["status"])).unwrap(),
            Command::Status(StatusArgs::default())
        );
        let expected = Command::Status(StatusArgs {
            data_dir: Some(PathBuf::from("/srv/ritornel")),
            json: true,
        });
        for line in [
            &["status", "--data-dir", "/srv/ritornel", "--json"][..],
            &["status", "--json", "--data-dir", "/srv/ritornel"],
        ] {
            assert_eq!(parse(args(line)).unwrap(), expected, "{line:?}");
        }
    }

    #[test]
    fn a_wrong_command_line_is_refused_with_the_usage_exit_code() {
        for line in [
            &[][..],
            &["--json"],
            &["--data-dir", "/srv/ritornel", "status"],
            &["status", "status"],
            &["status", "--data-dir"],
            &["status", "--data-dir", ""],
            &["status", "--data-dir", "--json"],
            &["status", "--yaml"],
            &["serve"],
        ] {
            let (code, out, err) = program(line);
            assert_eq!(code, EXIT_USAGE, "{line:?}");
            assert_eq!(out, "", "{line:?}");
            assert!(err.starts_with("ritornelctl: "), "{line:?}: {err}");
        }
    }

    #[test]
    fn an_error_message_repeats_no_argument() {
        let (_, _, err) = program(&["status", "--tokne", "SENTINEL_ARGUMENT_0001"]);
        assert!(!err.contains("SENTINEL"), "{err}");
    }

    #[test]
    fn help_and_version_print_and_succeed() {
        let (code, out, err) = program(&["--help"]);
        assert_eq!((code, err.as_str()), (0, ""));
        assert!(out.contains("ritornelctl status"), "{out}");
        assert!(out.contains("4    No server is running"), "{out}");
        // Help wins wherever it is.
        assert_eq!(program(&["status", "--json", "-h"]).1, out);

        let (code, out, _) = program(&["--version"]);
        assert_eq!(code, 0);
        assert_eq!(
            out,
            concat!("ritornelctl ", env!("CARGO_PKG_VERSION"), "\n")
        );
    }

    #[test]
    fn with_no_data_directory_to_use_it_says_how_to_name_one() {
        let (code, out, err) = program(&["status"]);
        assert_eq!(code, EXIT_FAILED);
        assert_eq!(out, "");
        assert_eq!(
            err,
            "ritornelctl: no data directory: give --data-dir or set RITORNEL_DATA_DIR\n"
        );
    }

    #[test]
    fn with_no_server_it_says_so_and_exits_with_the_code_kept_for_that() {
        let dir = std::env::temp_dir().join(format!("ritornelctl-unit-{}", std::process::id()));
        let (code, out, err) = program(&["status", "--data-dir", dir.to_str().unwrap()]);
        assert_eq!(code, EXIT_NO_SERVER);
        assert_eq!(out, "");
        assert_eq!(
            err,
            format!("ritornelctl: no server is running on {}\n", dir.display())
        );
        // Looking made nothing.
        assert!(!dir.exists());
    }
}
