//! The session calls and what they return: the handshake and the
//! descriptor, the status, and the list of servers. See "Session and
//! frames" in docs/design/contract.md.
//!
//! A field that describes something a server does not have yet is left
//! out: there are no sort keys, predicates or library revision in the
//! descriptor until there is a library to describe.

use serde::{Deserialize, Serialize};

use crate::Id;

/// A server's own record of its role. The library's hub pointer decides
/// which server is the hub (docs/design/linking.md, decision 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum ServerRole {
    /// It has no library and is linked to none.
    Unassigned,
    /// It holds the library.
    Hub,
    /// It is linked to a hub.
    Satellite,
}

impl ServerRole {
    /// The role as it is written on the wire.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unassigned => "unassigned",
            Self::Hub => "hub",
            Self::Satellite => "satellite",
        }
    }
}

/// What a session may do (docs/design/linking.md, decision 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum SessionKind {
    /// Everything.
    Full,
    /// Read the library and control playback. It may change or delete
    /// nothing.
    Limited,
}

/// What sort of program a client is. It is for logs and for display, and
/// never decides access: the session does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum ClientKind {
    /// The desktop app.
    Desktop,
    /// A page in a browser.
    Browser,
    /// A phone app.
    Phone,
    /// A command-line program, such as `ritornelctl`.
    Cli,
    /// The agent adapter.
    Agent,
}

impl ClientKind {
    /// The kind as it is written on the wire.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Desktop => "desktop",
            Self::Browser => "browser",
            Self::Phone => "phone",
            Self::Cli => "cli",
            Self::Agent => "agent",
        }
    }
}

/// The params of `server.hello`: who is calling.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct HelloParams {
    /// The program's name, such as `ritornelctl`. At most
    /// [`MAX_CLIENT_TEXT`] characters, and not empty.
    pub name: String,
    /// What sort of program it is.
    pub kind: ClientKind,
    /// The program's own version. At most [`MAX_CLIENT_TEXT`] characters,
    /// and not empty.
    pub version: String,
    /// The [contract version](crate::CONTRACT_VERSION) the program was
    /// built with. A server refuses one that is not its own.
    pub contract_version: u32,
}

/// The longest a client's name or version may be, in characters. Both are
/// text from another program, and both are logged.
pub const MAX_CLIENT_TEXT: usize = 100;

/// The public descriptor: what a server says about itself to anyone who
/// asks, with no credential, at [`route::DESCRIPTOR`](crate::route). A
/// client reads it to check which server answered before it sends one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct PublicDescriptor {
    /// The server's ID: made on first start and never changed.
    pub server_id: Id,
    /// The server's display name. Text from another machine: display only.
    pub name: String,
    /// The server's role.
    pub role: ServerRole,
    /// The [contract version](crate::CONTRACT_VERSION) the server speaks.
    pub contract_version: u32,
    /// The capability flags: what this server can do beyond the calls
    /// every server has. A client gates a feature on its flag, never on a
    /// version, and ignores a flag it does not know. See
    /// [`capability`](crate::capability).
    pub capabilities: Vec<String>,
    /// The limits a client must stay within.
    pub limits: Limits,
}

impl PublicDescriptor {
    /// Whether the server has this capability flag.
    pub fn has(&self, capability: &str) -> bool {
        self.capabilities.iter().any(|flag| flag == capability)
    }
}

/// The limits a client must stay within.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct Limits {
    /// The largest frame the server reads, in bytes. It closes a socket
    /// that sends a larger one.
    pub max_message_bytes: u64,
}

/// The descriptor: the result of `server.hello`. It is the public
/// descriptor, and what the server says to this session alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct Descriptor {
    /// What anyone may know.
    #[serde(flatten)]
    pub server: PublicDescriptor,
    /// What this session may do.
    pub session_kind: SessionKind,
}

/// The params of a method that takes none: an empty object, which may also
/// be left out of the frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct NoParams {}

/// The result of `server.status`. See "Configuration and logs" in
/// docs/design/server.md.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ServerStatus {
    /// The server's ID.
    pub server_id: Id,
    /// The server's display name.
    pub name: String,
    /// The server's role.
    pub role: ServerRole,
    /// What the server is built from and what its database is at.
    pub versions: Versions,
    /// The space left where the server keeps what it owns.
    pub free_disk: FreeDisk,
    /// The directory the server writes its logs in, on the server's own
    /// disk.
    pub log_path: String,
}

/// The versions a status reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct Versions {
    /// The server program.
    pub server: String,
    /// The [contract version](crate::CONTRACT_VERSION) it speaks.
    pub contract: u32,
    /// The SQLite it is built with.
    pub sqlite: String,
    /// The newest migration `server.db` has had applied.
    pub server_schema: i64,
}

/// Free disk space, in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct FreeDisk {
    /// What this server may still write on the disk that holds its data
    /// directory. It is left out when the system would not say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub data_bytes: Option<u64>,
}

/// The result of `servers.list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ServersList {
    /// Every server this one knows, itself included.
    pub servers: Vec<ServerInfo>,
}

