//! The player's rows in `server.db`: the queue, the one session, each
//! listen while it is open and until the hub has it, and the outbox row
//! that carries it there. See "The queue" and "Recording a listen" in
//! docs/design/playback.md, and "An effect, a job and a lease" in
//! docs/design/server.md.

use ritornel_domain::history::{ContextKind, EndReason, StartReason};
use rusqlite::{Connection, OptionalExtension, Row, Transaction};

use crate::jobs::{receipt, record_receipt};
use crate::{Command, Error, Id, ServerStore};

/// The kind of outbox row that delivers a listen to the hub.
pub(crate) const DELIVER_LISTEN: &str = "history.ingest";

/// One entry of the queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Entry {
    /// The row's key.
    pub(crate) row: i64,
    /// The contract's `entryId`.
    pub(crate) entry_id: Id,
    /// The track appearance it plays.
    pub(crate) appearance: Id,
    /// Why it was queued.
    pub(crate) context_kind: ContextKind,
    /// What it was queued from.
    pub(crate) context_ref: Option<Id>,
    /// The session that queued it, by its row in `session`.
    pub(crate) added_by: Option<i64>,
    /// Why it could not be played, once it could not.
    pub(crate) failed_reason: Option<String>,
}

/// An entry to put in the queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NewEntry {
    pub(crate) appearance: Id,
    pub(crate) context_kind: ContextKind,
    pub(crate) context_ref: Option<Id>,
    /// The session that queued it, by its public ID.
    pub(crate) added_by: Option<Id>,
}

/// The one row of the player.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Session {
    pub(crate) queue_revision: i64,
    pub(crate) state_revision: i64,
    /// The entry the player is on, by its row.
    pub(crate) current: Option<i64>,
    /// Where in it, as saved with the last checkpoint.
    pub(crate) position_ms: i64,
    /// The listen a resume of this entry would continue.
    pub(crate) resume_listen: Option<Id>,
    /// The listener set on the player.
    pub(crate) listener: Option<Id>,
}

/// Everything a staged listen holds that does not change while it is
/// open: written once, when the first frame is heard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Started {
    pub(crate) listen_id: Id,
    /// The queue entry it is heard from, by its `entryId`. `None` only for
    /// a listen staged, and closed, before the entry was kept.
    pub(crate) entry: Option<Id>,
    pub(crate) listener: Id,
    pub(crate) actor_session: Option<i64>,
    pub(crate) played_on_server: Id,
    pub(crate) appearance: Id,
    pub(crate) asset: Id,
    pub(crate) source_item: Id,
    pub(crate) source_revision: i64,
    pub(crate) started_at_ms: i64,
    pub(crate) zone: Option<String>,
    pub(crate) started_mono_ms: i64,
    pub(crate) boot: Id,
    pub(crate) start_reason: StartReason,
    pub(crate) continues: Option<Id>,
    pub(crate) start_position_ms: i64,
    pub(crate) track_duration_ms: i64,
    pub(crate) context_kind: ContextKind,
    pub(crate) context_ref: Option<Id>,
    pub(crate) shuffle: bool,
}

/// What a listen has heard so far: what each checkpoint saves.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Heard {
    pub(crate) ms_played: i64,
    /// The merged ranges, as JSON: `[[from, to], ...]`.
    pub(crate) ranges_json: String,
    /// Whether neighbours among them were joined to keep under the cap.
    /// Once they were, they stay approximate.
    pub(crate) ranges_approximate: bool,
    pub(crate) max_position_ms: i64,
    pub(crate) seek_count: i64,
    pub(crate) pause_count: i64,
}

/// How a listen ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Ended {
    pub(crate) end_position_ms: i64,
    pub(crate) end_reason: EndReason,
    pub(crate) ended_at_ms: i64,
    /// The monotonic clock then. `None` only for a listen staged before
    /// the clock was kept.
    pub(crate) ended_mono_ms: Option<i64>,
}

/// Where a listen's track is, by both clocks: what a checkpoint saves
/// besides what was heard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Mark {
    pub(crate) position_ms: i64,
    pub(crate) mono_ms: i64,
}

/// A listen as it is staged, whole.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Staged {
    pub(crate) started: Started,
    pub(crate) heard: Heard,
    /// `None` while it is open.
    pub(crate) ended: Option<Ended>,
    pub(crate) checkpoint_at_ms: i64,
    /// Where the track was at the last checkpoint.
    pub(crate) checkpoint_position_ms: i64,
    /// The monotonic clock then. `None` only for a listen staged before
    /// the clock was kept.
    pub(crate) checkpoint_mono_ms: Option<i64>,
}

/// What became of a command that replaces the queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Replaced {
    /// The queue is the new one, with the answer the command stored.
    Done(Vec<Entry>, String),
    /// The command was carried out before: its stored answer, and nothing
    /// was changed.
    Replayed(String),
}

