//! What the watching thread asks of the server: the folders to watch,
//! rescans and imports started on their own, and the status. See
//! `crate::watch`.

use std::path::PathBuf;

use ritornel_contract::{
    AutoTrigger, Id, Receipt, ReceiptStatus, RpcError, WatchStatus, WatchedFolder, WatchedKind,
    WatchingParams, WatchingSet,
};
use ritornel_engine::{Command, Fingerprint, JobState, NewJob, WriteRequest};
use serde::Serialize;
use tracing::{info, warn};

use super::{Core, ServerRole, Stores, lock};
use crate::watch::{Folder, Rescan, Signal, WatchRun, Watching};

/// The params an automatic scan's job holds: what started it, which is how
/// the status tells it from one asked for.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AutoScan {
    source_id: Id,
    trigger: AutoTrigger,
}

impl Core {
    /// Says how this run watches. A second call keeps the first.
    pub(crate) fn set_watch(&self, run: WatchRun) {
        let _ = self.watch.set(Watching::new(run));
    }

    pub(crate) fn watching(&self) -> Option<&Watching> {
        self.watch.get()
    }

    /// Wakes the watching thread to look at its folders again.
    pub(crate) fn wake_watch(&self) {
        if let Some(watching) = self.watching() {
            watching.send(Signal::Wake);
        }
    }

    /// The server's data directory.
    pub(crate) fn data_dir(&self) -> &std::path::Path {
        self.dir.root()
    }

    /// The time now, by the server's clock.
    pub(crate) fn now_ms(&self) -> i64 {
        self.clock.now_ms()
    }

    /// Turns watching on or off for this machine, and keeps it so. It is
    /// what a setting in the app calls, with the databases the call holds
    /// already: taking them again could wait behind the server stopping,
    /// which waits for the call.
    pub(crate) fn set_watching(&self, stores: &Stores, on: bool) -> Result<(), crate::Error> {
        stores.server().set_watching(on)?;
        info!(
            on,
            "watching the library's folders is turned {}",
            if on { "on" } else { "off" }
        );
        self.wake_watch();
        Ok(())
    }

    /// Why this server is not watching its folders now, if it is not. Only
    /// the hub watches: it is the library's one writer.
    pub(crate) fn watch_off(&self) -> Option<String> {
        self.with_stores(|stores| self.watch_off_in(stores))
            .unwrap_or_else(|| Some("the server is stopping".to_owned()))
    }

    fn watch_off_in(&self, stores: &Stores) -> Option<String> {
        if !self.watching().is_some_and(|watching| watching.run().on) {
            return Some("this run of the server does not watch".to_owned());
        }
        if lock(&self.this).role != ServerRole::Hub {
            return Some("this server is not the library's hub".to_owned());
        }
        match stores.server().watching() {
            Ok(true) => None,
            Ok(false) => Some("watching is turned off".to_owned()),
            Err(failed) => Some(failed.to_string()),
        }
    }

    /// The folders the hub watches: each folder source's root and each
    /// picture folder, where this server has a path for it.
    pub(crate) fn watched_folders(&self) -> Option<Vec<Folder>> {
        let found = self.with_library(|server, library| {
            let roots = server.roots()?;
            let path = |name: &str| {
                roots
                    .iter()
                    .find(|(root, _)| root == name)
                    .map(|(_, path)| PathBuf::from(path))
            };
            let mut folders = Vec::new();
            for source in library.sources()? {
                if source.kind == "folder"
                    && source.on_this_server
                    && let Some(path) = path(&source.name)
                {
                    folders.push(Folder {
                        name: source.name,
                        path,
                        source: Some(source.public_id),
                    });
                }
            }
            for name in library.picture_folders()? {
                if let Some(path) = path(&name) {
                    folders.push(Folder {
                        name,
                        path,
                        source: None,
                    });
                }
            }
            Ok::<_, ritornel_engine::Error>(folders)
        })?;
        match found {
            Ok(folders) => Some(folders),
            Err(failed) => {
                warn!(error = %failed, "the folders to watch could not be read");
                None
            }
        }
    }

    /// Queues a rescan of a root, unless one that will see changes made at
    /// `changed_ms` is queued or running already: a queued one will, and a
    /// running one will if it started after them.
    pub(crate) fn auto_scan(
        &self,
        root: &str,
        source: Id,
        trigger: AutoTrigger,
        changed_ms: Option<i64>,
    ) -> Rescan {
        self.with_stores(|stores| {
            match stores.server().active_job("scan", root) {
                Ok(Some(job)) => {
                    let sees = job.state == JobState::Queued
                        || job.state == JobState::Running
                            && changed_ms.is_none_or(|changed| {
                                job.started_at_ms.is_some_and(|started| started > changed)
                            });
                    return if sees { Rescan::Joined } else { Rescan::Busy };
                }
                Ok(None) => {}
                Err(failed) => return Rescan::Failed(failed.to_string()),
            }
            self.queue_scan(stores, root, source, trigger)
        })
        .unwrap_or_else(|| Rescan::Failed("the server is stopping".to_owned()))
    }

