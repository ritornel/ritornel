//! `history.export`: the record, written where a client names, in files a
//! program that knows nothing of Ritornel can read. See
//! `ritornel_engine::export`.

use std::path::Path;

use ritornel_contract::{
    ErrorCode, HistoryExportParams, HistoryExported, Receipt, ReceiptStatus, RpcError,
};
use ritornel_engine::EXPORT_VERSION;
use tracing::{info, warn};

use super::{Core, Stores};
use crate::backup::{publish, reserve_named, stamp};
use crate::platform::close_to_others;

impl Core {
    /// Writes the record into a new folder, `ritornel-record-<time>`, in
    /// the folder the params name, under a dotted name until it is whole.
    /// Exporting again writes another, so the receipt is made and not
    /// stored.
    pub(super) fn history_export(
        &self,
        stores: &Stores,
        params: &HistoryExportParams,
    ) -> Result<HistoryExported, RpcError> {
        let folder = Path::new(&params.folder);
        if !folder.is_absolute() {
            return Err(
                RpcError::new(ErrorCode::Invalid, "the folder must be an absolute path")
                    .with_detail("field", "folder"),
            );
        }
        // It is never made: a folder on a disk that is not connected
        // would be made on another.
        if !folder.is_dir() {
            return Err(RpcError::new(
                ErrorCode::Unavailable,
                format!("{} is not a folder that is there", folder.display()),
            )
            .with_detail("reason", "export_folder_missing"));
        }
        let at_ms = self.clock.now_ms();
        Self::library(stores, |library| {
            let unwritten = |error: String| {
                warn!(%error, "an export failed");
                RpcError::new(ErrorCode::Unavailable, error).with_detail("reason", "export_failed")
            };
            let (name, partial) =
                reserve_named(folder, &format!("ritornel-record-{}", stamp(at_ms)))
                    .map_err(|error| unwritten(error.to_string()))?;
            // A listening history is its owner's alone, as a backup is, and
            // stays so when its files are copied out of the folder.
            let exported = library
                .export_record(&partial, at_ms)
                .map_err(|error| format!("cannot export the record: {error}"))
                .and_then(|exported| {
                    let files = std::fs::read_dir(&partial).map_err(|error| error.to_string())?;
                    for file in files {
                        let path = file.map_err(|error| error.to_string())?.path();
                        close_to_others(&path)
                            .map_err(|error| format!("cannot close {}: {error}", path.display()))?;
                    }
                    Ok(exported)
                });
            let exported = match exported {
                Ok(exported) => exported,
                Err(error) => {
                    let _ = std::fs::remove_dir_all(&partial);
                    return Err(unwritten(error));
                }
            };
            let path =
                publish(&partial, folder, &name).map_err(|error| unwritten(error.to_string()))?;
            info!(path = %path.display(), observations = exported.observations, "exported the record");
            Ok(HistoryExported {
                receipt: Receipt {
                    command_id: params.idempotency_key,
                    status: ReceiptStatus::Done,
                    revision: None,
                },
                path: path.to_string_lossy().into_owned(),
                version: EXPORT_VERSION,
                observations: exported.observations,
                ratings: exported.ratings,
                plays: exported.plays,
            })
        })
    }
}
