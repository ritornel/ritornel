//! What reading one file gives: the payload that is stored verbatim as a
//! `source_item_revision`, and that claims are extracted from. See "Scanning
//! and audio hashing" in docs/internals/identity.md.
//!
//! The reader is in the engine, where the files are. The types are here
//! because the extractor, which is pure, reads them.

use serde::{Deserialize, Serialize};

/// The version of the payload's shape.
pub const PAYLOAD_VERSION: u32 = 1;

/// One file as it was read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Payload {
    /// [`PAYLOAD_VERSION`].
    pub v: u32,
    /// The path, relative to the root, with `/` between its parts.
    pub path: String,
    /// The whole file's size in bytes, tags included.
    pub size: u64,
    /// When the file was last modified, as Unix milliseconds.
    pub mtime_ms: i64,
    /// The container as the demuxer names it, when one was recognised.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
    /// The audio, when at least one packet was read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<Audio>,
    /// Every tag, as read and in the order read, understood or not.
    #[serde(default)]
    pub tags: Vec<Tag>,
    /// The pictures embedded in the file, by hash. The bytes stay in the
    /// file.
    #[serde(default)]
    pub pictures: Vec<Picture>,
    /// The folder's own images, such as `cover.jpg`, by hash.
    #[serde(default)]
    pub folder_images: Vec<FolderImage>,
    /// Why the file could not be read, or could be read only in part.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ReadError>,
}

/// The audio of a file: its hash and what the stream is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Audio {
    /// The scheme of `hash`. Scheme 1 is the audio-hash spike's.
    pub hash_scheme: u32,
    /// The audio hash, as 64 hex digits.
    pub hash: String,
    /// Our own name for the codec, which is part of the hash.
    pub codec: String,
    /// Samples a second, when the container says.
    pub sample_rate: Option<u32>,
    /// How many channels, when the container says.
    pub channels: Option<u32>,
    /// Bits per sample, for a codec that has them.
    pub bits: Option<u32>,
    /// The duration of the packets read, in milliseconds.
    pub duration_ms: u64,
    /// How many packets were read.
    pub packets: u64,
    /// How many bytes those packets held.
    pub payload_bytes: u64,
    /// The FLAC STREAMINFO MD5, when it is not zero. Evidence only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flac_md5: Option<String>,
}

impl Audio {
    /// Whether the codec loses nothing.
    pub fn lossless(&self) -> bool {
        self.codec == "flac" || self.codec == "alac" || self.codec.starts_with("pcm_")
    }
}

/// One tag as read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tag {
    /// The key as the file has it: a Vorbis comment's name, an ID3v2
    /// frame's ID, an MP4 atom's name.
    pub key: String,
    /// The block of tags it was read from, as the reader names it: `flac`
    /// or `vorbis` for Vorbis comments, `id3v2`, `id3v1`, `apev2`,
    /// `isomp4`. One file can hold several, and they can disagree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block: Option<String>,
    /// What tells one tag of this key from another, such as the
    /// description of a `TXXX` frame or the owner of a `UFID`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sub: Option<String>,
    /// The values. A tag with several holds them in order.
    pub values: Vec<String>,
    /// Set when the value was bytes and not text. A short one is in
    /// `values` as hex; a long one is left out and this is its length.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary_len: Option<u64>,
}

/// One picture embedded in a file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Picture {
    /// The BLAKE3 hash of its bytes, as hex.
    pub hash: String,
    /// Its media type, such as `image/jpeg`.
    pub mime: String,
    /// Its width in pixels, when the header could be read.
    pub width: Option<u32>,
    /// Its height in pixels.
    pub height: Option<u32>,
    /// Its size in bytes.
    pub size: u64,
    /// The tag format's own type number; 3 is a front cover.
    pub kind: Option<u32>,
}

/// One image file in a folder, recorded against each audio file there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderImage {
    /// The file's name, such as `cover.jpg`.
    pub name: String,
    /// When the image was last modified. With `picture.size` it is what
    /// lets a rescan skip an unchanged folder.
    pub mtime_ms: i64,
    /// The picture.
    #[serde(flatten)]
    pub picture: Picture,
}

/// Why a file could not be read as audio, or only in part.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadError {
    /// A short, fixed name for the kind of failure.
    pub kind: String,
    /// What the reader said.
    pub message: String,
}
