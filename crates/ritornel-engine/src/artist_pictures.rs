//! Artist pictures: the portrait a picture folder's manifest chose for each
//! artist, matched to the library's artists and read in place. See "Catalog
//! projection and displayed metadata" in docs/internals/identity.md.
//!
//! The folder is format 1, as its README sets out: a folder for each artist
//! named by their MusicBrainz ID, `by-name/<safe name>/` for an artist with
//! none, and in each a `manifest.json` whose `chosen.portrait` names a file
//! beside it. Nothing else of the folder is read, and nothing is written
//! there.
//!
//! An import reads the files outside the write, then replaces the folder's
//! rows in one transaction: what the manifests no longer choose is dropped.
//! Every file is read through [`read_inside`], so a link never leads out of
//! the folder and a pipe is never waited on.

use std::collections::{HashMap, HashSet};
use std::io::Read as _;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use rusqlite::{Connection, OptionalExtension};
use serde::Deserialize;
use unicode_normalization::UnicodeNormalization;

use crate::media::{image_size, mime_of};
use crate::resolve_store::{artist_mbids, rows};
use crate::{Error, LibraryStore, WriteRequest, Written};

/// What an import of a picture folder found and did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PicturesImported {
    /// The artists with a record in the library.
    pub artists: u64,
    /// Those whose portrait was found by their MusicBrainz ID.
    pub by_mbid: u64,
    /// Those with no ID whose portrait was found by their name.
    pub by_name: u64,
    /// Those the folder has no portrait for.
    pub without: u64,
    /// Portraits a manifest chose that could not be read: a file that is
    /// missing, or not a JPEG or a PNG.
    pub unreadable: u64,
    /// Portraits of this folder the library had and the folder no longer
    /// chooses.
    pub dropped: u64,
    /// Artists with no ID whose names the folder makes the same as
    /// another's, whose portrait names neither: counted with `without`.
    pub ambiguous: u64,
}

/// The most a manifest may be. The real ones are a few kilobytes.
pub(crate) const MANIFEST_CAP: u64 = 1 << 20;
/// The most the index may be. The real one is a few hundred kilobytes.
const INDEX_CAP: u64 = 16 << 20;
/// The most a portrait may be, imported or served.
pub(crate) const PORTRAIT_CAP: u64 = 32 << 20;

/// Reads the regular file at `rel`, a path of plain names under `root`, of
/// at most `cap` bytes. Nothing outside `root` is reached through a link,
/// even if the folder changes meanwhile (`platform::open_inside`), and the file is
/// checked by its handle, so a pipe or a device is refused rather than read.
pub(crate) fn read_inside(root: &Path, rel: &str, cap: u64) -> std::io::Result<Vec<u8>> {
    let parts: Vec<&str> = rel.split('/').collect();
    if parts
        .iter()
        .any(|part| part.is_empty() || *part == "." || *part == "..")
    {
        return Err(refused("not a plain path"));
    }
    let Some((name, dirs)) = parts.split_last() else {
        return Err(refused("no path"));
    };
    let file = crate::platform::open_inside(root, dirs, name)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(refused("not a regular file"));
    }
    if meta.len() > cap {
        return Err(refused("larger than it may be"));
    }
    let mut bytes = Vec::new();
    file.take(cap + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > cap {
        return Err(refused("larger than it may be"));
    }
    Ok(bytes)
}

/// An error that says the folder holds something that may not be read.
pub(crate) fn refused(why: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, why.to_owned())
}

/// A folder found to be a picture folder of format 1. An import takes one,
/// so the folder is checked once, before its root is recorded, and not
/// again after: a folder changed between the two is not refused late.
#[derive(Debug)]
pub struct PictureFolder(std::path::PathBuf, Option<(u64, u64)>);

/// The picture folder at `folder`, if its `index.json` says format 1.
pub fn check_picture_folder(folder: &Path) -> Result<PictureFolder, Error> {
    let index = read_inside(folder, "index.json", INDEX_CAP)
        .map_err(|_| Error::Invalid("that folder has no index.json: it is not a picture folder"))?;
    #[derive(Deserialize)]
    struct Index {
        format: u32,
    }
    match serde_json::from_slice::<Index>(&index) {
        // Which folder it is, so that a disk unmounted after the check is
        // told from a folder whose portraits were taken away.
        Ok(Index { format: 1 }) => Ok(PictureFolder(
            folder.to_path_buf(),
            crate::walk::folder_id(folder),
        )),
        _ => Err(Error::Invalid("that picture folder is not of format 1")),
    }
}

