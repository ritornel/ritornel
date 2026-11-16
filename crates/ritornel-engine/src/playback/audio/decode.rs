//! The decode side of the pipeline: a file through Symphonia, then into the
//! output's channels, then through rubato to the output's rate. See "The
//! pipeline" in docs/design/playback.md.
//!
//! Each step is one the pipeline can leave out: a track whose channels are
//! the output's is copied, and one at the output's rate is not resampled,
//! so the samples come out as the decoder made them. Samples are 32-bit
//! float, which holds 16 and 24-bit samples exactly.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Indexing, Resampler};
use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::errors::Error as Symphonia;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::{MediaSource, MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::{Time, TimeBase, Timestamp};

use crate::media::audio_end;
use crate::playback::renderer::{FileFacts, Format, RenderError};

/// What the output takes: its rate and its channels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OutputSpec {
    pub(crate) rate: u32,
    pub(crate) channels: usize,
}

/// The input frames the resampler takes at a time.
const RESAMPLER_CHUNK: usize = 1024;

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
    pub(crate) info: Format,
    /// Packets that would not decode and were passed over.
    pub(crate) bad_packets: u64,
    samples: Vec<f32>,
    /// Where in `samples` the frames to hand on begin.
    skip: usize,
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
        #[cfg(unix)]
        let read = std::os::unix::fs::FileExt::read_at(&*self.file, buffer, self.at)?;
        #[cfg(windows)]
        let read = std::os::windows::fs::FileExt::seek_read(&*self.file, buffer, self.at)?;
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
        // As the scan reads them: a tag after the last frame of an MP3 or
        // a FLAC is hidden, or its frame scanner would play it as audio.
        if matches!(
            format.format_info().short_name,
            "mp1" | "mp2" | "mp3" | "flac"
        ) && opened.audio_end < opened.len
        {
            format = probe(Box::new(opened.reader(opened.audio_end)), &hint)?;
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
            info: Format {
                codec,
                sample_rate_hz: rate,
                channels: u32::try_from(channels).unwrap_or(0),
                bits: params.bits_per_sample,
                duration_ms,
            },
            bad_packets: 0,
            samples: Vec::new(),
            skip: 0,
        };
        if start_ms > 0 {
            source.seek(start_ms)?;
        }
        Ok(source)
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
            // A seek past the end leaves nothing to play.
            Err(Symphonia::SeekError(_)) => {
                self.from_frame = u64::MAX;
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
    /// decode is passed over.
    pub(crate) fn next(&mut self) -> Result<bool, RenderError> {
        loop {
            let packet = match self.format.next_packet() {
                Ok(Some(packet)) => packet,
                Ok(None) => return Ok(false),
                // A file cut short ends where it is cut.
                Err(Symphonia::IoError(error))
                    if error.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    return Ok(false);
                }
                Err(error) => return Err(RenderError::new("unreadable", error.to_string())),
            };
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
            .map_err(|error| RenderError::new("output", error.to_string()))?;
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

/// Decodes the file at `path` from `start_ms`, converts it for the output,
/// and hands every frame to `sink`, until the track ends, `sink` returns
/// false, or `stop` is set. The file is opened by the caller, so that an
/// error in opening it is the caller's to report.
pub(crate) fn run(
    mut source: Source,
    to: OutputSpec,
    stop: &AtomicBool,
    sink: &mut dyn FnMut(&[f32]) -> bool,
) -> Result<Source, RenderError> {
    let (rate, channels) = source.spec();
    let mut convert: Option<Convert> = if channels > 0 {
        Some(Convert::new(rate, channels, to)?)
    } else {
        None
    };
    loop {
        if stop.load(Ordering::SeqCst) {
            return Ok(source);
        }
        if !source.next()? {
            break;
        }
        let convert = match &mut convert {
            Some(convert) => convert,
            None => convert.insert(Convert::new(rate, source.spec().1, to)?),
        };
        if !convert.push(source.frames(), sink)? {
            return Ok(source);
        }
    }
    if let Some(convert) = &mut convert {
        convert.finish(sink)?;
    }
    Ok(source)
}
