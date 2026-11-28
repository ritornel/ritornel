//! The folder family: a directory on a server's disk, or on a card in a
//! reader, behind the same trait as any player. It is the first
//! implementation and the one every test runs against. See "The folder
//! target" in docs/internals/device-sync.md.
//!
//! Every path is resolved a part at a time and must stay under the root: a
//! symbolic link inside it is never followed or written through. Ritornel
//! writes inside the music root and its own `.ritornel/` folder, and
//! nowhere else.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use ritornel_device::{
    Capabilities, Error, ErrorKind, ObjectId, ObjectInfo, Plays, Probe, Put, RelPath, Session,
    Signature, Storage, Transport,
};

/// Ritornel's own folder under a target's root: the marker, and puts in
/// flight.
const OWN: &str = ".ritornel";
const MARKER: &str = "root.json";
const PARTIALS: &str = "tmp";

/// How fast a folder is assumed to be written before a sync has measured
/// it: a disk, not a player's flash over USB.
const WRITE_BPS: u64 = 60 * 1024 * 1024;

/// An open folder target.
#[derive(Debug)]
pub struct FolderSession {
    root: PathBuf,
    /// Whether it is a drive or a card that is ejected, rather than a
    /// folder that is always there.
    removable: bool,
}

fn failed(doing: &str, error: &io::Error) -> Error {
    let kind = match error.kind() {
        io::ErrorKind::NotFound => ErrorKind::NotFound,
        io::ErrorKind::StorageFull | io::ErrorKind::QuotaExceeded => ErrorKind::Full,
        _ => ErrorKind::Io,
    };
    // The system's own words, which name no path.
    Error::new(kind, format!("{doing}: {}", io::Error::from(error.kind())))
}

fn modified_ms(metadata: &fs::Metadata) -> Option<i64> {
    let since = metadata
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?;
    i64::try_from(since.as_millis()).ok()
}

impl FolderSession {
    /// Opens the folder at `root`, which must be there. The path is taken
    /// as given: the caller has resolved it and checked where it lies.
    /// `removable` says it is a drive or a card, which is ejected.
    pub fn open(root: &Path, removable: bool) -> Result<Self, Error> {
        let session = Self {
            root: root.to_owned(),
            removable,
        };
        session.here()?;
        Ok(session)
    }

    /// Fails as gone when the folder is no longer there: a card pulled, a
    /// disk unmounted.
    fn here(&self) -> Result<(), Error> {
        match fs::symlink_metadata(&self.root) {
            Ok(found) if found.is_dir() => Ok(()),
            _ => Err(Error::new(ErrorKind::Gone, "the folder is not there")),
        }
    }

    /// Where `path` is on this machine, having checked that no part of it
    /// that exists is a link. The last part may be missing, and so may the
    /// folders above it.
    fn resolve(&self, path: &RelPath) -> Result<PathBuf, Error> {
        self.here()?;
        let mut at = self.root.clone();
        for part in path.parts() {
            at.push(part);
            match fs::symlink_metadata(&at) {
                Ok(found) if found.file_type().is_symlink() => {
                    return Err(Error::new(
                        ErrorKind::Unsafe,
                        format!("{path} passes through a link"),
                    ));
                }
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(failed("could not look at the folder", &error)),
            }
        }
        Ok(at)
    }

    fn own(&self, name: &str) -> Result<RelPath, Error> {
        RelPath::new(format!("{OWN}/{name}"))
            .map_err(|error| Error::new(ErrorKind::Unsafe, error.to_string()))
    }

