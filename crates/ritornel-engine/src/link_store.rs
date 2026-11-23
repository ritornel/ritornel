//! The link between a hub and a satellite, as `server.db` keeps it: on a
//! satellite the one `hub_link` row, and on a hub a session for each
//! satellite. See "Hub and satellites" in docs/internals/linking.md.

use rusqlite::{Connection, OptionalExtension};

use crate::server_store::{ServerIdentity, read_identity};
use crate::{Error, Id, LibraryStore, ServerStore, SessionToken, WriteRequest, new_id};

/// A satellite's link to its hub.
#[derive(Debug, Clone)]
pub struct HubLink {
    /// The hub's server ID.
    pub hub: Id,
    /// The hub's public key, pinned when the satellite joined.
    pub hub_public_key: [u8; 32],
    /// The hub's display name, as it gave it. Untrusted text.
    pub hub_name: String,
    /// The library the hub holds.
    pub library_id: Id,
    /// The hub epoch when the satellite joined.
    pub hub_epoch: i64,
    /// Where the hub is reached, tried in order: `address:port`.
    pub routes: Vec<String>,
    /// The link session's token at the hub.
    pub token: SessionToken,
    /// When the satellite joined, as Unix milliseconds.
    pub linked_at_ms: i64,
}

/// One satellite's session at a hub.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkSession {
    /// The session's public ID: the actor on what the satellite sends.
    pub session: Id,
    /// The satellite's server ID.
    pub peer: Id,
    /// The satellite's name when it joined. Untrusted text.
    pub label: String,
    /// The public key the satellite proved it holds when it joined.
    pub public_key: [u8; 32],
    /// The SHA-256 of the session's token.
    pub token_hash: [u8; 32],
    /// When it was issued, as Unix milliseconds.
    pub created_at_ms: i64,
}

impl ServerStore {
    /// This server's link to its hub, if it is a satellite.
    pub fn hub_link(&self) -> Result<Option<HubLink>, Error> {
        self.read(read_hub_link)
    }

    /// Makes this server a satellite of the hub `link` names: records the
    /// link, the role and the library in one transaction, and returns the
    /// identity as it now is. A server that already holds a library, or is
    /// linked already, is refused and left as it was.
    pub fn link_to_hub(&self, link: &HubLink) -> Result<ServerIdentity, Error> {
        let link = link.clone();
        self.transact(move |transaction, _, _| {
            let identity =
                read_identity(transaction)?.ok_or(Error::Corrupt("a server with no identity"))?;
            if identity.role != crate::Role::Unassigned || read_hub_link(transaction)?.is_some() {
                return Err(Error::Invalid(
                    "this server already holds a library or is linked to a hub",
                ));
            }
            transaction.execute(
                "INSERT INTO hub_link
                   (id, hub_server_id, hub_public_key, hub_name, library_id, hub_epoch,
                    routes_json, token, linked_at_ms)
                 VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                (
                    link.hub.as_bytes(),
                    link.hub_public_key,
                    &link.hub_name,
                    link.library_id.as_bytes(),
                    link.hub_epoch,
                    serde_json::to_string(&link.routes)?,
                    link.token.expose(),
                    link.linked_at_ms,
                ),
            )?;
            transaction.execute(
                "UPDATE server_identity SET role = 'satellite', library_id = ?1 WHERE id = 1",
                [link.library_id.as_bytes()],
            )?;
            read_identity(transaction)?.ok_or(Error::Corrupt("a server with no identity"))
        })
    }

    /// Issues a link session for the satellite `peer`, which proved it
    /// holds `public_key`, labelled with its name, and returns its public ID
    /// and its token. The token is returned here only: the database keeps
    /// its hash. A session the satellite held before is revoked in the same
    /// transaction, so a satellite that joins again holds one session. A
    /// server ID that was linked with another key is refused, so an invite
    /// cannot take over a satellite that is linked already.
    pub fn issue_link_session(
        &self,
        peer: Id,
        public_key: [u8; 32],
        label: &str,
    ) -> Result<(Id, SessionToken), Error> {
        let label = label.to_owned();
        self.transact(move |transaction, now_ms, entropy| {
            let known: Option<[u8; 32]> = transaction
                .query_row(
                    "SELECT peer_public_key FROM session
                     WHERE peer_server_pid = ?1 AND peer_public_key IS NOT NULL
                     ORDER BY id DESC LIMIT 1",
                    [peer.as_bytes()],
                    |row| row.get(0),
                )
                .optional()?;
            if known.is_some_and(|known| known != public_key) {
                return Err(Error::Invalid(
                    "a server of this ID is linked already, with another key",
                ));
            }
            transaction.execute(
                "UPDATE session SET revoked_at_ms = ?1
                 WHERE peer_server_pid = ?2 AND revoked_at_ms IS NULL",
                (now_ms, peer.as_bytes()),
            )?;
            let token = SessionToken::generate(entropy)?;
            let session = new_id(now_ms, entropy)?;
            transaction.execute(
                "INSERT INTO session
                   (public_id, subject_kind, label, kind, token_hash, created_at_ms, peer_server_pid,
                    peer_public_key)
                 VALUES (?1, 'server', ?2, 'full', ?3, ?4, ?5, ?6)",
                (
                    session.as_bytes(),
                    &label,
                    token.hash(),
                    now_ms,
                    peer.as_bytes(),
                    public_key,
                ),
            )?;
            Ok((session, token))
        })
    }

