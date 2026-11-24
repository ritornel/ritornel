//! What a library keeps to correct itself from: the sidecars a scan finds
//! with the audio, and the disc layouts derived from them. See "Disc
//! layouts" in docs/internals/corrections.md.
//!
//! Sidecars are durable, as source items' revisions are. Layouts are
//! derived, and are worked out again from the stored sidecars and the
//! files' stored lengths whenever either changes or [`LAYOUT_VERSION`] does.
//! None of it is read by the resolver, so none of it moves a wall.

use std::collections::{BTreeMap, HashMap, HashSet};

use ritornel_contract::{Disc, DiscFile, DiscList, DiscsParams, LayoutMethod};
use ritornel_domain::layout::{self, AudioFile, Folder, LAYOUT_VERSION, Sidecar};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::library_store::Write;
use crate::resolve_store::rows;
use crate::walk::folder_of;
use crate::{Error, LibraryStore, WriteRequest, Written};

/// The write that records a scan's sidecars. It changes nothing shown.
pub const SIDECARS_METHOD: &str = "scan.sidecars";

/// The write that derives disc layouts. It changes nothing shown.
pub const LAYOUTS_METHOD: &str = "corrections.layouts";

/// The setting that holds the layout version the stored layouts were
/// derived by.
const LAYOUTS_SETTING: &str = "corrections.layout_version";

/// The setting that says something was recorded that the layouts have not
/// been derived from. It is written in the same transaction as what it is
/// about, so a scan that stops before it derives leaves it for the next.
/// Only a derivation removes it.
const LAYOUTS_PENDING: &str = "corrections.layouts_pending";

/// Says, in the write's transaction, that the layouts are due: always when
/// sidecars changed, and when files changed in a library that has
/// sidecars.
pub(crate) fn mark_layouts_pending(write: &Write<'_>) -> Result<(), Error> {
    write.connection().execute(
        "INSERT INTO library_setting (key, value_json, rev)
         SELECT ?1, 'true', ?2 WHERE EXISTS (SELECT 1 FROM sidecar)
         ON CONFLICT (key) DO UPDATE SET value_json = excluded.value_json, rev = excluded.rev",
        (LAYOUTS_PENDING, write.rev()),
    )?;
    Ok(())
}

/// A sidecar the library has for a source, as a walk compares it with what
/// it finds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KnownSidecar {
    pub(crate) id: i64,
    pub(crate) size: i64,
    pub(crate) mtime_ms: i64,
    pub(crate) missing: bool,
}

/// One thing to record about one sidecar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SidecarOp {
    /// The file at this path, as read.
    Read {
        path: String,
        size: u64,
        mtime_ms: i64,
        bytes: Vec<u8>,
    },
    /// The sidecar's file is gone, or is no longer one.
    Gone { id: i64 },
}

fn signed(number: u64) -> i64 {
    i64::try_from(number).unwrap_or(i64::MAX)
}

pub(crate) fn unsigned(number: i64) -> u64 {
    u64::try_from(number).unwrap_or(0)
}

/// The library's revision.
pub(crate) fn revision(connection: &Connection) -> Result<u64, Error> {
    let rev: i64 =
        connection.query_row("SELECT rev FROM library_meta WHERE id = 1", [], |row| {
            row.get(0)
        })?;
    Ok(unsigned(rev))
}

/// Sets a library setting, at revision `rev`.
pub(crate) fn set_setting(
    connection: &Connection,
    key: &str,
    value: &str,
    rev: i64,
) -> Result<(), Error> {
    connection.execute(
        "INSERT INTO library_setting (key, value_json, rev) VALUES (?1, ?2, ?3)
         ON CONFLICT (key) DO UPDATE SET value_json = excluded.value_json, rev = excluded.rev",
        (key, value, rev),
    )?;
    Ok(())
}

/// One stored or derived layout, as it is compared.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DiscRow {
    offsets_json: String,
    lead_out: i64,
    found_json: String,
    provisional: bool,
    whole_file: bool,
    /// The source item at each position.
    items: Vec<(i64, i64)>,
}

/// Where a layout is: its source, its folder and its disc ID.
type DiscKey = (i64, String, String);

/// One way a layout was found, as `found_json` holds it.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Found {
    method: String,
    sidecar_revision_id: i64,
}

