//! A target's format policy and settings, by value, and how one track is
//! sent under them. See "Format" in docs/internals/device-sync.md.
//!
//! The policy is one for each target, set by kind of source: a lossless
//! master goes as it is or encoded, and a lossy one always goes as it is,
//! since lossy is never encoded to lossy. It is stored as the encoder's
//! settings themselves and never a profile's name, so it means the same
//! after the hub moves.

use ritornel_contract::{
    SyncFolderKind, SyncFormat, SyncLossless, SyncMadeFor, SyncOnConnect, SyncPlaylistKind,
    SyncWhenFull, transcode as wire,
};
use ritornel_domain::selection::Candidate;
use serde::{Deserialize, Serialize};

use crate::Error;
use crate::transcode::{Codec, Dsp, Mp3, Profile, SourceChoice, Target, Vbr, choose_source};

/// What a lossless master is sent as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "as", rename_all = "snake_case")]
pub enum Lossless {
    /// As it is.
    AsIs,
    /// Encoded with these settings.
    Encode { target: Target },
}

/// A target's format policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormatPolicy {
    pub lossless: Lossless,
}

/// The policy as it is stored, with the version it was written in.
#[derive(Serialize, Deserialize)]
struct Stored {
    v: u32,
    #[serde(flatten)]
    policy: FormatPolicy,
}

impl Default for FormatPolicy {
    fn default() -> Self {
        Self {
            lossless: Lossless::AsIs,
        }
    }
}

impl FormatPolicy {
    /// The policy that encodes lossless masters to `target`.
    pub fn encode(target: Target) -> Self {
        Self {
            lossless: Lossless::Encode { target },
        }
    }

    /// The encoder profile a lossless master is made with: native FLAC for
    /// one sent as it is. A device's policy never bakes gain into the
    /// audio: it writes the gain tags and the player applies them.
    pub fn profile(self) -> Profile {
        Profile {
            target: match self.lossless {
                Lossless::AsIs => Target::Flac {},
                Lossless::Encode { target } => target,
            },
            dsp: Dsp::default(),
        }
    }

    /// Refuses settings no encoder takes, and FLAC as an encode: a FLAC
    /// master is sent as it is.
    pub fn validate(self) -> Result<(), Error> {
        if matches!(
            self.lossless,
            Lossless::Encode {
                target: Target::Flac {}
            }
        ) {
            return Err(Error::Invalid("FLAC is sent as it is, not encoded"));
        }
        self.profile()
            .validate()
            .map_err(|_| Error::Invalid("the format's encoder settings are not ones it takes"))
    }

    pub(crate) fn to_json(self) -> Result<String, Error> {
        serde_json::to_string(&Stored { v: 1, policy: self }).map_err(Error::Json)
    }

    pub(crate) fn from_json(json: &str) -> Result<Self, Error> {
        let stored: Stored = serde_json::from_str(json).map_err(Error::Json)?;
        if stored.v != 1 {
            return Err(Error::Corrupt("a format policy of a version not read"));
        }
        Ok(stored.policy)
    }
}

fn target_from_wire(target: wire::Target) -> Target {
    match target {
        wire::Target::Flac {} => Target::Flac {},
        wire::Target::Mp3 { mode } => Target::Mp3 {
            mode: match mode {
                wire::Mp3::V0 => Mp3::V0,
                wire::Mp3::V2 => Mp3::V2,
                wire::Mp3::Cbr(kbps) => Mp3::Cbr(kbps),
            },
        },
        wire::Target::Opus {
            bitrate_kbps,
            complexity,
            vbr,
        } => Target::Opus {
            bitrate_kbps,
            complexity,
            vbr: match vbr {
                wire::Vbr::Variable => Vbr::Variable,
                wire::Vbr::Constrained => Vbr::Constrained,
                wire::Vbr::HardCbr => Vbr::HardCbr,
            },
        },
    }
}

