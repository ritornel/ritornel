//! Hub coordination for moving a queue. See "Queue and session" in docs/internals/playback.md.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use ritornel_contract::{
    ErrorCode, HandoffResult, Id, LinkHandoffParams, PlaybackMoveParams, Receipt, ReceiptStatus,
    RpcError, methods::PlaybackMove,
};
use ritornel_engine::{Command, ServerStore, playback::PendingMove};
use tokio::sync::Notify;

use super::{
    Core, fingerprint, lock,
    playback::{no_player, refused},
};

const STEP_WAIT: Duration = Duration::from_secs(3);
const RELEASE_RETRY: Duration = Duration::from_secs(2);

#[derive(Debug, Default)]
pub(super) struct Moves {
    core: OnceLock<Weak<Core>>,
    using: Mutex<HashMap<Id, Id>>,
    finished: Condvar,
    wake: Notify,
    stopped: AtomicBool,
}

struct InUse<'a, const N: usize> {
    moves: &'a Moves,
    places: [Id; N],
}

impl<const N: usize> Drop for InUse<'_, N> {
    fn drop(&mut self) {
        let mut using = lock(&self.moves.using);
        for id in self.places {
            using.remove(&id);
        }
        self.moves.finished.notify_all();
    }
}

fn busy() -> RpcError {
    RpcError::new(
        ErrorCode::Unavailable,
        "a move is in progress on this player",
    )
}

impl Moves {
    /// Wait only for another invocation of this same command. Unrelated
    /// controllers get a busy reply immediately when their players overlap.
    fn claim<const N: usize>(
        &self,
        places: [Id; N],
        key: Id,
        wait: Duration,
    ) -> Option<InUse<'_, N>> {
        let using = lock(&self.using);
        let (mut using, _) = self
            .finished
            .wait_timeout_while(using, wait, |using| {
                !wait.is_zero()
                    && places
                        .iter()
                        .filter_map(|p| using.get(p))
                        .any(|k| *k == key)
            })
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if places.iter().any(|p| using.contains_key(p)) {
            return None;
        }
        using.extend(places.map(|p| (p, key)));
        Some(InUse {
            moves: self,
            places,
        })
    }

    pub(super) fn player_available(&self) {
        self.wake.notify_one();
    }
}

fn remaining(deadline: Instant) -> Result<Duration, RpcError> {
    let time = deadline.saturating_duration_since(Instant::now());
    if time.is_zero() {
        Err(busy())
    } else {
        Ok(time.min(STEP_WAIT))
    }
}

fn permanent(error: &RpcError) -> bool {
    matches!(
        error.kind(),
        ErrorCode::Invalid
            | ErrorCode::Forbidden
            | ErrorCode::Unsupported
            | ErrorCode::IdempotencyMismatch
    )
}

fn stopping() -> RpcError {
    RpcError::new(ErrorCode::Unavailable, "the server is stopping")
}
fn unexpected() -> RpcError {
    RpcError::new(ErrorCode::Internal, "unexpected handoff reply")
}

impl Core {
    pub(crate) fn start_moves(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let _ = self.moves.core.set(Arc::downgrade(self));
        let core = Arc::clone(self);
        tokio::spawn(async move {
            while !core.moves.stopped.load(Ordering::Acquire) {
                let work = Arc::clone(&core);
                let Ok(pending) = tokio::task::spawn_blocking(move || work.reconcile_moves()).await
                else {
                    break;
                };
                if pending {
                    tokio::select! {
                        () = core.moves.wake.notified() => {},
                        () = tokio::time::sleep(RELEASE_RETRY) => {},
                    }
                } else {
                    core.moves.wake.notified().await;
                }
            }
        })
    }

    pub(crate) fn stop_moves(&self) {
        self.moves.stopped.store(true, Ordering::Release);
        self.moves.wake.notify_one();
    }

    fn move_store<T>(
        &self,
        work: impl FnOnce(&ServerStore) -> Result<T, ritornel_engine::Error>,
    ) -> Result<T, RpcError> {
        self.with_stores(|s| work(&s.server))
            .ok_or_else(stopping)?
            .map_err(refused)
    }

