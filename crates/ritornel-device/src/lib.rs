//! What a device family implements, so that the hub can sync part of a
//! library to it: a folder, a mass-storage player such as an iPod running
//! Rockbox, or an MTP player such as a Zune. See docs/internals/device-sync.md.
//!
//! The hub plans, makes every file and keeps every record. A family only
//! touches the hardware: a [`Session`] can probe, list, stat a named object,
//! hash a small part of one, put an object from a reader with a declared
//! length and digest, remove an object it is shown the signature of, keep a
//! small marker, and close with or without ejecting. No verb exposes a USB
//! command, an MTP handle or a path on the host.
//!
//! The types are shaped for both ways a player is reached:
//!
//! - **Mass storage and folders** name a file by its path under the
//!   storage root ([`ObjectId::Path`]). Its size is its signature for
//!   planning, and the first and last mebibyte are hashed before anything
//!   destructive.
//! - **MTP** names a file by the device's persistent object ID
//!   ([`ObjectId::Persistent`]). Where a device gives none, or no way to
//!   check bytes ([`Capabilities::can_sample`]), nothing destructive is
//!   done.
//!
//! This crate depends on no other crate of the workspace, as
//! `ritornel-source` does not. The trait is not stable until a second
//! transport has used it.

// Tests may unwrap and index. Everything else is held to the workspace's
// lints, which CI denies.
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

mod path;

use std::fmt;
use std::io::Read;

pub use path::{RelPath, UnsafePath};

/// How a target's storage is reached, which decides what names a file on
/// it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Transport {
    /// A directory on a server's disk, or on a card in a reader.
    Folder,
    /// A mounted volume, as a Rockbox player is.
    MassStorage,
    /// The Media Transfer Protocol, as a Zune speaks.
    Mtp,
}

impl Transport {
    /// The transport as records name it.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Folder => "folder",
            Self::MassStorage => "mass_storage",
            Self::Mtp => "mtp",
        }
    }
}

/// What names one file on a device, for its transport.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ObjectId {
    /// Its path under the storage root: folders and mass storage.
    Path(RelPath),
    /// The device's own persistent ID for it, which survives a rename and a
    /// reconnect: MTP.
    Persistent(String),
}

impl ObjectId {
    /// The path, where the transport names files by one.
    pub fn path(&self) -> Option<&RelPath> {
        match self {
            Self::Path(path) => Some(path),
            Self::Persistent(_) => None,
        }
    }
}

impl fmt::Display for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Path(path) => f.write_str(path.as_str()),
            Self::Persistent(id) => write!(f, "object {id}"),
        }
    }
}

/// What a file's bytes are known by without reading them all: its size,
/// and the BLAKE3 hash of its first and last mebibyte (the whole of a file
/// of two mebibytes or less). It guards against accident and not against
/// every change: a file altered only in its middle, at the same length,
/// passes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Signature {
    /// The file's length in bytes.
    pub size: u64,
    /// The sampled hash.
    pub sampled: [u8; 32],
}

/// One file as a listing gives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectInfo {
    /// What names it.
    pub id: ObjectId,
    /// Where it is shown to be: its path under the storage root, which an
    /// MTP device reports beside its ID.
    pub path: RelPath,
    /// Its length in bytes.
    pub size: u64,
    /// When it was last changed, as the device says, in Unix milliseconds.
    /// Recorded and never compared: FAT keeps local time.
    pub modified_ms: Option<i64>,
}

/// What plays a device gives back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plays {
    /// None, as a folder.
    None,
    /// A log of dated plays, as Rockbox writes.
    Log,
    /// A count for each track with no times, as a Zune keeps.
    Counters,
}

/// What a device can do, for the hub to plan by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capabilities {
    /// How it is reached.
    pub transport: Transport,
    /// What plays it gives back.
    pub plays: Plays,
    /// Whether closing with an eject makes it safe to unplug.
    pub ejectable: bool,
    /// Whether part of a file can be hashed without fetching all of it.
    /// Without it nothing destructive is done.
    pub can_sample: bool,
    /// The codecs it plays, as the scan names them: `flac`, `mp3`, `opus`.
    pub codecs: Vec<&'static str>,
    /// The cover sizes it is offered, longest side in pixels, the default
    /// first.
    pub cover_sizes: Vec<u32>,
    /// The longest path under the storage root it takes, in UTF-8 bytes.
    pub path_bytes: usize,
    /// How fast it is written before any sync has measured it, in bytes a
    /// second.
    pub write_bps: u64,
}

/// What a probe read of a device. Nothing is written to read it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Probe {
    /// The model, such as `iPod Video`.
    pub model: Option<String>,
    /// Its firmware, such as `Rockbox 4.0`.
    pub firmware: Option<String>,
    /// Its file system, such as `FAT32`.
    pub filesystem: Option<String>,
    /// The drawing that suits this model, as a short lowercase word path
    /// such as `player.ipod_video.white`, where the family knows one.
    pub icon: Option<String>,
    /// Whether it logs its plays: `Some(false)` is a player losing every
    /// play until its logging is turned on.
    pub logs_plays: Option<bool>,
    /// The device's clock at the probe, in Unix milliseconds, where it
    /// shows one.
    pub clock_ms: Option<i64>,
    /// What else the person should know, each a word a client has text
    /// for, such as `database_manual` or `sidecars_present`.
    pub notes: Vec<String>,
}

/// The room on a device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Storage {
    /// The whole volume, in bytes.
    pub capacity_bytes: u64,
    /// What is free.
    pub free_bytes: u64,
    /// The size files are rounded up to, where it is known.
    pub cluster_bytes: Option<u64>,
}

