//! `library.db`: the library, which follows the hub role from machine to
//! machine. One server may write it, the one its hub pointer names. See
//! "Storage split" and "Commands, receipts, and revisions" in
//! docs/internals/server.md, and "Server identity and hub role" in
//! docs/internals/linking.md.
//!
//! There is one way to change a library: [`LibraryStore::write`]. A write
//! is a closure that runs in one transaction. The transaction takes the
//! next library revision, which the closure stamps on every row it inserts
//! or changes, logs one `library_revision` row and, for a command, records
//! the receipt that makes a retry harmless. A transaction that fails leaves
//! none of it.

use std::path::Path;
use std::sync::{Arc, PoisonError, RwLock};

use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior};
use sha2::{Digest, Sha256};

use crate::migrate::{self, Migrated};
use crate::sqlite::{self, Location, Reader, Writer};
use crate::{Clock, Entropy, Error, Id, ServerIdentity, new_id};

/// The classes of metadata source and their ranks, highest first: what a
/// new library starts with. See "Catalog projection and displayed metadata"
/// in docs/internals/identity.md.
const METADATA_SOURCES: [(&str, i64); 6] = [
    ("user", 60),
    ("musicbrainz", 50),
    ("beets", 40),
    ("file_tags", 30),
    ("device", 20),
    ("inferred", 10),
];

/// The name of the listener a new library starts with. Display text only.
const DEFAULT_LISTENER: &str = "owner";

/// What tells one request from another that reuses its command ID: the
/// SHA-256 of the method's name and the params.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fingerprint([u8; 32]);

impl Fingerprint {
    /// The fingerprint of a call of `method` with these params, as bytes
    /// that are the same whenever the request is the same.
    pub fn of(method: &str, params: &[u8]) -> Self {
        let mut hash = Sha256::new();
        hash.update(method.as_bytes());
        // No method name holds a zero byte, so the two parts cannot run
        // together.
        hash.update([0]);
        hash.update(params);
        Self(hash.finalize().into())
    }

    /// The fingerprint as it is stored.
    pub(crate) fn bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// What makes a write a command: the ID its client minted for it, and the
/// fingerprint of the request. See "Commands, receipts, and revisions" in
/// docs/internals/server.md.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Command {
    /// The command ID. A job step derives its own from the job's ID and
    /// the step number.
    pub id: Id,
    /// What the request was.
    pub fingerprint: Fingerprint,
}

/// What kind of thing holds a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubjectKind {
    /// A person's client.
    Person,
    /// An agent.
    Agent,
    /// Another server.
    Server,
}

impl SubjectKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Person => "person",
            Self::Agent => "agent",
            Self::Server => "server",
        }
    }
}

/// The client a write was made for: the session, by its public ID in the
/// handling server's `server.db`, and what kind of thing holds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Client {
    /// The session's public ID.
    pub session: Id,
    /// The session's subject kind.
    pub kind: SubjectKind,
}

/// Which call a write is, and for whom: what its `library_revision` row
/// records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteRequest {
    /// The contract call or the internal writer, such as `scan.step`.
    pub method: &'static str,
    /// The command, for a write that has a receipt. A write with none,
    /// such as a history ingest, is safe to repeat by other means.
    pub command: Option<Command>,
    /// The client, or `None` for the server's own work.
    pub client: Option<Client>,
}

/// What became of a write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Written {
    /// The library revision the write was committed at.
    pub revision: i64,
    /// The result, as the JSON the write returned.
    pub result_json: String,
    /// Whether this is the stored result of a command that had already
    /// been carried out. Nothing was written this time.
    pub replayed: bool,
}

/// A write that has been committed: what a [`CommitHook`] is told.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Committed {
    /// The library revision it was committed at.
    pub revision: i64,
    /// The contract call or the internal writer it was, as its
    /// [`WriteRequest`] names it.
    pub method: &'static str,
}

/// Told of every write after it commits, on the writer thread and in the
/// order of the commits. It is how the server tells clients that the
/// library changed (docs/internals/server.md, "Commands, receipts, and
/// revisions": events are hints sent after commit). It must be quick: the next
/// write waits for it.
pub type CommitHook = Arc<dyn Fn(Committed) + Send + Sync>;

/// What `library_meta` says: which library this is, which server may write
/// it, and how far it has come.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LibraryMeta {
    /// The library's ID.
    pub library_id: Id,
    /// The server the hub pointer names. `None` is a released library,
    /// which no server may write.
    pub hub: Option<Id>,
    /// Raised by every release and adopt.
    pub hub_epoch: i64,
    /// The library revision: the number of the last write.
    pub revision: i64,
}

/// One library write in progress: the transaction, and the revision to
/// stamp on every row it inserts or changes.
pub(crate) struct Write<'a> {
    transaction: &'a Transaction<'a>,
    rev: i64,
    now_ms: i64,
    entropy: &'a dyn Entropy,
}

impl Write<'_> {
    /// The connection, inside the write's transaction.
    pub(crate) fn connection(&self) -> &Connection {
        self.transaction
    }

    /// The revision this write is: what goes in `rev`.
    pub(crate) fn rev(&self) -> i64 {
        self.rev
    }

    /// A new public ID.
    pub(crate) fn new_id(&self) -> Result<Id, Error> {
        new_id(self.now_ms, self.entropy)
    }

    /// When the write began, as Unix milliseconds: the one time every row
    /// of the write is stamped with.
    pub(crate) fn now_ms(&self) -> i64 {
        self.now_ms
    }
}

/// An open `library.db` that this server may write: one write connection on
/// a thread of its own, and a read-only connection for reads.
#[derive(Debug)]
pub struct LibraryStore {
    /// Where the database is, for a read that wants a connection of its
    /// own: an export.
    location: Location,
    writer: Writer,
    reader: Reader,
    clock: Arc<dyn Clock>,
    entropy: Arc<dyn Entropy>,
    migrated: Migrated,
    library_id: Id,
    /// This server, which the hub pointer named when the library was
    /// opened. Every write checks that it still does.
    hub: Id,
    /// Told of each write once it has committed.
    on_commit: OnCommit,
}

