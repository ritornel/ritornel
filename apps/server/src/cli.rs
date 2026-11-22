//! The command line.
//!
//! ```text
//! ritornel-server serve [--data-dir <dir>] [--listen <address>] [--owned]
//!                       [--log-level <level>] [--output <output>]
//!                       [--backup-dir <dir>]
//! ritornel-server service install [--data-dir <dir>]
//! ritornel-server service uninstall | status
//! ```
//!
//! clap parses it. The help is [`USAGE`] and the messages are the fixed
//! ones in [`usage`], so nothing a person typed is ever printed back.

use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;

use clap::builder::NonEmptyStringValueParser;
use clap::error::{ContextKind, ErrorKind};
use clap::{Args, Parser, Subcommand};

use crate::{Env, Error, serve};

const USAGE: &str = "\
The Ritornel server. It owns a data directory and the databases in it.

Usage:
  ritornel-server serve [--data-dir <dir>] [--listen <address>] [--owned]
                        [--log-level <level>] [--output <output>]
                        [--backup-dir <dir>]
  ritornel-server service install [--data-dir <dir>]
  ritornel-server service uninstall | status

Commands:
  serve              Run until asked to stop: by SIGINT or SIGTERM, or
                     with --owned by the end of standard input
  service install    Run serve on the data directory in the background,
                     now and at each login, as a launchd user agent on
                     macOS or a systemd user unit on Linux. The desktop
                     app then uses it and starts no
                     server of its own, and quitting the app leaves it
                     playing. It replaces one installed before
  service uninstall  Stop the background service and remove it
  service status     Say whether it is installed and running

Options:
  --data-dir <dir>      Where the server keeps everything it owns, created
                        if it is missing. One server runs on a directory
  --listen <address>    Where to listen: 127.0.0.1 and a port. The default is
                        127.0.0.1:4747, or a port the system picks if that
                        one is taken. A port given here is used or the
                        server does not start, and port 0 asks the system
                        for one. Clients read the port from server.json in
                        the data directory. private and a port, such as
                        private:4747, listens on every interface and takes
                        connections from private networks only, for other
                        machines' servers to link to this one
  --owned               Stop when standard input closes. A program that
                        starts the server holds the other end of that pipe,
                        so the server cannot outlive it
  --log-level <level>   error, warn, info, debug or trace. The default is
                        info
  --output <output>     Where the player plays: default, the system's
                        default output; null, which plays at real-time
                        speed into nothing and makes no sound; or none, for
                        a server with no player. The default is default
                        where this build has an audio output, and none
                        elsewhere. The device is opened when something is
                        first played
  --backup-dir <dir>    The folder backups are written to, outside the data
                        directory: on another disk where there is one. It
                        must already exist. Each backup is a folder of its
                        own in it, taken by `ritornelctl backup` and before
                        a database is migrated. With none named there is no
                        backup, and `ritornelctl status` says so
  -h, --help            Show this help
  -V, --version         Show the version

Environment:
  RITORNEL_DATA_DIR     The data directory, when --data-dir is not given.
                        With neither it is
                        ~/Library/Application Support/Ritornel on macOS,
                        and $XDG_DATA_HOME/ritornel elsewhere, which is
                        ~/.local/share/ritornel unless you moved it
  RITORNEL_LISTEN       The listen address, when --listen is not given
  RITORNEL_LOG_LEVEL    The log level, when --log-level is not given
  RITORNEL_BACKUP_DIR   The backup folder, when --backup-dir is not given

A setting is read from its flag, then its environment variable, then
config.toml in the data directory, then its default.

Exit codes:
  0    Stopped cleanly
  1    Failed
  2    The command line was wrong
  3    The data directory is in use by another server
";

/// The command line, as clap reads it.
#[derive(Debug, PartialEq, Parser)]
#[command(name = "ritornel-server", version, propagate_version = true)]
#[command(disable_help_subcommand = true)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, PartialEq, Subcommand)]
enum Command {
    Serve(ServeArgs),
    Service(crate::service::ServiceArgs),
    /// Developer-only, and not in the help: fills a library with an
    /// invented one. See `crate::invent`.
    #[command(hide = true)]
    Invent(crate::invent::InventArgs),
}

