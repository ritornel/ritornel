//! What says the record has stopped advancing, as far as the databases
//! hold it: listens a server could not deliver to history, imports that
//! failed, and scrobbles that may not have reached the service. A listen
//! the player could not save at all is known only to the running server.
//! See "Record health" in docs/internals/server.md.

use rusqlite::OptionalExtension;

use crate::{Error, LibraryStore, ServerStore};

/// The `server_setting` row that says which scrobbles that ended unknown
/// or withdrawn have been seen and set aside: those that ended by a
/// library revision. A revision only rises, whatever a clock does.
const DISMISSED: &str = "record.dismissed";

/// A dismissal: through which library revision, by which command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Dismissal {
    pub through_revision: i64,
    pub command: crate::Id,
}

/// How many of the last dismissals' commands are kept, so that one sent
/// again, however late, is known.
const COMMANDS_KEPT: usize = 64;

/// What is kept of the dismissals: the one in force, and the commands of
/// the last [`COMMANDS_KEPT`].
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct Held {
    through_revision: i64,
    command: crate::Id,
    #[serde(default)]
    commands: Vec<crate::Id>,
}

impl Held {
    fn dismissal(&self) -> Dismissal {
        Dismissal {
            through_revision: self.through_revision,
            command: self.command,
        }
    }
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
                "SELECT count(*), min(created_at_ms) FROM outbox
                 WHERE kind = 'history.ingest' AND last_error IS NOT NULL",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            let Some(since_ms) = since else {
                return Ok(None);
            };
            let detail = connection
                .query_row(
                    "SELECT last_error FROM outbox
                     WHERE kind = 'history.ingest' AND last_error IS NOT NULL
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
            // In the order the jobs were made, which no clock can change:
            // one job of a kind and key runs at a time.
            let (n, since, detail): (i64, Option<i64>, Option<String>) = connection.query_row(
                "WITH last AS (
                   SELECT coalesce(max(id), 0) AS id FROM job
                   WHERE kind = ?1 AND dedupe_key = ?2 AND state = 'succeeded'),
                 failed AS (
                   SELECT job.id, finished_at_ms, error_json FROM job, last
                   WHERE kind = ?1 AND dedupe_key = ?2 AND state = 'failed' AND job.id > last.id)
                 SELECT count(*),
                        (SELECT finished_at_ms FROM failed ORDER BY id LIMIT 1),
                        (SELECT error_json FROM failed ORDER BY id DESC LIMIT 1)
                 FROM failed",
                [&kind, &key],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
            if n == 0 {
                return Ok(None);
            }
            Ok(Some(Failing {
                since_ms: since.unwrap_or(0),
                count: count(n),
                // The error is stored as JSON text; a plain string is shown
                // as itself.
                detail: detail.map(|json| serde_json::from_str::<String>(&json).unwrap_or(json)),
            }))
        })
    }

    /// Since when the service has refused this server's session, while it
    /// does: nothing is scrobbled until a new sign-in.
    pub fn session_refused(&self, service: &str) -> Result<Option<i64>, Error> {
        let service = service.to_owned();
        self.read(move |connection| {
            let refused: Option<(bool, Option<String>, i64)> = connection
                .query_row(
                    "SELECT s.sign_in_again, t.value_json, s.signed_in_at_ms
                     FROM service_session s
                     LEFT JOIN server_setting t ON t.key = s.service || '.refused_at_ms'
                     WHERE s.service = ?1",
                    [&service],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()?;
            // A refusal recorded before its time was kept, or one older than
            // the session (a time kept for an earlier one), is said from the
            // sign-in: the earliest it can have been.
            Ok(match refused {
                Some((true, at, signed_in)) => Some(
                    at.and_then(|at| at.parse::<i64>().ok())
                        .filter(|at| *at >= signed_in)
                        .unwrap_or(signed_in),
                ),
                _ => None,
            })
        })
    }

    /// The last dismissal of the scrobbles that ended unknown,
    /// withdrawn or refused: none that ended by its revision count against the record.
    pub fn record_dismissal(&self) -> Result<Option<Dismissal>, Error> {
        self.read(|connection| Ok(held_dismissal(connection)?.map(|held| held.dismissal())))
    }

    /// Sets aside every scrobble that ended unknown, withdrawn or refused by the
    /// library revision of `dismissal`, and answers the dismissal in force.
    /// A command already seen changes nothing, so a client that sends one
    /// again for want of an answer, however late, hides nothing that ended
    /// since. A dismissal never moves back.
    pub fn dismiss_record(&self, dismissal: Dismissal) -> Result<Dismissal, Error> {
        self.transact(move |transaction, _, _| {
            let held = held_dismissal(transaction)?;
            if let Some(held) = &held
                && (held.command == dismissal.command || held.commands.contains(&dismissal.command))
            {
                return Ok(held.dismissal());
            }
            // A command not seen before is kept even when it moves nothing,
            // so that it moves nothing when it is sent again later either.
            let kept_revision = held.as_ref().map_or(i64::MIN, |held| held.through_revision);
            let (through_revision, command) = if kept_revision >= dismissal.through_revision {
                held.as_ref()
                    .map_or((dismissal.through_revision, dismissal.command), |held| {
                        (held.through_revision, held.command)
                    })
            } else {
                (dismissal.through_revision, dismissal.command)
            };
            let mut commands = held.map(|held| held.commands).unwrap_or_default();
            commands.push(dismissal.command);
            let excess = commands.len().saturating_sub(COMMANDS_KEPT);
            commands.drain(..excess);
            let kept = Held {
                through_revision,
                command,
                commands,
            };
            transaction.execute(
                "INSERT INTO server_setting (key, value_json) VALUES (?1, ?2)
                 ON CONFLICT (key) DO UPDATE SET value_json = excluded.value_json",
                (DISMISSED, serde_json::to_string(&kept)?),
            )?;
            Ok(kept.dismissal())
        })
    }
}

