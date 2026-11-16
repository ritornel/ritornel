//! The schema snapshots in `schema/` at the repository root: what applying
//! every migration to an empty database produces. A test here fails when a
//! snapshot and the migrations disagree, so CI catches a schema that changed
//! without anyone looking at the result.
//!
//! To write the snapshots again after adding a migration: `mise run schema`.

use std::path::Path;

use rusqlite::Connection;

use crate::migrate::{self, Migration};
use crate::testing::FixedClock;

/// The environment variable that makes the test write the snapshot instead
/// of comparing it.
const BLESS: &str = "RITORNEL_BLESS";

/// The schema of a database that has had `migrations` applied, as text.
///
/// It is every `CREATE` statement exactly as SQLite keeps it, in the order
/// the objects were created. That order and that text depend only on the
/// migrations, so two runs give the same bytes. Comments inside a statement
/// are kept; comments between statements are not, because SQLite does not
/// keep them.
///
/// Above the statements is the ledger: each migration with the checksum the
/// runner recorded for it. A migration can be edited without changing the
/// schema, by a comment for one, and every database it was applied to would
/// then be refused. The checksum makes that edit fail the test too.
fn dump(database: &str, migrations: &[Migration]) -> String {
    let mut connection = Connection::open_in_memory().unwrap();
    migrate::run(&mut connection, migrations, &FixedClock::at(0)).unwrap();
    let column = |sql: &str| -> Vec<String> {
        connection
            .prepare(sql)
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };

    let mut text = format!(
        "-- {database}: the schema after every migration, as SQLite keeps it.\n\
         -- Generated from crates/ritornel-engine/migrations. Do not edit it: change the\n\
         -- schema with a new migration, then run `mise run schema`.\n\
         --\n\
         -- Migrations, each with the SHA-256 recorded when it is applied. A checksum\n\
         -- that changes here means a migration that has shipped was edited.\n"
    );
    for migration in
        column("SELECT name || '  ' || lower(hex(checksum)) FROM schema_migration ORDER BY version")
    {
        text.push_str(&format!("--   {migration}\n"));
    }
    for statement in column("SELECT sql FROM sqlite_schema WHERE sql IS NOT NULL ORDER BY rowid") {
        text.push('\n');
        text.push_str(&statement);
        text.push_str(";\n");
    }
    text
}

fn check(file: &str, database: &str, migrations: &[Migration]) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../schema")
        .join(file);
    let produced = dump(database, migrations);
    if std::env::var_os(BLESS).is_some() {
        std::fs::write(&path, &produced).unwrap();
        return;
    }
    let committed = std::fs::read_to_string(&path).unwrap_or_default();
    if committed != produced {
        let line = committed
            .lines()
            .zip(produced.lines())
            .position(|(committed, produced)| committed != produced)
            .unwrap_or_else(|| committed.lines().count().min(produced.lines().count()));
        panic!(
            "schema/{file} is not what the migrations produce; they first differ at line {}.\n\
             If the migrations are right, run `mise run schema` and commit the result.",
            line + 1
        );
    }
}

#[test]
fn server_schema_matches_the_committed_snapshot() {
    check("server.sql", "server.db", migrate::SERVER);
}
