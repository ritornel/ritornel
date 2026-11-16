//! The calls that read lists through views, `media.token`, covers, and what
//! the server tells sessions after a library write. See "Large lists",
//! "Events and subscriptions" and "Media" in docs/design/contract.md.

use std::sync::{Arc, PoisonError, Weak};
use std::time::Instant;

use ritornel_contract::methods::{Method, SourcesAdd};
use ritornel_contract::{
    ErrorCode, Event, Id, LibraryChange, LibraryEvent, ListClosed, ListEvent, ListLocateParams,
    ListLocated, ListOpenParams, ListOpened, ListSections, ListWindow, ListWindowParams,
    MediaToken, Row, RpcError, ViewParams, Window,
};
use ritornel_engine::covers::{CoverError, Rendered};
use ritornel_engine::{Committed, LibraryStore, ListError, View};
use tracing::{debug, error};

use super::{Core, Stores, revision};
use crate::session::Session;
use crate::views::MAX_WINDOW_ROWS;

/// The sessions a library write is told to, without holding the server.
#[derive(Debug, Default)]
pub(crate) struct Fanout {
    sessions: std::sync::Mutex<std::collections::HashMap<u64, Weak<Session>>>,
}

impl Fanout {
    pub(crate) fn add(&self, session: &Arc<Session>) {
        self.lock().insert(session.id(), Arc::downgrade(session));
    }

