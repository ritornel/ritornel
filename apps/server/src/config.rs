//! Settings. Each is read from its flag, then its `RITORNEL_*` environment
//! variable, then `config.toml` in the data directory, then its default. See
//! decision 26 and "Configuration and logs" in docs/design/server.md.
//!
//! `config.toml` holds only what the server needs before a database opens,
//! and a container needs no file at all. The settings so far are the log
//! level, the listen address and where backups go, which a migration
//! needs before it opens a database.

use std::ffi::OsString;
use std::fmt;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use tracing::Level;

use crate::error::io_error;
use crate::{Env, Error};

/// The environment variable that sets the log level.
pub(crate) const LOG_LEVEL_VAR: &str = "RITORNEL_LOG_LEVEL";

/// The environment variable that sets the listen address.
pub(crate) const LISTEN_VAR: &str = "RITORNEL_LISTEN";

/// The environment variable that names where backups go.
pub(crate) const BACKUP_DIR_VAR: &str = "RITORNEL_BACKUP_DIR";

/// The port a server listens on when nobody has said and it is free.
pub(crate) const DEFAULT_PORT: u16 = 4747;

/// Where the server listens. It is always the loopback address 127.0.0.1:
/// listening beyond it arrives with pairing (docs/design/linking.md,
/// decision 8), and until then this type cannot name anything else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Listen(Option<u16>);

impl Listen {
    /// What a server does when nobody has said: the default port, or a
    /// port the system picks if that one is taken. A local client reads
    /// the port from `server.json` and never assumes it.
    pub const DEFAULT: Self = Self(None);

    /// This port and no other. Port 0 asks the system to pick one.
    pub const fn on_port(port: u16) -> Self {
        Self(Some(port))
    }

    /// The setting as text: `127.0.0.1` and a port.
    fn parse(text: &str) -> Option<Self> {
        match text.parse() {
            Ok(SocketAddr::V4(address)) if *address.ip() == Ipv4Addr::LOCALHOST => {
                Some(Self::on_port(address.port()))
            }
            _ => None,
        }
    }

    /// The address to try first.
    pub(crate) fn address(self) -> SocketAddr {
        (Ipv4Addr::LOCALHOST, self.0.unwrap_or(DEFAULT_PORT)).into()
    }

    /// The address to fall back to if the first is taken: none when
    /// someone chose the port.
    pub(crate) fn fallback(self) -> Option<SocketAddr> {
        self.0.is_none().then(|| (Ipv4Addr::LOCALHOST, 0).into())
    }
}

/// The settings a run uses, each already resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Settings {
    /// The least severe level that is logged. `info` unless set.
    pub(crate) log_level: Level,
    /// Where the server listens.
    pub(crate) listen: Listen,
    /// The folder backups are written to: an absolute path outside the
    /// data directory. With none there is no backup.
    pub(crate) backup_dir: Option<PathBuf>,
}

/// The settings given on the command line.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Flags {
    pub(crate) log_level: Option<String>,
    pub(crate) listen: Option<String>,
    pub(crate) backup_dir: Option<String>,
}

/// `config.toml`. A key it does not know is refused, so a misspelt setting
/// is noticed and not silently ignored.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    log_level: Option<String>,
    listen: Option<String>,
    backup_dir: Option<String>,
}

/// Where a setting's value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// The command line.
    Flag,
    /// A `RITORNEL_*` environment variable.
    Environment,
    /// `config.toml`.
    File,
}

impl Origin {
    /// What a message calls the place a setting was given, by the
    /// setting's three names.
    fn place(self, flag: &'static str, variable: &'static str, key: &'static str) -> String {
        match self {
            Self::Flag => flag.to_owned(),
            Self::Environment => variable.to_owned(),
            Self::File => format!("{key} in config.toml"),
        }
    }
}

