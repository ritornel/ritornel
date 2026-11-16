//! `server.db`: what describes this machine. In this layer that is the
//! server's identity and the local credential's session. See "Which file
//! holds what" in docs/design/server.md and decision 1 in
//! docs/design/linking.md.

use std::path::Path;
use std::sync::Arc;

use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ValueRef};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior};

use crate::migrate::{self, Migrated};
use crate::secret::SigningKey;
use crate::sqlite::{self, Location, Reader, Writer};
use crate::{Clock, Entropy, Error, Id, SessionToken, new_id};

/// The `server_setting` row that says which session is the local
/// credential's. Its value is that session's row ID.
const LOCAL_CREDENTIAL_SESSION: &str = "local_credential_session_id";

/// The label of the local credential's session. Display text only.
const LOCAL_CREDENTIAL_LABEL: &str = "local owner";

/// A server's own record of its role. The library's hub pointer decides
/// which server is the hub (docs/design/linking.md, decision 2), and until
/// there is a library every server is unassigned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// It has no library and is linked to none.
    Unassigned,
    /// It holds the library.
    Hub,
    /// It is linked to a hub.
    Satellite,
}

impl FromSql for Role {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        match value.as_str()? {
            "unassigned" => Ok(Self::Unassigned),
            "hub" => Ok(Self::Hub),
            "satellite" => Ok(Self::Satellite),
            _ => Err(FromSqlError::InvalidType),
        }
    }
}

/// The public half of a server's identity. The signing key stays in the
/// database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerIdentity {
    /// The server's ID: a UUIDv7 made on first start and never changed.
    pub server_id: Id,
    /// The Ed25519 public key that clients and peers pin.
    pub public_key: [u8; 32],
    /// The server's display name.
    pub name: String,
    /// The server's own record of its role.
    pub role: Role,
    /// The library it holds, as a hub, or is linked to, as a satellite.
    pub library_id: Option<Id>,
    /// When the identity was created, as Unix milliseconds.
    pub created_at_ms: i64,
}

/// An open `server.db`: one write connection on a thread of its own, and a
/// read-only connection for reads.
#[derive(Debug)]
pub struct ServerStore {
    writer: Writer,
    reader: Reader,
    clock: Arc<dyn Clock>,
    entropy: Arc<dyn Entropy>,
    migrated: Migrated,
}

impl ServerStore {
    /// Opens the database at `path`, creating it if it is missing, and
    /// brings its schema up to date.
    ///
    /// It refuses, and changes nothing, if the SQLite built in is not
    /// [`SQLITE_VERSION`](crate::SQLITE_VERSION), if the file was written by
    /// a newer version of this program, or if a migration it was built with
    /// has since changed.
    pub fn open(
        path: &Path,
        clock: Arc<dyn Clock>,
        entropy: Arc<dyn Entropy>,
    ) -> Result<Self, Error> {
        Self::open_at(Location::File(path.to_owned()), clock, entropy)
    }

    /// Opens a new, empty database in memory, for tests.
    pub fn open_in_memory(clock: Arc<dyn Clock>, entropy: Arc<dyn Entropy>) -> Result<Self, Error> {
        Self::open_at(Location::memory(), clock, entropy)
    }

    fn open_at(
        location: Location,
        clock: Arc<dyn Clock>,
        entropy: Arc<dyn Entropy>,
    ) -> Result<Self, Error> {
        sqlite::assert_version()?;
        let writer = Writer::start(&location, "server.db writer")?;
        let migrated = {
            let clock = Arc::clone(&clock);
            writer.call(move |connection| migrate::run(connection, migrate::SERVER, &*clock))?
        };
        let reader = Reader::open(&location)?;
        Ok(Self {
            writer,
            reader,
            clock,
            entropy,
            migrated,
        })
    }

    /// The schema version the database is at.
    pub fn schema_version(&self) -> i64 {
        self.migrated.version
    }

    /// How many migrations opening the database applied. 0 when it was up
    /// to date.
    pub fn migrations_applied(&self) -> usize {
        self.migrated.applied
    }

    /// The server's identity, if it has one yet.
    pub fn identity(&self) -> Result<Option<ServerIdentity>, Error> {
        self.reader.call(read_identity)
    }

