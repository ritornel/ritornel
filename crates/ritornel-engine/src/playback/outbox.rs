//! Delivering finished listens to the hub, from the outbox. See decision 21
//! in docs/design/playback.md and "An effect, a job and a lease" in
//! docs/design/server.md.
//!
//! A listen is staged in `server.db` and its delivery is an outbox row,
//! written in the transaction that closed it. Delivering is the one ingest
//! call, which on a single server is a call into the library here. Only
//! once the hub has the listen do its staging row and its outbox row go,
//! so a crash between the two sends it again, and the hub, keyed on the
//! listen's ID, writes nothing the second time.

use crate::history_store::{Ingested, LISTEN_RECORD_VERSION, ListenRecord};
use crate::playback::listen::Ranges;
use crate::playback::store::Staged;
use crate::{Clock, Error, LibraryStore, ServerStore};

pub use crate::history_store::SendStamps;

/// The most deliveries one pass makes.
const PER_PASS: u32 = 100;

/// The longest a failed delivery waits before it is tried again.
const MAX_BACKOFF_MS: i64 = 5 * 60 * 1000;

/// What a pass of deliveries did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Delivered {
    /// Listens the hub now has, and whose rows here are gone.
    pub delivered: Vec<crate::Id>,
    /// Of those, the ones it had already: a resend.
    pub duplicates: usize,
    /// Deliveries that failed, each with why, to be tried again.
    pub failed: Vec<(crate::Id, String)>,
    /// When the next delivery that is not due yet is due.
    pub next_due_ms: Option<i64>,
}

/// The record a staged listen is sent as.
fn record(staged: &Staged, actor: Option<crate::Id>) -> Result<ListenRecord, Error> {
    let ended = staged
        .ended
        .as_ref()
        .ok_or(Error::Corrupt("a listen to deliver that is still open"))?;
    let started = &staged.started;
    Ok(ListenRecord {
        v: LISTEN_RECORD_VERSION,
        listen_id: started.listen_id,
        listener: started.listener,
        actor_session: actor,
        played_on_server: started.played_on_server,
        appearance: started.appearance,
        asset: started.asset,
        copy_kind: "master".to_owned(),
        source_item: started.source_item,
        source_revision: started.source_revision,
        started_at_ms: started.started_at_ms,
        zone: started.zone.clone(),
        started_mono_ms: started.started_mono_ms,
        boot: started.boot,
        start_reason: started.start_reason.as_str().to_owned(),
        continues: started.continues,
        start_position_ms: started.start_position_ms,
        track_duration_ms: started.track_duration_ms,
        ms_played: staged.heard.ms_played,
        heard_ranges: Ranges::parse(&staged.heard.ranges_json),
        heard_ranges_approximate: staged.heard.ranges_approximate,
        max_position_ms: staged.heard.max_position_ms,
        seek_count: staged.heard.seek_count,
        pause_count: staged.heard.pause_count,
        end_position_ms: ended.end_position_ms,
        end_reason: ended.end_reason.as_str().to_owned(),
        ended_at_ms: ended.ended_at_ms,
        ended_mono_ms: ended.ended_mono_ms,
        context_kind: started.context_kind.as_str().to_owned(),
        context_ref: started.context_ref,
        shuffle: started.shuffle,
    })
}

/// Delivers the listens whose turn has come to the library, and removes
/// each from `server.db` once the library has it. `stamps` stamps each send
/// afresh. A delivery that fails is tried again later, waiting twice as
/// long each time up to five minutes.
///
/// `after_ingest` is for the crash test alone: it runs between the
/// library's commit and the removal here, where a crash leaves a listen the
/// hub has and this server still holds. An error from it stops the pass
/// there.
pub fn deliver_listens(
    server: &ServerStore,
    library: &LibraryStore,
    clock: &dyn Clock,
    stamps: &dyn Fn() -> SendStamps,
    after_ingest: &dyn Fn() -> Result<(), Error>,
) -> Result<Delivered, Error> {
    let now = clock.now_ms();
    let (due, mut next_due_ms) = server.due_deliveries(now, PER_PASS)?;
    let mut done = Delivered::default();
    for delivery in due {
        let sent = server
            .staged_listen(delivery.listen_id)?
            .ok_or(Error::Corrupt(
                "an outbox row for a listen that is not staged",
            ))
            .and_then(|staged| {
                let actor = match staged.started.actor_session {
                    Some(row) => server.session_public_id(row)?,
                    None => None,
                };
                record(&staged, actor)
            })
            .and_then(|record| library.ingest_listen(&record, stamps()));
        match sent {
            Ok(ingested) => {
                after_ingest()?;
                server.delivered(delivery.listen_id)?;
                if ingested == Ingested::Duplicate {
                    done.duplicates += 1;
                }
                done.delivered.push(delivery.listen_id);
            }
            Err(error) => {
                let wait = (1000_i64 << delivery.attempts.clamp(0, 20)).min(MAX_BACKOFF_MS);
                let at = now.saturating_add(wait);
                server.delivery_failed(delivery.row, error.to_string(), at)?;
                next_due_ms = Some(next_due_ms.map_or(at, |next| next.min(at)));
                done.failed.push((delivery.listen_id, error.to_string()));
            }
        }
    }
    done.next_due_ms = next_due_ms;
    Ok(done)
}
