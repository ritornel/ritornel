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

/// What the record's sign knows only while the server runs.
#[derive(Debug, Default)]
pub(crate) struct Signs {
    /// A listen the player cannot save: since when, and why.
    unsaved: Option<(i64, String)>,
    /// Since when what would say whether the record advances has failed
    /// to read, while it does.
    unreadable_since: Option<i64>,
}

/// The service whose imports and scrobbles are looked at.
const SERVICE: &str = "lastfm";

/// What one source says: what is wrong, which is added to the problems,
/// or why it could not be read, which is added to `unread`.
fn add(
    problems: &mut Vec<RecordProblem>,
    unread: &mut Vec<String>,
    kind: RecordProblemKind,
    found: Result<Option<Failing>, String>,
    dismissable: bool,
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
            if !unread.contains(&error) {
                unread.push(error);
            }
        }
    }
}

/// One problem for all that could not be read: whether the record
/// advances is not known. None when everything was read.
fn unreadable(unread: &[String], since: i64) -> Option<RecordProblem> {
    (!unread.is_empty()).then(|| RecordProblem {
        kind: RecordProblemKind::Unreadable,
        since,
        count: 1,
        detail: Some(unread.join("; ")),
        dismissable: false,
    })
}

impl Core {
    /// The player saved what it had to, or had nothing to save.
    pub(crate) fn listen_saved(&self) {
        if lock(&self.signs).unsaved.take().is_some() {
            info!("the player saves its listens again");
        }
    }

    /// The player could not save the listen it is recording. It tries
    /// again at its next tick; until one works, the record has stopped.
    pub(crate) fn listen_unsaved(&self, detail: String) {
        let now_ms = self.clock.now_ms();
        let mut signs = lock(&self.signs);
        let since = signs.unsaved.as_ref().map_or(now_ms, |(since, _)| *since);
        signs.unsaved = Some((since, detail));
    }

    /// What stops the record now, oldest first.
    pub(super) fn record_status(&self, stores: &Stores) -> RecordStatus {
        let now_ms = self.clock.now_ms();
        let (mut problems, mut unread) = (Vec::new(), Vec::new());
        let unsaved = lock(&self.signs).unsaved.clone();
        add(
            &mut problems,
            &mut unread,
            RecordProblemKind::ListenUnsaved,
            Ok(unsaved.map(|(since_ms, detail)| Failing {
                since_ms,
                count: 1,
                detail: Some(detail),
            })),
            false,
        );
        let server = stores.server();
        let text = |error: ritornel_engine::Error| error.to_string();
        add(
            &mut problems,
            &mut unread,
            RecordProblemKind::ListensUndelivered,
            server.failing_deliveries().map_err(text),
            false,
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
            &mut unread,
            RecordProblemKind::ImportFailed,
            imports,
            false,
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
                        &mut unread,
                        RecordProblemKind::ScrobblesUnknown,
                        Ok(lost.unknown),
                        true,
                    );
                    add(
                        &mut problems,
                        &mut unread,
                        RecordProblemKind::ScrobblesWithdrawn,
                        Ok(lost.withdrawn),
                        true,
                    );
                }
                Err(error) => add(
                    &mut problems,
                    &mut unread,
                    RecordProblemKind::ScrobblesUnknown,
                    Err(error),
                    true,
                ),
            }
        }
        // Since when, held from the first read that failed until one works.
        let since = {
            let mut signs = lock(&self.signs);
            signs.unreadable_since = if unread.is_empty() {
                None
            } else {
                Some(signs.unreadable_since.unwrap_or(now_ms))
            };
            signs.unreadable_since.unwrap_or(now_ms)
        };
        problems.extend(unreadable(&unread, since));
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
        // With no library there is no scrobble to set aside, and the
        // dismissal changes nothing.
        let revision = stores
            .library
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map(|library| library.meta().map(|meta| meta.revision))
            .transpose()
            .map_err(crate::Error::Library)
            .map_err(super::failed)?;
        if let Some(revision) = revision {
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
        }
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
    fn what_cannot_be_read_is_said_once_and_never_taken_for_a_record_that_advances() {
        let (mut problems, mut unread) = (Vec::new(), Vec::new());
        add(
            &mut problems,
            &mut unread,
            RecordProblemKind::ImportFailed,
            Ok(None),
            false,
        );
        assert!(unreadable(&unread, 5).is_none());
        for kind in [
            RecordProblemKind::ListensUndelivered,
            RecordProblemKind::ImportFailed,
        ] {
            add(
                &mut problems,
                &mut unread,
                kind,
                Err("disk I/O error".to_owned()),
                false,
            );
        }
        assert!(problems.is_empty());
        assert_eq!(
            unreadable(&unread, 5),
            Some(RecordProblem {
                kind: RecordProblemKind::Unreadable,
                since: 5,
                count: 1,
                detail: Some("disk I/O error".to_owned()),
                dismissable: false,
            })
        );
    }
}