    /// The link sessions that have not been revoked, oldest first.
    /// Names the link session of the satellite `peer` `label`, and says
    /// whether that was a new name. The same name again is only read.
    pub fn relabel_link_session(&self, peer: Id, label: &str) -> Result<bool, Error> {
        let label = label.to_owned();
        let named = self
            .link_sessions()?
            .into_iter()
            .any(|session| session.peer == peer && session.label == label);
        if named {
            return Ok(false);
        }
        self.transact(move |transaction, _, _| {
            let changed = transaction.execute(
                "UPDATE session SET label = ?1
                 WHERE peer_server_pid = ?2 AND revoked_at_ms IS NULL AND label != ?1",
                (&label, peer.as_bytes()),
            )?;
            Ok(changed > 0)
        })
    }

    pub fn link_sessions(&self) -> Result<Vec<LinkSession>, Error> {
        self.read(|connection| {
            let mut statement = connection.prepare(
                "SELECT public_id, peer_server_pid, label, token_hash, created_at_ms,
                        peer_public_key
                 FROM session
                 WHERE peer_server_pid IS NOT NULL AND revoked_at_ms IS NULL
                 ORDER BY id",
            )?;
            let rows = statement.query_map([], |row| {
                Ok(LinkSession {
                    session: Id::from_bytes(row.get(0)?),
                    peer: Id::from_bytes(row.get(1)?),
                    label: row.get(2)?,
                    token_hash: row.get(3)?,
                    created_at_ms: row.get(4)?,
                    public_key: row.get(5)?,
                })
            })?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
    }
}

/// What a library write that registers a satellite is recorded as.
pub(crate) const REGISTER_METHOD: &str = "link.register";

impl LibraryStore {
    /// Adds the server `server` to the library's registry, with its player
    /// as a place listens come from, so that what it plays can be taken
    /// in. A server already there keeps its rows and takes the new name.
    /// The name the library's registry gives the server `server`, if it
    /// has it.
    pub fn server_name(&self, server: Id) -> Result<Option<String>, Error> {
        self.read(|connection| {
            Ok(connection
                .query_row(
                    "SELECT name FROM server WHERE public_id = ?1",
                    [server.as_bytes()],
                    |row| row.get(0),
                )
                .optional()?)
        })
    }

    pub fn register_server(&self, server: Id, name: &str) -> Result<(), Error> {
        let name = name.to_owned();
        self.write(
            WriteRequest {
                method: REGISTER_METHOD,
                command: None,
                client: None,
            },
            move |write| {
                let (connection, rev) = (write.connection(), write.rev());
                connection.execute(
                    "INSERT INTO server (public_id, name, created_at_ms, rev) VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT (public_id) DO UPDATE SET name = excluded.name, rev = excluded.rev",
                    (server.as_bytes(), &name, write.now_ms(), rev),
                )?;
                let row: i64 = connection.query_row(
                    "SELECT id FROM server WHERE public_id = ?1",
                    [server.as_bytes()],
                    |row| row.get(0),
                )?;
                let has_player = connection.query_row(
                    "SELECT EXISTS (SELECT 1 FROM history_endpoint WHERE kind = 'player' AND server_id = ?1)",
                    [row],
                    |row| row.get::<_, bool>(0),
                )?;
                if !has_player {
                    connection.execute(
                        "INSERT INTO history_endpoint (public_id, kind, server_id, name, rev)
                         VALUES (?1, 'player', ?2, 'player', ?3)",
                        (write.new_id()?.as_bytes(), row, rev),
                    )?;
                }
                Ok(String::new())
            },
        )?;
        Ok(())
    }
}

fn read_hub_link(connection: &Connection) -> Result<Option<HubLink>, Error> {
    let row = connection
        .query_row(
            "SELECT hub_server_id, hub_public_key, hub_name, library_id, hub_epoch, routes_json,
                    token, linked_at_ms
             FROM hub_link WHERE id = 1",
            [],
            |row| {
                Ok((
                    Id::from_bytes(row.get(0)?),
                    row.get::<_, [u8; 32]>(1)?,
                    row.get::<_, String>(2)?,
                    Id::from_bytes(row.get(3)?),
                    row.get::<_, i64>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, i64>(7)?,
                ))
            },
        )
        .optional()?;
    row.map(
        |(hub, hub_public_key, hub_name, library_id, hub_epoch, routes, token, linked_at_ms)| {
            Ok(HubLink {
                hub,
                hub_public_key,
                hub_name,
                library_id,
                hub_epoch,
                routes: serde_json::from_str(&routes)?,
                token: SessionToken::new(token),
                linked_at_ms,
            })
        },
    )
    .transpose()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::testing::{CountingEntropy, FixedClock};
    use crate::{Role, ServerStore};

    fn store() -> ServerStore {
        let store = ServerStore::open_in_memory(
            Arc::new(FixedClock::at(1_000)),
            Arc::new(CountingEntropy::default()),
        )
        .unwrap();
        store.ensure_identity("desk").unwrap();
        store
    }

    fn link() -> HubLink {
        HubLink {
            hub: Id::from_bytes([0x11; 16]),
            hub_public_key: [0x22; 32],
            hub_name: "attic".to_owned(),
            library_id: Id::from_bytes([0x33; 16]),
            hub_epoch: 6,
            routes: vec![
                "192.168.1.20:4747".to_owned(),
                "100.101.7.3:4747".to_owned(),
            ],
            token: SessionToken::new("a".repeat(64)),
            linked_at_ms: 1_000,
        }
    }

    #[test]
    fn joining_makes_a_server_a_satellite_once() {
        let store = store();
        assert!(store.hub_link().unwrap().is_none());
        let identity = store.link_to_hub(&link()).unwrap();
        assert_eq!(identity.role, Role::Satellite);
        assert_eq!(identity.library_id, Some(link().library_id));
        let kept = store.hub_link().unwrap().unwrap();
        let expected = link();
        assert_eq!(
            (
                kept.hub,
                kept.hub_public_key,
                kept.hub_name,
                kept.library_id
            ),
            (
                expected.hub,
                expected.hub_public_key,
                expected.hub_name,
                expected.library_id
            )
        );
        assert_eq!(
            (kept.hub_epoch, kept.routes),
            (expected.hub_epoch, expected.routes)
        );
        assert_eq!(kept.token.expose(), expected.token.expose());
        // A second join, to this hub or another, is refused.
        assert!(matches!(store.link_to_hub(&link()), Err(Error::Invalid(_))));
    }

    #[test]
    fn a_hub_does_not_join_another() {
        let store = store();
        store.claim_library(Id::from_bytes([0x44; 16])).unwrap();
        assert!(matches!(store.link_to_hub(&link()), Err(Error::Invalid(_))));
        assert!(store.hub_link().unwrap().is_none());
    }

    #[test]
    fn a_satellite_holds_one_session_and_only_its_hash_is_kept() {
        let store = store();
        let peer = Id::from_bytes([0x55; 16]);
        let (first, first_token) = store.issue_link_session(peer, [7; 32], "desk").unwrap();
        let (second, second_token) = store.issue_link_session(peer, [7; 32], "desk").unwrap();
        assert_ne!(first, second);
        assert_ne!(first_token.expose(), second_token.expose());
        let sessions = store.link_sessions().unwrap();
        assert_eq!(sessions.len(), 1);
        let held = sessions.first().unwrap();
        assert_eq!((held.session, held.peer), (second, peer));
        assert_eq!(held.token_hash, second_token.hash());
        // Another key does not take the ID over.
        assert!(matches!(
            store.issue_link_session(peer, [8; 32], "thief"),
            Err(Error::Invalid(_))
        ));
        assert_eq!(store.link_sessions().unwrap().len(), 1);
        // The local credential is not a link session.
        assert!(
            sessions
                .iter()
                .all(|session| Some(session.session) != store.local_session().unwrap())
        );
    }
}
