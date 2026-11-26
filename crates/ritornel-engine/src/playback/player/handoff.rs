//! The player's three durable steps of a move.

use super::*;
use crate::Fingerprint;
use crate::playback::queue::{GAP, QueueWrite};
use ritornel_contract::{
    ContextKind as Kind, HandoffEntry, HandoffResult, HandoffSnapshot, LinkHandoffParams,
    PlayContext, PlaybackMoveParams, Repeat as WireRepeat,
};

fn step(movement: PlaybackMoveParams, n: i64, name: &str) -> Result<Command, Error> {
    Ok(Command {
        id: crate::step_command(movement.idempotency_key, n),
        fingerprint: Fingerprint::of(name, &serde_json::to_vec(&movement)?),
    })
}

fn receipt_request(
    command: Command,
    method: &'static str,
    result: &HandoffResult,
) -> Result<Request, Error> {
    let answer = serde_json::to_string(result)?;
    Ok(Request {
        command,
        method,
        answer: Box::new(move |_| answer),
    })
}

impl Player {
    /// Runs an authenticated handoff step, including a receipt-only load check.
    pub fn handoff(
        &mut self,
        params: LinkHandoffParams,
        server: &ServerStore,
        events: &mut Vec<PlayerEvent>,
    ) -> Result<HandoffResult, Error> {
        self.sync(server, events);
        let result = match params {
            LinkHandoffParams::Take { movement } => {
                let command = step(movement, 1, "link.handoff.take")?;
                if server.stored_result(command)?.is_some() {
                    match server.taken(command)? {
                        Some(HandoffResult::Taken { snapshot }) if self.still_taken(&snapshot) => {
                            HandoffResult::Taken { snapshot }
                        }
                        _ => return Err(Error::Invalid("the source changed after it was taken")),
                    }
                } else {
                    self.take(movement, command, server, events)?
                }
            }
            LinkHandoffParams::Load {
                movement,
                attempt,
                snapshot,
            } => {
                let command = step(movement, 2, "link.handoff.load")?;
                if let Some(saved) = server.stored_result(command)? {
                    serde_json::from_str(&saved)?
                } else {
                    // Serialize the probe and the load under the player lock. A
                    // load arriving after its probe must never replace the queue.
                    let probe = Command {
                        id: attempt,
                        fingerprint: Fingerprint::of(
                            "link.handoff.probe",
                            &serde_json::to_vec(&movement)?,
                        ),
                    };
                    if server.stored_result(probe)?.is_some() {
                        HandoffResult::NotLoaded
                    } else if let Some(snapshot) = snapshot {
                        self.load(movement, command, snapshot, server, events)?;
                        HandoffResult::Loaded
                    } else {
                        server.record_command(
                            probe,
                            "link.handoff.probe",
                            serde_json::to_string(&HandoffResult::NotLoaded)?,
                            None,
                        )?;
                        HandoffResult::NotLoaded
                    }
                }
            }
            LinkHandoffParams::Release { movement } => {
                let take = step(movement, 1, "link.handoff.take")?;
                let command = step(movement, 3, "link.handoff.release")?;
                if server.stored_result(command)?.is_none() {
                    if server.stored_result(take)?.is_none() {
                        return Err(Error::Invalid("this player never took the source"));
                    }
                    let request =
                        receipt_request(command, "link.handoff.release", &HandoffResult::Released)?;
                    if matches!(server.taken(take)?, Some(HandoffResult::Taken { snapshot }) if self.still_taken(&snapshot))
                    {
                        self.recorder.handoff_end = self.taken_at.take();
                        let done = self.clear_for(request, None, server, EndReason::Stopped)?;
                        events.extend(done.events);
                    } else {
                        self.receipt(server, request, Vec::new())?;
                    }
                }
                server.forget_take(take)?;
                HandoffResult::Released
            }
        };
        Ok(result)
    }

    fn still_taken(&self, s: &HandoffSnapshot) -> bool {
        self.status == Status::Paused
            && u64::try_from(self.queue_revision).ok() == Some(s.queue_revision)
            && self.entry().is_some_and(|e| e.entry_id == s.entry_id)
    }

