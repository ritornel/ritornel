//! The command line.
//!
//! ```text
//! ritornel-server serve [--data-dir <dir>] [--listen <address>] [--owned]
//!                       [--log-level <level>]
//! ```

use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;

use crate::{Env, Error, serve};

const USAGE: &str = "\
The Ritornel server. It owns a data directory and the databases in it.

Usage:
  ritornel-server serve [--data-dir <dir>] [--listen <address>] [--owned]
                        [--log-level <level>]

Commands:
  serve    Run until asked to stop: by SIGINT or SIGTERM, or with --owned
           by the end of standard input

Options:
  --data-dir <dir>      Where the server keeps everything it owns, created
                        if it is missing. One server runs on a directory
  --listen <address>    Where to listen: 127.0.0.1 and a port. The default is
                        127.0.0.1:4747, or a port the system picks if that
                        one is taken. A port given here is used or the
                        server does not start, and port 0 asks the system
                        for one. Clients read the port from server.json in
                        the data directory
  --owned               Stop when standard input closes. A program that
                        starts the server holds the other end of that pipe,
                        so the server cannot outlive it
  --log-level <level>   error, warn, info, debug or trace. The default is
                        info
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

A setting is read from its flag, then its environment variable, then
config.toml in the data directory, then its default.

Exit codes:
  0    Stopped cleanly
  1    Failed
  2    The command line was wrong
  3    The data directory is in use by another server
";

/// The arguments of `ritornel-server serve`.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct ServeArgs {
    pub(crate) data_dir: Option<PathBuf>,
    pub(crate) owned: bool,
    pub(crate) listen: Option<String>,
    pub(crate) log_level: Option<String>,
}

/// A parsed command line.
#[derive(Debug, PartialEq, Eq)]
enum Command {
    Help,
    Version,
    Serve(ServeArgs),
}

/// Parses the arguments after the program name. The messages are fixed and
/// repeat no argument.
fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Command, Error> {
    let mut args = args.into_iter();
    let (mut serve, mut parsed) = (false, ServeArgs::default());
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("-h" | "--help") => return Ok(Command::Help),
            Some("-V" | "--version") => return Ok(Command::Version),
            Some("serve") if !serve => serve = true,
            Some("--data-dir") if serve => match value(&mut args) {
                Some(dir) => parsed.data_dir = Some(PathBuf::from(dir)),
                None => return Err(Error::Usage("--data-dir needs a directory")),
            },
            Some("--listen") if serve => match value(&mut args).map(OsString::into_string) {
                Some(Ok(address)) => parsed.listen = Some(address),
                _ => return Err(Error::Usage("--listen needs an address")),
            },
            Some("--log-level") if serve => match value(&mut args).map(OsString::into_string) {
                Some(Ok(level)) => parsed.log_level = Some(level),
                _ => return Err(Error::Usage("--log-level needs a level")),
            },
            Some("--owned") if serve => parsed.owned = true,
            _ => {
                return Err(Error::Usage(
                    "unknown argument; try `ritornel-server --help`",
                ));
            }
        }
    }
    if serve {
        Ok(Command::Serve(parsed))
    } else {
        Err(Error::Usage(
            "expected `ritornel-server serve`; try `ritornel-server --help`",
        ))
    }
}

/// The value of an option: the next argument, unless there is none, it is
/// empty, or it looks like an option itself. `--data-dir --owned` is then a
/// missing directory and not a directory named `--owned`, which would start
/// a server that no longer stops with its owner. A directory whose name does
/// begin with a hyphen is written `./-name`.
fn value(args: &mut impl Iterator<Item = OsString>) -> Option<OsString> {
    args.next()
        .filter(|value| !value.is_empty() && !value.as_encoded_bytes().starts_with(b"-"))
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
    match parse(args)? {
        Command::Help => out.write_all(USAGE.as_bytes()).map_err(Error::Output),
        Command::Version => writeln!(
            out,
            "{} {}",
            env!("CARGO_PKG_NAME"),
            env!("CARGO_PKG_VERSION")
        )
        .map_err(Error::Output),
        Command::Serve(args) => serve::serve(&args, env, out),
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
            parse(args(&["serve"])).unwrap(),
            Command::Serve(ServeArgs::default())
        );
        let expected = Command::Serve(ServeArgs {
            data_dir: Some(PathBuf::from("/srv/ritornel")),
            owned: true,
            listen: Some("127.0.0.1:5000".to_owned()),
            log_level: Some("debug".to_owned()),
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
        ] {
            assert_eq!(parse(args(line)).unwrap(), expected, "{line:?}");
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
            &["serve", "--log-level", "--owned"],
            &["serve", "--listen"],
            &["serve", "--listen", "--owned"],
            &["serve", "--port", "4747"],
            &["status"],
        ] {
            let (code, out, err) = program(line, &[]);
            assert_eq!(code, EXIT_USAGE, "{line:?}");
            assert_eq!(out, "", "{line:?}");
            assert!(err.starts_with("ritornel-server: "), "{line:?}: {err}");
        }
    }

    #[test]
    fn an_error_message_repeats_no_argument() {
        let (_, _, err) = program(&["serve", "--tokne", "SENTINEL_ARGUMENT_0001"], &[]);
        assert!(!err.contains("SENTINEL"), "{err}");
    }

    #[test]
    fn help_and_version_print_and_succeed() {
        let (code, out, err) = program(&["--help"], &[]);
        assert_eq!((code, err.as_str()), (0, ""));
        assert!(out.contains("ritornel-server serve"), "{out}");
        assert!(out.contains("3    The data directory is in use"), "{out}");
        // Help wins wherever it is.
        assert_eq!(program(&["serve", "--owned", "-h"], &[]).1, out);

        let (code, out, _) = program(&["--version"], &[]);
        assert_eq!(code, 0);
        assert_eq!(
            out,
            concat!("ritornel-server ", env!("CARGO_PKG_VERSION"), "\n")
        );
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
            "ritornel-server: --listen must be 127.0.0.1 and a port, such as 127.0.0.1:4747: \
             the server listens on loopback only\n"
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
