//! Reading one file: the "where the file is" half of a scan. It demuxes
//! without decoding, hashes the audio, reads every tag verbatim and
//! records each picture by its hash, and returns the payload that is
//! stored as a revision. See "Scanning and audio hashing" in
//! docs/internals/identity.md.
//!
//! Audio hash scheme 1 (`HASH_SCHEME = 1`) hashes `ritornel-audio`, the
//! scheme number, our own name for the codec, the parameters and the
//! configuration the codec's table names, and the payload bytes of every
//! packet in order. It is that definition as Symphonia 0.6.1 delivers it,
//! which is why the fixtures in `fixtures/` pin every hash.
//!
//! Nothing here writes: a file is opened for reading and nothing else.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::panic::AssertUnwindSafe;
use std::path::Path;

use ritornel_domain::payload::{
    Audio, FolderImage, PAYLOAD_VERSION, Payload, Picture, ReadError, Tag,
};
use symphonia::core::codecs::audio::well_known::{
    CODEC_ID_AAC, CODEC_ID_ALAC, CODEC_ID_FLAC, CODEC_ID_MP1, CODEC_ID_MP2, CODEC_ID_MP3,
    CODEC_ID_OPUS, CODEC_ID_PCM_ALAW, CODEC_ID_PCM_F32BE, CODEC_ID_PCM_F32LE, CODEC_ID_PCM_F64BE,
    CODEC_ID_PCM_F64LE, CODEC_ID_PCM_MULAW, CODEC_ID_PCM_S8, CODEC_ID_PCM_S16BE,
    CODEC_ID_PCM_S16LE, CODEC_ID_PCM_S24BE, CODEC_ID_PCM_S24LE, CODEC_ID_PCM_S32BE,
    CODEC_ID_PCM_S32LE, CODEC_ID_PCM_U8, CODEC_ID_VORBIS,
};
use symphonia::core::codecs::audio::{AudioCodecId, VerificationCheck};
use symphonia::core::common::Limit;
use symphonia::core::errors::Error as Symphonia;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, TrackType};
use symphonia::core::io::{MediaSource, MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::{MetadataOptions, RawValue, StandardVisualKey, Visual};

/// The scheme of the audio hash this reader computes.
pub(crate) const HASH_SCHEME: u32 = 1;

/// The largest picture read from a file. A larger one is left out.
const MAX_PICTURE_BYTES: usize = 64 << 20;

/// A tag value that is bytes is kept, as hex, up to this length.
const MAX_BINARY_TAG_BYTES: usize = 256;

/// The file names, without their extension, of a folder's own image.
const FOLDER_IMAGE_STEMS: [&str; 3] = ["cover", "folder", "front"];

/// The extensions of a folder's own image.
const FOLDER_IMAGE_EXTENSIONS: [&str; 3] = ["jpg", "jpeg", "png"];

/// Whether a file of this name is a folder's own image.
pub(crate) fn is_folder_image(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.rsplit_once('.').is_some_and(|(stem, extension)| {
        FOLDER_IMAGE_STEMS.contains(&stem) && FOLDER_IMAGE_EXTENSIONS.contains(&extension)
    })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Our own codec names. They are part of the hash, so they never follow a
/// renaming in the demuxer.
fn codec_name(id: AudioCodecId) -> String {
    let name = match id {
        CODEC_ID_FLAC => "flac",
        CODEC_ID_MP3 => "mp3",
        CODEC_ID_MP2 => "mp2",
        CODEC_ID_MP1 => "mp1",
        CODEC_ID_AAC => "aac",
        CODEC_ID_ALAC => "alac",
        CODEC_ID_VORBIS => "vorbis",
        CODEC_ID_OPUS => "opus",
        CODEC_ID_PCM_S8 => "pcm_s8",
        CODEC_ID_PCM_U8 => "pcm_u8",
        CODEC_ID_PCM_S16LE => "pcm_s16le",
        CODEC_ID_PCM_S16BE => "pcm_s16be",
        CODEC_ID_PCM_S24LE => "pcm_s24le",
        CODEC_ID_PCM_S24BE => "pcm_s24be",
        CODEC_ID_PCM_S32LE => "pcm_s32le",
        CODEC_ID_PCM_S32BE => "pcm_s32be",
        CODEC_ID_PCM_F32LE => "pcm_f32le",
        CODEC_ID_PCM_F32BE => "pcm_f32be",
        CODEC_ID_PCM_F64LE => "pcm_f64le",
        CODEC_ID_PCM_F64BE => "pcm_f64be",
        CODEC_ID_PCM_ALAW => "pcm_alaw",
        CODEC_ID_PCM_MULAW => "pcm_mulaw",
        other => return format!("symphonia:{other}"),
    };
    name.to_owned()
}

/// A file with its trailing tags hidden, so that the demuxer's frame
/// scanner never reads an ID3v1, APEv2 or Lyrics3 tag as audio.
struct Bounded {
    file: File,
    len: u64,
}

impl Read for Bounded {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let position = self.file.stream_position()?;
        let left = usize::try_from(self.len.saturating_sub(position)).unwrap_or(usize::MAX);
        let take = buffer.len().min(left);
        match buffer.get_mut(..take) {
            Some(buffer) if take > 0 => self.file.read(buffer),
            _ => Ok(0),
        }
    }
}

impl Seek for Bounded {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        let to = match to {
            SeekFrom::End(offset) => {
                let end = i64::try_from(self.len).unwrap_or(i64::MAX);
                SeekFrom::Start(u64::try_from(end.saturating_add(offset).max(0)).unwrap_or(0))
            }
            other => other,
        };
        self.file.seek(to)
    }
}

