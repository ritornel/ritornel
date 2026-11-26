//! `server.settings` and `server.setSettings`: this server's settings,
//! each with what gave it. See "Settings" in docs/internals/server.md.

use ritornel_contract::methods::{Method, ServerSetSettings};
use ritornel_contract::{
    Awake, ErrorCode, LogLevel, Receipt, ReceiptStatus, RpcError, ServerSettings,
    ServerSettingsSet, SetBy, SetServerSettingsParams, Setting,
};
use ritornel_engine::{Command, SettingChange};
use tracing::Level;

use super::{Core, Stores, lock};
use crate::config::{Listen, Origin, Origins};
use crate::hub_catalog::DEFAULT_CACHE_BYTES;

/// What a run was given as it started that the server reports and keeps
/// nowhere else.
#[derive(Debug, Clone, Copy)]
pub(crate) struct StartUp {
    pub(crate) listen: Listen,
    pub(crate) log_level: Level,
    pub(crate) origins: Origins,
}

/// What gave a setting given as the server started: the default when
/// nothing did.
pub(super) fn set_by(origin: Option<Origin>) -> SetBy {
    match origin {
        None => SetBy::Default,
        Some(Origin::Flag) => SetBy::Flag,
        Some(Origin::Environment) => SetBy::Environment,
        Some(Origin::File) => SetBy::File,
    }
}

fn log_level(level: Level) -> LogLevel {
    match level {
        Level::ERROR => LogLevel::Error,
        Level::WARN => LogLevel::Warn,
        Level::DEBUG => LogLevel::Debug,
        Level::TRACE => LogLevel::Trace,
        _ => LogLevel::Info,
    }
}

impl Core {
    /// Says what this run was given as it started.
    pub(crate) fn set_start_up(&self, start_up: StartUp) {
        let _ = self.start_up.set(start_up);
    }

    /// `server.settings`: this server's own, a satellite's included.
    /// `backup_dir.value` is omitted when `show_paths` is false.
    pub(super) fn server_settings(
        &self,
        stores: &Stores,
        show_paths: bool,
    ) -> Result<ServerSettings, RpcError> {
        let start_up = self.start_up.get().copied().unwrap_or(StartUp {
            listen: Listen::DEFAULT,
            log_level: Level::INFO,
            origins: Origins::default(),
        });
        let origins = start_up.origins;
        let awake = {
            let awake = lock(&self.awake);
            Setting {
                value: awake.setting,
                set_by: if awake.set_by_call {
                    SetBy::Call
                } else {
                    SetBy::Default
                },
            }
        };
        Ok(ServerSettings {
            transcode: self.transcode_status(stores)?,
            name: Setting {
                value: self.name(),
                set_by: set_by(origins.name),
            },
            awake,
            cache_bytes: Setting {
                value: self
                    .cache_bytes
                    .get()
                    .copied()
                    .unwrap_or(DEFAULT_CACHE_BYTES),
                set_by: set_by(origins.cache_size),
            },
            listen: Setting {
                value: start_up.listen.into(),
                set_by: set_by(origins.listen),
            },
            log_level: Setting {
                value: log_level(start_up.log_level),
                set_by: set_by(origins.log_level),
            },
            backup_dir: Setting {
                value: if show_paths {
                    self.backups()
                        .and_then(crate::backup::Backups::place)
                        .map(|place| place.to_string_lossy().into_owned())
                } else {
                    None
                },
                set_by: set_by(origins.backup_dir),
            },
        })
    }

    /// `server.setSettings`: the change and its receipt are kept in
    /// `server.db` together, and the server follows it at once.
    pub(super) fn server_set_settings(
        &self,
        stores: &Stores,
        params: &SetServerSettingsParams,
        show_paths: bool,
    ) -> Result<ServerSettingsSet, RpcError> {
        let Some(awake) = params.awake else {
            return Err(RpcError::new(
                ErrorCode::Invalid,
                "name a setting to change: awake",
            ));
        };
        if !awake.is_valid() {
            return Err(RpcError::new(
                ErrorCode::Invalid,
                format!("minutes are at most {}, a week", Awake::MAX_MINUTES),
            ));
        }
        let command = Command {
            id: params.idempotency_key,
            fingerprint: super::fingerprint::<ServerSetSettings>(params)?,
        };
        let changed = stores
            .server()
            .change_once(
                command,
                ServerSetSettings::NAME,
                SettingChange::Awake(awake),
            )
            .map_err(crate::Error::Store)
            .map_err(super::failed)?;
        if changed.now {
            self.follow_stored_awake(stores);
        }
        Ok(ServerSettingsSet {
            receipt: Receipt {
                command_id: params.idempotency_key,
                status: ReceiptStatus::Done,
                revision: None,
            },
            settings: self.server_settings(stores, show_paths)?,
        })
    }
}
