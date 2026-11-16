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

#[test]
fn library_schema_matches_the_committed_snapshot() {
    check("library.sql", "library.db", migrate::LIBRARY);
}

/// A library after every migration.
fn library() -> Connection {
    let mut connection = Connection::open_in_memory().unwrap();
    migrate::run(&mut connection, migrate::LIBRARY, &FixedClock::at(0)).unwrap();
    connection
}

fn names(connection: &Connection, sql: &str) -> Vec<String> {
    connection
        .prepare(sql)
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

/// The tables that do not carry the library revision they last changed at:
/// a `rev` column that is an integer and never null. The migration ledger
/// is exempt, because the runner writes it outside any revision. See
/// decision 22 and "Testing and CI" in docs/design/server.md.
fn tables_without_rev(connection: &Connection) -> Vec<String> {
    names(
        connection,
        "SELECT t.name FROM pragma_table_list AS t
         WHERE t.schema = 'main' AND t.type = 'table' AND t.name NOT LIKE 'sqlite_%'
           AND t.name <> 'schema_migration'
           AND NOT EXISTS (
             SELECT 1 FROM pragma_table_info(t.name) AS c
             WHERE c.name = 'rev' AND c.type = 'INTEGER' AND (c.\"notnull\" = 1 OR c.pk = 1))
         ORDER BY t.name",
    )
}

#[test]
fn every_library_table_carries_the_revision_it_last_changed_at() {
    let library = library();
    assert_eq!(tables_without_rev(&library), Vec::<String>::new());

    // The other exemption is the text search index. It is a virtual table,
    // so it and the tables FTS5 keeps for it are not tables to this check,
    // and nothing else may be exempt that way.
    assert_eq!(
        names(
            &library,
            "SELECT name FROM pragma_table_list WHERE schema = 'main' AND type = 'virtual'"
        ),
        ["search_text"]
    );
    assert_eq!(
        names(
            &library,
            "SELECT name FROM pragma_table_list
             WHERE schema = 'main' AND type NOT IN ('table', 'view', 'virtual')
               AND name NOT LIKE 'search\\_text\\_%' ESCAPE '\\'"
        ),
        Vec::<String>::new()
    );
}

#[test]
fn a_library_table_without_the_revision_column_is_caught() {
    let library = library();
    library
        .execute_batch(
            "CREATE TABLE fixture_none (id INTEGER PRIMARY KEY, title TEXT NOT NULL) STRICT;
             CREATE TABLE fixture_nullable (id INTEGER PRIMARY KEY, rev INTEGER) STRICT;
             CREATE TABLE fixture_text (id INTEGER PRIMARY KEY, rev TEXT NOT NULL) STRICT;
             CREATE TABLE fixture_stamped (id INTEGER PRIMARY KEY, rev INTEGER NOT NULL) STRICT;",
        )
        .unwrap();
    assert_eq!(
        tables_without_rev(&library),
        ["fixture_none", "fixture_nullable", "fixture_text"]
    );
}

/// The search index needs FTS5 with the trigram tokenizer and its
/// `remove_diacritics` option. The migration would fail without them; this
/// says that the SQLite built in has them, and that they do what
/// docs/design/contract.md asks of search: a substring, whatever its case
/// and its accents.
#[test]
fn the_search_index_finds_a_substring_whatever_its_case_and_accents() {
    let library = library();
    library
        .execute(
            "INSERT INTO search_text (rowid, title, artist, album)
             VALUES (7, 'Señorita Über', 'Zoë Åström', 'An Invented Record')",
            [],
        )
        .unwrap();
    for (query, found) in [
        ("norita ub", 1),
        ("ZOE ASTROM", 1),
        ("artist : \"åström\"", 1),
        ("album : \"astrom\"", 0),
        ("vented rec", 1),
        ("no such song", 0),
    ] {
        let rows: Vec<i64> = library
            .prepare("SELECT rowid FROM search_text WHERE search_text MATCH ?1")
            .unwrap()
            .query_map([query], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(rows.len(), found, "{query}");
        assert!(rows.iter().all(|row| *row == 7), "{query}");
    }
}