fn method(text: &str) -> Option<LayoutMethod> {
    Some(match text {
        "log" => LayoutMethod::Log,
        "cue" => LayoutMethod::Cue,
        "renamed_cue" => LayoutMethod::RenamedCue,
        "accuraterip" => LayoutMethod::AccurateRip,
        "cddb" => LayoutMethod::Cddb,
        _ => return None,
    })
}

/// Every layout the stored sidecars and files give, by where it is.
fn derive(connection: &Connection) -> Result<BTreeMap<DiscKey, DiscRow>, Error> {
    // The first value of a file's own tag, by field.
    let mut tags: HashMap<(i64, String), (i64, String)> = HashMap::new();
    for (item, field, ordinal, value) in rows(
        connection,
        "SELECT mc.source_item_id, mc.field, mc.ordinal, mc.value
         FROM metadata_claim mc JOIN metadata_source ms ON ms.id = mc.metadata_source_id
         WHERE ms.kind = 'file_tags' AND mc.field IN ('track_number', 'title')",
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
            ))
        },
    )? {
        let first = tags
            .entry((item, field))
            .or_insert((ordinal, value.clone()));
        if ordinal < first.0 {
            *first = (ordinal, value);
        }
    }
    let mut tag = |item: i64, field: &str| {
        tags.remove(&(item, field.to_owned()))
            .map(|(_, value)| value)
    };
    // Every file that is there, by source and folder, in the order of
    // their paths.
    let mut folders: BTreeMap<i64, BTreeMap<String, Vec<AudioFile>>> = BTreeMap::new();
    let items = rows(
        connection,
        "SELECT si.id, si.source_instance_id, si.provider_key,
                aa.duration_ms, aa.sample_rate_hz, aa.channel_layout, aa.lossless
         FROM source_item si
         LEFT JOIN source_item_revision sr ON sr.id = si.current_revision_id
         LEFT JOIN audio_asset aa ON aa.id = sr.audio_asset_id
         WHERE si.item_kind = 'file' AND si.retired_at_ms IS NULL AND si.missing_since_ms IS NULL
         ORDER BY si.source_instance_id, si.provider_key",
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<i64>>(3)?,
                row.get::<_, Option<i64>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<bool>>(6)?,
            ))
        },
    )?;
    for (item, source, path, duration, rate, channels, lossless) in items {
        let sectors = match (duration, rate, channels.as_deref(), lossless) {
            (Some(duration), Some(rate), Some("stereo"), Some(lossless)) => layout::sectors(
                unsigned(duration),
                u32::try_from(rate).unwrap_or(0),
                2,
                lossless,
            ),
            _ => None,
        };
        let name = path.rsplit('/').next().unwrap_or(&path).to_owned();
        let file = AudioFile {
            item,
            name,
            track: tag(item, "track_number").and_then(|value| value.parse().ok()),
            title: tag(item, "title"),
            sectors,
        };
        folders
            .entry(source)
            .or_default()
            .entry(folder_of(&path).to_owned())
            .or_default()
            .push(file);
    }
    // Every sidecar that is there, by source and folder, in the order of
    // their paths.
    let mut sidecars: BTreeMap<i64, BTreeMap<String, Vec<Sidecar>>> = BTreeMap::new();
    let kept = rows(
        connection,
        "SELECT s.source_instance_id, s.rel_path, r.id, r.content
         FROM sidecar s JOIN sidecar_revision r ON r.id = s.current_revision_id
         WHERE s.missing_since_ms IS NULL
         ORDER BY s.source_instance_id, s.rel_path",
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Vec<u8>>(3)?,
            ))
        },
    )?;
    for (source, path, revision, bytes) in kept {
        let name = path.rsplit('/').next().unwrap_or(&path).to_owned();
        sidecars
            .entry(source)
            .or_default()
            .entry(folder_of(&path).to_owned())
            .or_default()
            .push(Sidecar {
                revision,
                name,
                bytes,
            });
    }
    let mut derived = BTreeMap::new();
    for (source, by_folder) in folders {
        let Some(found) = sidecars.get(&source) else {
            continue;
        };
        let folders: Vec<Folder> = by_folder
            .into_iter()
            .map(|(path, files)| Folder { path, files })
            .collect();
        for folder in layout::layouts(&folders, found) {
            for found in folder.layouts {
                let ways: Vec<Found> = found
                    .found
                    .iter()
                    .map(|(method, revision)| Found {
                        method: method.as_str().to_owned(),
                        sidecar_revision_id: *revision,
                    })
                    .collect();
                let items = match found.whole_file {
                    Some(item) => vec![(0, item)],
                    None => (1..).zip(found.tracks.iter().copied()).collect(),
                };
                derived.insert(
                    (source, folder.folder.clone(), found.disc_id),
                    DiscRow {
                        offsets_json: serde_json::to_string(&found.toc.starts)?,
                        lead_out: signed(found.toc.lead_out),
                        found_json: serde_json::to_string(&ways)?,
                        provisional: found.provisional,
                        whole_file: found.whole_file.is_some(),
                        items,
                    },
                );
            }
        }
    }
    Ok(derived)
}

