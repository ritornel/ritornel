//! The decode side of the pipeline: a file through Symphonia, then into the
//! output's channels, then through rubato to the output's rate. See "The
//! pipeline" in docs/design/playback.md.
//!
//! Each step is one the pipeline can leave out: a track with no ReplayGain
//! to apply is not scaled, one whose channels are the output's is copied,
//! and one at the output's rate is not resampled, so the samples come out
//! as the decoder made them. Samples are 32-bit float, which holds 16 and
//! 24-bit samples exactly.
//!
//! One decode thread plays a track and every track joined after it, into
//! one stream: tracks at one rate in the same channels share a resampler,
//! so a join is as smooth as the music across it.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Indexing, Resampler};
use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::errors::{Error as Symphonia, SeekErrorKind};
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::{MediaSource, MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::{MetadataOptions, StandardTag};
use symphonia::core::units::{Time, TimeBase, Timestamp};

use crate::media::audio_end;
use crate::playback::renderer::{FileFacts, Format, RenderError, ReplayGain};

/// What the output takes: its rate and its channels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OutputSpec {
    pub(crate) rate: u32,
    pub(crate) channels: usize,
}

/// The input frames the resampler takes at a time.
const RESAMPLER_CHUNK: usize = 1024;

/// How far past full scale a sample may go: 96 dB, far past what any
/// real file holds, and far short of what would overflow the resampler's
/// sums.
const LOUDEST: f32 = 65_536.0;

/// The rates a track may be at, from below the lowest any audio is
/// recorded at to the highest. A file that says it is at another is
/// broken, and resampling it could take an age or more memory than there
/// is.
const RATES: std::ops::RangeInclusive<u32> = 1_000..=768_000;

/// One file, open and decoding, from a position.
pub(crate) struct Source {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    time_base: Option<TimeBase>,
    rate: u32,
    channels: usize,
    /// The first frame to hand on, in source frames from the start: the
    /// position asked for, after a seek lands before it.
    from_frame: u64,
    /// Whether a seek went to the end, or past it: nothing is left.
    at_end: bool,
    pub(crate) info: Format,
    /// Packets that would not decode and were passed over.
    pub(crate) bad_packets: u64,
    /// Whether any packet decoded to frames.
    decoded: bool,
    samples: Vec<f32>,
    /// Where in `samples` the frames to hand on begin.
    skip: usize,
    /// The file's ReplayGain tags.
    pub(crate) tags: GainTags,
    /// What every sample is multiplied by; 1 leaves the step out.
    pub(crate) gain: f32,
    /// For a test: a file named `unreadable-…` fails at its first packet.
    #[cfg(test)]
    fails: bool,
    /// For a test: the packets read.
    #[cfg(test)]
    pub(crate) packets: u64,
}

/// How far under full scale a gain held by its peak takes that peak: a
/// decibel, the usual allowance for the peaks resampling makes between the
/// samples.
const PEAK_HEADROOM: f32 = 0.891;

/// A ReplayGain tag pair: the gain in decibels and the peak it was
/// measured with, as a fraction of full scale.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct GainTag {
    pub(crate) gain_db: Option<f32>,
    pub(crate) peak: Option<f32>,
}

/// A file's ReplayGain tags.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct GainTags {
    pub(crate) track: GainTag,
    pub(crate) album: GainTag,
}

impl GainTag {
    /// This pair, or `other` if this has no gain: a gain and its peak are
    /// measured together, so they are not taken from two tags.
    fn or(self, other: Self) -> Self {
        if self.gain_db.is_some() { self } else { other }
    }
}

impl GainTags {
    /// These tags, and `other`'s where these have none.
    fn or(self, other: Self) -> Self {
        Self {
            track: self.track.or(other.track),
            album: self.album.or(other.album),
        }
    }

    /// The factor a track plays at for `mode`. Limited by the peak, less a
    /// decibel for what resampling adds between the samples, and with no
    /// peak tag never above unity, since then nothing says how far it may
    /// go. An untagged file plays unchanged.
    pub(crate) fn factor(&self, mode: ReplayGain) -> f32 {
        let tag = match mode {
            ReplayGain::Off => return 1.0,
            ReplayGain::Track => self.track,
            ReplayGain::Album if self.album.gain_db.is_some() => self.album,
            ReplayGain::Album => self.track,
        };
        let Some(db) = tag.gain_db.filter(|db| db.is_finite()) else {
            return 1.0;
        };
        let gain = 10_f32.powf(db / 20.0);
        let ceiling = match tag.peak {
            Some(peak) if peak > 0.0 && peak.is_finite() => PEAK_HEADROOM / peak,
            _ => 1.0,
        };
        // Tags past any real level can take it past what a float holds.
        Some(gain.min(ceiling))
            .filter(|factor| factor.is_finite())
            .unwrap_or(1.0)
    }
}

