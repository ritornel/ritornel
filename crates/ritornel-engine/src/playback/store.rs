//! The player's rows in `server.db`: the queue, the one session, each
//! listen while it is open and until the hub has it, and the outbox row
//! that carries it there. See "Queue and session" and "Recording a listen"
//! in docs/internals/playback.md, and "Jobs and outbox" in
//! docs/internals/server.md.

use ritornel_domain::history::{ContextKind, EndReason, StartReason};
use rusqlite::{Connection, OptionalExtension, Row, Transaction};

use crate::jobs::{receipt, record_receipt};
use crate::playback::queue::{QueueWrite, Repeat};
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
    /// Its place in queue order.
    pub(crate) position: i64,
    /// Its place in the stored play order, while shuffle is on.
    pub(crate) shuffle_position: Option<i64>,
    /// Why it was queued.
    pub(crate) context_kind: ContextKind,
    /// What it was queued from.
    pub(crate) context_ref: Option<Id>,
    /// The session that queued it, by its row in `session`.
    pub(crate) added_by: Option<i64>,
    /// Why it could not be played, once it could not.
    pub(crate) failed_reason: Option<String>,
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
    /// Whether the queue plays in its stored shuffled order.
    pub(crate) shuffle: bool,
    /// Repeat.
    pub(crate) repeat: Repeat,
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

/// A listen as a save writes it: open, or closed and on its way to the
/// hub.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Saved {
    pub(crate) started: Started,
    pub(crate) heard: Heard,
    /// Where its track is now: where it ended, if a crash closes it.
    pub(crate) position_ms: i64,
    pub(crate) ended: Option<Ended>,
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

/// What became of a command that changes the queue or the player's
/// settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Wrote {
    /// It was written, with the answer stored. The queue's change is
    /// handed back, for memory to apply.
    Done {
        /// The queue's revision after it.
        revision: i64,
        /// What the command answers.
        answer: String,
        /// What changed in the queue.
        queue: QueueWrite,
    },
    /// The command was carried out before: its stored answer, and nothing
    /// was changed.
    Replayed(String),
}