/// What `manifest.json` says that is read here. Every other field is
/// ignored, as the format asks.
#[derive(Deserialize)]
struct Manifest {
    format: u32,
    #[serde(default)]
    artist: Option<ManifestArtist>,
    #[serde(default)]
    files: Vec<FileRecord>,
    #[serde(default)]
    chosen: HashMap<String, Chosen>,
}

#[derive(Deserialize)]
struct ManifestArtist {
    #[serde(default)]
    name: Option<String>,
}

#[derive(Deserialize)]
struct FileRecord {
    file: String,
    #[serde(default)]
    source: Option<String>,
}

#[derive(Deserialize)]
struct Chosen {
    file: String,
    #[serde(default)]
    by: Option<String>,
}

/// A chosen portrait, read.
struct Read {
    /// Relative to the folder.
    path: String,
    bytes: Vec<u8>,
    source: Option<String>,
    chosen_by: Option<String>,
    /// The artist the manifest says it is of.
    named: Option<String>,
}

/// A portrait found for an artist, read and hashed.
struct Found {
    artist: i64,
    by_mbid: bool,
    /// Relative to the folder.
    path: String,
    hash: Vec<u8>,
    mime: &'static str,
    size: Option<(u32, u32)>,
    bytes: i64,
    source: Option<String>,
    chosen_by: Option<String>,
}

/// An artist's folder name for an artist with no ID, as the picture folder
/// makes it: NFC, trimmed, with `/ \ : * ? " < > |` and control characters
/// as `_`, no leading dots, `_` if nothing is left, and at most 120
/// characters.
pub(crate) fn safe_name(name: &str) -> String {
    let normal: String = name.nfc().collect();
    let replaced: String = normal
        .trim()
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c < '\u{20}' {
                '_'
            } else {
                c
            }
        })
        .collect();
    let kept = replaced.trim_start_matches('.');
    let kept = if kept.is_empty() { "_" } else { kept };
    kept.chars().take(120).collect()
}

/// Whether `text` is a MusicBrainz ID as the folder names one: lower-case
/// hex in the 8-4-4-4-12 form.
fn is_mbid(text: &str) -> bool {
    text.len() == 36
        && text.char_indices().all(|(at, c)| match at {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_digit() || ('a'..='f').contains(&c),
        })
}

/// The portrait an artist's folder chose, read: `Ok(None)` when it has
/// none, `Err(())` when it chose one that cannot be read.
/// Why a portrait was not read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unread {
    /// What the folder holds is not a portrait that can be used: a
    /// manifest of another format, a name out of the folder, a link.
    Content,
    /// The system would not let it be read, as a disk that went away does.
    /// The portrait the library has is kept.
    Unreachable,
}

/// Whether a failed read says something of the folder, or only of the
/// system.
fn unread(error: &std::io::Error) -> Unread {
    // A link where a folder or the file should be.
    if error.kind() == std::io::ErrorKind::InvalidInput || crate::platform::met_a_link(error) {
        Unread::Content
    } else {
        Unread::Unreachable
    }
}

fn portrait(folder: &Path, dir: &str) -> Result<Option<Read>, Unread> {
    let text = match read_inside(folder, &format!("{dir}/manifest.json"), MANIFEST_CAP) {
        Ok(text) => text,
        // No manifest: the artist has no portrait chosen. Whether the
        // folder itself is still there is checked once every read is done.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(unread(&error)),
    };
    let Ok(manifest) = serde_json::from_slice::<Manifest>(&text) else {
        return Err(Unread::Content);
    };
    if manifest.format != 1 {
        return Err(Unread::Content);
    }
    let mut chosen = manifest.chosen;
    let Some(choice) = chosen.remove("portrait") else {
        return Ok(None);
    };
    // A plain name beside the manifest, never a path out of the folder.
    let file = &choice.file;
    if file.is_empty() || file.starts_with('.') || file.contains(['/', '\\']) {
        return Err(Unread::Content);
    }
    let path = format!("{dir}/{file}");
    // A chosen file that cannot be read is still chosen.
    let bytes = read_inside(folder, &path, PORTRAIT_CAP).map_err(|error| unread(&error))?;
    let source = manifest
        .files
        .into_iter()
        .find(|record| record.file == *file)
        .and_then(|record| record.source);
    Ok(Some(Read {
        path,
        bytes,
        source,
        chosen_by: choice.by,
        named: manifest.artist.and_then(|artist| artist.name),
    }))
}

/// An artist with a record, as the Artists list has them.
struct Artist {
    id: i64,
    name: Option<String>,
    /// The MusicBrainz ID they are known by, in lower case.
    mbid: Option<String>,
}

