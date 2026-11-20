//! Covers: a picture's bytes read from where they are, scaled to a served
//! size, and kept in the cache. See decision 18 and "Media" in
//! docs/design/contract.md, and "The data directory" in
//! docs/design/server.md.
//!
//! `library.db` keeps a picture as the hash of its bytes, and where it came
//! with: embedded in a file, a folder image beside one, or a file of a
//! picture folder (an artist's portrait). The bytes stay there, and are
//! read with the scan's own reader. [`Covers::render`] finds a source that still has
//! them, decodes them, scales them to fit the size asked for, writes the
//! result as a JPEG in `cache/tmp/`, syncs it and renames it into
//! `cache/covers/`, where [`Covers::cached`] finds it next time. A cover
//! whose files are all gone is gone too.
//!
//! Every step here waits on a disk or works the processor, so none of it
//! runs on an async worker. The server runs it on threads of its own.
//!
//! A cover's colour ([`Covers::color`]) is worked out from the same decoded
//! picture, when a cover is first made, and kept beside the covers as
//! `{artId}.color`: the hue the picture has most of, the way the
//! prototype's `scripts/colors.py` finds it. A page asks for it with the
//! artist or the album it draws, before any cover of it may have been
//! made, and then it is worked out there and kept the same way.

use std::fmt;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use ritornel_contract::{ART_SIZES, ArtColor, Id};
use rusqlite::Connection;

use crate::{Error, LibraryStore, ServerStore};

/// The quality served covers are written at.
const QUALITY: u8 = 85;

/// Why a cover could not be served.
#[derive(Debug)]
pub enum CoverError {
    /// No picture has that ID, or none of the files it came with still has
    /// it.
    NotFound,
    /// The size is not one of [`ART_SIZES`].
    BadSize,
    /// Reading the library failed.
    Engine(Error),
    /// The picture's bytes could not be read as an image, or the cache
    /// could not be written.
    Failed(String),
}

impl fmt::Display for CoverError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound => f.write_str("no such cover"),
            Self::BadSize => f.write_str("not a size covers are served at"),
            Self::Engine(error) => error.fmt(f),
            Self::Failed(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for CoverError {}

impl From<Error> for CoverError {
    fn from(error: Error) -> Self {
        Self::Engine(error)
    }
}

/// Where in a file a picture is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Place {
    /// Embedded in the audio file.
    Embedded,
    /// A folder image of this name beside the audio file.
    FolderImage(String),
    /// The file is the picture: a portrait in a picture folder.
    Whole,
}

/// Where a picture's bytes may be read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    /// The file the picture came with: its root, by name, and its path
    /// in it. A picture folder is a root of its own.
    pub root: String,
    /// The path of the file, relative to the root.
    pub path: String,
    /// Where in the file the picture is.
    pub place: Place,
    /// The hash the scan recorded the picture by: what was read must
    /// have it.
    pub hash: Vec<u8>,
}

/// How long each step of rendering a cover took, for the server's log and
/// for measuring.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Timings {
    /// Finding where the picture is.
    pub lookup_us: u64,
    /// Reading its bytes.
    pub read_us: u64,
    /// Decoding them.
    pub decode_us: u64,
    /// Scaling and writing the JPEG.
    pub resize_us: u64,
    /// Writing the cache file.
    pub store_us: u64,
}

/// A cover ready to send.
#[derive(Debug)]
pub struct Rendered {
    /// The JPEG.
    pub bytes: Vec<u8>,
    /// What it cost.
    pub timings: Timings,
}

/// The cover cache: `covers/` and `tmp/` under the cache root.
#[derive(Debug)]
pub struct Covers {
    covers: PathBuf,
    tmp: PathBuf,
}

impl Covers {
    /// The cache under `cache_root`, made if it is missing. What is left in
    /// `tmp/` from a run that stopped part-way is swept.
    pub fn new(cache_root: &Path) -> Result<Self, CoverError> {
        let covers = cache_root.join("covers");
        let tmp = cache_root.join("tmp");
        for dir in [&covers, &tmp] {
            std::fs::create_dir_all(dir)
                .map_err(|error| CoverError::Failed(format!("{}: {error}", dir.display())))?;
        }
        if let Ok(left) = std::fs::read_dir(&tmp) {
            for entry in left.flatten() {
                let _ = std::fs::remove_file(entry.path());
            }
        }
        Ok(Self { covers, tmp })
    }