/// One server's full status: a row of `servers.list`, and what an event on
/// the `servers` topic carries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ServerInfo {
    /// The server's ID.
    pub server_id: Id,
    /// The server's display name.
    pub name: String,
    /// The server's role.
    pub role: ServerRole,
    /// Whether this is the server the client is connected to.
    pub is_this_server: bool,
    /// Whether the server can be reached now.
    pub reachable: bool,
    /// Counts the changes to this status. A client keeps the copy with the
    /// higher revision and drops the other. It counts from when the
    /// answering server started, which is enough: a client that connects
    /// again reads everything again (docs/design/contract.md, decision 12).
    pub revision: u64,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::capability;

    fn public() -> PublicDescriptor {
        PublicDescriptor {
            server_id: Id::from_bytes([0x11; 16]),
            name: "desk".to_owned(),
            role: ServerRole::Unassigned,
            contract_version: 0,
            capabilities: vec![capability::EVENTS_SERVERS.to_owned()],
            limits: Limits {
                max_message_bytes: 1_048_576,
            },
        }
    }

    #[test]
    fn fields_are_camel_case_and_enum_values_snake_case() {
        let hello = HelloParams {
            name: "ritornelctl".to_owned(),
            kind: ClientKind::Cli,
            version: "0.1.0".to_owned(),
            contract_version: 0,
        };
        let wire = json!({
            "name": "ritornelctl",
            "kind": "cli",
            "version": "0.1.0",
            "contractVersion": 0,
        });
        assert_eq!(serde_json::to_value(&hello).unwrap(), wire);
        assert_eq!(serde_json::from_value::<HelloParams>(wire).unwrap(), hello);

        assert_eq!(
            serde_json::to_value(SessionKind::Limited).unwrap(),
            "limited"
        );
        for role in [
            ServerRole::Unassigned,
            ServerRole::Hub,
            ServerRole::Satellite,
        ] {
            assert_eq!(serde_json::to_value(role).unwrap(), role.as_str());
        }
        for kind in [
            ClientKind::Desktop,
            ClientKind::Browser,
            ClientKind::Phone,
            ClientKind::Cli,
            ClientKind::Agent,
        ] {
            assert_eq!(serde_json::to_value(kind).unwrap(), kind.as_str());
        }
    }

    #[test]
    fn the_descriptor_is_the_public_one_and_the_sessions_kind_in_one_object() {
        let descriptor = Descriptor {
            server: public(),
            session_kind: SessionKind::Full,
        };
        let wire = json!({
            "serverId": "11111111-1111-1111-1111-111111111111",
            "name": "desk",
            "role": "unassigned",
            "contractVersion": 0,
            "capabilities": ["events.servers"],
            "limits": { "maxMessageBytes": 1_048_576 },
            "sessionKind": "full",
        });
        assert_eq!(serde_json::to_value(&descriptor).unwrap(), wire);
        assert_eq!(
            serde_json::from_value::<Descriptor>(wire.clone()).unwrap(),
            descriptor
        );
        // The public descriptor is the same object without the session.
        assert_eq!(
            serde_json::from_value::<PublicDescriptor>(wire).unwrap(),
            public()
        );
        assert!(public().has(capability::EVENTS_SERVERS));
        assert!(!public().has("albums.collapse"));
    }

    #[test]
    fn a_field_this_program_does_not_know_is_ignored() {
        let newer = json!({
            "serverId": "11111111-1111-1111-1111-111111111111",
            "name": "desk",
            "role": "unassigned",
            "contractVersion": 0,
            "capabilities": ["events.servers", "from.the.future"],
            "limits": { "maxMessageBytes": 1_048_576, "maxViews": 8 },
            "libraryRevision": 4711,
        });
        let read = serde_json::from_value::<PublicDescriptor>(newer).unwrap();
        assert_eq!(read.limits, public().limits);
        assert!(read.has("from.the.future"));
    }

    #[test]
    fn a_status_leaves_out_what_the_system_would_not_say() {
        let mut status = ServerStatus {
            server_id: Id::from_bytes([0x11; 16]),
            name: "desk".to_owned(),
            role: ServerRole::Hub,
            versions: Versions {
                server: "0.1.0".to_owned(),
                contract: 0,
                sqlite: "3.54.1".to_owned(),
                server_schema: 1,
            },
            free_disk: FreeDisk {
                data_bytes: Some(1_000),
            },
            log_path: "/srv/ritornel/logs".to_owned(),
        };
        let mut wire = json!({
            "serverId": "11111111-1111-1111-1111-111111111111",
            "name": "desk",
            "role": "hub",
            "versions": { "server": "0.1.0", "contract": 0, "sqlite": "3.54.1", "serverSchema": 1 },
            "freeDisk": { "dataBytes": 1_000 },
            "logPath": "/srv/ritornel/logs",
        });
        assert_eq!(serde_json::to_value(&status).unwrap(), wire);

        status.free_disk.data_bytes = None;
        wire["freeDisk"] = json!({});
        assert_eq!(serde_json::to_value(&status).unwrap(), wire);
        assert_eq!(
            serde_json::from_value::<ServerStatus>(wire).unwrap(),
            status
        );
    }

    #[test]
    fn no_params_is_an_empty_object() {
        assert_eq!(serde_json::to_value(NoParams {}).unwrap(), json!({}));
        serde_json::from_value::<NoParams>(json!({})).unwrap();
        serde_json::from_value::<NoParams>(json!({ "later": true })).unwrap();
        assert!(serde_json::from_value::<NoParams>(json!(null)).is_err());
    }
}