/// A ReplayGain value as taggers write it: "-6.02 dB", "+3.1", "0.988".
fn gain_number(text: &str) -> Option<f32> {
    let text = text.trim();
    let text = text
        .strip_suffix("dB")
        .or_else(|| text.strip_suffix("db"))
        .unwrap_or(text)
        .trim();
    text.strip_prefix('+').unwrap_or(text).parse().ok()
}

/// The ReplayGain tags the reader found, in any block of metadata.
fn gain_tags(format: &mut dyn FormatReader) -> GainTags {
    let mut tags = GainTags::default();
    let mut log = format.metadata();
    loop {
        if let Some(revision) = log.current() {
            // Each block of tags gives its own pairs, which the first block
            // with a gain wins.
            let mut found = GainTags::default();
            let per_track = revision.per_track.iter().map(|track| &track.metadata);
            for container in std::iter::once(&revision.media).chain(per_track) {
                for tag in &container.tags {
                    let (slot, value) = match &tag.std {
                        Some(StandardTag::ReplayGainTrackGain(value)) => {
                            (&mut found.track.gain_db, value)
                        }
                        Some(StandardTag::ReplayGainTrackPeak(value)) => {
                            (&mut found.track.peak, value)
                        }
                        Some(StandardTag::ReplayGainAlbumGain(value)) => {
                            (&mut found.album.gain_db, value)
                        }
                        Some(StandardTag::ReplayGainAlbumPeak(value)) => {
                            (&mut found.album.peak, value)
                        }
                        _ => continue,
                    };
                    *slot = slot.or_else(|| gain_number(value));
                }
            }
            tags = tags.or(found);
        }
        if log.is_latest() {
            break;
        }
        log.pop();
    }
    tags
}

impl std::fmt::Debug for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Source")
            .field("info", &self.info)
            .field("from_frame", &self.from_frame)
            .finish_non_exhaustive()
    }
}

fn open_error(error: &Symphonia) -> RenderError {
    match error {
        Symphonia::Unsupported(_) => RenderError::new("unsupported_format", error.to_string()),
        _ => RenderError::new("unreadable", error.to_string()),
    }
}

fn probe(source: Box<dyn MediaSource>, hint: &Hint) -> Result<Box<dyn FormatReader>, RenderError> {
    let stream = MediaSourceStream::new(
        source,
        MediaSourceStreamOptions {
            buffer_len: 64 * 1024,
        },
    );
    symphonia::default::get_probe()
        .probe(
            hint,
            stream,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|error| open_error(&error))
}

/// A file a track was opened from, held open: each segment of the track
/// reads this file, whatever has taken its path since. Each reader keeps
/// its own place in it.
#[derive(Debug)]
pub(crate) struct Opened {
    path: PathBuf,
    file: Arc<File>,
    /// Its length when it was opened.
    len: u64,
    /// Where its audio ends: before any tag after the last frame.
    audio_end: u64,
    facts: FileFacts,
}

fn unreadable(error: &std::io::Error) -> RenderError {
    RenderError::new("unreadable", error.to_string())
}

/// What the file's handle says of it now: its size and time.
fn facts_of(file: &File) -> Result<FileFacts, RenderError> {
    let metadata = file.metadata().map_err(|error| unreadable(&error))?;
    Ok(FileFacts {
        size_bytes: metadata.len(),
        mtime_ms: crate::walk::mtime_ms(&metadata),
    })
}

fn changed(path: &Path) -> RenderError {
    RenderError::new(
        "file_changed",
        format!(
            "{} is not the file the library last read: its size or time is not the same. A \
             rescan of its folder reads it again",
            path.display()
        ),
    )
}