    /// Gives the server its identity if it has none: a new ID, a new signing
    /// key and the local credential's session, all in one transaction, so a
    /// server never runs with half an identity.
    ///
    /// The token is returned only by the call that created it. The database
    /// keeps its hash, so the caller must put the token where the local
    /// owner can read it, or [reissue](Self::reissue_local_credential) it.
    /// `name` is used only when the identity is created.
    pub fn ensure_identity(
        &self,
        name: &str,
    ) -> Result<(ServerIdentity, Option<SessionToken>), Error> {
        let (clock, entropy) = (Arc::clone(&self.clock), Arc::clone(&self.entropy));
        let name = name.to_owned();
        self.writer.call(move |connection| {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            if let Some(identity) = read_identity(&transaction)? {
                return Ok((identity, None));
            }
            let now_ms = clock.now_ms();
            let key = SigningKey::generate(&*entropy)?;
            let identity = ServerIdentity {
                server_id: new_id(now_ms, &*entropy)?,
                public_key: key.public_key(),
                name,
                // What the two columns default to.
                role: Role::Unassigned,
                library_id: None,
                created_at_ms: now_ms,
            };
            transaction.execute(
                "INSERT INTO server_identity
                   (id, server_id, signing_key, public_key, name, created_at_ms)
                 VALUES (1, ?1, ?2, ?3, ?4, ?5)",
                (
                    identity.server_id.as_bytes(),
                    key.expose(),
                    identity.public_key,
                    &identity.name,
                    identity.created_at_ms,
                ),
            )?;
            let token = issue_local_credential(&transaction, now_ms, &*entropy)?;
            transaction.commit()?;
            Ok((identity, Some(token)))
        })
    }

    /// Changes the server's display name, and returns the identity as it
    /// now is: `None` if the server has none yet. The ID and the key never
    /// change. What a name may be is for the call that takes one from a
    /// client to decide; there is none yet.
    pub fn rename(&self, name: &str) -> Result<Option<ServerIdentity>, Error> {
        let name = name.to_owned();
        self.writer.call(move |connection| {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute("UPDATE server_identity SET name = ?1 WHERE id = 1", [&name])?;
            let identity = read_identity(&transaction)?;
            transaction.commit()?;
            Ok(identity)
        })
    }

    /// Records that this server is the hub of the library `library_id`,
    /// and returns the identity as it now is: `None` if the server has none
    /// yet. This is the server's own claim. The library's hub pointer is
    /// what decides, and it is written first, in `library.db`.
    pub fn claim_library(&self, library_id: Id) -> Result<Option<ServerIdentity>, Error> {
        self.writer.call(move |connection| {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute(
                "UPDATE server_identity SET role = 'hub', library_id = ?1 WHERE id = 1",
                [library_id.as_bytes()],
            )?;
            let identity = read_identity(&transaction)?;
            transaction.commit()?;
            Ok(identity)
        })
    }

    /// The public ID of the local credential's session: the actor on every
    /// command that session sends. `None` before the server has an
    /// identity.
    pub fn local_session(&self) -> Result<Option<Id>, Error> {
        self.reader.call(|connection| {
            Ok(connection
                .query_row(
                    "SELECT session.public_id
                     FROM session
                     JOIN server_setting
                       ON server_setting.key = ?1
                      AND session.id = CAST(server_setting.value_json AS INTEGER)
                     WHERE session.revoked_at_ms IS NULL",
                    [LOCAL_CREDENTIAL_SESSION],
                    |row| row.get(0),
                )
                .optional()?
                .map(Id::from_bytes))
        })
    }

