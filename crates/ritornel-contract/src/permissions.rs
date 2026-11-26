//! Scoped per-token permissions and named roles. See "Local credentials,
//! sessions, and tokens" in docs/internals/linking.md and "Session and frames"
//! in docs/internals/contract.md.
//!
//! Each permission is named as `area:action`. A token holds a fixed set of
//! permissions chosen when it is created, and remembers the named
//! [`TokenRole`] it started from (if any). A method declares the permissions
//! it requires in [`crate::methods::METHODS`].

use serde::{Deserialize, Serialize};

/// One capability a session token may hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum Permission {
    /// Read the library, listening history, covers, servers and players.
    #[serde(rename = "library:read")]
    LibraryRead,
    /// Edit library metadata, ratings, scans, imports, corrections and library settings.
    #[serde(rename = "library:write")]
    LibraryWrite,
    /// Control playback and queue state on reachable players.
    #[serde(rename = "playback:control")]
    PlaybackControl,
    /// Read server health, jobs, link status, Last.fm status and settings (without paths).
    #[serde(rename = "status:read")]
    StatusRead,
    /// See and manage folders and filesystem paths on the server's machine.
    #[serde(rename = "folders:manage")]
    FoldersManage,
    /// Run server-level operations: settings, backups, library creation and transcode cache.
    #[serde(rename = "server:manage")]
    ServerManage,
    /// Connect and configure external services such as Last.fm.
    #[serde(rename = "services:connect")]
    ServicesConnect,
    /// Manage access: invites, links and tokens.
    #[serde(rename = "access:manage")]
    AccessManage,
}

impl Permission {
    /// Every permission in canonical order.
    pub const ALL: &'static [Self] = &[
        Self::LibraryRead,
        Self::LibraryWrite,
        Self::PlaybackControl,
        Self::StatusRead,
        Self::FoldersManage,
        Self::ServerManage,
        Self::ServicesConnect,
        Self::AccessManage,
    ];

    /// The permission as written on the wire and in storage.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LibraryRead => "library:read",
            Self::LibraryWrite => "library:write",
            Self::PlaybackControl => "playback:control",
            Self::StatusRead => "status:read",
            Self::FoldersManage => "folders:manage",
            Self::ServerManage => "server:manage",
            Self::ServicesConnect => "services:connect",
            Self::AccessManage => "access:manage",
        }
    }

    /// Parses a wire permission name.
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "library:read" => Some(Self::LibraryRead),
            "library:write" => Some(Self::LibraryWrite),
            "playback:control" => Some(Self::PlaybackControl),
            "status:read" => Some(Self::StatusRead),
            "folders:manage" => Some(Self::FoldersManage),
            "server:manage" => Some(Self::ServerManage),
            "services:connect" => Some(Self::ServicesConnect),
            "access:manage" => Some(Self::AccessManage),
            _ => None,
        }
    }

    const fn bit(self) -> u8 {
        match self {
            Self::LibraryRead => 1 << 0,
            Self::LibraryWrite => 1 << 1,
            Self::PlaybackControl => 1 << 2,
            Self::StatusRead => 1 << 3,
            Self::FoldersManage => 1 << 4,
            Self::ServerManage => 1 << 5,
            Self::ServicesConnect => 1 << 6,
            Self::AccessManage => 1 << 7,
        }
    }
}

/// A named permission bundle used as a starting point when issuing a token.
///
/// Separate from [`crate::ServerRole`] (whether a server is a hub or satellite).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum TokenRole {
    /// All permissions: local owner credential and owner browser sessions.
    Owner,
    /// Library read/write, playback control and status: a personal desktop or phone.
    Personal,
    /// Library read and status: a headless player satellite.
    Player,
    /// Library read and playback control: a guest or limited adapter.
    Guest,
    /// Status read only: a monitoring dashboard.
    Monitor,
}

impl TokenRole {
    /// Every named role in canonical order.
    pub const ALL: &'static [Self] = &[
        Self::Owner,
        Self::Personal,
        Self::Player,
        Self::Guest,
        Self::Monitor,
    ];

    /// The role as written on the wire and in storage.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Personal => "personal",
            Self::Player => "player",
            Self::Guest => "guest",
            Self::Monitor => "monitor",
        }
    }

    /// Parses a wire role name.
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "owner" => Some(Self::Owner),
            "personal" => Some(Self::Personal),
            "player" => Some(Self::Player),
            "guest" => Some(Self::Guest),
            "monitor" => Some(Self::Monitor),
            _ => None,
        }
    }

    /// The default permissions for this role.
    pub const fn permissions(self) -> &'static [Permission] {
        match self {
            Self::Owner => Permission::ALL,
            Self::Personal => &[
                Permission::LibraryRead,
                Permission::LibraryWrite,
                Permission::PlaybackControl,
                Permission::StatusRead,
            ],
            Self::Player => &[Permission::LibraryRead, Permission::StatusRead],
            Self::Guest => &[Permission::LibraryRead, Permission::PlaybackControl],
            Self::Monitor => &[Permission::StatusRead],
        }
    }
}

