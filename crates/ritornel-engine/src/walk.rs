//! Walking a root: every audio file under it, with its size and when it
//! was last modified, and each folder's own images. Nothing is opened, and
//! nothing is ever written: a root is read-only to the server. See
//! "Scanning one file" step 1 and "Rescans" in docs/design/identity.md.

use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::UNIX_EPOCH;

use crate::Error;
use crate::media::is_folder_image;

/// The extensions of the files a scan takes to be audio. A file Symphonia
/// cannot demux is here too, so that it is recorded as unreadable rather
/// than passed over.
const AUDIO_EXTENSIONS: [&str; 25] = [
    "flac", "mp3", "m4a", "m4b", "mp4", "aac", "ogg", "oga", "opus", "wav", "wave", "aif", "aiff",
    "aifc", "caf", "mka", "wma", "wv", "ape", "mpc", "dsf", "dff", "shn", "tta", "tak",
];

/// One file as the walk saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Seen {
    /// The path relative to the root, with `/` between its parts.
    pub(crate) path: String,
    pub(crate) size: u64,
    pub(crate) mtime_ms: i64,
}

/// What a walk found.
#[derive(Debug, Default)]
pub(crate) struct Walked {
    /// Every audio file, in the order of their paths.
    pub(crate) files: Vec<Seen>,
    /// Each folder's own images, by the folder's path relative to the
    /// root, in the order of their names.
    pub(crate) images: BTreeMap<String, Vec<Seen>>,
    /// Files and folders passed over, with why: a name that is not UTF-8,
    /// or a folder the server may not read.
    pub(crate) skipped: Vec<(String, &'static str)>,
    /// The folders the server may not read, relative to the root, in the
    /// order of their paths. What was known in them is not taken for gone.
    pub(crate) unreadable: Vec<String>,
    /// Which folder the root was when it was walked: its device and inode,
    /// or on Windows its volume's serial number and its file index. An
    /// unmounted disk leaves another folder at the same path.
    pub(crate) root_id: Option<(u64, u64)>,
}

/// Which folder is at `path` now, if one is: its device and inode, or on
/// Windows its volume's serial number and its file index. A disk mounted
/// again is another folder at the same path.
pub fn folder_id(path: &Path) -> Option<(u64, u64)> {
    let metadata = std::fs::metadata(path).ok()?;
    if !metadata.is_dir() {
        return None;
    }
    crate::platform::file_id(path, &metadata)
}

/// The file at `rel`, a path relative to `root` with `/` between its parts,
/// as the system names it. Each part is joined in turn: a root in the `\\?\`
/// form, as `canonicalize` gives on Windows, takes no `/` as a separator.
pub(crate) fn under(root: &Path, rel: &str) -> PathBuf {
    let mut path = root.to_owned();
    path.extend(rel.split('/').filter(|part| !part.is_empty()));
    path
}

/// The folder a file is in, relative to the root: empty at the root.
pub(crate) fn folder_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(folder, _)| folder)
}

pub(crate) fn mtime_ms(metadata: &std::fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |since| {
            i64::try_from(since.as_millis()).unwrap_or(i64::MAX)
        })
}

fn is_audio(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(_, extension)| {
        AUDIO_EXTENSIONS.contains(&extension.to_ascii_lowercase().as_str())
    })
}

/// Names the folder `rel` as one the walk could not read, and drops what
/// it found in it before it was refused: all of it is left as it was.
fn unreadable(walked: &mut Walked, rel: &str) {
    let inside = |path: &str| {
        path.strip_prefix(rel)
            .is_some_and(|rest| rest.starts_with('/'))
    };
    walked.files.retain(|seen| !inside(&seen.path));
    walked
        .images
        .retain(|folder, _| folder != rel && !inside(folder));
    walked.unreadable.retain(|folder| !inside(folder));
    walked.unreadable.push(rel.to_owned());
    walked
        .skipped
        .push((rel.to_owned(), "the folder cannot be read"));
}

