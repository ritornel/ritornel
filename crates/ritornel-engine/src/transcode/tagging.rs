//! A tagged copy: the projected tags and cover written onto a separate,
//! seekable copy of the audio. One definition serves every target; what a
//! target sets is the cover's size and whether an MP3 gets an ID3v1
//! trailer. A copy is made for one transfer and never cached, and the same
//! inputs make the same bytes every time.

use lofty::TextEncoding;
use lofty::config::WriteOptions;
use lofty::file::FileType;
use lofty::id3::v2::{ExtendedTextFrame, Frame, Id3v2Tag};
use lofty::ogg::tag::VorbisComments;
use lofty::picture::{MimeType, Picture, PictureType};
use lofty::prelude::{Accessor, ItemKey, TagExt};
use lofty::probe::Probe;
use lofty::tag::{Tag, TagType};
use serde::Serialize;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek};
use std::path::Path;

use crate::playback::FileFacts;

use super::{AudioAsset, CacheEntry, GainTag, GainTags};

/// The field a tagged copy carries Ritornel's own identifier in: a Vorbis
/// comment of this name, or a `TXXX` frame of this description.
pub const RITORNEL_ID: &str = "RITORNEL_ID";

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Codec {
    Mp3,
    Opus,
    Flac,
}

impl Codec {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Mp3 => "mp3",
            Self::Opus => "opus",
            Self::Flac => "flac",
        }
    }

    /// The codec the scan names so, if a tagged copy can be made of it.
    pub fn named(codec: &str) -> Option<Self> {
        match codec {
            "mp3" => Some(Self::Mp3),
            "opus" => Some(Self::Opus),
            "flac" => Some(Self::Flac),
            _ => None,
        }
    }
}

/// The display metadata projected by the caller. Numbers use their native
/// tag fields where the format has them.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Tags {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub track: Option<u32>,
    pub disc: Option<u32>,
    pub year: Option<u16>,
    pub genre: Option<String>,
    /// The master's ReplayGain figures, written as tags for the player to
    /// apply: Vorbis comments, or `TXXX` frames in an MP3 and never APEv2.
    /// A copy whose audio already has gain applied carries none.
    #[serde(skip)]
    pub gain: GainTags,
    /// Ritornel's own identifier for the file, as [`RITORNEL_ID`]. A
    /// recovery hint for finding a file again after a rename: no plan,
    /// check or readback uses it.
    #[serde(skip)]
    pub ritornel_id: Option<String>,
}

/// A gain in decibels as taggers write it.
fn gain_text(db: f32) -> String {
    format!("{db:.2} dB")
}

/// A peak as taggers write it.
fn peak_text(peak: f32) -> String {
    format!("{peak:.6}")
}

impl Tags {
    fn tag(&self, kind: TagType) -> Tag {
        let mut tag = Tag::new(kind);
        for (key, value) in [
            (ItemKey::TrackTitle, &self.title),
            (ItemKey::TrackArtist, &self.artist),
            (ItemKey::AlbumTitle, &self.album),
            (ItemKey::AlbumArtist, &self.album_artist),
            (ItemKey::Genre, &self.genre),
        ] {
            if let Some(value) = value {
                tag.insert_text(key, value.clone());
            }
        }
        if let Some(n) = self.track {
            tag.set_track(n);
        }
        if let Some(n) = self.disc {
            tag.set_disk(n);
        }
        if let Some(year) = self.year {
            tag.insert_text(ItemKey::RecordingDate, year.to_string());
        }
        tag
    }

    /// The ReplayGain fields and Ritornel's identifier, by the names both
    /// Vorbis comments and `TXXX` descriptions give them, in a fixed order.
    fn named(&self) -> Vec<(&'static str, String)> {
        let pair = |tag: GainTag, gain: &'static str, peak: &'static str| {
            [
                tag.gain_db.map(|db| (gain, gain_text(db))),
                tag.peak.map(|value| (peak, peak_text(value))),
            ]
        };
        pair(
            self.gain.album,
            "REPLAYGAIN_ALBUM_GAIN",
            "REPLAYGAIN_ALBUM_PEAK",
        )
        .into_iter()
        .chain(pair(
            self.gain.track,
            "REPLAYGAIN_TRACK_GAIN",
            "REPLAYGAIN_TRACK_PEAK",
        ))
        .flatten()
        .chain(
            self.ritornel_id
                .as_ref()
                .map(|id| (RITORNEL_ID, id.clone())),
        )
        .collect()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Artwork<'a> {
    pub bytes: &'a [u8],
    /// Longest side in pixels. Smaller originals keep their dimensions.
    pub max_side: u32,
}

/// What is written onto a copy of the audio.
#[derive(Debug)]
pub struct TaggedCopy<'a> {
    pub tags: &'a Tags,
    /// A supplied cover must decode successfully for the copy to be made.
    /// It is embedded as a baseline JPEG, never a progressive one.
    pub artwork: Option<Artwork<'a>>,
    /// Also write an ID3v1 trailer for older MP3 players.
    pub id3v1: bool,
}