/// The names of [`artists`], for tests.
#[cfg(test)]
pub(crate) fn artist_names(connection: &Connection) -> Result<Vec<Option<String>>, Error> {
    Ok(artists(connection)?
        .into_iter()
        .map(|artist| artist.name)
        .collect())
}

/// Each artist with a record, with their name and ID.
fn artists(connection: &Connection) -> Result<Vec<Artist>, Error> {
    let mbids = artist_mbids(connection)?;
    // An ID claimed for a row merged into another is the other's.
    let mut by_canonical: HashMap<i64, String> = HashMap::new();
    for (row, canonical) in rows(connection, "SELECT id, canonical_id FROM artist", |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
    })? {
        if let Some(mbid) = mbids.get(&row) {
            let held = by_canonical
                .entry(canonical)
                .or_insert_with(|| mbid.clone());
            if row == canonical {
                held.clone_from(mbid);
            }
        }
    }
    // As the Artists list has them: by a record a file reaches.
    let found = rows(
        connection,
        &format!(
            "SELECT a.id, a.display_name FROM artist a
             WHERE a.redirect_to_id IS NULL
               AND a.id IN (
                 SELECT ca.canonical_id
                 FROM release r
                 CROSS JOIN artist_credit_name acn ON acn.artist_credit_id = r.display_artist_credit_id
                 CROSS JOIN artist ca ON ca.id = acn.artist_id
                 WHERE r.redirect_to_id IS NULL AND {})
             ORDER BY a.id",
            crate::list::REACHED_RELEASE_R
        ),
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?)),
    )?;
    Ok(found
        .into_iter()
        .map(|(id, name)| Artist {
            id,
            name,
            mbid: by_canonical.get(&id).map(|mbid| mbid.to_lowercase()),
        })
        .collect())
}

/// A picture folder the library knows, as `artists.pictureFolders` lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownPictureFolder {
    /// Its name, which this server's `server.db` maps to a path if it is
    /// on this server.
    pub name: String,
    /// Whether it is on this server.
    pub on_this_server: bool,
    /// The portraits the library holds from it.
    pub portraits: u64,
}

/// The last import of a picture folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastPictureImport {
    /// When it was committed.
    pub at_ms: i64,
    /// What it answered, while the library keeps its receipt.
    pub result_json: Option<String>,
}

impl LibraryStore {
    /// Every picture folder, in the order they were first imported, and
    /// the last import of any that wrote something, asked for or started
    /// on its own. The import's time is its revision's, which is kept; its
    /// counts are its receipt's, which may be pruned, and which an import
    /// started on its own has none of.
    pub fn picture_folders_and_last_import(
        &self,
    ) -> Result<(Vec<KnownPictureFolder>, Option<LastPictureImport>), Error> {
        let hub = self.hub();
        self.read(|connection| {
            let mut statement = connection.prepare(
                "SELECT f.name, s.public_id,
                        (SELECT count(*) FROM artist_picture p WHERE p.folder_id = f.id)
                 FROM picture_folder f JOIN server s ON s.id = f.server_id
                 ORDER BY f.id",
            )?;
            let folders = statement
                .query_map([], |row| {
                    Ok(KnownPictureFolder {
                        name: row.get(0)?,
                        on_this_server: crate::Id::from_bytes(row.get(1)?) == hub,
                        portraits: u64::try_from(row.get::<_, i64>(2)?).unwrap_or(0),
                    })
                })?
                .collect::<Result<_, _>>()?;
            let last = connection
                .query_row(
                    "SELECT r.committed_at_ms, c.result_json FROM library_revision r
                     LEFT JOIN library_command c ON c.command_id = r.command_id
                     WHERE r.method IN ('artists.importPictures', 'artists.refreshPictures')
                     ORDER BY r.rev DESC LIMIT 1",
                    [],
                    |row| {
                        Ok(LastPictureImport {
                            at_ms: row.get(0)?,
                            result_json: row.get(1)?,
                        })
                    },
                )
                .optional()?;
            Ok((folders, last))
        })
    }

    /// Imports the portraits of the picture folder `name`, `folder` on
    /// this server, for the library's artists: by MusicBrainz ID, and by
    /// name for an artist with none. The folder's earlier portraits that
    /// are no longer chosen are dropped. `receipt` is given what was done
    /// and the revision, and returns the result to store and answer with.
    pub fn import_artist_pictures<R>(
        &self,
        request: WriteRequest,
        name: &str,
        folder: &PictureFolder,
        receipt: R,
    ) -> Result<Written, Error>
    where
        R: FnOnce(&PicturesImported, i64) -> String + Send + 'static,
    {
        self.import_pictures(request, name, folder, false, receipt)
    }