fn held_dismissal(connection: &rusqlite::Connection) -> Result<Option<Held>, Error> {
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
    /// Refused with the request that carried them, which is never sent
    /// again: a key the service no longer takes, or a request it cannot
    /// read. Only those since the account last took a scrobble: once one
    /// is taken, what refused them is put right.
    pub refused: Option<Failing>,
}

impl LibraryStore {
    /// The scrobbles that ended unknown, withdrawn or refused after the
    /// library revision `after`, and whose echo has not come back. Those
    /// refused are only of the account `signed_in`, the one there is to put
    /// right; with none signed in there are none.
    pub fn lost_scrobbles(
        &self,
        after: i64,
        signed_in: Option<&str>,
    ) -> Result<LostScrobbles, Error> {
        let signed_in = signed_in.map(str::to_owned);
        self.read(move |connection| {
            let mut statement = connection.prepare(
                "WITH lost AS (
                   SELECT d.id, d.updated_at_ms, d.detail FROM history_delivery d
                   LEFT JOIN (
                     -- The last each account took, found once and not for each row.
                     SELECT endpoint_id, max(rev) AS rev FROM history_delivery
                     WHERE ?1 = 'refused' AND state = 'accepted' GROUP BY endpoint_id) taken
                     ON taken.endpoint_id = d.endpoint_id
                   WHERE d.state = ?1 AND d.rev > ?2 AND coalesce(d.detail, '') <> ?3
                     -- A refusal is the signed-in account's, since the last it
                     -- took; another account's is not its to put right.
                     AND (d.state <> 'refused' OR (
                       d.endpoint_id = (SELECT endpoint_id FROM service_account
                                        WHERE service = 'lastfm'
                                          AND account_name = ?4 COLLATE NOCASE)
                       AND d.rev > coalesce(taken.rev, 0)))
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
                    (
                        state,
                        after,
                        crate::delivery_store::ALREADY_SCROBBLED,
                        signed_in.as_deref(),
                    ),
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
                refused: of("refused")?,
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
    fn imports_are_taken_in_the_order_they_were_made_whatever_the_clock_said() {
        let store = store();
        // The clock went back between a failure and the success after it.
        job(&store, 1, "failed", 5_000, Some("refused"));
        job(&store, 2, "succeeded", 4_000, None);
        assert_eq!(store.failing_jobs("lastfm.sync", "someone").unwrap(), None);
        // A failure in the same millisecond as the success before it.
        job(&store, 3, "failed", 4_000, Some("refused again"));
        assert_eq!(
            store
                .failing_jobs("lastfm.sync", "someone")
                .unwrap()
                .map(|failing| (failing.since_ms, failing.count)),
            Some((4_000, 1))
        );
    }

    #[test]
    fn a_refused_session_is_said_from_when_it_was_first_refused_until_a_new_sign_in() {
        let store = store();
        assert_eq!(store.session_refused("lastfm").unwrap(), None);
        let save = || {
            store
                .save_service_session(
                    "lastfm",
                    crate::Id::from_bytes([7; 16]),
                    "someone",
                    &crate::ServiceKey::new("invented"),
                    false,
                )
                .unwrap();
        };
        save();
        assert_eq!(store.session_refused("lastfm").unwrap(), None);
        // A refusal that came late, for a session signed out of since, is
        // not this one's.
        store
            .refuse_service_session("lastfm", "an older key")
            .unwrap();
        assert_eq!(store.session_refused("lastfm").unwrap(), None);
        store.refuse_service_session("lastfm", "invented").unwrap();
        assert_eq!(
            store.session_refused("lastfm").unwrap(),
            Some(1_794_776_400_000)
        );
        // Refused again, it is still since the first time.
        store.refuse_service_session("lastfm", "invented").unwrap();
        assert_eq!(
            store.session_refused("lastfm").unwrap(),
            Some(1_794_776_400_000)
        );
        save();
        assert_eq!(store.session_refused("lastfm").unwrap(), None);
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
        // Another, through the same revision: it moves nothing, and sent
        // again later it moves nothing either.
        let idle = Dismissal {
            through_revision: 10,
            command: crate::Id::from_bytes([4; 16]),
        };
        assert_eq!(store.dismiss_record(idle).unwrap(), first);
        let idle_again = Dismissal {
            through_revision: 25,
            ..idle
        };
        assert_eq!(store.dismiss_record(idle_again).unwrap(), first);
        let later = Dismissal {
            through_revision: 30,
            command: crate::Id::from_bytes([3; 16]),
        };
        assert_eq!(store.dismiss_record(later).unwrap(), later);
        assert_eq!(store.record_dismissal().unwrap(), Some(later));
        // The first again, long after, as a client that never had its
        // answer might send it: it hides nothing lost since the last.
        let late_retry = Dismissal {
            through_revision: 40,
            ..first
        };
        assert_eq!(store.dismiss_record(late_retry).unwrap(), later);
    }
}