    fn take(
        &mut self,
        m: PlaybackMoveParams,
        command: Command,
        server: &ServerStore,
        events: &mut Vec<PlayerEvent>,
    ) -> Result<HandoffResult, Error> {
        if self.ran_out.is_some()
            || self.status == Status::Stopped
            || self.entry().is_none_or(|e| e.entry_id != m.entry_id)
        {
            return Err(Error::Invalid(
                "that entry is no longer active at the source",
            ));
        }
        // Even the two IDs alone take 72 bytes. Bound the allocation before
        // encoding the full load to check its actual wire size.
        if self.queue.len() > (1 << 20) / 72 {
            return Err(Error::Invalid(
                "the queue exceeds the handoff message limit",
            ));
        }
        let entries = self
            .queue
            .iter()
            .map(|e| HandoffEntry {
                entry_id: e.entry_id,
                appearance_id: e.appearance,
                shuffle_position: e.shuffle_position,
                context: PlayContext {
                    reference: e.context_ref,
                    kind: match e.context_kind {
                        ContextKind::Manual => Kind::Manual,
                        ContextKind::Album => Kind::Album,
                        ContextKind::Artist => Kind::Artist,
                        ContextKind::Playlist => Kind::Playlist,
                        ContextKind::Query => Kind::Query,
                        ContextKind::Agent => Kind::Agent,
                    },
                },
            })
            .collect();
        let mut snapshot = HandoffSnapshot {
            entries,
            entry_id: m.entry_id,
            position_ms: self.position().unsigned_abs(),
            playing: self.status != Status::Paused,
            shuffle: self.queue.shuffled(),
            repeat: match self.repeat {
                Repeat::Off => WireRepeat::Off,
                Repeat::One => WireRepeat::One,
                Repeat::All => WireRepeat::All,
            },
            listen: self.open_listen().or(self.resume_listen),
            listener: self.listener,
            queue_revision: self.queue_revision.unsigned_abs(),
        };
        // Leave room for the load envelope, IDs, and the final position.
        if serde_json::to_vec(&snapshot)?.len() > (1 << 20) - 1024 {
            return Err(Error::Invalid(
                "the queue exceeds the handoff message limit",
            ));
        }
        self.pause_output(server, events, Some(m.entry_id))?;
        snapshot.position_ms = self.position().unsigned_abs();
        snapshot.listen = self.open_listen().or(self.resume_listen);
        let result = HandoffResult::Taken { snapshot };
        self.receipt(
            server,
            receipt_request(command, "link.handoff.take", &result)?,
            Vec::new(),
        )?;
        self.taken_at = self.open_listen().map(|id| {
            (
                id,
                (self.setup.clock.now_ms(), self.setup.timekeeper.mono_ms()),
            )
        });
        Ok(result)
    }

    fn load(
        &mut self,
        m: PlaybackMoveParams,
        command: Command,
        s: HandoffSnapshot,
        server: &ServerStore,
        events: &mut Vec<PlayerEvent>,
    ) -> Result<(), Error> {
        use std::collections::HashSet;
        let position =
            i64::try_from(s.position_ms).map_err(|_| Error::Invalid("invalid handoff position"))?;
        let mut ids = HashSet::new();
        let mut positions = HashSet::new();
        if s.entry_id != m.entry_id
            || s.entries.iter().any(|e| {
                !ids.insert(e.entry_id)
                    || e.shuffle_position.is_some() != s.shuffle
                    || e.shuffle_position.is_some_and(|p| !positions.insert(p))
            })
            || !ids.contains(&s.entry_id)
        {
            return Err(Error::Invalid("invalid handoff queue or current entry"));
        }
        let repeat = match s.repeat {
            WireRepeat::Off => Repeat::Off,
            WireRepeat::All => Repeat::All,
            WireRepeat::One => Repeat::One,
        };
        let mut current = None;
        let entries = s
            .entries
            .into_iter()
            .zip(1_i64..)
            .map(|(e, row)| {
                if e.entry_id == s.entry_id {
                    current = Some(row);
                }
                Entry {
                    row,
                    entry_id: e.entry_id,
                    appearance: e.appearance_id,
                    position: row * GAP,
                    shuffle_position: e.shuffle_position,
                    context_ref: e.context.reference,
                    context_kind: match e.context.kind {
                        Kind::Manual => ContextKind::Manual,
                        Kind::Album => ContextKind::Album,
                        Kind::Artist => ContextKind::Artist,
                        Kind::Playlist => ContextKind::Playlist,
                        Kind::Query => ContextKind::Query,
                        Kind::Agent => ContextKind::Agent,
                    },
                    added_by: None,
                    failed_reason: None,
                }
            })
            .collect();
        self.leave(server, EndReason::Replaced, events);
        let change = Change {
            command: None,
            expected: None,
            listener: s.listener,
            repeat: Some(repeat),
            volume: None,
            bump: true,
            place: (current, position, s.listen),
            queue: QueueWrite {
                clear: true,
                insert: entries,
                shuffle: Some(s.shuffle),
                ..Default::default()
            },
        };
        let request = receipt_request(command, "link.handoff.load", &HandoffResult::Loaded)?;
        self.commit(server, Some(request), change, Some(s.entry_id))?;
        self.listener = s.listener.or(self.listener);
        self.repeat = repeat;
        self.resume_listen = s.listen;
        self.position_ms = position;
        self.display = None;
        self.fetching = None;
        self.start_reason = StartReason::Resume;
        self.failures_in_a_row = 0;
        self.paused_buffering = !s.playing;
        self.status = if s.playing {
            Status::Buffering
        } else {
            Status::Paused
        };
        self.start_handoff = true;
        events.push(PlayerEvent::Queue(self.queue_revision));
        self.changed(events);
        Ok(())
    }
}
