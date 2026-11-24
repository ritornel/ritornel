//! Everything that differs from one system to another, behind one set of
//! plain functions and values. The rest of the server never names a system:
//! there is no `cfg` of an operating system outside this module.
//!
//! Each system has a file of its own (`mac.rs`, `linux.rs`, `windows.rs`),
//! and each offers the same names:
//!
//! - files and folders only the OS user can open, a folder synced, a
//!   folder renamed (on Windows, asking for it to be written through to
//!   the disk), and the free space beside them;
//! - the lock on `server.lock`, which a client tries as the contract says
//!   (`ritornel_contract::local`);
//! - what asks a running server to stop, and the main thread's wait for
//!   it ([`wait`]), which is a Mac's run loop;
//! - the background service: launchd's agent (`launchd.rs`) on a Mac and a
//!   systemd user unit (`systemd.rs`) on Linux, both built for the tests on
//!   every Unix, with what they share in `service.rs`, and on Windows a
//!   task at sign-in and the supervisor it runs (`windows_service.rs`);
//! - the media session: Now Playing on a Mac (`mac_now_playing.rs`), MPRIS
//!   on Linux (`linux_mpris.rs`), and the system's media transport controls
//!   on Windows (`windows_media.rs`);
//! - the computer's own name, which a server takes by default, and keeping
//!   the machine from sleeping while satellites are linked to it
//!   ([`Awake`]).
//!
//! A Mac and Linux are Unix, and take what they share from `unix.rs`. What
//! a system has no code of its own for, it takes from `fallback`: no
//! service, no media session, and a plain wait. Any other Unix takes
//! `unix.rs` and the fallback whole.

#[cfg(unix)]
mod unix;

#[cfg(any(target_os = "macos", all(test, unix)))]
mod launchd;
#[cfg(any(target_os = "macos", target_os = "linux", windows, all(test, unix)))]
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
#[cfg(all(windows, feature = "media-session"))]
mod windows_media;
#[cfg(windows)]
mod windows_service;
#[cfg(windows)]
use windows as system;

#[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
use other_unix as system;

#[cfg(feature = "media-session")]
pub(crate) use system::{HAS_MEDIA_SESSION, MediaControls, STOP_MEDIA_SESSION_WITH_SERVER};
pub(crate) use system::{
    PrivateDir, check_dir_usable, close_backup_file, close_data_dir_to_others, close_dir_to_others,
    close_to_others, create_backup_file, create_new_private_dir, create_owner_only,
    create_private_dir, create_private_file, free_bytes, install_service, is_private,
    lock_shareable, open_lock_file, rename_dir, run_service, service_status, stay_awake, sync_dir,
    take_lock, uninstall_service, wait, watch_signals,
};

/// The computer's name, as the person who set it up named it: what a server
/// is called by default. `None` when it cannot be read.
pub(crate) fn computer_name() -> Option<String> {
    let name = system::computer_name()?;
    let name: String = name.chars().filter(|c| !c.is_control()).take(100).collect();
    let name = name.trim();
    (!name.is_empty()).then(|| name.to_owned())
}

/// The name an older build took from this computer, where it differs from
/// how the name is read now (`COMPUTERNAME` on Windows): a server still
/// carrying it takes the computer's name as it is read now.
pub(crate) fn legacy_computer_name() -> Option<String> {
    system::legacy_computer_name()
}

/// The name this machine answers to on the local network by multicast DNS
/// (Bonjour), such as `studio-mac.local`, when it answers to one: what an
/// invite offers first, so that a hub whose address changes is still
/// found.
pub(crate) fn mdns_name() -> Option<String> {
    local_name(&system::mdns_host()?)
}

/// `host` as a `.local` name, if it is a label multicast DNS answers to:
/// not `localhost`, which no responder announces.
fn local_name(host: &str) -> Option<String> {
    let host = host.trim().to_ascii_lowercase();
    let host = host.strip_suffix(".local").unwrap_or(&host);
    let valid = (1..=63).contains(&host.len())
        && host != "localhost"
        && !host.starts_with('-')
        && !host.ends_with('-')
        && host.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    valid.then(|| format!("{host}.local"))
}

/// The machine kept from sleeping when idle, its screen still free to, for
/// as long as this is held. It is a helper process the system already has,
/// asked to hold the system's own assertion; dropping this ends it, and so
/// does the server's ending, however it ends.
#[derive(Debug)]
pub(crate) struct Awake(std::process::Child);

impl Awake {
    /// Whether the helper still holds the assertion. One the system
    /// refused has ended.
    pub(crate) fn holds(&mut self) -> bool {
        matches!(self.0.try_wait(), Ok(None))
    }
}

impl Drop for Awake {
    fn drop(&mut self) {
        // Closing its input ends a helper that waits on it; the rest are
        // ended here. Either way it is waited for, so none is left behind.
        drop(self.0.stdin.take());
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
#[cfg(test)]
pub(crate) use system::{close_scratch, open_to_others, path_is_private};

/// Any other Unix: what every Unix shares, and nothing of its own.
#[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
mod other_unix {
    #[cfg(feature = "media-session")]
    pub(crate) use super::fallback::{
        HAS_MEDIA_SESSION, MediaControls, STOP_MEDIA_SESSION_WITH_SERVER,
    };
    pub(crate) use super::fallback::{
        computer_name, install_service, legacy_computer_name, mdns_host, run_service,
        service_status, stay_awake, uninstall_service, wait,
    };
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

    /// There is no background service here.
    fn no_service() -> Error {
        Error::Service(
            "it is built for macOS, Linux and Windows only so far; elsewhere run \
             `ritornel-server serve` under the system's service manager"
                .to_owned(),
        )
    }

    /// Only Windows runs a supervisor of its own: elsewhere the service
    /// manager starts the server again.
    pub(crate) fn run_service(_: &Path, _: Env<'_>) -> Result<(), Error> {
        Err(Error::Service(
            "`service run` is what the background service runs on Windows".to_owned(),
        ))
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

    /// The computer's name is not read here yet.
    pub(crate) fn computer_name() -> Option<String> {
        None
    }

    /// No older build named a server otherwise here.
    pub(crate) fn legacy_computer_name() -> Option<String> {
        None
    }

    /// Whether this machine answers to a name by multicast DNS is not
    /// known here yet.
    pub(crate) fn mdns_host() -> Option<String> {
        None
    }

    /// Keeping the machine awake is not built here yet.
    pub(crate) fn stay_awake(_why: &str) -> std::io::Result<super::Awake> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "keeping the machine awake is built for macOS and Linux only so far",
        ))
    }

    /// There is no media session: no Now Playing and no media keys.
    #[cfg(feature = "media-session")]
    pub(crate) const HAS_MEDIA_SESSION: bool = false;
    #[cfg(feature = "media-session")]
    pub(crate) const STOP_MEDIA_SESSION_WITH_SERVER: bool = false;

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
            _: Option<&std::path::Path>,
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
    fn a_local_name_is_one_label_multicast_dns_answers_to() {
        assert_eq!(
            local_name("Studio-Mac\n").as_deref(),
            Some("studio-mac.local")
        );
        assert_eq!(
            local_name("studio-mac.local").as_deref(),
            Some("studio-mac.local")
        );
        let long = "a".repeat(64);
        for not in [
            "",
            "localhost",
            "-studio",
            "studio-",
            "studio mac",
            "studio.lan",
            &long,
        ] {
            assert_eq!(local_name(not), None, "{not}");
        }
    }

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
