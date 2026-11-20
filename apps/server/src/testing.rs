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
#[cfg(unix)]
pub(crate) fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// Whether only the owner can open a file or directory: what the modes
/// `0600` and `0700` are on Unix.
pub(crate) fn private(path: &Path) -> bool {
    #[cfg(unix)]
    {
        mode(path) & 0o077 == 0
    }
    #[cfg(windows)]
    {
        ritornel_windows::private::path_is_private(path).unwrap()
    }
}

/// Opens a file to other users, as a copy that did not keep its mode or
/// access list might be.
pub(crate) fn open_to_others(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644)).unwrap();
    }
    #[cfg(windows)]
    {
        let done = std::process::Command::new("icacls")
            .arg(path)
            .args(["/grant", "*S-1-1-0:(R)"])
            .output()
            .unwrap();
        assert!(done.status.success(), "{done:?}");
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
