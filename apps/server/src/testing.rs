//! What the unit tests share.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use ritornel_engine::{OsEntropy, ServerStore, SystemClock};

use crate::Bounds;
use crate::data_dir::DataDir;
use crate::dispatch::{Core, Stores};

/// A core on a database in memory, and the directory it reports on.
pub(crate) fn core() -> (Core, Scratch) {
    let scratch = Scratch::new();
    let dir = DataDir::prepare(scratch.path().join("data")).unwrap();
    let store = ServerStore::open_in_memory(Arc::new(SystemClock), Arc::new(OsEntropy)).unwrap();
    let (identity, issued) = store.ensure_identity("desk").unwrap();
    let (_, verifier) = crate::credential::ensure(&store, &dir.credential(), issued).unwrap();
    let bounds = Bounds {
        max_sessions: 4,
        ..Bounds::default()
    };
    let core = Core::new(
        Stores::new(store, None),
        &identity,
        &dir,
        verifier,
        Arc::new(SystemClock),
        Arc::new(OsEntropy),
        bounds,
    )
    .unwrap();
    (core, scratch)
}

/// A directory that is removed when the test ends.
#[derive(Debug)]
pub(crate) struct Scratch(PathBuf);

impl Scratch {
    pub(crate) fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "ritornel-server-unit-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// An environment that holds exactly `variables`.
pub(crate) fn env(variables: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
    let variables: Vec<(String, OsString)> = variables
        .iter()
        .map(|(name, value)| ((*name).to_owned(), OsString::from(value)))
        .collect();
    move |wanted| {
        variables
            .iter()
            .find(|(name, _)| name == wanted)
            .map(|(_, value)| value.clone())
    }
}

/// A clock a test sets.
#[derive(Debug)]
pub(crate) struct SetClock(std::sync::atomic::AtomicI64);

impl SetClock {
    pub(crate) fn at(now_ms: i64) -> Self {
        Self(std::sync::atomic::AtomicI64::new(now_ms))
    }

    pub(crate) fn set(&self, now_ms: i64) {
        self.0.store(now_ms, Ordering::SeqCst);
    }
}

impl ritornel_engine::Clock for SetClock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}