impl Opened {
    /// Opens the file at `path` and, when `expected` is given, holds it to
    /// what the library recorded of it: a file whose size or time is not
    /// what the scan saw is refused, since the library does not know what
    /// it is.
    pub(crate) fn open(path: &Path, expected: Option<FileFacts>) -> Result<Self, RenderError> {
        let file = File::open(path).map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => RenderError::new("missing", error.to_string()),
            _ => unreadable(&error),
        })?;
        let facts = facts_of(&file)?;
        if expected.is_some_and(|expected| expected != facts) {
            return Err(changed(path));
        }
        let mut probing = file.try_clone().map_err(|error| unreadable(&error))?;
        let audio_end = audio_end(&mut probing).map_err(|error| unreadable(&error))?;
        Ok(Self {
            path: path.to_owned(),
            file: Arc::new(file),
            len: facts.size_bytes,
            audio_end,
            facts,
        })
    }

    /// Fails if the file held open has been written to since it was
    /// opened, which would put other audio under the same listen.
    pub(crate) fn unchanged(&self) -> Result<(), RenderError> {
        if facts_of(&self.file)? == self.facts {
            Ok(())
        } else {
            Err(changed(&self.path))
        }
    }

    fn reader(&self, len: u64) -> Held {
        Held {
            file: Arc::clone(&self.file),
            at: 0,
            len,
        }
    }
}

/// A reader of a file held open, with a place of its own in it.
#[derive(Debug)]
struct Held {
    file: Arc<File>,
    at: u64,
    len: u64,
}

impl Read for Held {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let left = usize::try_from(self.len.saturating_sub(self.at)).unwrap_or(usize::MAX);
        let Some(buffer) = buffer.get_mut(..buffer.len().min(left)) else {
            return Ok(0);
        };
        if buffer.is_empty() {
            return Ok(0);
        }
        let read = crate::platform::read_at(&self.file, buffer, self.at)?;
        self.at += read as u64;
        Ok(read)
    }
}

impl Seek for Held {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        let from = |base: u64, offset: i64| {
            base.checked_add_signed(offset).ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "a seek before the start")
            })
        };
        self.at = match to {
            SeekFrom::Start(at) => at,
            SeekFrom::End(offset) => from(self.len, offset)?,
            SeekFrom::Current(offset) => from(self.at, offset)?,
        };
        Ok(self.at)
    }
}

impl MediaSource for Held {
    fn is_seekable(&self) -> bool {
        true
    }

    fn byte_len(&self) -> Option<u64> {
        Some(self.len)
    }
}

impl Source {
    /// Opens the file at `path` to play from `start_ms`.
    #[cfg(test)]
    pub(crate) fn open(path: &Path, start_ms: u64) -> Result<Self, RenderError> {
        Self::of(&Opened::open(path, None)?, start_ms)
    }

