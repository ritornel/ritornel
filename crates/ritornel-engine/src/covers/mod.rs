//! Covers: a picture's bytes read from where they are, scaled to a served
//! size, and kept in the cache. See decision 18 and "Media" in
//! docs/design/contract.md, and "The data directory" in
//! docs/design/server.md.
//!
//! `library.db` keeps a picture as the hash of its bytes, and where it came
//! with: embedded in a file, or a folder image beside one. The bytes stay
//! there, and are read with the scan's own reader. [`Covers::render`] finds a source that still has
//! them, decodes them, scales them to fit the size asked for, writes the
//! result as a JPEG in `cache/tmp/`, syncs it and renames it into
//! `cache/covers/`, where [`Covers::cached`] finds it next time. A cover
//! whose files are all gone is gone too.
//!
//! Every step here waits on a disk or works the processor, so none of it
//! runs on an async worker. The server runs it on threads of its own.

use std::fmt;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use ritornel_contract::{ART_SIZES, Id};
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

/// Where a picture's bytes may be read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    /// The file the picture came with: its root, by name, and its path
    /// in it.
    pub root: String,
    /// The path of the audio file, relative to the root.
    pub path: String,
    /// A folder image's name beside the file, or `None` for a picture
    /// embedded in it.
    pub folder_image: Option<String>,
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
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let tmp = self.tmp.join(format!(
            "{art}-{size}-{}-{}.part",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let failed =
            |error: std::io::Error| CoverError::Failed(format!("the cover cache: {error}"));
        let mut file = std::fs::File::create(&tmp).map_err(failed)?;
        file.write_all(bytes).map_err(failed)?;
        file.sync_all().map_err(failed)?;
        drop(file);
        std::fs::rename(&tmp, self.path(art, size)).map_err(|error| {
            let _ = std::fs::remove_file(&tmp);
            failed(error)
        })
    }
}

fn micros(since: std::time::Instant) -> u64 {
    u64::try_from(since.elapsed().as_micros()).unwrap_or(u64::MAX)
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
    let file = Path::new(root).join(&source.path);
    match &source.folder_image {
        Some(name) => {
            let folder = file.parent()?;
            std::fs::read(folder.join(name))
                .ok()
                .filter(|bytes| blake3::hash(bytes).as_bytes().as_slice() == source.hash)
        }
        None => crate::media::embedded_picture(&file, &source.hash),
    }
}

impl LibraryStore {
    /// Where the picture `art` may be read from, best first: the files it
    /// came with that are still there, folder images before embedded ones,
    /// by `revision_picture_picture`.
    pub fn picture_sources(&self, art: Id) -> Result<Vec<Source>, Error> {
        self.read(move |connection: &Connection| {
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
            let mut sources = Vec::new();
            while let Some(row) = rows.next()? {
                sources.push(Source {
                    root: row.get(0)?,
                    path: row.get(1)?,
                    folder_image: row.get(2)?,
                    hash: row.get(3)?,
                });
            }
            Ok(sources)
        })
    }
}

#[cfg(test)]
mod tests;