    fn path_of<'a>(&self, id: &'a ObjectId) -> Result<&'a RelPath, Error> {
        id.path()
            .ok_or_else(|| Error::new(ErrorKind::Unsupported, "a folder names its files by path"))
    }

    fn info(&self, path: &RelPath, metadata: &fs::Metadata) -> ObjectInfo {
        ObjectInfo {
            id: ObjectId::Path(path.clone()),
            path: path.clone(),
            size: metadata.len(),
            modified_ms: modified_ms(metadata),
        }
    }

    fn walk(&self, folder: &RelPath, found: &mut Vec<ObjectInfo>) -> Result<(), Error> {
        let at = self.resolve(folder)?;
        let entries = match fs::read_dir(&at) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(failed("could not list the folder", &error)),
        };
        for entry in entries {
            let entry = entry.map_err(|error| failed("could not list the folder", &error))?;
            // A name that is no plain UTF-8 part is nothing Ritornel wrote
            // or will write over: it is left unlisted.
            let Some(path) = entry
                .file_name()
                .to_str()
                .and_then(|name| folder.join(name).ok())
            else {
                continue;
            };
            // `file_type` and `symlink_metadata` do not follow a link.
            let kind = entry
                .file_type()
                .map_err(|error| failed("could not list the folder", &error))?;
            if kind.is_dir() {
                self.walk(&path, found)?;
            } else if kind.is_file() {
                let metadata = fs::symlink_metadata(entry.path())
                    .map_err(|error| failed("could not list the folder", &error))?;
                found.push(self.info(&path, &metadata));
            }
        }
        Ok(())
    }

    /// Removes the folders a removed file left empty, up to the root.
    fn prune(&self, path: &RelPath) {
        let mut parts: Vec<&str> = path.parts().collect();
        while parts.len() > 1 {
            parts.pop();
            let mut at = self.root.clone();
            at.extend(&parts);
            // A folder with anything in it stays, and ends the walk.
            if fs::remove_dir(&at).is_err() {
                break;
            }
        }
    }
}

