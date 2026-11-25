//! The settings that determine the encoded audio bytes.

use serde::{Deserialize, Serialize};
use std::io;

pub use crate::playback::renderer::ReplayGain;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Dsp {
    pub replay_gain: ReplayGain,
    pub max_rate_hz: Option<u32>,
}

impl Dsp {
    pub(super) fn validate(self) -> io::Result<()> {
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
#[serde(rename_all = "snake_case")]
pub enum Vbr {
    Variable,
    Constrained,
    HardCbr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mp3 {
    V0,
    V2,
    Cbr(u16),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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

// The contract describes a running profile to clients. The engine serializes
// its own profile for configuration and cache keys, independently of that wire format.
impl From<Profile> for ritornel_contract::transcode::Profile {
    fn from(profile: Profile) -> Self {
        use ritornel_contract::transcode as wire;
        let target = match profile.target {
            Target::Flac {} => wire::Target::Flac {},
            Target::Mp3 { mode } => wire::Target::Mp3 {
                mode: match mode {
                    Mp3::V0 => wire::Mp3::V0,
                    Mp3::V2 => wire::Mp3::V2,
                    Mp3::Cbr(kbps) => wire::Mp3::Cbr(kbps),
                },
            },
            Target::Opus {
                bitrate_kbps,
                complexity,
                vbr,
            } => wire::Target::Opus {
                bitrate_kbps,
                complexity,
                vbr: match vbr {
                    Vbr::Variable => wire::Vbr::Variable,
                    Vbr::Constrained => wire::Vbr::Constrained,
                    Vbr::HardCbr => wire::Vbr::HardCbr,
                },
            },
        };
        Self {
            target,
            dsp: wire::Dsp {
                max_rate_hz: profile.dsp.max_rate_hz,
                replay_gain: match profile.dsp.replay_gain {
                    ReplayGain::Off => wire::ReplayGain::Off,
                    ReplayGain::Album => wire::ReplayGain::Album,
                    ReplayGain::Track => wire::ReplayGain::Track,
                },
            },
        }
    }
}
