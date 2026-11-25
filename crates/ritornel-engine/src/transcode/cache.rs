//! Two layers of immutable audio files. Publication, access and eviction
//! share a lock. File I/O runs outside SQLite transactions.

use super::{AudioAsset, Codec, EncoderId, GainTags, Package, PackageSource, Profile, Target};
use crate::{Error, Id, ServerStore};
use rusqlite::{OptionalExtension, Transaction, params};
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError, atomic::AtomicBool};

#[derive(Debug, Clone, Copy)]
pub enum PinOwner {
    SyncBatch,
    HttpTransfer,
    AbSession,
}

/// One run-scoped consumer can hold several files under the same id.
#[derive(Debug, Clone, Copy)]
pub struct Pin {
    pub id: Id,
    pub owner: PinOwner,
}

#[derive(Debug)]
pub struct Derivative<'a> {
    pub asset: AudioAsset,
    pub encoder: &'a EncoderId,
    pub profile: Profile,
    pub gain_tags: GainTags,
}

impl Derivative<'_> {
    pub(super) fn object(&self) -> Result<Object, Error> {
        let codec = match self.profile.target {
            Target::Mp3 { .. } => Codec::Mp3,
            Target::Opus { .. } => Codec::Opus,
            Target::Flac {} => {
                return Err(Error::Invalid("FLAC pass-through uses the master directly"));
            }
        };
        let key = self
            .encoder
            .derivative_key(self.asset, self.profile, self.gain_tags)
            .map_err(Error::Io)?;
        Ok(Object {
            key,
            layer: Layer::Derivative,
            codec,
            source: None,
            master_scheme: None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layer {
    Derivative,
    Packaged,
}
impl Layer {
    fn name(self) -> &'static str {
        match self {
            Self::Derivative => "derivative",
            Self::Packaged => "packaged",
        }
    }
    fn directory(self) -> &'static str {
        match self {
            Self::Derivative => "transcodes",
            Self::Packaged => "packaged",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct Object {
    pub(super) key: blake3::Hash,
    layer: Layer,
    pub(super) codec: Codec,
    source: Option<blake3::Hash>,
    master_scheme: Option<u32>,
}

impl Object {
    pub(super) fn package(
        key: blake3::Hash,
        codec: Codec,
        source: blake3::Hash,
        master_scheme: Option<u32>,
    ) -> Self {
        Self {
            key,
            layer: Layer::Packaged,
            codec,
            source: Some(source),
            master_scheme,
        }
    }

    fn path(self, root: &Path) -> PathBuf {
        let key = self.key.to_hex();
        root.join(self.layer.directory())
            .join(key.as_str().get(..2).unwrap_or_default())
            .join(format!("{key}.{}", self.codec.extension()))
    }
}

#[derive(Debug, Clone)]
pub struct CacheEntry {
    object: Object,
    path: PathBuf,
    pub size_bytes: u64,
    pub file_hash: blake3::Hash,
    pub sampled_hash: blake3::Hash,
}
impl CacheEntry {
    pub fn key(&self) -> blake3::Hash {
        self.object.key
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn codec(&self) -> Codec {
        self.object.codec
    }
    pub fn is_derivative(&self) -> bool {
        self.object.layer == Layer::Derivative
    }
}

#[derive(Debug)]
pub struct TranscodeCache {
    root: PathBuf,
    boot_id: Id,
    budget: u64,
    publication: Mutex<()>,
}

impl TranscodeCache {
    pub const DEFAULT_BUDGET: u64 = 10 * 1024 * 1024 * 1024;

    /// Open once while the server owns the data directory, before workers
    /// start. Reconcile either side of a filesystem/SQLite commit interrupted
    /// by a crash, including a restored index with absent files.
    pub fn open(store: &ServerStore, root: &Path, boot_id: Id, budget: u64) -> Result<Self, Error> {
        ensure_directory(root)?;
        for name in ["tmp", "transcodes", "packaged"] {
            ensure_directory(&root.join(name))?;
        }
        for entry in fs::read_dir(root.join("tmp")).map_err(Error::Io)? {
            let entry = entry.map_err(Error::Io)?;
            if entry.file_type().map_err(Error::Io)?.is_dir()
                && entry.path().extension().is_some_and(|ext| ext == "part")
                && entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("transcode-")
            {
                fs::remove_dir_all(entry.path()).map_err(Error::Io)?;
            }
        }
        let cache = Self {
            root: root.into(),
            boot_id,
            budget,
            publication: Mutex::new(()),
        };
        store.transact(move |tx, _, _| {
            tx.execute(
                "DELETE FROM transcode_pin WHERE boot_id != ?1",
                [boot_id.as_bytes().as_slice()],
            )?;
            Ok(())
        })?;
        let rows = store.read(|db| {
            let mut stmt =
                db.prepare("SELECT id,cache_key,layer,codec,size_bytes FROM transcode_entry")?;
            Ok(stmt
                .query_map([], IndexedFile::read)?
                .collect::<Result<Vec<_>, _>>()?)
        })?;
        let mut kept = HashSet::new();
        let mut missing = Vec::new();
        for row in rows {
            let path = row.path(root)?;
            if is_present(&path, row.size)? {
                kept.insert(path);
            } else {
                missing.push(row.id);
            }
        }
        store.transact(move |tx, _, _| {
            for id in missing {
                tx.execute("DELETE FROM transcode_entry WHERE id=?1", [id])?;
            }
            Ok(())
        })?;
        // Only descend into the two cache-owned trees, never through a link.
        for layer in [Layer::Derivative, Layer::Packaged] {
            for bucket in fs::read_dir(root.join(layer.directory())).map_err(Error::Io)? {
                let bucket = bucket.map_err(Error::Io)?;
                if !bucket.file_type().map_err(Error::Io)?.is_dir() {
                    continue;
                }
                for entry in fs::read_dir(bucket.path()).map_err(Error::Io)? {
                    let entry = entry.map_err(Error::Io)?;
                    if entry.file_type().map_err(Error::Io)?.is_file()
                        && is_object_path(&entry.path())
                        && !kept.contains(&entry.path())
                    {
                        fs::remove_file(entry.path()).map_err(Error::Io)?;
                    }
                }
            }
        }
        cache.prune_to(store, budget)?;
        Ok(cache)
    }

    /// The producer writes a new file at the supplied path. On a hit it
    /// is never called. A concurrent winner's bytes take precedence.
    pub fn derivative(
        &self,
        store: &ServerStore,
        spec: Derivative<'_>,
        pin: Pin,
        produce: impl FnOnce(&Path, blake3::Hash) -> io::Result<()>,
    ) -> Result<CacheEntry, Error> {
        let object = spec.object()?;
        self.ensure(store, object, pin, &AtomicBool::new(false), |path| {
            produce(path, object.key)
        })
    }

    pub fn package(
        &self,
        store: &ServerStore,
        source: PackageSource<'_>,
        package: &Package<'_>,
        pin: Pin,
    ) -> Result<CacheEntry, Error> {
        if let PackageSource::Derivative(entry) = source
            && !entry.is_derivative()
        {
            return Err(Error::Invalid(
                "a packaged file cannot be a derivative source",
            ));
        }
        let object = package.object(source).map_err(Error::Io)?;
        let cancel = AtomicBool::new(false);
        self.ensure(store, object, pin, &cancel, |path| {
            package.write(source, path, &cancel)
        })
    }

    pub(super) fn ensure(
        &self,
        store: &ServerStore,
        object: Object,
        pin: Pin,
        cancel: &AtomicBool,
        produce: impl FnOnce(&Path) -> io::Result<()>,
    ) -> Result<CacheEntry, Error> {
        let boot = self.boot_id;
        let path = object.path(&self.root);
        if let Some(hit) = self.lookup(store, object, pin)? {
            return Ok(hit);
        }
        super::check_cancel(cancel).map_err(Error::Io)?;
        // The encoder may make its own scratch files. Keep the entire
        // attempt in a private directory swept as a unit on restart.
        let attempt = tempfile::Builder::new()
            .prefix("transcode-")
            .suffix(".part")
            .tempdir_in(self.root.join("tmp"))
            .map_err(Error::Io)?;
        let part = attempt.path().join("audio.part");
        produce(&part).map_err(Error::Io)?;
        super::check_cancel(cancel).map_err(Error::Io)?;
        let mut file = File::options()
            .read(true)
            .write(true)
            .open(&part)
            .map_err(Error::Io)?;
        file.sync_all().map_err(Error::Io)?;
        let (size_bytes, file_hash, sampled_hash) = hashes(&mut file, cancel).map_err(Error::Io)?;
        drop(file);
        let sql_size =
            i64::try_from(size_bytes).map_err(|_| Error::Invalid("file too large for cache"))?;
        let _publication = self
            .publication
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        super::check_cancel(cancel).map_err(Error::Io)?;
        if let Some(hit) = self.hit(store, object, pin)? {
            return Ok(hit);
        }
        self.prune_to(store, self.budget.saturating_sub(size_bytes))?;
        super::check_cancel(cancel).map_err(Error::Io)?;
        let parent = path
            .parent()
            .ok_or(Error::Invalid("missing cache parent"))?;
        ensure_directory(parent)?;
        fs::rename(&part, &path).map_err(Error::Io)?;
        store.transact(move |tx, now, _| {
            let key = object.key.to_hex().to_string();
            tx.execute(
                "INSERT INTO transcode_entry
                 (cache_key, layer, codec, source_key, master_scheme, size_bytes,
                  file_hash, sampled_hash, created_at_ms, last_used_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)",
                params![
                    key,
                    object.layer.name(),
                    object.codec.extension(),
                    object.source.map(|hash| hash.to_hex().to_string()),
                    object.master_scheme,
                    sql_size,
                    file_hash.as_bytes(),
                    sampled_hash.as_bytes(),
                    now,
                ],
            )?;
            hold(tx, tx.last_insert_rowid(), boot, pin, now)?;
            Ok(())
        })?;
        Ok(CacheEntry {
            object,
            path,
            size_bytes,
            file_hash,
            sampled_hash,
        })
    }

    pub(super) fn lookup(
        &self,
        store: &ServerStore,
        object: Object,
        pin: Pin,
    ) -> Result<Option<CacheEntry>, Error> {
        let _publication = self
            .publication
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        self.hit(store, object, pin)
    }

    // Caller holds publication across the stat and pin acquisition.
    fn hit(
        &self,
        store: &ServerStore,
        object: Object,
        pin: Pin,
    ) -> Result<Option<CacheEntry>, Error> {
        let path = object.path(&self.root);
        let found = store.read(|db| {
            Ok(db
                .query_row(
                    "SELECT id, size_bytes, file_hash, sampled_hash
                 FROM transcode_entry WHERE cache_key=?1",
                    [object.key.to_hex().as_str()],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            size_column(row, 1)?,
                            row.get::<_, [u8; 32]>(2)?,
                            row.get::<_, [u8; 32]>(3)?,
                        ))
                    },
                )
                .optional()?)
        })?;
        let Some((id, size_bytes, full, sample)) = found else {
            return Ok(None);
        };
        if !is_present(&path, size_bytes)? {
            store.transact(move |tx, _, _| {
                tx.execute("DELETE FROM transcode_entry WHERE id=?1", [id])?;
                Ok(())
            })?;
            return Ok(None);
        }
        let boot = self.boot_id;
        store.transact(move |tx, now, _| {
            tx.execute(
                "UPDATE transcode_entry SET last_used_at_ms=?1 WHERE id=?2",
                params![now, id],
            )?;
            hold(tx, id, boot, pin, now)
        })?;
        Ok(Some(CacheEntry {
            object,
            path,
            size_bytes,
            file_hash: full.into(),
            sampled_hash: sample.into(),
        }))
    }

    pub fn release(&self, store: &ServerStore, pin_id: Id) -> Result<(), Error> {
        let _publication = self
            .publication
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let boot = self.boot_id;
        store.transact(move |tx, _, _| {
            tx.execute(
                "DELETE FROM transcode_pin WHERE pin_id=?1 AND boot_id=?2",
                params![pin_id.as_bytes().as_slice(), boot.as_bytes().as_slice()],
            )?;
            Ok(())
        })?;
        self.prune_to(store, self.budget)
    }

    // The publication lock excludes new pins while these rows are removed.
    // Keep a failed unlink indexed so a later eviction can retry it.
    fn prune_to(&self, store: &ServerStore, budget: u64) -> Result<(), Error> {
        let victims = store.read(|db| victims(db, budget))?;
        for row in victims {
            match fs::remove_file(row.path(&self.root)?) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(Error::Io(e)),
                _ => {}
            }
            store.transact(move |tx, _, _| {
                tx.execute(
                    "INSERT OR REPLACE INTO transcode_size_hint (cache_key, size_bytes)
                     SELECT cache_key, size_bytes FROM transcode_entry WHERE id=?1",
                    [row.id],
                )?;
                tx.execute("DELETE FROM transcode_entry WHERE id=?1", [row.id])?;
                // REPLACE assigns a fresh rowid, bounding both history and pruning cost.
                tx.execute(
                    "DELETE FROM transcode_size_hint WHERE id <= last_insert_rowid() - 65536",
                    [],
                )?;
                Ok(())
            })?;
        }
        Ok(())
    }

    pub fn size_hint(&self, store: &ServerStore, key: blake3::Hash) -> Result<Option<u64>, Error> {
        store.read(|db| {
            Ok(db
                .query_row(
                    "SELECT size_bytes FROM transcode_entry WHERE cache_key=?1
                     UNION ALL SELECT size_bytes FROM transcode_size_hint WHERE cache_key=?1 LIMIT 1",
                    [key.to_hex().as_str()],
                    |row| size_column(row, 0),
                )
                .optional()?)
        })
    }
}

fn ensure_directory(path: &Path) -> Result<(), Error> {
    fs::create_dir_all(path).map_err(Error::Io)?;
    if !fs::symlink_metadata(path).map_err(Error::Io)?.is_dir() {
        return Err(Error::Invalid("cache directory must be a directory"));
    }
    Ok(())
}

fn is_object_path(path: &Path) -> bool {
    let Some(key) = path.file_stem().and_then(|key| key.to_str()) else {
        return false;
    };
    blake3::Hash::from_hex(key).is_ok()
        && path
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            == key.get(..2)
        && matches!(
            path.extension().and_then(|ext| ext.to_str()),
            Some("mp3" | "opus" | "flac")
        )
}

fn size_column(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let size: i64 = row.get(index)?;
    u64::try_from(size).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(index, size))
}