/// A setting the server cannot use.
#[derive(Debug)]
pub enum ConfigError {
    /// `config.toml` is not TOML, or holds a key that is not a setting.
    File(toml::de::Error),
    /// The log level is not one of the five.
    LogLevel(Origin),
    /// The listen address is not 127.0.0.1 and a port.
    Listen(Origin),
    /// The backup folder is not an absolute path.
    BackupRelative(Origin),
    /// The backup folder is the data directory or inside it, where a
    /// backup would be lost with what it copies.
    BackupInside(Origin),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::File(error) => write!(f, "config.toml: {}", error.message()),
            Self::LogLevel(origin) => write!(
                f,
                "{} must be error, warn, info, debug or trace",
                origin.place("--log-level", LOG_LEVEL_VAR, "log_level")
            ),
            Self::Listen(origin) => write!(
                f,
                "{} must be 127.0.0.1 and a port, such as 127.0.0.1:{DEFAULT_PORT}: the server \
                 listens on loopback only",
                origin.place("--listen", LISTEN_VAR, "listen")
            ),
            Self::BackupRelative(origin) => write!(
                f,
                "{} must be an absolute path",
                origin.place("--backup-dir", BACKUP_DIR_VAR, "backup_dir")
            ),
            Self::BackupInside(origin) => write!(
                f,
                "{} must be outside the data directory: a backup beside what it copies is lost \
                 with it",
                origin.place("--backup-dir", BACKUP_DIR_VAR, "backup_dir")
            ),
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::File(error) => Some(error),
            Self::LogLevel(_)
            | Self::Listen(_)
            | Self::BackupRelative(_)
            | Self::BackupInside(_) => None,
        }
    }
}

/// Reads `config.toml` in the data directory `root`, if there is one, and
/// resolves every setting.
pub(crate) fn load(root: &Path, flags: &Flags, env: Env<'_>) -> Result<Settings, Error> {
    let path = root.join("config.toml");
    let file = match std::fs::read_to_string(&path) {
        Ok(text) => Some(text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(io_error("read", &path)(error)),
    };
    Ok(resolve(root, flags, env, file.as_deref())?)
}

/// Resolves every setting from the flags, the environment and the text of
/// `config.toml`. The file is checked whole even when nothing is taken from
/// it.
fn resolve(
    root: &Path,
    flags: &Flags,
    env: Env<'_>,
    file: Option<&str>,
) -> Result<Settings, ConfigError> {
    let file: File = file
        .map(toml::from_str)
        .transpose()
        .map_err(ConfigError::File)?
        .unwrap_or_default();

    let log_level = match chosen(
        flags.log_level.as_deref(),
        env(LOG_LEVEL_VAR),
        file.log_level,
    ) {
        Some((text, origin)) => level(text.as_deref(), origin)?,
        None => Level::INFO,
    };
    let listen = match chosen(flags.listen.as_deref(), env(LISTEN_VAR), file.listen) {
        Some((text, origin)) => text
            .as_deref()
            .and_then(Listen::parse)
            .ok_or(ConfigError::Listen(origin))?,
        None => Listen::DEFAULT,
    };
    let backup_dir = match chosen(
        flags.backup_dir.as_deref(),
        env(BACKUP_DIR_VAR),
        file.backup_dir,
    ) {
        Some((text, origin)) => Some(backup_dir(root, text, origin)?),
        None => None,
    };
    Ok(Settings {
        log_level,
        listen,
        backup_dir,
    })
}

/// The backup folder, which must be an absolute path and outside the data
/// directory `root`. Each is compared as the system resolves it where it
/// can be, so a link to the data directory is inside it too. The folder
/// need not be there yet: a disk that is not connected is found out when a
/// backup is taken, and said in the status.
fn backup_dir(root: &Path, text: Option<String>, origin: Origin) -> Result<PathBuf, ConfigError> {
    let path = PathBuf::from(text.ok_or(ConfigError::BackupRelative(origin))?);
    if !path.is_absolute() {
        return Err(ConfigError::BackupRelative(origin));
    }
    let resolved = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_owned());
    if resolved(&path).starts_with(resolved(root)) || path.starts_with(root) {
        return Err(ConfigError::BackupInside(origin));
    }
    Ok(path)
}