/// A compact, copyable set of [`Permission`] values held by a session.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct PermissionSet(u8);

impl PermissionSet {
    /// No permissions.
    pub const EMPTY: Self = Self(0);

    /// Every permission.
    pub const ALL: Self = Self(u8::MAX);

    /// A set containing every permission in `permissions`.
    pub const fn from_slice(mut permissions: &[Permission]) -> Self {
        let mut bits = 0u8;
        while let Some((first, rest)) = permissions.split_first() {
            bits |= first.bit();
            permissions = rest;
        }
        Self(bits)
    }

    /// The permission set for a named [`TokenRole`].
    pub const fn for_role(role: TokenRole) -> Self {
        Self::from_slice(role.permissions())
    }

    /// Whether this set holds `permission`.
    pub const fn contains(self, permission: Permission) -> bool {
        (self.0 & permission.bit()) != 0
    }

    /// Whether this set holds every permission in `required`.
    pub const fn contains_all(self, required: &[Permission]) -> bool {
        let need = Self::from_slice(required);
        (self.0 & need.0) == need.0
    }

    /// Whether this set holds at least one permission in `candidates`.
    pub const fn contains_any(self, candidates: &[Permission]) -> bool {
        if candidates.is_empty() {
            return true;
        }
        let mask = Self::from_slice(candidates);
        (self.0 & mask.0) != 0
    }

    /// Whether this set holds every permission in `other`.
    pub const fn is_superset_of(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    /// Whether this set holds no permissions.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Returns the permissions in canonical order.
    pub fn to_vec(self) -> Vec<Permission> {
        Permission::ALL
            .iter()
            .copied()
            .filter(|permission| self.contains(*permission))
            .collect()
    }

    /// Returns `role` if this set holds exactly `role`'s current permissions.
    pub const fn exact_role(self, role: Option<TokenRole>) -> Option<TokenRole> {
        match role {
            Some(role) if self.0 == Self::for_role(role).0 => Some(role),
            _ => None,
        }
    }

    /// Returns the named [`TokenRole`] whose permission set matches this set
    /// exactly, if any.
    pub const fn matching_role(self) -> Option<TokenRole> {
        let mut roles = TokenRole::ALL;
        while let Some((role, rest)) = roles.split_first() {
            if self.0 == Self::for_role(*role).0 {
                return Some(*role);
            }
            roles = rest;
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_wire_names_and_parse_round_trip() {
        for permission in Permission::ALL {
            let wire = serde_json::to_string(permission).unwrap();
            assert_eq!(wire, format!("\"{}\"", permission.as_str()));
            assert_eq!(Permission::parse(permission.as_str()), Some(*permission));
            let decoded: Permission = serde_json::from_str(&wire).unwrap();
            assert_eq!(decoded, *permission);
        }
        assert_eq!(Permission::parse("unknown:read"), None);
    }

    #[test]
    fn role_wire_names_and_permissions_match() {
        for role in TokenRole::ALL {
            let wire = serde_json::to_string(role).unwrap();
            assert_eq!(wire, format!("\"{}\"", role.as_str()));
            assert_eq!(TokenRole::parse(role.as_str()), Some(*role));
            let set = PermissionSet::for_role(*role);
            assert_eq!(set.exact_role(Some(*role)), Some(*role));
            assert_eq!(set.to_vec(), role.permissions());
        }
        assert_eq!(
            PermissionSet::for_role(TokenRole::Owner),
            PermissionSet::ALL
        );
        assert_eq!(
            PermissionSet::from_slice(&[Permission::StatusRead])
                .exact_role(Some(TokenRole::Personal)),
            None
        );
    }

    #[test]
    fn permission_set_operations_hold() {
        let personal = PermissionSet::for_role(TokenRole::Personal);
        assert!(personal.contains(Permission::LibraryRead));
        assert!(!personal.contains(Permission::FoldersManage));
        assert!(personal.contains_all(&[Permission::LibraryRead, Permission::StatusRead]));
        assert!(!personal.contains_all(&[Permission::LibraryRead, Permission::ServerManage]));
        assert!(personal.contains_any(&[Permission::FoldersManage, Permission::StatusRead]));
        assert!(!personal.contains_any(&[Permission::FoldersManage, Permission::AccessManage]));
        assert!(PermissionSet::ALL.is_superset_of(personal));
        assert!(!personal.is_superset_of(PermissionSet::ALL));
    }
}