/// The hook a store tells of each commit, if it has one.
#[derive(Default)]
struct OnCommit(RwLock<Option<CommitHook>>);

impl std::fmt::Debug for OnCommit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OnCommit")
    }
}

impl LibraryStore {
    /// The hub's clock now, read by the caller and not by the writer.
    pub(crate) fn clock_now_ms(&self) -> i64 {
        self.clock.now_ms()
    }

    /// Creates the library in the file at `path`, with `server` as its hub,
    /// and opens it. The file may be missing or hold no library yet. One
    /// that holds a library is refused with [`Error::LibraryExists`] and
    /// left as it was.
    ///
    /// Creating is the library's first write, revision 1: one transaction
    /// holds the library's ID and hub pointer, the registry row for
    /// `server`, the default listener, the metadata sources with their
    /// ranks, this server's player endpoint and the endpoint for
    /// corrections, and the receipt of `request`. `receipt` is given the
    /// library as it then is and returns the result to store and to answer
    /// with.
    pub fn create<R>(
        path: &Path,
        server: &ServerIdentity,
        request: WriteRequest,
        receipt: R,
        clock: Arc<dyn Clock>,
        entropy: Arc<dyn Entropy>,
    ) -> Result<(Self, Written), Error>
    where
        R: FnOnce(&LibraryMeta) -> String + Send + 'static,
    {
        Self::create_at(
            Location::File(path.to_owned()),
            server,
            request,
            receipt,
            clock,
            entropy,
        )
    }

    /// Creates a library in a new database in memory, for tests.
    pub fn create_in_memory<R>(
        server: &ServerIdentity,
        request: WriteRequest,
        receipt: R,
        clock: Arc<dyn Clock>,
        entropy: Arc<dyn Entropy>,
    ) -> Result<(Self, Written), Error>
    where
        R: FnOnce(&LibraryMeta) -> String + Send + 'static,
    {
        Self::create_at(Location::memory(), server, request, receipt, clock, entropy)
    }

    fn create_at<R>(
        location: Location,
        server: &ServerIdentity,
        request: WriteRequest,
        receipt: R,
        clock: Arc<dyn Clock>,
        entropy: Arc<dyn Entropy>,
    ) -> Result<(Self, Written), Error>
    where
        R: FnOnce(&LibraryMeta) -> String + Send + 'static,
    {
        sqlite::assert_version()?;
        let writer = Writer::start(&location, "library.db writer")?;
        let (hub, name) = (server.server_id, server.name.clone());
        let created = {
            let (clock, entropy) = (Arc::clone(&clock), Arc::clone(&entropy));
            writer.call(move |connection| {
                // Before anything is changed: a library that is there is
                // not this call's to migrate.
                if read_meta(connection)?.is_some() {
                    return Err(Error::LibraryExists);
                }
                let migrated = migrate::run(connection, migrate::LIBRARY, &*clock)?;
                let now_ms = clock.now_ms();
                let library_id = new_id(now_ms, &*entropy)?;
                let transaction =
                    connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
                // The registry row and the pointer are what make this
                // server the one that may write, so they go in ahead of
                // the write that every other row of revision 1 is part of.
                // The pointer's counter starts at 0 and that write takes 1.
                transaction.execute(
                    "INSERT INTO server (public_id, name, created_at_ms, rev) VALUES (?1, ?2, ?3, 1)",
                    (hub.as_bytes(), &name, now_ms),
                )?;
                let hub_row = transaction.last_insert_rowid();
                transaction.execute(
                    "INSERT INTO library_meta
                       (id, library_id, hub_server_id, hub_epoch, rev, created_at_ms)
                     VALUES (1, ?1, ?2, 1, 0, ?3)",
                    (library_id.as_bytes(), hub_row, now_ms),
                )?;
                let written = write_in(&transaction, hub, &request, now_ms, &*entropy, |write| {
                    first_rows(write, hub_row)?;
                    Ok(receipt(&LibraryMeta {
                        library_id,
                        hub: Some(hub),
                        hub_epoch: 1,
                        revision: write.rev(),
                    }))
                })?;
                transaction.commit()?;
                Ok((migrated, library_id, written))
            })?
        };
        let (migrated, library_id, written) = created;
        let reader = Reader::open(&location)?;
        let store = Self {
            location,
            writer,
            reader,
            clock,
            entropy,
            migrated,
            library_id,
            hub,
            on_commit: OnCommit::default(),
        };
        Ok((store, written))
    }

    /// Opens the library at `path` for the server `server_id` to write, and
    /// brings its schema up to date.
    ///
    /// It refuses with [`Error::NotTheHub`] if the library's hub pointer is
    /// empty or names another server, and with [`Error::NoLibrary`] if the
    /// file is missing or holds no library. Either way the library is not
    /// migrated and nothing in it is changed. It also refuses what
    /// [`ServerStore::open`](crate::ServerStore::open) refuses.
    pub fn open(
        path: &Path,
        server_id: Id,
        clock: Arc<dyn Clock>,
        entropy: Arc<dyn Entropy>,
    ) -> Result<Self, Error> {
        // Starting the writer sets the journal mode, so the pointer is read
        // first on a connection that writes nothing.
        Self::check(path, server_id)?;
        let location = Location::File(path.to_owned());
        let writer = Writer::start(&location, "library.db writer")?;
        let (migrated, library_id) = {
            let clock = Arc::clone(&clock);
            writer.call(move |connection| {
                let meta = read_meta(connection)?.ok_or(Error::NoLibrary)?;
                if meta.hub != Some(server_id) {
                    return Err(not_the_hub(&meta, server_id));
                }
                let migrated = migrate::run(connection, migrate::LIBRARY, &*clock)?;
                Ok((migrated, meta.library_id))
            })?
        };
        let reader = Reader::open(&location)?;
        Ok(Self {
            location,
            writer,
            reader,
            clock,
            entropy,
            migrated,
            library_id,
            hub: server_id,
            on_commit: OnCommit::default(),
        })
    }

