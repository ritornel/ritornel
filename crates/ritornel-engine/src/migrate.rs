//! The migration runner: numbered SQL files that are only ever added to,
//! applied in order on the writer. See "Migrations" in
//! docs/internals/server.md.
//!
//! The ledger is the table `schema_migration`, which the first migration of
//! each database creates. A database's schema version is the highest number
//! in it.

use rusqlite::functions::FunctionFlags;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior};
use sha2::{Digest, Sha256};

use crate::{Clock, Error};

/// One migration, as shipped.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Migration {
    /// Its number. A database's migrations are numbered from 1 with no gaps.
    pub(crate) version: i64,
    /// The file's name without `.sql`.
    pub(crate) name: &'static str,
    /// The file. It is never edited once it has shipped.
    pub(crate) sql: &'static str,
}

impl Migration {
    /// What the ledger keeps to notice a changed file: the SHA-256 of its
    /// bytes.
    fn checksum(&self) -> [u8; 32] {
        Sha256::digest(self.sql.as_bytes()).into()
    }
}

// A change to a schema is the next numbered file, `0002_<name>.sql`,
// added to its list below. It runs over data, so it comes with a saved
// database at the schema before it: a dump of a database the migrations
// so far made, with rows in it, in `saved/<server or library>/0001.sql`,
// and a test, among the tests of the store it changes, that opens the
// dump through that store and finds what it held. AGENTS.md has the rule.

/// The migrations of `server.db`.
pub(crate) const SERVER: &[Migration] = &[
    Migration {
        version: 1,
        name: "0001_server",
        sql: include_str!("../migrations/server/0001_server.sql"),
    },
    Migration {
        version: 2,
        name: "0002_hub_link",
        sql: include_str!("../migrations/server/0002_hub_link.sql"),
    },
];

/// The migrations of `library.db`.
pub(crate) const LIBRARY: &[Migration] = &[
    Migration {
        version: 1,
        name: "0001_library",
        sql: include_str!("../migrations/library/0001_library.sql"),
    },
    Migration {
        version: 2,
        name: "0002_fold_search_text",
        sql: include_str!("../migrations/library/0002_fold_search_text.sql"),
    },
    Migration {
        version: 3,
        name: "0003_claim_and_love_indexes",
        sql: include_str!("../migrations/library/0003_claim_and_love_indexes.sql"),
    },
    Migration {
        version: 4,
        name: "0004_observation_recording_correction",
        sql: include_str!("../migrations/library/0004_observation_recording_correction.sql"),
    },
    Migration {
        version: 5,
        name: "0005_artist_keys",
        sql: include_str!("../migrations/library/0005_artist_keys.sql"),
    },
    Migration {
        version: 6,
        name: "0006_alias_spellings_and_unbind",
        sql: include_str!("../migrations/library/0006_alias_spellings_and_unbind.sql"),
    },
    Migration {
        version: 7,
        name: "0007_delivery_sender",
        sql: include_str!("../migrations/library/0007_delivery_sender.sql"),
    },
    Migration {
        version: 8,
        name: "0008_spotify_streams",
        sql: include_str!("../migrations/library/0008_spotify_streams.sql"),
    },
    Migration {
        version: 9,
        name: "0009_sidecars_and_disc_layouts",
        sql: include_str!("../migrations/library/0009_sidecars_and_disc_layouts.sql"),
    },
    Migration {
        version: 10,
        name: "0010_lookup_replies",
        sql: include_str!("../migrations/library/0010_lookup_replies.sql"),
    },
    Migration {
        version: 11,
        name: "0011_album_proposals",
        sql: include_str!("../migrations/library/0011_album_proposals.sql"),
    },
    Migration {
        version: 12,
        name: "0012_answers",
        sql: include_str!("../migrations/library/0012_answers.sql"),
    },
];

/// What a run did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Migrated {
    /// How many migrations this run applied. 0 when the database was up to
    /// date.
    pub(crate) applied: usize,
    /// The schema version the database is at now.
    pub(crate) version: i64,
}