    /// Where the cover `art` at `size` is kept, whether or not it is there.
    pub fn path(&self, art: Id, size: u32) -> PathBuf {
        self.covers.join(format!("{art}-{size}.jpg"))
    }

    /// The cover `art` at `size`, if the cache has it.
    pub fn cached(&self, art: Id, size: u32) -> Option<Vec<u8>> {
        std::fs::read(self.path(art, size)).ok()
    }

    /// Where the colour of `art` is kept, whether or not it is there.
    fn color_path(&self, art: Id) -> PathBuf {
        self.covers.join(format!("{art}.color"))
    }

    /// The colour of `art`, if the cache has worked it out: `Some(None)`
    /// for a picture that is close to grey.
    pub fn cached_color(&self, art: Id) -> Option<Option<ArtColor>> {
        let text = std::fs::read_to_string(self.color_path(art)).ok()?;
        let mut parts = text.split_whitespace();
        match (parts.next(), parts.next()) {
            (Some("none"), None) => Some(None),
            (Some(hue), Some(saturation)) => Some(Some(ArtColor {
                hue: hue.parse().ok()?,
                saturation: saturation.parse().ok()?,
            })),
            _ => None,
        }
    }

    /// The colour of `art`: from the cache, or else worked out from the
    /// smallest cover of it the cache has, or from its source, and kept.
    /// `Ok(None)` for a picture that is close to grey all over.
    pub fn color(
        &self,
        library: &LibraryStore,
        servers: &ServerStore,
        art: Id,
    ) -> Result<Option<ArtColor>, CoverError> {
        if let Some(known) = self.cached_color(art) {
            return Ok(known);
        }
        // A cover already made is a smaller picture to decode than its
        // source. A colour is taken from a few pixels, so any will do.
        let made = ART_SIZES.iter().find_map(|size| self.cached(art, *size));
        let bytes = match made {
            Some(bytes) => bytes,
            None => {
                let sources = library.picture_sources(art)?;
                let roots = servers.roots()?;
                sources
                    .iter()
                    .find_map(|source| read_source(&roots, source))
                    .ok_or(CoverError::NotFound)?
            }
        };
        let decoded = image::load_from_memory(&bytes).map_err(|error| {
            CoverError::Failed(format!("the picture could not be decoded: {error}"))
        })?;
        let color = color_of(&decoded);
        self.store_color(art, color);
        Ok(color)
    }

    /// Keeps a colour worked out. A cache that cannot be written loses
    /// nothing: the colour is worked out again next time.
    fn store_color(&self, art: Id, color: Option<ArtColor>) {
        let text = match color {
            Some(color) => format!("{} {}", color.hue, color.saturation),
            None => "none".to_owned(),
        };
        let _ = self.store_file(&self.color_path(art), text.as_bytes());
    }

    /// Makes the cover `art` at `size` from its source, keeps it in the
    /// cache, and returns it.
    pub fn render(
        &self,
        library: &LibraryStore,
        servers: &ServerStore,
        art: Id,
        size: u32,
    ) -> Result<Rendered, CoverError> {
        if !ART_SIZES.contains(&size) {
            return Err(CoverError::BadSize);
        }
        let mut timings = Timings::default();
        let started = std::time::Instant::now();
        let sources = library.picture_sources(art)?;
        timings.lookup_us = micros(started);
        if sources.is_empty() {
            return Err(CoverError::NotFound);
        }
        let started = std::time::Instant::now();
        let roots = servers.roots()?;
        let Some(original) = sources
            .iter()
            .find_map(|source| read_source(&roots, source))
        else {
            return Err(CoverError::NotFound);
        };
        timings.read_us = micros(started);
        let started = std::time::Instant::now();
        let decoded = image::load_from_memory(&original).map_err(|error| {
            CoverError::Failed(format!("the picture could not be decoded: {error}"))
        })?;
        timings.decode_us = micros(started);
        // The colour, from the picture decoded here, the first time.
        if self.cached_color(art).is_none() {
            self.store_color(art, color_of(&decoded));
        }
        let started = std::time::Instant::now();
        let bytes = scale(&decoded, size)?;
        timings.resize_us = micros(started);
        let started = std::time::Instant::now();
        self.store(art, size, &bytes)?;
        timings.store_us = micros(started);
        Ok(Rendered { bytes, timings })
    }

