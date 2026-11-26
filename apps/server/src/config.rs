//! Settings. Each is read from its flag, then its `RITORNEL_*` environment
//! variable, then `config.toml` in the data directory, then its default. See
//! "Configuration and logging" in docs/internals/server.md.
//!
//! `config.toml` holds only what the server needs as it starts, and a
//! container needs no file at all. The settings so far are the log level,
//! the listen address, where backups go, which a migration needs before it
//! opens a database, the server's name, which a machine nobody sits at can
//! be given no other way, the size of a satellite's cache of masters, and
//! the player's volume curve.

use std::ffi::OsString;
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};

use ritornel_contract::MAX_CLIENT_TEXT;
use ritornel_engine::playback::VolumeCurve;
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

/// The environment variable that sets the server's name.
pub(crate) const NAME_VAR: &str = "RITORNEL_NAME";

/// The environment variable that sets the size of the cache of masters.
pub(crate) const CACHE_SIZE_VAR: &str = "RITORNEL_CACHE_SIZE";

/// The environment variable that sets the player's volume curve.
pub(crate) const VOLUME_CURVE_VAR: &str = "RITORNEL_VOLUME_CURVE";

/// The port a server listens on when nobody has said and it is free.
pub(crate) const DEFAULT_PORT: u16 = 4747;

/// Whether `ip` is in `100.64.0.0/10`, the shared address space a tailnet
/// uses.
pub(crate) fn is_tailnet(ip: std::net::Ipv4Addr) -> bool {
    let [first, second, ..] = ip.octets();
    first == 100 && (64..128).contains(&second)
}

/// Where the server listens: on loopback, the default, or on the private
/// network. Nothing else can be named, so the server never binds a public
/// address ("Listener and transport boundaries" in
/// docs/internals/linking.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Listen {
    port: Option<u16>,
    private: bool,
}

impl Listen {
    /// What a server does when nobody has said: loopback, on the default
    /// port, or a port the system picks if that one is taken. A local
    /// client reads the port from `server.json` and never assumes it.
    pub const DEFAULT: Self = Self {
        port: None,
        private: false,
    };

    /// Loopback, on this port and no other. Port 0 asks the system to pick
    /// one.
    pub const fn on_port(port: u16) -> Self {
        Self {
            port: Some(port),
            private: false,
        }
    }

    /// Every interface, on this port and no other, taking connections only
    /// from loopback and private addresses: the mode a hub runs in for its
    /// satellites to reach it. Port 0 asks the system to pick one.
    pub const fn private_on_port(port: u16) -> Self {
        Self {
            port: Some(port),
            private: true,
        }
    }

    /// The setting as text: `127.0.0.1` and a port, or `private` and a
    /// port.
    fn parse(text: &str) -> Option<Self> {
        if let Some(port) = text.strip_prefix("private:") {
            return port.parse().ok().map(Self::private_on_port);
        }
        match text.parse() {
            Ok(SocketAddr::V4(address)) if *address.ip() == Ipv4Addr::LOCALHOST => {
                Some(Self::on_port(address.port()))
            }
            _ => None,
        }
    }

    /// Whether this listens beyond loopback.
    pub(crate) fn is_private(self) -> bool {
        self.private
    }

    /// The address to try first.
    pub(crate) fn address(self) -> SocketAddr {
        let ip = if self.private {
            Ipv4Addr::UNSPECIFIED
        } else {
            Ipv4Addr::LOCALHOST
        };
        (ip, self.port.unwrap_or(DEFAULT_PORT)).into()
    }

    /// The address to fall back to if the first is taken: none when
    /// someone chose the port.
    pub(crate) fn fallback(self) -> Option<SocketAddr> {
        self.port.is_none().then(|| (Ipv4Addr::LOCALHOST, 0).into())
    }

