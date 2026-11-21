//! Whether the listening record is advancing, for `server.status`, and
//! `record.dismiss`. Music can go on while the record stops: a disk that is
//! full fails a listen's save, a scrobble can end unanswered, an import can
//! fail. Each is said here, with since when, until it is put right. A
//! scrobble that ended unanswered is put right when an import brings it
//! back; one that does not come back, or that was never sent, is said
//! until it is dismissed. What cannot be read is said too, and never taken
//! for a record that advances.

use ritornel_contract::{
    DismissParams, Dismissed, Receipt, ReceiptStatus, RecordProblem, RecordProblemKind,
    RecordStatus, RpcError,
};
use ritornel_engine::{Dismissal, Failing};
use tracing::{info, warn};

use super::{Core, Stores, lock};

/// A listen the player could not save: since when, and what the last
/// attempt said.
#[derive(Debug, Default)]
pub(crate) struct Unsaved(Option<(i64, String)>);

/// The service whose imports and scrobbles are looked at.
const SERVICE: &str = "lastfm";

/// Adds what one source says to the problems: what is wrong, or that it
/// could not be read, as of `now_ms`.
fn add(
    problems: &mut Vec<RecordProblem>,
    kind: RecordProblemKind,
    found: Result<Option<Failing>, String>,
    dismissable: bool,
    now_ms: i64,
) {
    match found {
        Ok(Some(failing)) => problems.push(RecordProblem {
            kind,
            since: failing.since_ms,
            count: failing.count,
            detail: failing.detail,
            dismissable,
        }),
        Ok(None) => {}
        Err(error) => {
            warn!(%error, kind = kind.as_str(), "what stops the record could not be read");
            problems.push(RecordProblem {
                kind: RecordProblemKind::Unreadable,
                since: now_ms,
                count: 1,
                detail: Some(error),
                dismissable: false,
            });
        }
    }
}

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
        let now_ms = self.clock.now_ms();
        let mut problems = Vec::new();
        let unsaved = lock(&self.unsaved).0.clone();
        add(
            &mut problems,
            RecordProblemKind::ListenUnsaved,
            Ok(unsaved.map(|(since_ms, detail)| Failing {
                since_ms,
                count: 1,
                detail: Some(detail),
            })),
            false,
            now_ms,
        );
        let server = stores.server();
        let text = |error: ritornel_engine::Error| error.to_string();
        add(
            &mut problems,
            RecordProblemKind::ListensUndelivered,
            server.failing_deliveries().map_err(text),
            false,
            now_ms,
        );
        // The imports of the account signed in now: one signed out of is
        // not this server's to import.
        let imports = server
            .service_session(SERVICE)
            .map_err(text)
            .and_then(|session| {
                session.map_or(Ok(None), |session| {
                    server
                        .failing_jobs(
                            crate::lastfm::SYNC_JOB,
                            &session.account_name.to_lowercase(),
                        )
                        .map_err(text)
                })
            });
        add(
            &mut problems,
            RecordProblemKind::ImportFailed,
            imports,
            false,
            now_ms,
        );
        let library = stores
            .library
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(library) = library.as_ref() {
            let lost = server
                .record_dismissal()
                .map_err(text)
                .map(|dismissal| dismissal.map_or(i64::MIN, |held| held.through_revision))
                .and_then(|after| library.lost_scrobbles(after).map_err(text));
            match lost {
                Ok(lost) => {
                    add(
                        &mut problems,
                        RecordProblemKind::ScrobblesUnknown,
                        Ok(lost.unknown),
                        true,
                        now_ms,
                    );
                    add(
                        &mut problems,
                        RecordProblemKind::ScrobblesWithdrawn,
                        Ok(lost.withdrawn),
                        true,
                        now_ms,
                    );
                }
                Err(error) => add(
                    &mut problems,
                    RecordProblemKind::ScrobblesUnknown,
                    Err(error),
                    true,
                    now_ms,
                ),
            }
        }
        problems.sort_by_key(|problem| problem.since);
        RecordStatus { problems }
    }

    /// `record.dismiss`: the scrobbles that ended unknown or withdrawn by
    /// the library's revision now are set aside. The same command again
    /// changes nothing, so a retry hides nothing that ended since, and the
    /// receipt is made and not stored.
    pub(super) fn record_dismiss(
        &self,
        stores: &Stores,
        params: &DismissParams,
    ) -> Result<Dismissed, RpcError> {
        let revision = Self::library(stores, |library| {
            library
                .meta()
                .map(|meta| meta.revision)
                .map_err(crate::Error::Library)
                .map_err(super::failed)
        })?;
        stores
            .server()
            .dismiss_record(Dismissal {
                through_revision: revision,
                command: params.idempotency_key,
            })
            .map_err(crate::Error::Store)
            .map_err(super::failed)?;
        info!(
            revision,
            "the scrobbles that ended unknown or withdrawn so far are dismissed"
        );
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_cannot_be_read_is_said_and_never_taken_for_a_record_that_advances() {
        let mut problems = Vec::new();
        add(
            &mut problems,
            RecordProblemKind::ImportFailed,
            Ok(None),
            false,
            5,
        );
        assert!(problems.is_empty());
        add(
            &mut problems,
            RecordProblemKind::ScrobblesUnknown,
            Err("no such table: history_delivery".to_owned()),
            true,
            5,
        );
        assert_eq!(
            problems,
            [RecordProblem {
                kind: RecordProblemKind::Unreadable,
                since: 5,
                count: 1,
                detail: Some("no such table: history_delivery".to_owned()),
                dismissable: false,
            }]
        );
    }
}