/// The arguments of `ritornel-server serve`.
///
/// A token that looks like an option is never the value of one: `--data-dir
/// --owned` is a missing directory and not a directory named `--owned`,
/// which would start a server that no longer stops with its owner. A
/// directory whose name does begin with a hyphen is written `./-name`. An
/// empty value is a missing one too.
#[derive(Debug, Default, PartialEq, Eq, Args)]
pub(crate) struct ServeArgs {
    #[arg(long)]
    pub(crate) data_dir: Option<PathBuf>,
    #[arg(long)]
    pub(crate) owned: bool,
    #[arg(long, value_parser = NonEmptyStringValueParser::new())]
    pub(crate) listen: Option<String>,
    #[arg(long, value_parser = NonEmptyStringValueParser::new())]
    pub(crate) log_level: Option<String>,
    /// Developer-only, and not in the help: writes to an invented library
    /// this many times a second while serving, as a scan would.
    #[arg(long, hide = true)]
    pub(crate) invent_churn: Option<u32>,
    #[arg(long, value_parser = ["default", "null", "none"])]
    pub(crate) output: Option<String>,
    #[arg(long, value_parser = NonEmptyStringValueParser::new())]
    pub(crate) backup_dir: Option<String>,
}

/// What a wrong command line is answered with. The messages are fixed and
/// repeat no argument: clap's own would print back what was typed.
fn usage(error: &clap::Error) -> Error {
    // The context holds the option as this program declares it, never what
    // was typed.
    let option = error
        .get(ContextKind::InvalidArg)
        .map(ToString::to_string)
        .unwrap_or_default();
    Error::Usage(match error.kind() {
        ErrorKind::UnknownArgument | ErrorKind::InvalidSubcommand => {
            "unknown argument; try `ritornel-server --help`"
        }
        ErrorKind::MissingSubcommand | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => {
            "expected `ritornel-server serve` or `service`; try `ritornel-server --help`"
        }
        ErrorKind::InvalidValue | ErrorKind::ValueValidation
            if option.starts_with("--data-dir") =>
        {
            "--data-dir needs a directory"
        }
        ErrorKind::InvalidValue | ErrorKind::ValueValidation if option.starts_with("--listen") => {
            "--listen needs an address"
        }
        ErrorKind::InvalidValue | ErrorKind::ValueValidation
            if option.starts_with("--log-level") =>
        {
            "--log-level needs a level"
        }
        ErrorKind::InvalidValue | ErrorKind::ValueValidation if option.starts_with("--output") => {
            "--output is default, null or none"
        }
        ErrorKind::InvalidValue | ErrorKind::ValueValidation
            if option.starts_with("--backup-dir") =>
        {
            "--backup-dir needs a folder"
        }
        _ => "the command line is wrong; try `ritornel-server --help`",
    })
}

/// Parses the arguments after the program name.
fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Cli, clap::Error> {
    Cli::try_parse_from(std::iter::once(OsString::from(env!("CARGO_PKG_NAME"))).chain(args))
}

/// Runs the program and returns its exit code. The ready line, the help and
/// the version go to `out`, and an error to `err`.
///
/// `serve` blocks the thread it is called on until the server stops, and
/// that must be the process's main thread.
pub fn run(
    args: impl IntoIterator<Item = OsString>,
    env: Env<'_>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> u8 {
    match try_run(args, env, out) {
        Ok(()) => 0,
        Err(error) => {
            // Nothing useful can be done if the terminal is gone.
            let _ = writeln!(err, "ritornel-server: {error}");
            error.exit_code()
        }
    }
}