/// The layouts stored now, with the version each was derived by.
fn stored(connection: &Connection) -> Result<BTreeMap<DiscKey, (i64, i64, DiscRow)>, Error> {
    let mut items: HashMap<i64, Vec<(i64, i64)>> = HashMap::new();
    for (disc, position, item) in rows(
        connection,
        "SELECT evidence_disc_id, position, source_item_id FROM evidence_disc_item
         ORDER BY evidence_disc_id, position",
        |row| Ok((row.get::<_, i64>(0)?, row.get(1)?, row.get(2)?)),
    )? {
        items.entry(disc).or_default().push((position, item));
    }
    let discs = rows(
        connection,
        "SELECT id, source_instance_id, folder, disc_id, track_offsets_json, lead_out, found_json,
                provisional, whole_file, layout_version
         FROM evidence_disc",
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                (row.get::<_, i64>(1)?, row.get(2)?, row.get(3)?),
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, bool>(7)?,
                row.get::<_, bool>(8)?,
                row.get::<_, i64>(9)?,
            ))
        },
    )?;
    Ok(discs
        .into_iter()
        .map(
            |(id, key, offsets_json, lead_out, found_json, provisional, whole_file, version)| {
                let row = DiscRow {
                    offsets_json,
                    lead_out,
                    found_json,
                    provisional,
                    whole_file,
                    items: items.remove(&id).unwrap_or_default(),
                };
                (key, (id, version, row))
            },
        )
        .collect())
}

/// The layout version the stored layouts were derived by, if any were.
fn stored_version(connection: &Connection) -> Result<Option<i64>, Error> {
    let json: Option<String> = connection
        .query_row(
            "SELECT value_json FROM library_setting WHERE key = ?1",
            [LAYOUTS_SETTING],
            |row| row.get(0),
        )
        .optional()?;
    Ok(json.and_then(|json| json.parse().ok()))
}

/// Derives every layout again and writes what differs from the stored
/// ones, then the version, and settles what was pending.
fn apply(write: &Write<'_>) -> Result<(), Error> {
    let (connection, rev) = (write.connection(), write.rev());
    let mut stored = stored(connection)?;
    let version = i64::from(LAYOUT_VERSION);
    let mut removed = Vec::new();
    let mut added = Vec::new();
    for (key, row) in derive(connection)? {
        match stored.remove(&key) {
            Some((_, had_version, had)) if had_version == version && had == row => {}
            Some((id, _, _)) => {
                removed.push(id);
                added.push((key, row));
            }
            None => added.push((key, row)),
        }
    }
    removed.extend(stored.into_values().map(|(id, _, _)| id));
    removed.sort_unstable();
    for id in &removed {
        connection
            .prepare_cached("DELETE FROM evidence_disc_item WHERE evidence_disc_id = ?1")?
            .execute([id])?;
        connection
            .prepare_cached("DELETE FROM evidence_disc WHERE id = ?1")?
            .execute([id])?;
    }
    for ((source, folder, disc_id), row) in &added {
        connection
            .prepare_cached(
                "INSERT INTO evidence_disc
                   (source_instance_id, folder, disc_id, track_offsets_json, lead_out, found_json,
                    provisional, whole_file, layout_version, rev)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            )?
            .execute((
                source,
                folder,
                disc_id,
                &row.offsets_json,
                row.lead_out,
                &row.found_json,
                row.provisional,
                row.whole_file,
                LAYOUT_VERSION,
                rev,
            ))?;
        let disc = connection.last_insert_rowid();
        for (position, item) in &row.items {
            connection
                .prepare_cached(
                    "INSERT INTO evidence_disc_item (evidence_disc_id, position, source_item_id, rev)
                     VALUES (?1, ?2, ?3, ?4)",
                )?
                .execute((disc, position, item, rev))?;
        }
    }
    set_setting(
        connection,
        LAYOUTS_SETTING,
        &LAYOUT_VERSION.to_string(),
        rev,
    )?;
    connection.execute(
        "DELETE FROM library_setting WHERE key = ?1",
        [LAYOUTS_PENDING],
    )?;
    Ok(())
}