    /// What `library_meta` says in the library at `path`, if the server
    /// `server_id` may write it. It refuses with [`Error::NotTheHub`] and
    /// [`Error::NoLibrary`] as [`open`](Self::open) does, and reads on a
    /// read-only connection, so nothing in the file changes, its journal
    /// mode included.
    pub fn check(path: &Path, server_id: Id) -> Result<LibraryMeta, Error> {
        sqlite::assert_version()?;
        // SQLite would make the file, and a library is never made by
        // looking for one.
        if !path.try_exists().unwrap_or(true) {
            return Err(Error::NoLibrary);
        }
        let connection = Location::File(path.to_owned()).open(OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let meta = read_meta(&connection)?.ok_or(Error::NoLibrary)?;
        if meta.hub != Some(server_id) {
            return Err(not_the_hub(&meta, server_id));
        }
        Ok(meta)
    }

    /// The library's ID.
    pub fn library_id(&self) -> Id {
        self.library_id
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

    /// What `library_meta` says now.
    pub fn meta(&self) -> Result<LibraryMeta, Error> {
        self.reader
            .call(|connection| read_meta(connection)?.ok_or(Error::NoLibrary))
    }

    /// Answers a request to create a library here, where there is one
    /// already. If `request` is the command that created this library, sent
    /// again, the answer is the result it was given the first time. If it
    /// is that command's ID with other params, it is
    /// [`Error::CommandMismatch`]. Any other command is refused with
    /// [`Error::LibraryExists`]. Nothing is written in any of the three.
    pub fn created_by(&self, request: WriteRequest) -> Result<Written, Error> {
        self.write(request, |_| Err(Error::LibraryExists))
    }

    /// Changes the library: the only way there is.
    ///
    /// `work` runs on the writer thread in one transaction and returns the
    /// write's result as JSON. Before it runs, the transaction takes the
    /// next library revision, which `work` stamps on every row it inserts
    /// or changes. After it, the transaction logs the `library_revision`
    /// row and, for a command, the `library_command` receipt, and commits
    /// them with the rows. If anything fails, nothing is kept: no revision
    /// is used up, and a rejected command leaves no receipt.
    ///
    /// A command that was carried out before is not carried out again: the
    /// same ID with the same fingerprint returns the stored result, and
    /// with another fingerprint is [`Error::CommandMismatch`]. `work` runs
    /// in neither case.
    pub(crate) fn write<F>(&self, request: WriteRequest, work: F) -> Result<Written, Error>
    where
        F: FnOnce(&Write<'_>) -> Result<String, Error> + Send + 'static,
    {
        let (clock, entropy, hub) = (Arc::clone(&self.clock), Arc::clone(&self.entropy), self.hub);
        let on_commit = self
            .on_commit
            .0
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        self.writer.call(move |connection| {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let written = write_in(&transaction, hub, &request, clock.now_ms(), &*entropy, work)?;
            transaction.commit()?;
            // The one place a write is known to have committed. A replayed
            // command wrote nothing, so there is nothing to tell.
            if let Some(hook) = on_commit
                && !written.replayed
            {
                hook(Committed {
                    revision: written.revision,
                    method: request.method,
                });
            }
            Ok(written)
        })
    }

    /// Tells `hook` of every write from now on, after it commits. A second
    /// hook replaces the first.
    pub fn on_commit(&self, hook: CommitHook) {
        *self
            .on_commit
            .0
            .write()
            .unwrap_or_else(PoisonError::into_inner) = Some(hook);
    }

    /// Where the database is.
    pub(crate) fn location(&self) -> &Location {
        &self.location
    }

    /// The clock the store takes its times from.
    pub(crate) fn clock(&self) -> &Arc<dyn Clock> {
        &self.clock
    }

    /// Runs a short read on the read connection.
    pub(crate) fn read<T>(
        &self,
        work: impl FnOnce(&Connection) -> Result<T, Error>,
    ) -> Result<T, Error> {
        self.reader.call(work)
    }

    /// Runs a read of several queries on the read connection, in one read
    /// transaction, so that they all see the library as of one commit.
    pub(crate) fn read_snapshot<T>(
        &self,
        work: impl FnOnce(&Connection) -> Result<T, Error>,
    ) -> Result<T, Error> {
        self.reader
            .call(|connection| crate::sqlite::snapshot(connection, work))
    }

    /// The server whose library this is: this one.
    pub(crate) fn hub(&self) -> Id {
        self.hub
    }

    /// The stored result of a command carried out before, read without
    /// taking the writer: a call that checks the disk first answers a
    /// replay before it looks. The write still checks again.
    pub fn replay(&self, command: &Command) -> Result<Option<Written>, Error> {
        let command = *command;
        self.reader
            .call(move |connection| stored_receipt(connection, &command))
    }

    /// The stored result of the command with this ID, if it has been
    /// carried out. A job asks before it redoes a step that a crash may have
    /// cut off after its commit.
    pub(crate) fn command_result(&self, command: Id) -> Result<Option<String>, Error> {
        self.reader.call(|connection| {
            Ok(connection
                .query_row(
                    "SELECT result_json FROM library_command WHERE command_id = ?1",
                    [command.as_bytes()],
                    |row| row.get(0),
                )
                .optional()?)
        })
    }

    /// Closes the database after a clean stop: moves the write-ahead log
    /// into the file and empties it.
    pub fn close(self) -> Result<(), Error> {
        drop(self.reader);
        self.writer.close()
    }
}

/// One write, inside a transaction the caller commits. See
/// [`LibraryStore::write`].
/// The stored result of `command`, if it was carried out before: the same
/// ID with another fingerprint is [`Error::CommandMismatch`].
fn stored_receipt(connection: &Connection, command: &Command) -> Result<Option<Written>, Error> {
    let stored: Option<(Vec<u8>, String, i64)> = connection
        .query_row(
            "SELECT request_fingerprint, result_json, rev
             FROM library_command WHERE command_id = ?1",
            [command.id.as_bytes()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    match stored {
        None => Ok(None),
        Some((fingerprint, result_json, revision)) if fingerprint == command.fingerprint.0 => {
            Ok(Some(Written {
                revision,
                result_json,
                replayed: true,
            }))
        }
        Some(_) => Err(Error::CommandMismatch {
            command: command.id,
        }),
    }
}

fn write_in(
    transaction: &Transaction<'_>,
    hub: Id,
    request: &WriteRequest,
    now_ms: i64,
    entropy: &dyn Entropy,
    work: impl FnOnce(&Write<'_>) -> Result<String, Error>,
) -> Result<Written, Error> {
    if let Some(command) = &request.command
        && let Some(written) = stored_receipt(transaction, command)?
    {
        return Ok(written);
    }

    // The counter moves only while the pointer names this server, so no
    // write gets as far as a row in a library that is not this server's.
    let rev: Option<i64> = transaction
        .query_row(
            "UPDATE library_meta SET rev = rev + 1
             WHERE id = 1 AND hub_server_id = (SELECT id FROM server WHERE public_id = ?1)
             RETURNING rev",
            [hub.as_bytes()],
            |row| row.get(0),
        )
        .optional()?;
    let Some(rev) = rev else {
        return Err(match read_meta(transaction)? {
            Some(meta) => not_the_hub(&meta, hub),
            None => Error::NoLibrary,
        });
    };

    let result_json = work(&Write {
        transaction,
        rev,
        now_ms,
        entropy,
    })?;

    let command_id = request.command.map(|command| command.id);
    transaction.execute(
        "INSERT INTO library_revision
           (rev, command_id, method, client_session_pid, client_kind, committed_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        (
            rev,
            command_id.as_ref().map(Id::as_bytes),
            request.method,
            request
                .client
                .as_ref()
                .map(|client| client.session.as_bytes()),
            request.client.map(|client| client.kind.as_str()),
            now_ms,
        ),
    )?;
    if let Some(command) = &request.command {
        transaction.execute(
            "INSERT INTO library_command
               (command_id, request_fingerprint, method, result_json, rev, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            (
                command.id.as_bytes(),
                command.fingerprint.0,
                request.method,
                &result_json,
                rev,
                now_ms,
            ),
        )?;
    }
    Ok(Written {
        revision: rev,
        result_json,
        replayed: false,
    })
}

/// What a library has from the start, beside its pointer and its hub's
/// registry row: a listener, the ranks of the metadata sources, the hub's
/// player as a place listens come from, and the place corrections come
/// from.
fn first_rows(write: &Write<'_>, hub_row: i64) -> Result<(), Error> {
    let (connection, rev) = (write.connection(), write.rev());
    connection.execute(
        "INSERT INTO listener (public_id, name, rev) VALUES (?1, ?2, ?3)",
        (write.new_id()?.as_bytes(), DEFAULT_LISTENER, rev),
    )?;
    for (kind, precedence) in METADATA_SOURCES {
        connection.execute(
            "INSERT INTO metadata_source (kind, precedence, rev) VALUES (?1, ?2, ?3)",
            (kind, precedence, rev),
        )?;
    }
    connection.execute(
        "INSERT INTO history_endpoint (public_id, kind, server_id, name, rev)
         VALUES (?1, 'player', ?2, 'player', ?3)",
        (write.new_id()?.as_bytes(), hub_row, rev),
    )?;
    connection.execute(
        "INSERT INTO history_endpoint (public_id, kind, name, rev)
         VALUES (?1, 'manual', 'corrections', ?2)",
        (write.new_id()?.as_bytes(), rev),
    )?;
    Ok(())
}

/// What `library_meta` says, or `None` if the database holds no library:
/// it is empty, or has the tables and not yet the row.
fn read_meta(connection: &Connection) -> Result<Option<LibraryMeta>, Error> {
    let table: Option<String> = connection
        .query_row(
            "SELECT name FROM sqlite_schema WHERE type = 'table' AND name = 'library_meta'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if table.is_none() {
        return Ok(None);
    }
    Ok(connection
        .query_row(
            "SELECT library_id, hub_epoch, rev,
                    (SELECT public_id FROM server WHERE server.id = library_meta.hub_server_id)
             FROM library_meta WHERE id = 1",
            [],
            |row| {
                Ok(LibraryMeta {
                    library_id: Id::from_bytes(row.get(0)?),
                    hub_epoch: row.get(1)?,
                    revision: row.get(2)?,
                    hub: row.get::<_, Option<[u8; 16]>>(3)?.map(Id::from_bytes),
                })
            },
        )
        .optional()?)
}

fn not_the_hub(meta: &LibraryMeta, server_id: Id) -> Error {
    Error::NotTheHub {
        library_id: meta.library_id,
        hub: meta.hub,
        server_id,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::testing::{CountingEntropy, FixedClock, Scratch};
    use crate::{Role, ServerStore};

    const T0: i64 = 1_763_000_000_000;

    fn id(byte: u8) -> Id {
        Id::from_bytes([byte; 16])
    }

    /// A server, as the library is told of it.
    fn server(byte: u8, name: &str) -> ServerIdentity {
        ServerIdentity {
            server_id: id(byte),
            public_key: [0; 32],
            name: name.to_owned(),
            role: Role::Unassigned,
            library_id: None,
            created_at_ms: T0,
        }
    }

    fn command(byte: u8, params: &str) -> Command {
        Command {
            id: id(byte),
            fingerprint: Fingerprint::of("test.write", params.as_bytes()),
        }
    }

    /// A command from a person's session.
    fn request(method: &'static str, command: Option<Command>) -> WriteRequest {
        WriteRequest {
            method,
            command,
            client: Some(Client {
                session: id(0x5e),
                kind: SubjectKind::Person,
            }),
        }
    }

    fn creation() -> WriteRequest {
        WriteRequest {
            method: "library.create",
            command: Some(Command {
                id: id(0xc0),
                fingerprint: Fingerprint::of("library.create", b"{}"),
            }),
            client: Some(Client {
                session: id(0x5e),
                kind: SubjectKind::Person,
            }),
        }
    }

    /// The result `library.create` is given in these tests.
    fn receipt(meta: &LibraryMeta) -> String {
        format!("created {} at {}", meta.library_id, meta.revision)
    }

    fn in_memory(clock: &Arc<FixedClock>) -> LibraryStore {
        let (store, _) = LibraryStore::create_in_memory(
            &server(0x11, "desk"),
            creation(),
            receipt,
            clock.clone(),
            Arc::new(CountingEntropy::default()),
        )
        .unwrap();
        store
    }

    fn create_on_disk(path: &Path) -> Result<(LibraryStore, Written), Error> {
        LibraryStore::create(
            path,
            &server(0x11, "desk"),
            creation(),
            receipt,
            Arc::new(FixedClock::at(T0)),
            Arc::new(CountingEntropy::default()),
        )
    }

    fn open_on_disk(path: &Path, server_id: Id) -> Result<LibraryStore, Error> {
        LibraryStore::open(
            path,
            server_id,
            Arc::new(FixedClock::at(T0)),
            Arc::new(CountingEntropy::default()),
        )
    }

    /// Every row a query returns, each as its columns' text.
    fn rows(store: &LibraryStore, sql: &'static str) -> Vec<String> {
        store
            .reader
            .call(|connection| {
                let mut statement = connection.prepare(sql)?;
                let columns = statement.column_count();
                let rows = statement.query_map([], |row| {
                    (0..columns)
                        .map(|column| {
                            Ok(match row.get_ref(column)? {
                                rusqlite::types::ValueRef::Null => "-".to_owned(),
                                rusqlite::types::ValueRef::Integer(number) => number.to_string(),
                                rusqlite::types::ValueRef::Real(number) => number.to_string(),
                                rusqlite::types::ValueRef::Text(text) => {
                                    String::from_utf8_lossy(text).into_owned()
                                }
                                rusqlite::types::ValueRef::Blob(bytes) => {
                                    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
                                }
                            })
                        })
                        .collect::<Result<Vec<_>, _>>()
                        .map(|columns| columns.join("|"))
                })?;
                Ok(rows.collect::<Result<_, _>>()?)
            })
            .unwrap()
    }

    /// What the write path keeps: the counter, the log and the receipts.
    fn bookkeeping(store: &LibraryStore) -> (i64, Vec<String>, Vec<String>) {
        (
            store.meta().unwrap().revision,
            rows(
                store,
                "SELECT rev, method, hex(command_id) FROM library_revision ORDER BY rev",
            ),
            rows(
                store,
                "SELECT rev, method, hex(command_id), result_json FROM library_command ORDER BY id",
            ),
        )
    }

    /// A write that adds one setting, stamped with its revision.
    fn set(key: &'static str) -> impl FnOnce(&Write<'_>) -> Result<String, Error> + Send + 'static {
        move |write| {
            write.connection().execute(
                "INSERT INTO library_setting (key, value_json, rev) VALUES (?1, '1', ?2)",
                (key, write.rev()),
            )?;
            Ok(format!("set {key} at {}", write.rev()))
        }
    }

    fn settings(store: &LibraryStore) -> Vec<String> {
        rows(store, "SELECT key, rev FROM library_setting ORDER BY id")
    }

    #[test]
    fn a_store_can_be_shared_between_threads() {
        fn shared<T: Send + Sync>() {}
        shared::<LibraryStore>();
    }

    #[test]
    fn creating_a_library_writes_its_pointer_its_hub_and_what_it_starts_with() {
        let (store, written) = LibraryStore::create_in_memory(
            &server(0x11, "desk"),
            creation(),
            receipt,
            Arc::new(FixedClock::at(T0)),
            Arc::new(CountingEntropy::default()),
        )
        .unwrap();
        // T0 is 0x019a_7afd_1e00 milliseconds, and the counting source
        // gives the library's ID the bytes 00 to 09.
        let library_id = "019a7afd-1e00-7001-8203-040506070809";
        assert_eq!(store.library_id().to_string(), library_id);
        assert_eq!(store.schema_version(), newest());
        assert_eq!(
            store.meta().unwrap(),
            LibraryMeta {
                library_id: store.library_id(),
                hub: Some(id(0x11)),
                hub_epoch: 1,
                revision: 1,
            }
        );
        assert_eq!(
            written,
            Written {
                revision: 1,
                result_json: format!("created {library_id} at 1"),
                replayed: false,
            }
        );

        assert_eq!(
            rows(
                &store,
                "SELECT hex(public_id), name, created_at_ms FROM server"
            ),
            [format!("{}|desk|{T0}", "11".repeat(16))]
        );
        assert_eq!(rows(&store, "SELECT name FROM listener"), ["owner"]);
        assert_eq!(
            rows(
                &store,
                "SELECT kind, precedence FROM metadata_source ORDER BY precedence DESC"
            ),
            [
                "user|60",
                "musicbrainz|50",
                "beets|40",
                "file_tags|30",
                "device|20",
                "inferred|10"
            ]
        );
        assert_eq!(
            rows(
                &store,
                "SELECT e.kind, e.name, ifnull(s.name, '-')
                 FROM history_endpoint e LEFT JOIN server s ON s.id = e.server_id ORDER BY e.id"
            ),
            ["player|player|desk", "manual|corrections|-"]
        );
    }

    #[test]
    fn creating_is_revision_1_with_its_log_row_and_its_receipt() {
        let store = in_memory(&Arc::new(FixedClock::at(T0)));
        assert_eq!(
            rows(
                &store,
                "SELECT rev, hex(command_id), method, hex(client_session_pid), client_kind,
                        committed_at_ms
                 FROM library_revision"
            ),
            [format!(
                "1|{}|library.create|{}|person|{T0}",
                "C0".repeat(16),
                "5E".repeat(16)
            )]
        );
        let fingerprint: String = Fingerprint::of("library.create", b"{}")
            .0
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        assert_eq!(
            rows(
                &store,
                "SELECT hex(command_id), request_fingerprint, method, result_json, rev,
                        created_at_ms
                 FROM library_command"
            ),
            [format!(
                "{}|{fingerprint}|library.create|created {} at 1|1|{T0}",
                "C0".repeat(16),
                store.library_id()
            )]
        );
        // Every row the creation wrote is stamped with its revision.
        for table in [
            "server",
            "library_meta",
            "listener",
            "metadata_source",
            "history_endpoint",
        ] {
            let stamped: i64 = store
                .reader
                .call(|connection| {
                    Ok(connection.query_row(
                        &format!("SELECT count(*) FROM {table} WHERE rev = 1"),
                        [],
                        |row| row.get(0),
                    )?)
                })
                .unwrap();
            let all: i64 = store
                .reader
                .call(|connection| {
                    Ok(connection.query_row(
                        &format!("SELECT count(*) FROM {table}"),
                        [],
                        |row| row.get(0),
                    )?)
                })
                .unwrap();
            assert!(all > 0 && stamped == all, "{table}: {stamped} of {all}");
        }
    }

    #[test]
    fn the_fingerprint_tells_requests_apart_by_method_and_by_params() {
        let fingerprint = Fingerprint::of("library.create", b"{}");
        assert_eq!(fingerprint, Fingerprint::of("library.create", b"{}"));
        assert_ne!(fingerprint, Fingerprint::of("library.create", b"{ }"));
        assert_ne!(fingerprint, Fingerprint::of("library.creat", b"e{}"));
    }

    #[test]
    fn a_write_takes_the_next_revision_stamps_it_and_logs_it_with_its_receipt() {
        let clock = Arc::new(FixedClock::at(T0));
        let store = in_memory(&clock);

        clock.set(T0 + 1_000);
        let written = store
            .write(
                request("test.write", Some(command(0xc1, "a"))),
                set("first"),
            )
            .unwrap();
        assert_eq!(
            written,
            Written {
                revision: 2,
                result_json: "set first at 2".to_owned(),
                replayed: false,
            }
        );
        assert_eq!(settings(&store), ["first|2"]);
        assert_eq!(store.meta().unwrap().revision, 2);
        assert_eq!(
            rows(
                &store,
                "SELECT rev, hex(command_id), method, hex(client_session_pid), client_kind,
                        committed_at_ms
                 FROM library_revision WHERE rev = 2"
            ),
            [format!(
                "2|{}|test.write|{}|person|{}",
                "C1".repeat(16),
                "5E".repeat(16),
                T0 + 1_000
            )]
        );
        assert_eq!(
            rows(
                &store,
                "SELECT hex(command_id), method, result_json, rev, created_at_ms
                 FROM library_command WHERE rev = 2"
            ),
            [format!(
                "{}|test.write|set first at 2|2|{}",
                "C1".repeat(16),
                T0 + 1_000
            )]
        );
    }

    #[test]
    fn a_write_with_no_command_is_logged_and_leaves_no_receipt() {
        let store = in_memory(&Arc::new(FixedClock::at(T0)));
        let ingest = WriteRequest {
            method: "history.ingest",
            command: None,
            client: None,
        };
        let written = store.write(ingest, set("heard")).unwrap();
        assert_eq!((written.revision, written.replayed), (2, false));
        assert_eq!(settings(&store), ["heard|2"]);
        assert_eq!(
            rows(
                &store,
                "SELECT rev, command_id, method, client_session_pid, client_kind
                 FROM library_revision WHERE rev = 2"
            ),
            ["2|-|history.ingest|-|-"]
        );
        // The one receipt is the creation's.
        assert_eq!(bookkeeping(&store).2.len(), 1);
    }

    #[test]
    fn a_command_sent_again_gets_its_first_result_and_writes_nothing() {
        let clock = Arc::new(FixedClock::at(T0));
        let store = in_memory(&clock);
        let first = store
            .write(
                request("test.write", Some(command(0xc1, "a"))),
                set("first"),
            )
            .unwrap();
        let before = (bookkeeping(&store), settings(&store));

        clock.set(T0 + 60_000);
        let again = store
            .write(request("test.write", Some(command(0xc1, "a"))), |_| {
                panic!("a command that was carried out is not carried out again")
            })
            .unwrap();
        assert_eq!(
            again,
            Written {
                replayed: true,
                ..first
            }
        );
        assert_eq!((bookkeeping(&store), settings(&store)), before);
    }

    #[test]
    fn a_command_id_used_again_with_other_params_is_refused_and_writes_nothing() {
        let store = in_memory(&Arc::new(FixedClock::at(T0)));
        store
            .write(
                request("test.write", Some(command(0xc1, "a"))),
                set("first"),
            )
            .unwrap();
        let before = (bookkeeping(&store), settings(&store));

        let refused = store.write(
            request("test.write", Some(command(0xc1, "b"))),
            set("second"),
        );
        assert!(
            matches!(refused, Err(Error::CommandMismatch { command }) if command == id(0xc1)),
            "{refused:?}"
        );
        assert_eq!((bookkeeping(&store), settings(&store)), before);
    }

    #[test]
    fn a_write_that_fails_leaves_no_revision_no_receipt_and_no_rows() {
        let store = in_memory(&Arc::new(FixedClock::at(T0)));
        let before = (bookkeeping(&store), settings(&store));

        // It fails after its rows are in, as a crash before the commit
        // would leave it.
        let failed = store.write(request("test.write", Some(command(0xc1, "a"))), |write| {
            set("half done")(write)?;
            write.connection().execute(
                "INSERT INTO library_setting (key, value_json, rev) VALUES ('half done', '2', ?1)",
                [write.rev()],
            )?;
            Ok("never returned".to_owned())
        });
        assert!(matches!(failed, Err(Error::Sqlite(_))), "{failed:?}");
        assert_eq!((bookkeeping(&store), settings(&store)), before);

        // A rejected command left no receipt, so sending it again is
        // simply carrying it out, and it gets the revision the failure
        // did not use up.
        let written = store
            .write(request("test.write", Some(command(0xc1, "a"))), set("done"))
            .unwrap();
        assert_eq!((written.revision, written.replayed), (2, false));
        assert_eq!(settings(&store), ["done|2"]);
    }

    #[test]
    fn revisions_run_on_without_a_gap() {
        let store = in_memory(&Arc::new(FixedClock::at(T0)));
        for (key, expected) in [("a", 2), ("b", 3), ("c", 4)] {
            let none = WriteRequest {
                method: "test.write",
                command: None,
                client: None,
            };
            assert_eq!(store.write(none, set(key)).unwrap().revision, expected);
        }
        let (revision, log, _) = bookkeeping(&store);
        assert_eq!(revision, 4);
        assert_eq!(
            log,
            [
                format!("1|library.create|{}", "C0".repeat(16)),
                // A write with no command has no ID to show.
                "2|test.write|".to_owned(),
                "3|test.write|".to_owned(),
                "4|test.write|".to_owned(),
            ]
        );
    }

    /// Changes the library behind the store's back, as a release or an
    /// adopt on another machine would.
    fn tamper(store: &LibraryStore, sql: &'static str) {
        store
            .writer
            .call(move |connection| Ok(connection.execute_batch(sql)?))
            .unwrap();
    }

    #[test]
    fn a_write_is_refused_once_the_pointer_no_longer_names_this_server() {
        let store = in_memory(&Arc::new(FixedClock::at(T0)));
        let before = (bookkeeping(&store), settings(&store));

        tamper(&store, "UPDATE library_meta SET hub_server_id = NULL");
        let refused = store.write(request("test.write", None), set("after a release"));
        assert!(
            matches!(refused, Err(Error::NotTheHub { hub: None, .. })),
            "{refused:?}"
        );

        tamper(
            &store,
            "INSERT INTO server (public_id, name, created_at_ms, rev)
             VALUES (x'22222222222222222222222222222222', 'attic', 0, 1);
             UPDATE library_meta SET hub_server_id = 2",
        );
        let refused = store.write(request("test.write", None), set("after an adopt"));
        let Err(Error::NotTheHub {
            library_id,
            hub,
            server_id,
        }) = refused
        else {
            panic!("{refused:?}");
        };
        assert_eq!(
            (library_id, hub, server_id),
            (store.library_id(), Some(id(0x22)), id(0x11))
        );
        assert_eq!((bookkeeping(&store), settings(&store)), before);
    }

    #[test]
    fn asking_to_create_a_library_that_exists_replays_its_own_command_and_refuses_another() {
        let store = in_memory(&Arc::new(FixedClock::at(T0)));
        let before = bookkeeping(&store);

        let again = store.created_by(creation()).unwrap();
        assert_eq!(
            again,
            Written {
                revision: 1,
                result_json: format!("created {} at 1", store.library_id()),
                replayed: true,
            }
        );

        let mut other_params = creation();
        other_params.command = Some(Command {
            id: id(0xc0),
            fingerprint: Fingerprint::of("library.create", br#"{"name":"another"}"#),
        });
        let refused = store.created_by(other_params);
        assert!(
            matches!(refused, Err(Error::CommandMismatch { .. })),
            "{refused:?}"
        );

        let mut another = creation();
        another.command = Some(Command {
            id: id(0xc9),
            fingerprint: Fingerprint::of("library.create", b"{}"),
        });
        let refused = store.created_by(another);
        assert!(matches!(refused, Err(Error::LibraryExists)), "{refused:?}");
        assert_eq!(bookkeeping(&store), before);
    }

    /// The schema version of a library this program makes.
    fn newest() -> i64 {
        crate::migrate::LIBRARY
            .last()
            .map_or(0, |last| last.version)
    }

    fn library_db(scratch: &Scratch) -> PathBuf {
        scratch.path().join("library.db")
    }

    #[test]
    fn opening_schema_one_reindexes_existing_songs_before_the_first_search() {
        let scratch = Scratch::new();
        let path = library_db(&scratch);
        let mut db = Connection::open(&path).unwrap();
        migrate::run(&mut db, &migrate::LIBRARY[..1], &FixedClock::at(T0)).unwrap();
        db.execute_batch(include_str!("../saved/library/0001.sql"))
            .unwrap();
        drop(db);

        let store = open_on_disk(&path, id(0x11)).unwrap();
        for query in ["title:quael", "artist:solen", "album:blae"] {
            let key: i64 = store
                .read(|db| {
                    Ok(db.query_row(
                        "SELECT rowid FROM search_text WHERE search_text MATCH ?1",
                        [query],
                        |row| row.get(0),
                    )?)
                })
                .unwrap();
            assert_eq!(key, 7);
        }
    }

    #[test]
    fn a_library_is_the_same_library_after_a_restart() {
        let scratch = Scratch::new();
        let path = library_db(&scratch);
        let (store, _) = create_on_disk(&path).unwrap();
        assert_eq!(store.migrations_applied(), crate::migrate::LIBRARY.len());
        let created = store.meta().unwrap();
        store
            .write(request("test.write", None), set("kept"))
            .unwrap();
        store.close().unwrap();
        assert!(!scratch.path().join("library.db-wal").exists());

        let store = open_on_disk(&path, id(0x11)).unwrap();
        assert_eq!(store.migrations_applied(), 0);
        assert_eq!(store.schema_version(), newest());
        assert_eq!(store.library_id(), created.library_id);
        assert_eq!(
            store.meta().unwrap(),
            LibraryMeta {
                revision: 2,
                ..created
            }
        );
        // And it is still this server's to write.
        let written = store
            .write(request("test.write", None), set("more"))
            .unwrap();
        assert_eq!(written.revision, 3);
        assert_eq!(settings(&store), ["kept|2", "more|3"]);
    }

    #[test]
    fn creating_where_a_library_is_refuses_and_leaves_it_alone() {
        let scratch = Scratch::new();
        let path = library_db(&scratch);
        let (store, _) = create_on_disk(&path).unwrap();
        let library_id = store.library_id();
        store.close().unwrap();
        let before = std::fs::read(&path).unwrap();

        let refused = create_on_disk(&path);
        assert!(matches!(refused, Err(Error::LibraryExists)), "{refused:?}");
        drop(refused);
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(
            open_on_disk(&path, id(0x11)).unwrap().library_id(),
            library_id
        );
    }

    #[test]
    fn a_file_with_the_tables_and_no_library_yet_can_have_one_created_in_it() {
        let scratch = Scratch::new();
        let path = library_db(&scratch);
        // As a creation leaves the file if it stops between the migration
        // and the first write.
        {
            let mut connection = Connection::open(&path).unwrap();
            migrate::run(&mut connection, migrate::LIBRARY, &FixedClock::at(T0)).unwrap();
        }
        let refused = open_on_disk(&path, id(0x11));
        assert!(matches!(refused, Err(Error::NoLibrary)), "{refused:?}");
        drop(refused);

        let (store, written) = create_on_disk(&path).unwrap();
        assert_eq!(store.migrations_applied(), 0);
        assert_eq!(written.revision, 1);
    }

    #[test]
    fn where_there_is_no_library_none_is_opened_and_none_is_made() {
        let scratch = Scratch::new();
        let path = library_db(&scratch);
        let refused = open_on_disk(&path, id(0x11));
        assert!(matches!(refused, Err(Error::NoLibrary)), "{refused:?}");
        assert!(!path.exists());

        // An empty file is no library either.
        std::fs::write(&path, b"").unwrap();
        let refused = open_on_disk(&path, id(0x11));
        assert!(matches!(refused, Err(Error::NoLibrary)), "{refused:?}");
    }

    /// Changes a closed database behind the program's back.
    fn tamper_file(path: &Path, sql: &str) {
        Connection::open(path).unwrap().execute_batch(sql).unwrap();
    }

    #[test]
    fn a_library_whose_pointer_names_another_server_or_none_is_not_opened_for_writing() {
        let scratch = Scratch::new();
        let path = library_db(&scratch);
        let (store, _) = create_on_disk(&path).unwrap();
        let library = store.library_id();
        store.close().unwrap();

        // A server other than the one the pointer names, on a library in
        // rollback-journal mode, which opening it for writing would change.
        tamper_file(&path, "PRAGMA journal_mode = DELETE");
        let before = std::fs::read(&path).unwrap();
        let refused = open_on_disk(&path, id(0x22));
        let Err(Error::NotTheHub {
            library_id,
            hub,
            server_id,
        }) = &refused
        else {
            panic!("{refused:?}");
        };
        assert_eq!(
            (*library_id, *hub, *server_id),
            (library, Some(id(0x11)), id(0x22))
        );
        assert_eq!(
            refused.unwrap_err().to_string(),
            format!(
                "library {library} belongs to server {}, and this server is {}: only the server \
                 its hub pointer names may write it",
                id(0x11),
                id(0x22)
            )
        );
        // Being refused changed nothing in the file.
        assert_eq!(std::fs::read(&path).unwrap(), before);

        // A released library: the pointer is empty.
        tamper_file(
            &path,
            "UPDATE library_meta SET hub_server_id = NULL, hub_epoch = 2",
        );
        let refused = open_on_disk(&path, id(0x11));
        assert!(
            matches!(&refused, Err(Error::NotTheHub { hub: None, .. })),
            "{refused:?}"
        );
        assert_eq!(
            refused.unwrap_err().to_string(),
            format!(
                "library {library} has been released: no server may write it until one adopts it"
            )
        );
    }

    #[test]
    fn a_library_is_not_migrated_for_a_server_that_may_not_write_it() {
        let scratch = Scratch::new();
        let path = library_db(&scratch);
        create_on_disk(&path).unwrap().0.close().unwrap();
        // As a library at an older schema would be: the ledger is behind
        // what this program ships.
        tamper_file(&path, "DELETE FROM schema_migration");
        let refused = open_on_disk(&path, id(0x22));
        assert!(
            matches!(refused, Err(Error::NotTheHub { .. })),
            "{refused:?}"
        );
        drop(refused);
        let ledger: i64 = Connection::open(&path)
            .unwrap()
            .query_row("SELECT count(*) FROM schema_migration", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(ledger, 0);
    }

    #[test]
    fn a_library_from_a_newer_program_is_not_opened() {
        let scratch = Scratch::new();
        let path = library_db(&scratch);
        create_on_disk(&path).unwrap().0.close().unwrap();
        let future = newest() + 1;
        tamper_file(
            &path,
            &format!(
                "INSERT INTO schema_migration VALUES ({future}, '{future:04}_from_the_future', x'00', 0)"
            ),
        );
        let refused = open_on_disk(&path, id(0x11));
        assert!(
            matches!(refused, Err(Error::NewerSchema { found, known }) if (found, known) == (future, newest())),
            "{refused:?}"
        );
    }

    #[test]
    fn the_library_a_real_server_creates_names_that_server() {
        let clock = Arc::new(FixedClock::at(T0));
        let entropy = Arc::new(CountingEntropy::default());
        let servers = ServerStore::open_in_memory(clock.clone(), entropy.clone()).unwrap();
        let (identity, _) = servers.ensure_identity("desk").unwrap();
        let (library, _) =
            LibraryStore::create_in_memory(&identity, creation(), receipt, clock, entropy).unwrap();
        assert_eq!(library.meta().unwrap().hub, Some(identity.server_id));

        let claimed = servers
            .claim_library(library.library_id())
            .unwrap()
            .unwrap();
        assert_eq!(
            (claimed.role, claimed.library_id),
            (Role::Hub, Some(library.library_id()))
        );
    }
}
