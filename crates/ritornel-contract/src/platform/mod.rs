//! Everything that differs from one system to another, behind one set of
//! plain functions. The rest of the contract never names a system: there is
//! no `cfg` of an operating system outside this module.
//!
//! Each system has a file of its own (`mac.rs`, `windows.rs`), and each
//! offers the same names. What a system has no code of its own for, it
//! takes from `fallback`, the XDG base directories. A system with no file
//! at all (Linux) takes the fallback whole.

#[cfg(target_os = "macos")]
mod mac;
#[cfg(target_os = "macos")]
use mac as system;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as system;

#[cfg(not(any(target_os = "macos", windows)))]
use fallback as system;

pub(crate) use system::default_data_dir;

/// The plain behaviour, for what a system has no code of its own for.
#[allow(dead_code, reason = "each system takes only what it lacks")]
mod fallback {
    use std::path::PathBuf;

    use crate::local::{Env, set};

    /// `$XDG_DATA_HOME/ritornel`, which is `~/.local/share/ritornel` unless
    /// the user moved it. It is the data directory of the XDG base
    /// directory specification and not the state one: `server.db` holds
    /// this server's identity and pairings, and the hub's `library.db` sits
    /// beside it. The specification says to ignore a relative
    /// `XDG_DATA_HOME`.
    pub(crate) fn default_data_dir(env: Env<'_>) -> Option<PathBuf> {
        set(env, "XDG_DATA_HOME")
            .filter(|base| base.is_absolute())
            .or_else(|| Some(set(env, "HOME")?.join(".local/share")))
            .map(|base| base.join("ritornel"))
    }

    // The paths are Unix's: elsewhere `/srv/data` is not absolute.
    #[cfg(all(test, unix))]
    mod tests {
        use std::path::Path;

        use super::*;
        use crate::local::tests::env;

        #[test]
        fn the_default_follows_the_xdg_data_directory() {
            assert_eq!(
                default_data_dir(&env(&[("HOME", "/home/pat")])).unwrap(),
                Path::new("/home/pat/.local/share/ritornel")
            );
            assert_eq!(
                default_data_dir(&env(&[
                    ("HOME", "/home/pat"),
                    ("XDG_DATA_HOME", "/srv/data")
                ]))
                .unwrap(),
                Path::new("/srv/data/ritornel")
            );
            // A relative XDG_DATA_HOME is ignored, as the specification says.
            assert_eq!(
                default_data_dir(&env(&[("HOME", "/home/pat"), ("XDG_DATA_HOME", "data")]))
                    .unwrap(),
                Path::new("/home/pat/.local/share/ritornel")
            );
            assert_eq!(default_data_dir(&env(&[])), None);
        }
    }
}