    /// The file held open, to play from `start_ms`.
    pub(crate) fn of(opened: &Opened, start_ms: u64) -> Result<Self, RenderError> {
        let mut hint = Hint::new();
        if let Some(extension) = opened
            .path
            .extension()
            .and_then(|extension| extension.to_str())
        {
            hint.with_extension(extension);
        }
        let mut format = probe(Box::new(opened.reader(opened.len)), &hint)?;
        let mut tags = gain_tags(format.as_mut());
        // As the scan reads them: a tag after the last frame of an MP3 or
        // a FLAC is hidden, or its frame scanner would play it as audio.
        if matches!(
            format.format_info().short_name,
            "mp1" | "mp2" | "mp3" | "flac"
        ) && opened.audio_end < opened.len
        {
            format = probe(Box::new(opened.reader(opened.audio_end)), &hint)?;
            // The tags before the audio come first. One after it, where
            // mp3gain and foobar2000 write theirs, gives what they lack.
            tags = gain_tags(format.as_mut()).or(tags);
        }
        let track = format
            .default_track(TrackType::Audio)
            .ok_or_else(|| RenderError::new("unreadable", "the file has no audio track"))?
            .clone();
        let params = track
            .codec_params
            .as_ref()
            .and_then(|params| params.audio())
            .ok_or_else(|| RenderError::new("unreadable", "the audio track has no parameters"))?
            .clone();
        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(&params, &AudioDecoderOptions::default())
            .map_err(|error| open_error(&error))?;
        let rate = params
            .sample_rate
            .filter(|rate| *rate > 0)
            .ok_or_else(|| RenderError::new("unreadable", "the track has no sample rate"))?;
        if !RATES.contains(&rate) {
            return Err(RenderError::new(
                "unsupported_format",
                format!("the track says it is at {rate} Hz, which no audio is recorded at"),
            ));
        }
        let channels = params
            .channels
            .as_ref()
            .map_or(0, symphonia::core::audio::Channels::count);
        let duration_ms = track
            .num_frames
            .map(|frames| frames.saturating_mul(1000) / u64::from(rate));
        let codec = symphonia::default::get_codecs()
            .get_audio_decoder(params.codec)
            .map_or_else(
                || "unknown".to_owned(),
                |codec| codec.codec.info.short_name.to_owned(),
            );
        let mut source = Self {
            format,
            decoder,
            track_id: track.id,
            time_base: track.time_base,
            rate,
            channels,
            from_frame: 0,
            at_end: false,
            info: Format {
                codec,
                sample_rate_hz: rate,
                channels: u32::try_from(channels).unwrap_or(0),
                bits: params.bits_per_sample,
                duration_ms,
            },
            bad_packets: 0,
            decoded: false,
            #[cfg(test)]
            fails: opened
                .path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("unreadable-")),
            #[cfg(test)]
            packets: 0,
            samples: Vec::new(),
            skip: 0,
            tags,
            gain: 1.0,
        };
        if start_ms > 0 {
            source.seek(start_ms)?;
        }
        Ok(source)
    }

    /// Plays the track at the ReplayGain `mode` asks for.
    pub(crate) fn set_replay_gain(&mut self, mode: ReplayGain) {
        self.gain = self.tags.factor(mode);
    }

    fn seek(&mut self, start_ms: u64) -> Result<(), RenderError> {
        let to = SeekTo::Time {
            time: Time::from_millis_u64(start_ms),
            track_id: Some(self.track_id),
        };
        match self.format.seek(SeekMode::Accurate, to) {
            Ok(seeked) => {
                self.decoder.reset();
                self.from_frame = self.frame_of(seeked.required_ts);
                Ok(())
            }
            // A seek to the end, or past it, leaves nothing to play, and
            // nothing to read: a FLAC's reader runs out of file looking for
            // the frame there, and the others say it is out of range.
            Err(Symphonia::SeekError(SeekErrorKind::OutOfRange)) => {
                self.at_end = true;
                Ok(())
            }
            // Running out of file anywhere short of the end the track says
            // it has is a file cut short, or a reader that lost its place.
            Err(Symphonia::IoError(error))
                if error.kind() == std::io::ErrorKind::UnexpectedEof
                    && self.info.duration_ms.is_none_or(|end| start_ms >= end) =>
            {
                self.at_end = true;
                Ok(())
            }
            Err(error) => Err(RenderError::new("unreadable", error.to_string())),
        }
    }

    /// A timestamp of the track as a frame number at its own rate.
    fn frame_of(&self, ts: Timestamp) -> u64 {
        let ts = u64::try_from(ts.get().max(0)).unwrap_or(0);
        match self.time_base {
            Some(base) => {
                let frames = u128::from(ts) * u128::from(base.numer.get()) * u128::from(self.rate)
                    / u128::from(base.denom.get());
                u64::try_from(frames).unwrap_or(u64::MAX)
            }
            None => ts,
        }
    }

    /// The track's own rate and channels.
    pub(crate) fn spec(&self) -> (u32, usize) {
        (self.rate, self.channels)
    }

    /// The frames the last [`next`](Self::next) decoded, interleaved at
    /// the track's own rate and channels.
    pub(crate) fn frames(&self) -> &[f32] {
        self.samples.get(self.skip..).unwrap_or(&[])
    }

    /// Decodes the next frames. `false` at the end. A packet that will not
    /// decode is passed over, but a file that gives no frames at all, from
    /// its start or through packets that would not decode, is unreadable,
    /// not over.
    pub(crate) fn next(&mut self) -> Result<bool, RenderError> {
        #[cfg(test)]
        if self.fails {
            return Err(RenderError::new("unreadable", "the test made it so"));
        }
        if self.at_end {
            return Ok(false);
        }
        loop {
            let packet = match self.format.next_packet() {
                Ok(Some(packet)) => packet,
                Ok(None) => return self.over(),
                // A file cut short ends where it is cut.
                Err(Symphonia::IoError(error))
                    if error.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    return self.over();
                }
                Err(error) => return Err(RenderError::new("unreadable", error.to_string())),
            };
            #[cfg(test)]
            {
                self.packets += 1;
            }
            if packet.track_id != self.track_id {
                continue;
            }
            let first = self.frame_of(packet.pts.saturating_add(packet.trim_start));
            let decoded = match self.decoder.decode(&packet) {
                Ok(decoded) => decoded,
                Err(Symphonia::DecodeError(_) | Symphonia::IoError(_)) => {
                    self.bad_packets += 1;
                    continue;
                }
                Err(error) => return Err(RenderError::new("unreadable", error.to_string())),
            };
            let frames = decoded.frames();
            let channels = decoded.spec().channels().count();
            if frames == 0 || channels == 0 {
                continue;
            }
            self.decoded = true;
            if self.channels == 0 {
                self.channels = channels;
                self.info.channels = u32::try_from(channels).unwrap_or(0);
            }
            if channels != self.channels {
                return Err(RenderError::new(
                    "unreadable",
                    "the channels changed in the middle of the track",
                ));
            }
            self.samples.clear();
            decoded.copy_to_vec_interleaved(&mut self.samples);
            // After a seek the packet may begin before the position.
            let skip = usize::try_from(self.from_frame.saturating_sub(first))
                .unwrap_or(usize::MAX)
                .min(frames);
            if skip == frames {
                continue;
            }
            self.skip = skip * channels;
            return Ok(true);
        }
    }

    /// The end of the file, once its packets have run out. Read from its
    /// start, or with packets passed over, a file that gave no frames is
    /// unreadable; one sought near its end may rightly give none.
    fn over(&self) -> Result<bool, RenderError> {
        if !self.decoded && (self.bad_packets > 0 || self.from_frame == 0) {
            return Err(RenderError::new(
                "unreadable",
                format!(
                    "no audio would decode: {} packets passed over",
                    self.bad_packets
                ),
            ));
        }
        Ok(false)
    }
}

