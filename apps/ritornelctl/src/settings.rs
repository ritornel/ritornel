//! `ritornelctl settings`: the running server's settings, each with what
//! gave it, and changing those a call may change. See "Settings" in
//! docs/internals/server.md.

use std::io::Write;

use ritornel_client::contract::methods::{ServerSetSettings, ServerSettingsCall};
use ritornel_client::contract::{
    Awake, LogLevel, NoParams, ServerSettings, ServerSettingsSet, SetBy, SetServerSettingsParams,
};
use ritornel_client::local;

use crate::cli::{AwakeArgs, Error, SettingsArgs, SettingsCommand};
use crate::server;

pub(crate) fn settings(
    args: &SettingsArgs,
    env: local::Env<'_>,
    out: &mut dyn Write,
) -> Result<(), Error> {
    match &args.command {
        None => {
            let settings = server::ask(&args.server, env, async |client| {
                client.call::<ServerSettingsCall>(&NoParams {}).await
            })?;
            server::print(
                &args.server,
                out,
                &settings,
                serde_json::to_string_pretty,
                render,
            )
        }
        Some(SettingsCommand::Awake(args)) => {
            let awake = awake(args)?;
            let set = server::ask(&args.server, env, async |client| {
                client
                    .call::<ServerSetSettings>(&SetServerSettingsParams {
                        idempotency_key: ritornel_client::idempotency_key()?,
                        awake: Some(awake),
                    })
                    .await
            })?;
            server::print(
                &args.server,
                out,
                &set,
                serde_json::to_string_pretty,
                |set: &ServerSettingsSet| render(&set.settings),
            )
        }
    }
}

/// The wake setting `settings awake` was given.
fn awake(args: &AwakeArgs) -> Result<Awake, Error> {
    match (args.mode.as_str(), args.minutes.as_deref()) {
        ("while-active", None) => Ok(Awake::default()),
        ("while-active", Some(minutes)) => minutes
            .parse()
            .map(|minutes| Awake::WhileActive { minutes })
            .map_err(|_| Error::Usage("while-active takes a whole number of minutes")),
        ("always", None) => Ok(Awake::Always),
        ("never", None) => Ok(Awake::Never),
        _ => Err(Error::Usage(
            "awake is `while-active [<minutes>]`, `always` or `never`",
        )),
    }
}

/// The wake setting, as a person reads it.
pub(crate) fn describe_awake(awake: Awake) -> String {
    match awake {
        Awake::WhileActive { minutes } => {
            format!("while a linked server plays or called in the last {minutes} minutes")
        }
        Awake::Always => "always".to_owned(),
        Awake::Never => "never".to_owned(),
    }
}

/// A size in bytes, in the unit `--cache-size` takes where it fits one.
fn size(bytes: u64) -> String {
    const MIB: u64 = 1 << 20;
    const GIB: u64 = 1 << 30;
    if bytes.is_multiple_of(GIB) {
        format!("{} GiB", bytes / GIB)
    } else if bytes.is_multiple_of(MIB) {
        format!("{} MiB", bytes / MIB)
    } else {
        format!("{bytes} bytes")
    }
}

fn level(level: LogLevel) -> &'static str {
    match level {
        LogLevel::Error => "error",
        LogLevel::Warn => "warn",
        LogLevel::Info => "info",
        LogLevel::Debug => "debug",
        LogLevel::Trace => "trace",
    }
}

/// A value, and what gave it.
fn given(value: String, set_by: SetBy) -> String {
    let by = match set_by {
        SetBy::Default => "default",
        SetBy::File => "config.toml",
        SetBy::Environment => "environment",
        SetBy::Flag => "command line",
        SetBy::Call => "set by a call",
    };
    format!("{value} ({by})")
}

fn render(settings: &ServerSettings) -> String {
    server::facts(&[
        (
            "Name",
            given(settings.name.value.clone(), settings.name.set_by),
        ),
        (
            "Awake",
            given(describe_awake(settings.awake.value), settings.awake.set_by),
        ),
        (
            "Cache",
            given(
                size(settings.cache_bytes.value),
                settings.cache_bytes.set_by,
            ),
        ),
        (
            "Listens on",
            given(settings.listen.value.clone(), settings.listen.set_by),
        ),
        (
            "Log level",
            given(
                level(settings.log_level.value).to_owned(),
                settings.log_level.set_by,
            ),
        ),
        (
            "Backups",
            given(
                settings
                    .backup_dir
                    .value
                    .clone()
                    .unwrap_or_else(|| "none".to_owned()),
                settings.backup_dir.set_by,
            ),
        ),
    ])
}

#[cfg(test)]
mod tests {
    use ritornel_client::contract::Setting;

    use super::*;

    fn args(mode: &str, minutes: Option<&str>) -> AwakeArgs {
        AwakeArgs {
            mode: mode.to_owned(),
            minutes: minutes.map(str::to_owned),
            ..AwakeArgs::default()
        }
    }

    #[test]
    fn awake_is_while_active_always_or_never() {
        assert_eq!(
            awake(&args("while-active", None)).unwrap(),
            Awake::default()
        );
        assert_eq!(
            awake(&args("while-active", Some("45"))).unwrap(),
            Awake::WhileActive { minutes: 45 }
        );
        assert_eq!(awake(&args("always", None)).unwrap(), Awake::Always);
        assert_eq!(awake(&args("never", None)).unwrap(), Awake::Never);
        for wrong in [
            args("while-active", Some("soon")),
            args("always", Some("5")),
            args("sometimes", None),
        ] {
            assert!(matches!(awake(&wrong), Err(Error::Usage(_))), "{wrong:?}");
        }
    }

    #[test]
    fn each_setting_is_shown_with_what_gave_it() {
        let settings = ServerSettings {
            name: Setting {
                value: "Kitchen Speakers".to_owned(),
                set_by: SetBy::Environment,
            },
            awake: Setting {
                value: Awake::Never,
                set_by: SetBy::Call,
            },
            cache_bytes: Setting {
                value: 512 << 20,
                set_by: SetBy::File,
            },
            listen: Setting {
                value: "private:4747".to_owned(),
                set_by: SetBy::Flag,
            },
            log_level: Setting {
                value: LogLevel::Info,
                set_by: SetBy::Default,
            },
            backup_dir: Setting {
                value: None,
                set_by: SetBy::Default,
            },
        };
        assert_eq!(
            render(&settings),
            "\
Name              Kitchen Speakers (environment)
Awake             never (set by a call)
Cache             512 MiB (config.toml)
Listens on        private:4747 (command line)
Log level         info (default)
Backups           none (default)
"
        );
        assert_eq!(size(2 << 30), "2 GiB");
        assert_eq!(size(1000), "1000 bytes");
    }
}
