//! Encoder capabilities, reported profiles, encode jobs and download keys.

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
    /// A FLAC master with unchanged audio packets: nothing is encoded.
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
    /// Maximum number of simultaneous encodes.
    pub workers: crate::Setting<u32>,
    /// Maximum cache size in bytes, subject to active pins.
    pub budget_bytes: crate::Setting<u64>,
    /// Bytes occupied by cached encodes.
    pub used_bytes: u64,
    /// Number of cached encodes.
    pub derivatives: u64,
}

/// Warm the encode cache for a batch on the library hub.
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

/// What became of one requested appearance. A job answers one for each, in
/// request order, so one blocked or failed track never hides the others.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct TranscodeOutcome {
    /// Appearance requested by the caller.
    pub appearance_id: crate::Id,
    /// What was made of it, or why nothing was.
    #[serde(flatten)]
    pub result: TranscodeResult,
}

/// One appearance's result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum TranscodeResult {
    /// The encode is cached. The job retains the key after eviction, so a
    /// later download can return 404.
    #[serde(rename_all = "camelCase")]
    Encoded {
        /// BLAKE3 cache key, encoded as 64 hexadecimal digits.
        key: String,
        /// Exact length of the cached file.
        size_bytes: u64,
        /// BLAKE3 digest of the entire file.
        file_hash: String,
        /// BLAKE3 of the first and last MiB, counting overlapping bytes once.
        sampled_hash: String,
    },
    /// The master's own audio serves this profile, so nothing was encoded.
    PassedThrough {
        /// The master's codec, such as `flac` or `mp3`.
        codec: String,
    },
    /// The track has no copy this profile can be made from.
    Blocked {
        /// Why, as a word: `no_source`, `no_format` or `master_unavailable`.
        reason: String,
    },
    /// Making it failed.
    Failed {
        /// What went wrong.
        reason: String,
    },
}
