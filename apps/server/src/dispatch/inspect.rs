//! Read-only database inspection, provenance, conflicts, and revision log
//! handlers (`inspect.*` and `library.revisions`). See "Database inspection" in
//! docs/internals/contract.md.

use std::collections::HashMap;
use std::path::Path;
use std::sync::PoisonError;

use ritornel_contract::{
    ConflictsReport, DbInspection, EntityProvenance, ErrorCode, InspectConflictsParams, InspectDb,
    InspectEntityParams, InspectPlayParams, InspectTableParams, LibraryRevisionsPage,
    LibraryRevisionsParams, PlayProvenance, QueueDepths, RpcError, TablePage,
};
use ritornel_engine::{InspectTableError, LibraryQueueDepths};

use super::{Core, Stores};

fn db_file_sizes(path: &Path) -> Option<(u64, u64)> {
    let main = std::fs::metadata(path).ok()?.len();
    let mut wal_os = path.as_os_str().to_owned();
    wal_os.push("-wal");
    let wal = std::fs::metadata(Path::new(&wal_os))
        .map(|meta| meta.len())
        .unwrap_or(0);
    Some((main, wal))
}

fn map_inspect_error(error: InspectTableError) -> RpcError {
    match error {
        InspectTableError::Forbidden(message) => RpcError::new(ErrorCode::Forbidden, message),
        InspectTableError::NotFound(message) => RpcError::new(ErrorCode::NotFound, message),
        InspectTableError::Invalid(message) => RpcError::new(ErrorCode::Invalid, message),
        InspectTableError::Engine(error) => RpcError::new(ErrorCode::Internal, error.to_string()),
    }
}

fn no_library() -> RpcError {
    RpcError::new(ErrorCode::Invalid, "this server holds no library")
        .with_detail("reason", "no_library")
}

impl Core {
    /// `inspect.db`: schema, sizes, migrations, and queue depths.
    pub(super) fn inspect_db(&self, stores: &Stores) -> Result<DbInspection, RpcError> {
        let server_sizes = db_file_sizes(&self.dir.server_db());
        let server = stores
            .server
            .inspect_db(server_sizes)
            .map_err(|error| RpcError::new(ErrorCode::Internal, error.to_string()))?;
        let srv_queues = stores
            .server
            .queue_depths()
            .map_err(|error| RpcError::new(ErrorCode::Internal, error.to_string()))?;

        let library_guard = stores
            .library
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        let (library, library_revision, lib_queues) = match library_guard.as_ref() {
            Some(lib) => {
                let lib_sizes = db_file_sizes(&self.dir.library_db());
                let summary = lib
                    .inspect_db(lib_sizes)
                    .map_err(|error| RpcError::new(ErrorCode::Internal, error.to_string()))?;
                let queues = lib
                    .queue_depths()
                    .map_err(|error| RpcError::new(ErrorCode::Internal, error.to_string()))?;
                let rev = lib
                    .meta()
                    .map_err(|error| RpcError::new(ErrorCode::Internal, error.to_string()))?
                    .revision;
                let rev_u64 = u64::try_from(rev).unwrap_or(0);
                (Some(summary), Some(rev_u64), queues)
            }
            None => (None, None, LibraryQueueDepths::default()),
        };

        Ok(DbInspection {
            server,
            library,
            library_revision,
            queues: QueueDepths {
                outbox: srv_queues.outbox,
                listen_staging_open: srv_queues.listen_staging_open,
                listen_staging_closed: srv_queues.listen_staging_closed,
                history_delivery_pending: lib_queues.history_delivery_pending,
                history_delivery_refused: lib_queues.history_delivery_refused,
                history_delivery_unknown: lib_queues.history_delivery_unknown,
                history_ingest_conflicts: lib_queues.history_ingest_conflicts,
            },
        })
    }