/// Brings a database up to the newest of `migrations`.
///
/// Nothing is changed unless the database is one this program can take
/// forward: empty, or holding a ledger whose every entry is a shipped
/// migration with the checksum it had when it was applied. Each migration
/// then runs in a transaction of its own with its ledger row, so a failure
/// leaves the database at the migration before it.
pub(crate) fn run(
    connection: &mut Connection,
    migrations: &[Migration],
    clock: &dyn Clock,
) -> Result<Migrated, Error> {
    let recorded = recorded(connection)?;
    let known = migrations.last().map_or(0, |last| last.version);
    if let Some(&(newest, _)) = recorded.last()
        && newest > known
    {
        return Err(Error::NewerSchema {
            found: newest,
            known,
        });
    }

    let mut shipped = migrations.iter();
    for (version, checksum) in &recorded {
        match shipped.next() {
            Some(migration) if migration.version != *version => {
                return Err(Error::MigrationGap {
                    missing: migration.version,
                });
            }
            Some(migration) if migration.checksum().as_slice() != checksum.as_slice() => {
                return Err(Error::ChangedMigration { version: *version });
            }
            _ => {}
        }
    }

    if shipped.len() == 0 {
        return Ok(Migrated {
            applied: 0,
            version: known,
        });
    }
    // SQL migrations can fold search text exactly as queries do.
    connection.create_scalar_function(
        "search_fold",
        1,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| {
            Ok(ctx
                .get::<Option<String>>(0)?
                .as_deref()
                .map(crate::list::fold::fold))
        },
    )?;

    // A migration that changes a table rebuilds it: a new table, the rows
    // copied, the old one dropped. SQLite's procedure for that turns foreign
    // keys off for the migration, which only works outside a transaction,
    // and checks them before the commit, so a migration can never leave a
    // dangling reference behind. They are on again whatever happens.
    let enforced: bool = connection.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
    connection.pragma_update(None, "foreign_keys", false)?;
    let applied = apply(connection, shipped, clock);
    connection.pragma_update(None, "foreign_keys", enforced)?;
    connection.remove_function("search_fold", 1)?;
    Ok(Migrated {
        applied: applied?,
        version: known,
    })
}

/// Applies each of `migrations` in a transaction of its own with its ledger
/// row, and returns how many it applied.
fn apply<'a>(
    connection: &mut Connection,
    migrations: impl Iterator<Item = &'a Migration>,
    clock: &dyn Clock,
) -> Result<usize, Error> {
    let mut applied = 0;
    for migration in migrations {
        let failed = |source| Error::MigrationFailed {
            version: migration.version,
            source,
        };
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(failed)?;
        transaction.execute_batch(migration.sql).map_err(failed)?;
        let dangling: Option<String> = transaction
            .query_row("PRAGMA foreign_key_check", [], |row| row.get(0))
            .optional()
            .map_err(failed)?;
        if let Some(table) = dangling {
            return Err(failed(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY),
                Some(format!(
                    "the migration leaves a dangling reference in {table}"
                )),
            )));
        }
        transaction
            .execute(
                "INSERT INTO schema_migration (version, name, checksum, applied_at_ms)
                 VALUES (?1, ?2, ?3, ?4)",
                (
                    migration.version,
                    migration.name,
                    migration.checksum(),
                    clock.now_ms(),
                ),
            )
            .map_err(failed)?;
        transaction.commit().map_err(failed)?;
        applied += 1;
    }
    Ok(applied)
}