    /// Whether a connection from `peer` is served. On loopback only
    /// loopback can connect at all. In private mode a peer must be on
    /// loopback, a private or link-local network, or a tailnet. This is no
    /// firewall: behind a proxy or a container's bridge every peer looks
    /// private.
    pub(crate) fn admits(self, peer: IpAddr) -> bool {
        let peer = match peer {
            IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(IpAddr::V6(v6), IpAddr::V4),
            v4 @ IpAddr::V4(_) => v4,
        };
        match peer {
            IpAddr::V4(v4) => {
                v4.is_loopback()
                    || (self.private && (v4.is_private() || v4.is_link_local() || is_tailnet(v4)))
            }
            IpAddr::V6(v6) => {
                let first = v6.segments()[0];
                // fc00::/7, unique local, and fe80::/10, link-local.
                let local = (first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfe80;
                v6.is_loopback() || (self.private && local)
            }
        }
    }
}

impl From<Listen> for ritornel_contract::ListenOn {
    /// The setting as `server.settings` reports it. The default is
    /// reported with the port it tries first.
    fn from(listen: Listen) -> Self {
        Self {
            network: if listen.private {
                ritornel_contract::ListenNetwork::Private
            } else {
                ritornel_contract::ListenNetwork::Loopback
            },
            port: listen.port.unwrap_or(DEFAULT_PORT),
        }
    }
}

/// Startup settings for generated audio and portable files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscodeConfig {
    pub workers: std::num::NonZeroUsize,
    pub budget: u64,
    pub profiles: std::collections::BTreeMap<String, ritornel_engine::transcode::Profile>,
}

impl Default for TranscodeConfig {
    fn default() -> Self {
        use ritornel_engine::transcode::{
            Mp3, Profile, Target, TranscodeCache, TranscodePool, Vbr,
        };
        let opus = |bitrate_kbps| Target::Opus {
            bitrate_kbps,
            complexity: 10,
            vbr: Vbr::Variable,
        };
        let profiles = [
            ("mp3-v0", Target::Mp3 { mode: Mp3::V0 }),
            ("mp3-v2", Target::Mp3 { mode: Mp3::V2 }),
            ("opus-160", opus(160)),
            ("opus-192", opus(192)),
            ("passthrough-flac", Target::Flac {}),
        ]
        .into_iter()
        .map(|(name, target)| {
            (
                name.to_owned(),
                Profile {
                    target,
                    dsp: Default::default(),
                },
            )
        })
        .collect();
        Self {
            workers: TranscodePool::default_workers(),
            budget: TranscodeCache::DEFAULT_BUDGET,
            profiles,
        }
    }
}

/// The settings a run uses, each already resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Settings {
    pub(crate) transcode: TranscodeConfig,
    /// The least severe level that is logged. `info` unless set.
    pub(crate) log_level: Level,
    /// Where the server listens.
    pub(crate) listen: Listen,
    /// The folder backups are written to: an absolute path outside the
    /// data directory. With none there is no backup.
    pub(crate) backup_dir: Option<PathBuf>,
    /// What the server is called, in place of the name it has stored,
    /// at every start. With none the stored name stands.
    pub(crate) name: Option<String>,
    /// The most a satellite's cache of masters pulled from its hub holds,
    /// in bytes. With none it is the default.
    pub(crate) cache_bytes: Option<u64>,
    /// How the player's `0..=1` volume maps to linear output amplitude.
    pub(crate) volume_curve: VolumeCurve,
    /// Where each was given.
    pub(crate) origins: Origins,
}

/// Where each setting of a run was given: none for one left at its
/// default. `server.settings` reports it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Origins {
    pub transcode_workers: Option<Origin>,
    pub transcode_cache_size: Option<Origin>,
    pub transcode_profiles: Option<Origin>,
    /// The log level.
    pub log_level: Option<Origin>,
    /// The listen address.
    pub listen: Option<Origin>,
    /// The backup folder.
    pub backup_dir: Option<Origin>,
    /// The name.
    pub name: Option<Origin>,
    /// The cache size.
    pub cache_size: Option<Origin>,
}

/// The settings given on the command line.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Flags {
    pub(crate) transcode_workers: Option<String>,
    pub(crate) transcode_cache_size: Option<String>,
    pub(crate) transcode_profiles: Option<String>,
    pub(crate) log_level: Option<String>,
    pub(crate) listen: Option<String>,
    pub(crate) backup_dir: Option<String>,
    pub(crate) name: Option<String>,
    pub(crate) cache_size: Option<String>,
    pub(crate) volume_curve: Option<String>,
}