impl Session for FolderSession {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            transport: Transport::Folder,
            plays: Plays::None,
            ejectable: self.removable,
            can_sample: true,
            codecs: vec!["flac", "mp3", "opus"],
            cover_sizes: vec![500, 300],
            path_bytes: 250,
            write_bps: WRITE_BPS,
        }
    }

    fn probe(&mut self) -> Result<Probe, Error> {
        self.here()?;
        Ok(Probe {
            icon: Some("folder".to_owned()),
            ..Probe::default()
        })
    }

    fn storage(&mut self) -> Result<Storage, Error> {
        self.here()?;
        let (capacity_bytes, free_bytes) = crate::platform::disk_bytes(&self.root)
            .ok_or_else(|| Error::new(ErrorKind::Io, "could not read the disk's size"))?;
        Ok(Storage {
            capacity_bytes,
            free_bytes,
            cluster_bytes: None,
        })
    }

    fn inventory(&mut self, music_root: &RelPath) -> Result<Vec<ObjectInfo>, Error> {
        let mut found = Vec::new();
        self.walk(music_root, &mut found)?;
        Ok(found)
    }

    fn stat(&mut self, id: &ObjectId) -> Result<Option<ObjectInfo>, Error> {
        let path = self.path_of(id)?;
        match fs::symlink_metadata(self.resolve(path)?) {
            Ok(found) if found.is_file() => Ok(Some(self.info(path, &found))),
            Ok(_) => Ok(None),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(failed("could not look at the file", &error)),
        }
    }

    fn signature(&mut self, id: &ObjectId) -> Result<Option<Signature>, Error> {
        let Some(info) = self.stat(id)? else {
            return Ok(None);
        };
        let at = self.resolve(self.path_of(id)?)?;
        let mut file = File::open(at).map_err(|error| failed("could not read the file", &error))?;
        let sampled = crate::transcode::sampled_hash(&mut file, info.size)
            .map_err(|error| failed("could not read the file", &error))?;
        Ok(Some(Signature {
            size: info.size,
            sampled: *sampled.as_bytes(),
        }))
    }

    fn put(
        &mut self,
        put: &Put<'_>,
        bytes: &mut dyn Read,
        sent: &mut dyn FnMut(u64) -> bool,
    ) -> Result<ObjectInfo, Error> {
        let place = self.resolve(put.path)?;
        let partials = self.resolve(&self.own(PARTIALS)?)?;
        fs::create_dir_all(&partials)
            .map_err(|error| failed("could not make room for the file", &error))?;
        // Written under a temporary name, which goes with the handle
        // unless the file is published.
        let mut part = tempfile::Builder::new()
            .suffix(".part")
            .tempfile_in(&partials)
            .map_err(|error| failed("could not start the file", &error))?;
        let mut hasher = blake3::Hasher::new();
        let mut written = 0u64;
        let mut block = vec![0; 256 * 1024];
        loop {
            let read = match bytes.read(&mut block) {
                Ok(0) => break,
                Ok(read) => read,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(failed("could not read the file to send", &error)),
            };
            let chunk = block.get(..read).unwrap_or_default();
            hasher.update(chunk);
            part.write_all(chunk)
                .map_err(|error| failed("could not write the file", &error))?;
            written += read as u64;
            if !sent(written) {
                return Err(Error::new(ErrorKind::Stopped, "stopped part way"));
            }
        }
        if written != put.len || *hasher.finalize().as_bytes() != put.digest {
            return Err(Error::new(
                ErrorKind::Corrupt,
                "the file written is not the file that was sent",
            ));
        }
        part.as_file()
            .sync_all()
            .map_err(|error| failed("could not flush the file", &error))?;
        if let Some(parent) = place.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| failed("could not make the file's folder", &error))?;
        }
        // The folders just made are under the root: look again for a link.
        let place = self.resolve(put.path)?;
        match put.replaces {
            Some((old, expect)) => {
                // The new copy is whole before the old file is touched, and
                // the old file must still be the one that was recorded.
                match self.signature(old)? {
                    Some(found) if found == expect => {}
                    Some(_) => {
                        return Err(Error::new(
                            ErrorKind::Changed,
                            "the file on the device is not the one Ritornel wrote",
                        ));
                    }
                    None => {}
                }
                if self.path_of(old)? != put.path && self.stat(old)?.is_some() {
                    return Err(Error::new(
                        ErrorKind::Unsupported,
                        "a folder replaces a file at its own path",
                    ));
                }
                part.persist(&place)
                    .map_err(|error| failed("could not publish the file", &error.error))?;
            }
            None => {
                // An add never overwrites. Where the file system cannot
                // refuse an occupied name itself, the name is checked and
                // then taken: two steps, with Ritornel the one writer.
                if fs::symlink_metadata(&place).is_ok() {
                    return Err(Error::new(
                        ErrorKind::Occupied,
                        format!("{} is already there", put.path),
                    ));
                }
                if let Err(refused) = part.persist_noclobber(&place) {
                    if refused.error.kind() == io::ErrorKind::AlreadyExists {
                        return Err(Error::new(
                            ErrorKind::Occupied,
                            format!("{} is already there", put.path),
                        ));
                    }
                    refused
                        .file
                        .persist(&place)
                        .map_err(|error| failed("could not publish the file", &error.error))?;
                }
            }
        }
        let metadata = fs::symlink_metadata(&place)
            .map_err(|error| failed("could not look at the file written", &error))?;
        Ok(self.info(put.path, &metadata))
    }

    fn remove(&mut self, id: &ObjectId, expect: Signature) -> Result<bool, Error> {
        let path = self.path_of(id)?.clone();
        match self.signature(id)? {
            None => return Ok(false),
            Some(found) if found != expect => {
                return Err(Error::new(
                    ErrorKind::Changed,
                    "the file on the device is not the one Ritornel wrote",
                ));
            }
            Some(_) => {}
        }
        fs::remove_file(self.resolve(&path)?)
            .map_err(|error| failed("could not remove the file", &error))?;
        self.prune(&path);
        Ok(true)
    }

    fn marker(&mut self) -> Result<Option<Vec<u8>>, Error> {
        match fs::read(self.resolve(&self.own(MARKER)?)?) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(failed("could not read the marker", &error)),
        }
    }

    fn set_marker(&mut self, marker: Option<&[u8]>) -> Result<(), Error> {
        let place = self.resolve(&self.own(MARKER)?)?;
        let Some(marker) = marker else {
            return match fs::remove_file(&place) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => {
                    Err(failed("could not remove the marker", &error))
                }
                _ => Ok(()),
            };
        };
        let folder = self.resolve(
            &RelPath::new(OWN).map_err(|error| Error::new(ErrorKind::Unsafe, error.to_string()))?,
        )?;
        let write = |folder: &Path| -> io::Result<()> {
            fs::create_dir_all(folder)?;
            let mut part = tempfile::NamedTempFile::new_in(folder)?;
            part.write_all(marker)?;
            part.as_file().sync_all()?;
            part.persist(&place).map_err(|error| error.error)?;
            Ok(())
        };
        write(&folder).map_err(|error| failed("could not write the marker", &error))
    }

    fn clear_partials(&mut self) -> Result<(), Error> {
        let partials = self.resolve(&self.own(PARTIALS)?)?;
        let entries = match fs::read_dir(&partials) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(failed("could not clear what was left", &error)),
        };
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|kind| kind.is_file()) {
                let _ = fs::remove_file(entry.path());
            }
        }
        Ok(())
    }

    fn close(self: Box<Self>, eject: bool) -> Result<(), Error> {
        if !eject {
            return Ok(());
        }
        if !self.removable {
            return Err(Error::new(
                ErrorKind::Unsupported,
                "a folder that is always here is not ejected",
            ));
        }
        // The drive the folder is on, whole: safe to unplug only once the
        // system says it has let go of it.
        crate::platform::eject(&self.root).map_err(|error| {
            let kind = match error.kind() {
                io::ErrorKind::Unsupported => ErrorKind::Unsupported,
                _ => ErrorKind::Io,
            };
            Error::new(kind, error.to_string())
        })
    }
}