    /// Queues a rescan of a root whose last scan ended `after_ms` or more
    /// ago, or that was never scanned. A failed scan counts, so a root that
    /// is offline is not tried again until the timer says.
    pub(crate) fn scan_if_stale(
        &self,
        root: &str,
        source: Id,
        trigger: AutoTrigger,
        after_ms: i64,
    ) -> Rescan {
        let now_ms = self.now_ms();
        self.with_stores(|stores| {
            let server = stores.server();
            let last = match server.active_job("scan", root) {
                Ok(Some(_)) => return Rescan::Joined,
                Ok(None) => server.last_ended_job("scan", root),
                Err(failed) => Err(failed),
            };
            match last {
                Ok(last) => {
                    let ended = last.and_then(|job| job.finished_at_ms);
                    if ended.is_some_and(|ended| now_ms.saturating_sub(ended) < after_ms) {
                        return Rescan::NotDue;
                    }
                }
                Err(failed) => return Rescan::Failed(failed.to_string()),
            }
            self.queue_scan(stores, root, source, trigger)
        })
        .unwrap_or_else(|| Rescan::Failed("the server is stopping".to_owned()))
    }

    fn queue_scan(&self, stores: &Stores, root: &str, source: Id, trigger: AutoTrigger) -> Rescan {
        let id = match ritornel_engine::new_id(self.now_ms(), &*self.entropy) {
            Ok(id) => id,
            Err(failed) => return Rescan::Failed(failed.to_string()),
        };
        let params = serde_json::to_string(&AutoScan {
            source_id: source,
            trigger,
        })
        .unwrap_or_default();
        let command = Command {
            id,
            fingerprint: Fingerprint::of("scan.auto", params.as_bytes()),
        };
        let new = NewJob {
            kind: "scan",
            dedupe_key: root.to_owned(),
            target: source,
            params_json: params,
            // No one asked: the server started it.
            session: None,
        };
        match stores.server().start_job(command, "scan.auto", new) {
            Ok((job, created)) => {
                if created {
                    self.publish_job(&job);
                }
                self.jobs.wake();
                if created {
                    Rescan::Started(job.public_id)
                } else {
                    Rescan::Joined
                }
            }
            Err(failed) => Rescan::Failed(failed.to_string()),
        }
    }

    /// Imports a picture folder again. It writes nothing if nothing
    /// changed.
    pub(crate) fn auto_import(
        &self,
        folder: &Folder,
        trigger: AutoTrigger,
    ) -> ritornel_contract::AutoRun {
        let started_at = self.now_ms();
        let imported = self.with_library(|_, library| {
            let checked = ritornel_engine::check_picture_folder(&folder.path)?;
            let request = WriteRequest {
                method: "artists.refreshPictures",
                command: None,
                client: None,
            };
            library.refresh_artist_pictures(request, &folder.name, &checked)
        });
        let ok = match imported {
            Some(Ok(Some(counts))) => {
                info!(
                    folder = %folder.name,
                    trigger = trigger.as_str(),
                    by_mbid = counts.by_mbid,
                    by_name = counts.by_name,
                    dropped = counts.dropped,
                    "artist pictures are imported again on their own"
                );
                true
            }
            Some(Ok(None)) => true,
            Some(Err(failed)) => {
                warn!(folder = %folder.name, error = %failed, "a picture folder could not be imported again");
                false
            }
            None => false,
        };
        crate::watch::imported(trigger, started_at, self.now_ms(), ok)
    }

    /// What the status says of watching.
    pub(crate) fn watch_status(&self, stores: &Stores) -> WatchStatus {
        let (off, shown) = self.watching().map(Watching::shown).unwrap_or_else(|| {
            (
                Some("this run of the server does not watch".to_owned()),
                Vec::new(),
            )
        });
        // The thread says what it last found, and the role or the setting
        // may have changed since.
        let off = off.or_else(|| self.watch_off_in(stores));
        let folders = shown
            .into_iter()
            .map(|shown| {
                let last_auto = match shown.kind {
                    WatchedKind::Music => stores
                        .server()
                        .last_automatic_job("scan", &shown.name)
                        .ok()
                        .flatten()
                        .and_then(|job| crate::watch::auto_run(&job)),
                    WatchedKind::Pictures => shown.imported,
                };
                WatchedFolder {
                    name: shown.name,
                    kind: shown.kind,
                    state: shown.state,
                    detail: shown.detail,
                    pending_since: shown.pending_since,
                    last_auto,
                }
            })
            .collect();
        WatchStatus {
            on: off.is_none(),
            enabled: stores.server().watching().unwrap_or(true),
            off,
            folders,
        }
    }

    /// `library.setWatching`: the setting is kept in `server.db` and the
    /// watching thread told. Setting it again changes nothing, so the
    /// receipt is made and not stored, as `lastfm.setScrobbling`'s is.
    pub(super) fn library_set_watching(
        &self,
        stores: &Stores,
        params: &WatchingParams,
    ) -> Result<WatchingSet, RpcError> {
        self.set_watching(stores, params.on)
            .map_err(super::failed)?;
        Ok(WatchingSet {
            receipt: Receipt {
                command_id: params.idempotency_key,
                status: ReceiptStatus::Done,
                revision: None,
            },
            watch: self.watch_status(stores),
        })
    }
}