/// `config.toml`. A key it does not know is refused, so a misspelt setting
/// is noticed and not silently ignored.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    transcode_workers: Option<u32>,
    transcode_cache_size: Option<String>,
    transcode_profiles:
        Option<std::collections::BTreeMap<String, ritornel_engine::transcode::Profile>>,
    log_level: Option<String>,
    listen: Option<String>,
    backup_dir: Option<String>,
    name: Option<String>,
    cache_size: Option<String>,
    volume_curve: Option<String>,
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
    Transcode(String),
    /// `config.toml` is not TOML, or holds a key that is not a setting.
    File(toml::de::Error),
    /// The log level is not one of the five.
    LogLevel(Origin),
    /// The listen address is not 127.0.0.1 and a port, or private and a
    /// port.
    Listen(Origin),
    /// The backup folder is not an absolute path.
    BackupRelative(Origin),
    /// The backup folder is the data directory or inside it, where a
    /// backup would be lost with what it copies.
    BackupInside(Origin),
    /// The name is empty, too long, or holds a control character.
    Name(Origin),
    /// The cache size is not a whole number of MiB or GiB above 0.
    CacheSize(Origin),
    /// The volume curve is not `cubic` or `log`.
    VolumeCurve(Origin),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transcode(message) => f.write_str(message),
            Self::File(error) => write!(f, "config.toml: {}", error.message()),
            Self::LogLevel(origin) => write!(
                f,
                "{} must be error, warn, info, debug or trace",
                origin.place("--log-level", LOG_LEVEL_VAR, "log_level")
            ),
            Self::Listen(origin) => write!(
                f,
                "{} must be 127.0.0.1 and a port, such as 127.0.0.1:{DEFAULT_PORT}, or private \
                 and a port, such as private:{DEFAULT_PORT}: the server listens on loopback or \
                 the private network only",
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
            Self::Name(origin) => write!(
                f,
                "{} must be 1 to {MAX_CLIENT_TEXT} characters with no control characters",
                origin.place("--name", NAME_VAR, "name")
            ),
            Self::CacheSize(origin) => write!(
                f,
                "{} must be a whole number of MiB or GiB above 0, such as 512 MiB or 8 GiB",
                origin.place("--cache-size", CACHE_SIZE_VAR, "cache_size")
            ),
            Self::VolumeCurve(origin) => write!(
                f,
                "{} must be cubic or log",
                origin.place("--volume-curve", VOLUME_CURVE_VAR, "volume_curve")
            ),
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::File(error) => Some(error),
            Self::Transcode(_)
            | Self::LogLevel(_)
            | Self::Listen(_)
            | Self::BackupRelative(_)
            | Self::BackupInside(_)
            | Self::Name(_)
            | Self::CacheSize(_)
            | Self::VolumeCurve(_) => None,
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

    let mut origins = Origins::default();
    let log_level = match chosen(
        flags.log_level.as_deref(),
        env(LOG_LEVEL_VAR),
        file.log_level,
    ) {
        Some((text, origin)) => {
            origins.log_level = Some(origin);
            level(text.as_deref(), origin)?
        }
        None => Level::INFO,
    };
    let listen = match chosen(flags.listen.as_deref(), env(LISTEN_VAR), file.listen) {
        Some((text, origin)) => {
            origins.listen = Some(origin);
            text.as_deref()
                .and_then(Listen::parse)
                .ok_or(ConfigError::Listen(origin))?
        }
        None => Listen::DEFAULT,
    };
    let backup_dir = match chosen(
        flags.backup_dir.as_deref(),
        env(BACKUP_DIR_VAR),
        file.backup_dir,
    ) {
        Some((text, origin)) => {
            origins.backup_dir = Some(origin);
            Some(backup_dir(root, text, origin)?)
        }
        None => None,
    };
    let name = match chosen(flags.name.as_deref(), env(NAME_VAR), file.name) {
        Some((text, origin)) => {
            origins.name = Some(origin);
            Some(name(text, origin)?)
        }
        None => None,
    };
    let cache_bytes = match chosen(
        flags.cache_size.as_deref(),
        env(CACHE_SIZE_VAR),
        file.cache_size,
    ) {
        Some((text, origin)) => {
            origins.cache_size = Some(origin);
            Some(
                text.as_deref()
                    .and_then(bytes)
                    .ok_or(ConfigError::CacheSize(origin))?,
            )
        }
        None => None,
    };
    let volume_curve = match chosen(
        flags.volume_curve.as_deref(),
        env(VOLUME_CURVE_VAR),
        file.volume_curve,
    ) {
        Some((text, origin)) => volume_curve(text.as_deref(), origin)?,
        None => VolumeCurve::default(),
    };
    let mut transcode = TranscodeConfig::default();
    if let Some((text, origin)) = chosen(
        flags.transcode_cache_size.as_deref(),
        env("RITORNEL_TRANSCODE_CACHE_SIZE"),
        file.transcode_cache_size,
    ) {
        transcode.budget = text.as_deref().and_then(bytes).ok_or_else(|| {
            ConfigError::Transcode(format!(
                "{}: cache size must be a positive whole number of MiB or GiB",
                origin.place(
                    "--transcode-cache-size",
                    "RITORNEL_TRANSCODE_CACHE_SIZE",
                    "transcode_cache_size"
                )
            ))
        })?;
        origins.transcode_cache_size = Some(origin);
    }
    if let Some((text, origin)) = chosen(
        flags.transcode_workers.as_deref(),
        env("RITORNEL_TRANSCODE_WORKERS"),
        file.transcode_workers.map(|n| n.to_string()),
    ) {
        transcode.workers = text
            .and_then(|s| s.parse::<std::num::NonZeroUsize>().ok())
            .filter(|n| n.get() <= 1024)
            .ok_or_else(|| {
                ConfigError::Transcode(format!(
                    "{}: worker count must be 1 to 1024",
                    origin.place(
                        "--transcode-workers",
                        "RITORNEL_TRANSCODE_WORKERS",
                        "transcode_workers"
                    )
                ))
            })?;
        origins.transcode_workers = Some(origin);
    }
    let profiles = if let Some((text, origin)) = chosen(
        flags.transcode_profiles.as_deref(),
        env("RITORNEL_TRANSCODE_PROFILES"),
        None,
    ) {
        origins.transcode_profiles = Some(origin);
        Some(
            serde_json::from_str(&text.unwrap_or_default()).map_err(|e| {
                ConfigError::Transcode(format!(
                    "{}: invalid profiles JSON: {e}",
                    origin.place(
                        "--transcode-profiles",
                        "RITORNEL_TRANSCODE_PROFILES",
                        "transcode_profiles"
                    )
                ))
            })?,
        )
    } else {
        origins.transcode_profiles = file.transcode_profiles.as_ref().map(|_| Origin::File);
        file.transcode_profiles
    };
    if let (Some(profiles), Some(origin)) = (profiles, origins.transcode_profiles) {
        let source = origin.place(
            "--transcode-profiles",
            "RITORNEL_TRANSCODE_PROFILES",
            "transcode_profiles",
        );
        for (name, profile) in profiles {
            if name.is_empty()
                || name.len() > 64
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
            {
                return Err(ConfigError::Transcode(format!(
                    "{source}: profile names must contain 1 to 64 letters, digits, hyphens or underscores"
                )));
            }
            profile.validate().map_err(|e| {
                ConfigError::Transcode(format!("{source}: transcode profile {name}: {e}"))
            })?;
            transcode.profiles.insert(name, profile);
        }
    }
    Ok(Settings {
        transcode,
        log_level,
        listen,
        backup_dir,
        name,
        cache_bytes,
        volume_curve,
        origins,
    })
}

