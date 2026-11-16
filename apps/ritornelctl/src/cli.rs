//! The command line.
//!
//! ```text
//! ritornelctl status [--data-dir <dir>] [--json]
//! ritornelctl library create [--data-dir <dir>] [--json]
//! ritornelctl library stats [--data-dir <dir>] [--json]
//! ritornelctl scan <folder> [--name <name>] [--no-wait] [--data-dir <dir>] [--json]
//! ritornelctl jobs [<job>] [--cancel] [--data-dir <dir>] [--json]
//! ```
//!
//! clap parses it. The help is [`USAGE`] and the messages are the fixed
//! ones in [`usage`], so nothing a person typed is ever printed back.

use std::ffi::OsString;
use std::fmt;
use std::io::Write;
use std::path::PathBuf;

use clap::error::{ContextKind, ErrorKind};
use clap::{Args, Parser, Subcommand};
use ritornel_client::contract::local::Env;

use crate::{jobs, library, scan, status};

const USAGE: &str = "\
The command-line client of a Ritornel server. It talks to the server that is
running on a data directory, and never starts one.

Usage:
  ritornelctl status [--data-dir <dir>] [--json]
  ritornelctl library create [--data-dir <dir>] [--json]
  ritornelctl library stats [--data-dir <dir>] [--json]
  ritornelctl scan <folder> [--name <name>] [--no-wait] [--data-dir <dir>] [--json]
  ritornelctl jobs [<job>] [--cancel] [--data-dir <dir>] [--json]

Commands:
  status           Show the running server's ID, name and role, the versions
                   it is built from, the library it holds, the free disk
                   space and where its logs are
  library create   Create a library on the server and make the server its
                   hub. A server holds one library, and one that has a
                   library already refuses
  library stats    Show what the library holds and what the scans made of
                   it: files, what could not be read and why, artists,
                   albums and songs, how each file was matched, and how
                   many matches want a look
  scan <folder>    Scan a folder of music into the library, and show the
                   scan until it ends. The folder is added as a source if
                   it is not one. The server only ever reads it. The scan
                   is the server's: it carries on if this program stops
  jobs [<job>]     List the server's jobs, newest first, or show one with
                   what its scan found and what could not be read

Options:
  --name <name>         scan: the name the folder is added under. With none
                        it is the folder's own name
  --no-wait             scan: start the scan and return at once
  --cancel              jobs: ask the job to stop. It stops after the files
                        it is reading, and what it has recorded stays
  --data-dir <dir>      The data directory of the server to talk to
  --json                Print the answer as JSON, in the shape the server
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
    /// The folder to scan could not be found.
    Folder(std::io::Error),
    /// The job that was waited for did not succeed. The text is its state.
    Job(&'static str),
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
            Self::Folder(error) => write!(f, "cannot scan that folder: {error}"),
            Self::Job(state) => write!(f, "the job {state}"),
        }
    }
}

impl From<ritornel_client::Error> for Error {
    fn from(error: ritornel_client::Error) -> Self {
        Self::Client(error)
    }
}

/// The command line, as clap reads it.
#[derive(Debug, PartialEq, Eq, Parser)]
#[command(name = "ritornelctl", version, propagate_version = true)]
#[command(disable_help_subcommand = true)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, PartialEq, Eq, Subcommand)]
enum Command {
    Status(ServerArgs),
    #[command(subcommand)]
    Library(LibraryCommand),
    Scan(ScanArgs),
    Jobs(JobsArgs),
}

#[derive(Debug, PartialEq, Eq, Subcommand)]
enum LibraryCommand {
    Create(ServerArgs),
    Stats(ServerArgs),
}

/// What `scan` takes.
#[derive(Debug, Default, PartialEq, Eq, Args)]
pub(crate) struct ScanArgs {
    /// The folder to scan.
    pub(crate) folder: PathBuf,
    /// The name the folder is added under, if it is not a source yet.
    #[arg(long)]
    pub(crate) name: Option<String>,
    /// Start the scan and return at once.
    #[arg(long)]
    pub(crate) no_wait: bool,
    #[command(flatten)]
    pub(crate) server: ServerArgs,
}

/// What `jobs` takes.
#[derive(Debug, Default, PartialEq, Eq, Args)]
pub(crate) struct JobsArgs {
    /// The job to show. With none, the jobs are listed.
    pub(crate) job: Option<String>,
    /// Ask the job to stop.
    #[arg(long)]
    pub(crate) cancel: bool,
    #[command(flatten)]
    pub(crate) server: ServerArgs,
}

/// What every command takes: which server to talk to, and how to print
/// what it answers.
#[derive(Debug, Default, PartialEq, Eq, Args)]
pub(crate) struct ServerArgs {
    /// A token that looks like an option is never the value: `--data-dir
    /// --json` is a missing directory, not a directory named `--json`.
    /// One whose name does begin with a hyphen is written `./-name`.
    #[arg(long)]
    pub(crate) data_dir: Option<PathBuf>,
    #[arg(long)]
    pub(crate) json: bool,
}

