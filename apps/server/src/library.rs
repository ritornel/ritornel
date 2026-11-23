//! The library this server holds, found when the server starts. See
//! "Server identity and hub role" in docs/internals/linking.md and
//! "Storage split" in docs/internals/server.md.
//!
//! A server's role is its own record, in `server.db`. Which server may
//! write a library is the library's record, the hub pointer in
//! `library.db`, and the pointer decides.

use std::sync::Arc;

use ritornel_engine::{Clock, Entropy, LibraryStore, Role, ServerIdentity, ServerStore};
use tracing::{info, warn};

use crate::Error;
use crate::data_dir::DataDir;

/// Opens the library this server holds, if it holds one, and returns it
/// with the server's identity as it then is.
///
/// - A server that is a hub opens `library.db`, and does not start without
///   it: the file must be there, be the library `server.db` names, and
///   name this server in its hub pointer.
/// - A server with no role runs with no library. If `library.db` is there
///   and its pointer names this server, creating it was cut short between
///   the two files, and this finishes it: the server becomes its hub.
pub(crate) fn open(
    dir: &DataDir,
    store: &ServerStore,
    identity: ServerIdentity,
    clock: &Arc<dyn Clock>,
    entropy: &Arc<dyn Entropy>,
) -> Result<(ServerIdentity, Option<LibraryStore>), Error> {
    use ritornel_engine::Error as Engine;
    let path = dir.library_db();
    let opened = || LibraryStore::open(&path, identity.server_id, clock.clone(), entropy.clone());
    let (identity, library) = match identity.role {
        Role::Hub => {
            // Checked before the library is opened for writing, which would
            // set its journal mode and migrate it: a library refused here is
            // left exactly as it was.
            let found = LibraryStore::check(&path, identity.server_id)
                .map_err(Error::Library)?
                .library_id;
            if identity.library_id != Some(found) {
                return Err(Error::AnotherLibrary {
                    claimed: identity.library_id,
                    found,
                });
            }
            let library = opened().map_err(Error::Library)?;
            // And again, in case the file was replaced in between.
            if library.library_id() != found {
                return Err(Error::AnotherLibrary {
                    claimed: identity.library_id,
                    found: library.library_id(),
                });
            }
            (identity, library)
        }
        Role::Unassigned => match opened() {
            Ok(library) => {
                let claimed = store.claim_library(library.library_id())?;
                warn!(
                    library_id = %library.library_id(),
                    "library.db names this server as its hub and server.db did not say so: creating the library was \
                     cut short, and is now finished"
                );
                (claimed.unwrap_or(identity), library)
            }
            Err(Engine::NoLibrary) => return Ok((identity, None)),
            Err(error @ Engine::NotTheHub { .. }) => {
                warn!(%error, "library.db is not this server's to write, and is left as it is");
                return Ok((identity, None));
            }
            Err(error) => return Err(Error::Library(error)),
        },
        Role::Satellite => return Ok((identity, None)),
    };
    let meta = library.meta().map_err(Error::Library)?;
    info!(
        library_id = %meta.library_id,
        schema_version = library.schema_version(),
        migrations_applied = library.migrations_applied(),
        hub_epoch = meta.hub_epoch,
        revision = meta.revision,
        "library.db is open"
    );
    Ok((identity, Some(library)))
}

