//! What a scan writes to `library.db` as it records files: sources, source
//! items, revisions, audio assets, pictures and claims. This is step 2 and
//! steps 4 to 6 of "Scanning one file" in docs/design/identity.md, and the
//! rows of its "Rescans" table. Which recording a file is comes after, in
//! `resolve_store`.

use std::collections::HashMap;

use ritornel_domain::payload::{FolderImage, Payload, Picture};
use ritornel_domain::{EXTRACTOR_VERSION, extract};
use rusqlite::OptionalExtension;

use crate::library_store::Write;
use crate::{Error, Id, LibraryStore, WriteRequest, Written};

/// One place items come from: for a scan, a folder that is the whole of one
/// master root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    /// The row's key, which never leaves the server.
    pub row: i64,
    /// The source's ID.
    pub public_id: Id,
    /// Its name, which is its root's name.
    pub name: String,
    /// Its kind: `folder`.
    pub kind: String,
    /// Whether its root is on this server.
    pub on_this_server: bool,
}

/// A live item of a source, as a walk compares it with what it finds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Known {
    pub(crate) id: i64,
    pub(crate) size: i64,
    pub(crate) mtime_ms: i64,
    pub(crate) missing: bool,
    /// The audio hash of its current revision, as hex.
    pub(crate) audio: Option<String>,
    /// Its folder's images as last recorded: name, size and time.
    pub(crate) images: Vec<(String, u64, i64)>,
}

/// One thing to record about one file.
#[derive(Debug, Clone)]
pub(crate) enum RecordOp {
    /// A path the library has not seen: a new item.
    New(Payload),
    /// The item at this path, read again: a new revision.
    Update { item: i64, payload: Payload },
    /// The file of this item is now at the payload's path: the same item,
    /// re-keyed.
    Move { item: i64, payload: Payload },
    /// A different song has taken this item's path: the item is retired
    /// with everything it has, and a new item takes the path.
    Retire { item: i64, payload: Payload },
}

const SOURCE_COLUMNS: &str = "src.id, src.public_id, src.name, src.kind,
    (SELECT s.public_id FROM library_root r JOIN server s ON s.id = r.server_id WHERE r.id = src.root_id)";

fn source(row: &rusqlite::Row<'_>, hub: Id) -> rusqlite::Result<Source> {
    Ok(Source {
        row: row.get(0)?,
        public_id: Id::from_bytes(row.get(1)?),
        name: row.get(2)?,
        kind: row.get(3)?,
        on_this_server: row.get::<_, Option<[u8; 16]>>(4)?.map(Id::from_bytes) == Some(hub),
    })
}

fn unhex(text: &str) -> Vec<u8> {
    let digits: Vec<u8> = text
        .chars()
        .filter_map(|c| c.to_digit(16))
        .filter_map(|d| u8::try_from(d).ok())
        .collect();
    digits
        .chunks(2)
        .filter_map(|pair| match pair {
            [high, low] => Some(high * 16 + low),
            _ => None,
        })
        .collect()
}

fn signed(number: u64) -> i64 {
    i64::try_from(number).unwrap_or(i64::MAX)
}

/// The rows a recording writes through, with the metadata sources' keys.
struct Recorder<'a> {
    write: &'a Write<'a>,
    sources: HashMap<String, i64>,
}

impl<'a> Recorder<'a> {
    fn new(write: &'a Write<'a>) -> Result<Self, Error> {
        let mut statement = write
            .connection()
            .prepare("SELECT kind, id FROM metadata_source")?;
        let sources = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<_, _>>()?;
        Ok(Self { write, sources })
    }