/// What a wrong command line is answered with. The messages are fixed and
/// repeat no argument: clap's own would print back what was typed.
fn usage(error: &clap::Error) -> Error {
    Error::Usage(match error.kind() {
        ErrorKind::UnknownArgument | ErrorKind::InvalidSubcommand => {
            "unknown argument; try `ritornelctl --help`"
        }
        ErrorKind::MissingSubcommand | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => {
            "expected a command; try `ritornelctl --help`"
        }
        ErrorKind::InvalidValue | ErrorKind::ValueValidation if names(error, "--data-dir") => {
            "--data-dir needs a directory"
        }
        _ => "the command line is wrong; try `ritornelctl --help`",
    })
}

/// Whether the option an error is about is `option`. The context holds the
/// option as this program declares it, never what was typed.
fn names(error: &clap::Error, option: &str) -> bool {
    error
        .get(ContextKind::InvalidArg)
        .is_some_and(|arg| arg.to_string().starts_with(option))
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

/// Parses the arguments after the program name.
fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Cli, clap::Error> {
    Cli::try_parse_from(std::iter::once(OsString::from(env!("CARGO_PKG_NAME"))).chain(args))
}

fn try_run(
    args: impl IntoIterator<Item = OsString>,
    env: Env<'_>,
    out: &mut dyn Write,
) -> Result<(), Error> {
    let command = match parse(args) {
        Ok(cli) => cli.command,
        // Help wins wherever it is, and it is the same help.
        Err(error) if error.kind() == ErrorKind::DisplayHelp => {
            return out.write_all(USAGE.as_bytes()).map_err(Error::Output);
        }
        Err(error) if error.kind() == ErrorKind::DisplayVersion => {
            return writeln!(
                out,
                "{} {}",
                env!("CARGO_PKG_NAME"),
                env!("CARGO_PKG_VERSION")
            )
            .map_err(Error::Output);
        }
        Err(error) => return Err(usage(&error)),
    };
    match command {
        Command::Status(args) => status::status(&args, env, out),
        Command::Library(LibraryCommand::Create(args)) => library::create(&args, env, out),
        Command::Library(LibraryCommand::Stats(args)) => library::stats(&args, env, out),
        Command::Scan(args) => scan::scan(&args, env, out),
        Command::Jobs(args) => jobs::jobs(&args, env, out),
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
    fn a_command_is_parsed_with_its_options_in_any_order() {
        assert_eq!(
            parse(args(&["status"])).unwrap().command,
            Command::Status(ServerArgs::default())
        );
        let given = || ServerArgs {
            data_dir: Some(PathBuf::from("/srv/ritornel")),
            json: true,
        };
        for line in [
            &["status", "--data-dir", "/srv/ritornel", "--json"][..],
            &["status", "--json", "--data-dir", "/srv/ritornel"],
            &["status", "--json", "--data-dir=/srv/ritornel"],
        ] {
            assert_eq!(
                parse(args(line)).unwrap().command,
                Command::Status(given()),
                "{line:?}"
            );
        }
        assert_eq!(
            parse(args(&["library", "create"])).unwrap().command,
            Command::Library(LibraryCommand::Create(ServerArgs::default()))
        );
        assert_eq!(
            parse(args(&[
                "library",
                "create",
                "--json",
                "--data-dir",
                "/srv/ritornel"
            ]))
            .unwrap()
            .command,
            Command::Library(LibraryCommand::Create(given()))
        );
    }

    #[test]
    fn scan_and_jobs_take_what_they_work_on_and_their_own_options() {
        assert_eq!(
            parse(args(&[
                "scan",
                "/music",
                "--no-wait",
                "--name",
                "main",
                "--json"
            ]))
            .unwrap()
            .command,
            Command::Scan(ScanArgs {
                folder: PathBuf::from("/music"),
                name: Some("main".to_owned()),
                no_wait: true,
                server: ServerArgs {
                    data_dir: None,
                    json: true
                },
            })
        );
        assert_eq!(
            parse(args(&["jobs"])).unwrap().command,
            Command::Jobs(JobsArgs::default())
        );
        assert_eq!(
            parse(args(&["jobs", "abc", "--cancel"])).unwrap().command,
            Command::Jobs(JobsArgs {
                job: Some("abc".to_owned()),
                cancel: true,
                server: ServerArgs::default(),
            })
        );
        assert_eq!(
            parse(args(&["library", "stats", "--json"]))
                .unwrap()
                .command,
            Command::Library(LibraryCommand::Stats(ServerArgs {
                data_dir: None,
                json: true
            }))
        );
        // What is wrong with these is known before any server is looked
        // for.
        for (line, message) in [
            (
                &["scan"][..],
                "the command line is wrong; try `ritornelctl --help`",
            ),
            (
                &["scan", "a", "b"],
                "unknown argument; try `ritornelctl --help`",
            ),
            (&["jobs", "--cancel"], "--cancel needs a job's ID"),
            (&["jobs", "not-an-id"], "that is not a job's ID"),
        ] {
            let (code, out, err) = program(line);
            assert_eq!((code, out.as_str()), (EXIT_USAGE, ""), "{line:?}");
            assert_eq!(err, format!("ritornelctl: {message}\n"), "{line:?}");
        }
        let (code, _, err) = program(&["scan", "/there/is/no/such/folder"]);
        assert_eq!(code, EXIT_FAILED);
        assert!(
            err.starts_with("ritornelctl: cannot scan that folder: "),
            "{err}"
        );
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
            &["status", "--data-dir", "-x"],
            &["status", "--json", "--json"],
            &["status", "--yaml"],
            &["serve"],
            &["library"],
            &["library", "--json"],
            &["library", "destroy"],
            &["library", "create", "now"],
            &["library", "create", "--data-dir"],
            &["help"],
        ] {
            let (code, out, err) = program(line);
            assert_eq!(code, EXIT_USAGE, "{line:?}");
            assert_eq!(out, "", "{line:?}");
            assert!(err.starts_with("ritornelctl: "), "{line:?}: {err}");
            assert_eq!(err.lines().count(), 1, "{line:?}: {err}");
        }
    }

    #[test]
    fn each_way_of_being_wrong_has_its_own_fixed_message() {
        for (line, message) in [
            (&["serve"][..], "unknown argument; try `ritornelctl --help`"),
            (
                &["status", "--yaml"],
                "unknown argument; try `ritornelctl --help`",
            ),
            (&[], "expected a command; try `ritornelctl --help`"),
            (&["library"], "expected a command; try `ritornelctl --help`"),
            (&["status", "--data-dir"], "--data-dir needs a directory"),
            (
                &["status", "--data-dir", ""],
                "--data-dir needs a directory",
            ),
            (
                &["library", "create", "--data-dir", "--json"],
                "--data-dir needs a directory",
            ),
            (
                &["status", "--json", "--json"],
                "the command line is wrong; try `ritornelctl --help`",
            ),
        ] {
            let (_, _, err) = program(line);
            assert_eq!(err, format!("ritornelctl: {message}\n"), "{line:?}");
        }
    }

    #[test]
    fn an_error_message_repeats_no_argument() {
        for line in [
            &["status", "--tokne", "SENTINEL_ARGUMENT_0001"][..],
            &["status", "SENTINEL_ARGUMENT_0001"],
            &["SENTINEL_ARGUMENT_0001"],
            &["status", "--SENTINEL_ARGUMENT_0001"],
            &["status", "--json=SENTINEL_ARGUMENT_0001"],
            &["library", "create", "--data-dir", "-SENTINEL_ARGUMENT_0001"],
        ] {
            let (code, _, err) = program(line);
            assert_eq!(code, EXIT_USAGE, "{line:?}");
            assert!(!err.contains("SENTINEL"), "{err}");
        }
    }

    #[test]
    fn help_and_version_print_and_succeed() {
        let (code, out, err) = program(&["--help"]);
        assert_eq!((code, err.as_str()), (0, ""));
        assert!(out.contains("ritornelctl status"), "{out}");
        assert!(out.contains("ritornelctl library create"), "{out}");
        assert!(out.contains("4    No server is running"), "{out}");
        // Help wins wherever it is.
        assert_eq!(program(&["status", "--json", "-h"]).1, out);
        assert_eq!(program(&["library", "--help"]).1, out);
        assert_eq!(program(&["library", "create", "-h"]).1, out);

        for line in [&["--version"][..], &["-V"], &["status", "-V"]] {
            let (code, out, _) = program(line);
            assert_eq!(code, 0, "{line:?}");
            assert_eq!(
                out,
                concat!("ritornelctl ", env!("CARGO_PKG_VERSION"), "\n")
            );
        }
    }

    #[test]
    fn with_no_data_directory_to_use_it_says_how_to_name_one() {
        for line in [&["status"][..], &["library", "create"]] {
            let (code, out, err) = program(line);
            assert_eq!(code, EXIT_FAILED, "{line:?}");
            assert_eq!(out, "");
            assert_eq!(
                err,
                "ritornelctl: no data directory: give --data-dir or set RITORNEL_DATA_DIR\n"
            );
        }
    }

    #[test]
    fn with_no_server_it_says_so_and_exits_with_the_code_kept_for_that() {
        let dir = std::env::temp_dir().join(format!("ritornelctl-unit-{}", std::process::id()));
        for line in [&["status"][..], &["library", "create"]] {
            let mut line = line.to_vec();
            line.extend(["--data-dir", dir.to_str().unwrap()]);
            let (code, out, err) = program(&line);
            assert_eq!(code, EXIT_NO_SERVER, "{line:?}");
            assert_eq!(out, "");
            assert_eq!(
                err,
                format!("ritornelctl: no server is running on {}\n", dir.display())
            );
        }
        // Looking made nothing.
        assert!(!dir.exists());
    }
}