impl From<SyncFormat> for FormatPolicy {
    fn from(format: SyncFormat) -> Self {
        Self {
            lossless: match format.lossless {
                SyncLossless::AsIs => Lossless::AsIs,
                SyncLossless::Encode { target } => Lossless::Encode {
                    target: target_from_wire(target),
                },
            },
        }
    }
}

impl From<FormatPolicy> for SyncFormat {
    fn from(policy: FormatPolicy) -> Self {
        Self {
            lossless: match policy.lossless {
                Lossless::AsIs => SyncLossless::AsIs,
                Lossless::Encode { target } => SyncLossless::Encode {
                    target: wire::Profile::from(Profile {
                        target,
                        dsp: Dsp::default(),
                    })
                    .target,
                },
            },
        }
    }
}

/// A target's settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub format: FormatPolicy,
    /// The embedded cover's longest side in pixels. 0 embeds none.
    pub cover_px: u32,
    pub id3v1: bool,
    /// The folder under the storage root that music is written in.
    pub music_root: String,
    /// What is always kept free, or none for the default.
    pub reserve_bytes: Option<u64>,
    /// The capacity of a folder that is always here, or none for its
    /// volume's.
    pub limit_bytes: Option<u64>,
    /// For a folder: what it is made for, and whether it comes and goes.
    pub folder: Option<(SyncMadeFor, SyncFolderKind)>,
    /// The codecs the target is taken to play.
    pub codecs: Vec<Codec>,
    pub playlists: SyncPlaylistKind,
    pub when_full: SyncWhenFull,
    pub on_connect: SyncOnConnect,
    pub read_plays: bool,
    pub scrobble_plays: bool,
    pub remind_after_days: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            format: FormatPolicy::default(),
            cover_px: 500,
            id3v1: false,
            music_root: "Music".to_owned(),
            reserve_bytes: None,
            limit_bytes: None,
            folder: None,
            codecs: vec![Codec::Flac, Codec::Mp3, Codec::Opus],
            playlists: SyncPlaylistKind::M3u8,
            when_full: SyncWhenFull::Ask,
            on_connect: SyncOnConnect::Nothing,
            read_plays: true,
            scrobble_plays: true,
            remind_after_days: 10,
        }
    }
}

/// What a folder made for something starts with: the codecs it is taken
/// to play, the cover's size, the kind of playlist, and whether an MP3
/// gets an ID3v1 trailer. Each can then be changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Preset {
    pub codecs: &'static [Codec],
    pub cover_px: u32,
    pub playlists: SyncPlaylistKind,
    pub id3v1: bool,
}

/// The preset of a purpose.
pub fn preset(made_for: SyncMadeFor) -> Preset {
    match made_for {
        // A head unit reads MP3 everywhere and little else, shows a small
        // cover if any, and some only read the old trailer.
        SyncMadeFor::CarStereo => Preset {
            codecs: &[Codec::Mp3],
            cover_px: 300,
            playlists: SyncPlaylistKind::M3u,
            id3v1: true,
        },
        SyncMadeFor::Phone | SyncMadeFor::PlainCopy => Preset {
            codecs: &[Codec::Flac, Codec::Mp3, Codec::Opus],
            cover_px: 500,
            playlists: SyncPlaylistKind::M3u8,
            id3v1: false,
        },
        SyncMadeFor::OtherPlayer => Preset {
            codecs: &[Codec::Flac, Codec::Mp3],
            cover_px: 500,
            playlists: SyncPlaylistKind::M3u,
            id3v1: false,
        },
    }
}

impl Settings {
    /// The format a target that plays `codecs` starts with, and falls back
    /// to when it no longer plays the one it had: lossless as it is where
    /// it plays FLAC, and otherwise the first encode it plays.
    pub fn format_for(codecs: &[Codec]) -> FormatPolicy {
        if codecs.contains(&Codec::Flac) {
            FormatPolicy::default()
        } else if codecs.contains(&Codec::Mp3) {
            FormatPolicy::encode(Target::Mp3 { mode: Mp3::V0 })
        } else {
            FormatPolicy::encode(Target::Opus {
                bitrate_kbps: 160,
                complexity: 10,
                vbr: Vbr::Variable,
            })
        }
    }

