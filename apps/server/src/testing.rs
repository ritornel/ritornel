//! What the unit tests share.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

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

/// The permission bits of a file or directory.
pub(crate) fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
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