/// The audio a tagged copy is made of. Keep the pin on an encode until the
/// copy has been written.
#[derive(Debug, Clone, Copy)]
pub enum CopySource<'a> {
    Encode(&'a CacheEntry),
    Master {
        asset: AudioAsset,
        path: &'a Path,
        facts: FileFacts,
        codec: Codec,
    },
}

impl CopySource<'_> {
    pub(super) fn codec(self) -> Codec {
        match self {
            Self::Encode(entry) => entry.codec(),
            Self::Master { codec, .. } => codec,
        }
    }
    fn verify(self, input: &File) -> io::Result<()> {
        if let Self::Master { facts, .. } = self
            && FileFacts::of(&input.metadata()?) != facts
        {
            return Err(io::Error::other("master changed since the scan"));
        }
        Ok(())
    }
    fn path(&self) -> &Path {
        match self {
            Self::Encode(entry) => entry.path(),
            Self::Master { path, .. } => path,
        }
    }
}

impl TaggedCopy<'_> {
    pub(super) fn check(&self, codec: Codec) -> io::Result<()> {
        if self.id3v1 && codec != Codec::Mp3 {
            return Err(io::Error::other("ID3v1 requires MP3 audio"));
        }
        if self.artwork.is_some_and(|art| art.max_side == 0) {
            return Err(io::Error::other("cover size must be positive"));
        }
        Ok(())
    }

    /// Writes the copy to a new file at `staged`.
    pub fn write(
        &self,
        source: CopySource<'_>,
        staged: &Path,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> io::Result<()> {
        let codec = source.codec();
        self.check(codec)?;
        let mut input = File::open(source.path())?;
        source.verify(&input)?;
        let expected = match codec {
            Codec::Mp3 => FileType::Mpeg,
            Codec::Opus => FileType::Opus,
            Codec::Flac => FileType::Flac,
        };
        if Probe::new(std::io::BufReader::new(&mut input))
            .guess_file_type()?
            .file_type()
            != Some(expected)
        {
            return Err(io::Error::other("the audio does not match its codec"));
        }
        input.rewind()?;
        let mut output = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(staged)?;
        // Trailing ID3v1, APE or lyrics may exist even on FLAC masters.
        // The projection owns these tags. Copy only through the audio end.
        let end = match codec {
            Codec::Mp3 | Codec::Flac => crate::media::audio_end(&mut input)?,
            Codec::Opus => input.metadata()?.len(),
        };
        super::copy(&mut (&mut input).take(end), &mut output, cancel)?;
        source.verify(&input)?;
        output.rewind()?;
        let mut tag = self.tags.tag(expected.primary_tag_type());
        if let Some(art) = self.artwork {
            let decoded = image::load_from_memory(art.bytes).map_err(io::Error::other)?;
            let jpeg = crate::covers::scale(&decoded, art.max_side).map_err(io::Error::other)?;
            tag.push_picture(
                Picture::unchecked(jpeg)
                    .pic_type(PictureType::CoverFront)
                    .mime_type(MimeType::Jpeg)
                    .build(),
            );
        }
        super::check_cancel(cancel)?;
        let options = WriteOptions::new()
            .use_id3v23(true)
            .respect_read_only(false);
        let named = self.tags.named();
        if codec == Codec::Mp3 {
            // Conversion groups fields by frame ID in a hash map, and the
            // ReplayGain figures and Ritornel's identifier are several
            // `TXXX` frames. Ordering by frame ID and then by description
            // makes a rebuild the same bytes.
            let mut frames: Vec<_> = Id3v2Tag::from(tag).into_iter().collect();
            // Plain figures and IDs, in the encoding every player reads.
            frames.extend(named.into_iter().map(|(description, value)| {
                Frame::UserText(ExtendedTextFrame::new(
                    TextEncoding::Latin1,
                    description.to_owned(),
                    value,
                ))
            }));
            let described = |frame: &Frame<'_>| match frame {
                Frame::UserText(text) => text.description.to_string(),
                _ => String::new(),
            };
            frames.sort_by(|a, b| {
                (a.id().as_str(), described(a)).cmp(&(b.id().as_str(), described(b)))
            });
            let mut ordered = Id3v2Tag::new();
            for frame in frames {
                ordered.insert(frame);
            }
            ordered.save_to(&mut output, options.remove_others(true))
        } else {
            let mut comments = VorbisComments::from(tag);
            for (key, value) in named {
                comments.push(key.to_owned(), value);
            }
            comments.save_to(&mut output, options.remove_others(true))
        }
        .map_err(io::Error::other)?;
        if self.id3v1 {
            output.rewind()?;
            self.tags
                .tag(TagType::Id3v1)
                .save_to(&mut output, options)
                .map_err(io::Error::other)?;
        }
        Ok(())
    }
}