/// One object to write.
#[derive(Debug, Clone)]
pub struct Put<'a> {
    /// Where it goes, under the storage root. An MTP family makes its
    /// folders from it and answers the ID the device gave.
    pub path: &'a RelPath,
    /// The exact length of the bytes that follow.
    pub len: u64,
    /// BLAKE3 of those bytes. A put whose bytes do not hash to it leaves
    /// nothing behind.
    pub digest: [u8; 32],
    /// For a replace, the object it takes the place of and the signature
    /// that object must still have. Without it the put is an add, which
    /// never overwrites: an occupied name is [`ErrorKind::Occupied`].
    pub replaces: Option<(&'a ObjectId, Signature)>,
}

/// A play log as read: its name on the device and its bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayLog {
    /// The log's name, such as `.rockbox/playback.log`.
    pub name: String,
    /// Its bytes, whole.
    pub bytes: Vec<u8>,
}

/// Why a verb failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// There is nothing by that name.
    NotFound,
    /// An add found its name taken. The file there is left.
    Occupied,
    /// The object is not the one the signature named. Nothing was touched.
    Changed,
    /// The bytes written did not match their declared length or digest.
    Corrupt,
    /// The device is gone: unplugged, unmounted, or its folder removed.
    Gone,
    /// There is no room for it.
    Full,
    /// The path leaves the storage root, or passes through a link.
    Unsafe,
    /// The family cannot do this.
    Unsupported,
    /// The caller stopped it part way. Nothing partial is left.
    Stopped,
    /// Anything else the device or the system refused.
    Io,
}

/// A failed verb: what kind, and the detail. The detail never holds a path
/// on the host, only one under the storage root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    /// What kind of failure.
    pub kind: ErrorKind,
    /// What happened, in words.
    pub detail: String,
}

impl Error {
    /// An error of `kind` that says `detail`.
    pub fn new(kind: ErrorKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }

    /// A word for the kind, as an action's error code.
    pub const fn code(&self) -> &'static str {
        match self.kind {
            ErrorKind::NotFound => "not_found",
            ErrorKind::Occupied => "path_taken",
            ErrorKind::Changed => "changed_on_device",
            ErrorKind::Corrupt => "write_corrupt",
            ErrorKind::Gone => "device_gone",
            ErrorKind::Full => "device_full",
            ErrorKind::Unsafe => "unsafe_path",
            ErrorKind::Unsupported => "unsupported",
            ErrorKind::Stopped => "stopped",
            ErrorKind::Io => "write_failed",
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.detail)
    }
}

impl std::error::Error for Error {}

/// One open device. A session is the only thing that touches it, and every
/// verb is relative to its storage root.
///
/// Ritornel writes inside the music root it is given and its own
/// `.ritornel/` folder, and nowhere else. A family reads at most the rest
/// of the volume.
pub trait Session: Send + fmt::Debug {
    /// What the device can do. It does not change while the session is
    /// open.
    fn capabilities(&self) -> Capabilities;

    /// Reads what the device says of itself. Nothing is written.
    fn probe(&mut self) -> Result<Probe, Error>;

    /// The room on it now.
    fn storage(&mut self) -> Result<Storage, Error>;

    /// Every file under `music_root`, in no particular order. Links are
    /// neither followed nor listed.
    fn inventory(&mut self, music_root: &RelPath) -> Result<Vec<ObjectInfo>, Error>;

    /// One object by name: `None` when nothing is there.
    fn stat(&mut self, id: &ObjectId) -> Result<Option<ObjectInfo>, Error>;

    /// The signature of one object, read from the device: `None` when
    /// nothing is there. A family that cannot sample
    /// ([`Capabilities::can_sample`]) answers [`ErrorKind::Unsupported`].
    fn signature(&mut self, id: &ObjectId) -> Result<Option<Signature>, Error>;

    /// Writes one object from `bytes`, which holds exactly `put.len` bytes
    /// hashing to `put.digest`. It is written under a temporary name,
    /// checked, flushed and only then given its own name, so a put that
    /// fails or is stopped leaves nothing by that name. `sent` is told the
    /// bytes written so far and answers whether to go on.
    fn put(
        &mut self,
        put: &Put<'_>,
        bytes: &mut dyn Read,
        sent: &mut dyn FnMut(u64) -> bool,
    ) -> Result<ObjectInfo, Error>;

    /// Removes one object, only after finding it with the signature given:
    /// anything else is [`ErrorKind::Changed`] and deletes nothing. An
    /// object already gone is `Ok(false)`.
    fn remove(&mut self, id: &ObjectId, expect: Signature) -> Result<bool, Error>;

    /// The marker that makes this storage Ritornel's, as the hub wrote it:
    /// `None` when there is none.
    fn marker(&mut self) -> Result<Option<Vec<u8>>, Error>;

    /// Writes the marker, or with `None` removes it.
    fn set_marker(&mut self, marker: Option<&[u8]>) -> Result<(), Error>;

    /// Clears what a put cut short left under temporary names.
    fn clear_partials(&mut self) -> Result<(), Error>;

    /// The device's play logs, whole. A device with none answers nothing.
    fn play_logs(&mut self) -> Result<Vec<PlayLog>, Error> {
        Ok(Vec::new())
    }

    /// Flushes and closes. With `eject`, it also makes the device safe to
    /// unplug, and answers only once that has succeeded.
    fn close(self: Box<Self>, eject: bool) -> Result<(), Error>;
}