impl MediaSource for Bounded {
    fn is_seekable(&self) -> bool {
        true
    }

    fn byte_len(&self) -> Option<u64> {
        Some(self.len)
    }
}

/// The last `count` bytes before `end`.
fn tail(file: &mut File, end: u64, count: u64) -> std::io::Result<Option<Vec<u8>>> {
    if end < count {
        return Ok(None);
    }
    let mut bytes = vec![0; usize::try_from(count).unwrap_or(0)];
    file.seek(SeekFrom::Start(end - count))?;
    file.read_exact(&mut bytes)?;
    Ok(Some(bytes))
}

/// Where the audio ends: the file's length with the trailing ID3v1,
/// Lyrics3v2 and APEv2 tags removed, repeatedly. These are the three kinds
/// of tag that sit after the last frame of an MP3 or a FLAC.
pub(crate) fn audio_end(file: &mut File) -> std::io::Result<u64> {
    let mut end = file.metadata()?.len();
    loop {
        let before = end;
        if tail(file, end, 128)?.is_some_and(|bytes| bytes.starts_with(b"TAG")) {
            end -= 128;
        }
        if let Some(bytes) = tail(file, end, 15)?
            && bytes.ends_with(b"LYRICS200")
            && let Some(size) = bytes
                .get(..6)
                .and_then(|digits| std::str::from_utf8(digits).ok())
            && let Ok(size) = size.parse::<u64>()
        {
            // The size leaves out these 15 bytes.
            end = end.saturating_sub(size + 15);
        }
        if let Some(bytes) = tail(file, end, 32)?
            && bytes.starts_with(b"APETAGEX")
        {
            let word = |at: usize| {
                bytes
                    .get(at..at + 4)
                    .and_then(|word| <[u8; 4]>::try_from(word).ok())
                    .map_or(0, u32::from_le_bytes)
            };
            // The size is the items and the footer; a header is 32 more.
            let header = if word(20) & 0x8000_0000 != 0 { 32 } else { 0 };
            end = end.saturating_sub(u64::from(word(12)) + header);
        }
        if end == before {
            break;
        }
    }
    file.seek(SeekFrom::Start(0))?;
    Ok(end)
}

fn probe(
    source: Box<dyn MediaSource>,
    hint: &Hint,
) -> symphonia::core::errors::Result<Box<dyn FormatReader>> {
    let stream = MediaSourceStream::new(
        source,
        MediaSourceStreamOptions {
            buffer_len: 64 * 1024,
        },
    );
    let metadata = MetadataOptions::default().limit_visual_bytes(Limit::Maximum(MAX_PICTURE_BYTES));
    symphonia::default::get_probe().probe(hint, stream, FormatOptions::default(), metadata)
}