/// A size such as `512 MiB` or `8GiB`, in bytes: a whole number above 0 and
/// a unit, either case, with or without a space between.
fn bytes(text: &str) -> Option<u64> {
    let text = text.trim();
    let digits = text.find(|c: char| !c.is_ascii_digit())?;
    let (number, unit) = text.split_at(digits);
    let shift = match unit.trim().to_ascii_lowercase().as_str() {
        "mib" => 20,
        "gib" => 30,
        _ => return None,
    };
    let number: u64 = number.parse().ok().filter(|number| *number > 0)?;
    // Every number of the contract fits in 53 bits, and the size is one.
    number
        .checked_mul(1 << shift)
        .filter(|bytes| *bytes < 1 << 53)
}

/// The server's name, without the spaces around it: some text, no longer
/// than a client's name may be, with no control character in it.
fn name(text: Option<String>, origin: Origin) -> Result<String, ConfigError> {
    let text = text.ok_or(ConfigError::Name(origin))?;
    let name = text.trim();
    if name.is_empty()
        || name.chars().count() > MAX_CLIENT_TEXT
        || name.chars().any(char::is_control)
    {
        return Err(ConfigError::Name(origin));
    }
    Ok(name.to_owned())
}

/// The backup folder, which must be an absolute path and outside the data
/// directory `root`, as the system resolves each
/// ([`crate::backup::is_inside`]), so a link to the data directory is
/// inside it too. The folder need not be there yet: a disk that is not
/// connected is found out when a backup is taken, and said in the status,
/// and the folder is asked again then.
fn backup_dir(root: &Path, text: Option<String>, origin: Origin) -> Result<PathBuf, ConfigError> {
    let path = PathBuf::from(text.ok_or(ConfigError::BackupRelative(origin))?);
    if !path.is_absolute() {
        return Err(ConfigError::BackupRelative(origin));
    }
    if crate::backup::is_inside(&path, root) {
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

fn volume_curve(text: Option<&str>, origin: Origin) -> Result<VolumeCurve, ConfigError> {
    match text.map(str::to_ascii_lowercase).as_deref() {
        Some("cubic") => Ok(VolumeCurve::Cubic),
        Some("log") => Ok(VolumeCurve::Log),
        _ => Err(ConfigError::VolumeCurve(origin)),
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
            ..Flags::default()
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
            "private:",
            "private:http",
            "public:4747",
            "",
        ] {
            assert_eq!(
                listen(Some(wrong), None, None).unwrap_err(),
                "--listen must be 127.0.0.1 and a port, such as 127.0.0.1:4747, or private and \
                 a port, such as private:4747: the server listens on loopback or the private \
                 network only",
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

    #[test]
    fn private_listens_on_every_interface_and_admits_only_private_peers() {
        let private = listen(Some("private:4848"), None, None).unwrap();
        assert_eq!(private, Listen::private_on_port(4848));
        assert_eq!(private.address().to_string(), "0.0.0.0:4848");
        assert_eq!(private.fallback(), None);
        assert!(private.is_private() && !Listen::DEFAULT.is_private());
        for peer in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.9",
            "192.168.1.20",
            "169.254.3.4",
            "100.101.7.3",
            "::1",
            "fd7a:115c:a1e0::1",
            "fe80::1",
            "::ffff:192.168.1.20",
        ] {
            assert!(private.admits(peer.parse().unwrap()), "{peer}");
        }
        for peer in [
            "8.8.8.8",
            "100.128.0.1",
            "172.32.0.1",
            "2001:db8::1",
            "::ffff:8.8.8.8",
        ] {
            assert!(!private.admits(peer.parse().unwrap()), "{peer}");
        }
        // On loopback, only loopback.
        assert!(Listen::DEFAULT.admits("127.0.0.1".parse().unwrap()));
        assert!(!Listen::DEFAULT.admits("192.168.1.20".parse().unwrap()));
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
        // A `..` that leads out of it is outside.
        let out = Path::new(ROOT).join("..").join("outside");
        assert_eq!(backup_dir(out.to_str(), None, None), Ok(Some(out.clone())));
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
            (
                &format!("{ROOT}/elsewhere/../backups"),
                "--backup-dir must be outside the data directory",
            ),
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

    fn name(
        flag: Option<&str>,
        variable: Option<&str>,
        file: Option<&str>,
    ) -> Result<Option<String>, String> {
        let variables: Vec<_> = variable
            .map(|value| (NAME_VAR, value))
            .into_iter()
            .collect();
        let flags = Flags {
            name: flag.map(str::to_owned),
            ..Flags::default()
        };
        resolve(Path::new(ROOT), &flags, &env(&variables), file)
            .map(|settings| settings.name)
            .map_err(|error| error.to_string())
    }

    fn cache_bytes(
        flag: Option<&str>,
        variable: Option<&str>,
        file: Option<&str>,
    ) -> Result<Option<u64>, String> {
        let variables: Vec<_> = variable
            .map(|value| (CACHE_SIZE_VAR, value))
            .into_iter()
            .collect();
        let flags = Flags {
            cache_size: flag.map(str::to_owned),
            ..Flags::default()
        };
        resolve(Path::new(ROOT), &flags, &env(&variables), file)
            .map(|settings| settings.cache_bytes)
            .map_err(|error| error.to_string())
    }

    #[test]
    fn the_cache_size_follows_the_same_order_in_mib_or_gib() {
        const MIB: u64 = 1 << 20;
        const GIB: u64 = 1 << 30;
        const FILE: &str = "cache_size = \"8 GiB\"\n";
        assert_eq!(cache_bytes(None, None, None), Ok(None));
        assert_eq!(cache_bytes(None, None, Some(FILE)), Ok(Some(8 * GIB)));
        assert_eq!(
            cache_bytes(None, Some("512 MiB"), Some(FILE)),
            Ok(Some(512 * MIB))
        );
        assert_eq!(
            cache_bytes(Some("1gib"), Some("512 MiB"), Some(FILE)),
            Ok(Some(GIB))
        );
        assert_eq!(cache_bytes(Some(" 64MiB "), None, None), Ok(Some(64 * MIB)));
    }

    #[test]
    fn a_cache_size_that_is_not_a_whole_number_of_mib_or_gib_above_0_is_refused() {
        for wrong in [
            "0 GiB",
            "8",
            "8 GB",
            "8 TiB",
            "1.5 GiB",
            "-1 GiB",
            "GiB",
            "",
            "99999999999 GiB",
        ] {
            assert_eq!(
                cache_bytes(Some(wrong), None, None),
                Err(
                    "--cache-size must be a whole number of MiB or GiB above 0, such as 512 \
                     MiB or 8 GiB"
                        .to_owned()
                ),
                "{wrong:?}"
            );
        }
        assert!(
            cache_bytes(None, Some("lots"), None)
                .unwrap_err()
                .starts_with("RITORNEL_CACHE_SIZE must be")
        );
        assert!(
            cache_bytes(None, None, Some("cache_size = 8"))
                .unwrap_err()
                .starts_with("config.toml:")
        );
    }

    #[test]
    fn the_name_follows_the_same_order_and_is_unset_until_given() {
        const FILE: &str = "name = \"Workbench Speakers\"\n";
        let named = |name: &str| Ok(Some(name.to_owned()));
        assert_eq!(name(None, None, None), Ok(None));
        assert_eq!(name(None, None, Some(FILE)), named("Workbench Speakers"));
        assert_eq!(name(None, Some("Porch"), Some(FILE)), named("Porch"));
        assert_eq!(
            name(Some("Attic"), Some("Porch"), Some(FILE)),
            named("Attic")
        );
        assert_eq!(
            name(None, Some(""), Some(FILE)),
            named("Workbench Speakers")
        );
        // The spaces around it are not part of it.
        assert_eq!(name(Some("  Attic \t"), None, None), named("Attic"));
        let longest = "x".repeat(MAX_CLIENT_TEXT);
        assert_eq!(name(Some(&longest), None, None), named(&longest));
    }

    #[test]
    fn a_name_that_is_blank_too_long_or_holds_a_control_character_is_refused() {
        let too_long = "x".repeat(MAX_CLIENT_TEXT + 1);
        for wrong in ["   ", "Two\nLines", "Bell\u{7}", too_long.as_str()] {
            assert_eq!(
                name(Some(wrong), None, None),
                Err(format!(
                    "--name must be 1 to {MAX_CLIENT_TEXT} characters with no control characters"
                )),
                "{wrong:?}"
            );
        }
        assert!(
            name(None, Some("\t"), None)
                .unwrap_err()
                .starts_with("RITORNEL_NAME must be")
        );
        assert!(
            name(None, None, Some("name = \"\""))
                .unwrap_err()
                .starts_with("name in config.toml must be")
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
        // The link, and a folder not made yet beneath a link to the data
        // directory itself.
        let to_data = scratch.path().join("alias");
        std::os::unix::fs::symlink(&root, &to_data).unwrap();
        for given in [link, to_data.join("later")] {
            let flags = Flags {
                backup_dir: given.to_str().map(str::to_owned),
                ..Flags::default()
            };
            assert!(
                matches!(
                    resolve(&root, &flags, &env(&[]), None),
                    Err(ConfigError::BackupInside(Origin::Flag))
                ),
                "{given:?}"
            );
        }
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
                name: None,
                transcode: Default::default(),
                cache_bytes: None,
                volume_curve: VolumeCurve::Cubic,
                origins: Origins::default(),
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

    #[test]
    fn the_listen_setting_is_reported_with_the_port_it_takes_first() {
        use ritornel_contract::{ListenNetwork, ListenOn};
        let on = |network, port| ListenOn { network, port };
        assert_eq!(
            ListenOn::from(Listen::DEFAULT),
            on(ListenNetwork::Loopback, DEFAULT_PORT)
        );
        assert_eq!(
            ListenOn::from(Listen::on_port(0)),
            on(ListenNetwork::Loopback, 0)
        );
        assert_eq!(
            ListenOn::from(Listen::private_on_port(4800)),
            on(ListenNetwork::Private, 4800)
        );
    }

    #[test]
    fn transcode_configuration_reports_the_winning_source_for_each_setting() {
        let file = r#"
            transcode_workers = 2
            transcode_cache_size = "3 MiB"
            [transcode_profiles.travel.target]
            codec = "mp3"
            mode = "v2"
        "#;
        let defaults = Flags::default();
        let vars = [
            ("RITORNEL_TRANSCODE_WORKERS", "3"),
            ("RITORNEL_TRANSCODE_CACHE_SIZE", "4 MiB"),
            (
                "RITORNEL_TRANSCODE_PROFILES",
                r#"{"travel":{"target":{"codec":"mp3","mode":"v0"}}}"#,
            ),
        ];
        let flags = Flags {
            transcode_workers: Some("4".into()),
            transcode_cache_size: Some("5 MiB".into()),
            transcode_profiles: Some(r#"{"travel":{"target":{"codec":"flac"}}}"#.into()),
            ..Flags::default()
        };
        use ritornel_engine::transcode::{Mp3, Target};
        for (flags, vars, origin, workers, mib, target) in [
            (
                &defaults,
                &[][..],
                Origin::File,
                2,
                3,
                Target::Mp3 { mode: Mp3::V2 },
            ),
            (
                &defaults,
                vars.as_slice(),
                Origin::Environment,
                3,
                4,
                Target::Mp3 { mode: Mp3::V0 },
            ),
            (&flags, vars.as_slice(), Origin::Flag, 4, 5, Target::Flac {}),
        ] {
            let s = resolve(Path::new(ROOT), flags, &env(vars), Some(file)).unwrap();
            assert_eq!(s.transcode.workers.get(), workers);
            assert_eq!(s.transcode.budget, mib << 20);
            assert_eq!(s.transcode.profiles["travel"].target, target);
            assert!(s.transcode.profiles.contains_key("opus-160"));
            assert_eq!(s.origins.transcode_workers, Some(origin));
            assert_eq!(s.origins.transcode_cache_size, Some(origin));
            assert_eq!(s.origins.transcode_profiles, Some(origin));
        }
        let s = resolve(Path::new(ROOT), &defaults, &env(&[]), None).unwrap();
        assert_eq!(s.transcode.budget, 10 << 30);
        assert_eq!(s.transcode, TranscodeConfig::default());
    }

    #[test]
    fn invalid_transcode_settings_fail_before_listening() {
        for (var, value, expected) in [
            ("RITORNEL_TRANSCODE_CACHE_SIZE", "0 MiB", "cache size"),
            ("RITORNEL_TRANSCODE_WORKERS", "0", "worker"),
            ("RITORNEL_TRANSCODE_PROFILES", "[]", "profiles JSON"),
            (
                "RITORNEL_TRANSCODE_PROFILES",
                r#"{"bad name":{"target":{"codec":"flac"}}}"#,
                "profile name",
            ),
            (
                "RITORNEL_TRANSCODE_PROFILES",
                r#"{"bad":{"target":{"codec":"mp3","mode":{"cbr":3}}}}"#,
                "bad: unsupported MP3",
            ),
            (
                "RITORNEL_TRANSCODE_PROFILES",
                r#"{"bad":{"target":{"codec":"flac","typo":3}}}"#,
                "unknown field",
            ),
        ] {
            let error = resolve(
                Path::new(ROOT),
                &Flags::default(),
                &env(&[(var, value)]),
                None,
            )
            .unwrap_err()
            .to_string();
            assert!(error.contains(expected), "{error}");
        }
    }

    #[test]
    fn invalid_profile_names_the_configuration_file_and_profile() {
        let file = "[transcode_profiles.pocket.target]\ncodec='mp3'\nmode={cbr=7}";
        let error = resolve(Path::new(ROOT), &Flags::default(), &env(&[]), Some(file))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("config.toml") && error.contains("pocket"),
            "{error}"
        );
    }

    #[test]
    fn a_cache_size_fits_in_53_bits() {
        assert_eq!(bytes("8388607 GiB"), Some(8_388_607 << 30));
        assert_eq!(bytes("8388608 GiB"), None);
    }

    #[test]
    fn each_setting_keeps_where_it_was_given() {
        let flags = Flags {
            name: Some("Kitchen Speakers".to_owned()),
            ..Flags::default()
        };
        let settings = resolve(
            Path::new(ROOT),
            &flags,
            &env(&[(LISTEN_VAR, "private:4747")]),
            Some("log_level = \"warn\"\nname = \"Den\"\n"),
        )
        .unwrap();
        assert_eq!(
            settings.origins,
            Origins {
                log_level: Some(Origin::File),
                listen: Some(Origin::Environment),
                backup_dir: None,
                name: Some(Origin::Flag),
                cache_size: None,
                ..Origins::default()
            }
        );
    }

    fn resolved_volume_curve(
        flag: Option<&str>,
        variable: Option<&str>,
        file: Option<&str>,
    ) -> Result<VolumeCurve, String> {
        let variables: Vec<_> = variable
            .map(|value| (VOLUME_CURVE_VAR, value))
            .into_iter()
            .collect();
        let flags = Flags {
            volume_curve: flag.map(str::to_owned),
            ..Flags::default()
        };
        resolve(Path::new(ROOT), &flags, &env(&variables), file)
            .map(|settings| settings.volume_curve)
            .map_err(|error| error.to_string())
    }

    #[test]
    fn the_volume_curve_defaults_to_cubic_and_follows_the_same_order() {
        const FILE: &str = "volume_curve = \"log\"\n";
        assert_eq!(
            resolved_volume_curve(None, None, None),
            Ok(VolumeCurve::Cubic)
        );
        assert_eq!(
            resolved_volume_curve(None, None, Some(FILE)),
            Ok(VolumeCurve::Log)
        );
        assert_eq!(
            resolved_volume_curve(None, Some("cubic"), Some(FILE)),
            Ok(VolumeCurve::Cubic)
        );
        assert_eq!(
            resolved_volume_curve(Some("LOG"), Some("cubic"), Some(FILE)),
            Ok(VolumeCurve::Log)
        );
        assert_eq!(
            resolved_volume_curve(None, Some(""), Some(FILE)),
            Ok(VolumeCurve::Log)
        );
    }

    #[test]
    fn a_volume_curve_other_than_cubic_or_log_is_refused() {
        assert_eq!(
            resolved_volume_curve(Some("linear"), None, None),
            Err("--volume-curve must be cubic or log".to_owned())
        );
        assert_eq!(
            resolved_volume_curve(None, Some("linear"), None),
            Err("RITORNEL_VOLUME_CURVE must be cubic or log".to_owned())
        );
        assert_eq!(
            resolved_volume_curve(None, None, Some("volume_curve = \"linear\"\n")),
            Err("volume_curve in config.toml must be cubic or log".to_owned())
        );
        assert!(
            resolved_volume_curve(None, None, Some("volume_curve = 3\n"))
                .unwrap_err()
                .starts_with("config.toml:")
        );
    }
}