    /// `inspect.table`: paged read-only view of an allowlisted table.
    pub(super) fn inspect_table(
        &self,
        stores: &Stores,
        params: &InspectTableParams,
        show_paths: bool,
    ) -> Result<TablePage, RpcError> {
        match params.db {
            InspectDb::Server => stores
                .server
                .inspect_table(params, show_paths)
                .map_err(map_inspect_error),
            InspectDb::Library => {
                let library = stores
                    .library
                    .read()
                    .unwrap_or_else(PoisonError::into_inner);
                let Some(lib) = library.as_ref() else {
                    if self.uplink().is_some() {
                        return Err(RpcError::new(
                            ErrorCode::Unsupported,
                            "the library database lives on the hub",
                        )
                        .with_detail("reason", "on_the_hub"));
                    }
                    return Err(no_library());
                };
                lib.inspect_table(params, show_paths)
                    .map_err(map_inspect_error)
            }
        }
    }

    /// `inspect.entity`: full provenance chain for an entity.
    pub(super) fn inspect_entity(
        &self,
        stores: &Stores,
        params: &InspectEntityParams,
        show_paths: bool,
    ) -> Result<EntityProvenance, RpcError> {
        let local_roots: Option<HashMap<String, String>> = if show_paths {
            let roots = stores
                .server
                .roots()
                .map_err(|error| RpcError::new(ErrorCode::Internal, error.to_string()))?;
            Some(roots.into_iter().collect())
        } else {
            None
        };
        let library = stores
            .library
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        let Some(lib) = library.as_ref() else {
            return Err(no_library());
        };
        lib.inspect_entity(params.kind, params.id, self.server_id, local_roots)
            .map_err(|error| RpcError::new(ErrorCode::Internal, error.to_string()))?
            .ok_or_else(|| RpcError::new(ErrorCode::NotFound, "entity not found"))
    }

    /// `inspect.play`: derived play with observations, deliveries, and corrections.
    pub(super) fn inspect_play(
        &self,
        stores: &Stores,
        params: &InspectPlayParams,
    ) -> Result<PlayProvenance, RpcError> {
        let library = stores
            .library
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        let Some(lib) = library.as_ref() else {
            return Err(no_library());
        };
        lib.inspect_play(params.play_id)
            .map_err(|error| RpcError::new(ErrorCode::Internal, error.to_string()))?
            .ok_or_else(|| RpcError::new(ErrorCode::NotFound, "play event not found"))
    }

    /// `inspect.conflicts`: paged ingest conflicts, stuck deliveries, and waiting outbox items.
    pub(super) fn inspect_conflicts(
        &self,
        stores: &Stores,
        params: &InspectConflictsParams,
        show_paths: bool,
    ) -> Result<ConflictsReport, RpcError> {
        let outbox = stores
            .server
            .inspect_outbox(params.offset, params.limit, show_paths)
            .map_err(map_inspect_error)?;
        let library = stores
            .library
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        let (ingest_conflicts_total, ingest_conflicts, stuck_deliveries_total, stuck_deliveries) =
            match library.as_ref() {
                Some(lib) => {
                    let lib_conflicts = lib
                        .inspect_conflicts(params.offset, params.limit)
                        .map_err(map_inspect_error)?;
                    (
                        lib_conflicts.ingest_conflicts_total,
                        lib_conflicts.ingest_conflicts,
                        lib_conflicts.stuck_deliveries_total,
                        lib_conflicts.stuck_deliveries,
                    )
                }
                None => (0, Vec::new(), 0, Vec::new()),
            };
        Ok(ConflictsReport {
            ingest_conflicts_total,
            ingest_conflicts,
            stuck_deliveries_total,
            stuck_deliveries,
            outbox_total: outbox.total,
            outbox: outbox.items,
        })
    }

    /// `library.revisions`: paged `library_revision` write log.
    pub(super) fn library_revisions(
        &self,
        stores: &Stores,
        params: &LibraryRevisionsParams,
    ) -> Result<LibraryRevisionsPage, RpcError> {
        let library = stores
            .library
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        let Some(lib) = library.as_ref() else {
            return Err(no_library());
        };
        lib.library_revisions(params.offset, params.limit)
            .map_err(map_inspect_error)
    }
}