    pub(crate) fn remove(&self, session: u64) {
        self.lock().remove(&session);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, std::collections::HashMap<u64, Weak<Session>>> {
        self.sessions.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Tells every session that the library changed: a `library` event to
    /// those subscribed, and a `list` event for each view each one holds.
    ///
    /// The rule for `reorder` is the coarse one of "Large lists" step 5:
    /// a change of play counts reorders only views that sort or filter on
    /// plays, and any other change reorders every view. No write changes
    /// play counts yet, and no view sorts on them, so every view is told to
    /// reorder.
    pub(crate) fn changed(&self, committed: Committed) {
        let revision = revision(committed.revision);
        let sessions: Vec<Arc<Session>> = self.lock().values().filter_map(Weak::upgrade).collect();
        let changed = changes(committed.method);
        for session in sessions {
            session.publish(Event::Library(LibraryEvent {
                library_revision: revision,
                changed: changed.clone(),
            }));
            for view in session.view_ids() {
                session.publish(Event::List(ListEvent {
                    view,
                    library_revision: revision,
                    reorder: true,
                }));
            }
        }
    }
}

/// What kind of change a write made, by the method that made it. Adding a
/// source adds a root and nothing in it. Any other write does not yet say,
/// so it is taken as every kind.
fn changes(method: &str) -> Vec<LibraryChange> {
    if method == SourcesAdd::NAME {
        vec![LibraryChange::Membership]
    } else {
        LibraryChange::ALL.to_vec()
    }
}

/// Tells `fanout` of every write to `library` from now on.
pub(crate) fn hook(library: &LibraryStore, fanout: &Arc<Fanout>) {
    let fanout = Arc::clone(fanout);
    library.on_commit(Arc::new(move |committed| fanout.changed(committed)));
}

fn expired(view: &str) -> RpcError {
    RpcError::new(ErrorCode::ViewExpired, "the view has ended: open it again")
        .with_detail("view", view.to_owned())
}

fn failed(error: impl std::fmt::Display) -> RpcError {
    RpcError::new(ErrorCode::Internal, error.to_string())
}

fn index(value: u64) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

impl Core {
    /// A new view's ID: unique in this run of the server, so that an ID a
    /// client kept from before a reconnection names nothing.
    fn new_view_id(&self) -> Result<String, RpcError> {
        let mut bytes = [0u8; 8];
        self.entropy.fill(&mut bytes).map_err(failed)?;
        Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
    }

    /// The rows of `resolved` in `window`, read now.
    fn window(
        library: &LibraryStore,
        resolved: &View,
        window: Window,
    ) -> Result<(u64, Vec<Row>), RpcError> {
        if window.limit > MAX_WINDOW_ROWS {
            return Err(RpcError::new(
                ErrorCode::Invalid,
                format!("a window is at most {MAX_WINDOW_ROWS} rows"),
            )
            .with_detail("maxWindowRows", MAX_WINDOW_ROWS));
        }
        let start = index(window.offset).min(resolved.ids.len());
        let end = start
            .saturating_add(index(window.limit))
            .min(resolved.ids.len());
        let keys = resolved.ids.get(start..end).unwrap_or_default();
        let started = Instant::now();
        let rows = library.list_rows(resolved.kind, keys).map_err(failed)?;
        debug!(
            kind = ?resolved.kind,
            offset = start,
            rows = rows.len(),
            ms = started.elapsed().as_secs_f64() * 1000.0,
            "read a window"
        );
        Ok((start as u64, rows))
    }

    pub(super) fn list_open(
        &self,
        stores: &Stores,
        session: &Session,
        params: ListOpenParams,
    ) -> Result<ListOpened, RpcError> {
        Self::library(stores, |library| {
            if let Some(replaces) = &params.replaces {
                session.close_view(&replaces.view);
            }
            let started = Instant::now();
            let resolved = library
                .list_open(params.kind, &params.query)
                .map_err(|error| match error {
                    ListError::Invalid(why) => RpcError::new(ErrorCode::Invalid, why),
                    ListError::Unsupported(why) => RpcError::new(ErrorCode::Unsupported, why),
                    ListError::Engine(error) => failed(error),
                })?;
            let opened_ms = started.elapsed().as_secs_f64() * 1000.0;
            let resolved = Arc::new(resolved);
            let view = self.new_view_id()?;
            session.hold_view(view.clone(), Arc::clone(&resolved));
            debug!(
                session = session.id(),
                kind = ?params.kind,
                rows = resolved.ids.len(),
                bytes = resolved.bytes(),
                session_view_bytes = session.view_bytes(),
                ms = opened_ms,
                "opened a view"
            );
            let anchor_index = match params.replaces.as_ref().and_then(|r| r.anchor.as_deref()) {
                Some(anchor) => library
                    .list_key(params.kind, anchor)
                    .map_err(failed)?
                    .and_then(|key| resolved.ids.iter().position(|id| *id == key))
                    .map(|position| position as u64),
                None => None,
            };
            let (offset, rows) = match params.window {
                Some(window) => Self::window(library, &resolved, window)?,
                None => (0, Vec::new()),
            };
            Ok(ListOpened {
                view,
                total: resolved.ids.len() as u64,
                library_revision: revision(resolved.revision),
                offset,
                rows,
                anchor_index,
            })
        })
    }

    pub(super) fn list_window(
        stores: &Stores,
        session: &Session,
        params: ListWindowParams,
    ) -> Result<ListWindow, RpcError> {
        let resolved = session
            .view(&params.view)
            .ok_or_else(|| expired(&params.view))?;
        Self::library(stores, |library| {
            let window = Window {
                offset: params.offset,
                limit: params.limit,
            };
            let (offset, rows) = Self::window(library, &resolved, window)?;
            Ok(ListWindow {
                view: params.view.clone(),
                offset,
                rows,
            })
        })
    }

    pub(super) fn list_sections(
        session: &Session,
        params: &ViewParams,
    ) -> Result<ListSections, RpcError> {
        let resolved = session
            .view(&params.view)
            .ok_or_else(|| expired(&params.view))?;
        Ok(ListSections {
            sections: resolved.sections.clone(),
        })
    }

    pub(super) fn list_locate(
        stores: &Stores,
        session: &Session,
        params: &ListLocateParams,
    ) -> Result<ListLocated, RpcError> {
        let resolved = session
            .view(&params.view)
            .ok_or_else(|| expired(&params.view))?;
        Self::library(stores, |library| {
            let index = match (&params.id, &params.prefix) {
                (Some(id), None) => library
                    .list_key(resolved.kind, id)
                    .map_err(failed)?
                    .and_then(|key| resolved.ids.iter().position(|row| *row == key))
                    .map(|position| position as u64),
                (None, Some(prefix)) => library
                    .list_locate_prefix(&resolved, prefix)
                    .map_err(failed)?,
                _ => {
                    return Err(RpcError::new(
                        ErrorCode::Invalid,
                        "list.locate takes an id or a prefix, and not both",
                    ));
                }
            };
            Ok(ListLocated { index })
        })
    }

    pub(super) fn list_close(session: &Session, params: &ViewParams) -> ListClosed {
        ListClosed {
            closed: session.close_view(&params.view),
        }
    }

    pub(super) fn media_token(&self, session: &Session) -> Result<MediaToken, RpcError> {
        let token = self
            .media_tokens
            .of(session.id(), &*self.entropy)
            .map_err(failed)?;
        Ok(MediaToken { token })
    }

    /// Whether a media token presented on a media route is one a session
    /// holds. It compares in memory and reads nothing.
    pub(crate) fn media_accepts(&self, token: &str) -> bool {
        self.media_tokens.accepts(token)
    }

    /// The cover `art` at `size`, if the cache has it. Reading the file is
    /// the caller's: this only says where it is.
    pub(crate) fn cover_path(&self, art: Id, size: u32) -> std::path::PathBuf {
        self.covers.path(art, size)
    }

    /// Makes the cover `art` at `size`. It reads the database, a file and
    /// works the processor, so it runs on a cover thread.
    pub(crate) fn render_cover(&self, art: Id, size: u32) -> Result<Rendered, CoverError> {
        // Another request may have made it while this one waited.
        if let Some(bytes) = self.covers.cached(art, size) {
            return Ok(Rendered {
                bytes,
                timings: Default::default(),
            });
        }
        let stores = self.stores.read().unwrap_or_else(PoisonError::into_inner);
        let Some(stores) = stores.as_ref() else {
            return Err(CoverError::Failed("the server is stopping".to_owned()));
        };
        let library = stores
            .library
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        let Some(library) = library.as_ref() else {
            return Err(CoverError::NotFound);
        };
        let rendered = self.covers.render(library, &stores.server, art, size);
        match &rendered {
            Ok(rendered) => debug!(
                art = %art,
                size,
                bytes = rendered.bytes.len(),
                lookup_us = rendered.timings.lookup_us,
                read_us = rendered.timings.read_us,
                decode_us = rendered.timings.decode_us,
                resize_us = rendered.timings.resize_us,
                store_us = rendered.timings.store_us,
                "made a cover"
            ),
            Err(CoverError::NotFound | CoverError::BadSize) => {}
            Err(failed) => error!(art = %art, size, error = %failed, "a cover could not be made"),
        }
        rendered
    }

    /// Submits work to the cover threads, newest first.
    pub(crate) fn submit_cover(
        &self,
        work: impl FnOnce() + Send + 'static,
    ) -> crate::media::CancelOnDrop {
        self.cover_pool.submit(work)
    }
}