    /// The asset with this audio, made if the library has not seen it.
    fn asset(&self, payload: &Payload) -> Result<Option<i64>, Error> {
        let Some(audio) = &payload.audio else {
            return Ok(None);
        };
        let (connection, hash) = (self.write.connection(), unhex(&audio.hash));
        let existing: Option<i64> = connection
            .prepare_cached(
                "SELECT id FROM audio_asset WHERE hash_scheme = ?1 AND audio_hash = ?2",
            )?
            .query_row((audio.hash_scheme, &hash), |row| row.get(0))
            .optional()?;
        if let Some(row) = existing {
            return Ok(Some(row));
        }
        let layout = match audio.channels {
            Some(1) => "mono".to_owned(),
            Some(2) => "stereo".to_owned(),
            Some(count) => format!("{count}ch"),
            None => "unknown".to_owned(),
        };
        let bitrate = (!audio.lossless() && audio.duration_ms > 0)
            .then(|| signed(audio.payload_bytes.saturating_mul(8) / audio.duration_ms));
        connection
            .prepare_cached(
                "INSERT INTO audio_asset
                   (public_id, hash_scheme, audio_hash, flac_md5, codec, sample_rate_hz, channel_layout,
                    bit_depth, bitrate_kbps, lossless, duration_ms, rev)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            )?
            .execute((
                self.write.new_id()?.as_bytes(),
                audio.hash_scheme,
                &hash,
                audio.flac_md5.as_deref().map(unhex),
                &audio.codec,
                audio.sample_rate.unwrap_or(0),
                layout,
                audio.bits,
                bitrate,
                audio.lossless(),
                signed(audio.duration_ms),
                self.write.rev(),
            ))?;
        Ok(Some(connection.last_insert_rowid()))
    }