impl LibraryStore {
    /// Every sidecar of a source the library has, by its path.
    pub(crate) fn known_sidecars(
        &self,
        source: i64,
    ) -> Result<HashMap<String, KnownSidecar>, Error> {
        self.read(|connection| {
            let mut statement = connection.prepare(
                "SELECT rel_path, id, size_bytes, mtime_ms, missing_since_ms IS NOT NULL
                 FROM sidecar WHERE source_instance_id = ?1",
            )?;
            let known = statement.query_map([source], |row| {
                Ok((
                    row.get(0)?,
                    KnownSidecar {
                        id: row.get(1)?,
                        size: row.get(2)?,
                        mtime_ms: row.get(3)?,
                        missing: row.get(4)?,
                    },
                ))
            })?;
            Ok(known.collect::<Result<_, _>>()?)
        })
    }

    /// Records what a scan found of a source's sidecars, in one
    /// transaction: each file read, as a revision of its sidecar, and each
    /// sidecar gone, with `receipt` as the write's stored result. A file
    /// read before with the same bytes is that revision again.
    pub(crate) fn record_sidecars(
        &self,
        request: WriteRequest,
        source: i64,
        ops: Vec<SidecarOp>,
        receipt: String,
    ) -> Result<Written, Error> {
        self.write(request, move |write| {
            let (connection, rev, now_ms) = (write.connection(), write.rev(), write.now_ms());
            for op in &ops {
                match op {
                    SidecarOp::Gone { id } => {
                        connection
                            .prepare_cached(
                                "UPDATE sidecar SET missing_since_ms = ?2, rev = ?3
                                 WHERE id = ?1 AND missing_since_ms IS NULL",
                            )?
                            .execute((id, now_ms, rev))?;
                    }
                    SidecarOp::Read {
                        path,
                        size,
                        mtime_ms,
                        bytes,
                    } => {
                        let existing: Option<i64> = connection
                            .prepare_cached(
                                "SELECT id FROM sidecar WHERE source_instance_id = ?1 AND rel_path = ?2",
                            )?
                            .query_row((source, path), |row| row.get(0))
                            .optional()?;
                        let sidecar = match existing {
                            Some(id) => id,
                            None => {
                                connection
                                    .prepare_cached(
                                        "INSERT INTO sidecar
                                           (source_instance_id, rel_path, size_bytes, mtime_ms,
                                            first_seen_at_ms, rev)
                                         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                                    )?
                                    .execute((source, path, signed(*size), mtime_ms, now_ms, rev))?;
                                connection.last_insert_rowid()
                            }
                        };
                        let hash = blake3::hash(bytes);
                        let had: Option<i64> = connection
                            .prepare_cached(
                                "SELECT id FROM sidecar_revision WHERE sidecar_id = ?1 AND content_hash = ?2",
                            )?
                            .query_row((sidecar, hash.as_bytes()), |row| row.get(0))
                            .optional()?;
                        let revision = match had {
                            Some(id) => id,
                            None => {
                                connection
                                    .prepare_cached(
                                        "INSERT INTO sidecar_revision
                                           (sidecar_id, content_hash, content, observed_at_ms, rev)
                                         VALUES (?1, ?2, ?3, ?4, ?5)",
                                    )?
                                    .execute((sidecar, hash.as_bytes(), bytes, now_ms, rev))?;
                                connection.last_insert_rowid()
                            }
                        };
                        connection
                            .prepare_cached(
                                "UPDATE sidecar SET current_revision_id = ?2, size_bytes = ?3,
                                   mtime_ms = ?4, missing_since_ms = NULL, rev = ?5
                                 WHERE id = ?1",
                            )?
                            .execute((sidecar, revision, signed(*size), mtime_ms, rev))?;
                    }
                }
            }
            set_setting(write.connection(), LAYOUTS_PENDING, "true", rev)?;
            Ok(receipt)
        })
    }