    /// The names of this hub's picture folders: each root an import has
    /// read.
    pub fn picture_folders(&self) -> Result<Vec<String>, Error> {
        let hub = self.hub();
        self.read(move |connection| {
            let mut statement = connection.prepare(
                "SELECT f.name FROM picture_folder f JOIN server s ON s.id = f.server_id
                 WHERE s.public_id = ?1 ORDER BY f.id",
            )?;
            let rows = statement.query_map([hub.as_bytes()], |row| row.get(0))?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
    }

    /// Imports a picture folder again, as the hub does when the folder
    /// changes: as [`import_artist_pictures`](Self::import_artist_pictures),
    /// except that a folder that would change nothing is not written, and
    /// takes no revision. `None` then.
    pub fn refresh_artist_pictures(
        &self,
        request: WriteRequest,
        name: &str,
        folder: &PictureFolder,
    ) -> Result<Option<PicturesImported>, Error> {
        let slot = Arc::new(Mutex::new(PicturesImported::default()));
        let kept = Arc::clone(&slot);
        let receipt = move |counts: &PicturesImported, _| {
            *kept.lock().unwrap_or_else(PoisonError::into_inner) = *counts;
            String::new()
        };
        match self.import_pictures(request, name, folder, true, receipt) {
            Ok(_) => Ok(Some(*slot.lock().unwrap_or_else(PoisonError::into_inner))),
            Err(Error::Unchanged) => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn import_pictures<R>(
        &self,
        request: WriteRequest,
        name: &str,
        folder: &PictureFolder,
        only_if_changed: bool,
        receipt: R,
    ) -> Result<Written, Error>
    where
        R: FnOnce(&PicturesImported, i64) -> String + Send + 'static,
    {
        let (folder, walked) = (folder.0.as_path(), folder.1);
        let artists = self.read(artists)?;
        // Names with no ID that the folder makes the same: a portrait there
        // is only an artist's whose name its manifest gives exactly.
        let mut sharing: HashMap<String, Vec<String>> = HashMap::new();
        for artist in &artists {
            if let (None, Some(name)) = (&artist.mbid, &artist.name) {
                sharing
                    .entry(safe_name(name))
                    .or_default()
                    .push(name.clone());
            }
        }
        let mut counts = PicturesImported {
            artists: artists.len() as u64,
            ..PicturesImported::default()
        };
        let mut found = Vec::new();
        // Artists whose portrait could not be reached: what the library has
        // of them is kept.
        let mut held = HashSet::new();
        for artist in artists {
            let (dir, by_mbid) = match (&artist.mbid, &artist.name) {
                (Some(mbid), _) if is_mbid(mbid) => (mbid.clone(), true),
                (None, Some(name)) => (safe_name(name), false),
                _ => {
                    counts.without += 1;
                    continue;
                }
            };
            let shared = sharing
                .get(&dir)
                .filter(|names| !by_mbid && names.len() > 1);
            let dir = if by_mbid {
                dir
            } else {
                format!("by-name/{dir}")
            };
            match portrait(folder, &dir) {
                Ok(Some(read))
                    if shared.is_some_and(|names| {
                        let named = read.named.as_deref();
                        named != artist.name.as_deref()
                            || names
                                .iter()
                                .filter(|name| Some(name.as_str()) == named)
                                .count()
                                != 1
                    }) =>
                {
                    counts.ambiguous += 1;
                    counts.without += 1;
                }
                Ok(Some(read)) => {
                    let mime = mime_of(&read.bytes);
                    if !matches!(mime, "image/jpeg" | "image/png") {
                        counts.unreadable += 1;
                        counts.without += 1;
                        continue;
                    }
                    if by_mbid {
                        counts.by_mbid += 1;
                    } else {
                        counts.by_name += 1;
                    }
                    found.push(Found {
                        artist: artist.id,
                        by_mbid,
                        hash: blake3::hash(&read.bytes).as_bytes().to_vec(),
                        mime,
                        size: image_size(&read.bytes),
                        bytes: i64::try_from(read.bytes.len()).unwrap_or(i64::MAX),
                        path: read.path,
                        source: read.source,
                        chosen_by: read.chosen_by,
                    });
                }
                Ok(None) => counts.without += 1,
                Err(why) => {
                    if why == Unread::Unreachable {
                        held.insert(artist.id);
                    }
                    counts.unreadable += 1;
                    counts.without += 1;
                }
            }
        }
        // The folder went away while it was read: what it seemed to say is
        // not written, and nothing is dropped.
        if walked.is_none() || crate::walk::folder_id(folder) != walked {
            return Err(Error::RootOffline {
                path: folder.to_owned(),
            });
        }

        let (name, hub) = (name.to_owned(), self.hub());
        self.write(request, move |write| {
            let (connection, rev) = (write.connection(), write.rev());
            let mut changed = false;
            let folder_id: i64 = match connection
                .query_row(
                    "SELECT f.id FROM picture_folder f JOIN server s ON s.id = f.server_id
                     WHERE s.public_id = ?1 AND f.name = ?2",
                    (hub.as_bytes(), &name),
                    |row| row.get(0),
                )
                .optional()?
            {
                Some(id) => id,
                None => {
                    connection.execute(
                        "INSERT INTO picture_folder (public_id, server_id, name, created_at_ms, rev)
                         VALUES (?1, (SELECT id FROM server WHERE public_id = ?2), ?3, ?4, ?5)",
                        (write.new_id()?.as_bytes(), hub.as_bytes(), &name, write.now_ms(), rev),
                    )?;
                    changed = true;
                    connection.last_insert_rowid()
                }
            };
            let mut kept = held;
            for portrait in &found {
                // An artist merged away since the read is left for next time.
                let live: bool = connection.query_row(
                    "SELECT EXISTS (SELECT 1 FROM artist WHERE id = ?1 AND redirect_to_id IS NULL)",
                    [portrait.artist],
                    |row| row.get(0),
                )?;
                if !live {
                    continue;
                }
                kept.insert(portrait.artist);
                let picture = picture_row(write, portrait)?;
                let matched_by = if portrait.by_mbid { "mbid" } else { "name" };
                // Only a portrait that changed takes this revision.
                changed |= 0 < connection.execute(
                    "INSERT INTO artist_picture
                       (artist_id, kind, picture_id, folder_id, path, matched_by, source, chosen_by, rev)
                     VALUES (?1, 'portrait', ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                     ON CONFLICT (artist_id, kind) DO UPDATE SET
                       picture_id = excluded.picture_id, folder_id = excluded.folder_id,
                       path = excluded.path, matched_by = excluded.matched_by,
                       source = excluded.source, chosen_by = excluded.chosen_by, rev = excluded.rev
                     WHERE (picture_id, folder_id, path, matched_by, source, chosen_by)
                       IS NOT (excluded.picture_id, excluded.folder_id, excluded.path,
                               excluded.matched_by, excluded.source, excluded.chosen_by)",
                    (
                        portrait.artist,
                        picture,
                        folder_id,
                        &portrait.path,
                        matched_by,
                        &portrait.source,
                        &portrait.chosen_by,
                        rev,
                    ),
                )?;
            }
            let held: Vec<i64> = connection
                .prepare("SELECT artist_id FROM artist_picture WHERE folder_id = ?1")?
                .query_map([folder_id], |row| row.get(0))?
                .collect::<Result<_, _>>()?;
            let mut dropped = 0;
            for artist in held.into_iter().filter(|artist| !kept.contains(artist)) {
                dropped += connection.execute(
                    "DELETE FROM artist_picture WHERE folder_id = ?1 AND artist_id = ?2",
                    (folder_id, artist),
                )?;
            }
            counts.dropped = dropped as u64;
            if only_if_changed && !changed && dropped == 0 {
                return Err(Error::Unchanged);
            }
            Ok(receipt(&counts, rev))
        })
    }
}

/// The `picture` row of a portrait's bytes, made if the library has none.
fn picture_row(write: &crate::library_store::Write<'_>, portrait: &Found) -> Result<i64, Error> {
    let connection = write.connection();
    if let Some(row) = connection
        .query_row(
            "SELECT id FROM picture WHERE content_hash = ?1",
            [&portrait.hash],
            |row| row.get(0),
        )
        .optional()?
    {
        return Ok(row);
    }
    connection.execute(
        "INSERT INTO picture (public_id, content_hash, mime_type, width, height, size_bytes, rev)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        (
            write.new_id()?.as_bytes(),
            &portrait.hash,
            portrait.mime,
            portrait.size.map(|(width, _)| width),
            portrait.size.map(|(_, height)| height),
            portrait.bytes,
            write.rev(),
        ),
    )?;
    Ok(connection.last_insert_rowid())
}

#[cfg(test)]
mod tests;