    fn check_place(&self, id: Id) -> Result<(), RpcError> {
        if id == self.server_id {
            if self.has_player() {
                Ok(())
            } else {
                Err(no_player())
            }
        } else {
            self.require_online_player(id)
        }
    }

    pub(super) fn local_handoff(&self, p: LinkHandoffParams) -> Result<HandoffResult, RpcError> {
        let target = match &p {
            LinkHandoffParams::Load { movement, .. } => movement.to,
            LinkHandoffParams::Take { movement } | LinkHandoffParams::Release { movement } => {
                movement.from
            }
        };
        if target != self.server_id {
            return Err(RpcError::new(
                ErrorCode::Invalid,
                "handoff names another player",
            ));
        }
        self.with_stores(|s| {
            self.with_player(s, |player, server, _| {
                let mut events = Vec::new();
                let result = player.handoff(p, server, &mut events).map_err(refused);
                Ok((result, events))
            })
        })
        .ok_or_else(stopping)??
    }

    fn handoff_step(
        &self,
        id: Id,
        p: LinkHandoffParams,
        within: Duration,
    ) -> Result<HandoffResult, RpcError> {
        let rt = self.runtime.as_ref().ok_or_else(stopping)?;
        if id == self.server_id {
            let core = self
                .moves
                .core
                .get()
                .and_then(Weak::upgrade)
                .ok_or_else(stopping)?;
            rt.block_on(async {
                tokio::time::timeout(
                    within,
                    tokio::task::spawn_blocking(move || core.local_handoff(p)),
                )
                .await
                .map_err(|_| {
                    RpcError::new(
                        ErrorCode::Unavailable,
                        "the local player did not answer the handoff in time",
                    )
                })?
                .map_err(|_| stopping())?
            })
        } else {
            let wire = serde_json::to_value(p).map_err(|_| unexpected())?;
            let answer =
                rt.block_on(self.call_player_within(id, "link.handoff", Some(wire), within))?;
            serde_json::from_value(answer).map_err(|_| unexpected())
        }
    }

    pub(super) fn move_playback(&self, m: PlaybackMoveParams) -> Result<Receipt, RpcError> {
        if m.from == m.to {
            return Err(RpcError::new(
                ErrorCode::Invalid,
                "choose two different players",
            ));
        }
        let command = Command {
            id: m.idempotency_key,
            fingerprint: fingerprint::<PlaybackMove>(&m)?,
        };
        let receipt = self.move_store(|s| s.begin_move(command))?;
        if receipt.status == ReceiptStatus::Done {
            return Ok(receipt);
        }
        // Leave a full second inside the uplink timeout. A resend may
        // have to wait for a probe and then replay the two player steps.
        let deadline = Instant::now() + Duration::from_secs(7);
        let claim = self
            .moves
            .claim(
                [m.from, m.to],
                m.idempotency_key,
                deadline.saturating_duration_since(Instant::now()),
            )
            .ok_or_else(busy)?;
        // A pending source may still be released. Nor may a late load
        // overwrite another move on a destination whose answer was lost.
        for pending in self.move_store(ServerStore::pending_moves)? {
            let prior = pending.movement;
            if prior.from == m.from || prior.from == m.to || prior.to == m.from || prior.to == m.to
            {
                let status = self.move_receipt(prior)?;
                if prior == m {
                    if status.status == ReceiptStatus::Done {
                        return Ok(status);
                    }
                    if let Some(done) = self.probe_load(pending, remaining(deadline)?)? {
                        drop(claim);
                        self.moves.wake.notify_one();
                        return Ok(done);
                    }
                    continue;
                }
                if prior.from == m.from
                    || prior.from == m.to
                    || status.status != ReceiptStatus::Done
                {
                    return Err(busy());
                }
            }
        }
        // A worker may have finished a pending move between the first
        // receipt check and our claim.
        let saved = self.move_receipt(m)?;
        if saved.status == ReceiptStatus::Done {
            return Ok(saved);
        }
        let result = self.transfer(m, deadline);
        drop(claim);
        self.moves.wake.notify_one();
        result
    }