/// A change to the queue and the player's settings, with the command it
/// is and where the player is after it: what [`ServerStore::write_queue`]
/// writes in one transaction.
#[derive(Debug)]
pub(crate) struct Change {
    /// The command and its method, or `None` for the player's own change,
    /// such as a new order when the queue starts again.
    pub(crate) command: Option<(Command, &'static str)>,
    /// The queue revision the edit was decided on, if it names one.
    pub(crate) expected: Option<i64>,
    /// What changes in the queue.
    pub(crate) queue: QueueWrite,
    /// Whether the queue's revision moves: its entries or their order
    /// changed.
    pub(crate) bump: bool,
    /// Repeat, where it changes.
    pub(crate) repeat: Option<Repeat>,
    /// The volume and whether it is muted, where they change.
    pub(crate) volume: Option<(f64, bool)>,
    /// The entry, by row, and the position the player is at after it, and
    /// the listen a resume would continue.
    pub(crate) place: (Option<i64>, i64, Option<Id>),
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

const ENTRY_COLUMNS: &str = "id, public_id, appearance_pid, context_kind, context_ref,
    added_by_session_id, failed_reason, position, shuffle_position";

fn entry(row: &Row<'_>) -> rusqlite::Result<Entry> {
    Ok(Entry {
        row: row.get(0)?,
        entry_id: id(row, 1)?,
        appearance: id(row, 2)?,
        position: row.get(7)?,
        shuffle_position: row.get(8)?,
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
                    resume_listen_pid, listener_pid, shuffle, repeat
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
                    shuffle: row.get(6)?,
                    repeat: parsed(row.get(7)?, Repeat::parse)?,
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

/// How many entries one statement inserts: a long queue goes in far faster
/// this way than a row at a time.
const INSERT_ROWS: usize = 100;

/// Inserts entries, with their rows and positions.
fn insert_entries(
    transaction: &Transaction<'_>,
    entries: &[Entry],
    now_ms: i64,
) -> Result<(), Error> {
    for batch in entries.chunks(INSERT_ROWS) {
        let rows = vec!["(?, ?, ?, ?, ?, ?, ?, ?, ?)"; batch.len()].join(", ");
        let mut insert = transaction.prepare_cached(&format!(
            "INSERT INTO queue_entry
               (id, public_id, appearance_pid, position, shuffle_position, context_kind,
                context_ref, added_by_session_id, added_at_ms)
             VALUES {rows}"
        ))?;
        let mut values: Vec<rusqlite::types::Value> = Vec::with_capacity(batch.len() * 9);
        for entry in batch {
            values.extend([
                entry.row.into(),
                entry.entry_id.as_bytes().to_vec().into(),
                entry.appearance.as_bytes().to_vec().into(),
                entry.position.into(),
                entry.shuffle_position.into(),
                entry.context_kind.as_str().to_owned().into(),
                entry.context_ref.map(|id| id.as_bytes().to_vec()).into(),
                entry.added_by.into(),
                now_ms.into(),
            ]);
        }
        insert.execute(rusqlite::params_from_iter(values))?;
    }
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
    deliver(transaction, listen_id, now_ms)
}

/// Queues a closed listen's delivery to the hub.
fn deliver(transaction: &Transaction<'_>, listen_id: Id, now_ms: i64) -> Result<(), Error> {
    transaction.execute(
        "INSERT INTO outbox (kind, dedupe_key, next_attempt_at_ms, created_at_ms)
         VALUES (?1, ?2, ?3, ?3)
         ON CONFLICT (kind, dedupe_key) DO NOTHING",
        (DELIVER_LISTEN, listen_id.as_bytes(), now_ms),
    )?;
    Ok(())
}

/// Writes a listen as it is now, whether its row is there yet or not. A
/// row once closed is never written again.
fn upsert(
    transaction: &Transaction<'_>,
    listen: &Saved,
    mono_ms: i64,
    now_ms: i64,
) -> Result<(), Error> {
    let (started, heard, ended) = (&listen.started, &listen.heard, listen.ended.as_ref());
    let written = transaction
        .prepare_cached(
            "INSERT INTO listen_staging
               (listen_id, listener_pid, actor_session_id, played_on_server_pid, appearance_pid,
                asset_pid, copy_kind, source_item_pid, source_revision_no, started_at_utc_ms,
                zone_name, started_mono_ms, boot_id, start_reason, continues_listen_id,
                start_position_ms, track_duration_ms, context_kind, context_ref, shuffle,
                queue_entry_pid, ms_played, heard_ranges_json, heard_ranges_approximate,
                max_position_ms, seek_count, pause_count, checkpoint_at_ms,
                checkpoint_position_ms, checkpoint_mono_ms, end_position_ms, end_reason,
                ended_at_utc_ms, ended_mono_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'master', ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                     ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30,
                     ?31, ?32, ?33)
             ON CONFLICT (listen_id) DO UPDATE SET
               ms_played = excluded.ms_played, heard_ranges_json = excluded.heard_ranges_json,
               heard_ranges_approximate =
                 max(heard_ranges_approximate, excluded.heard_ranges_approximate),
               max_position_ms = excluded.max_position_ms, seek_count = excluded.seek_count,
               pause_count = excluded.pause_count, checkpoint_at_ms = excluded.checkpoint_at_ms,
               checkpoint_position_ms = excluded.checkpoint_position_ms,
               checkpoint_mono_ms = excluded.checkpoint_mono_ms,
               end_position_ms = excluded.end_position_ms, end_reason = excluded.end_reason,
               ended_at_utc_ms = excluded.ended_at_utc_ms, ended_mono_ms = excluded.ended_mono_ms
             WHERE listen_staging.ended_at_utc_ms IS NULL",
        )?
        .execute(rusqlite::params![
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
            started.entry.as_ref().map(Id::as_bytes),
            heard.ms_played,
            &heard.ranges_json,
            heard.ranges_approximate,
            heard.max_position_ms,
            heard.seek_count,
            heard.pause_count,
            now_ms,
            listen.position_ms,
            ended
                .and_then(|ended| ended.ended_mono_ms)
                .unwrap_or(mono_ms),
            ended.map(|ended| ended.end_position_ms),
            ended.map(|ended| ended.end_reason.as_str()),
            ended.map(|ended| ended.ended_at_ms),
            ended.and_then(|ended| ended.ended_mono_ms),
        ])?;
    match ended {
        Some(_) if written > 0 => deliver(transaction, started.listen_id, now_ms),
        _ => Ok(()),
    }
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
    pub fn stored_result(&self, command: Command) -> Result<Option<String>, Error> {
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

    /// The row of a session, by its public ID.
    pub(crate) fn session_row(&self, session: Id) -> Result<Option<i64>, Error> {
        self.read(|connection| {
            Ok(connection
                .query_row(
                    "SELECT id FROM session WHERE public_id = ?1",
                    [session.as_bytes()],
                    |row| row.get(0),
                )
                .optional()?)
        })
    }

    /// Writes a change to the queue and the player's settings, with the
    /// place the player is at after it and the command's receipt, in one
    /// transaction. An edit decided on a queue revision that is not the
    /// queue's now changes nothing and fails with [`Error::StaleQueue`].
    /// `answer` is given the queue's revision after it and returns what the
    /// command answers.
    pub(crate) fn write_queue(
        &self,
        change: Change,
        answer: impl FnOnce(i64) -> String + Send + 'static,
    ) -> Result<Wrote, Error> {
        self.transact(move |transaction, now_ms, _| {
            if let Some((command, _)) = &change.command
                && let Some(stored) = receipt(transaction, command)?
            {
                return Ok(Wrote::Replayed(stored));
            }
            let current: i64 = transaction.query_row(
                "SELECT queue_revision FROM playback_session WHERE id = 1",
                [],
                |row| row.get(0),
            )?;
            if change.expected.is_some_and(|expected| expected != current) {
                return Err(Error::StaleQueue { current });
            }
            let write = &change.queue;
            if write.clear || !write.remove.is_empty() {
                // The session's entry may be among those that go; its
                // place is saved again below.
                transaction.execute(
                    "UPDATE playback_session SET current_entry_id = NULL WHERE id = 1",
                    [],
                )?;
            }
            if write.clear {
                transaction.execute("DELETE FROM queue_entry", [])?;
            }
            let mut remove = transaction.prepare("DELETE FROM queue_entry WHERE id = ?1")?;
            for row in &write.remove {
                remove.execute([row])?;
            }
            drop(remove);
            if write.shuffle == Some(false) {
                transaction.execute("UPDATE queue_entry SET shuffle_position = NULL", [])?;
            }
            let mut moved = transaction.prepare(
                "UPDATE queue_entry SET position = ?2, shuffle_position = ?3 WHERE id = ?1",
            )?;
            for (row, position, place) in &write.moved {
                moved.execute((row, position, place))?;
            }
            drop(moved);
            insert_entries(transaction, &write.insert, now_ms)?;
            if let Some(shuffle) = write.shuffle {
                transaction.execute(
                    "UPDATE playback_session SET shuffle = ?1 WHERE id = 1",
                    [shuffle],
                )?;
            }
            if let Some(repeat) = change.repeat {
                transaction.execute(
                    "UPDATE playback_session SET repeat = ?1 WHERE id = 1",
                    [repeat.as_str()],
                )?;
            }
            if let Some((volume, muted)) = change.volume {
                transaction.execute(
                    "UPDATE playback_session SET volume = ?1, muted = ?2 WHERE id = 1",
                    (volume, muted),
                )?;
            }
            let revision = if change.bump { current + 1 } else { current };
            transaction.execute(
                "UPDATE playback_session SET queue_revision = ?1 WHERE id = 1",
                [revision],
            )?;
            let (entry, position_ms, resume) = change.place;
            save_place(transaction, entry, position_ms, resume, now_ms)?;
            let answer = answer(revision);
            if let Some((command, method)) = &change.command {
                record_receipt(transaction, command, method, &answer, now_ms)?;
            }
            Ok(Wrote::Done {
                revision,
                answer,
                queue: change.queue,
            })
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

    /// The player's volume, 0 to 1, and whether it is muted.
    pub(crate) fn volume(&self) -> Result<(f64, bool), Error> {
        self.playback_session()?;
        self.read(|connection| {
            Ok(connection.query_row(
                "SELECT volume, muted FROM playback_session WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?)
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

    /// Saves listens as they are now, oldest first, each closed one queued
    /// for delivery, with the player's place: one transaction, which says
    /// the same however often it is written. `mono_ms` is the monotonic
    /// clock as the open listen was read.
    pub(crate) fn save_listens(
        &self,
        listens: Vec<Saved>,
        place: (Option<i64>, i64, Option<Id>),
        mono_ms: i64,
    ) -> Result<(), Error> {
        self.transact(move |transaction, now_ms, _| {
            for listen in &listens {
                upsert(transaction, listen, mono_ms, now_ms)?;
            }
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

    use super::*;
    use crate::OsEntropy;
    use crate::playback::queue::{Queue, Random};
    use crate::testing::FixedClock;

    const T0: i64 = 1_763_000_000_000;

    /// Puts `count` entries in place of the queue, as `playback.play` does,
    /// queued by the session in row `added_by`, and returns them with the
    /// command's answer.
    fn replace(
        store: &ServerStore,
        key: u8,
        count: u8,
        kind: ContextKind,
        added_by: Option<i64>,
    ) -> (Vec<Entry>, String) {
        let queue = Queue::default();
        let template = Entry {
            row: 0,
            entry_id: Id::from_bytes([0; 16]),
            appearance: Id::from_bytes([0xa0; 16]),
            position: 0,
            shuffle_position: None,
            context_kind: kind,
            context_ref: None,
            added_by,
            failed_reason: None,
        };
        let tracks = (0..count)
            .map(|at| (Id::from_bytes([0xe0 + at; 16]), Id::from_bytes([0xa0; 16])))
            .collect();
        let write = queue
            .replace(
                queue.fresh(tracks, &template),
                Some(0),
                false,
                &mut Random::new(&OsEntropy),
            )
            .unwrap();
        let first = write.insert.first().map(|entry| entry.row);
        let change = Change {
            command: Some((
                Command {
                    id: Id::from_bytes([key; 16]),
                    fingerprint: crate::Fingerprint::of("playback.play", b"{}"),
                },
                "playback.play",
            )),
            expected: None,
            queue: write,
            bump: true,
            repeat: None,
            volume: None,
            place: (first, 0, None),
        };
        match store
            .write_queue(change, |revision| revision.to_string())
            .unwrap()
        {
            Wrote::Done { answer, queue, .. } => (queue.insert, answer),
            Wrote::Replayed(_) => panic!("a new command is carried out"),
        }
    }

    #[test]
    fn one_listen_is_open_at_most_and_one_from_an_artists_page_is_taken() {
        let store = ServerStore::open_in_memory(
            Arc::new(FixedClock::at(T0)),
            Arc::new(crate::testing::CountingEntropy::default()),
        )
        .unwrap();
        store.playback_session().unwrap();
        let (queue, _) = replace(&store, 0xc5, 1, ContextKind::Artist, None);
        let opened = |listen: u8| Saved {
            started: Started {
                listen_id: Id::from_bytes([listen; 16]),
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
                context_kind: ContextKind::Artist,
                context_ref: None,
                shuffle: false,
            },
            heard: Heard {
                ranges_json: "[]".to_owned(),
                ..Heard::default()
            },
            position_ms: 0,
            ended: None,
        };
        let place = (Some(queue[0].row), 0, None);
        store
            .save_listens(vec![opened(0xd1)], place, 1_000)
            .unwrap();
        // A second is refused while the first is open.
        assert!(
            store
                .save_listens(vec![opened(0xd2)], place, 1_000)
                .is_err()
        );
        // Once the first is closed, as the next run closes it, it can open.
        assert_eq!(
            store.close_crashed_listens().unwrap(),
            [Id::from_bytes([0xd1; 16])]
        );
        store
            .save_listens(vec![opened(0xd2)], place, 2_000)
            .unwrap();
        assert_eq!(store.undelivered_listens().unwrap(), 1);
    }

    /// Two listens as a build that kept no monotonic clock at a checkpoint
    /// or an end left them staged, and as a database from then still holds
    /// them: one closed and waiting for the hub, which named no queue
    /// entry, and one a crash left open, whose entry and place were the
    /// session's.
    #[test]
    fn listens_staged_before_the_clocks_were_kept_are_read_and_the_open_one_closed_as_a_crash() {
        let store = ServerStore::open_in_memory(
            Arc::new(FixedClock::at(T0)),
            Arc::new(crate::testing::CountingEntropy::default()),
        )
        .unwrap();
        store.playback_session().unwrap();
        let (queue, _) = replace(&store, 0xc6, 2, ContextKind::Album, None);
        store.save_place(Some(queue[1].row), 10_000, None).unwrap();
        let (skipped, open) = (Id::from_bytes([0xd3; 16]), Id::from_bytes([0xd4; 16]));
        let entry = queue[1].entry_id;
        store
            .transact(move |transaction, _, _| {
                let stage = "INSERT INTO listen_staging (
                       listen_id, queue_entry_pid, listener_pid, played_on_server_pid,
                       appearance_pid, asset_pid, copy_kind, source_item_pid,
                       source_revision_no, started_at_utc_ms, zone_name, started_mono_ms,
                       boot_id, start_reason, start_position_ms, track_duration_ms, ms_played,
                       heard_ranges_json, max_position_ms, context_kind, shuffle,
                       checkpoint_position_ms, checkpoint_at_ms, checkpoint_mono_ms,
                       end_position_ms, end_reason, ended_at_utc_ms, ended_mono_ms)
                     VALUES (?1, ?2, zeroblob(16), zeroblob(16), x'a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0',
                       zeroblob(16), 'master', zeroblob(16), 1, ?3, 'America/New_York', ?4,
                       x'babababababababababababababababa', 'user_pick', 0, ?5, ?6, ?7, ?6,
                       'album', 0, ?8, ?9, NULL, ?10, ?11, ?12, NULL)";
                // Skipped at 1:40: closed, its place its end, no entry.
                transaction.execute(
                    stage,
                    rusqlite::params![
                        skipped.as_bytes(),
                        None::<Vec<u8>>,
                        T0,
                        1_000_000,
                        240_000,
                        100_000,
                        "[[0,100000]]",
                        100_000,
                        T0 + 100_000,
                        Some(100_000),
                        Some("next"),
                        Some(T0 + 100_000),
                    ],
                )?;
                transaction.execute(
                    "INSERT INTO outbox (kind, dedupe_key, next_attempt_at_ms, created_at_ms)
                     VALUES ('history.ingest', ?1, ?2, ?2)",
                    (skipped.as_bytes(), T0 + 100_000),
                )?;
                // Open at its 10-second checkpoint when the run died.
                transaction.execute(
                    stage,
                    rusqlite::params![
                        open.as_bytes(),
                        Some(entry.as_bytes()),
                        T0 + 100_000,
                        1_100_000,
                        300_000,
                        10_000,
                        "[[0,10000]]",
                        10_000,
                        T0 + 110_000,
                        None::<i64>,
                        None::<&str>,
                        None::<i64>,
                    ],
                )?;
                Ok(())
            })
            .unwrap();

        let read = |listen| store.staged_listen(listen).unwrap().unwrap();
        let before = read(skipped);
        assert_eq!(before.started.entry, None);
        assert_eq!(
            before.ended,
            Some(Ended {
                end_position_ms: 100_000,
                end_reason: EndReason::Next,
                ended_at_ms: T0 + 100_000,
                ended_mono_ms: None,
            })
        );
        assert_eq!(
            (
                before.heard.ms_played,
                before.checkpoint_position_ms,
                before.checkpoint_mono_ms
            ),
            (100_000, 100_000, None)
        );
        let left = read(open);
        assert_eq!(
            (left.started.entry, left.ended.clone()),
            (Some(entry), None)
        );
        assert_eq!(
            (left.checkpoint_position_ms, left.checkpoint_mono_ms),
            (10_000, None)
        );
        assert_eq!(store.undelivered_listens().unwrap(), 1);

        // The next run closes it where and when its last checkpoint was,
        // with no clock reading to give, and a resume of the session's
        // entry continues it.
        assert_eq!(store.close_crashed_listens().unwrap(), [open]);
        let closed = read(open);
        assert_eq!(
            closed.ended,
            Some(Ended {
                end_position_ms: 10_000,
                end_reason: EndReason::Crash,
                ended_at_ms: T0 + 110_000,
                ended_mono_ms: None,
            })
        );
        assert_eq!(closed.heard, left.heard);
        assert_eq!(store.playback_session().unwrap().resume_listen, Some(open));
        assert_eq!(store.undelivered_listens().unwrap(), 2);
        // The closed one is as it was.
        assert_eq!(read(skipped), before);
    }

    #[test]
    fn a_listen_left_open_is_what_a_resume_continues_only_if_its_entry_is_the_sessions() {
        let store = ServerStore::open_in_memory(
            Arc::new(FixedClock::at(T0)),
            Arc::new(crate::testing::CountingEntropy::default()),
        )
        .unwrap();
        store.playback_session().unwrap();
        let (queue, _) = replace(&store, 0xc4, 2, ContextKind::Album, None);
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
        // Saved as it opened, and again five seconds in: the second save
        // says what the first did, and more.
        for (ms, ranges) in [(0, "[]"), (5_000, "[[0,5000]]")] {
            let saved = Saved {
                started: started.clone(),
                heard: Heard {
                    ms_played: ms,
                    ranges_json: ranges.to_owned(),
                    max_position_ms: ms,
                    ..Heard::default()
                },
                position_ms: ms,
                ended: None,
            };
            let place = (Some(queue[0].row), ms, None);
            store.save_listens(vec![saved], place, 1_000 + ms).unwrap();
        }
        // The session moved on to the second entry, and saved a place there,
        // before the run died with the first entry's listen open.
        store.save_place(Some(queue[1].row), 42_000, None).unwrap();
        assert_eq!(store.close_crashed_listens().unwrap(), [started.listen_id]);
        let closed = store.staged_listen(started.listen_id).unwrap().unwrap();
        assert_eq!(closed.heard.ms_played, 5_000);
        assert_eq!(closed.ended.unwrap().end_position_ms, 5_000);
        assert_eq!(store.playback_session().unwrap().resume_listen, None);
    }
}
