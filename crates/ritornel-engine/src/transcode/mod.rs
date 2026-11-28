//! File decoding, encoder backends, the encode cache and tagged copies. See
//! docs/internals/transcoding.md.

mod cache;
mod cli;
mod tagging;
pub use cache::{CacheEntry, Derivative, Pin, PinOwner, TranscodeCache, sampled_hash};
pub use tagging::{Artwork, Codec, CopySource, RITORNEL_ID, TaggedCopy, Tags};
mod pool;
pub use pool::{
    Batch, BatchItem, BatchProgress, Cover, Encoding, Master, PreparedTransfer, Tagging,
    TranscodePool, Transfer, TransferAudio, TransferRequest,
};

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

fn copy(
    reader: &mut impl io::Read,
    writer: &mut impl Write,
    cancel: &AtomicBool,
) -> io::Result<()> {
    use io::BufRead;
    let mut input = io::BufReader::with_capacity(64 * 1024, reader);
    loop {
        check_cancel(cancel)?;
        let block = match input.fill_buf() {
            Ok([]) => return Ok(()),
            Ok(block) => block,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        writer.write_all(block)?;
        let count = block.len();
        input.consume(count);
    }
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

/// What one track's file is made from, by the source policy.
#[derive(Debug, Clone)]
pub enum Sourced {
    /// A lossless master, to encode.
    Encode(Master),
    /// A master whose own audio serves: nothing is encoded.
    PassThrough(Master, String),
    /// No copy can be made into what was asked for, and why, as a word:
    /// `master_unavailable` where its files are not where the scan found
    /// them, `no_format` otherwise.
    Blocked(&'static str),
}

impl crate::LibraryStore {
    /// Chooses the copy of an appearance on `server` that `profile` is
    /// made from ([`choose_source`]): a lossless master is encoded, or
    /// passed through for native FLAC; a lossy one is passed through where
    /// `accepts` takes its codec, and is never encoded again.
    pub fn transcode_source(
        &self,
        appearance: crate::Id,
        server: crate::Id,
        roots: &std::collections::HashMap<String, std::path::PathBuf>,
        profile: Profile,
        accepts: impl Fn(&str) -> bool,
    ) -> Result<Sourced, crate::Error> {
        use ritornel_domain::selection::Candidate;
        let found: Vec<(Candidate, String, Master)> = self.read(|db| {
            let mut statement = db.prepare_cached(
                "SELECT si.id, aa.id, aa.lossless, aa.sample_rate_hz, aa.bit_depth,
                        aa.bitrate_kbps, si.missing_since_ms IS NOT NULL, aa.codec,
                        aa.hash_scheme, aa.audio_hash, lr.name, si.provider_key,
                        si.size_bytes, si.mtime_ms
                 FROM assignment a
                 JOIN source_item si ON si.id = a.source_item_id
                 JOIN source_item_revision sr ON sr.id = si.current_revision_id
                 JOIN audio_asset aa ON aa.id = sr.audio_asset_id
                 JOIN source_instance src ON src.id = si.source_instance_id
                 JOIN library_root lr ON lr.id = src.root_id
                 JOIN server s ON s.id = lr.server_id
                 WHERE a.superseded_at_ms IS NULL
                   AND si.retired_at_ms IS NULL
                   AND si.item_kind = 'file'
                   AND lr.kind = 'master'
                   AND s.public_id = ?2
                   AND a.appearance_id IN (
                     SELECT id FROM track_appearance WHERE canonical_id =
                       (SELECT canonical_id FROM track_appearance WHERE public_id = ?1))",
            )?;
            let rows = statement.query_map((appearance.as_bytes(), server.as_bytes()), |row| {
                let root: String = row.get(10)?;
                let rel_path: String = row.get(11)?;
                Ok((
                    Candidate {
                        source_item: row.get(0)?,
                        asset: row.get(1)?,
                        lossless: row.get(2)?,
                        sample_rate_hz: row.get(3)?,
                        bit_depth: row.get(4)?,
                        bitrate_kbps: row.get(5)?,
                        missing: row.get(6)?,
                    },
                    row.get::<_, String>(7)?,
                    (
                        AudioAsset {
                            scheme: row.get(8)?,
                            hash: row.get::<_, [u8; 32]>(9)?.into(),
                        },
                        root,
                        rel_path,
                        FileFacts {
                            size_bytes: row
                                .get::<_, Option<i64>>(12)?
                                .and_then(|size| u64::try_from(size).ok())
                                .unwrap_or(u64::MAX),
                            mtime_ms: row.get::<_, Option<i64>>(13)?.unwrap_or(i64::MIN),
                        },
                    ),
                ))
            })?;
            let mut found = Vec::new();
            for row in rows {
                let (candidate, codec, (asset, root, rel_path, facts)) = row?;
                // A copy in a root this server has no folder for cannot be opened.
                let Some(folder) = roots.get(&root) else {
                    continue;
                };
                found.push((
                    candidate,
                    codec,
                    Master {
                        path: crate::walk::under(folder, &rel_path),
                        asset,
                        facts,
                    },
                ));
            }
            Ok(found)
        })?;
        let copies: Vec<(Candidate, &str)> = found
            .iter()
            .map(|(candidate, codec, _)| (candidate.clone(), codec.as_str()))
            .collect();
        let master = |chosen: &Candidate| {
            found
                .iter()
                .find(|(candidate, ..)| candidate.source_item == chosen.source_item)
                .map(|(_, codec, master)| (master.clone(), codec.clone()))
        };
        let blocked = if copies.iter().all(|(copy, _)| copy.missing) {
            "master_unavailable"
        } else {
            "no_format"
        };
        Ok(
            match choose_source(&copies, profile, accepts, false).map_err(crate::Error::Io)? {
                SourceChoice::Encode(copy) => match master(copy) {
                    Some((master, _)) => Sourced::Encode(master),
                    None => Sourced::Blocked(blocked),
                },
                SourceChoice::PassThrough(copy) => match master(copy) {
                    Some((master, codec)) => Sourced::PassThrough(master, codec),
                    None => Sourced::Blocked(blocked),
                },
                SourceChoice::Blocked => Sourced::Blocked(blocked),
            },
        )
    }
}

#[cfg(test)]
mod tests;