/// The kind of a read that failed because the file could not be reached,
/// not because of what is in it: it was opened, or read, and the system
/// said no, as a disk that went away does. A scan records nothing of such
/// a read, so the file keeps what was last read of it and is read again.
pub(crate) const UNAVAILABLE: &str = "unavailable";

/// The kind of an I/O failure. One the system reports, or a file that has
/// gone, says nothing of the file's content; a short read does.
fn io_kind(error: &std::io::Error) -> &'static str {
    if error.raw_os_error().is_some() || error.kind() == std::io::ErrorKind::NotFound {
        UNAVAILABLE
    } else {
        "io"
    }
}

fn io_failed(error: &std::io::Error) -> ReadError {
    failed(io_kind(error), error.to_string())
}

fn read_error(error: &Symphonia) -> ReadError {
    let kind = match error {
        Symphonia::IoError(error) => io_kind(error),
        Symphonia::DecodeError(_) => "malformed",
        Symphonia::SeekError(_) => "seek",
        Symphonia::Unsupported(_) => "unsupported",
        Symphonia::LimitError(_) => "limit",
        _ => "other",
    };
    ReadError {
        kind: kind.to_owned(),
        message: error.to_string(),
    }
}

fn failed(kind: &str, message: impl Into<String>) -> ReadError {
    ReadError {
        kind: kind.to_owned(),
        message: message.into(),
    }
}

/// The size of an image, from the header of a PNG, a JPEG or a GIF.
pub(crate) fn image_size(bytes: &[u8]) -> Option<(u32, u32)> {
    let be16 = |at: usize| {
        bytes
            .get(at..at + 2)
            .and_then(|pair| <[u8; 2]>::try_from(pair).ok())
            .map(|pair| u32::from(u16::from_be_bytes(pair)))
    };
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        let be32 = |at: usize| {
            bytes
                .get(at..at + 4)
                .and_then(|word| <[u8; 4]>::try_from(word).ok())
                .map(u32::from_be_bytes)
        };
        return Some((be32(16)?, be32(20)?));
    }
    if bytes.starts_with(b"GIF8") {
        let le16 = |at: usize| {
            bytes
                .get(at..at + 2)
                .and_then(|pair| <[u8; 2]>::try_from(pair).ok())
                .map(|pair| u32::from(u16::from_le_bytes(pair)))
        };
        return Some((le16(6)?, le16(8)?));
    }
    if bytes.starts_with(&[0xff, 0xd8]) {
        // Walk the segments to the first start-of-frame.
        let mut at = 2;
        loop {
            if *bytes.get(at)? != 0xff {
                return None;
            }
            let marker = *bytes.get(at + 1)?;
            if marker == 0xff {
                at += 1;
                continue;
            }
            if matches!(marker, 0xd0..=0xd9 | 0x01) {
                at += 2;
                continue;
            }
            let frame = matches!(marker, 0xc0..=0xcf) && !matches!(marker, 0xc4 | 0xc8 | 0xcc);
            if frame {
                return Some((be16(at + 7)?, be16(at + 5)?));
            }
            at += 2 + usize::try_from(be16(at + 2)?).ok()?;
        }
    }
    None
}

pub(crate) fn mime_of(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(b"\x89PNG") {
        "image/png"
    } else if bytes.starts_with(&[0xff, 0xd8]) {
        "image/jpeg"
    } else if bytes.starts_with(b"GIF8") {
        "image/gif"
    } else {
        "application/octet-stream"
    }
}

fn picture(
    bytes: &[u8],
    mime: Option<&str>,
    size: Option<(u32, u32)>,
    kind: Option<u32>,
) -> Picture {
    let size = size
        .filter(|(width, height)| *width > 0 && *height > 0)
        .or_else(|| image_size(bytes));
    Picture {
        hash: blake3::hash(bytes).to_hex().to_string(),
        mime: mime
            .filter(|mime| mime.contains('/'))
            .unwrap_or_else(|| mime_of(bytes))
            .to_owned(),
        width: size.map(|(width, _)| width),
        height: size.map(|(_, height)| height),
        size: bytes.len() as u64,
        kind,
    }
}