    fn picture(&self, picture: &Picture) -> Result<i64, Error> {
        let (connection, hash) = (self.write.connection(), unhex(&picture.hash));
        let existing: Option<i64> = connection
            .prepare_cached("SELECT id FROM picture WHERE content_hash = ?1")?
            .query_row([&hash], |row| row.get(0))
            .optional()?;
        if let Some(row) = existing {
            return Ok(row);
        }
        connection
            .prepare_cached(
                "INSERT INTO picture (public_id, content_hash, mime_type, width, height, size_bytes, rev)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?
            .execute((
                self.write.new_id()?.as_bytes(),
                &hash,
                &picture.mime,
                picture.width,
                picture.height,
                signed(picture.size),
                self.write.rev(),
            ))?;
        Ok(connection.last_insert_rowid())
    }

    /// Records the payload as the item's current revision, with its asset,
    /// its pictures and its claims. A payload the item has had before is
    /// that revision again, and none is written twice.
    fn revision(&self, item: i64, payload: &Payload, is_new: bool) -> Result<(), Error> {
        let (connection, rev) = (self.write.connection(), self.write.rev());
        let asset = self.asset(payload)?;
        let json = serde_json::to_string(payload)?;
        let hash = blake3::hash(json.as_bytes());
        let existing: Option<i64> = connection
            .prepare_cached("SELECT id FROM source_item_revision WHERE source_item_id = ?1 AND payload_hash = ?2")?
            .query_row((item, hash.as_bytes()), |row| row.get(0))
            .optional()?;
        let revision = match existing {
            Some(row) => row,
            None => {
                connection
                    .prepare_cached(
                        "INSERT INTO source_item_revision
                           (source_item_id, revision_no, payload_hash, payload_json, audio_asset_id,
                            observed_at_ms, rev)
                         VALUES (?1, (SELECT coalesce(max(revision_no), 0) + 1 FROM source_item_revision
                                      WHERE source_item_id = ?1), ?2, ?3, ?4, ?5, ?6)",
                    )?
                    .execute((item, hash.as_bytes(), &json, asset, self.write.now_ms(), rev))?;
                let revision = connection.last_insert_rowid();
                // Embedded pictures first, then the folder's own.
                let embedded = payload.pictures.iter().map(|picture| (picture, None));
                let folder = payload
                    .folder_images
                    .iter()
                    .map(|image| (&image.picture, Some(&image.name)));
                for (position, (picture, file_name)) in embedded.chain(folder).enumerate() {
                    let kind = if file_name.is_some() {
                        "folder_image"
                    } else {
                        "embedded"
                    };
                    connection
                        .prepare_cached(
                            "INSERT INTO revision_picture
                               (revision_id, picture_id, position, source_kind, file_name, picture_type, rev)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                        )?
                        .execute((
                            revision,
                            self.picture(picture)?,
                            signed(position as u64),
                            kind,
                            file_name,
                            picture.kind.filter(|_| file_name.is_none()),
                            rev,
                        ))?;
                }
                revision
            }
        };
        connection
            .prepare_cached(
                "UPDATE source_item
                 SET current_revision_id = ?2, provider_key = ?3, size_bytes = ?4, mtime_ms = ?5,
                     missing_since_ms = NULL, rev = ?6
                 WHERE id = ?1",
            )?
            .execute((
                item,
                revision,
                &payload.path,
                signed(payload.size),
                payload.mtime_ms,
                rev,
            ))?;
        self.claims(item, revision, payload, !is_new)
    }

    /// Rebuilds the item's claims from a payload: extractor 1. `clear` is
    /// whether the item may have claims to remove first. No index finds an
    /// item's claims, so a new item's are not looked for.
    fn claims(
        &self,
        item: i64,
        revision: i64,
        payload: &Payload,
        clear: bool,
    ) -> Result<(), Error> {
        let (connection, rev) = (self.write.connection(), self.write.rev());
        for table in ["identifier_claim", "metadata_claim"]
            .into_iter()
            .filter(|_| clear)
        {
            connection
                .prepare_cached(&format!("DELETE FROM {table} WHERE source_item_id = ?1"))?
                .execute([item])?;
        }
        for claim in extract(payload) {
            let (table, column) = if claim.identifier {
                ("identifier_claim", "namespace")
            } else {
                ("metadata_claim", "field")
            };
            let source = self
                .sources
                .get(claim.source.as_str())
                .ok_or(Error::Corrupt("no metadata source"))?;
            connection
                .prepare_cached(&format!(
                    "INSERT INTO {table}
                       (source_item_id, revision_id, extractor_version, scope, {column}, value, ordinal,
                        metadata_source_id, rev)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"
                ))?
                .execute((
                    item,
                    revision,
                    EXTRACTOR_VERSION,
                    claim.scope.as_str(),
                    &claim.name,
                    &claim.value,
                    claim.ordinal,
                    source,
                    rev,
                ))?;
        }
        Ok(())
    }

    fn new_item(&self, source: i64, payload: &Payload) -> Result<(), Error> {
        let connection = self.write.connection();
        connection
            .prepare_cached(
                "INSERT INTO source_item
                   (public_id, source_instance_id, item_kind, provider_key, first_seen_at_ms, rev)
                 VALUES (?1, ?2, 'file', ?3, ?4, ?5)",
            )?
            .execute((
                self.write.new_id()?.as_bytes(),
                source,
                &payload.path,
                self.write.now_ms(),
                self.write.rev(),
            ))?;
        self.revision(connection.last_insert_rowid(), payload, true)
    }
}

/// The versions the library's claims and bindings were made by, kept as a
/// setting that follows the library.
pub(crate) const VERSIONS_SETTING: &str = "identity.versions";

/// The setting that says something was recorded that no resolution has
/// seen yet. It lives in the library, beside what was recorded, so that a
/// scan cancelled before it resolved leaves it for the next one. Only a
/// resolution that completes removes it.
pub(crate) const PENDING_SETTING: &str = "identity.resolution_pending";

/// Says, in the write's transaction, that a resolution is owed.
pub(crate) fn mark_pending(write: &Write<'_>) -> Result<(), Error> {
    write.connection().execute(
        "INSERT INTO library_setting (key, value_json, rev) VALUES (?1, 'true', ?2)
         ON CONFLICT (key) DO UPDATE SET value_json = excluded.value_json, rev = excluded.rev",
        (PENDING_SETTING, write.rev()),
    )?;
    Ok(())
}

impl LibraryStore {
    /// Adds a source: a named master root on this server and the folder
    /// that is the whole of it. The root's path is not here: it is in this
    /// server's `server.db`. A name the library already has on this server
    /// is refused with [`Error::SourceExists`]. `receipt` is given the new
    /// source and the revision it was added at, and returns the result to
    /// store and to answer with.
    pub fn add_source<R>(
        &self,
        request: WriteRequest,
        name: &str,
        receipt: R,
    ) -> Result<Written, Error>
    where
        R: FnOnce(&Source, i64) -> String + Send + 'static,
    {
        let (name, hub) = (name.to_owned(), self.hub());
        self.write(request, move |write| {
            let (connection, rev, now_ms) = (write.connection(), write.rev(), write.now_ms());
            let existing: Option<[u8; 16]> = connection
                .query_row(
                    "SELECT src.public_id FROM library_root r
                     JOIN server s ON s.id = r.server_id
                     JOIN source_instance src ON src.root_id = r.id
                     WHERE s.public_id = ?1 AND r.name = ?2",
                    (hub.as_bytes(), &name),
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(source) = existing {
                return Err(Error::SourceExists {
                    source: Id::from_bytes(source),
                });
            }
            connection.execute(
                "INSERT INTO library_root (public_id, server_id, name, kind, created_at_ms, rev)
                 VALUES (?1, (SELECT id FROM server WHERE public_id = ?2), ?3, 'master', ?4, ?5)",
                (
                    write.new_id()?.as_bytes(),
                    hub.as_bytes(),
                    &name,
                    now_ms,
                    rev,
                ),
            )?;
            let root = connection.last_insert_rowid();
            let public_id = write.new_id()?;
            connection.execute(
                "INSERT INTO source_instance (public_id, kind, name, root_id, created_at_ms, rev)
                 VALUES (?1, 'folder', ?2, ?3, ?4, ?5)",
                (public_id.as_bytes(), &name, root, now_ms, rev),
            )?;
            let added = Source {
                row: connection.last_insert_rowid(),
                public_id,
                name,
                kind: "folder".to_owned(),
                on_this_server: true,
            };
            Ok(receipt(&added, rev))
        })
    }

    /// Every source, in the order they were added.
    pub fn sources(&self) -> Result<Vec<Source>, Error> {
        let hub = self.hub();
        self.read(|connection| {
            let mut statement = connection.prepare(&format!(
                "SELECT {SOURCE_COLUMNS} FROM source_instance src ORDER BY src.id"
            ))?;
            let rows = statement.query_map([], |row| source(row, hub))?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
    }

    /// The source with this ID.
    pub fn source(&self, id: Id) -> Result<Option<Source>, Error> {
        let hub = self.hub();
        self.read(|connection| {
            Ok(connection
                .query_row(
                    &format!(
                        "SELECT {SOURCE_COLUMNS} FROM source_instance src WHERE src.public_id = ?1"
                    ),
                    [id.as_bytes()],
                    |row| source(row, hub),
                )
                .optional()?)
        })
    }

    /// Every live item of a source, by its path.
    pub(crate) fn known_items(&self, source: i64) -> Result<HashMap<String, Known>, Error> {
        self.read(|connection| {
            let mut statement = connection.prepare(
                "SELECT si.provider_key, si.id, si.size_bytes, si.mtime_ms, si.missing_since_ms IS NOT NULL,
                        lower(hex(aa.audio_hash)), json_extract(sr.payload_json, '$.folder_images')
                 FROM source_item si
                 LEFT JOIN source_item_revision sr ON sr.id = si.current_revision_id
                 LEFT JOIN audio_asset aa ON aa.id = sr.audio_asset_id
                 WHERE si.source_instance_id = ?1 AND si.item_kind = 'file' AND si.retired_at_ms IS NULL",
            )?;
            let rows = statement.query_map([source], |row| {
                let images: Option<String> = row.get(6)?;
                let images: Vec<FolderImage> =
                    images.and_then(|json| serde_json::from_str(&json).ok()).unwrap_or_default();
                Ok((
                    row.get(0)?,
                    Known {
                        id: row.get(1)?,
                        size: row.get::<_, Option<i64>>(2)?.unwrap_or(-1),
                        mtime_ms: row.get::<_, Option<i64>>(3)?.unwrap_or(-1),
                        missing: row.get(4)?,
                        audio: row.get(5)?,
                        images: images
                            .into_iter()
                            .map(|image| (image.name, image.picture.size, image.mtime_ms))
                            .collect(),
                    },
                ))
            })?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
    }

    /// The asset with this audio hash, given as hex, if the library has it.
    pub(crate) fn asset_of(&self, scheme: u32, hash: &str) -> Result<Option<i64>, Error> {
        let hash = unhex(hash);
        self.read(|connection| {
            Ok(connection
                .query_row(
                    "SELECT id FROM audio_asset WHERE hash_scheme = ?1 AND audio_hash = ?2",
                    (scheme, &hash),
                    |row| row.get(0),
                )
                .optional()?)
        })
    }

    /// Records what a scan read, in one transaction.
    pub(crate) fn record(
        &self,
        request: WriteRequest,
        source: i64,
        ops: Vec<RecordOp>,
    ) -> Result<Written, Error> {
        self.write(request, move |write| {
            let recorder = Recorder::new(write)?;
            for op in &ops {
                match op {
                    RecordOp::New(payload) => recorder.new_item(source, payload)?,
                    RecordOp::Update { item, payload } | RecordOp::Move { item, payload } => {
                        recorder.revision(*item, payload, false)?;
                    }
                    RecordOp::Retire { item, payload } => {
                        write.connection().execute(
                            "UPDATE source_item SET retired_at_ms = ?2, rev = ?3 WHERE id = ?1",
                            (item, write.now_ms(), write.rev()),
                        )?;
                        recorder.new_item(source, payload)?;
                    }
                }
            }
            if !ops.is_empty() {
                mark_pending(write)?;
            }
            Ok(ops.len().to_string())
        })
    }

    /// Marks items whose files a scan of a reachable root no longer finds.
    /// An item keeps everything it has, and reclaims it if the file
    /// returns.
    pub(crate) fn mark_missing(
        &self,
        request: WriteRequest,
        items: Vec<i64>,
    ) -> Result<Written, Error> {
        self.write(request, move |write| {
            let mut update = write.connection().prepare(
                "UPDATE source_item SET missing_since_ms = ?2, rev = ?3
                 WHERE id = ?1 AND missing_since_ms IS NULL",
            )?;
            for item in &items {
                update.execute((item, write.now_ms(), write.rev()))?;
            }
            drop(update);
            mark_pending(write)?;
            Ok(items.len().to_string())
        })
    }

    /// Whether the library's claims were made by another version of the
    /// extractor than this one.
    pub(crate) fn claims_are_stale(&self) -> Result<bool, Error> {
        self.read(|connection| {
            Ok(connection.query_row(
                "SELECT EXISTS (SELECT 1 FROM metadata_claim WHERE extractor_version <> ?1)
                     OR EXISTS (SELECT 1 FROM identifier_claim WHERE extractor_version <> ?1)",
                [EXTRACTOR_VERSION],
                |row| row.get(0),
            )?)
        })
    }

    /// Rebuilds every item's claims from its stored payload, as a changed
    /// extractor asks.
    pub(crate) fn rebuild_claims(&self, request: WriteRequest) -> Result<Written, Error> {
        self.write(request, |write| {
            let recorder = Recorder::new(write)?;
            mark_pending(write)?;
            write
                .connection()
                .execute_batch("DELETE FROM identifier_claim; DELETE FROM metadata_claim;")?;
            let mut statement = write.connection().prepare(
                "SELECT si.id, sr.id, sr.payload_json FROM source_item si
                 JOIN source_item_revision sr ON sr.id = si.current_revision_id",
            )?;
            let rows = statement
                .query_map([], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get::<_, String>(2)?))
                })?
                .collect::<Result<Vec<(i64, i64, String)>, _>>()?;
            for (item, revision, json) in &rows {
                recorder.claims(*item, *revision, &serde_json::from_str(json)?, false)?;
            }
            Ok(rows.len().to_string())
        })
    }
}
