//! The views a session holds. See "Large lists" in docs/design/contract.md,
//! steps 10 and 11.
//!
//! A view is a resolved query: its row keys in order and its sections, at
//! about one integer a row. It belongs to its session and ends with it, on
//! `list.close`, when it has gone unused for [`VIEW_IDLE`], when the view it
//! was replaced by opens, or when the session opens more than [`MAX_VIEWS`]
//! and it is the one used least recently. A call on a view that has ended
//! fails with `view_expired`, and the client opens it again: the server
//! never guesses what it was.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ritornel_engine::View;

/// How many views a session may hold.
pub(crate) const MAX_VIEWS: usize = 16;

/// How long a view lasts unused.
pub(crate) const VIEW_IDLE: Duration = Duration::from_secs(10 * 60);

/// The most rows one call sends.
pub(crate) const MAX_WINDOW_ROWS: u64 = 500;

#[derive(Debug)]
struct Held {
    resolved: Arc<View>,
    used: Instant,
}

/// The views one session holds.
#[derive(Debug, Default)]
pub(crate) struct Views {
    held: HashMap<String, Held>,
}

impl Views {
    /// Holds a new view under `id`, and ends what must end to make room:
    /// views idle too long, then the one used least recently.
    pub(crate) fn hold(&mut self, id: String, resolved: Arc<View>, now: Instant) {
        self.sweep(now);
        while self.held.len() >= MAX_VIEWS {
            let oldest = self
                .held
                .iter()
                .min_by_key(|(_, held)| held.used)
                .map(|(id, _)| id.clone());
            match oldest {
                Some(oldest) => {
                    self.held.remove(&oldest);
                }
                None => break,
            }
        }
        self.held.insert(
            id,
            Held {
                resolved,
                used: now,
            },
        );
    }

    /// The view held under `id`, if it has not ended. Using it keeps it.
    pub(crate) fn get(&mut self, id: &str, now: Instant) -> Option<Arc<View>> {
        self.sweep(now);
        let held = self.held.get_mut(id)?;
        held.used = now;
        Some(Arc::clone(&held.resolved))
    }

    /// Ends the view held under `id`. Whether there was one.
    pub(crate) fn close(&mut self, id: &str) -> bool {
        self.held.remove(id).is_some()
    }

    /// Every view held, by its ID, without counting as a use of any.
    pub(crate) fn all(&self) -> Vec<(String, Arc<View>)> {
        self.held
            .iter()
            .map(|(id, held)| (id.clone(), Arc::clone(&held.resolved)))
            .collect()
    }

    /// How many bytes the views hold, near enough.
    pub(crate) fn bytes(&self) -> usize {
        self.held.values().map(|held| held.resolved.bytes()).sum()
    }

    fn sweep(&mut self, now: Instant) {
        self.held
            .retain(|_, held| now.saturating_duration_since(held.used) < VIEW_IDLE);
    }
}

#[cfg(test)]
mod tests {
    use ritornel_contract::{ListKind, SortKey};

    use super::*;

    fn view(rows: i64) -> Arc<View> {
        Arc::new(View {
            kind: ListKind::Artists,
            sort: SortKey::Name,
            revision: 1,
            ids: (0..rows).collect(),
            sections: Vec::new(),
        })
    }

    #[test]
    fn a_view_lasts_while_it_is_used_and_ends_when_it_is_idle_too_long() {
        let start = Instant::now();
        let mut views = Views::default();
        views.hold("a".to_owned(), view(3), start);
        assert!(views.get("a", start + VIEW_IDLE / 2).is_some());
        // Used half-way, so it lasts another whole stretch from then.
        assert!(views.get("a", start + VIEW_IDLE).is_some());
        assert!(views.get("a", start + VIEW_IDLE * 3).is_none());
    }

    #[test]
    fn one_view_too_many_ends_the_one_used_least_recently() {
        let start = Instant::now();
        let mut views = Views::default();
        for index in 0..MAX_VIEWS {
            let at = start + Duration::from_secs(index as u64);
            views.hold(index.to_string(), view(1), at);
        }
        // The first is used, so the second is now the oldest.
        let now = start + Duration::from_secs(100);
        assert!(views.get("0", now).is_some());
        views.hold("new".to_owned(), view(1), now);
        assert_eq!(views.all().len(), MAX_VIEWS);
        assert!(views.get("1", now).is_none());
        assert!(views.get("0", now).is_some());
        assert!(views.close("new"));
        assert!(!views.close("new"));
    }
}