/// Gives this server's row in the library's registry of servers the name
/// `server.db` gives it, after a rename at start or while it runs. Nothing
/// is written when the names already agree. The two files cannot commit
/// together, so a registry left behind, as by a crash between them or a
/// write that failed, is put right at the next start or rename.
pub(crate) fn keep_registry_name(
    library: &LibraryStore,
    identity: &ServerIdentity,
) -> Result<(), ritornel_engine::Error> {
    let known = library.server_name(identity.server_id)?;
    if known.is_some_and(|known| known != identity.name) {
        library.register_server(identity.server_id, &identity.name)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use ritornel_client::Client;
    use ritornel_client::contract::{
        CONTRACT_VERSION, ClientKind, ErrorCode, HelloParams, Id, ServerRole, ServerStatus,
        SessionKind,
    };
    use ritornel_engine::{OsEntropy, SystemClock, WriteRequest};

    use super::*;
    use crate::testing::Scratch;
    use crate::{HeldDir, Listen, Server};

    async fn start(data_dir: &Path) -> Result<Server, Error> {
        Server::start(
            HeldDir::take(data_dir.to_owned())?,
            Listen::on_port(0),
            Arc::new(SystemClock),
            Arc::new(OsEntropy),
        )
        .await
    }

    /// A client of the server with a full session, that has said hello.
    async fn client(server: &Server) -> Client {
        let (outgoing, incoming) = server.connect_in_memory(SessionKind::Full);
        let client = Client::over_memory(outgoing, incoming);
        client
            .hello(&HelloParams {
                name: "library-test".to_owned(),
                kind: ClientKind::Cli,
                version: "1".to_owned(),
                contract_version: CONTRACT_VERSION,
            })
            .await
            .unwrap();
        client
    }

    async fn status(server: &Server) -> ServerStatus {
        client(server).await.status().await.unwrap()
    }

    fn key(byte: u8) -> Id {
        Id::from_bytes([byte; 16])
    }

    /// Puts a library whose hub is `hub` at `path`, as that server would
    /// have made it, and returns the library's ID.
    fn library_of(hub: Id, path: &Path) -> Id {
        let hub = ServerIdentity {
            server_id: hub,
            public_key: [0; 32],
            name: "its hub".to_owned(),
            role: Role::Unassigned,
            library_id: None,
            created_at_ms: 0,
        };
        let request = WriteRequest {
            method: "library.create",
            command: None,
            client: None,
        };
        let (library, _) = LibraryStore::create(
            path,
            &hub,
            request,
            |_| String::new(),
            Arc::new(SystemClock),
            Arc::new(OsEntropy),
        )
        .unwrap();
        let library_id = library.library_id();
        library.close().unwrap();
        library_id
    }

    #[tokio::test]
    async fn a_hub_opens_its_library_when_it_starts_and_it_is_the_same_library() {
        let scratch = Scratch::new();
        let data_dir = scratch.path().join("data");

        let server = start(&data_dir).await.unwrap();
        let before = status(&server).await;
        assert_eq!(
            (before.role, before.library, before.versions.library_schema),
            (ServerRole::Unassigned, None, None)
        );
        let created = client(&server)
            .await
            .create_library(key(0xc0))
            .await
            .unwrap();
        server.stop().await.unwrap();
        // A clean stop leaves no log beside either database.
        assert!(!data_dir.join("library.db-wal").exists());
        assert!(!data_dir.join("server.db-wal").exists());

        let server = start(&data_dir).await.unwrap();
        let after = status(&server).await;
        assert_eq!(after.role, ServerRole::Hub);
        assert_eq!(
            after.versions.library_schema,
            Some(ritornel_engine::LIBRARY_SCHEMA_VERSION)
        );
        let library = after.library.unwrap();
        assert_eq!(
            (library.library_id, library.revision),
            (created.library_id, 1)
        );
        // The command that made it is still answered as it was, and the
        // server will not make another.
        let client = client(&server).await;
        assert_eq!(client.create_library(key(0xc0)).await.unwrap(), created);
        let Err(ritornel_client::Error::Call(refused)) = client.create_library(key(0xc1)).await
        else {
            panic!("a second library was created");
        };
        assert_eq!(refused.kind(), ErrorCode::Conflict);
        server.stop().await.unwrap();
    }

    #[tokio::test]
    async fn a_registry_left_behind_by_a_rename_catches_up_at_the_next_start() {
        let scratch = Scratch::new();
        let data_dir = scratch.path().join("data");
        let server = start(&data_dir).await.unwrap();
        let this = server.server_id();
        client(&server)
            .await
            .create_library(key(0xc0))
            .await
            .unwrap();
        let name = server.name();
        server.stop().await.unwrap();
        let library_db = || rusqlite::Connection::open(data_dir.join("library.db")).unwrap();
        let registry = || -> String {
            library_db()
                .query_row(
                    "SELECT name FROM server WHERE public_id = ?1",
                    [this.as_bytes()],
                    |row| row.get(0),
                )
                .unwrap()
        };
        assert_eq!(registry(), name);

        // As a crash between the two files' commits would leave it.
        library_db()
            .execute(
                "UPDATE server SET name = 'Old Name' WHERE public_id = ?1",
                [this.as_bytes()],
            )
            .unwrap();
        let server = start(&data_dir).await.unwrap();
        assert_eq!(server.name(), name);
        server.stop().await.unwrap();
        assert_eq!(registry(), name);
    }

    #[tokio::test]
    async fn a_hub_does_not_start_on_a_library_whose_pointer_names_another_server() {
        let scratch = Scratch::new();
        let data_dir = scratch.path().join("data");
        let server = start(&data_dir).await.unwrap();
        let this = server.server_id();
        client(&server)
            .await
            .create_library(key(0xc0))
            .await
            .unwrap();
        server.stop().await.unwrap();

        // The library that is there now is one another server holds.
        let library_db = data_dir.join("library.db");
        std::fs::remove_file(&library_db).unwrap();
        let library = library_of(key(0x22), &library_db);
        let before = std::fs::read(&library_db).unwrap();

        let refused = start(&data_dir).await.unwrap_err();
        assert_eq!(
            refused.to_string(),
            format!(
                "library.db: library {library} belongs to server {}, and this server is {this}: \
                 only the server its hub pointer names may write it",
                key(0x22)
            )
        );
        assert_eq!(refused.exit_code(), crate::EXIT_FAILED);
        // It was not written, and the directory is free again.
        assert_eq!(std::fs::read(&library_db).unwrap(), before);
        assert!(!data_dir.join("server.json").exists());
        HeldDir::take(data_dir).unwrap();
    }

    #[tokio::test]
    async fn a_hub_does_not_start_without_its_library_or_on_another_library() {
        let scratch = Scratch::new();
        let data_dir = scratch.path().join("data");
        let server = start(&data_dir).await.unwrap();
        let this = server.server_id();
        let created = client(&server)
            .await
            .create_library(key(0xc0))
            .await
            .unwrap();
        server.stop().await.unwrap();

        let library_db = data_dir.join("library.db");
        std::fs::remove_file(&library_db).unwrap();
        let refused = start(&data_dir).await.unwrap_err();
        assert_eq!(refused.to_string(), "library.db: there is no library here");
        // Looking for it did not make one.
        assert!(!library_db.exists());

        // A library this server may write, and not the one it holds.
        let other = library_of(this, &library_db);
        let refused = start(&data_dir).await.unwrap_err();
        assert_eq!(
            refused.to_string(),
            format!(
                "library.db: this is library {other}, and server.db says this server holds \
                 library {}",
                created.library_id
            )
        );
    }

    /// Puts the library at `path` back as an older build would have left
    /// it: in rollback-journal mode, and a migration behind.
    fn as_an_older_build_left_it(path: &Path) {
        rusqlite::Connection::open(path)
            .unwrap()
            .execute_batch(
                "PRAGMA journal_mode = DELETE;
                 DELETE FROM schema_migration
                 WHERE version = (SELECT max(version) FROM schema_migration);",
            )
            .unwrap();
    }

    /// Asserts that the library at `path` holds `before`, and that nothing
    /// was opened for writing beside it.
    fn untouched(path: &Path, before: &[u8]) {
        assert!(
            std::fs::read(path).unwrap() == before,
            "library.db was written"
        );
        for log in ["library.db-wal", "library.db-shm", "library.db-journal"] {
            assert!(!path.with_file_name(log).exists(), "{log}");
        }
    }

    #[tokio::test]
    async fn a_hub_refuses_another_library_before_writing_anything_in_it() {
        let scratch = Scratch::new();
        let data_dir = scratch.path().join("data");
        let server = start(&data_dir).await.unwrap();
        let this = server.server_id();
        client(&server)
            .await
            .create_library(key(0xc0))
            .await
            .unwrap();
        server.stop().await.unwrap();

        // A library this server may write, and not the one it holds, left
        // by an older build.
        let library_db = data_dir.join("library.db");
        std::fs::remove_file(&library_db).unwrap();
        library_of(this, &library_db);
        as_an_older_build_left_it(&library_db);
        let before = std::fs::read(&library_db).unwrap();

        let refused = start(&data_dir).await.unwrap_err();
        assert!(matches!(refused, Error::AnotherLibrary { .. }), "{refused}");
        // Neither migrated nor switched to write-ahead logging.
        untouched(&library_db, &before);
    }

    #[tokio::test]
    async fn a_creation_cut_short_between_the_two_files_is_finished_at_the_next_start() {
        let scratch = Scratch::new();
        let data_dir = scratch.path().join("data");
        let server = start(&data_dir).await.unwrap();
        let this = server.server_id();
        server.stop().await.unwrap();

        // library.db has the library, with this server as its hub, and
        // server.db was never told.
        let library = library_of(this, &data_dir.join("library.db"));

        let server = start(&data_dir).await.unwrap();
        let found = status(&server).await;
        assert_eq!(found.role, ServerRole::Hub);
        assert_eq!(found.library.unwrap().library_id, library);
        server.stop().await.unwrap();

        // It is this server's own record now, and the next start is an
        // ordinary one.
        let server = start(&data_dir).await.unwrap();
        assert_eq!(status(&server).await.library.unwrap().library_id, library);
        server.stop().await.unwrap();
    }

    #[tokio::test]
    async fn a_server_with_no_role_leaves_a_library_that_is_not_its_own_alone() {
        let scratch = Scratch::new();
        let data_dir = scratch.path().join("data");
        std::fs::create_dir_all(&data_dir).unwrap();
        let library_db = data_dir.join("library.db");
        library_of(key(0x22), &library_db);
        as_an_older_build_left_it(&library_db);
        let before = std::fs::read(&library_db).unwrap();

        let server = start(&data_dir).await.unwrap();
        let found = status(&server).await;
        assert_eq!((found.role, found.library), (ServerRole::Unassigned, None));
        server.stop().await.unwrap();
        untouched(&library_db, &before);
    }
}
