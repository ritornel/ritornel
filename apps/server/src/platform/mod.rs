//! Everything that differs from one system to another, behind one set of
//! plain functions and values. The rest of the server never names a system:
//! there is no `cfg` of an operating system outside this module.
//!
//! Each system has a file of its own (`mac.rs`, `linux.rs`, `windows.rs`),
//! and each offers the same names:
//!
//! - files and folders only the OS user can open, a folder synced, and
//!   the free space beside them;
//! - the lock on `server.lock`, which a client tries as the contract says
//!   (`ritornel_contract::local`);
//! - what asks a running server to stop, and the main thread's wait for
//!   it ([`wait`]), which is a Mac's run loop;
//! - the background service: launchd's agent (`launchd.rs`) on a Mac and a
//!   systemd user unit (`systemd.rs`) on Linux, both built for the tests on
//!   every Unix, with what they share in `service.rs`;
//! - the media session: Now Playing on a Mac (`mac_now_playing.rs`) and
//!   MPRIS on Linux (`linux_mpris.rs`).
//!
//! A Mac and Linux are Unix, and take what they share from `unix.rs`. What
//! a system has no code of its own for, it takes from `fallback`: no
//! service, no media session, and a plain wait. Any other Unix takes
//! `unix.rs` and the fallback whole.

#[cfg(unix)]
mod unix;

#[cfg(any(target_os = "macos", all(test, unix)))]
mod launchd;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod service;
#[cfg(any(target_os = "linux", all(test, unix)))]
mod systemd;

#[cfg(target_os = "macos")]
mod mac;
#[cfg(all(target_os = "macos", feature = "media-session"))]
mod mac_now_playing;
#[cfg(target_os = "macos")]
use mac as system;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(all(target_os = "linux", feature = "media-session"))]
mod linux_mpris;
#[cfg(target_os = "linux")]
use linux as system;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as system;

#[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
use other_unix as system;

#[cfg(feature = "media-session")]
pub(crate) use system::{HAS_MEDIA_SESSION, MEDIA_SESSION_ON_RUNTIME, MediaControls};
pub(crate) use system::{
    close_to_others, create_new_private_dir, create_owner_only, create_private_dir,
    create_private_file, free_bytes, install_service, is_private, lock_shareable, service_status,
    sync_dir, take_lock, uninstall_service, wait, watch_signals,
};
#[cfg(test)]
pub(crate) use system::{open_to_others, path_is_private};

/// Any other Unix: what every Unix shares, and nothing of its own.
#[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
mod other_unix {
    #[cfg(feature = "media-session")]
    pub(crate) use super::fallback::{HAS_MEDIA_SESSION, MEDIA_SESSION_ON_RUNTIME, MediaControls};
    pub(crate) use super::fallback::{install_service, service_status, uninstall_service, wait};
    pub(crate) use super::unix::*;
}

/// The plain behaviour, for what a system has no code of its own for.
#[allow(dead_code, reason = "each system takes only what it lacks")]
mod fallback {
    use std::path::Path;
    use std::sync::mpsc::Receiver;

    use crate::{Env, Error};

    /// Waits until `stopped` gives a reason to stop. Only a Mac needs the
    /// main thread for its media session; elsewhere it simply waits.
    pub(crate) fn wait<T: Send + 'static>(stopped: Receiver<T>) -> Option<T> {
        stopped.recv().ok()
    }

    /// There is no background service yet. On Windows it is still to come:
    /// a task at sign-in, in the user's session, where the media keys and
    /// the default audio device are.
    fn no_service() -> Error {
        Error::Service(
            "it is built for macOS and Linux only so far; elsewhere run `ritornel-server serve` under \
             the system's service manager"
                .to_owned(),
        )
    }

    pub(crate) fn install_service(_: Option<&Path>, _: Env<'_>) -> Result<String, Error> {
        Err(no_service())
    }

    pub(crate) fn uninstall_service(_: Env<'_>) -> Result<String, Error> {
        Err(no_service())
    }

    pub(crate) fn service_status(_: Env<'_>) -> Result<String, Error> {
        Err(no_service())
    }

    /// There is no media session: no Now Playing and no media keys. On
    /// Windows the system's media transport controls, tied to the hidden
    /// window `serve` opens there, are still to come.
    #[cfg(feature = "media-session")]
    pub(crate) const HAS_MEDIA_SESSION: bool = false;
    #[cfg(feature = "media-session")]
    pub(crate) const MEDIA_SESSION_ON_RUNTIME: bool = false;

    /// Controls that are never opened.
    #[cfg(feature = "media-session")]
    #[derive(Debug)]
    pub(crate) enum MediaControls {}

    #[cfg(feature = "media-session")]
    impl MediaControls {
        pub(crate) fn open(
            _: crate::media_session::Keys,
            _: &tokio::runtime::Handle,
        ) -> Option<Self> {
            None
        }

        pub(crate) fn set_track(
            &mut self,
            _: &ritornel_engine::playback::PlayerState,
            _: Option<&str>,
        ) {
            match *self {}
        }

        pub(crate) fn set_state(&mut self, _: &ritornel_engine::playback::PlayerState) {
            match *self {}
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;
    use crate::testing::Scratch;

    #[test]
    fn a_directory_that_exists_has_room_and_one_that_does_not_has_no_answer() {
        let scratch = Scratch::new();
        assert!(free_bytes(scratch.path()).unwrap() > 0);
        assert_eq!(free_bytes(&scratch.path().join("missing")), None);
    }

    #[test]
    fn off_the_main_thread_waiting_is_only_waiting() {
        let (send, receive) = mpsc::channel();
        send.send(7).unwrap();
        assert_eq!(wait(receive), Some(7));
    }
}