/// The steps between the decoder and the output: the output's channels,
/// then its rate.
pub(crate) struct Convert {
    from_channels: usize,
    to: OutputSpec,
    resampler: Option<Box<Fft<f32>>>,
    /// Frames in the output's channels waiting for the resampler.
    pending: Vec<f32>,
    /// The resampler's output.
    resampled: Vec<f32>,
    /// Frames of the resampler's start-up delay still to drop.
    delay: usize,
    /// Frames taken in, and frames handed on, after the delay.
    taken: u64,
    given: u64,
    from_rate: u32,
    mapped: Vec<f32>,
}

impl std::fmt::Debug for Convert {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Convert")
            .field("from_channels", &self.from_channels)
            .field("to", &self.to)
            .field("resamples", &self.resampler.is_some())
            .finish_non_exhaustive()
    }
}

impl Convert {
    /// The steps from a track's rate and channels to the output's. Either
    /// step that would change nothing is left out.
    pub(crate) fn new(
        from_rate: u32,
        from_channels: usize,
        to: OutputSpec,
    ) -> Result<Self, RenderError> {
        let resampler = if from_rate == to.rate {
            None
        } else {
            let fft = Fft::<f32>::new(
                usize::try_from(from_rate).unwrap_or(0),
                usize::try_from(to.rate).unwrap_or(0),
                RESAMPLER_CHUNK,
                to.channels,
                FixedSync::Input,
            )
            // A rate it cannot convert is the track's, not the output's.
            .map_err(|error| RenderError::new("unsupported_format", error.to_string()))?;
            Some(Box::new(fft))
        };
        let delay = resampler.as_ref().map_or(0, |fft| fft.output_delay());
        let resampled = resampler.as_ref().map_or(Vec::new(), |fft| {
            vec![0.0; fft.output_frames_max() * to.channels]
        });
        Ok(Self {
            from_channels: from_channels.max(1),
            to,
            resampler,
            pending: Vec::new(),
            resampled,
            delay,
            taken: 0,
            given: 0,
            from_rate,
            mapped: Vec::new(),
        })
    }

    /// Whether a track at this rate and in these channels can go on through
    /// these steps, joined to the one before it.
    fn takes(&self, rate: u32, channels: usize) -> bool {
        rate == self.from_rate && channels == self.from_channels
    }

    /// Where the input taken so far ends in the output, in frames from the
    /// first: where a track that ends here ends, once all of it is out.
    fn boundary(&self) -> u64 {
        self.taken * u64::from(self.to.rate) / u64::from(self.from_rate.max(1))
    }

