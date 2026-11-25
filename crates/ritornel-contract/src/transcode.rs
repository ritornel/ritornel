//! The settings that determine the encoded audio bytes.

use serde::{Deserialize, Serialize};
use std::io;

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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(default, deny_unknown_fields)]
pub struct Dsp {
    pub replay_gain: ReplayGain,
    pub max_rate_hz: Option<u32>,
}

impl Dsp {
    pub fn validate(self) -> io::Result<()> {
        if self.max_rate_hz.is_some_and(|hz| {
            !matches!(
                hz,
                8000 | 11025
                    | 12000
                    | 16000
                    | 22050
                    | 24000
                    | 32000
                    | 44100
                    | 48000
                    | 88200
                    | 96000
                    | 176400
                    | 192000
            )
        }) {
            return Err(io::Error::other("unsupported sample-rate ceiling"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum Vbr {
    Variable,
    Constrained,
    HardCbr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum Mp3 {
    V0,
    V2,
    Cbr(u16),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(tag = "codec", rename_all = "snake_case", deny_unknown_fields)]
pub enum Target {
    Mp3 {
        mode: Mp3,
    },
    Opus {
        bitrate_kbps: u16,
        complexity: u8,
        vbr: Vbr,
    },
    Flac {},
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub target: Target,
    #[serde(default)]
    pub dsp: Dsp,
}

impl Profile {
    pub fn validate(self) -> io::Result<()> {
        self.dsp.validate()?;
        match self.target {
            Target::Mp3 {
                mode: Mp3::Cbr(kbps),
            } if !matches!(
                kbps,
                32 | 40 | 48 | 56 | 64 | 80 | 96 | 112 | 128 | 160 | 192 | 224 | 256 | 320
            ) =>
            {
                Err(io::Error::other("unsupported MP3 CBR bitrate"))
            }
            Target::Opus {
                bitrate_kbps,
                complexity,
                ..
            } if !(6..=256).contains(&bitrate_kbps) || complexity > 10 => Err(io::Error::other(
                "Opus requires 6–256 kbps and complexity 0–10",
            )),
            Target::Flac {} if self.dsp != Dsp::default() => Err(io::Error::other(
                "FLAC pass-through requires unchanged audio",
            )),
            _ => Ok(()),
        }
    }

    /// A normalized representation with defaults present and stable field order.
    pub fn canonical_json(self) -> io::Result<String> {
        self.validate()?;
        Ok(serde_json::to_string(&self)?)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct EncoderCapability {
    pub family: String,
    pub version: String,
    pub formats: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct TranscodeStatus {
    pub encoders: Vec<EncoderCapability>,
    pub profiles: crate::Setting<std::collections::BTreeMap<String, Profile>>,
    pub workers: crate::Setting<u32>,
    pub budget_bytes: crate::Setting<u64>,
    pub used_bytes: u64,
    pub derivatives: u64,
    pub packages: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "lowercase")]
pub enum ArtifactLayer {
    Derivative,
    Packaged,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TranscodeParams {
    pub idempotency_key: crate::Id,
    pub appearances: Vec<crate::Id>,
    pub profile: String,
    pub layer: ArtifactLayer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TranscodePruneParams {
    pub idempotency_key: crate::Id,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct TranscodeStarted {
    #[serde(flatten)]
    pub receipt: crate::Receipt,
    pub job: crate::Job,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct TranscodeArtifact {
    pub appearance_id: crate::Id,
    pub layer: ArtifactLayer,
    pub key: String,
    pub size_bytes: u64,
    pub file_hash: String,
    pub sampled_hash: String,
}