/// One delivery waiting in the outbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Delivery {
    pub(crate) row: i64,
    pub(crate) listen_id: Id,
    pub(crate) attempts: i64,
}

fn id(row: &Row<'_>, column: usize) -> rusqlite::Result<Id> {
    Ok(Id::from_bytes(row.get(column)?))
}

fn optional_id(row: &Row<'_>, column: usize) -> rusqlite::Result<Option<Id>> {
    Ok(row.get::<_, Option<[u8; 16]>>(column)?.map(Id::from_bytes))
}

fn parsed<T>(text: String, parse: impl FnOnce(&str) -> Option<T>) -> rusqlite::Result<T> {
    parse(&text).ok_or(rusqlite::Error::InvalidQuery)
}

const ENTRY_COLUMNS: &str =
    "id, public_id, appearance_pid, context_kind, context_ref, added_by_session_id, failed_reason";

fn entry(row: &Row<'_>) -> rusqlite::Result<Entry> {
    Ok(Entry {
        row: row.get(0)?,
        entry_id: id(row, 1)?,
        appearance: id(row, 2)?,
        context_kind: parsed(row.get(3)?, ContextKind::parse)?,
        context_ref: optional_id(row, 4)?,
        added_by: row.get(5)?,
        failed_reason: row.get(6)?,
    })
}

fn session(connection: &Connection) -> Result<Option<Session>, Error> {
    Ok(connection
        .query_row(
            "SELECT queue_revision, state_revision, current_entry_id, position_ms,
                    resume_listen_pid, listener_pid
             FROM playback_session WHERE id = 1",
            [],
            |row| {
                Ok(Session {
                    queue_revision: row.get(0)?,
                    state_revision: row.get(1)?,
                    current: row.get(2)?,
                    position_ms: row.get(3)?,
                    resume_listen: optional_id(row, 4)?,
                    listener: optional_id(row, 5)?,
                })
            },
        )
        .optional()?)
}

const STAGED_COLUMNS: &str = "listen_id, listener_pid, actor_session_id, played_on_server_pid,
    appearance_pid, asset_pid, source_item_pid, source_revision_no, started_at_utc_ms, zone_name,
    started_mono_ms, boot_id, start_reason, continues_listen_id, start_position_ms,
    track_duration_ms, ms_played, heard_ranges_json, max_position_ms, seek_count, pause_count,
    context_kind, context_ref, shuffle, end_position_ms, end_reason, ended_at_utc_ms,
    checkpoint_at_ms, queue_entry_pid, ended_mono_ms, checkpoint_position_ms, checkpoint_mono_ms,
    heard_ranges_approximate";

fn staged(row: &Row<'_>) -> rusqlite::Result<Staged> {
    let end_reason: Option<String> = row.get(25)?;
    let ended = match end_reason {
        Some(reason) => Some(Ended {
            end_position_ms: row.get(24)?,
            end_reason: parsed(reason, EndReason::parse)?,
            ended_at_ms: row.get(26)?,
            ended_mono_ms: row.get(29)?,
        }),
        None => None,
    };
    Ok(Staged {
        started: Started {
            listen_id: id(row, 0)?,
            entry: optional_id(row, 28)?,
            listener: id(row, 1)?,
            actor_session: row.get(2)?,
            played_on_server: id(row, 3)?,
            appearance: id(row, 4)?,
            asset: id(row, 5)?,
            source_item: id(row, 6)?,
            source_revision: row.get(7)?,
            started_at_ms: row.get(8)?,
            zone: row.get(9)?,
            started_mono_ms: row.get(10)?,
            boot: id(row, 11)?,
            start_reason: parsed(row.get(12)?, StartReason::parse)?,
            continues: optional_id(row, 13)?,
            start_position_ms: row.get(14)?,
            track_duration_ms: row.get(15)?,
            context_kind: parsed(row.get(21)?, ContextKind::parse)?,
            context_ref: optional_id(row, 22)?,
            shuffle: row.get(23)?,
        },
        heard: Heard {
            ms_played: row.get(16)?,
            ranges_json: row.get(17)?,
            ranges_approximate: row.get(32)?,
            max_position_ms: row.get(18)?,
            seek_count: row.get(19)?,
            pause_count: row.get(20)?,
        },
        ended,
        checkpoint_at_ms: row.get(27)?,
        checkpoint_position_ms: row.get(30)?,
        checkpoint_mono_ms: row.get(31)?,
    })
}

/// Saves where the player is: the entry and the position, and the listen
/// a resume would continue.
fn save_place(
    transaction: &Transaction<'_>,
    current: Option<i64>,
    position_ms: i64,
    resume_listen: Option<Id>,
    now_ms: i64,
) -> Result<(), Error> {
    transaction.execute(
        "UPDATE playback_session
         SET current_entry_id = ?1, position_ms = ?2, resume_listen_pid = ?3,
             state_revision = state_revision + 1, updated_at_ms = ?4
         WHERE id = 1",
        (
            current,
            position_ms,
            resume_listen.as_ref().map(Id::as_bytes),
            now_ms,
        ),
    )?;
    Ok(())
}

