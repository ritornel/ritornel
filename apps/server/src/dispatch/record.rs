//! Whether the listening record is advancing, for `server.status`, and
//! `record.dismiss`. Music can go on while the record stops: a disk that is
//! full fails a listen's save, a scrobble can end unanswered, an import can
//! fail. Each is said here, with since when, until it is put right. A
//! scrobble that ended unanswered is put right when an import brings it
//! back; one that does not come back, or that was never sent, is said
//! until it is dismissed.

use ritornel_contract::{
    DismissParams, Dismissed, Receipt, ReceiptStatus, RecordProblem, RecordProblemKind,
    RecordStatus, RpcError,
};
use ritornel_engine::Failing;
use tracing::{info, warn};

use super::{Core, Stores, lock};

/// A listen the player could not save: since when, and what the last
/// attempt said.
#[derive(Debug, Default)]
pub(crate) struct Unsaved(Option<(i64, String)>);

impl Core {
    /// The player saved what it had to, or had nothing to save.
    pub(crate) fn listen_saved(&self) {
        if lock(&self.unsaved).0.take().is_some() {
            info!("the player saves its listens again");
        }
    }

    /// The player could not save the listen it is recording. It tries
    /// again at its next tick; until one works, the record has stopped.
    pub(crate) fn listen_unsaved(&self, detail: String) {
        let now_ms = self.clock.now_ms();
        let mut unsaved = lock(&self.unsaved);
        let since = unsaved.0.as_ref().map_or(now_ms, |(since, _)| *since);
        unsaved.0 = Some((since, detail));
    }

    /// What stops the record now, oldest first.
    pub(super) fn record_status(&self, stores: &Stores) -> RecordStatus {
        let mut problems = Vec::new();
        let mut add = |kind, failing: Option<Failing>, dismissable| {
            if let Some(failing) = failing {
                problems.push(RecordProblem {
                    kind,
                    since: failing.since_ms,
                    count: failing.count,
                    detail: failing.detail,
                    dismissable,
                });
            }
        };
        let unsaved = lock(&self.unsaved).0.clone();
        add(
            RecordProblemKind::ListenUnsaved,
            unsaved.map(|(since_ms, detail)| Failing {
                since_ms,
                count: 1,
                detail: Some(detail),
            }),
            false,
        );
        let server = stores.server();
        let read = |what: &str, found: Result<Option<Failing>, ritornel_engine::Error>| {
            found.unwrap_or_else(|error| {
                warn!(%error, "what stops the record could not be read: {what}");
                None
            })
        };
        add(
            RecordProblemKind::ListensUndelivered,
            read("listens", server.failing_deliveries()),
            false,
        );
        add(
            RecordProblemKind::ImportFailed,
            read("imports", server.failing_jobs(crate::lastfm::SYNC_JOB)),
            false,
        );
        let library = stores
            .library
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(library) = library.as_ref() {
            let after = server
                .record_dismissed_through()
                .unwrap_or_else(|error| {
                    warn!(%error, "what stops the record could not be read: dismissed");
                    None
                })
                .unwrap_or(i64::MIN);
            match library.lost_scrobbles(after) {
                Ok(lost) => {
                    add(RecordProblemKind::ScrobblesUnknown, lost.unknown, true);
                    add(RecordProblemKind::ScrobblesWithdrawn, lost.withdrawn, true);
                }
                Err(error) => warn!(%error, "what stops the record could not be read: scrobbles"),
            }
        }
        problems.sort_by_key(|problem| problem.since);
        RecordStatus { problems }
    }

    /// `record.dismiss`: the scrobbles that ended unknown or withdrawn
    /// until now are set aside. Dismissing again changes nothing that
    /// matters, so the receipt is made and not stored.
    pub(super) fn record_dismiss(
        &self,
        stores: &Stores,
        params: &DismissParams,
    ) -> Result<Dismissed, RpcError> {
        stores
            .server()
            .dismiss_record_through(self.clock.now_ms())
            .map_err(crate::Error::Store)
            .map_err(super::failed)?;
        info!("the scrobbles that ended unknown or withdrawn so far are dismissed");
        Ok(Dismissed {
            receipt: Receipt {
                command_id: params.idempotency_key,
                status: ReceiptStatus::Done,
                revision: None,
            },
            record: self.record_status(stores),
        })
    }
}