fn walk_into(dir: &Path, rel: &str, walked: &mut Walked, stop: &AtomicBool) -> Result<(), Error> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        // A folder inside the root that the server may not read, as a
        // disk's `lost+found` is, is passed over and named, and what was
        // known in it is kept. Any other failure fails the walk, rather
        // than what the folder held being taken for gone.
        // A folder removed since its parent was listed is gone, with all
        // that was in it, as it would be had the walk come a moment later.
        Err(error) if !rel.is_empty() && error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) if !rel.is_empty() && error.kind() == ErrorKind::PermissionDenied => {
            unreadable(walked, rel);
            return Ok(());
        }
        Err(error) => return Err(Error::Io(error)),
    };
    for entry in entries {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        let entry = entry.map_err(Error::Io)?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            walked.skipped.push((
                format!("{rel}/{}", name.to_string_lossy()),
                "its name is not UTF-8",
            ));
            continue;
        };
        // Hidden files, and the resource forks macOS leaves on other
        // file systems.
        if name.starts_with('.') {
            continue;
        }
        let path = if rel.is_empty() {
            name.to_owned()
        } else {
            format!("{rel}/{name}")
        };
        // A folder that may be listed but not gone into, mode `r--`, says
        // no only here: it is passed over as one that cannot be listed.
        let denied =
            |error: &std::io::Error| !rel.is_empty() && error.kind() == ErrorKind::PermissionDenied;
        let kind = match entry.file_type() {
            Ok(kind) => kind,
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) if denied(&error) => {
                unreadable(walked, rel);
                return Ok(());
            }
            Err(error) => return Err(Error::Io(error)),
        };
        if kind.is_dir() {
            walk_into(&entry.path(), &path, walked, stop)?;
            continue;
        }
        // A link to a file is followed. A link to a folder is not: it
        // could lead back to where the walk came from. A link that leads
        // nowhere is passed over; a file the system will not describe
        // fails the walk, rather than being taken for gone.
        // A file removed since the folder was listed is gone, as it would
        // be had the walk come a moment later.
        let metadata = match std::fs::metadata(entry.path()) {
            Ok(metadata) => metadata,
            Err(_) if kind.is_symlink() => continue,
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) if denied(&error) => {
                unreadable(walked, rel);
                return Ok(());
            }
            Err(error) => return Err(Error::Io(error)),
        };
        if !metadata.is_file() {
            continue;
        }
        let seen = Seen {
            path,
            size: metadata.len(),
            mtime_ms: mtime_ms(&metadata),
        };
        if is_audio(name) {
            walked.files.push(seen);
        } else if is_folder_image(name) {
            walked.images.entry(rel.to_owned()).or_default().push(seen);
        }
    }
    Ok(())
}