/// Closes a listen, and queues its delivery, in the transaction given.
fn close(
    transaction: &Transaction<'_>,
    listen_id: Id,
    heard: &Heard,
    ended: &Ended,
    now_ms: i64,
) -> Result<(), Error> {
    let closed = transaction.execute(
        "UPDATE listen_staging
         SET ms_played = ?2, heard_ranges_json = ?3, max_position_ms = ?4, seek_count = ?5,
             pause_count = ?6, end_position_ms = ?7, end_reason = ?8, ended_at_utc_ms = ?9,
             ended_mono_ms = ?10, checkpoint_at_ms = ?11, checkpoint_position_ms = ?7,
             checkpoint_mono_ms = coalesce(?10, checkpoint_mono_ms),
             heard_ranges_approximate = max(heard_ranges_approximate, ?12)
         WHERE listen_id = ?1 AND ended_at_utc_ms IS NULL",
        rusqlite::params![
            listen_id.as_bytes(),
            heard.ms_played,
            &heard.ranges_json,
            heard.max_position_ms,
            heard.seek_count,
            heard.pause_count,
            ended.end_position_ms,
            ended.end_reason.as_str(),
            ended.ended_at_ms,
            ended.ended_mono_ms,
            now_ms,
            heard.ranges_approximate,
        ],
    )?;
    if closed == 0 {
        return Err(Error::Corrupt("a listen to close that is not open"));
    }
    transaction.execute(
        "INSERT INTO outbox (kind, dedupe_key, next_attempt_at_ms, created_at_ms)
         VALUES (?1, ?2, ?3, ?3)
         ON CONFLICT (kind, dedupe_key) DO NOTHING",
        (DELIVER_LISTEN, listen_id.as_bytes(), now_ms),
    )?;
    Ok(())
}

impl ServerStore {
    /// The player's session, made if this database has none yet.
    pub(crate) fn playback_session(&self) -> Result<Session, Error> {
        if let Some(found) = self.read(session)? {
            return Ok(found);
        }
        self.transact(|transaction, now_ms, _| {
            transaction.execute(
                "INSERT INTO playback_session (id, updated_at_ms) VALUES (1, ?1)
                 ON CONFLICT (id) DO NOTHING",
                [now_ms],
            )?;
            session(transaction)?.ok_or(Error::Corrupt("a playback session"))
        })
    }

