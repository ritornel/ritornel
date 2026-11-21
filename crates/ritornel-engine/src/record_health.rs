//! What says the record has stopped advancing, as far as the databases
//! hold it: listens a server could not deliver to history, imports that
//! failed, and scrobbles that may not have reached the service. A listen
//! the player could not save at all is known only to the running server.
//! See "Keeping the record" in docs/design/server.md.

use rusqlite::OptionalExtension;

use crate::{Error, LibraryStore, ServerStore};

/// The `server_setting` row that says until when the scrobbles that ended
/// unknown or withdrawn have been seen and set aside.
const DISMISSED: &str = "record.dismissed_through_ms";

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

    /// The jobs of `kind` that failed since the last that succeeded: none
    /// once one succeeds again.
    pub fn failing_jobs(&self, kind: &str) -> Result<Option<Failing>, Error> {
        let kind = kind.to_owned();
        self.read(move |connection| {
            let (n, since): (i64, Option<i64>) = connection.query_row(
                "SELECT count(*), min(finished_at_ms) FROM job
                 WHERE kind = ?1 AND state = 'failed'
                   AND finished_at_ms > coalesce(
                     (SELECT max(finished_at_ms) FROM job WHERE kind = ?1 AND state = 'succeeded'),
                     -1)",
                [&kind],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            let Some(since_ms) = since else {
                return Ok(None);
            };
            let detail: Option<String> = connection
                .query_row(
                    "SELECT error_json FROM job WHERE kind = ?1 AND state = 'failed'
                     ORDER BY finished_at_ms DESC, id DESC LIMIT 1",
                    [&kind],
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

    /// Until when the scrobbles that ended unknown or withdrawn have been
    /// set aside: none before it count against the record.
    pub fn record_dismissed_through(&self) -> Result<Option<i64>, Error> {
        self.read(|connection| {
            let value: Option<String> = connection
                .query_row(
                    "SELECT value_json FROM server_setting WHERE key = ?1",
                    [DISMISSED],
                    |row| row.get(0),
                )
                .optional()?;
            Ok(value.and_then(|value| value.parse().ok()))
        })
    }

    /// Sets aside every scrobble that ended unknown or withdrawn until
    /// `through_ms`.
    pub fn dismiss_record_through(&self, through_ms: i64) -> Result<(), Error> {
        self.transact(move |transaction, _, _| {
            transaction.execute(
                "INSERT INTO server_setting (key, value_json) VALUES (?1, ?2)
                 ON CONFLICT (key) DO UPDATE SET value_json = excluded.value_json",
                (DISMISSED, through_ms.to_string()),
            )?;
            Ok(())
        })
    }
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
    /// The scrobbles that ended unknown or withdrawn after `after_ms`, and
    /// whose echo has not come back.
    pub fn lost_scrobbles(&self, after_ms: i64) -> Result<LostScrobbles, Error> {
        self.read(move |connection| {
            let mut statement = connection.prepare(
                "WITH lost AS (
                   SELECT d.id, d.updated_at_ms, d.detail FROM history_delivery d
                   WHERE d.state = ?1 AND d.updated_at_ms > ?2 AND coalesce(d.detail, '') <> ?3
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
                    (state, after_ms, crate::delivery_store::ALREADY_SCROBBLED),
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
        store
            .transact(move |transaction, _, _| {
                transaction.execute(
                    "INSERT INTO job (public_id, kind, params_json, state, error_json, created_at_ms,
                                      finished_at_ms)
                     VALUES (?1, 'lastfm.sync', '{}', ?2, ?3, ?4, ?4)",
                    rusqlite::params![[n; 16], state, error, at_ms],
                )?;
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn imports_fail_from_the_first_failure_after_the_last_success_until_the_next() {
        let store = store();
        assert_eq!(store.failing_jobs("lastfm.sync").unwrap(), None);
        job(&store, 1, "failed", 1_000, Some("\"long ago\""));
        job(&store, 2, "succeeded", 2_000, None);
        assert_eq!(store.failing_jobs("lastfm.sync").unwrap(), None);
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
            store.failing_jobs("lastfm.sync").unwrap(),
            Some(Failing {
                since_ms: 3_000,
                count: 2,
                detail: Some("the service refused the key".to_owned()),
            })
        );
        // Another kind of job is not an import.
        assert_eq!(store.failing_jobs("scan").unwrap(), None);
        job(&store, 6, "succeeded", 5_000, None);
        assert_eq!(store.failing_jobs("lastfm.sync").unwrap(), None);
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
    fn a_dismissal_is_kept() {
        let store = store();
        assert_eq!(store.record_dismissed_through().unwrap(), None);
        store.dismiss_record_through(1_234).unwrap();
        store.dismiss_record_through(5_678).unwrap();
        assert_eq!(store.record_dismissed_through().unwrap(), Some(5_678));
    }
}