    /// Whether `token` is the local credential: the token of the local
    /// credential's session, which has not been revoked.
    pub fn is_local_credential(&self, token: &SessionToken) -> Result<bool, Error> {
        let hash = token.hash();
        self.reader.call(|connection| {
            Ok(connection.query_row(
                "SELECT EXISTS (
                   SELECT 1
                   FROM session
                   JOIN server_setting
                     ON server_setting.key = ?1
                    AND session.id = CAST(server_setting.value_json AS INTEGER)
                   WHERE session.token_hash = ?2 AND session.revoked_at_ms IS NULL
                 )",
                (LOCAL_CREDENTIAL_SESSION, hash),
                |row| row.get(0),
            )?)
        })
    }

    /// Revokes the local credential's session and issues a new one, in one
    /// transaction. This is what puts things right when the token has been
    /// lost: only its hash is kept, so it cannot be read back.
    pub fn reissue_local_credential(&self) -> Result<SessionToken, Error> {
        let (clock, entropy) = (Arc::clone(&self.clock), Arc::clone(&self.entropy));
        self.writer.call(move |connection| {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let now_ms = clock.now_ms();
            transaction.execute(
                "UPDATE session SET revoked_at_ms = ?1
                 WHERE revoked_at_ms IS NULL
                   AND id = (SELECT CAST(value_json AS INTEGER) FROM server_setting WHERE key = ?2)",
                (now_ms, LOCAL_CREDENTIAL_SESSION),
            )?;
            let token = issue_local_credential(&transaction, now_ms, &*entropy)?;
            transaction.commit()?;
            Ok(token)
        })
    }

    /// Runs `work` in one write transaction, with the time it began.
    pub(crate) fn transact<T, F>(&self, work: F) -> Result<T, Error>
    where
        T: Send + 'static,
        F: FnOnce(&Transaction<'_>, i64, &dyn Entropy) -> Result<T, Error> + Send + 'static,
    {
        let (clock, entropy) = (Arc::clone(&self.clock), Arc::clone(&self.entropy));
        self.writer.call(move |connection| {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let done = work(&transaction, clock.now_ms(), &*entropy)?;
            transaction.commit()?;
            Ok(done)
        })
    }

    /// Runs a short read on the read-only connection.
    pub(crate) fn read<T>(
        &self,
        work: impl FnOnce(&Connection) -> Result<T, Error>,
    ) -> Result<T, Error> {
        self.reader.call(work)
    }

    /// Closes the database after a clean stop: moves the write-ahead log
    /// into the file and empties it.
    pub fn close(self) -> Result<(), Error> {
        drop(self.reader);
        self.writer.close()
    }
}

fn read_identity(connection: &Connection) -> Result<Option<ServerIdentity>, Error> {
    Ok(connection
        .query_row(
            "SELECT server_id, public_key, name, role, library_id, created_at_ms
             FROM server_identity WHERE id = 1",
            [],
            |row| {
                Ok(ServerIdentity {
                    server_id: Id::from_bytes(row.get(0)?),
                    public_key: row.get(1)?,
                    name: row.get(2)?,
                    role: row.get(3)?,
                    library_id: row.get::<_, Option<[u8; 16]>>(4)?.map(Id::from_bytes),
                    created_at_ms: row.get(5)?,
                })
            },
        )
        .optional()?)
}