/// How one folder lies to another.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overlap {
    /// They are the same folder.
    Same,
    /// The first is inside the second.
    Inside,
    /// The first holds the second.
    Holds,
}

/// Whether `folder` is, lies inside or holds `other`, each as the system
/// resolves it. A folder that cannot be resolved is compared as written.
pub fn overlap(folder: &Path, other: &Path) -> Option<Overlap> {
    let resolved = |path: &Path| fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
    let (folder, other) = (resolved(folder), resolved(other));
    if folder == other {
        Some(Overlap::Same)
    } else if folder.starts_with(&other) {
        Some(Overlap::Inside)
    } else if other.starts_with(&folder) {
        Some(Overlap::Holds)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rel(path: &str) -> RelPath {
        RelPath::new(path).unwrap()
    }

    fn put(session: &mut FolderSession, path: &str, bytes: &[u8]) -> Result<ObjectInfo, Error> {
        let path = rel(path);
        session.put(
            &Put {
                path: &path,
                len: bytes.len() as u64,
                digest: *blake3::hash(bytes).as_bytes(),
                replaces: None,
            },
            &mut &*bytes,
            &mut |_| true,
        )
    }

    fn signature(bytes: &[u8]) -> Signature {
        Signature {
            size: bytes.len() as u64,
            sampled: *blake3::hash(bytes).as_bytes(),
        }
    }

    #[test]
    fn a_put_is_checked_then_published_and_an_add_never_overwrites() {
        let scratch = tempfile::tempdir().unwrap();
        let mut session = FolderSession::open(scratch.path(), false).unwrap();
        let info = put(
            &mut session,
            "Music/Ada Vale/Low Tide/01 Shoreline.flac",
            b"one",
        )
        .unwrap();
        assert_eq!(info.size, 3);
        // What Ritornel did not write at a planned path is left.
        let again = put(
            &mut session,
            "Music/Ada Vale/Low Tide/01 Shoreline.flac",
            b"two",
        );
        assert_eq!(again.unwrap_err().kind, ErrorKind::Occupied);
        assert_eq!(
            fs::read(
                scratch
                    .path()
                    .join("Music/Ada Vale/Low Tide/01 Shoreline.flac")
            )
            .unwrap(),
            b"one"
        );
        // Bytes that are not the ones declared leave nothing by that name.
        let path = rel("Music/x.flac");
        let wrong = session.put(
            &Put {
                path: &path,
                len: 3,
                digest: [0; 32],
                replaces: None,
            },
            &mut &b"abc"[..],
            &mut |_| true,
        );
        assert_eq!(wrong.unwrap_err().kind, ErrorKind::Corrupt);
        assert!(!scratch.path().join("Music/x.flac").exists());
        // Nor does a put stopped part way, and nothing partial is left.
        let stopped = session.put(
            &Put {
                path: &path,
                len: 3,
                digest: *blake3::hash(b"abc").as_bytes(),
                replaces: None,
            },
            &mut &b"abc"[..],
            &mut |_| false,
        );
        assert_eq!(stopped.unwrap_err().kind, ErrorKind::Stopped);
        assert!(!scratch.path().join("Music/x.flac").exists());
        let left: Vec<_> = fs::read_dir(scratch.path().join(".ritornel/tmp"))
            .unwrap()
            .collect();
        assert!(left.is_empty());
        let listed = session.inventory(&rel("Music")).unwrap();
        assert_eq!(listed.len(), 1);
    }

    #[test]
    fn a_file_is_removed_or_replaced_only_while_it_is_the_one_recorded() {
        let scratch = tempfile::tempdir().unwrap();
        let mut session = FolderSession::open(scratch.path(), false).unwrap();
        put(&mut session, "Music/A/B/01 T.flac", b"written").unwrap();
        let id = ObjectId::Path(rel("Music/A/B/01 T.flac"));
        // Changed on the device since: nothing is deleted.
        fs::write(scratch.path().join("Music/A/B/01 T.flac"), b"edited!").unwrap();
        let refused = session.remove(&id, signature(b"written"));
        assert_eq!(refused.unwrap_err().kind, ErrorKind::Changed);
        let path = rel("Music/A/B/01 T.flac");
        let replace = |session: &mut FolderSession, expect: &[u8]| {
            session.put(
                &Put {
                    path: &path,
                    len: 3,
                    digest: *blake3::hash(b"new").as_bytes(),
                    replaces: Some((&id, signature(expect))),
                },
                &mut &b"new"[..],
                &mut |_| true,
            )
        };
        assert_eq!(
            replace(&mut session, b"written").unwrap_err().kind,
            ErrorKind::Changed
        );
        assert_eq!(
            fs::read(scratch.path().join("Music/A/B/01 T.flac")).unwrap(),
            b"edited!"
        );
        replace(&mut session, b"edited!").unwrap();
        assert!(session.remove(&id, signature(b"new")).unwrap());
        // Its empty folders go with it, and the root stays.
        assert!(!scratch.path().join("Music").exists());
        assert!(scratch.path().exists());
        assert!(!session.remove(&id, signature(b"new")).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn a_link_inside_the_root_is_neither_followed_nor_written_through() {
        let scratch = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret.flac"), b"elsewhere").unwrap();
        fs::create_dir(scratch.path().join("Music")).unwrap();
        std::os::unix::fs::symlink(outside.path(), scratch.path().join("Music/Linked")).unwrap();
        let mut session = FolderSession::open(scratch.path(), false).unwrap();
        assert!(session.inventory(&rel("Music")).unwrap().is_empty());
        let through = put(&mut session, "Music/Linked/new.flac", b"x");
        assert_eq!(through.unwrap_err().kind, ErrorKind::Unsafe);
        assert!(!outside.path().join("new.flac").exists());
        let id = ObjectId::Path(rel("Music/Linked/secret.flac"));
        assert_eq!(
            session
                .remove(&id, signature(b"elsewhere"))
                .unwrap_err()
                .kind,
            ErrorKind::Unsafe
        );
        assert!(outside.path().join("secret.flac").exists());
    }

    #[test]
    fn the_marker_is_kept_in_ritornels_own_folder_and_a_folder_gone_says_so() {
        let scratch = tempfile::tempdir().unwrap();
        let root = scratch.path().join("card");
        fs::create_dir(&root).unwrap();
        let mut session = FolderSession::open(&root, false).unwrap();
        assert_eq!(session.marker().unwrap(), None);
        session.set_marker(Some(b"{}")).unwrap();
        assert_eq!(session.marker().unwrap().as_deref(), Some(&b"{}"[..]));
        assert_eq!(fs::read(root.join(".ritornel/root.json")).unwrap(), b"{}");
        session.set_marker(None).unwrap();
        assert_eq!(session.marker().unwrap(), None);
        fs::remove_dir_all(&root).unwrap();
        assert_eq!(session.storage().unwrap_err().kind, ErrorKind::Gone);
        assert_eq!(
            overlap(&scratch.path().join("a/b"), &scratch.path().join("a")),
            Some(Overlap::Inside)
        );
        assert_eq!(
            overlap(&scratch.path().join("a"), &scratch.path().join("a/b")),
            Some(Overlap::Holds)
        );
        assert_eq!(
            overlap(&scratch.path().join("a"), &scratch.path().join("ab")),
            None
        );
    }
}
