//! How a database file is opened: the SQLite version, the settings every
//! connection gets, the one writer and the readers. Both stores use it. See
//! decisions 16 to 18 and "SQLite rules" in docs/design/server.md.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError, TryLockError, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};

use crate::Error;

/// The SQLite this program is written for: the one the pinned `rusqlite`
/// bundles. A store refuses to open under any other.
pub const SQLITE_VERSION: &str = "3.54.1";

/// How long a connection waits for a lock another one holds.
const BUSY_TIMEOUT: Duration = Duration::from_millis(5000);

/// Fails unless the SQLite linked into this program is [`SQLITE_VERSION`].
pub(crate) fn assert_version() -> Result<(), Error> {
    match rusqlite::version() {
        SQLITE_VERSION => Ok(()),
        found => Err(Error::SqliteVersion { found }),
    }
}

/// Where a database is.
#[derive(Debug, Clone)]
pub(crate) enum Location {
    /// A file, which runs with a write-ahead log.
    File(PathBuf),
    /// A database in memory, by its name in SQLite's `memdb` file system.
    /// Every connection that opens the name sees the one database, and it is
    /// gone when the last of them closes.
    Memory(String),
}

impl Location {
    /// A new, empty database in memory.
    pub(crate) fn memory() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        // The leading slash is what makes `memdb` share it between
        // connections.
        Self::Memory(format!("/engine-{}", NEXT.fetch_add(1, Ordering::Relaxed)))
    }

    fn open(&self, access: OpenFlags) -> Result<Connection, Error> {
        // Without SQLITE_OPEN_URI a path is only ever a path, whatever
        // characters it holds.
        let flags = access | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let connection = match self {
            Self::File(path) => Connection::open_with_flags(path, flags)?,
            Self::Memory(name) => Connection::open_with_flags_and_vfs(name, flags, c"memdb")?,
        };
        connection.busy_timeout(BUSY_TIMEOUT)?;
        connection.pragma_update(None, "foreign_keys", true)?;
        connection.pragma_update(None, "temp_store", "MEMORY")?;
        Ok(connection)
    }

    /// Opens the write connection: the only one a process ever has for the
    /// database.
    fn open_writer(&self) -> Result<Connection, Error> {
        let connection =
            self.open(OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE)?;
        if matches!(self, Self::File(_)) {
            let mode: String =
                connection
                    .pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))?;
            if mode != "wal" {
                return Err(Error::NotWal { found: mode });
            }
        }
        // An acknowledged commit must survive a power loss: decision 18.
        connection.pragma_update(None, "synchronous", "FULL")?;
        Ok(connection)
    }
}

type Job = Box<dyn FnOnce(&mut Connection) + Send>;

/// The one thread that owns a database's write connection. A write is a
/// closure sent to it, so writes happen one at a time, in the order they
/// were sent, with no lock in the program.
#[derive(Debug)]
pub(crate) struct Writer {
    jobs: Option<mpsc::Sender<Job>>,
    thread: Option<JoinHandle<()>>,
}

impl Writer {
    /// Opens the write connection and starts the thread that owns it.
    pub(crate) fn start(location: &Location, thread_name: &str) -> Result<Self, Error> {
        let mut connection = location.open_writer()?;
        let (jobs, queue) = mpsc::channel::<Job>();
        let thread = std::thread::Builder::new()
            .name(thread_name.to_owned())
            .spawn(move || {
                for job in queue {
                    job(&mut connection);
                }
            })
            .map_err(Error::WriterSpawn)?;
        Ok(Self {
            jobs: Some(jobs),
            thread: Some(thread),
        })
    }

    /// Runs `work` on the writer thread and waits for what it returns.
    pub(crate) fn call<T, F>(&self, work: F) -> Result<T, Error>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T, Error> + Send + 'static,
    {
        let (reply, answer) = mpsc::channel();
        let job: Job = Box::new(move |connection| {
            // The caller has gone only if its thread has died.
            let _ = reply.send(work(connection));
        });
        self.jobs
            .as_ref()
            .ok_or(Error::WriterStopped)?
            .send(job)
            .map_err(|_| Error::WriterStopped)?;
        answer.recv().map_err(|_| Error::WriterStopped)?
    }

    /// Moves everything in the write-ahead log into the database file and
    /// empties the log, then stops the thread. Readers must be closed first.
    pub(crate) fn close(mut self) -> Result<(), Error> {
        let truncated = self.call(|connection| {
            let busy: i64 =
                connection.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))?;
            if busy == 0 {
                Ok(())
            } else {
                Err(Error::CheckpointBusy)
            }
        });
        self.stop();
        truncated
    }

    fn stop(&mut self) {
        // The thread ends when the last sender is gone.
        self.jobs = None;
        if let Some(thread) = self.thread.take() {
            // It has panicked only if a job did, and that job's caller has
            // been told already.
            let _ = thread.join();
        }
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        self.stop();
    }
}