    /// Whether the layouts are due: a write since the last derivation said
    /// so, or, in a library that has sidecars, they were derived by another
    /// layout version than this one.
    pub(crate) fn layouts_are_stale(&self) -> Result<bool, Error> {
        self.read(|connection| {
            let (pending, any): (bool, bool) = connection.query_row(
                "SELECT EXISTS (SELECT 1 FROM library_setting WHERE key = ?1),
                        EXISTS (SELECT 1 FROM sidecar)",
                [LAYOUTS_PENDING],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            Ok(pending || (any && stored_version(connection)? != Some(i64::from(LAYOUT_VERSION))))
        })
    }

    /// Derives every disc layout again from the stored sidecars and files,
    /// and writes what differs from the stored ones, in one transaction,
    /// with `receipt` as the write's stored result. It is run only when
    /// they are due, so it always writes: at least that they are no longer
    /// due.
    pub(crate) fn derive_layouts(
        &self,
        request: WriteRequest,
        receipt: String,
    ) -> Result<Written, Error> {
        self.write(request, |write| {
            apply(write)?;
            Ok(receipt)
        })
    }

    /// `corrections.discs`: the stored layouts, of the whole library or of
    /// one folder.
    pub fn discs(&self, params: &DiscsParams) -> Result<DiscList, Error> {
        let folder = params.folder.clone();
        self.read_snapshot(move |connection| {
            let revision = revision(connection)?;
            let wanted = |path: &str| folder.as_deref().is_none_or(|folder| folder == path);
            // Folders that hold audio now.
            let mut with_audio: HashSet<(i64, String)> = HashSet::new();
            for (source, path) in rows(
                connection,
                "SELECT source_instance_id, provider_key FROM source_item
                 WHERE item_kind = 'file' AND retired_at_ms IS NULL AND missing_since_ms IS NULL",
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )? {
                let path = folder_of(&path);
                if wanted(path) {
                    with_audio.insert((source, path.to_owned()));
                }
            }
            let mut files: HashMap<i64, Vec<DiscFile>> = HashMap::new();
            if let Some(folder) = &folder {
                let mut statement = connection.prepare(
                    "SELECT edi.evidence_disc_id, edi.position, si.provider_key
                     FROM evidence_disc_item edi
                     JOIN evidence_disc ed ON ed.id = edi.evidence_disc_id
                     JOIN source_item si ON si.id = edi.source_item_id
                     WHERE ed.folder = ?1
                     ORDER BY edi.evidence_disc_id, edi.position",
                )?;
                let placed = statement.query_map([folder], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, u32>(1)?, row.get::<_, String>(2)?))
                })?;
                for found in placed {
                    let (disc, position, path) = found?;
                    files.entry(disc).or_default().push(DiscFile { position, path });
                }
            }
            let mut statement = connection.prepare(
                "SELECT ed.id, ed.source_instance_id, src.name, ed.folder, ed.disc_id,
                        ed.track_offsets_json, ed.lead_out, ed.found_json, ed.provisional, ed.whole_file,
                        (SELECT count(*) FROM evidence_disc_item edi WHERE edi.evidence_disc_id = ed.id)
                 FROM evidence_disc ed JOIN source_instance src ON src.id = ed.source_instance_id
                 WHERE ?1 IS NULL OR ed.folder = ?1
                 ORDER BY src.name, ed.folder, ed.disc_id",
            )?;
            let found = statement.query_map([&folder], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, bool>(8)?,
                    row.get::<_, bool>(9)?,
                    row.get::<_, i64>(10)?,
                ))
            })?;
            let mut discs = Vec::new();
            let mut laid_out: HashSet<(i64, String)> = HashSet::new();
            for row in found {
                let (id, source, root, folder, disc_id, offsets, lead_out, found, provisional, whole, placed) =
                    row?;
                let offsets: Vec<u64> = serde_json::from_str(&offsets)?;
                let ways: Vec<Found> = serde_json::from_str(&found)?;
                let mut methods: Vec<LayoutMethod> =
                    ways.iter().filter_map(|way| method(&way.method)).collect();
                methods.dedup();
                let tracks = u32::try_from(offsets.len()).unwrap_or(u32::MAX);
                if !provisional && with_audio.contains(&(source, folder.clone())) {
                    laid_out.insert((source, folder.clone()));
                }
                discs.push(Disc {
                    root,
                    folder,
                    disc_id,
                    tracks,
                    sectors: unsigned(lead_out),
                    methods,
                    provisional,
                    whole_file: whole,
                    matched: whole || (placed > 0 && placed == i64::from(tracks)),
                    files: files.remove(&id).unwrap_or_default(),
                });
            }
            Ok(DiscList {
                library_revision: revision,
                folders: with_audio.len() as u64,
                with_layout: laid_out.len() as u64,
                discs,
            })
        })
    }
}
