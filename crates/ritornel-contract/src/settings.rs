//! A server's settings: what `server.settings` reports, each value with
//! what gave it, and what `server.setSettings` changes. See "Settings" in
//! docs/internals/server.md.

use serde::{Deserialize, Serialize};

use crate::{Awake, Id};

/// What gave a setting its value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum SetBy {
    /// Nothing did: it is the default.
    Default,
    /// `config.toml` in the data directory.
    File,
    /// A `RITORNEL_*` environment variable.
    Environment,
    /// The server's command line.
    Flag,
    /// A call, `server.setSettings`, and the server keeps it.
    Call,
}

/// A setting's value, and what gave it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct Setting<T> {
    /// The value the server runs with.
    pub value: T,
    /// What gave it.
    pub set_by: SetBy,
}

/// The least severe level the server logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    /// Errors only.
    Error,
    /// Warnings and errors.
    Warn,
    /// What the server does, as well: the default.
    Info,
    /// More, for finding a fault.
    Debug,
    /// Everything.
    Trace,
}

/// The result of `server.settings`: the settings of the server called,
/// a satellite's own on a satellite.
///
/// Only `awake` is changed by a call. The rest are given as the server
/// starts, by a flag, a `RITORNEL_*` variable or `config.toml`, and hold
/// until it starts again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ServerSettings {
    /// What the server is called. With no name given as it starts, it is
    /// the default: the name it already had, which a new server takes
    /// from its computer.
    pub name: Setting<String>,
    /// When it keeps its machine from sleeping for its linked servers.
    pub awake: Setting<Awake>,
    /// The most a satellite's cache of masters pulled from its hub holds,
    /// in bytes. 2 GiB by default.
    pub cache_bytes: Setting<u64>,
    /// Where it listens: `127.0.0.1:<port>`, or `private:<port>` for the
    /// private network. By default `127.0.0.1:4747`, or a port the system
    /// picks when that one is taken.
    pub listen: Setting<String>,
    /// The least severe level it logs.
    pub log_level: Setting<LogLevel>,
    /// The folder backups are written to, outside the data directory.
    /// With none there is no backup.
    pub backup_dir: Setting<Option<String>>,
}

/// The params of `server.setSettings`: the settings to change, each left
/// out to keep it as it is. At least one is given.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SetServerSettingsParams {
    /// The command's ID, made by the client.
    pub idempotency_key: Id,
    /// When to keep the machine awake from now on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub awake: Option<Awake>,
}

/// The result of `server.setSettings`: the receipt, and the settings as
/// `server.settings` now reports them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ServerSettingsSet {
    /// The receipt.
    #[serde(flatten)]
    pub receipt: crate::Receipt,
    /// The settings, as `server.settings` now reports them.
    pub settings: ServerSettings,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_setting_is_its_value_and_what_gave_it() {
        let setting = Setting {
            value: Awake::Never,
            set_by: SetBy::Call,
        };
        assert_eq!(
            serde_json::to_value(setting).unwrap(),
            serde_json::json!({ "value": { "mode": "never" }, "setBy": "call" })
        );
        let level = Setting {
            value: LogLevel::Warn,
            set_by: SetBy::Environment,
        };
        assert_eq!(
            serde_json::to_value(level).unwrap(),
            serde_json::json!({ "value": "warn", "setBy": "environment" })
        );
    }

    #[test]
    fn a_change_leaves_out_what_it_keeps() {
        let params: SetServerSettingsParams = serde_json::from_value(serde_json::json!({
            "idempotencyKey": "0190d5a4-1c2b-7000-8000-000000000001"
        }))
        .unwrap();
        assert_eq!(params.awake, None);
        assert_eq!(
            serde_json::to_value(params).unwrap(),
            serde_json::json!({ "idempotencyKey": "0190d5a4-1c2b-7000-8000-000000000001" })
        );
    }
}
