//! `server.backup`, and what `server.status` says about backups. See
//! `crate::backup`.

use ritornel_contract::{
    BackedUp, BackupFailure, BackupMade, BackupParams, BackupReason, BackupStatus, ErrorCode,
    Receipt, ReceiptStatus, RpcError,
};
use ritornel_engine::{BackupFailed, BackupTaken, ServerStore};
use tracing::{info, warn};

use super::{Core, Stores};
use crate::backup::{About, BackupError, Backups, Taken};

/// Keeps in `server.db` what a backup did: one written, or one that
/// failed. Losing the record loses no backup, so a failure to keep it is
/// only logged.
pub(crate) fn record_backup(
    store: &ServerStore,
    at_ms: i64,
    taken: Option<&Taken>,
    failed: Option<&str>,
) {
    let kept = match (taken, failed) {
        (Some(taken), _) => store.backup_taken(&BackupTaken {
            at_ms: taken.at_ms,
            path: taken.path.to_string_lossy().into_owned(),
            reason: taken.reason.as_str().to_owned(),
        }),
        (None, Some(message)) => store.backup_failed(&BackupFailed {
            at_ms,
            message: message.to_owned(),
        }),
        (None, None) => Ok(()),
    };
    if let Err(error) = kept {
        warn!(%error, "what a backup did could not be recorded in server.db");
    }
}

impl Core {
    /// Says where this run's backups go. A second call keeps the first.
    pub(crate) fn set_backups(&self, backups: Backups) {
        let _ = self.backups.set(backups);
    }

    pub(super) fn backups(&self) -> Option<&Backups> {
        self.backups.get()
    }

    /// Backups, as `server.status` says them. Paths and OS failure text are
    /// omitted when `show_paths` is false (caller lacks `folders:manage`).
    pub(super) fn backup_status(&self, stores: &Stores, show_paths: bool) -> BackupStatus {
        let place = if show_paths {
            self.backups()
                .and_then(Backups::place)
                .map(|place| place.to_string_lossy().into_owned())
        } else {
            None
        };
        let (last, failed) = stores.server().backups().unwrap_or_else(|error| {
            warn!(%error, "what backups did could not be read from server.db");
            (None, None)
        });
        BackupStatus {
            place,
            last: last.map(|taken| BackupMade {
                at: taken.at_ms,
                present: std::path::Path::new(&taken.path).is_dir(),
                path: show_paths.then_some(taken.path),
                reason: BackupReason::from_wire(&taken.reason).unwrap_or(BackupReason::OnDemand),
            }),
            failed: failed.map(|failed| BackupFailure {
                at: failed.at_ms,
                message: show_paths.then_some(failed.message),
            }),
        }
    }

    /// `server.backup`: a backup now, while the server goes on playing.
    /// Taking one again takes another, so the receipt is made and not
    /// stored.
    pub(super) fn server_backup(
        &self,
        stores: &Stores,
        params: &BackupParams,
        show_paths: bool,
    ) -> Result<BackedUp, RpcError> {
        let backups = self
            .backups()
            .ok_or_else(|| refused(&BackupError::NoPlace))?;
        let library = stores
            .library
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map(|library| (library.library_id(), library.schema_version()));
        let about = About {
            server_id: Some(self.server_id),
            library_id: library.map(|(id, _)| id),
            server_schema: Some(self.schema_version),
            library_schema: library.map(|(_, schema)| schema),
        };
        let at_ms = self.clock.now_ms();
        match backups.take(&self.dir, at_ms, BackupReason::OnDemand, about) {
            Ok(taken) => {
                info!(path = %taken.path.display(), "backed up");
                record_backup(stores.server(), at_ms, Some(&taken), None);
                Ok(BackedUp {
                    receipt: Receipt {
                        command_id: params.idempotency_key,
                        status: ReceiptStatus::Done,
                        revision: None,
                    },
                    backup: BackupMade {
                        at: taken.at_ms,
                        path: show_paths.then(|| taken.path.to_string_lossy().into_owned()),
                        reason: taken.reason,
                        present: true,
                    },
                })
            }
            Err(error) => {
                warn!(%error, "a backup failed");
                // Asked for with no place, or while another is taken, is
                // not a backup that failed.
                if !matches!(error, BackupError::NoPlace | BackupError::Busy) {
                    let message = error.to_string();
                    record_backup(stores.server(), at_ms, None, Some(&message));
                }
                Err(if show_paths {
                    refused(&error)
                } else {
                    match error {
                        BackupError::NoPlace | BackupError::Busy => refused(&error),
                        _ => RpcError::new(ErrorCode::Unavailable, "backup failed"),
                    }
                })
            }
        }
    }
}

/// The error a client is given for a backup that could not be taken.
fn refused(error: &BackupError) -> RpcError {
    let (code, reason) = match error {
        BackupError::NoPlace => (ErrorCode::Invalid, "no_backup_place"),
        BackupError::Missing(_) => (ErrorCode::Unavailable, "backup_place_missing"),
        BackupError::Inside(_) => (ErrorCode::Unavailable, "backup_place_inside"),
        BackupError::Foreign { .. } => (ErrorCode::Unavailable, "backup_place_foreign"),
        BackupError::Unclear { .. } => (ErrorCode::Unavailable, "backup_place_unclear"),
        BackupError::Busy => (ErrorCode::Unavailable, "backup_running"),
        BackupError::Io { .. } | BackupError::Copy(..) => (ErrorCode::Unavailable, "backup_failed"),
    };
    RpcError::new(code, error.to_string()).with_detail("reason", reason)
}
