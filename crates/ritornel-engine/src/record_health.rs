//! What says the record has stopped advancing, as far as the databases
//! hold it: listens a server could not deliver to history, imports that
//! failed, and scrobbles that may not have reached the service. A listen
//! the player could not save at all is known only to the running server.
//! See "Keeping the record" in docs/design/server.md.

use rusqlite::OptionalExtension;

use crate::{Error, LibraryStore, ServerStore};

/// The `server_setting` row that says which scrobbles that ended unknown
/// or withdrawn have been seen and set aside: those that ended by a
/// library revision. A revision only rises, whatever a clock does.
const DISMISSED: &str = "record.dismissed";

/// The last dismissal: through which library revision, by which command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Dismissal {
    pub through_revision: i64,
    pub command: crate::Id,
}

/// Something that has gone wrong since a time, and is still wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failing {
    /// When it first went wrong, as Unix milliseconds.
    pub since_ms: i64,
    /// How many.
    pub count: u64,
    /// What the newest said, if it said anything.
    pub detail: Option<String>,
}

fn count(n: i64) -> u64 {
    u64::try_from(n).unwrap_or(0)
}

impl ServerStore {
    /// The listens waiting in the outbox that have failed to reach history
    /// at least once: they are tried again, and are not lost, but are not
    /// in history yet.
    pub fn failing_deliveries(&self) -> Result<Option<Failing>, Error> {
        self.read(|connection| {
            let (n, since): (i64, Option<i64>) = connection.query_row(
                "SELECT count(*), min(created_at_ms) FROM outbox WHERE last_error IS NOT NULL",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            let Some(since_ms) = since else {
                return Ok(None);
            };
            let detail = connection
                .query_row(
                    "SELECT last_error FROM outbox WHERE last_error IS NOT NULL
                     ORDER BY id DESC LIMIT 1",
                    [],
                    |row| row.get(0),
                )
                .optional()?;
            Ok(Some(Failing {
                since_ms,
                count: count(n),
                detail,
            }))
        })
    }

    /// The jobs of `kind` for `key`, such as one account's imports, that
    /// failed since the last that succeeded: none once one succeeds again.
    pub fn failing_jobs(&self, kind: &str, key: &str) -> Result<Option<Failing>, Error> {
        let (kind, key) = (kind.to_owned(), key.to_owned());
        self.read(move |connection| {
            let (n, since): (i64, Option<i64>) = connection.query_row(
                "SELECT count(*), min(finished_at_ms) FROM job
                 WHERE kind = ?1 AND dedupe_key = ?2 AND state = 'failed'
                   AND finished_at_ms > coalesce(
                     (SELECT max(finished_at_ms) FROM job
                      WHERE kind = ?1 AND dedupe_key = ?2 AND state = 'succeeded'),
                     -1)",
                [&kind, &key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            let Some(since_ms) = since else {
                return Ok(None);
            };
            let detail: Option<String> = connection
                .query_row(
                    "SELECT error_json FROM job WHERE kind = ?1 AND dedupe_key = ?2
                       AND state = 'failed'
                     ORDER BY finished_at_ms DESC, id DESC LIMIT 1",
                    [&kind, &key],
                    |row| row.get(0),
                )
                .optional()?
                .flatten();
            Ok(Some(Failing {
                since_ms,
                count: count(n),
                // The error is stored as JSON text; a plain string is shown
                // as itself.
                detail: detail.map(|json| serde_json::from_str::<String>(&json).unwrap_or(json)),
            }))
        })
    }

    /// The last dismissal of the scrobbles that ended unknown or
    /// withdrawn: none that ended by its revision count against the record.
    pub fn record_dismissal(&self) -> Result<Option<Dismissal>, Error> {
        self.read(held_dismissal)
    }

    /// Sets aside every scrobble that ended unknown or withdrawn by the
    /// library revision of `dismissal`, and answers the dismissal in force.
    /// The same command again changes nothing, so a client that sends it
    /// again for want of an answer hides nothing that ended since. A
    /// dismissal never moves back.
    pub fn dismiss_record(&self, dismissal: Dismissal) -> Result<Dismissal, Error> {
        self.transact(move |transaction, _, _| {
            if let Some(held) = held_dismissal(transaction)?
                && (held.command == dismissal.command
                    || held.through_revision >= dismissal.through_revision)
            {
                return Ok(held);
            }
            transaction.execute(
                "INSERT INTO server_setting (key, value_json) VALUES (?1, ?2)
                 ON CONFLICT (key) DO UPDATE SET value_json = excluded.value_json",
                (DISMISSED, serde_json::to_string(&dismissal)?),
            )?;
            Ok(dismissal)
        })
    }
}

fn held_dismissal(connection: &rusqlite::Connection) -> Result<Option<Dismissal>, Error> {
    let value: Option<String> = connection
        .query_row(
            "SELECT value_json FROM server_setting WHERE key = ?1",
            [DISMISSED],
            |row| row.get(0),
        )
        .optional()?;
    Ok(value.map(|json| serde_json::from_str(&json)).transpose()?)
}

/// The scrobbles that may not have reached the service, by how they ended.
/// One whose echo an import has brought back since, joined to its play,
/// did reach it, and is not among them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LostScrobbles {
    /// Sent, and never answered: the service may or may not have them.
    pub unknown: Option<Failing>,
    /// Never sent, because their play stopped being one to send. Those the
    /// account already had from another player are not among them.
    pub withdrawn: Option<Failing>,
}

impl LibraryStore {
    /// The scrobbles that ended unknown or withdrawn after the library
    /// revision `after`, and
    /// whose echo has not come back.
    pub fn lost_scrobbles(&self, after: i64) -> Result<LostScrobbles, Error> {
        self.read(move |connection| {
            let mut statement = connection.prepare(
                "WITH lost AS (
                   SELECT d.id, d.updated_at_ms, d.detail FROM history_delivery d
                   WHERE d.state = ?1 AND d.rev > ?2 AND coalesce(d.detail, '') <> ?3
                     -- Its echo, imported from the account it was sent to, joined its play.
                     AND NOT EXISTS (
                       SELECT 1 FROM play_event_observation mine
                       JOIN play_event_observation theirs ON theirs.play_event_id = mine.play_event_id
                       JOIN history_observation echo ON echo.id = theirs.observation_id
                       WHERE mine.observation_id = d.observation_id
                         AND echo.endpoint_id = d.endpoint_id AND echo.kind = 'import'))
                 SELECT count(*), min(updated_at_ms),
                        (SELECT detail FROM lost ORDER BY updated_at_ms DESC, id DESC LIMIT 1)
                 FROM lost",
            )?;
            let mut of = |state: &str| -> Result<Option<Failing>, Error> {
                let (n, since, detail): (i64, Option<i64>, Option<String>) = statement.query_row(
                    (state, after, crate::delivery_store::ALREADY_SCROBBLED),
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )?;
                Ok(since.map(|since_ms| Failing {
                    since_ms,
                    count: count(n),
                    detail,
                }))
            };
            Ok(LostScrobbles {
                unknown: of("unknown")?,
                withdrawn: of("withdrawn")?,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::testing::{CountingEntropy, FixedClock};

    fn store() -> ServerStore {
        ServerStore::open_in_memory(
            Arc::new(FixedClock::at(1_794_776_400_000)),
            Arc::new(CountingEntropy::default()),
        )
        .unwrap()
    }

    fn job(
        store: &ServerStore,
        n: u8,
        state: &'static str,
        at_ms: i64,
        error: Option<&'static str>,
    ) {
        account_job(store, n, "someone", state, at_ms, error);
    }

    fn account_job(
        store: &ServerStore,
        n: u8,
        account: &'static str,
        state: &'static str,
        at_ms: i64,
        error: Option<&'static str>,
    ) {
        store
            .transact(move |transaction, _, _| {
                transaction.execute(
                    "INSERT INTO job (public_id, kind, dedupe_key, params_json, state, error_json,
                                      created_at_ms, finished_at_ms)
                     VALUES (?1, 'lastfm.sync', ?5, '{}', ?2, ?3, ?4, ?4)",
                    rusqlite::params![[n; 16], state, error, at_ms, account],
                )?;
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn imports_fail_from_the_first_failure_after_the_last_success_until_the_next() {
        let store = store();
        assert_eq!(store.failing_jobs("lastfm.sync", "someone").unwrap(), None);
        job(&store, 1, "failed", 1_000, Some("\"long ago\""));
        job(&store, 2, "succeeded", 2_000, None);
        assert_eq!(store.failing_jobs("lastfm.sync", "someone").unwrap(), None);
        job(
            &store,
            3,
            "failed",
            3_000,
            Some("\"Last.fm did not answer\""),
        );
        job(&store, 4, "cancelled", 3_500, None);
        job(
            &store,
            5,
            "failed",
            4_000,
            Some("the service refused the key"),
        );
        assert_eq!(
            store.failing_jobs("lastfm.sync", "someone").unwrap(),
            Some(Failing {
                since_ms: 3_000,
                count: 2,
                detail: Some("the service refused the key".to_owned()),
            })
        );
        // Another kind of job is not an import.
        assert_eq!(store.failing_jobs("scan", "someone").unwrap(), None);
        job(&store, 6, "succeeded", 5_000, None);
        assert_eq!(store.failing_jobs("lastfm.sync", "someone").unwrap(), None);
    }

    #[test]
    fn listens_fail_to_reach_history_while_the_outbox_holds_an_error() {
        let store = store();
        assert_eq!(store.failing_deliveries().unwrap(), None);
        store
            .transact(|transaction, _, _| {
                transaction.execute_batch(
                    "INSERT INTO outbox (kind, dedupe_key, attempts, next_attempt_at_ms, last_error,
                                         created_at_ms)
                     VALUES ('history.ingest', x'01', 0, 0, NULL, 500),
                            ('history.ingest', x'02', 2, 0, 'disk I/O error', 700),
                            ('history.ingest', x'03', 1, 0, 'database is locked', 900);",
                )?;
                Ok(())
            })
            .unwrap();
        assert_eq!(
            store.failing_deliveries().unwrap(),
            Some(Failing {
                since_ms: 700,
                count: 2,
                detail: Some("database is locked".to_owned()),
            })
        );
    }

    #[test]
    fn one_accounts_imports_are_not_put_right_by_anothers() {
        let store = store();
        account_job(&store, 1, "alice", "failed", 1_000, Some("refused"));
        account_job(&store, 2, "bob", "succeeded", 2_000, None);
        assert_eq!(
            store
                .failing_jobs("lastfm.sync", "alice")
                .unwrap()
                .map(|failing| failing.since_ms),
            Some(1_000)
        );
        assert_eq!(store.failing_jobs("lastfm.sync", "bob").unwrap(), None);
    }

    #[test]
    fn a_dismissal_is_kept_never_moves_back_and_is_not_moved_by_its_own_command_again() {
        let store = store();
        assert_eq!(store.record_dismissal().unwrap(), None);
        let first = Dismissal {
            through_revision: 10,
            command: crate::Id::from_bytes([1; 16]),
        };
        assert_eq!(store.dismiss_record(first).unwrap(), first);
        // The same command, sent again when the library has moved on.
        let again = Dismissal {
            through_revision: 20,
            ..first
        };
        assert_eq!(store.dismiss_record(again).unwrap(), first);
        // Another, through an earlier revision, moves nothing back.
        let earlier = Dismissal {
            through_revision: 5,
            command: crate::Id::from_bytes([2; 16]),
        };
        assert_eq!(store.dismiss_record(earlier).unwrap(), first);
        let later = Dismissal {
            through_revision: 30,
            command: crate::Id::from_bytes([3; 16]),
        };
        assert_eq!(store.dismiss_record(later).unwrap(), later);
        assert_eq!(store.record_dismissal().unwrap(), Some(later));
    }
}