/// How many read connections a database has: enough that a slow read, such
/// as a window of a large list, does not hold up the covers being found
/// beside it. Measured on M1's walls (docs/design/server.md, "Deferred").
const READERS: usize = 4;

/// The read side of a database: a few connections that cannot write. With
/// a write-ahead log, reads on them run at once and beside the writer.
#[derive(Debug)]
pub(crate) struct Reader {
    /// The connection a read waits for when every one is in use.
    first: Mutex<Connection>,
    others: Vec<Mutex<Connection>>,
    next: AtomicUsize,
}

impl Reader {
    /// Opens the read side. The writer must have opened the database first.
    pub(crate) fn open(location: &Location) -> Result<Self, Error> {
        let open = || {
            location
                .open(OpenFlags::SQLITE_OPEN_READ_ONLY)
                .map(Mutex::new)
        };
        Ok(Self {
            first: open()?,
            others: (1..READERS).map(|_| open()).collect::<Result<_, _>>()?,
            next: AtomicUsize::new(0),
        })
    }

    /// Runs a short read, on a connection no other read is using if there
    /// is one, and otherwise waits for one.
    pub(crate) fn call<T>(
        &self,
        work: impl FnOnce(&Connection) -> Result<T, Error>,
    ) -> Result<T, Error> {
        work(&self.lock())
    }

    fn lock(&self) -> MutexGuard<'_, Connection> {
        // Each read starts looking at the next connection, so that reads
        // spread over them.
        let start = self.next.fetch_add(1, Ordering::Relaxed);
        let all: Vec<&Mutex<Connection>> =
            std::iter::once(&self.first).chain(&self.others).collect();
        for offset in 0..all.len() {
            let Some(connection) = all.get((start + offset) % all.len()) else {
                continue;
            };
            match connection.try_lock() {
                Ok(guard) => return guard,
                // A read that panicked has left the connection as good as
                // it was.
                Err(TryLockError::Poisoned(poisoned)) => return poisoned.into_inner(),
                Err(TryLockError::WouldBlock) => {}
            }
        }
        // Every one is in use: wait for the first.
        self.first.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Scratch;

    #[test]
    fn the_bundled_sqlite_is_the_one_asserted() {
        assert_eq!(rusqlite::version(), SQLITE_VERSION);
        assert_version().unwrap();
    }

    fn pragma(connection: &Connection, name: &str) -> Result<String, Error> {
        Ok(connection
            .query_row(&format!("PRAGMA {name}"), [], |row| {
                row.get::<_, rusqlite::types::Value>(0)
            })
            .map(|value| match value {
                rusqlite::types::Value::Integer(number) => number.to_string(),
                rusqlite::types::Value::Text(text) => text,
                other => format!("{other:?}"),
            })?)
    }

    #[test]
    fn a_file_is_opened_with_the_settings_the_notes_ask_for() {
        let scratch = Scratch::new();
        let location = Location::File(scratch.path().join("test.db"));
        let writer = Writer::start(&location, "test writer").unwrap();
        let reader = Reader::open(&location).unwrap();

        let on_writer = writer
            .call(|connection| {
                Ok([
                    pragma(connection, "journal_mode")?,
                    pragma(connection, "synchronous")?,
                    pragma(connection, "foreign_keys")?,
                    pragma(connection, "busy_timeout")?,
                    pragma(connection, "temp_store")?,
                ])
            })
            .unwrap();
        // synchronous 2 is FULL, and temp_store 2 is MEMORY.
        assert_eq!(on_writer, ["wal", "2", "1", "5000", "2"]);

        let on_reader = reader
            .call(|connection| {
                Ok([
                    pragma(connection, "journal_mode")?,
                    pragma(connection, "foreign_keys")?,
                    pragma(connection, "busy_timeout")?,
                    pragma(connection, "temp_store")?,
                ])
            })
            .unwrap();
        assert_eq!(on_reader, ["wal", "1", "5000", "2"]);
    }

    #[test]
    fn a_reader_sees_what_the_writer_wrote_and_cannot_write() {
        let scratch = Scratch::new();
        for location in [
            Location::memory(),
            Location::File(scratch.path().join("test.db")),
        ] {
            let writer = Writer::start(&location, "test writer").unwrap();
            let reader = Reader::open(&location).unwrap();
            writer
                .call(|connection| {
                    connection.execute_batch(
                        "CREATE TABLE note (body TEXT NOT NULL) STRICT;
                         INSERT INTO note VALUES ('first');",
                    )?;
                    Ok(())
                })
                .unwrap();

            let body: String = reader
                .call(|connection| {
                    Ok(connection.query_row("SELECT body FROM note", [], |row| row.get(0))?)
                })
                .unwrap();
            assert_eq!(body, "first");

            let refused = reader.call(|connection| {
                connection.execute("INSERT INTO note VALUES ('second')", [])?;
                Ok(())
            });
            assert!(
                matches!(&refused, Err(Error::Sqlite(error))
                    if error.sqlite_error_code() == Some(rusqlite::ErrorCode::ReadOnly)),
                "{refused:?}"
            );
        }
    }