    /// Every entry of the queue, in queue order.
    pub(crate) fn queue(&self) -> Result<Vec<Entry>, Error> {
        self.read(|connection| {
            let mut statement = connection.prepare(&format!(
                "SELECT {ENTRY_COLUMNS} FROM queue_entry ORDER BY position, id"
            ))?;
            let rows = statement.query_map([], entry)?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
    }

    /// The stored result of a server-local command that was carried out
    /// before. The same ID with another request is refused.
    pub(crate) fn stored_result(&self, command: Command) -> Result<Option<String>, Error> {
        self.read(|connection| receipt(connection, &command))
    }

    /// Records the receipt of a command that changed nothing in this file
    /// but the session's revision, with the session's place.
    pub(crate) fn record_command(
        &self,
        command: Command,
        method: &'static str,
        result: String,
        place: Option<(Option<i64>, i64, Option<Id>)>,
    ) -> Result<(), Error> {
        self.transact(move |transaction, now_ms, _| {
            if receipt(transaction, &command)?.is_some() {
                return Ok(());
            }
            if let Some((current, position_ms, resume)) = place {
                save_place(transaction, current, position_ms, resume, now_ms)?;
            }
            record_receipt(transaction, &command, method, &result, now_ms)
        })
    }

    /// Replaces the queue with `entries` and puts the player on the one at
    /// `start`, at its beginning, with the command's receipt: one
    /// transaction. `result` is given the new queue and returns what the
    /// command answers.
    pub(crate) fn replace_queue(
        &self,
        command: Command,
        method: &'static str,
        entries: Vec<NewEntry>,
        start: usize,
        result: impl FnOnce(&[Entry], i64) -> String + Send + 'static,
    ) -> Result<Replaced, Error> {
        self.transact(move |transaction, now_ms, entropy| {
            if let Some(stored) = receipt(transaction, &command)? {
                return Ok(Replaced::Replayed(stored));
            }
            transaction.execute(
                "UPDATE playback_session SET current_entry_id = NULL WHERE id = 1",
                [],
            )?;
            transaction.execute("DELETE FROM queue_entry", [])?;
            let mut insert = transaction.prepare(
                "INSERT INTO queue_entry
                   (public_id, appearance_pid, position, context_kind, context_ref,
                    added_by_session_id, added_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, (SELECT id FROM session WHERE public_id = ?6), ?7)",
            )?;
            for (position, new) in entries.iter().enumerate() {
                insert.execute((
                    crate::new_id(now_ms, entropy)?.as_bytes(),
                    new.appearance.as_bytes(),
                    i64::try_from(position).unwrap_or(i64::MAX),
                    new.context_kind.as_str(),
                    new.context_ref.as_ref().map(Id::as_bytes),
                    new.added_by.as_ref().map(Id::as_bytes),
                    now_ms,
                ))?;
            }
            drop(insert);
            let mut statement = transaction.prepare(&format!(
                "SELECT {ENTRY_COLUMNS} FROM queue_entry ORDER BY position, id"
            ))?;
            let queue: Vec<Entry> = statement.query_map([], entry)?.collect::<Result<_, _>>()?;
            drop(statement);
            let current = queue.get(start).map(|entry| entry.row);
            transaction.execute(
                "UPDATE playback_session
                 SET queue_revision = queue_revision + 1, current_entry_id = ?1, position_ms = 0,
                     resume_listen_pid = NULL, state_revision = state_revision + 1,
                     updated_at_ms = ?2
                 WHERE id = 1",
                (current, now_ms),
            )?;
            let revision: i64 = transaction.query_row(
                "SELECT queue_revision FROM playback_session WHERE id = 1",
                [],
                |row| row.get(0),
            )?;
            let answer = result(&queue, revision);
            record_receipt(transaction, &command, method, &answer, now_ms)?;
            Ok(Replaced::Done(queue, answer))
        })
    }

    /// Saves the player's place: the entry, the position, and the listen a
    /// resume would continue.
    pub(crate) fn save_place(
        &self,
        current: Option<i64>,
        position_ms: i64,
        resume_listen: Option<Id>,
    ) -> Result<(), Error> {
        self.transact(move |transaction, now_ms, _| {
            save_place(transaction, current, position_ms, resume_listen, now_ms)
        })
    }

    /// Sets the listener on the player.
    pub(crate) fn set_listener(&self, listener: Id) -> Result<(), Error> {
        self.transact(move |transaction, _, _| {
            transaction.execute(
                "UPDATE playback_session SET listener_pid = ?1 WHERE id = 1",
                [listener.as_bytes()],
            )?;
            Ok(())
        })
    }

    /// Records why an entry could not be played.
    pub(crate) fn mark_failed(&self, entry: i64, reason: String) -> Result<(), Error> {
        self.transact(move |transaction, _, _| {
            transaction.execute(
                "UPDATE queue_entry SET failed_reason = ?2 WHERE id = ?1",
                (entry, &reason),
            )?;
            Ok(())
        })
    }

    /// Writes a listen when its first frame is heard, with the player's
    /// place. `mark` is where the track is now.
    pub(crate) fn open_listen(
        &self,
        started: Started,
        current: Option<i64>,
        mark: Mark,
    ) -> Result<(), Error> {
        self.transact(move |transaction, now_ms, _| {
            transaction.execute(
                "INSERT INTO listen_staging
                   (listen_id, listener_pid, actor_session_id, played_on_server_pid,
                    appearance_pid, asset_pid, copy_kind, source_item_pid, source_revision_no,
                    started_at_utc_ms, zone_name, started_mono_ms, boot_id, start_reason,
                    continues_listen_id, start_position_ms, track_duration_ms, max_position_ms,
                    context_kind, context_ref, shuffle, checkpoint_at_ms, queue_entry_pid,
                    checkpoint_position_ms, checkpoint_mono_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'master', ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                         ?15, ?16, ?15, ?17, ?18, ?19, ?20, ?21, ?22, ?23)",
                rusqlite::params![
                    started.listen_id.as_bytes(),
                    started.listener.as_bytes(),
                    started.actor_session,
                    started.played_on_server.as_bytes(),
                    started.appearance.as_bytes(),
                    started.asset.as_bytes(),
                    started.source_item.as_bytes(),
                    started.source_revision,
                    started.started_at_ms,
                    started.zone,
                    started.started_mono_ms,
                    started.boot.as_bytes(),
                    started.start_reason.as_str(),
                    started.continues.as_ref().map(Id::as_bytes),
                    started.start_position_ms,
                    started.track_duration_ms,
                    started.context_kind.as_str(),
                    started.context_ref.as_ref().map(Id::as_bytes),
                    started.shuffle,
                    now_ms,
                    started.entry.as_ref().map(Id::as_bytes),
                    mark.position_ms,
                    mark.mono_ms,
                ],
            )?;
            transaction.execute(
                "UPDATE playback_session SET current_entry_id = ?1, position_ms = ?2 WHERE id = 1",
                (current, mark.position_ms),
            )?;
            Ok(())
        })
    }

    /// Saves what an open listen has heard so far and where its track is,
    /// and with it the player's position: a checkpoint.
    pub(crate) fn checkpoint_listen(
        &self,
        listen_id: Id,
        heard: Heard,
        mark: Mark,
    ) -> Result<(), Error> {
        self.transact(move |transaction, now_ms, _| {
            transaction.execute(
                "UPDATE listen_staging
                 SET ms_played = ?2, heard_ranges_json = ?3, max_position_ms = ?4,
                     seek_count = ?5, pause_count = ?6, checkpoint_at_ms = ?7,
                     checkpoint_position_ms = ?8, checkpoint_mono_ms = ?9,
                     heard_ranges_approximate = max(heard_ranges_approximate, ?10)
                 WHERE listen_id = ?1 AND ended_at_utc_ms IS NULL",
                rusqlite::params![
                    listen_id.as_bytes(),
                    heard.ms_played,
                    &heard.ranges_json,
                    heard.max_position_ms,
                    heard.seek_count,
                    heard.pause_count,
                    now_ms,
                    mark.position_ms,
                    mark.mono_ms,
                    heard.ranges_approximate,
                ],
            )?;
            transaction.execute(
                "UPDATE playback_session SET position_ms = ?1 WHERE id = 1",
                [mark.position_ms],
            )?;
            Ok(())
        })
    }

    /// Closes a listen and queues its delivery, and saves the player's
    /// place: one transaction.
    pub(crate) fn close_listen(
        &self,
        listen_id: Id,
        heard: Heard,
        ended: Ended,
        place: (Option<i64>, i64, Option<Id>),
    ) -> Result<(), Error> {
        self.transact(move |transaction, now_ms, _| {
            close(transaction, listen_id, &heard, &ended, now_ms)?;
            let (current, position_ms, resume) = place;
            save_place(transaction, current, position_ms, resume, now_ms)
        })
    }

    /// Closes every listen a run that is gone left open, as a crash, where
    /// and when its own last checkpoint was and with what it had heard,
    /// and queues each for delivery. The one cut off in the entry the
    /// player is on becomes the listen a resume continues. Returns the
    /// listens closed. This server holds the data directory's lock, so the
    /// run that left them is gone.
    pub fn close_crashed_listens(&self) -> Result<Vec<Id>, Error> {
        self.transact(|transaction, now_ms, _| {
            let current: Option<[u8; 16]> = transaction
                .query_row(
                    "SELECT q.public_id FROM playback_session s
                     JOIN queue_entry q ON q.id = s.current_entry_id WHERE s.id = 1",
                    [],
                    |row| row.get(0),
                )
                .optional()?;
            let current = current.map(Id::from_bytes);
            let mut statement = transaction.prepare(&format!(
                "SELECT {STAGED_COLUMNS} FROM listen_staging
                 WHERE ended_at_utc_ms IS NULL ORDER BY id"
            ))?;
            let open: Vec<Staged> = statement.query_map([], staged)?.collect::<Result<_, _>>()?;
            drop(statement);
            let mut closed = Vec::with_capacity(open.len());
            let mut resume = None;
            for listen in &open {
                let ended = Ended {
                    end_position_ms: listen.checkpoint_position_ms,
                    end_reason: EndReason::Crash,
                    ended_at_ms: listen.checkpoint_at_ms,
                    ended_mono_ms: listen.checkpoint_mono_ms,
                };
                close(
                    transaction,
                    listen.started.listen_id,
                    &listen.heard,
                    &ended,
                    now_ms,
                )?;
                closed.push(listen.started.listen_id);
                if listen.started.entry.is_some() && listen.started.entry == current {
                    resume = Some(listen.started.listen_id);
                }
            }
            if let Some(resume) = resume {
                transaction.execute(
                    "UPDATE playback_session SET resume_listen_pid = ?1, updated_at_ms = ?2
                     WHERE id = 1",
                    (resume.as_bytes(), now_ms),
                )?;
            }
            Ok(closed)
        })
    }

    /// The staged listen with this ID.
    pub(crate) fn staged_listen(&self, listen_id: Id) -> Result<Option<Staged>, Error> {
        self.read(|connection| {
            Ok(connection
                .query_row(
                    &format!("SELECT {STAGED_COLUMNS} FROM listen_staging WHERE listen_id = ?1"),
                    [listen_id.as_bytes()],
                    staged,
                )
                .optional()?)
        })
    }

    /// The deliveries due by `now_ms`, oldest first, and when the next one
    /// that is not due yet will be.
    pub(crate) fn due_deliveries(
        &self,
        now_ms: i64,
        limit: u32,
    ) -> Result<(Vec<Delivery>, Option<i64>), Error> {
        self.read(|connection| {
            let mut statement = connection.prepare(
                "SELECT id, dedupe_key, attempts FROM outbox
                 WHERE kind = ?1 AND next_attempt_at_ms <= ?2 ORDER BY id LIMIT ?3",
            )?;
            let due = statement
                .query_map((DELIVER_LISTEN, now_ms, limit), |row| {
                    Ok(Delivery {
                        row: row.get(0)?,
                        listen_id: id(row, 1)?,
                        attempts: row.get(2)?,
                    })
                })?
                .collect::<Result<_, _>>()?;
            let next: Option<i64> = connection.query_row(
                "SELECT min(next_attempt_at_ms) FROM outbox WHERE kind = ?1 AND next_attempt_at_ms > ?2",
                (DELIVER_LISTEN, now_ms),
                |row| row.get(0),
            )?;
            Ok((due, next))
        })
    }

    /// How many listens are waiting to be delivered.
    pub fn undelivered_listens(&self) -> Result<i64, Error> {
        self.read(|connection| {
            Ok(connection.query_row(
                "SELECT count(*) FROM outbox WHERE kind = ?1",
                [DELIVER_LISTEN],
                |row| row.get(0),
            )?)
        })
    }

    /// The hub has the listen: its staging row and its outbox row go, in
    /// one transaction.
    pub(crate) fn delivered(&self, listen_id: Id) -> Result<(), Error> {
        self.transact(move |transaction, _, _| {
            transaction.execute(
                "DELETE FROM outbox WHERE kind = ?1 AND dedupe_key = ?2",
                (DELIVER_LISTEN, listen_id.as_bytes()),
            )?;
            transaction.execute(
                "DELETE FROM listen_staging WHERE listen_id = ?1 AND ended_at_utc_ms IS NOT NULL",
                [listen_id.as_bytes()],
            )?;
            Ok(())
        })
    }

    /// A delivery failed: it is tried again at `next_at_ms`.
    pub(crate) fn delivery_failed(
        &self,
        row: i64,
        error: String,
        next_at_ms: i64,
    ) -> Result<(), Error> {
        self.transact(move |transaction, _, _| {
            transaction.execute(
                "UPDATE outbox SET attempts = attempts + 1, last_error = ?2, next_attempt_at_ms = ?3
                 WHERE id = ?1",
                (row, &error, next_at_ms),
            )?;
            Ok(())
        })
    }

    /// The public ID of a session, by its row.
    pub(crate) fn session_public_id(&self, row: i64) -> Result<Option<Id>, Error> {
        self.read(|connection| {
            Ok(connection
                .query_row(
                    "SELECT public_id FROM session WHERE id = ?1",
                    [row],
                    |row| row.get::<_, [u8; 16]>(0),
                )
                .optional()?
                .map(Id::from_bytes))
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rusqlite::Connection;

    use super::*;
    use crate::migrate;
    use crate::testing::{FixedClock, Scratch};
    use crate::{OsEntropy, SessionToken, SystemClock};

    /// A `server.db` as the second migration left it: an identity, the
    /// local credential's session, a root, a finished scan and its receipt.
    const SAVED_0002: &str = include_str!("../../saved/server/0002.sql");

    /// A `server.db` as the third migration left it: an album queued, a
    /// listen closed and waiting for the hub, and one left open by a crash.
    const SAVED_0003: &str = include_str!("../../saved/server/0003.sql");

    const T0: i64 = 1_763_000_000_000;

    fn schema(connection: &Connection) -> Vec<String> {
        connection
            .prepare("SELECT sql FROM sqlite_schema WHERE sql IS NOT NULL ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    #[test]
    fn the_saved_database_is_one_the_first_two_migrations_made() {
        let mut saved = Connection::open_in_memory().unwrap();
        saved.execute_batch(SAVED_0002).unwrap();
        let mut made = Connection::open_in_memory().unwrap();
        migrate::run(
            &mut made,
            migrate::SERVER.get(..2).unwrap(),
            &FixedClock::at(T0),
        )
        .unwrap();
        assert_eq!(schema(&saved), schema(&made));
        let again = migrate::run(
            &mut saved,
            migrate::SERVER.get(..2).unwrap(),
            &FixedClock::at(T0),
        )
        .unwrap();
        assert_eq!((again.applied, again.version), (0, 2));
    }

    #[test]
    fn a_database_saved_at_the_second_migration_is_taken_forward_with_what_it_holds() {
        let scratch = Scratch::new();
        let path = scratch.path().join("server.db");
        Connection::open(&path)
            .unwrap()
            .execute_batch(SAVED_0002)
            .unwrap();

        let store = ServerStore::open(&path, Arc::new(SystemClock), Arc::new(OsEntropy)).unwrap();
        assert_eq!(store.migrations_applied(), 3);
        assert_eq!(store.schema_version(), 5);
        // Who the server is, its credential, its root and its scan are as
        // they were.
        let identity = store.identity().unwrap().unwrap();
        assert_eq!(
            identity.server_id.to_string(),
            "019a7afd-1e00-7021-a223-242526272829"
        );
        let token =
            SessionToken::new("2a2b2c2d2e2f303132333435363738393a3b3c3d3e3f40414243444546474849");
        assert!(store.is_local_credential(&token).unwrap());
        assert_eq!(
            store.roots().unwrap(),
            [("music".to_owned(), "/srv/invented/music".to_owned())]
        );
        let [scan] = store.jobs(10).unwrap().try_into().unwrap();
        assert_eq!((scan.kind.as_str(), scan.done), ("scan", 2));
        assert_eq!(store.failed_job_items(scan.id, 10).unwrap().len(), 1);

        // The player's tables are there to be used, and name the session
        // that was there before them.
        let session = store.local_session().unwrap();
        let fresh = store.playback_session().unwrap();
        assert_eq!((fresh.queue_revision, fresh.current), (0, None));
        let replaced = store
            .replace_queue(
                Command {
                    id: Id::from_bytes([0xc3; 16]),
                    fingerprint: crate::Fingerprint::of("playback.play", b"{}"),
                },
                "playback.play",
                vec![NewEntry {
                    appearance: Id::from_bytes([0xa0; 16]),
                    context_kind: ContextKind::Manual,
                    context_ref: None,
                    added_by: session,
                }],
                0,
                |_, revision| revision.to_string(),
            )
            .unwrap();
        let Replaced::Done(queue, answer) = replaced else {
            panic!("a new command is carried out");
        };
        assert_eq!((queue.len(), answer.as_str()), (1, "1"));
        assert_eq!(queue[0].added_by, Some(1));
        store.close().unwrap();

        let store = ServerStore::open(&path, Arc::new(SystemClock), Arc::new(OsEntropy)).unwrap();
        assert_eq!(store.migrations_applied(), 0);
        assert_eq!(store.queue().unwrap().len(), 1);
        assert_eq!(
            store.playback_session().unwrap().current,
            Some(queue[0].row)
        );
    }

    #[test]
    fn the_saved_database_is_one_the_first_three_migrations_made() {
        let saved = Connection::open_in_memory().unwrap();
        saved.execute_batch(SAVED_0003).unwrap();
        let mut made = Connection::open_in_memory().unwrap();
        migrate::run(
            &mut made,
            migrate::SERVER.get(..3).unwrap(),
            &FixedClock::at(T0),
        )
        .unwrap();
        assert_eq!(schema(&saved), schema(&made));
    }

    /// A `server.db` as the fourth migration left it: [`SAVED_0003`] taken
    /// forward one migration.
    const SAVED_0004: &str = include_str!("../../saved/server/0004.sql");

    #[test]
    fn a_database_saved_at_the_fourth_migration_is_taken_forward_with_its_listens() {
        let made = |count: usize| {
            let mut made = Connection::open_in_memory().unwrap();
            migrate::run(
                &mut made,
                migrate::SERVER.get(..count).unwrap(),
                &FixedClock::at(T0),
            )
            .unwrap();
            made
        };
        let saved = Connection::open_in_memory().unwrap();
        saved.execute_batch(SAVED_0004).unwrap();
        assert_eq!(schema(&saved), schema(&made(4)));

        let scratch = Scratch::new();
        let path = scratch.path().join("server.db");
        Connection::open(&path)
            .unwrap()
            .execute_batch(SAVED_0004)
            .unwrap();
        let clock = Arc::new(FixedClock::at(1_794_776_600_000));
        let store = ServerStore::open(&path, clock, Arc::new(OsEntropy)).unwrap();
        assert_eq!((store.migrations_applied(), store.schema_version()), (1, 5));
        assert_eq!(store.queue().unwrap().len(), 4);
        assert_eq!(store.undelivered_listens().unwrap(), 1);
    }

    #[test]
    fn a_database_saved_with_listens_staged_is_taken_forward_with_every_one() {
        let scratch = Scratch::new();
        let path = scratch.path().join("server.db");
        Connection::open(&path)
            .unwrap()
            .execute_batch(SAVED_0003)
            .unwrap();

        let clock = Arc::new(FixedClock::at(1_794_776_600_000));
        let store = ServerStore::open(&path, clock, Arc::new(OsEntropy)).unwrap();
        assert_eq!((store.migrations_applied(), store.schema_version()), (2, 5));
        let queue = store.queue().unwrap();
        let session = store.playback_session().unwrap();
        assert_eq!(queue.len(), 4);
        assert_eq!(
            (session.current, session.position_ms),
            (Some(queue[1].row), 10_000)
        );
        assert_eq!(store.undelivered_listens().unwrap(), 1);

        let staged: Vec<Staged> = store
            .read(|connection| {
                let mut statement = connection.prepare(&format!(
                    "SELECT {STAGED_COLUMNS} FROM listen_staging ORDER BY id"
                ))?;
                let rows = statement.query_map([], staged)?;
                Ok(rows.collect::<Result<_, _>>()?)
            })
            .unwrap();
        let [skipped, open] = staged.try_into().unwrap();
        // The first track's listen, skipped at 1:40: closed, as it was.
        let ended = skipped.ended.clone().unwrap();
        assert_eq!(
            (ended.end_reason, ended.end_position_ms, ended.ended_mono_ms),
            (EndReason::Next, 100_000, None)
        );
        assert_eq!(
            (skipped.heard.ms_played, skipped.checkpoint_position_ms),
            (100_000, 100_000)
        );
        assert_eq!(skipped.heard.ranges_json, "[[0,100000]]");
        // The second's, open at its 10-second checkpoint: its entry and
        // position are the session's, and its clock then was not kept.
        assert_eq!(open.ended, None);
        assert_eq!(open.started.entry, Some(queue[1].entry_id));
        assert_eq!(
            (
                open.heard.ms_played,
                open.checkpoint_position_ms,
                open.checkpoint_mono_ms
            ),
            (10_000, 10_000, None)
        );

        store
            .read(|connection| {
                let dangling: i64 = connection.query_row(
                    "SELECT count(*) FROM pragma_foreign_key_check",
                    [],
                    |row| row.get(0),
                )?;
                let whole: String =
                    connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
                assert_eq!((dangling, whole.as_str()), (0, "ok"));
                let mut fresh = Connection::open_in_memory()?;
                migrate::run(&mut fresh, migrate::SERVER, &FixedClock::at(T0))?;
                assert_eq!(schema(connection), schema(&fresh));
                Ok(())
            })
            .unwrap();

        // One listen is open at most: a second is refused while it is.
        let second = Started {
            listen_id: Id::from_bytes([0xd2; 16]),
            ..open.started.clone()
        };
        let mark = Mark {
            position_ms: 0,
            mono_ms: 5_000_000,
        };
        assert!(store.open_listen(second.clone(), None, mark).is_err());

        // The next run closes the open one as a crash, and then the second
        // can open, queued from an artist's page.
        assert_eq!(
            store.close_crashed_listens().unwrap(),
            [open.started.listen_id]
        );
        let closed = store
            .staged_listen(open.started.listen_id)
            .unwrap()
            .unwrap();
        let ended = closed.ended.unwrap();
        assert_eq!(
            (ended.end_reason, ended.end_position_ms, ended.ended_mono_ms),
            (EndReason::Crash, 10_000, None)
        );
        let artist = Started {
            context_kind: ContextKind::Artist,
            ..second
        };
        store.open_listen(artist, None, mark).unwrap();
        assert_eq!(store.undelivered_listens().unwrap(), 2);
    }

    #[test]
    fn a_listen_left_open_is_what_a_resume_continues_only_if_its_entry_is_the_sessions() {
        let store = ServerStore::open_in_memory(
            Arc::new(FixedClock::at(T0)),
            Arc::new(crate::testing::CountingEntropy::default()),
        )
        .unwrap();
        store.playback_session().unwrap();
        let entries = |kind| NewEntry {
            appearance: Id::from_bytes([0xa0; 16]),
            context_kind: kind,
            context_ref: None,
            added_by: None,
        };
        let Replaced::Done(queue, _) = store
            .replace_queue(
                Command {
                    id: Id::from_bytes([0xc4; 16]),
                    fingerprint: crate::Fingerprint::of("playback.play", b"{}"),
                },
                "playback.play",
                vec![entries(ContextKind::Album), entries(ContextKind::Album)],
                0,
                |_, revision| revision.to_string(),
            )
            .unwrap()
        else {
            panic!("a new command is carried out");
        };
        let started = Started {
            listen_id: Id::from_bytes([0xd1; 16]),
            entry: Some(queue[0].entry_id),
            listener: Id::from_bytes([0x11; 16]),
            actor_session: None,
            played_on_server: Id::from_bytes([0x5e; 16]),
            appearance: Id::from_bytes([0xa0; 16]),
            asset: Id::from_bytes([0xa5; 16]),
            source_item: Id::from_bytes([0x51; 16]),
            source_revision: 1,
            started_at_ms: T0,
            zone: None,
            started_mono_ms: 1_000,
            boot: Id::from_bytes([0xba; 16]),
            start_reason: StartReason::UserPick,
            continues: None,
            start_position_ms: 0,
            track_duration_ms: 200_000,
            context_kind: ContextKind::Album,
            context_ref: None,
            shuffle: false,
        };
        let mark = |position_ms| Mark {
            position_ms,
            mono_ms: 1_000 + position_ms,
        };
        store
            .open_listen(started.clone(), Some(queue[0].row), mark(0))
            .unwrap();
        store
            .checkpoint_listen(
                started.listen_id,
                Heard {
                    ms_played: 5_000,
                    ranges_json: "[[0,5000]]".to_owned(),
                    max_position_ms: 5_000,
                    ..Heard::default()
                },
                mark(5_000),
            )
            .unwrap();
        // The session moved on to the second entry, and saved a place there,
        // before the run died with the first entry's listen open.
        store.save_place(Some(queue[1].row), 42_000, None).unwrap();
        assert_eq!(store.close_crashed_listens().unwrap(), [started.listen_id]);
        let closed = store.staged_listen(started.listen_id).unwrap().unwrap();
        assert_eq!(closed.ended.unwrap().end_position_ms, 5_000);
        assert_eq!(store.playback_session().unwrap().resume_listen, None);
    }
}