/// Adds a `full` session for the person at this machine and points the
/// local-credential setting at it. Only the token's hash is stored.
fn issue_local_credential(
    transaction: &Transaction<'_>,
    now_ms: i64,
    entropy: &dyn Entropy,
) -> Result<SessionToken, Error> {
    let token = SessionToken::generate(entropy)?;
    transaction.execute(
        "INSERT INTO session (public_id, subject_kind, label, kind, token_hash, created_at_ms)
         VALUES (?1, 'person', ?2, 'full', ?3, ?4)",
        (
            new_id(now_ms, entropy)?.as_bytes(),
            LOCAL_CREDENTIAL_LABEL,
            token.hash(),
            now_ms,
        ),
    )?;
    transaction.execute(
        "INSERT INTO server_setting (key, value_json) VALUES (?1, ?2)
         ON CONFLICT (key) DO UPDATE SET value_json = excluded.value_json",
        (
            LOCAL_CREDENTIAL_SESSION,
            transaction.last_insert_rowid().to_string(),
        ),
    )?;
    Ok(token)
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::{Signer, Verifier, VerifyingKey};

    use super::*;
    use crate::testing::{CountingEntropy, FixedClock, Scratch};
    use crate::{OsEntropy, SystemClock};

    const T0: i64 = 1_763_000_000_000;

    fn in_memory(clock: &Arc<FixedClock>) -> ServerStore {
        ServerStore::open_in_memory(clock.clone(), Arc::new(CountingEntropy::default())).unwrap()
    }

    fn on_disk(path: &Path) -> Result<ServerStore, Error> {
        ServerStore::open(path, Arc::new(SystemClock), Arc::new(OsEntropy))
    }

    /// One row of `session`, as the tests look at it.
    #[derive(Debug, PartialEq, Eq)]
    struct Session {
        subject_kind: String,
        label: String,
        kind: String,
        token_hash: Vec<u8>,
        has_listener: bool,
        has_media_token: bool,
        created_at_ms: i64,
        revoked_at_ms: Option<i64>,
    }

    fn sessions(store: &ServerStore) -> Vec<Session> {
        store
            .reader
            .call(|connection| {
                let mut statement = connection.prepare(
                    "SELECT subject_kind, label, kind, token_hash, listener_pid IS NOT NULL,
                            media_token_hash IS NOT NULL, created_at_ms, revoked_at_ms
                     FROM session ORDER BY id",
                )?;
                let rows = statement.query_map([], |row| {
                    Ok(Session {
                        subject_kind: row.get(0)?,
                        label: row.get(1)?,
                        kind: row.get(2)?,
                        token_hash: row.get(3)?,
                        has_listener: row.get(4)?,
                        has_media_token: row.get(5)?,
                        created_at_ms: row.get(6)?,
                        revoked_at_ms: row.get(7)?,
                    })
                })?;
                Ok(rows.collect::<Result<_, _>>()?)
            })
            .unwrap()
    }

    #[test]
    fn a_store_can_be_shared_between_threads() {
        fn shared<T: Send + Sync>() {}
        shared::<ServerStore>();
    }

    #[test]
    fn a_new_database_is_at_the_newest_schema_and_has_no_identity() {
        let store = in_memory(&Arc::new(FixedClock::at(T0)));
        assert_eq!(store.schema_version(), 4);
        assert_eq!(store.migrations_applied(), 4);
        assert_eq!(store.identity().unwrap(), None);
        assert!(sessions(&store).is_empty());
    }

    #[test]
    fn first_start_creates_the_id_the_key_and_the_local_session_together() {
        let store = in_memory(&Arc::new(FixedClock::at(T0)));
        let (identity, token) = store.ensure_identity("desk").unwrap();
        let token = token.unwrap();

        // T0 is 0x019a_7afd_1e00 milliseconds. The counting source gives the
        // key bytes 00 to 1f, then the ID bytes 20 to 29.
        assert_eq!(
            identity.server_id.to_string(),
            "019a7afd-1e00-7021-a223-242526272829"
        );
        assert_eq!(identity.name, "desk");
        assert_eq!(identity.role, Role::Unassigned);
        assert_eq!(identity.library_id, None);
        assert_eq!(identity.created_at_ms, T0);
        assert_eq!(store.identity().unwrap(), Some(identity));

        assert_eq!(
            sessions(&store),
            [Session {
                subject_kind: "person".to_owned(),
                label: "local owner".to_owned(),
                kind: "full".to_owned(),
                token_hash: token.hash().to_vec(),
                has_listener: false,
                has_media_token: false,
                created_at_ms: T0,
                revoked_at_ms: None,
            }]
        );
        assert!(store.is_local_credential(&token).unwrap());
    }

    #[test]
    fn the_stored_key_pair_signs_and_verifies() {
        let store = in_memory(&Arc::new(FixedClock::at(T0)));
        let (identity, _) = store.ensure_identity("desk").unwrap();
        let (signing_key, role, library): ([u8; 32], String, Option<Vec<u8>>) = store
            .reader
            .call(|connection| {
                Ok(connection.query_row(
                    "SELECT signing_key, role, library_id FROM server_identity",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )?)
            })
            .unwrap();
        let signature = ed25519_dalek::SigningKey::from_bytes(&signing_key).sign(b"nonce");
        VerifyingKey::from_bytes(&identity.public_key)
            .unwrap()
            .verify(b"nonce", &signature)
            .unwrap();
        // A new server claims no library.
        assert_eq!((role.as_str(), library), ("unassigned", None));
    }

    #[test]
    fn the_token_itself_is_nowhere_in_the_database() {
        let store = in_memory(&Arc::new(FixedClock::at(T0)));
        let (_, token) = store.ensure_identity("desk").unwrap();
        let token = token.unwrap();
        let [session] = sessions(&store).try_into().unwrap();
        assert_eq!(session.token_hash.len(), 32);
        assert_ne!(session.token_hash, token.expose().as_bytes());

        let holding_it: i64 = store
            .reader
            .call(|connection| {
                Ok(connection.query_row(
                    "SELECT (SELECT count(*) FROM session
                             WHERE instr(hex(token_hash), upper(?1)) OR label = ?1)
                          + (SELECT count(*) FROM server_setting WHERE instr(value_json, ?1))
                          + (SELECT count(*) FROM server_identity WHERE name = ?1)",
                    [token.expose()],
                    |row| row.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(holding_it, 0);
    }

    #[test]
    fn a_second_call_changes_nothing_and_gives_no_token() {
        let clock = Arc::new(FixedClock::at(T0));
        let store = in_memory(&clock);
        let (first, token) = store.ensure_identity("desk").unwrap();
        let before = sessions(&store);

        clock.set(T0 + 60_000);
        let (second, none) = store.ensure_identity("another name").unwrap();
        assert_eq!(second, first);
        assert!(none.is_none());
        assert_eq!(sessions(&store), before);
        assert!(store.is_local_credential(&token.unwrap()).unwrap());
    }

    #[test]
    fn renaming_changes_the_name_and_nothing_else() {
        let store = in_memory(&Arc::new(FixedClock::at(T0)));
        assert_eq!(store.rename("too early").unwrap(), None);

        let (first, token) = store.ensure_identity("desk").unwrap();
        let renamed = store.rename("attic").unwrap().unwrap();
        assert_eq!(
            renamed,
            ServerIdentity {
                name: "attic".to_owned(),
                ..first
            }
        );
        assert_eq!(store.identity().unwrap(), Some(renamed));
        assert!(store.is_local_credential(&token.unwrap()).unwrap());
    }

    #[test]
    fn the_role_is_read_as_the_database_holds_it() {
        let store = in_memory(&Arc::new(FixedClock::at(T0)));
        store.ensure_identity("desk").unwrap();
        for (stored, role) in [("hub", Role::Hub), ("satellite", Role::Satellite)] {
            store
                .writer
                .call(move |connection| {
                    Ok(connection.execute("UPDATE server_identity SET role = ?1", [stored])?)
                })
                .unwrap();
            assert_eq!(store.identity().unwrap().unwrap().role, role);
        }
    }

    #[test]
    fn claiming_a_library_makes_the_server_its_hub_and_changes_nothing_else() {
        let store = in_memory(&Arc::new(FixedClock::at(T0)));
        let library = Id::from_bytes([0x1b; 16]);
        assert_eq!(store.claim_library(library).unwrap(), None);

        let (first, token) = store.ensure_identity("desk").unwrap();
        let claimed = store.claim_library(library).unwrap().unwrap();
        assert_eq!(
            claimed,
            ServerIdentity {
                role: Role::Hub,
                library_id: Some(library),
                ..first
            }
        );
        assert_eq!(store.identity().unwrap(), Some(claimed.clone()));
        assert!(store.is_local_credential(&token.unwrap()).unwrap());
        // Claiming it again is the same claim.
        assert_eq!(store.claim_library(library).unwrap(), Some(claimed));
    }

    #[test]
    fn the_local_session_is_the_one_the_credential_holds_now() {
        let store = in_memory(&Arc::new(FixedClock::at(T0)));
        assert_eq!(store.local_session().unwrap(), None);
        store.ensure_identity("desk").unwrap();
        // The counting source gives the key 00 to 1f, the server's ID 20
        // to 29, the token 2a to 49 and the session's ID 4a to 53.
        let first = store.local_session().unwrap().unwrap();
        assert_eq!(first.to_string(), "019a7afd-1e00-7a4b-8c4d-4e4f50515253");

        store.reissue_local_credential().unwrap();
        let second = store.local_session().unwrap().unwrap();
        assert_ne!(second, first);
    }

    /// A `server.db` as the first migration left it, with an identity and
    /// a session in it.
    const SAVED_0001: &str = include_str!("../saved/server/0001.sql");

    #[test]
    fn a_database_saved_at_the_first_migration_is_taken_forward_with_what_it_holds() {
        let scratch = Scratch::new();
        let path = scratch.path().join("server.db");
        Connection::open(&path)
            .unwrap()
            .execute_batch(SAVED_0001)
            .unwrap();

        let store = on_disk(&path).unwrap();
        assert_eq!(store.migrations_applied(), 3);
        assert_eq!(store.schema_version(), 4);
        // Who the server is, and the credential it issued, are as they
        // were.
        let identity = store.identity().unwrap().unwrap();
        assert_eq!(
            identity.server_id.to_string(),
            "019a7afd-1e00-7021-a223-242526272829"
        );
        assert_eq!(
            (identity.name.as_str(), identity.role, identity.library_id),
            ("desk", Role::Unassigned, None)
        );
        let token =
            SessionToken::new("2a2b2c2d2e2f303132333435363738393a3b3c3d3e3f40414243444546474849");
        assert!(store.is_local_credential(&token).unwrap());
        assert_eq!(sessions(&store).len(), 1);

        // What the second migration adds is there to be used, and a job
        // may name the session that was there before it.
        store
            .writer
            .call(|connection| {
                Ok(connection.execute_batch(
                    "INSERT INTO root (name, abs_path, created_at_ms)
                     VALUES ('main', '/srv/invented', 0);
                     INSERT INTO job (public_id, kind, params_json, created_by_session_id, created_at_ms)
                     VALUES (zeroblob(16), 'scan', '{}', 1, 0);
                     INSERT INTO job_item (job_id, item_ref, state) VALUES (1, 'a.flac', 'succeeded');
                     INSERT INTO command_receipt
                       (command_id, request_fingerprint, method, result_json, created_at_ms)
                     VALUES (zeroblob(16), x'f0', 'queue.clear', '{}', 0);
                     INSERT INTO outbox (kind, dedupe_key, next_attempt_at_ms, created_at_ms)
                     VALUES ('history.ingest', x'01', 0, 0);",
                )?)
            })
            .unwrap();
        store.close().unwrap();

        let store = on_disk(&path).unwrap();
        assert_eq!(store.migrations_applied(), 0);
        assert_eq!(store.identity().unwrap(), Some(identity));
    }

    #[test]
    fn the_saved_database_is_one_the_first_migration_made() {
        let mut saved = Connection::open_in_memory().unwrap();
        saved.execute_batch(SAVED_0001).unwrap();
        let mut made = Connection::open_in_memory().unwrap();
        migrate::run(
            &mut made,
            migrate::SERVER.get(..1).unwrap(),
            &FixedClock::at(T0),
        )
        .unwrap();
        let schema = |connection: &Connection| -> Vec<String> {
            connection
                .prepare("SELECT sql FROM sqlite_schema WHERE sql IS NOT NULL ORDER BY name")
                .unwrap()
                .query_map([], |row| row.get(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        };
        assert_eq!(schema(&saved), schema(&made));
        let ledger = |connection: &Connection| -> (i64, String, Vec<u8>) {
            connection
                .query_row(
                    "SELECT version, name, checksum FROM schema_migration",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .unwrap()
        };
        assert_eq!(ledger(&saved), ledger(&made));
        // Running the same first migration over it changes nothing: it is
        // at that schema already.
        let again = migrate::run(
            &mut saved,
            migrate::SERVER.get(..1).unwrap(),
            &FixedClock::at(T0),
        )
        .unwrap();
        assert_eq!((again.applied, again.version), (0, 1));
    }

    #[test]
    fn a_token_the_server_did_not_issue_is_not_the_local_credential() {
        let store = in_memory(&Arc::new(FixedClock::at(T0)));
        assert!(
            !store
                .is_local_credential(&SessionToken::new("made up"))
                .unwrap()
        );
        store.ensure_identity("desk").unwrap();
        assert!(
            !store
                .is_local_credential(&SessionToken::new("made up"))
                .unwrap()
        );
        assert!(!store.is_local_credential(&SessionToken::new("")).unwrap());
    }

    #[test]
    fn reissuing_revokes_the_old_session_and_the_new_token_works() {
        let clock = Arc::new(FixedClock::at(T0));
        let store = in_memory(&clock);
        let (identity, old) = store.ensure_identity("desk").unwrap();
        let old = old.unwrap();

        clock.set(T0 + 5_000);
        let new = store.reissue_local_credential().unwrap();
        assert_ne!(new.expose(), old.expose());
        assert!(store.is_local_credential(&new).unwrap());
        assert!(!store.is_local_credential(&old).unwrap());

        let all = sessions(&store);
        let revoked: Vec<_> = all.iter().map(|session| session.revoked_at_ms).collect();
        assert_eq!(revoked, [Some(T0 + 5_000), None]);
        assert_eq!(all.last().unwrap().token_hash, new.hash());
        // The identity is not part of it.
        assert_eq!(store.identity().unwrap(), Some(identity));
    }

    #[test]
    fn the_identity_and_the_credential_survive_a_restart() {
        let scratch = Scratch::new();
        let path = scratch.path().join("server.db");

        let store = on_disk(&path).unwrap();
        assert_eq!(store.migrations_applied(), 4);
        let (first, token) = store.ensure_identity("desk").unwrap();
        let token = token.unwrap();
        store.close().unwrap();
        assert!(!scratch.path().join("server.db-wal").exists());

        let store = on_disk(&path).unwrap();
        assert_eq!(store.migrations_applied(), 0);
        assert_eq!(store.schema_version(), 4);
        let (second, none) = store.ensure_identity("desk").unwrap();
        assert_eq!(second, first);
        assert!(none.is_none());
        assert!(store.is_local_credential(&token).unwrap());
        assert_eq!(sessions(&store).len(), 1);
    }

    #[test]
    fn the_schema_rules_hold_one_identity_and_closed_value_lists() {
        let store = in_memory(&Arc::new(FixedClock::at(T0)));
        store.ensure_identity("desk").unwrap();
        for refused in [
            "INSERT INTO server_identity (id, server_id, signing_key, public_key, name, created_at_ms)
             VALUES (2, zeroblob(16), zeroblob(32), zeroblob(32), 'second', 0)",
            "INSERT INTO session (public_id, subject_kind, label, kind, token_hash, created_at_ms)
             VALUES (zeroblob(16), 'person', 'x', 'admin', x'01', 0)",
            "INSERT INTO session (public_id, subject_kind, label, kind, token_hash, created_at_ms)
             VALUES (zeroblob(16), 'robot', 'x', 'full', x'01', 0)",
            "INSERT INTO session (public_id, subject_kind, label, kind, token_hash, created_at_ms)
             VALUES (zeroblob(16), 'person', 'x', 'full', x'01', 'a string')",
        ] {
            let result = store
                .writer
                .call(move |connection| Ok(connection.execute(refused, [])?));
            assert!(result.is_err(), "{refused}");
        }
    }

    /// Changes a closed database behind the program's back.
    fn tamper(path: &Path, sql: &str) {
        Connection::open(path).unwrap().execute_batch(sql).unwrap();
    }

    #[test]
    fn a_database_whose_migration_has_changed_is_not_opened() {
        let scratch = Scratch::new();
        let path = scratch.path().join("server.db");
        on_disk(&path).unwrap().close().unwrap();
        tamper(
            &path,
            "UPDATE schema_migration SET checksum = x'00' WHERE version = 1",
        );
        let refused = on_disk(&path);
        assert!(
            matches!(refused, Err(Error::ChangedMigration { version: 1 })),
            "{refused:?}"
        );
    }

    #[test]
    fn a_database_from_a_newer_program_is_not_opened() {
        let scratch = Scratch::new();
        let path = scratch.path().join("server.db");
        on_disk(&path).unwrap().close().unwrap();
        tamper(
            &path,
            "INSERT INTO schema_migration VALUES (5, '0005_from_the_future', x'00', 0)",
        );
        let refused = on_disk(&path);
        assert!(
            matches!(refused, Err(Error::NewerSchema { found: 5, known: 4 })),
            "{refused:?}"
        );
    }
}
