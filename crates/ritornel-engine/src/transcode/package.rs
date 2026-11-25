//! Project tags and cover art onto a separate, seekable copy of the audio.

use lofty::config::WriteOptions;
use lofty::file::FileType;
use lofty::id3::v2::Id3v2Tag;
use lofty::picture::{MimeType, Picture, PictureType};
use lofty::prelude::{Accessor, ItemKey, TagExt};
use lofty::probe::Probe;
use lofty::tag::{Tag, TagType};
use serde::Serialize;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek};
use std::path::Path;

use crate::playback::FileFacts;

use super::{AudioAsset, CacheEntry, cache::Object};

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Codec {
    Mp3,
    Opus,
    Flac,
}

impl Codec {
    pub(super) fn extension(self) -> &'static str {
        match self {
            Self::Mp3 => "mp3",
            Self::Opus => "opus",
            Self::Flac => "flac",
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
}

#[derive(Debug, Clone, Copy)]
pub struct Artwork<'a> {
    pub bytes: &'a [u8],
    /// Longest side in pixels. Smaller originals keep their dimensions.
    pub max_side: u32,
}

#[derive(Debug)]
pub struct Package<'a> {
    pub tags: &'a Tags,
    /// A supplied cover must decode successfully for this package to be published.
    pub artwork: Option<Artwork<'a>>,
    /// Also write an ID3v1 trailer for older MP3 players.
    pub id3v1: bool,
}

/// Keep the pin on a derivative until the package call returns.
#[derive(Debug, Clone, Copy)]
pub enum PackageSource<'a> {
    Derivative(&'a CacheEntry),
    Master {
        asset: AudioAsset,
        path: &'a Path,
        facts: FileFacts,
        codec: Codec,
    },
}
impl PackageSource<'_> {
    pub(super) fn codec(self) -> Codec {
        match self {
            Self::Derivative(entry) => entry.codec(),
            Self::Master { codec, .. } => codec,
        }
    }
    pub(super) fn hash(self) -> blake3::Hash {
        match self {
            Self::Derivative(entry) => entry.key(),
            Self::Master { asset, .. } => asset.hash,
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
            Self::Derivative(entry) => entry.path(),
            Self::Master { path, .. } => path,
        }
    }
}

impl Package<'_> {
    pub fn key(&self, source: PackageSource<'_>) -> io::Result<blake3::Hash> {
        Ok(self.object(source)?.key)
    }

    pub(super) fn object(&self, source: PackageSource<'_>) -> io::Result<Object> {
        let scheme = match source {
            PackageSource::Master { asset, .. } => Some(asset.scheme),
            PackageSource::Derivative(_) => None,
        };
        self.object_for(
            source.hash(),
            scheme,
            source.codec(),
            self.artwork.map(|a| (blake3::hash(a.bytes), a.max_side)),
        )
    }

    pub(super) fn object_for(
        &self,
        hash: blake3::Hash,
        scheme: Option<u32>,
        codec: Codec,
        artwork: Option<(blake3::Hash, u32)>,
    ) -> io::Result<Object> {
        if self.id3v1 && codec != Codec::Mp3 {
            return Err(io::Error::other("ID3v1 requires MP3 audio"));
        }
        if artwork.is_some_and(|(_, max_side)| max_side == 0) {
            return Err(io::Error::other("cover size must be positive"));
        }
        let art = artwork.map(|(hash, max_side)| (hash.to_hex().to_string(), max_side));
        // Version the tag writer, JPEG settings and metadata projection together.
        let bytes = serde_json::to_vec(&(
            "ritornel-package-v1",
            scheme,
            hash.to_hex().as_str(),
            codec,
            self.tags,
            art,
            self.id3v1,
        ))?;
        Ok(Object::package(blake3::hash(&bytes), codec, hash, scheme))
    }

    pub(super) fn write(
        &self,
        source: PackageSource<'_>,
        staged: &Path,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> io::Result<()> {
        let mut input = File::open(source.path())?;
        source.verify(&input)?;
        let codec = source.codec();
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
            return Err(io::Error::other("package source does not match its codec"));
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
        if codec == Codec::Mp3 {
            // Conversion groups fields by frame ID in a hash map. The projection
            // has at most one frame per ID, so sorting makes rebuilds byte-stable.
            let mut frames: Vec<_> = Id3v2Tag::from(tag).into_iter().collect();
            frames.sort_unstable_by(|a, b| a.id().as_str().cmp(b.id().as_str()));
            let mut ordered = Id3v2Tag::new();
            for frame in frames {
                ordered.insert(frame);
            }
            ordered.save_to(&mut output, options.remove_others(true))
        } else {
            tag.save_to(&mut output, options.remove_others(true))
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