    #[test]
    fn a_slow_read_does_not_hold_up_another() {
        let location = Location::memory();
        let writer = Writer::start(&location, "test writer").unwrap();
        writer
            .call(|connection| Ok(connection.execute_batch("CREATE TABLE note (body)")?))
            .unwrap();
        let reader = Reader::open(&location).unwrap();
        let (holding, held) = mpsc::channel();
        let (release, released) = mpsc::channel::<()>();
        let reader = &reader;
        std::thread::scope(|scope| {
            scope.spawn(move || {
                reader
                    .call(|_| {
                        holding.send(()).unwrap();
                        released.recv().unwrap();
                        Ok(())
                    })
                    .unwrap();
            });
            held.recv().unwrap();
            // Another read goes ahead while the first is still reading.
            let count: i64 = reader
                .call(|connection| {
                    Ok(connection.query_row("SELECT count(*) FROM note", [], |row| row.get(0))?)
                })
                .unwrap();
            assert_eq!(count, 0);
            release.send(()).unwrap();
        });
    }

    #[test]
    fn two_databases_in_memory_are_separate() {
        let (first, second) = (Location::memory(), Location::memory());
        let first = Writer::start(&first, "test writer").unwrap();
        let second = Writer::start(&second, "test writer").unwrap();
        first
            .call(|connection| Ok(connection.execute_batch("CREATE TABLE only_here (x)")?))
            .unwrap();
        let tables: i64 = second
            .call(|connection| {
                Ok(connection
                    .query_row("SELECT count(*) FROM sqlite_schema", [], |row| row.get(0))?)
            })
            .unwrap();
        assert_eq!(tables, 0);
    }

    #[test]
    fn writes_run_in_the_order_they_were_sent() {
        let writer = Writer::start(&Location::memory(), "test writer").unwrap();
        writer
            .call(|connection| Ok(connection.execute_batch("CREATE TABLE step (n INTEGER)")?))
            .unwrap();
        std::thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| {
                    for _ in 0..25 {
                        writer
                            .call(|connection| {
                                // Read, then write: two of these at once
                                // would lose a count.
                                let next: i64 = connection.query_row(
                                    "SELECT count(*) FROM step",
                                    [],
                                    |row| row.get(0),
                                )?;
                                connection.execute("INSERT INTO step VALUES (?1)", [next])?;
                                Ok(())
                            })
                            .unwrap();
                    }
                });
            }
        });
        let (count, distinct): (i64, i64) = writer
            .call(|connection| {
                Ok(connection.query_row(
                    "SELECT count(*), count(DISTINCT n) FROM step",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )?)
            })
            .unwrap();
        assert_eq!((count, distinct), (100, 100));
    }

    #[test]
    fn closing_empties_the_write_ahead_log() {
        let scratch = Scratch::new();
        let path = scratch.path().join("test.db");
        let location = Location::File(path.clone());
        let writer = Writer::start(&location, "test writer").unwrap();
        writer
            .call(|connection| Ok(connection.execute_batch("CREATE TABLE note (body)")?))
            .unwrap();
        let wal = scratch.path().join("test.db-wal");
        assert!(std::fs::metadata(&wal).unwrap().len() > 0);

        writer.close().unwrap();
        // SQLite removes an empty log when the last connection closes.
        assert!(!wal.exists());
        assert!(std::fs::metadata(&path).unwrap().len() > 0);
    }

    #[test]
    fn closing_with_a_read_in_progress_says_so() {
        let scratch = Scratch::new();
        let location = Location::File(scratch.path().join("test.db"));
        let writer = Writer::start(&location, "test writer").unwrap();
        writer
            .call(|connection| Ok(connection.execute_batch("CREATE TABLE note (body)")?))
            .unwrap();
        let reader = Reader::open(&location).unwrap();
        // A read transaction left open holds the log.
        reader
            .call(|connection| {
                connection.execute_batch("BEGIN; SELECT count(*) FROM note;")?;
                Ok(())
            })
            .unwrap();
        writer
            .call(|connection| {
                connection.execute_batch("INSERT INTO note VALUES ('x')")?;
                // Not the five seconds a server gives a reader to finish.
                Ok(connection.busy_timeout(Duration::ZERO)?)
            })
            .unwrap();
        assert!(matches!(writer.close(), Err(Error::CheckpointBusy)));
    }

    #[test]
    fn a_write_that_panics_stops_the_writer_and_callers_are_told() {
        let writer = Writer::start(&Location::memory(), "test writer").unwrap();
        let panicked: Result<(), Error> = writer.call(|_| panic!("a write that panics"));
        assert!(matches!(panicked, Err(Error::WriterStopped)));
        assert!(matches!(writer.call(|_| Ok(())), Err(Error::WriterStopped)));
    }
}