/// Walks a root. A root that is missing, is not a folder, or in which a
/// whole walk finds no audio file at all is offline, not empty: this is
/// what a share that failed to mount looks like, often with a `.DS_Store`
/// at its mount point, and the scan must not mark its files missing.
/// Emptiness is decided by what the walk counts, never by whether the
/// folder has entries.
pub(crate) fn walk(root: &Path, stop: &AtomicBool) -> Result<Walked, Error> {
    let offline = || Error::RootOffline {
        path: root.to_owned(),
    };
    std::fs::read_dir(root).map_err(|_| offline())?;
    let mut walked = Walked {
        root_id: folder_id(root),
        ..Walked::default()
    };
    walk_into(root, "", &mut walked, stop)?;
    // A walk that was asked to stop has not looked everywhere.
    if walked.files.is_empty() && !stop.load(Ordering::Relaxed) {
        return Err(offline());
    }
    walked.files.sort_by(|a, b| a.path.cmp(&b.path));
    walked.unreadable.sort();
    walked.skipped.sort();
    for images in walked.images.values_mut() {
        images.sort_by(|a, b| a.path.cmp(&b.path));
    }
    Ok(walked)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Scratch;

    #[test]
    fn a_walk_finds_audio_and_folder_images_in_path_order_and_nothing_else() {
        let scratch = Scratch::new();
        let root = scratch.path();
        for path in [
            "b/02.flac",
            "b/01.MP3",
            "b/cover.jpg",
            "b/notes.txt",
            "b/.hidden.flac",
            "a/deep/er/01.ogg",
            "a/Folder.PNG",
            "a/back.jpg",
            "top.wav",
        ] {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"12345").unwrap();
        }
        let walked = walk(root, &AtomicBool::new(false)).unwrap();
        let paths: Vec<&str> = walked.files.iter().map(|seen| seen.path.as_str()).collect();
        assert_eq!(
            paths,
            ["a/deep/er/01.ogg", "b/01.MP3", "b/02.flac", "top.wav"]
        );
        assert!(
            walked
                .files
                .iter()
                .all(|seen| seen.size == 5 && seen.mtime_ms > 0)
        );
        let images: Vec<(&str, &str)> = walked
            .images
            .iter()
            .flat_map(|(folder, images)| {
                images
                    .iter()
                    .map(move |seen| (folder.as_str(), seen.path.as_str()))
            })
            .collect();
        assert_eq!(images, [("a", "a/Folder.PNG"), ("b", "b/cover.jpg")]);
        assert_eq!(folder_of("a/deep/er/01.ogg"), "a/deep/er");
        assert_eq!(folder_of("top.wav"), "");
    }

    #[test]
    fn a_folder_that_cannot_be_read_is_passed_over_and_named() {
        use crate::platform::set_listable;
        let scratch = Scratch::new();
        let root = scratch.path();
        for path in ["a/01.flac", "lost+found/02.flac", "b/c/03.flac"] {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"12345").unwrap();
        }
        // As a disk's `lost+found` is.
        set_listable(&root.join("lost+found"), false);
        set_listable(&root.join("b/c"), false);
        let walked = walk(root, &AtomicBool::new(false));
        set_listable(&root.join("lost+found"), true);
        set_listable(&root.join("b/c"), true);
        let walked = walked.unwrap();
        let paths: Vec<&str> = walked.files.iter().map(|seen| seen.path.as_str()).collect();
        assert_eq!(paths, ["a/01.flac"]);
        assert_eq!(walked.unreadable, ["b/c", "lost+found"]);
        assert_eq!(
            walked.skipped,
            [
                ("b/c".to_owned(), "the folder cannot be read"),
                ("lost+found".to_owned(), "the folder cannot be read"),
            ]
        );
    }

    // A folder that may be listed but not gone into, mode `r--`: the mode
    // is the system's.
    #[cfg(unix)]
    #[test]
    fn a_folder_that_can_be_listed_but_not_gone_into_is_passed_over_too() {
        use std::os::unix::fs::PermissionsExt;
        let scratch = Scratch::new();
        let root = scratch.path();
        for path in ["a/01.flac", "listed/02.flac", "listed/deeper/03.flac"] {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"12345").unwrap();
        }
        let mode = |mode: u32| {
            std::fs::set_permissions(root.join("listed"), std::fs::Permissions::from_mode(mode))
                .unwrap();
        };
        mode(0o444);
        let walked = walk(root, &AtomicBool::new(false));
        mode(0o755);
        let walked = walked.unwrap();
        let paths: Vec<&str> = walked.files.iter().map(|seen| seen.path.as_str()).collect();
        assert_eq!(paths, ["a/01.flac"]);
        assert_eq!(walked.unreadable, ["listed"]);
    }

    #[test]
    fn a_root_that_is_missing_or_empty_is_offline() {
        let scratch = Scratch::new();
        let stop = AtomicBool::new(false);
        let hidden = scratch.path().join("mount-point");
        std::fs::create_dir(&hidden).unwrap();
        std::fs::write(hidden.join(".DS_Store"), b"Bud1").unwrap();
        let no_audio = scratch.path().join("no-audio");
        std::fs::create_dir_all(no_audio.join("a")).unwrap();
        std::fs::write(no_audio.join("a/cover.jpg"), b"jpeg").unwrap();
        for root in [
            scratch.path().join("never-mounted"),
            scratch.path().join("empty"),
            hidden,
            no_audio,
        ] {
            let _ = std::fs::create_dir(scratch.path().join("empty"));
            let refused = walk(&root, &stop);
            assert!(
                matches!(&refused, Err(Error::RootOffline { path }) if *path == root),
                "{refused:?}"
            );
        }
        // A file where the folder should be is no root either.
        let file = scratch.path().join("a-file");
        std::fs::write(&file, b"").unwrap();
        assert!(matches!(walk(&file, &stop), Err(Error::RootOffline { .. })));
    }
}