/// The picture type as ID3v2 and FLAC number it; 3 is a front cover.
fn picture_kind(usage: Option<StandardVisualKey>) -> Option<u32> {
    Some(match usage? {
        StandardVisualKey::FileIcon => 1,
        StandardVisualKey::OtherIcon => 2,
        StandardVisualKey::FrontCover => 3,
        StandardVisualKey::BackCover => 4,
        StandardVisualKey::Leaflet => 5,
        StandardVisualKey::Media => 6,
        StandardVisualKey::LeadArtistPerformerSoloist => 7,
        StandardVisualKey::ArtistPerformer => 8,
        StandardVisualKey::Conductor => 9,
        StandardVisualKey::BandOrchestra => 10,
        StandardVisualKey::Composer => 11,
        StandardVisualKey::Lyricist => 12,
        StandardVisualKey::RecordingLocation => 13,
        StandardVisualKey::RecordingSession => 14,
        StandardVisualKey::Performance => 15,
        StandardVisualKey::ScreenCapture => 16,
        StandardVisualKey::Illustration => 18,
        StandardVisualKey::BandArtistLogo => 19,
        StandardVisualKey::PublisherStudioLogo => 20,
        _ => 0,
    })
}

fn visual(visual: &Visual) -> Picture {
    picture(
        &visual.data,
        visual.media_type.as_deref(),
        visual.dimensions.map(|size| (size.width, size.height)),
        picture_kind(visual.usage),
    )
}

/// One tag as the payload keeps it: the key and the value as read.
fn tag(tag: &symphonia::core::meta::Tag) -> Tag {
    let raw = &tag.raw;
    let text = |value: &RawValue| -> (Vec<String>, Option<u64>) {
        match value {
            RawValue::String(text) => (vec![text.as_ref().clone()], None),
            RawValue::StringList(list) => (list.as_ref().clone(), None),
            RawValue::Binary(bytes) if bytes.len() <= MAX_BINARY_TAG_BYTES => {
                (vec![hex(bytes)], Some(bytes.len() as u64))
            }
            RawValue::Binary(bytes) => (Vec::new(), Some(bytes.len() as u64)),
            RawValue::Boolean(flag) => (vec![flag.to_string()], None),
            RawValue::Flag => (Vec::new(), None),
            RawValue::Float(number) => (vec![number.to_string()], None),
            RawValue::SignedInt(number) => (vec![number.to_string()], None),
            RawValue::UnsignedInt(number) => (vec![number.to_string()], None),
            _ => (Vec::new(), None),
        }
    };
    let (values, binary_len) = text(&raw.value);
    // What tells one frame of this key from another: a TXXX frame's
    // description, a UFID's owner, a freeform atom's mean and name.
    let sub = raw.sub_fields.as_ref().map(|fields| {
        fields
            .iter()
            .flat_map(|field| text(&field.value).0)
            .collect::<Vec<_>>()
            .join(":")
    });
    Tag {
        key: raw.key.clone(),
        block: None,
        sub: sub.filter(|sub| !sub.is_empty()),
        values,
        binary_len,
    }
}

/// Every tag and picture the reader has, across every block of metadata
/// the file holds, each tag with the block it came from.
fn metadata(format: &mut dyn FormatReader) -> (Vec<Tag>, Vec<Picture>) {
    let (mut tags, mut pictures) = (Vec::new(), Vec::new());
    let mut log = format.metadata();
    loop {
        if let Some(revision) = log.current() {
            let block = revision.info.short_name;
            let per_track = revision.per_track.iter().map(|track| &track.metadata);
            for container in std::iter::once(&revision.media).chain(per_track) {
                tags.extend(container.tags.iter().map(|read| Tag {
                    block: Some(block.to_owned()),
                    ..tag(read)
                }));
                pictures.extend(container.visuals.iter().map(visual));
            }
        }
        if log.is_latest() {
            break;
        }
        log.pop();
    }
    (tags, pictures)
}

