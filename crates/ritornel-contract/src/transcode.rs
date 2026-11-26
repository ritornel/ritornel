//! Encoder capabilities, reported profiles, materialization jobs and download keys.

use serde::{Deserialize, Serialize};

/// Which ReplayGain tag a track plays at. See "Audio pipeline" in
/// docs/internals/playback.md.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum ReplayGain {
    /// None: the samples as they are.
    #[default]
    Off,
    /// The track's gain.
    Track,
    /// The album's gain, or the track's where the album has none.
    Album,
}

/// Sample processing applied before encoding.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct Dsp {
    /// Gain tag applied to the samples.
    pub replay_gain: ReplayGain,
    /// Sample-rate ceiling in Hz, or null to use the encoder default.
    pub max_rate_hz: Option<u32>,
}

/// Opus rate control mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum Vbr {
    Variable,
    Constrained,
    HardCbr,
}

/// LAME quality preset or constant bitrate in kbps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum Mp3 {
    V0,
    V2,
    Cbr(u16),
}

/// Output codec and its encoder options.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(tag = "codec", rename_all = "snake_case")]
pub enum Target {
    Mp3 {
        /// Quality preset or constant bitrate.
        mode: Mp3,
    },
    Opus {
        /// Target bitrate in kbps.
        #[serde(rename = "bitrateKbps")]
        bitrate_kbps: u16,
        /// Encoder effort from 0 to 10.
        complexity: u8,
        /// Bitrate control mode.
        vbr: Vbr,
    },
    /// Package a FLAC master with unchanged audio packets.
    Flac {},
}

/// An effective profile as reported by the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct Profile {
    /// Output codec and encoder options.
    pub target: Target,
    /// Sample processing for this profile.
    #[serde(default)]
    pub dsp: Dsp,
}

/// An external encoder detected at server startup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct EncoderCapability {
    /// Encoder program name.
    pub family: String,
    /// Detected encoder build.
    pub version: String,
    /// Output formats supported by this encoder.
    pub formats: Vec<String>,
}

/// Effective encoder settings and the local cache index at the time of the call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct TranscodeStatus {
    /// Available external encoders. Native FLAC pass-through needs no program.
    pub encoders: Vec<EncoderCapability>,
    /// Built-ins overlaid by the winning profile table. The source identifies the overlay.
    pub profiles: crate::Setting<std::collections::BTreeMap<String, Profile>>,
    /// Maximum number of simultaneous encode or package tasks.
    pub workers: crate::Setting<u32>,
    /// Maximum cache size in bytes, subject to active pins.
    pub budget_bytes: crate::Setting<u64>,
    /// Bytes occupied by indexed files across both layers.
    pub used_bytes: u64,
    /// Number of encoded audio entries.
    pub derivatives: u64,
    /// Number of tagged artifact entries.
    pub packages: u64,
}

/// The cache layer that holds the requested file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "lowercase")]
pub enum ArtifactLayer {
    /// Encoded audio without catalog tags or cover art.
    Derivative,
    /// A file with the projected metadata and available artwork.
    Packaged,
}

/// Materialize a batch on the library hub.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct TranscodeParams {
    /// Client-generated command ID, reused on retry.
    pub idempotency_key: crate::Id,
    /// One to 128 track appearance IDs, in result order.
    pub appearances: Vec<crate::Id>,
    /// Name of the configured profile.
    pub profile: String,
    /// Which cache route serves the output.
    pub layer: ArtifactLayer,
}

/// Evict all currently unpinned entries on the hub.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct TranscodePruneParams {
    /// Client-generated command ID, reused on retry.
    pub idempotency_key: crate::Id,
}

/// Receipt and durable job for a materialization or prune request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct JobStarted {
    /// Receipt for the submitted command.
    #[serde(flatten)]
    pub receipt: crate::Receipt,
    /// The durable job to inspect with jobs.get.
    pub job: crate::Job,
}

/// One completed output. Its job retains the key after eviction, so a later download can return 404.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct TranscodeArtifact {
    /// Appearance requested by the caller.
    pub appearance_id: crate::Id,
    /// Which cache route serves the output.
    pub layer: ArtifactLayer,
    /// BLAKE3 cache key, encoded as 64 hexadecimal digits.
    pub key: String,
    /// Exact length of the published file.
    pub size_bytes: u64,
    /// BLAKE3 digest of the entire file.
    pub file_hash: String,
    /// BLAKE3 of the first and last MiB, counting overlapping bytes once.
    pub sampled_hash: String,
}