fn is_present(path: &Path, size: u64) -> Result<bool, Error> {
    match fs::symlink_metadata(path) {
        Ok(file) => Ok(file.is_file() && file.len() == size),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(Error::Io(e)),
    }
}

fn hold(tx: &Transaction<'_>, entry: i64, boot: Id, pin: Pin, now: i64) -> Result<(), Error> {
    let kind = match pin.owner {
        PinOwner::SyncBatch => "sync_batch",
        PinOwner::HttpTransfer => "http_transfer",
        PinOwner::AbSession => "ab_session",
    };
    tx.execute(
        "INSERT INTO transcode_pin (entry_id, pin_id, boot_id, owner_kind, created_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT(entry_id, pin_id) DO NOTHING",
        params![
            entry,
            pin.id.as_bytes().as_slice(),
            boot.as_bytes().as_slice(),
            kind,
            now
        ],
    )?;
    Ok(())
}

struct IndexedFile {
    id: i64,
    key: String,
    layer: String,
    codec: String,
    size: u64,
}
impl IndexedFile {
    fn read(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            key: row.get(1)?,
            layer: row.get(2)?,
            codec: row.get(3)?,
            size: size_column(row, 4)?,
        })
    }

    fn path(&self, root: &Path) -> Result<PathBuf, Error> {
        let key = blake3::Hash::from_hex(&self.key)
            .map_err(|_| Error::Corrupt("bad transcode cache key"))?;
        let layer = match self.layer.as_str() {
            "packaged" => Layer::Packaged,
            "derivative" => Layer::Derivative,
            _ => return Err(Error::Corrupt("bad transcode layer")),
        };
        let codec = match self.codec.as_str() {
            "flac" => Codec::Flac,
            "mp3" => Codec::Mp3,
            "opus" => Codec::Opus,
            _ => return Err(Error::Corrupt("bad transcode codec")),
        };
        Ok(Object {
            key,
            layer,
            codec,
            source: None,
            master_scheme: None,
        }
        .path(root))
    }
}