    /// Whether the target plays what its format makes of a lossless master.
    pub fn plays_its_format(&self) -> bool {
        let made = match self.format.profile().target {
            Target::Flac {} => Codec::Flac,
            Target::Mp3 { .. } => Codec::Mp3,
            Target::Opus { .. } => Codec::Opus,
        };
        self.codecs.contains(&made)
    }

    /// The capacity the person set, which only a folder that is always
    /// here has: a removable one's room is its drive's.
    pub fn limit(&self) -> Option<u64> {
        self.limit_bytes
            .filter(|_| !matches!(self.folder, Some((_, SyncFolderKind::Removable))))
    }

    /// What is kept free of a capacity: the setting, or the larger of 1%
    /// and 256 MB, and never more than half the capacity. A limit the
    /// person set is their own figure for the room, so nothing more is
    /// kept back from it.
    pub fn reserve(&self, capacity: u64) -> u64 {
        const DEFAULT: u64 = 256 * 1024 * 1024;
        let default = if self.limit().is_some() {
            0
        } else {
            (capacity / 100).max(DEFAULT).min(capacity / 2)
        };
        self.reserve_bytes.unwrap_or(default).min(capacity)
    }
}

/// How one track's file is made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sent {
    /// The master's own audio, tagged afresh.
    AsIs(Codec),
    /// Encoded from the master.
    Encode(Codec, Profile),
}

impl Sent {
    pub fn codec(self) -> Codec {
        match self {
            Self::AsIs(codec) | Self::Encode(codec, _) => codec,
        }
    }
}

/// Why a wanted track cannot be sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Blocked {
    /// No copy of it can be made into anything the target plays.
    NoFormat,
    /// The hub has no encoder for the format.
    NoEncoder,
    /// Its files are not where the last scan found them.
    MasterUnavailable,
}

impl Blocked {
    pub const fn reason(self) -> &'static str {
        match self {
            Self::NoFormat => "no_format",
            Self::NoEncoder => "no_encoder",
            Self::MasterUnavailable => "master_unavailable",
        }
    }
}

/// Chooses the copy a track is sent from and how, by the source policy: a
/// lossless master is encoded or passed through, a lossy one is passed
/// through, and otherwise the track is blocked. A lossy master passes only
/// where the target plays its codec and the tag writer knows its
/// container. Returns the chosen copy's source item.
pub(crate) fn choose(
    copies: &[(Candidate, &str)],
    policy: FormatPolicy,
    plays: &[&str],
    encoder_for: impl Fn(Codec) -> bool,
) -> Result<(i64, Sent), Blocked> {
    if copies.iter().all(|(copy, _)| copy.missing) {
        return Err(Blocked::MasterUnavailable);
    }
    let profile = policy.profile();
    let accepts = |codec: &str| plays.contains(&codec) && Codec::named(codec).is_some();
    let codec_of = |chosen: &Candidate| {
        copies
            .iter()
            .find(|(copy, _)| copy.source_item == chosen.source_item)
            .and_then(|(_, codec)| Codec::named(codec))
    };
    match choose_source(copies, profile, accepts, false) {
        Ok(SourceChoice::PassThrough(copy)) => {
            let codec = codec_of(copy).ok_or(Blocked::NoFormat)?;
            if !plays.contains(&codec.extension()) {
                return Err(Blocked::NoFormat);
            }
            Ok((copy.source_item, Sent::AsIs(codec)))
        }
        Ok(SourceChoice::Encode(copy)) => {
            let codec = match profile.target {
                Target::Mp3 { .. } => Codec::Mp3,
                Target::Opus { .. } => Codec::Opus,
                Target::Flac {} => return Err(Blocked::NoFormat),
            };
            if !plays.contains(&codec.extension()) {
                return Err(Blocked::NoFormat);
            }
            if !encoder_for(codec) {
                return Err(Blocked::NoEncoder);
            }
            Ok((copy.source_item, Sent::Encode(codec, profile)))
        }
        Ok(SourceChoice::Blocked) | Err(_) => Err(Blocked::NoFormat),
    }
}