    /// Writes a cover in `tmp/`, syncs it, and renames it into place.
    fn store(&self, art: Id, size: u32, bytes: &[u8]) -> Result<(), CoverError> {
        self.store_file(&self.path(art, size), bytes)
    }

    /// Writes a file of the cache in `tmp/`, syncs it, and renames it to
    /// `path`.
    fn store_file(&self, path: &Path, bytes: &[u8]) -> Result<(), CoverError> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let tmp = self.tmp.join(format!(
            "{name}-{}-{}.part",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let failed =
            |error: std::io::Error| CoverError::Failed(format!("the cover cache: {error}"));
        let mut file = std::fs::File::create(&tmp).map_err(failed)?;
        file.write_all(bytes).map_err(failed)?;
        file.sync_all().map_err(failed)?;
        drop(file);
        std::fs::rename(&tmp, path).map_err(|error| {
            let _ = std::fs::remove_file(&tmp);
            failed(error)
        })
    }
}

fn micros(since: std::time::Instant) -> u64 {
    u64::try_from(since.elapsed().as_micros()).unwrap_or(u64::MAX)
}

/// The side of the square a picture is shrunk to before its colour is
/// taken: the prototype's.
const COLOR_SIDE: u32 = 24;

/// The colour a picture is mostly made of, as the prototype's
/// `scripts/colors.py` finds it: shrunk to 24 pixels square, each pixel
/// counts by how coloured it is (grey, near black and near white count for
/// nothing), and the hue is the middle of the twelfth of the colour wheel
/// that has the most, with the colour's strength there. `None` for a
/// picture that is close to grey all over.
pub(crate) fn color_of(decoded: &image::DynamicImage) -> Option<ArtColor> {
    let small = decoded.thumbnail_exact(COLOR_SIDE, COLOR_SIDE).to_rgb8();
    color_of_pixels(small.pixels().map(|pixel| pixel.0))
}

/// [`color_of`], from pixels already shrunk.
pub(crate) fn color_of_pixels(pixels: impl Iterator<Item = [u8; 3]>) -> Option<ArtColor> {
    // For each twelfth: its weight, and the weighted sums of the hue's sine
    // and cosine and of the saturation.
    let mut buckets = [[0.0_f64; 4]; 12];
    let (mut total, mut count) = (0.0_f64, 0_u32);
    for [r, g, b] in pixels {
        count += 1;
        let (hue, lightness, saturation) = hls(
            f64::from(r) / 255.0,
            f64::from(g) / 255.0,
            f64::from(b) / 255.0,
        );
        let weight = saturation * (1.0 - (2.0 * lightness - 1.0).abs());
        if weight < 0.04 {
            continue;
        }
        let twelfth = ((hue * 12.0) as usize) % 12;
        let angle = hue * std::f64::consts::TAU;
        if let Some(bucket) = buckets.get_mut(twelfth) {
            bucket[0] += weight;
            bucket[1] += weight * angle.sin();
            bucket[2] += weight * angle.cos();
            bucket[3] += weight * saturation;
        }
        total += weight;
    }
    let best = buckets
        .iter()
        .copied()
        .max_by(|a, b| a[0].total_cmp(&b[0]))?;
    if count == 0 || total / f64::from(count) < 0.018 || best[0] <= 0.0 {
        return None;
    }
    let hue = (best[1].atan2(best[2]).to_degrees() + 360.0) % 360.0;
    Some(ArtColor {
        hue: (hue.round() as u16) % 360,
        saturation: (100.0 * best[3] / best[0]).round().clamp(0.0, 100.0) as u8,
    })
}

/// Hue, lightness and saturation, each from 0 to 1, as Python's
/// `colorsys.rgb_to_hls` gives them.
fn hls(r: f64, g: f64, b: f64) -> (f64, f64, f64) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let lightness = (max + min) / 2.0;
    if max == min {
        return (0.0, lightness, 0.0);
    }
    let range = max - min;
    let saturation = if lightness <= 0.5 {
        range / (max + min)
    } else {
        range / (2.0 - max - min)
    };
    let (rc, gc, bc) = ((max - r) / range, (max - g) / range, (max - b) / range);
    let hue = if r == max {
        bc - gc
    } else if g == max {
        2.0 + rc - bc
    } else {
        4.0 + gc - rc
    };
    ((hue / 6.0).rem_euclid(1.0), lightness, saturation)
}