    /// Puts interleaved frames in the output's channels: a mono track goes
    /// to every channel, a track's first channels go to the output's first,
    /// and a channel the track does not have is silent. Two or more going
    /// to one are averaged from the first two.
    fn map(&mut self, frames: &[f32]) {
        let (from, to) = (self.from_channels, self.to.channels);
        self.mapped.clear();
        if from == to {
            self.mapped.extend_from_slice(frames);
            return;
        }
        for frame in frames.chunks_exact(from) {
            for channel in 0..to {
                let sample = match (from, to) {
                    (1, _) => frame.first().copied(),
                    (_, 1) => match (frame.first(), frame.get(1)) {
                        (Some(left), Some(right)) => Some((left + right) * 0.5),
                        (Some(only), None) => Some(*only),
                        _ => None,
                    },
                    _ => frame.get(channel).copied(),
                };
                self.mapped.push(sample.unwrap_or(0.0));
            }
        }
    }

    /// Converts `frames`, interleaved at the track's rate and channels, and
    /// hands what comes out to `sink`. `sink` returns false to stop.
    pub(crate) fn push(
        &mut self,
        frames: &[f32],
        sink: &mut dyn FnMut(&[f32]) -> bool,
    ) -> Result<bool, RenderError> {
        self.map(frames);
        let channels = self.to.channels;
        self.taken += (self.mapped.len() / channels) as u64;
        if self.resampler.is_none() {
            return Ok(sink(&self.mapped));
        }
        let mapped = std::mem::take(&mut self.mapped);
        self.pending.extend_from_slice(&mapped);
        self.mapped = mapped;
        loop {
            let needed = self
                .resampler
                .as_ref()
                .map_or(0, |fft| fft.input_frames_next());
            if self.pending.len() < needed * channels {
                return Ok(true);
            }
            if !self.resample(false, sink)? {
                return Ok(false);
            }
        }
    }

    /// Resamples one chunk of what is pending. The last chunk of a track
    /// is what is left, padded with silence, and after it the chunks are
    /// silence alone, which bring out what the resampler still holds.
    /// Returns what `sink` said.
    fn resample(
        &mut self,
        last: bool,
        sink: &mut dyn FnMut(&[f32]) -> bool,
    ) -> Result<bool, RenderError> {
        let channels = self.to.channels;
        let Some(fft) = self.resampler.as_mut() else {
            return Ok(true);
        };
        let needed = fft.input_frames_next();
        let take = (self.pending.len() / channels).min(needed);
        // The last chunk is padded with silence to a whole one. What is
        // pending past this chunk stays for the next.
        if self.pending.len() < needed * channels {
            self.pending.resize(needed * channels, 0.0);
        }
        let input = InterleavedSlice::new(&self.pending, channels, needed)
            .map_err(|error| RenderError::new("output", error.to_string()))?;
        let output_frames = self.resampled.len() / channels;
        let mut output = InterleavedSlice::new_mut(&mut self.resampled, channels, output_frames)
            .map_err(|error| RenderError::new("output", error.to_string()))?;
        let indexing = Indexing {
            input_offset: 0,
            output_offset: 0,
            partial_len: last.then_some(take),
            active_channels_mask: None,
        };
        let (_, made) = fft
            .process_into_buffer(&input, &mut output, Some(&indexing))
            .map_err(|error| RenderError::new("output", error.to_string()))?;
        if last {
            self.pending.clear();
        } else {
            self.pending.drain(..take * channels);
        }
        // The resampler's start-up delay is silence before the track.
        let dropped = made.min(self.delay);
        self.delay -= dropped;
        // Nothing is handed on past the track's length at the new rate.
        let expected = self.taken * u64::from(self.to.rate) / u64::from(self.from_rate.max(1));
        let room = usize::try_from(expected.saturating_sub(self.given)).unwrap_or(usize::MAX);
        let give = (made - dropped).min(room);
        self.given += give as u64;
        let from = dropped * channels;
        Ok(match self.resampled.get(from..from + give * channels) {
            Some(frames) if !frames.is_empty() => sink(frames),
            _ => true,
        })
    }