/// The bytes of the picture embedded in the file at `path` whose hash is
/// `hash`, if the file still has it. It is the scan's reader, so it finds
/// what a scan recorded, in any container the scan reads.
pub(crate) fn embedded_picture(path: &Path, hash: &[u8]) -> Option<Vec<u8>> {
    let mut hint = Hint::new();
    if let Some(extension) = path.extension().and_then(|extension| extension.to_str()) {
        hint.with_extension(extension);
    }
    let mut format = probe(Box::new(File::open(path).ok()?), &hint).ok()?;
    let mut log = format.metadata();
    loop {
        if let Some(revision) = log.current() {
            let per_track = revision.per_track.iter().map(|track| &track.metadata);
            for container in std::iter::once(&revision.media).chain(per_track) {
                if let Some(found) = container
                    .visuals
                    .iter()
                    .find(|visual| blake3::hash(&visual.data).as_bytes() == hash)
                {
                    return Some(found.data.to_vec());
                }
            }
        }
        if log.is_latest() {
            return None;
        }
        log.pop();
    }
}

/// A folder's own image, the file at `rel` under `root`, read whole to hash
/// it, as [`open_under`](crate::walk::open_under) opens it.
pub(crate) fn read_folder_image(root: &Path, rel: &str, mtime_ms: i64) -> Option<FolderImage> {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    let mut bytes = Vec::new();
    crate::walk::open_under(root, rel)
        .ok()?
        .read_to_end(&mut bytes)
        .ok()?;
    Some(FolderImage {
        name: name.to_owned(),
        mtime_ms,
        picture: picture(&bytes, None, None, None),
    })
}