/// The ledger, oldest first: each applied migration's number and checksum.
/// An empty database has none. A database with tables and no ledger is not
/// ours.
pub(crate) fn recorded(connection: &Connection) -> Result<Vec<(i64, Vec<u8>)>, Error> {
    let ledger: Option<String> = connection
        .query_row(
            "SELECT name FROM sqlite_schema WHERE type = 'table' AND name = 'schema_migration'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if ledger.is_none() {
        let objects: i64 =
            connection.query_row("SELECT count(*) FROM sqlite_schema", [], |row| row.get(0))?;
        return if objects == 0 {
            Ok(Vec::new())
        } else {
            Err(Error::ForeignDatabase)
        };
    }
    let mut statement =
        connection.prepare("SELECT version, checksum FROM schema_migration ORDER BY version")?;
    let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FixedClock;

    const FIRST: Migration = Migration {
        version: 1,
        name: "0001_first",
        sql: "CREATE TABLE schema_migration (
                version INTEGER PRIMARY KEY, name TEXT NOT NULL, checksum BLOB NOT NULL,
                applied_at_ms INTEGER NOT NULL
              ) STRICT;
              CREATE TABLE fixture_album (id INTEGER PRIMARY KEY, title TEXT NOT NULL) STRICT;",
    };
    const SECOND: Migration = Migration {
        version: 2,
        name: "0002_second",
        sql: "ALTER TABLE fixture_album ADD COLUMN year INTEGER;",
    };
    const BROKEN_THIRD: Migration = Migration {
        version: 3,
        name: "0003_broken",
        sql: "CREATE TABLE fixture_track (id INTEGER PRIMARY KEY) STRICT;
              INSERT INTO no_such_table VALUES (1);",
    };

    fn ledger(connection: &Connection) -> Vec<(i64, String, i64)> {
        connection
            .prepare("SELECT version, name, applied_at_ms FROM schema_migration ORDER BY version")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    fn tables(connection: &Connection) -> Vec<String> {
        connection
            .prepare("SELECT name FROM sqlite_schema WHERE type = 'table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    #[test]
    fn a_fresh_database_gets_every_migration_and_a_ledger_row_for_each() {
        let mut connection = Connection::open_in_memory().unwrap();
        let clock = FixedClock::at(1_700_000_000_000);
        let migrated = run(&mut connection, &[FIRST, SECOND], &clock).unwrap();
        assert_eq!(
            migrated,
            Migrated {
                applied: 2,
                version: 2
            }
        );
        assert_eq!(
            ledger(&connection),
            [
                (1, "0001_first".to_owned(), 1_700_000_000_000),
                (2, "0002_second".to_owned(), 1_700_000_000_000),
            ]
        );
        let checksum: Vec<u8> = connection
            .query_row(
                "SELECT checksum FROM schema_migration WHERE version = 2",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(checksum, Sha256::digest(SECOND.sql.as_bytes()).to_vec());
        connection
            .execute(
                "INSERT INTO fixture_album (title, year) VALUES ('An Invented Record', 1999)",
                [],
            )
            .unwrap();
    }

    #[test]
    fn running_again_changes_nothing() {
        let mut connection = Connection::open_in_memory().unwrap();
        let clock = FixedClock::at(1_000);
        run(&mut connection, &[FIRST, SECOND], &clock).unwrap();
        let before = ledger(&connection);

        clock.set(2_000);
        let again = run(&mut connection, &[FIRST, SECOND], &clock).unwrap();
        assert_eq!(
            again,
            Migrated {
                applied: 0,
                version: 2
            }
        );
        assert_eq!(ledger(&connection), before);
    }

    #[test]
    fn a_later_version_of_the_program_applies_only_what_is_new() {
        let mut connection = Connection::open_in_memory().unwrap();
        let clock = FixedClock::at(1_000);
        run(&mut connection, &[FIRST], &clock).unwrap();

        clock.set(2_000);
        let migrated = run(&mut connection, &[FIRST, SECOND], &clock).unwrap();
        assert_eq!(
            migrated,
            Migrated {
                applied: 1,
                version: 2
            }
        );
        assert_eq!(
            ledger(&connection),
            [
                (1, "0001_first".to_owned(), 1_000),
                (2, "0002_second".to_owned(), 2_000),
            ]
        );
    }

    #[test]
    fn a_shipped_migration_that_changed_is_refused() {
        let mut connection = Connection::open_in_memory().unwrap();
        let clock = FixedClock::at(1_000);
        run(&mut connection, &[FIRST, SECOND], &clock).unwrap();

        let edited = Migration {
            sql: "ALTER TABLE fixture_album ADD COLUMN year TEXT;",
            ..SECOND
        };
        let refused = run(&mut connection, &[FIRST, edited], &clock);
        assert!(
            matches!(refused, Err(Error::ChangedMigration { version: 2 })),
            "{refused:?}"
        );
    }

    #[test]
    fn a_changed_migration_is_refused_before_a_new_one_is_applied() {
        let mut connection = Connection::open_in_memory().unwrap();
        let clock = FixedClock::at(1_000);
        run(&mut connection, &[FIRST], &clock).unwrap();

        let edited = Migration {
            sql: "CREATE TABLE schema_migration (version INTEGER PRIMARY KEY);",
            ..FIRST
        };
        let refused = run(&mut connection, &[edited, SECOND], &clock);
        assert!(
            matches!(refused, Err(Error::ChangedMigration { version: 1 })),
            "{refused:?}"
        );
        assert_eq!(ledger(&connection).len(), 1);
    }

    #[test]
    fn a_database_from_a_newer_program_is_refused_and_left_alone() {
        let mut connection = Connection::open_in_memory().unwrap();
        let clock = FixedClock::at(1_000);
        run(&mut connection, &[FIRST, SECOND], &clock).unwrap();

        let refused = run(&mut connection, &[FIRST], &clock);
        assert!(
            matches!(refused, Err(Error::NewerSchema { found: 2, known: 1 })),
            "{refused:?}"
        );
        assert_eq!(ledger(&connection).len(), 2);
    }

    #[test]
    fn a_database_something_else_made_is_refused_and_left_alone() {
        let mut connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch("CREATE TABLE somebody_elses (id INTEGER PRIMARY KEY)")
            .unwrap();
        let refused = run(&mut connection, &[FIRST], &FixedClock::at(1_000));
        assert!(
            matches!(refused, Err(Error::ForeignDatabase)),
            "{refused:?}"
        );
        assert_eq!(tables(&connection), ["somebody_elses"]);
    }

    #[test]
    fn a_ledger_with_a_missing_number_is_refused() {
        let mut connection = Connection::open_in_memory().unwrap();
        let clock = FixedClock::at(1_000);
        run(&mut connection, &[FIRST, SECOND], &clock).unwrap();
        connection
            .execute("DELETE FROM schema_migration WHERE version = 1", [])
            .unwrap();
        let refused = run(&mut connection, &[FIRST, SECOND], &clock);
        assert!(
            matches!(refused, Err(Error::MigrationGap { missing: 1 })),
            "{refused:?}"
        );
    }

    #[test]
    fn a_migration_that_fails_leaves_the_database_at_the_one_before() {
        let mut connection = Connection::open_in_memory().unwrap();
        let clock = FixedClock::at(1_000);
        let failed = run(&mut connection, &[FIRST, SECOND, BROKEN_THIRD], &clock);
        assert!(
            matches!(failed, Err(Error::MigrationFailed { version: 3, .. })),
            "{failed:?}"
        );
        assert_eq!(ledger(&connection).len(), 2);
        // The table the broken migration made before it failed is gone too.
        assert_eq!(tables(&connection), ["fixture_album", "schema_migration"]);
    }

    /// The steps of the plan SQLite makes for `sql`.
    fn plan(connection: &Connection, sql: &str) -> Vec<String> {
        connection
            .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
            .unwrap()
            .query_map([1], |row| row.get(3))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    #[test]
    fn a_files_claims_and_an_accounts_loves_are_found_without_reading_every_row() {
        let mut connection = Connection::open_in_memory().unwrap();
        run(&mut connection, LIBRARY, &FixedClock::at(0)).unwrap();
        // A rescan clears each changed file's claims before it writes them
        // again, and an import of loves reads the newest of each.
        for sql in [
            "DELETE FROM metadata_claim WHERE source_item_id = ?1",
            "DELETE FROM identifier_claim WHERE source_item_id = ?1",
            crate::import_store::LOVED_NOW,
        ] {
            let plan = plan(&connection, sql);
            assert!(
                plan.iter().all(|step| !step.starts_with("SCAN")),
                "{sql}: {plan:?}"
            );
        }
    }

    #[test]
    fn the_shipped_migrations_are_numbered_from_1_and_named_by_number() {
        for database in [SERVER, LIBRARY] {
            for (index, migration) in database.iter().enumerate() {
                let number = i64::try_from(index).unwrap() + 1;
                assert_eq!(migration.version, number);
                assert!(
                    migration.name.starts_with(&format!("{number:04}_")),
                    "{}",
                    migration.name
                );
            }
        }
    }
}