    /// The track has ended: what the resampler still holds is handed on,
    /// up to the track's length at the output's rate.
    pub(crate) fn finish(
        &mut self,
        sink: &mut dyn FnMut(&[f32]) -> bool,
    ) -> Result<(), RenderError> {
        if self.resampler.is_none() {
            return Ok(());
        }
        let expected = self.taken * u64::from(self.to.rate) / u64::from(self.from_rate.max(1));
        // Each round feeds a chunk, the last of the track and then silence,
        // until the whole length has come out. The bound stops a resampler
        // that makes nothing.
        for _ in 0..64 {
            if self.given >= expected {
                break;
            }
            if !self.resample(true, sink)? {
                break;
            }
        }
        Ok(())
    }
}

/// Where the decode thread puts what it decodes.
pub(crate) trait Stream {
    /// Hands frames on, in the output's rate and channels. `false` stops
    /// the decoding.
    fn write(&mut self, frames: &[f32]) -> bool;

    /// The track to join after the one that has just ended, with the
    /// segment it plays as, if one has been handed over by now. It never
    /// waits for one: with none, the stream is finished.
    fn next(&mut self) -> Option<(u64, Source)> {
        None
    }

    /// The joined track `segment` begins at frame `at` of the stream,
    /// counted from its first.
    fn mark(&mut self, _at: u64, _segment: u64) {}
}

/// A stream that is a sink and nothing more.
#[cfg(test)]
struct Sink<'a>(&'a mut dyn FnMut(&[f32]) -> bool);

#[cfg(test)]
impl Stream for Sink<'_> {
    fn write(&mut self, frames: &[f32]) -> bool {
        (self.0)(frames)
    }
}

/// Decodes a track, converts it for the output, and hands every frame to
/// `sink`, until the track ends, `sink` returns false, or `stop` is set.
/// The file is opened by the caller, so that an error in opening it is the
/// caller's to report.
#[cfg(test)]
pub(crate) fn run(
    source: Source,
    to: OutputSpec,
    stop: &AtomicBool,
    sink: &mut dyn FnMut(&[f32]) -> bool,
) -> Result<(), RenderError> {
    run_joined(source, to, stop, &mut Sink(sink))
}

/// As `run`, and each time a track ends the stream may give the track
/// that follows it: it is decoded into the same stream with nothing
/// between them, and the stream is told the frame where it begins.
pub(crate) fn run_joined(
    mut source: Source,
    to: OutputSpec,
    stop: &AtomicBool,
    stream: &mut dyn Stream,
) -> Result<(), RenderError> {
    let mut convert: Option<Convert> = None;
    // Frames of the stream from steps already finished, at a change of
    // rate or channels.
    let mut base: u64 = 0;
    let mut scaled = Vec::new();
    loop {
        loop {
            if stop.load(Ordering::SeqCst) {
                return Ok(());
            }
            let more = match source.next() {
                Ok(more) => more,
                Err(error) => {
                    // What was decoded is heard before the error ends the
                    // stream: past a join it reaches the marker, so the
                    // track before ends and the failing one fails.
                    if let Some(convert) = &mut convert {
                        convert.finish(&mut |frames| stream.write(frames))?;
                    }
                    return Err(error);
                }
            };
            if !more {
                break;
            }
            let (rate, channels) = source.spec();
            let convert = match &mut convert {
                Some(convert) => convert,
                None => convert.insert(Convert::new(rate, channels, to)?),
            };
            let frames = source.frames();
            let fit = |sample: &f32| sample.abs() <= LOUDEST;
            let frames = if source.gain == 1.0 && frames.iter().all(fit) {
                frames
            } else {
                // A broken float file can hold a sample that is not a
                // number, which is silence, or one too large for the
                // resampler, which is held to the loudest it takes.
                let gain = source.gain;
                scaled.clear();
                scaled.extend(frames.iter().map(|sample| {
                    let sample = sample * gain;
                    if sample.is_finite() {
                        sample.clamp(-LOUDEST, LOUDEST)
                    } else {
                        0.0
                    }
                }));
                &scaled
            };
            if !convert.push(frames, &mut |frames| stream.write(frames))? {
                return Ok(());
            }
        }
        let Some((segment, following)) = stream.next() else {
            break;
        };
        let (rate, channels) = following.spec();
        if let Some(mut ended) = convert.take_if(|convert| !convert.takes(rate, channels)) {
            ended.finish(&mut |frames| stream.write(frames))?;
            base += ended.boundary();
        }
        stream.mark(
            base + convert.as_ref().map_or(0, Convert::boundary),
            segment,
        );
        source = following;
    }
    if let Some(convert) = &mut convert {
        convert.finish(&mut |frames| stream.write(frames))?;
    }
    Ok(())
}