/// The value of a setting from the first place that gives one, and which
/// place that was. The text is `None` if the environment held something
/// that is not text. An empty variable counts as unset.
fn chosen(
    flag: Option<&str>,
    variable: Option<OsString>,
    file: Option<String>,
) -> Option<(Option<String>, Origin)> {
    let variable = variable.filter(|value| !value.is_empty());
    if let Some(flag) = flag {
        Some((Some(flag.to_owned()), Origin::Flag))
    } else if let Some(variable) = variable {
        Some((variable.into_string().ok(), Origin::Environment))
    } else {
        file.map(|setting| (Some(setting), Origin::File))
    }
}

fn level(text: Option<&str>, origin: Origin) -> Result<Level, ConfigError> {
    match text.map(str::to_ascii_lowercase).as_deref() {
        Some("error") => Ok(Level::ERROR),
        Some("warn") => Ok(Level::WARN),
        Some("info") => Ok(Level::INFO),
        Some("debug") => Ok(Level::DEBUG),
        Some("trace") => Ok(Level::TRACE),
        _ => Err(ConfigError::LogLevel(origin)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Scratch, env};

    const FILE: &str = "log_level = \"warn\"\n";

    /// A data directory the tests' settings are resolved for. Nothing is
    /// made there.
    const ROOT: &str = if cfg!(windows) {
        r"C:\ritornel-config-test\data"
    } else {
        "/ritornel-config-test/data"
    };

    fn flags(log_level: Option<&str>, listen: Option<&str>) -> Flags {
        Flags {
            log_level: log_level.map(str::to_owned),
            listen: listen.map(str::to_owned),
            backup_dir: None,
        }
    }

    fn log_level(flag: Option<&str>, variable: Option<&str>, file: Option<&str>) -> Level {
        let variables: Vec<_> = variable
            .map(|value| (LOG_LEVEL_VAR, value))
            .into_iter()
            .collect();
        resolve(Path::new(ROOT), &flags(flag, None), &env(&variables), file)
            .unwrap()
            .log_level
    }

    fn listen(
        flag: Option<&str>,
        variable: Option<&str>,
        file: Option<&str>,
    ) -> Result<Listen, String> {
        let variables: Vec<_> = variable
            .map(|value| (LISTEN_VAR, value))
            .into_iter()
            .collect();
        resolve(Path::new(ROOT), &flags(None, flag), &env(&variables), file)
            .map(|settings| settings.listen)
            .map_err(|error| error.to_string())
    }

    #[test]
    fn a_flag_beats_the_environment_which_beats_the_file_which_beats_the_default() {
        assert_eq!(
            log_level(Some("trace"), Some("debug"), Some(FILE)),
            Level::TRACE
        );
        assert_eq!(log_level(None, Some("debug"), Some(FILE)), Level::DEBUG);
        assert_eq!(log_level(None, None, Some(FILE)), Level::WARN);
        assert_eq!(log_level(None, None, None), Level::INFO);
        assert_eq!(log_level(None, None, Some("")), Level::INFO);
        // Each source also wins over everything below it when the ones
        // between are missing.
        assert_eq!(log_level(Some("trace"), None, Some(FILE)), Level::TRACE);
        assert_eq!(log_level(Some("trace"), None, None), Level::TRACE);
        assert_eq!(log_level(None, Some("debug"), None), Level::DEBUG);
    }

    #[test]
    fn an_empty_variable_counts_as_unset_and_case_does_not_matter() {
        assert_eq!(log_level(None, Some(""), Some(FILE)), Level::WARN);
        assert_eq!(log_level(None, Some("ERROR"), None), Level::ERROR);
    }

    #[test]
    fn a_bad_level_is_refused_and_the_message_says_where_it_was_set() {
        let message = |flag, variable: Option<&str>, file| {
            let variables: Vec<_> = variable
                .map(|value| (LOG_LEVEL_VAR, value))
                .into_iter()
                .collect();
            resolve(Path::new(ROOT), &flags(flag, None), &env(&variables), file)
                .unwrap_err()
                .to_string()
        };
        assert_eq!(
            message(Some("loud"), None, None),
            "--log-level must be error, warn, info, debug or trace"
        );
        assert_eq!(
            message(None, Some("loud"), None),
            "RITORNEL_LOG_LEVEL must be error, warn, info, debug or trace"
        );
        assert_eq!(
            message(None, None, Some("log_level = \"loud\"")),
            "log_level in config.toml must be error, warn, info, debug or trace"
        );
    }

    #[test]
    fn a_file_that_is_wrong_is_refused_even_when_a_flag_would_win() {
        for file in ["log_levle = \"warn\"", "log_level = 3", "not toml at all ["] {
            let refused = resolve(
                Path::new(ROOT),
                &flags(Some("info"), None),
                &env(&[]),
                Some(file),
            );
            assert!(
                matches!(refused, Err(ConfigError::File(_))),
                "{file}: {refused:?}"
            );
        }
        let message = resolve(
            Path::new(ROOT),
            &Flags::default(),
            &env(&[]),
            Some("cache_dir = \"/big/disk\""),
        )
        .unwrap_err()
        .to_string();
        assert!(
            message.starts_with("config.toml: unknown field `cache_dir`"),
            "{message}"
        );
    }

    #[test]
    fn the_listen_address_follows_the_same_order_and_defaults_to_the_fixed_port() {
        const FILE: &str = "listen = \"127.0.0.1:5001\"\n";
        let on = |port| Ok(Listen::on_port(port));
        assert_eq!(
            listen(Some("127.0.0.1:5003"), Some("127.0.0.1:5002"), Some(FILE)),
            on(5003)
        );
        assert_eq!(listen(None, Some("127.0.0.1:5002"), Some(FILE)), on(5002));
        assert_eq!(listen(None, None, Some(FILE)), on(5001));
        assert_eq!(listen(None, Some(""), Some(FILE)), on(5001));
        assert_eq!(listen(None, None, None), Ok(Listen::DEFAULT));
        // Port 0 asks the system for a port.
        assert_eq!(listen(Some("127.0.0.1:0"), None, None), on(0));
    }

    #[test]
    fn the_default_falls_back_to_a_port_the_system_picks_and_a_chosen_port_does_not() {
        assert_eq!(Listen::DEFAULT.address().to_string(), "127.0.0.1:4747");
        assert_eq!(
            Listen::DEFAULT.fallback().unwrap().to_string(),
            "127.0.0.1:0"
        );
        assert_eq!(
            Listen::on_port(4747).address().to_string(),
            "127.0.0.1:4747"
        );
        assert_eq!(Listen::on_port(4747).fallback(), None);
    }

    #[test]
    fn anything_but_loopback_is_refused_and_the_message_says_where_it_was_set() {
        for wrong in [
            "0.0.0.0:4747",
            "192.168.1.20:4747",
            "[::1]:4747",
            "[::]:4747",
            "127.0.0.2:4747",
            "localhost:4747",
            "127.0.0.1",
            "4747",
            "private",
            "",
        ] {
            assert_eq!(
                listen(Some(wrong), None, None).unwrap_err(),
                "--listen must be 127.0.0.1 and a port, such as 127.0.0.1:4747: the server \
                 listens on loopback only",
                "{wrong}"
            );
        }
        assert!(
            listen(None, Some("0.0.0.0:4747"), None)
                .unwrap_err()
                .starts_with("RITORNEL_LISTEN must be 127.0.0.1 and a port")
        );
        assert!(
            listen(None, None, Some("listen = \"0.0.0.0:4747\""))
                .unwrap_err()
                .starts_with("listen in config.toml must be 127.0.0.1 and a port")
        );
    }

    fn backup_dir(
        flag: Option<&str>,
        variable: Option<&str>,
        file: Option<&str>,
    ) -> Result<Option<PathBuf>, String> {
        let variables: Vec<_> = variable
            .map(|value| (BACKUP_DIR_VAR, value))
            .into_iter()
            .collect();
        let flags = Flags {
            backup_dir: flag.map(str::to_owned),
            ..Flags::default()
        };
        resolve(Path::new(ROOT), &flags, &env(&variables), file)
            .map(|settings| settings.backup_dir)
            .map_err(|error| error.to_string())
    }

    #[test]
    fn the_backup_folder_is_unset_until_named_and_never_in_the_data_directory() {
        let elsewhere = if cfg!(windows) {
            r"D:\Backups\Ritornel"
        } else {
            "/Volumes/Backups/Ritornel"
        };
        let file = format!("backup_dir = {:?}\n", elsewhere);
        assert_eq!(backup_dir(None, None, None), Ok(None));
        assert_eq!(
            backup_dir(None, None, Some(&file)),
            Ok(Some(PathBuf::from(elsewhere)))
        );
        // The same order as every other setting.
        let flag = Path::new(ROOT).with_file_name("by-flag");
        assert_eq!(
            backup_dir(flag.to_str(), Some(elsewhere), Some(&file)),
            Ok(Some(flag.clone()))
        );

        let inside = Path::new(ROOT).join("backups");
        for (given, message) in [
            (
                inside.to_str().unwrap(),
                "--backup-dir must be outside the data directory",
            ),
            (ROOT, "--backup-dir must be outside the data directory"),
            ("backups", "--backup-dir must be an absolute path"),
        ] {
            let refused = backup_dir(Some(given), None, None).unwrap_err();
            assert!(refused.starts_with(message), "{given}: {refused}");
        }
        assert!(
            backup_dir(None, Some("relative"), None)
                .unwrap_err()
                .starts_with("RITORNEL_BACKUP_DIR must be an absolute path")
        );
        let inside_file = format!("backup_dir = {:?}\n", inside.to_str().unwrap());
        assert!(
            backup_dir(None, None, Some(&inside_file))
                .unwrap_err()
                .starts_with("backup_dir in config.toml must be outside the data directory")
        );
    }

    // A link is made differently on each system, and on Windows only with
    // a privilege a test does not have.
    #[cfg(unix)]
    #[test]
    fn a_link_into_the_data_directory_is_inside_it() {
        let scratch = Scratch::new();
        let root = scratch.path().join("data");
        std::fs::create_dir_all(root.join("backups")).unwrap();
        let link = scratch.path().join("looks-elsewhere");
        std::os::unix::fs::symlink(root.join("backups"), &link).unwrap();
        let flags = Flags {
            backup_dir: link.to_str().map(str::to_owned),
            ..Flags::default()
        };
        assert!(matches!(
            resolve(&root, &flags, &env(&[]), None),
            Err(ConfigError::BackupInside(Origin::Flag))
        ));
    }

    #[test]
    fn the_file_is_read_from_the_data_directory_and_may_be_missing() {
        let scratch = Scratch::new();
        let (root, path) = (scratch.path(), scratch.path().join("config.toml"));
        let none = Flags::default();
        assert_eq!(
            load(root, &none, &env(&[])).unwrap(),
            Settings {
                log_level: Level::INFO,
                listen: Listen::DEFAULT,
                backup_dir: None,
            }
        );

        std::fs::write(&path, FILE).unwrap();
        assert_eq!(load(root, &none, &env(&[])).unwrap().log_level, Level::WARN);
        assert_eq!(
            load(root, &none, &env(&[(LOG_LEVEL_VAR, "debug")]))
                .unwrap()
                .log_level,
            Level::DEBUG
        );
    }
}