fn try_run(
    args: impl IntoIterator<Item = OsString>,
    env: Env<'_>,
    out: &mut dyn Write,
) -> Result<(), Error> {
    match parse(args) {
        Ok(Cli {
            command: Command::Serve(args),
        }) => serve::serve(&args, env, out),
        Ok(Cli {
            command: Command::Service(args),
        }) => crate::service::run(&args, env, out),
        Ok(Cli {
            command: Command::Invent(args),
        }) => crate::invent::run(&args, env, out),
        // Help wins wherever it is, and it is the same help.
        Err(error) if error.kind() == ErrorKind::DisplayHelp => {
            out.write_all(USAGE.as_bytes()).map_err(Error::Output)
        }
        Err(error) if error.kind() == ErrorKind::DisplayVersion => writeln!(
            out,
            "{} {}",
            env!("CARGO_PKG_NAME"),
            env!("CARGO_PKG_VERSION")
        )
        .map_err(Error::Output),
        Err(error) => Err(usage(&error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::env;
    use crate::{EXIT_FAILED, EXIT_USAGE};

    fn args(line: &[&str]) -> Vec<OsString> {
        line.iter().map(OsString::from).collect()
    }

    /// Runs the program and returns its exit code, standard output and
    /// standard error.
    fn program(line: &[&str], variables: &[(&str, &str)]) -> (u8, String, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run(args(line), &env(variables), &mut out, &mut err);
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    #[test]
    fn serve_is_parsed_with_its_options_in_any_order() {
        assert_eq!(
            parse(args(&["serve"])).unwrap().command,
            Command::Serve(ServeArgs::default())
        );
        let expected = Command::Serve(ServeArgs {
            data_dir: Some(PathBuf::from("/srv/ritornel")),
            owned: true,
            listen: Some("127.0.0.1:5000".to_owned()),
            log_level: Some("debug".to_owned()),
            invent_churn: None,
            output: None,
            backup_dir: None,
        });
        for line in [
            &[
                "serve",
                "--data-dir",
                "/srv/ritornel",
                "--owned",
                "--listen",
                "127.0.0.1:5000",
                "--log-level",
                "debug",
            ][..],
            &[
                "serve",
                "--log-level",
                "debug",
                "--owned",
                "--data-dir",
                "/srv/ritornel",
                "--listen",
                "127.0.0.1:5000",
            ],
            &[
                "serve",
                "--log-level=debug",
                "--owned",
                "--data-dir=/srv/ritornel",
                "--listen=127.0.0.1:5000",
            ],
        ] {
            assert_eq!(parse(args(line)).unwrap().command, expected, "{line:?}");
        }
    }

    #[test]
    fn a_wrong_command_line_is_refused_with_the_usage_exit_code() {
        for line in [
            &[][..],
            &["--owned"],
            &["--data-dir", "/srv/ritornel", "serve"],
            &["serve", "serve"],
            &["serve", "--data-dir"],
            &["serve", "--data-dir", ""],
            &["serve", "--data-dir", "--owned"],
            &["serve", "--data-dir", "-x"],
            &["serve", "--log-level"],
            &["serve", "--log-level", ""],
            &["serve", "--log-level", "--owned"],
            &["serve", "--listen"],
            &["serve", "--listen", ""],
            &["serve", "--listen", "--owned"],
            &["serve", "--owned", "--owned"],
            &["serve", "--port", "4747"],
            &["status"],
            &["help"],
        ] {
            let (code, out, err) = program(line, &[]);
            assert_eq!(code, EXIT_USAGE, "{line:?}");
            assert_eq!(out, "", "{line:?}");
            assert!(err.starts_with("ritornel-server: "), "{line:?}: {err}");
            assert_eq!(err.lines().count(), 1, "{line:?}: {err}");
        }
    }

    #[test]
    fn each_way_of_being_wrong_has_its_own_fixed_message() {
        for (line, message) in [
            (
                &["status"][..],
                "unknown argument; try `ritornel-server --help`",
            ),
            (
                &["serve", "--port", "4747"],
                "unknown argument; try `ritornel-server --help`",
            ),
            (
                &[],
                "expected `ritornel-server serve` or `service`; try `ritornel-server --help`",
            ),
            (&["serve", "--data-dir"], "--data-dir needs a directory"),
            (&["serve", "--data-dir", ""], "--data-dir needs a directory"),
            (
                &["serve", "--data-dir", "--owned"],
                "--data-dir needs a directory",
            ),
            (&["serve", "--listen"], "--listen needs an address"),
            (&["serve", "--listen", ""], "--listen needs an address"),
            (
                &["serve", "--log-level", "--owned"],
                "--log-level needs a level",
            ),
            (
                &["serve", "--owned", "--owned"],
                "the command line is wrong; try `ritornel-server --help`",
            ),
        ] {
            let (_, _, err) = program(line, &[]);
            assert_eq!(err, format!("ritornel-server: {message}\n"), "{line:?}");
        }
    }

    #[test]
    fn an_error_message_repeats_no_argument() {
        for line in [
            &["serve", "--tokne", "SENTINEL_ARGUMENT_0001"][..],
            &["serve", "SENTINEL_ARGUMENT_0001"],
            &["SENTINEL_ARGUMENT_0001"],
            &["serve", "--SENTINEL_ARGUMENT_0001"],
            &["serve", "--owned=SENTINEL_ARGUMENT_0001"],
            &["serve", "--data-dir", "-SENTINEL_ARGUMENT_0001"],
        ] {
            let (code, _, err) = program(line, &[]);
            assert_eq!(code, EXIT_USAGE, "{line:?}");
            assert!(!err.contains("SENTINEL"), "{err}");
        }
    }

    #[test]
    fn help_and_version_print_and_succeed() {
        let (code, out, err) = program(&["--help"], &[]);
        assert_eq!((code, err.as_str()), (0, ""));
        assert!(out.contains("ritornel-server serve"), "{out}");
        assert!(out.contains("3    The data directory is in use"), "{out}");
        // Help wins wherever it is.
        assert_eq!(program(&["serve", "--owned", "-h"], &[]).1, out);

        for line in [&["--version"][..], &["-V"], &["serve", "-V"]] {
            let (code, out, _) = program(line, &[]);
            assert_eq!(code, 0, "{line:?}");
            assert_eq!(
                out,
                concat!("ritornel-server ", env!("CARGO_PKG_VERSION"), "\n")
            );
        }
    }

    #[test]
    fn a_bad_setting_stops_the_server_before_it_starts() {
        let scratch = crate::testing::Scratch::new();
        let dir = scratch.path().to_str().unwrap();
        let (code, out, err) = program(&["serve", "--data-dir", dir, "--log-level", "loud"], &[]);
        assert_eq!(code, EXIT_FAILED);
        assert_eq!(out, "");
        assert_eq!(
            err,
            "ritornel-server: --log-level must be error, warn, info, debug or trace\n"
        );
        // It got as far as the lock and let it go again.
        assert!(!scratch.path().join("server.json").exists());
        crate::HeldDir::take(scratch.path().to_owned()).unwrap();
    }

    #[test]
    fn an_address_beyond_loopback_stops_the_server_before_it_starts() {
        let scratch = crate::testing::Scratch::new();
        let dir = scratch.path().to_str().unwrap();
        let (code, out, err) =
            program(&["serve", "--data-dir", dir, "--listen", "0.0.0.0:80"], &[]);
        assert_eq!(code, EXIT_FAILED);
        assert_eq!(out, "");
        assert_eq!(
            err,
            "ritornel-server: --listen must be 127.0.0.1 and a port, such as 127.0.0.1:4747, \
             or private and a port, such as private:4747: the server listens on loopback or \
             the private network only\n"
        );
        assert!(!scratch.path().join("server.json").exists());
        assert!(!scratch.path().join("server.db-wal").exists());
    }

    #[test]
    fn with_no_data_directory_to_use_it_says_how_to_name_one() {
        let (code, _, err) = program(&["serve"], &[]);
        assert_eq!(code, EXIT_FAILED);
        assert_eq!(
            err,
            "ritornel-server: no data directory: give --data-dir or set RITORNEL_DATA_DIR\n"
        );
    }
}