/// Reads the audio of the file at `rel` under `root` into `payload`: the
/// container, the tags, the pictures and the audio with its hash. A file
/// that cannot be opened, as [`open_under`](crate::walk::open_under) opens
/// it, is unavailable: it was not reached, so nothing is known of it.
fn read_audio(root: &Path, rel: &str, payload: &mut Payload) -> Result<(), ReadError> {
    let mut hint = Hint::new();
    if let Some(extension) = Path::new(rel)
        .extension()
        .and_then(|extension| extension.to_str())
    {
        hint.with_extension(extension);
    }
    let open = || {
        crate::walk::open_under(root, rel).map_err(|error| failed(UNAVAILABLE, error.to_string()))
    };
    let mut format = probe(Box::new(open()?), &hint).map_err(|error| read_error(&error))?;
    payload.container = Some(format.format_info().short_name.to_owned());
    (payload.tags, payload.pictures) = metadata(format.as_mut());

    // The MPEG audio and FLAC readers scan for frames to the end of the
    // file, so a tag stored after the last frame is hidden from them.
    if matches!(
        format.format_info().short_name,
        "mp1" | "mp2" | "mp3" | "flac"
    ) {
        let mut file = open()?;
        let total = file.metadata().map_err(|error| io_failed(&error))?.len();
        let end = audio_end(&mut file).map_err(|error| io_failed(&error))?;
        if end < total {
            format = probe(Box::new(Bounded { file, len: end }), &hint)
                .map_err(|error| read_error(&error))?;
        }
    }

    let track = format
        .default_track(TrackType::Audio)
        .ok_or_else(|| failed("no_audio_track", "the file has no audio track"))?
        .clone();
    let params = track
        .codec_params
        .as_ref()
        .and_then(|params| params.audio())
        .ok_or_else(|| failed("no_audio_track", "the audio track has no parameters"))?;
    let codec = codec_name(params.codec);
    let channels = params
        .channels
        .as_ref()
        .map(|channels| u32::try_from(channels.count()).unwrap_or(u32::MAX));

    // Only what the packets cannot be read without, and only from a source
    // that is the same in every container. PCM has nothing in its stream,
    // so its rate and channel count are hashed. FLAC takes rate, channels
    // and bit depth from STREAMINFO. Every other codec carries them in its
    // frames or in its configuration.
    let (rate, count, bits) = if codec.starts_with("pcm_") {
        (params.sample_rate.unwrap_or(0), channels.unwrap_or(0), 0)
    } else if codec == "flac" {
        (
            params.sample_rate.unwrap_or(0),
            channels.unwrap_or(0),
            params.bits_per_sample.unwrap_or(0),
        )
    } else {
        (0, 0, 0)
    };
    let extra: &[u8] = params.extra_data.as_deref().unwrap_or(&[]);
    // FLAC is deliberately absent: its extra data is STREAMINFO, which
    // also holds the MD5, the sample count and frame sizes that tools
    // rewrite.
    let hashed = matches!(codec.as_str(), "aac" | "alac" | "vorbis" | "opus")
        || codec.starts_with("symphonia:");
    let mut config: &[u8] = if hashed { extra } else { &[] };
    // CAF wraps the 24-byte ALACSpecificConfig in `frma` and `alac` atoms.
    if codec == "alac" && config.len() == 48 && config.get(4..8) == Some(b"frma".as_slice()) {
        config = config.get(24..).unwrap_or(config);
    }
    let length = |bytes: &[u8]| u32::try_from(bytes.len()).unwrap_or(u32::MAX).to_le_bytes();
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"ritornel-audio\x01");
    hasher.update(&length(codec.as_bytes()));
    hasher.update(codec.as_bytes());
    hasher.update(&rate.to_le_bytes());
    hasher.update(&count.to_le_bytes());
    hasher.update(&bits.to_le_bytes());
    hasher.update(&length(config));
    hasher.update(config);

    let (mut packets, mut payload_bytes, mut units) = (0_u64, 0_u64, 0_u64);
    let mut error = None;
    loop {
        let packet = match format.next_packet() {
            Ok(Some(packet)) => packet,
            Ok(None) => break,
            Err(failure) => {
                // A truncated file ends in an error. What was read is kept,
                // and its hash differs from the whole file's.
                error = Some(read_error(&failure));
                break;
            }
        };
        if packet.track_id != track.id {
            continue;
        }
        hasher.update(&packet.data);
        packets += 1;
        payload_bytes += packet.data.len() as u64;
        // The duration is that of the packets read: a header's sample
        // count can be stale.
        units += packet
            .dur
            .get()
            .saturating_sub(packet.trim_start.get())
            .saturating_sub(packet.trim_end.get());
    }
    if packets == 0 {
        // Two different files that yield no packets would hash the same.
        return Err(error.unwrap_or_else(|| failed("no_packets", "the audio track has no packets")));
    }
    let duration_ms = match (track.time_base, params.sample_rate) {
        (Some(base), _) => {
            u128::from(units) * u128::from(base.numer.get()) * 1000 / u128::from(base.denom.get())
        }
        (None, Some(rate)) if rate > 0 => u128::from(units) * 1000 / u128::from(rate),
        _ => 0,
    };
    let flac_md5 = match params.verification_check {
        Some(VerificationCheck::Md5(md5)) if md5 != [0; 16] => Some(hex(&md5)),
        _ => None,
    };
    payload.audio = Some(Audio {
        hash_scheme: HASH_SCHEME,
        hash: hasher.finalize().to_hex().to_string(),
        codec,
        sample_rate: params.sample_rate,
        channels,
        bits: params.bits_per_sample,
        duration_ms: u64::try_from(duration_ms).unwrap_or(u64::MAX),
        packets,
        payload_bytes,
        flac_md5,
    });
    match error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// Reads one file under a root and returns its payload. A file that cannot
/// be read as audio still has a payload, with the error in it; one read in
/// part has both the error and the hash of what was read.
pub(crate) fn read_file(
    root: &Path,
    rel_path: &str,
    size: u64,
    mtime_ms: i64,
    folder_images: Vec<FolderImage>,
) -> Payload {
    let mut payload = Payload {
        v: PAYLOAD_VERSION,
        path: rel_path.to_owned(),
        size,
        mtime_ms,
        container: None,
        audio: None,
        tags: Vec::new(),
        pictures: Vec::new(),
        folder_images,
        error: None,
    };
    payload.error = caught(&mut payload, |payload| read_audio(root, rel_path, payload)).err();
    if payload
        .error
        .as_ref()
        .is_some_and(|error| error.kind == UNAVAILABLE)
    {
        // What was read before the system said no is not the file's audio.
        payload.audio = None;
    }
    payload
}

