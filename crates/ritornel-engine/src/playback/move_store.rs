//! Durable move intent and the source releases still owed by the hub.

use crate::{
    Command, Error, ServerStore,
    jobs::{receipt, record_receipt},
};
use ritornel_contract::{Id, PlaybackMoveParams, Receipt, ReceiptStatus};
use serde::{Deserialize, Serialize};

/// A load that may have reached the destination and whose source still needs release.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
pub struct PendingMove {
    pub movement: PlaybackMoveParams,
    pub attempt: Id,
}

impl ServerStore {
    /// Reserve the caller's key before touching either player.
    pub fn begin_move(&self, command: Command) -> Result<Receipt, Error> {
        self.transact(move |tx, now, _| {
            if let Some(saved) = receipt(tx, &command)? {
                return Ok(serde_json::from_str(&saved)?);
            }
            let result = Receipt {
                command_id: command.id,
                status: ReceiptStatus::Accepted,
                revision: None,
            };
            record_receipt(
                tx,
                &command,
                "playback.move",
                &serde_json::to_string(&result)?,
                now,
            )?;
            Ok(result)
        })
    }

    /// Remember whom to check before sending load. A lost answer is resolved
    /// by asking for its receipt, without replaying audio on either player.
    pub fn expect_load(&self, pending: PendingMove) -> Result<(), Error> {
        self.transact(move |tx, _, _| {
            tx.execute(
                "INSERT INTO server_setting(key,value_json) VALUES (?1,?2)",
                (
                    format!("move.{}", pending.movement.idempotency_key),
                    serde_json::to_string(&pending)?,
                ),
            )?;
            Ok(())
        })
    }

    pub fn accepted_load(&self, movement: PlaybackMoveParams) -> Result<Receipt, Error> {
        let result = Receipt {
            command_id: movement.idempotency_key,
            status: ReceiptStatus::Done,
            revision: None,
        };
        self.transact(move |tx, _, _| {
            tx.execute(
                "UPDATE command_receipt SET result_json=?2
                 WHERE command_id=?1 AND method='playback.move'",
                (
                    movement.idempotency_key.as_bytes(),
                    serde_json::to_string(&result)?,
                ),
            )?;
            Ok(result)
        })
    }

    pub fn pending_moves(&self) -> Result<Vec<PendingMove>, Error> {
        self.read(|db| {
            let mut q = db.prepare(
                "SELECT value_json FROM server_setting WHERE key GLOB 'move.*' ORDER BY key",
            )?;
            q.query_map([], |r| r.get::<_, String>(0))?
                .map(|row| Ok(serde_json::from_str(&row?)?))
                .collect()
        })
    }

    pub fn forget_release(&self, pending: PendingMove) -> Result<(), Error> {
        self.transact(move |tx, _, _| {
            tx.execute(
                "DELETE FROM server_setting WHERE key=?1 AND json_extract(value_json,'$.attempt')=?2",
                (format!("move.{}", pending.movement.idempotency_key),pending.attempt.to_string()),
            )?;
            Ok(())
        })
    }

    /// Only the current take retains its queue snapshot. Older take receipts
    /// still identify retries after that queue has changed or been released.
    pub(super) fn taken(
        &self,
        command: Command,
    ) -> Result<Option<ritornel_contract::HandoffResult>, Error> {
        use rusqlite::OptionalExtension;
        self.read(|db| {
            let saved: Option<String> = db
                .query_row(
                    "SELECT value_json FROM server_setting WHERE key=?1",
                    [format!("handoff.take.{}", command.id)],
                    |row| row.get(0),
                )
                .optional()?;
            saved
                .map(|s| serde_json::from_str(&s).map_err(Error::from))
                .transpose()
        })
    }
}

pub(super) fn record_player_receipt(
    tx: &rusqlite::Transaction<'_>,
    command: &Command,
    method: &str,
    result: &str,
    now: i64,
) -> Result<(), Error> {
    // Resuming and pausing again can leave the same queue and position.
    // A deferred release must still leave that playback alone.
    let takes = method == "link.handoff.take";
    if takes
        || matches!(
            method,
            "playback.play"
                | "playback.resume"
                | "playback.seek"
                | "playback.skip"
                | "playback.jump"
                | "playback.stop"
        )
        || method.starts_with("queue.")
        || method == "link.handoff.load"
    {
        tx.execute(
            "DELETE FROM server_setting WHERE key GLOB 'handoff.take.*'",
            [],
        )?;
    }
    if takes {
        tx.execute(
            "INSERT INTO server_setting(key,value_json) VALUES(?1,?2)",
            (format!("handoff.take.{}", command.id), result),
        )?;
    }
    record_receipt(
        tx,
        command,
        method,
        if takes { "null" } else { result },
        now,
    )
}
