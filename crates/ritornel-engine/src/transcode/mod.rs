//! File decoding and encoder backends. See "Transcoding" in docs/internals/playback.md.

mod cache;
mod cli;
mod package;
pub use cache::{CacheEntry, Derivative, Pin, PinOwner, TranscodeCache};
pub use package::{Artwork, Codec, Package, PackageSource, Tags};
mod profile;
mod selection;

use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

pub use crate::playback::audio::decode::{GainTag, GainTags, OutputSpec as PcmSpec};
use crate::playback::{
    FileFacts,
    audio::decode::{self, Opened, Source},
};
pub use cli::{LameEncoder, OpusEncoder};
pub use profile::{Dsp, Mp3, Profile, ReplayGain, Target, Vbr};
pub use selection::{SourceChoice, choose_source};

/// One master held open read-only, ready to stream once. Sample blocks are
/// interleaved in source channel order.
#[derive(Debug)]
pub struct Pcm {
    source: Source,
    opened: Opened,
}

impl Pcm {
    pub fn open(path: &Path, facts: Option<FileFacts>) -> io::Result<Self> {
        let opened = Opened::open(path, facts).map_err(decode_error)?;
        let mut source = Source::of(&opened, 0).map_err(decode_error)?;
        source.discover_channels().map_err(decode_error)?;
        Ok(Self { source, opened })
    }

    pub fn spec(&self, dsp: Dsp) -> io::Result<PcmSpec> {
        dsp.validate()?;
        let (rate, channels) = self.source.spec();
        if channels == 0 {
            return Err(io::Error::other("unknown source channel count"));
        }
        Ok(PcmSpec {
            rate: rate.min(dsp.max_rate_hz.unwrap_or(rate)),
            channels,
        })
    }

    /// Gain metadata as read from this master, before applying the profile.
    pub fn gain_tags(&self) -> GainTags {
        self.source.tags
    }

    pub fn stream(
        mut self,
        dsp: Dsp,
        cancel: &AtomicBool,
        sink: &mut dyn FnMut(&[f32]) -> io::Result<()>,
    ) -> io::Result<()> {
        let spec = self.spec(dsp)?;
        self.source.set_replay_gain(dsp.replay_gain);
        let mut written = Ok(());
        let decoded = decode::run(self.source, spec, cancel, &mut |samples| {
            if written.is_ok() {
                written = sink(samples);
            }
            written.is_ok()
        });
        check_cancel(cancel)?;
        written?;
        decoded.map_err(decode_error)?;
        self.opened.unchanged().map_err(decode_error)
    }
}

fn decode_error(error: crate::playback::RenderError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}

fn check_cancel(cancel: &AtomicBool) -> io::Result<()> {
    if cancel.load(Ordering::Relaxed) {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "encoding cancelled",
        ))
    } else {
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncoderId {
    pub family: &'static str,
    pub version: String,
}

/// Scheme and digest recorded for the audio packets by the scanner.
#[derive(Debug, Clone, Copy)]
pub struct AudioAsset {
    pub scheme: u32,
    pub hash: blake3::Hash,
}

impl EncoderId {
    /// Inputs are length-delimited by JSON. Bump the epoch when changing DSP,
    /// sample packing or adapter defaults in ways that alter encoded audio.
    pub fn derivative_key(
        &self,
        asset: AudioAsset,
        profile: Profile,
        tags: GainTags,
    ) -> io::Result<blake3::Hash> {
        profile.validate()?;
        const EPOCH: u32 = 1;
        let gain = tags.selected(profile.dsp.replay_gain);
        let bytes = serde_json::to_vec(&(
            "ritornel-derivative-v1",
            asset.scheme,
            asset.hash.to_hex().as_str(),
            self.family,
            &self.version,
            EPOCH,
            profile,
            gain.gain_db.map(f32::to_bits),
            gain.peak.map(f32::to_bits),
        ))?;
        Ok(blake3::hash(&bytes))
    }
}

/// An encoder consumes one PCM stream, publishing only a complete new file.
/// Missing optional programs return Ok(None) at probe time.
pub trait Encoder: Send + Sync {
    fn probe(&self) -> io::Result<Option<EncoderId>>;
    fn supports(&self, profile: Profile) -> bool;
    /// Encode one file using its derivative key for reproducible container framing.
    fn encode(
        &self,
        pcm: Pcm,
        profile: Profile,
        key: blake3::Hash,
        output: &Path,
        cancel: &AtomicBool,
    ) -> io::Result<()>;
}

/// A deterministic backend for callers exercising the pipeline without codecs.
#[derive(Debug)]
pub struct FakeEncoder;

impl Encoder for FakeEncoder {
    fn probe(&self) -> io::Result<Option<EncoderId>> {
        Ok(Some(EncoderId {
            family: "fake",
            version: "1".into(),
        }))
    }

    fn supports(&self, profile: Profile) -> bool {
        profile.validate().is_ok() && !matches!(profile.target, Target::Flac {})
    }

    fn encode(
        &self,
        pcm: Pcm,
        profile: Profile,
        _key: blake3::Hash,
        output: &Path,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        if !self.supports(profile) {
            return Err(io::Error::other("unsupported fake encoder profile"));
        }
        let spec = pcm.spec(profile.dsp)?;
        let mut hash = blake3::Hasher::new();
        hash.update(profile.canonical_json()?.as_bytes());
        hash.update(&spec.rate.to_le_bytes());
        hash.update(&(spec.channels as u64).to_le_bytes());
        pcm.stream(profile.dsp, cancel, &mut |samples| {
            for s in samples {
                hash.update(&s.to_le_bytes());
            }
            Ok(())
        })?;
        let mut stage = staged(output)?;
        stage.write_all(hash.finalize().as_bytes())?;
        publish(stage, output, cancel)
    }
}

fn staged(output: &Path) -> io::Result<tempfile::NamedTempFile> {
    tempfile::NamedTempFile::new_in(
        output
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new(".")),
    )
}

fn publish(stage: tempfile::NamedTempFile, output: &Path, cancel: &AtomicBool) -> io::Result<()> {
    check_cancel(cancel)?;
    stage.persist_noclobber(output)?;
    Ok(())
}

#[cfg(test)]
mod tests;