/// Runs `read` on `payload`, and takes a panic in it, as a decoder may
/// have on a file it cannot make sense of, for a malformed file: the file is
/// recorded as unreadable, and the scan goes on to the next. What the read
/// had filled in is dropped.
fn caught(
    payload: &mut Payload,
    read: impl FnOnce(&mut Payload) -> Result<(), ReadError>,
) -> Result<(), ReadError> {
    let panicked = match std::panic::catch_unwind(AssertUnwindSafe(|| read(payload))) {
        Ok(result) => return result,
        Err(panicked) => panicked,
    };
    let why = panicked
        .downcast_ref::<&str>()
        .map(|why| (*why).to_owned())
        .or_else(|| panicked.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "no reason given".to_owned());
    payload.container = None;
    payload.audio = None;
    payload.tags.clear();
    payload.pictures.clear();
    Err(failed(
        "malformed",
        format!("the reader stopped on an error it did not expect: {why}"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reader_that_panics_leaves_the_file_unreadable_and_nothing_half_read() {
        let mut payload = read_file(
            &crate::media_fixtures::fixtures(),
            "a.flac",
            0,
            0,
            Vec::new(),
        );
        assert!(payload.audio.is_some() && payload.error.is_none());
        let panicked = caught(&mut payload, |_| panic!("an index out of bounds"));
        assert_eq!(
            panicked,
            Err(ReadError {
                kind: "malformed".to_owned(),
                message: "the reader stopped on an error it did not expect: an index out of bounds"
                    .to_owned(),
            })
        );
        assert!(payload.audio.is_none() && payload.tags.is_empty());
        assert_eq!(caught(&mut payload, |_| Ok(())), Ok(()));
        let malformed = failed("malformed", "no frames");
        assert_eq!(
            caught(&mut payload, |_| Err(malformed.clone())),
            Err(malformed)
        );
    }

    #[test]
    fn the_size_of_an_image_is_read_from_its_header() {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend([0, 0, 1, 0x2c, 0, 0, 0, 0xc8]);
        assert_eq!(image_size(&png), Some((300, 200)));
        let jpeg = [
            0xff, 0xd8, 0xff, 0xe0, 0, 4, 1, 2, 0xff, 0xc0, 0, 11, 8, 0, 0x40, 0, 0x80, 3,
        ];
        assert_eq!(image_size(&jpeg), Some((128, 64)));
        assert_eq!(image_size(b"GIF89a\x10\0\x08\0"), Some((16, 8)));
        assert_eq!(image_size(b"not an image"), None);
        assert_eq!(image_size(&[0xff, 0xd8, 0xff]), None);
    }

    #[test]
    fn a_failure_the_system_reports_is_not_the_files_fault_and_a_short_read_is() {
        let gone = std::io::Error::from_raw_os_error(6);
        assert_eq!(io_kind(&gone), UNAVAILABLE);
        assert_eq!(io_kind(&std::io::Error::from_raw_os_error(5)), UNAVAILABLE);
        let missing = std::io::Error::new(std::io::ErrorKind::NotFound, "gone");
        assert_eq!(io_kind(&missing), UNAVAILABLE);
        let short = std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "end of stream");
        assert_eq!(io_kind(&short), "io");
        assert_eq!(read_error(&Symphonia::IoError(gone)).kind, UNAVAILABLE);
        assert_eq!(read_error(&Symphonia::IoError(short)).kind, "io");

        // A file that is not there when it is read is unavailable, with
        // nothing of its audio kept.
        let payload = read_file(Path::new("/nonexistent-root"), "a.flac", 1, 1, Vec::new());
        assert_eq!(payload.error.unwrap().kind, UNAVAILABLE);
        assert_eq!(payload.audio, None);
    }

    #[test]
    fn a_folders_own_image_is_known_by_its_name() {
        for name in ["cover.jpg", "Folder.JPG", "front.png", "COVER.jpeg"] {
            assert!(is_folder_image(name), "{name}");
        }
        for name in [
            "back.jpg",
            "cover.txt",
            "cover",
            "discover.jpg",
            "cover.jpg.bak",
        ] {
            assert!(!is_folder_image(name), "{name}");
        }
    }
}