fn victims(db: &rusqlite::Connection, budget: u64) -> Result<Vec<IndexedFile>, Error> {
    let mut total = db.query_row(
        "SELECT coalesce(sum(size_bytes),0) FROM transcode_entry",
        [],
        |r| size_column(r, 0),
    )?;
    let mut victims = Vec::new();
    if total > budget {
        // The index orders 'packaged' ahead of 'derivative' within this closed enum.
        let mut oldest = db.prepare(
            "SELECT id, cache_key, layer, codec, size_bytes FROM transcode_entry e
             WHERE NOT EXISTS (SELECT 1 FROM transcode_pin p WHERE p.entry_id = e.id)
             ORDER BY layer DESC, last_used_at_ms, id",
        )?;
        for row in oldest.query_map([], IndexedFile::read)? {
            let row = row?;
            total -= row.size;
            victims.push(row);
            if total <= budget {
                break;
            }
        }
    }
    Ok(victims)
}

/// Hash short files once. Above two MiB the sample is the concatenation of
/// the first and last MiB, with no overlapping bytes.
fn hashes(file: &mut File, cancel: &AtomicBool) -> io::Result<(u64, blake3::Hash, blake3::Hash)> {
    const MIB: u64 = 1 << 20;
    let size = file.metadata()?.len();
    if size == 0 {
        return Err(io::Error::other("empty transcode output"));
    }
    let mut full = blake3::Hasher::new();
    super::copy(file, &mut full, cancel)?;
    let full = full.finalize();
    let sampled = if size <= 2 * MIB {
        full
    } else {
        let mut h = blake3::Hasher::new();
        file.rewind()?;
        io::copy(&mut file.take(MIB), &mut h)?;
        file.seek(SeekFrom::End(-(MIB as i64)))?;
        io::copy(file, &mut h)?;
        h.finalize()
    };
    Ok((size, full, sampled))
}

#[cfg(test)]
mod tests;