/// What an encode of `duration_ms` comes to at the format's nominal
/// bitrate, before any is made: the bitrate for constant-rate MP3 and for
/// Opus, and about 245 and 190 kbps for LAME's V0 and V2.
pub fn nominal_bytes(profile: Profile, duration_ms: u64) -> u64 {
    let kbps: u64 = match profile.target {
        Target::Mp3 { mode: Mp3::V0 } => 245,
        Target::Mp3 { mode: Mp3::V2 } => 190,
        Target::Mp3 {
            mode: Mp3::Cbr(kbps),
        } => u64::from(kbps),
        Target::Opus { bitrate_kbps, .. } => u64::from(bitrate_kbps),
        Target::Flac {} => 900,
    };
    kbps.saturating_mul(duration_ms) / 8
}

/// What the tags and the cover add to a file, near enough for an estimate.
pub fn tag_overhead(cover_px: u32, has_cover: bool) -> u64 {
    let cover = if has_cover {
        u64::from(cover_px).pow(2) / 6
    } else {
        0
    };
    4096 + cover
}

#[cfg(test)]
mod tests {
    use super::*;

    fn copy(item: i64, lossless: bool, missing: bool) -> Candidate {
        Candidate {
            source_item: item,
            asset: item,
            lossless,
            sample_rate_hz: 44_100,
            bit_depth: lossless.then_some(16),
            bitrate_kbps: (!lossless).then_some(256),
            missing,
        }
    }

    const ALL: &[&str] = &["flac", "mp3", "opus"];

    #[test]
    fn lossless_goes_as_it_is_or_encoded_and_lossy_always_as_it_is() {
        let v0 = FormatPolicy::encode(Target::Mp3 { mode: Mp3::V0 });
        let flac = [(copy(1, true, false), "flac")];
        assert_eq!(
            choose(&flac, FormatPolicy::default(), ALL, |_| true),
            Ok((1, Sent::AsIs(Codec::Flac)))
        );
        assert_eq!(
            choose(&flac, v0, ALL, |_| true),
            Ok((1, Sent::Encode(Codec::Mp3, v0.profile())))
        );
        // Lossy is never encoded to lossy, under either policy.
        let mp3 = [(copy(2, false, false), "mp3")];
        for policy in [FormatPolicy::default(), v0] {
            assert_eq!(
                choose(&mp3, policy, ALL, |_| true),
                Ok((2, Sent::AsIs(Codec::Mp3)))
            );
        }
    }

    #[test]
    fn a_track_with_nothing_the_target_plays_is_blocked_and_says_why() {
        let v0 = FormatPolicy::encode(Target::Mp3 { mode: Mp3::V0 });
        let flac = [(copy(1, true, false), "flac")];
        assert_eq!(
            choose(&flac, FormatPolicy::default(), &["mp3"], |_| true),
            Err(Blocked::NoFormat)
        );
        assert_eq!(choose(&flac, v0, ALL, |_| false), Err(Blocked::NoEncoder));
        // A lossy master in a container the tag writer does not know.
        let aac = [(copy(2, false, false), "aac")];
        assert_eq!(choose(&aac, v0, ALL, |_| true), Err(Blocked::NoFormat));
        let gone = [(copy(3, true, true), "flac")];
        assert_eq!(
            choose(&gone, v0, ALL, |_| true),
            Err(Blocked::MasterUnavailable)
        );
    }

    #[test]
    fn a_policy_is_stored_by_value_with_its_version() {
        let policy = FormatPolicy::encode(Target::Mp3 {
            mode: Mp3::Cbr(192),
        });
        let json = policy.to_json().unwrap();
        assert_eq!(
            json,
            r#"{"v":1,"lossless":{"as":"encode","target":{"codec":"mp3","mode":{"cbr":192}}}}"#
        );
        assert_eq!(FormatPolicy::from_json(&json).unwrap(), policy);
        assert!(FormatPolicy::encode(Target::Flac {}).validate().is_err());
    }
}