/// The image scaled to fit `size` on its longer side, never up, as a JPEG.
fn scale(decoded: &image::DynamicImage, size: u32) -> Result<Vec<u8>, CoverError> {
    let longer = decoded.width().max(decoded.height());
    let scaled = if longer > size {
        // Triangle is a bilinear filter: plenty for scaling down a cover,
        // and several times quicker than Lanczos.
        decoded.resize(size, size, FilterType::Triangle)
    } else {
        decoded.clone()
    };
    let rgb = scaled.to_rgb8();
    let mut jpeg = Vec::new();
    JpegEncoder::new_with_quality(&mut jpeg, QUALITY)
        .encode_image(&rgb)
        .map_err(|error| CoverError::Failed(format!("the cover could not be written: {error}")))?;
    Ok(jpeg)
}

/// The picture's bytes from one source, if that source still has them.
/// `roots` are the roots this server has a path for.
fn read_source(roots: &[(String, String)], source: &Source) -> Option<Vec<u8>> {
    let (_, root) = roots.iter().find(|(name, _)| *name == source.root)?;
    let file = crate::walk::under(Path::new(root), &source.path);
    let same = |bytes: &Vec<u8>| blake3::hash(bytes).as_bytes().as_slice() == source.hash;
    match &source.place {
        Place::FolderImage(name) => std::fs::read(file.parent()?.join(name)).ok().filter(same),
        // A portrait is read as it was imported: inside its folder, a
        // regular file of bounded size.
        Place::Whole => crate::artist_pictures::read_inside(
            Path::new(root),
            &source.path,
            crate::artist_pictures::PORTRAIT_CAP,
        )
        .ok()
        .filter(same),
        Place::Embedded => crate::media::embedded_picture(&file, &source.hash),
    }
}

impl LibraryStore {
    /// Where the picture `art` may be read from, best first: an artist's
    /// portrait in a picture folder, by `artist_picture_picture`, then the
    /// files it came with that are still there, folder images before
    /// embedded ones, by `revision_picture_picture`.
    pub fn picture_sources(&self, art: Id) -> Result<Vec<Source>, Error> {
        self.read(move |connection: &Connection| {
            let mut sources = Vec::new();
            let mut statement = connection.prepare(
                "SELECT f.name, ap.path, p.content_hash
                 FROM picture p
                 JOIN artist_picture ap ON ap.picture_id = p.id
                 JOIN picture_folder f ON f.id = ap.folder_id
                 WHERE p.public_id = ?1
                 ORDER BY ap.id
                 LIMIT 8",
            )?;
            let mut rows = statement.query([art.as_bytes()])?;
            while let Some(row) = rows.next()? {
                sources.push(Source {
                    root: row.get(0)?,
                    path: row.get(1)?,
                    place: Place::Whole,
                    hash: row.get(2)?,
                });
            }
            let mut statement = connection.prepare(
                "SELECT lr.name, si.provider_key, rp.file_name, p.content_hash
                 FROM picture p
                 JOIN revision_picture rp ON rp.picture_id = p.id
                 JOIN source_item_revision sr ON sr.id = rp.revision_id
                 JOIN source_item si ON si.id = sr.source_item_id
                 JOIN source_instance src ON src.id = si.source_instance_id
                 JOIN library_root lr ON lr.id = src.root_id
                 WHERE p.public_id = ?1
                   AND si.retired_at_ms IS NULL
                   AND si.missing_since_ms IS NULL
                 ORDER BY rp.source_kind DESC, rp.id
                 LIMIT 8",
            )?;
            let mut rows = statement.query([art.as_bytes()])?;
            while let Some(row) = rows.next()? {
                let folder_image: Option<String> = row.get(2)?;
                sources.push(Source {
                    root: row.get(0)?,
                    path: row.get(1)?,
                    place: folder_image.map_or(Place::Embedded, Place::FolderImage),
                    hash: row.get(3)?,
                });
            }
            Ok(sources)
        })
    }
}

#[cfg(test)]
mod tests;