    fn move_receipt(&self, m: PlaybackMoveParams) -> Result<Receipt, RpcError> {
        let command = Command {
            id: m.idempotency_key,
            fingerprint: fingerprint::<PlaybackMove>(&m)?,
        };
        self.move_store(|s| s.stored_result(command))?
            .and_then(|s| serde_json::from_str(&s).ok())
            .ok_or_else(unexpected)
    }

    fn transfer(&self, m: PlaybackMoveParams, deadline: Instant) -> Result<Receipt, RpcError> {
        self.check_place(m.to)?;
        let HandoffResult::Taken { snapshot } = self.handoff_step(
            m.from,
            LinkHandoffParams::Take { movement: m },
            remaining(deadline)?,
        )?
        else {
            return Err(unexpected());
        };
        let attempt = ritornel_engine::new_id(self.now_ms(), &*self.entropy).map_err(refused)?;
        let pending = PendingMove {
            movement: m,
            attempt,
        };
        self.move_store(|s| s.expect_load(pending))?;
        let reply = self.handoff_step(
            m.to,
            LinkHandoffParams::Load {
                movement: m,
                attempt,
                snapshot: Some(snapshot),
            },
            remaining(deadline)?,
        );
        match reply {
            Ok(HandoffResult::Loaded) => self.move_store(|s| s.accepted_load(m)),
            Ok(_) => Err(unexpected()),
            Err(e) => {
                if permanent(&e)
                    || e.data.details.get("reason").and_then(|v| v.as_str()) == Some("player_busy")
                {
                    self.move_store(|s| s.forget_move(pending))?;
                }
                Err(e)
            }
        }
    }

    fn probe_load(
        &self,
        journal: PendingMove,
        within: Duration,
    ) -> Result<Option<Receipt>, RpcError> {
        let m = journal.movement;
        let check = LinkHandoffParams::Load {
            movement: m,
            attempt: journal.attempt,
            snapshot: None,
        };
        match self.handoff_step(m.to, check, within)? {
            HandoffResult::Loaded => self.move_store(|s| s.accepted_load(m)).map(Some),
            HandoffResult::NotLoaded => {
                self.move_store(|s| s.forget_move(journal))?;
                Ok(None)
            }
            _ => Err(unexpected()),
        }
    }

    /// Reconcile lost load answers and retry releases. A receipt probe cannot
    /// install a queue, even if the destination never received the original load.
    fn reconcile_moves(&self) -> bool {
        let Ok(moves) = self.move_store(ServerStore::pending_moves) else {
            return true;
        };
        let mut pending = false;
        for journal in moves {
            if self.moves.stopped.load(Ordering::Acquire) {
                break;
            }
            let m = journal.movement;
            let result = (|| {
                if self.move_receipt(m)?.status != ReceiptStatus::Done {
                    let _claim = self
                        .moves
                        .claim([m.from, m.to], m.idempotency_key, Duration::ZERO)
                        .ok_or_else(busy)?;
                    if self.move_receipt(m)?.status != ReceiptStatus::Done
                        && self.probe_load(journal, STEP_WAIT)?.is_none()
                    {
                        return Ok(());
                    }
                }
                let _source = self
                    .moves
                    .claim([m.from], m.idempotency_key, Duration::ZERO)
                    .ok_or_else(busy)?;
                if self.handoff_step(
                    m.from,
                    LinkHandoffParams::Release { movement: m },
                    STEP_WAIT,
                )? != HandoffResult::Released
                {
                    return Err(unexpected());
                }
                self.move_store(|s| s.forget_move(journal))?;
                Ok(())
            })();
            // A missing player or a definitive rejection ends this obligation.
            // An offline player can return: retain its journal until then.
            if let Err(e) = result {
                if permanent(&e) {
                    tracing::warn!(movement = %m.idempotency_key, reason = %e.message, "ending a refused handoff");
                    pending |= self.move_store(|s| s.forget_move(journal)).is_err();
                } else {
                    pending = true;
                }
            }
        }
        pending
    }
}
