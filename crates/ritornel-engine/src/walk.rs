//! Walking a root: every audio file under it, with its size and when it
//! was last modified, and each folder's own images; and opening what it
//! found for the scan to read. The walk opens only folders, each from the
//! root without following a link, and nothing is ever written: a root is
//! read-only to the server. See
//! "Scanning and audio hashing" and "Rescans and file moves" in
//! docs/internals/identity.md.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::UNIX_EPOCH;

use ritornel_domain::layout;

use crate::Error;
use crate::artist_pictures::refused;
use crate::media::is_folder_image;
use crate::platform::{Folder, Kind, met_a_link, open_folder};

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

/// A folder's identity: its device and inode on Unix, or its volume's serial
/// number and file index on Windows.
pub type FolderId = (u64, u64);

/// What a walk found.
#[derive(Debug, Default)]
pub(crate) struct Walked {
    /// Every audio file, in the order of their paths.
    pub(crate) files: Vec<Seen>,
    /// Each folder's own images, by the folder's path relative to the
    /// root, in the order of their names.
    pub(crate) images: BTreeMap<String, Vec<Seen>>,
    /// Every file that may be a sidecar, by its name and size, in the order
    /// of their paths: a rip log, a CUE sheet, a checksum report, or a text
    /// file that may be a rip log. See "Disc layouts" in
    /// docs/internals/corrections.md.
    pub(crate) sidecars: Vec<Seen>,
    /// Files and folders passed over, with why: a name that is not UTF-8,
    /// a folder the server may not read, or one inside itself.
    pub(crate) skipped: Vec<(String, &'static str)>,
    /// The folders the walk did not go into, relative to the root, in the
    /// order of their paths: those the server may not read, and those inside
    /// themselves. What was known in them is not taken for gone.
    pub(crate) unreadable: Vec<String>,
    /// Which folder the root was when it was walked: its device and inode,
    /// or on Windows its volume's serial number and its file index. An
    /// unmounted disk leaves another folder at the same path.
    pub(crate) root_id: FolderId,
}

/// Which folder is at `path` now, if one is: its device and inode, or on
/// Windows its volume's serial number and its file index. A disk mounted
/// again is another folder at the same path.
pub fn folder_id(path: &Path) -> Option<FolderId> {
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
    walked.sidecars.retain(|seen| !inside(&seen.path));
    walked
        .images
        .retain(|folder, _| folder != rel && !inside(folder));
    walked.unreadable.retain(|folder| !inside(folder));
    walked.unreadable.push(rel.to_owned());
    walked
        .skipped
        .push((rel.to_owned(), "the folder cannot be read"));
}

/// Lists the open folder `folder`, at `rel` under the root: its audio
/// files and images go into `walked`, and the folders in it are returned,
/// relative to the root, to be listed in their turn. What is in it is
/// described from the open folder, not by a path from the root.
fn list(
    folder: &Folder,
    rel: &str,
    walked: &mut Walked,
    stop: &AtomicBool,
) -> Result<Vec<String>, Error> {
    let mut folders = Vec::new();
    let entries = match folder.list() {
        Ok(entries) => entries,
        // A folder inside the root that the server may not read, as a
        // disk's `lost+found` is, is passed over and named, and what was
        // known in it is kept. Any other failure fails the walk, rather
        // than what the folder held being taken for gone.
        Err(error) if !rel.is_empty() && error.kind() == ErrorKind::PermissionDenied => {
            unreadable(walked, rel);
            return Ok(folders);
        }
        // Made a link while it was listed, which Windows can tell only
        // after: what was listed was not in it, and a link is not followed.
        Err(error) if !rel.is_empty() && met_a_link(&error) => return Ok(folders),
        Err(error) => return Err(Error::Io(error)),
    };
    for (name, kind) in entries {
        if stop.load(Ordering::Relaxed) {
            return Ok(Vec::new());
        }
        let joined = |name: &str| {
            if rel.is_empty() {
                name.to_owned()
            } else {
                format!("{rel}/{name}")
            }
        };
        let Some(text) = name.to_str() else {
            walked
                .skipped
                .push((joined(&name.to_string_lossy()), "its name is not UTF-8"));
            continue;
        };
        // Hidden files, and the resource forks macOS leaves on other
        // file systems.
        if text.starts_with('.') {
            continue;
        }
        let path = joined(text);
        if kind == Kind::Folder {
            folders.push(path);
            continue;
        }
        // A link to a file is followed. A link to a folder is not: it
        // could lead back to where the walk came from. A link that leads
        // nowhere is passed over; a file the system will not describe
        // fails the walk, rather than being taken for gone.
        // A file removed since the folder was listed is gone, as it would
        // be had the walk come a moment later. A folder that may be listed
        // but not gone into, mode `r--`, says no only here: it is passed
        // over as one that cannot be listed.
        let described = match folder.describe(&name) {
            Ok(described) => described,
            Err(_) if kind == Kind::Link => continue,
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) if !rel.is_empty() && error.kind() == ErrorKind::PermissionDenied => {
                unreadable(walked, rel);
                return Ok(Vec::new());
            }
            Err(error) => return Err(Error::Io(error)),
        };
        if !described.is_file {
            continue;
        }
        let seen = Seen {
            path,
            size: described.len,
            mtime_ms: described.mtime_ms,
        };
        if is_audio(text) {
            walked.files.push(seen);
        } else if is_folder_image(text) {
            walked.images.entry(rel.to_owned()).or_default().push(seen);
        } else if layout::kind(text).is_some() && seen.size <= layout::SIDECAR_MAX_BYTES {
            walked.sidecars.push(seen);
        }
    }
    Ok(folders)
}

/// The parts of `rel`, a path relative to the root with `/` between its
/// parts: none for the root.
fn parts(rel: &str) -> Vec<&str> {
    rel.split('/').filter(|part| !part.is_empty()).collect()
}

/// Lists the root and every folder under it, one at a time from a list of
/// those still to go rather than by recursion, so that how deep a tree
/// goes costs no stack. A folder that is one of the folders it is inside,
/// as a bind mount can make, is passed over rather than gone round again.
fn walk_tree(
    root: &Path,
    walked: &mut Walked,
    stop: &AtomicBool,
    id_of: &dyn Fn(&Path) -> Option<FolderId>,
) -> Result<(), Error> {
    // Each folder still to list, with how deep it is; and the identity of
    // each folder on the way down to the one being listed.
    let mut todo = vec![(String::new(), 0)];
    let mut ancestors: Vec<Option<FolderId>> = Vec::new();
    while let Some((rel, depth)) = todo.pop() {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        // The list is taken deepest first, so the folders on the way to
        // this one are the first `depth` of those last gone down.
        ancestors.truncate(depth);
        // A zero file index is what some network or virtual filesystems
        // return for every folder, so it proves no cycle. Both device and
        // index must match an ancestor (`FolderId` is `(device, index)`).
        let here = if depth == 0 {
            Some(walked.root_id)
        } else {
            id_of(&under(root, &rel))
        }
        .filter(|&(_, index)| index != 0);
        // Not gone into, and what was known in it is kept, as in a folder
        // that cannot be read: its files may well still be there.
        if here.is_some() && ancestors.contains(&here) {
            walked.unreadable.push(rel.clone());
            walked.skipped.push((rel, "the folder is inside itself"));
            continue;
        }
        // The folder is opened from the root a part at a time, none of
        // them followed if it is a link: one on the way that has become a
        // link since this folder was queued, or this folder itself, leads
        // nowhere, as a link to a folder found in a listing does.
        let folder = match open_folder(root, &parts(&rel)) {
            Ok(folder) => folder,
            // A folder removed since its parent was listed is gone, with
            // all that was in it, as it would be had the walk come a moment
            // later.
            Err(error) if depth > 0 && error.kind() == ErrorKind::NotFound => continue,
            Err(error) if depth > 0 && met_a_link(&error) => continue,
            Err(error) if depth > 0 && error.kind() == ErrorKind::PermissionDenied => {
                unreadable(walked, &rel);
                continue;
            }
            Err(error) => return Err(Error::Io(error)),
        };
        ancestors.push(here);
        let folders = list(&folder, &rel, walked, stop)?;
        todo.extend(folders.into_iter().map(|rel| (rel, depth + 1)));
    }
    Ok(())
}

/// Opens the file at `rel` under `root`, a path the walk found, for the
/// scan to read. Each folder on the way is opened from the last without
/// following a link (`platform::open_folder`), so the read stays under the
/// root however a folder above the file changes during the scan; a link to
/// a file is followed, as the walk follows it. Anything but a regular file
/// is refused, and a pipe does not hold the open up.
pub(crate) fn open_under(root: &Path, rel: &str) -> std::io::Result<File> {
    let parts = parts(rel);
    if parts.iter().any(|part| *part == "." || *part == "..") {
        return Err(refused("not a plain path"));
    }
    let Some((name, dirs)) = parts.split_last() else {
        return Err(refused("no path"));
    };
    let file = open_folder(root, dirs)?.open(name, true)?;
    if !file.metadata()?.is_file() {
        return Err(refused("not a regular file"));
    }
    Ok(file)
}

/// Reads the file at `rel` under `root` whole, as [`open_under`] opens it,
/// and of it at most `cap` bytes and one more, so that a file larger than
/// it may be is seen to be.
pub(crate) fn read_under(root: &Path, rel: &str, cap: u64) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    open_under(root, rel)?
        .take(cap.saturating_add(1))
        .read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Walks a root. A root that is missing, is not a folder, has no identity
/// the system will give, or in which a whole walk finds no audio file at
/// all is offline, not empty: this is what a share that failed to mount
/// looks like, often with a `.DS_Store` at its mount point, and the scan
/// must not mark its files missing. Emptiness is decided by what the walk
/// counts, never by whether the folder has entries.
pub(crate) fn walk(root: &Path, stop: &AtomicBool) -> Result<Walked, Error> {
    walk_by(root, stop, &folder_id)
}

/// [`walk`], with what says which folder is at a path.
fn walk_by(
    root: &Path,
    stop: &AtomicBool,
    id_of: &dyn Fn(&Path) -> Option<FolderId>,
) -> Result<Walked, Error> {
    let offline = || Error::RootOffline {
        path: root.to_owned(),
    };
    std::fs::read_dir(root).map_err(|_| offline())?;
    // A root that cannot be told from another folder mounted at its path
    // later is not walked: nothing could prove that what the scan then
    // reads, or does not find, is of the folder it walked.
    let mut walked = Walked {
        root_id: id_of(root).ok_or_else(offline)?,
        ..Walked::default()
    };
    walk_tree(root, &mut walked, stop, id_of)?;
    // A walk that was asked to stop has not looked everywhere.
    if !stop.load(Ordering::Relaxed)
        && (walked.files.is_empty() || id_of(root) != Some(walked.root_id))
    {
        return Err(offline());
    }
    walked.files.sort_by(|a, b| a.path.cmp(&b.path));
    walked.sidecars.sort_by(|a, b| a.path.cmp(&b.path));
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
            "b/rip.LOG",
            "b/misc/album.cue",
            "a/big.cue",
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
        let sidecars: Vec<&str> = walked
            .sidecars
            .iter()
            .map(|seen| seen.path.as_str())
            .collect();
        assert_eq!(
            sidecars,
            ["a/big.cue", "b/misc/album.cue", "b/notes.txt", "b/rip.LOG"]
        );
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
    fn a_deep_tree_is_walked_on_a_small_stack() {
        let scratch = Scratch::new();
        // Canonicalizing gives Windows the `\\?\` form for a path past
        // `MAX_PATH`.
        let root = std::fs::canonicalize(scratch.path()).unwrap();
        // Short names keep the deepest path inside every system's limit.
        let deep = (0..300).fold(root.clone(), |path, _| path.join("d"));
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("01.flac"), b"12345").unwrap();
        std::fs::write(root.join("top.flac"), b"12345").unwrap();
        let walk_root = root.clone();
        let walked = std::thread::Builder::new()
            .stack_size(128 * 1024)
            .spawn(move || walk(&walk_root, &AtomicBool::new(false)).unwrap())
            .unwrap()
            .join()
            .unwrap();
        let paths: Vec<&str> = walked.files.iter().map(|seen| seen.path.as_str()).collect();
        assert_eq!(paths.len(), 2);
        assert_eq!(paths[0], format!("{}01.flac", "d/".repeat(300)));
        assert_eq!(paths[1], "top.flac");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_folder_that_is_one_it_is_inside_is_passed_over() {
        let scratch = Scratch::new();
        let root = scratch.path();
        let files = [
            "a/01.flac",
            "a/loop/02.flac",
            "b/c/03.flac",
            "b/c/up/04.flac",
            "b/loop/05.flac",
            "n/m/06.flac",
        ];
        for path in files {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"12345").unwrap();
        }
        // As bind mounts would make them: `a/loop` is the root again, and
        // `b/c/up` is `b`. `b/loop` is only another folder of the name, and
        // `n` and `n/m` have no identity, which proves nothing.
        let ids = [
            ("", Some((1, 1))),
            ("a", Some((1, 2))),
            ("a/loop", Some((1, 1))),
            ("b", Some((1, 3))),
            ("b/c", Some((1, 4))),
            ("b/c/up", Some((1, 3))),
            ("b/loop", Some((1, 5))),
            ("n", None),
            ("n/m", None),
        ];
        let id_of = |path: &Path| {
            ids.iter()
                .find(|(rel, _)| under(root, rel) == path)
                .and_then(|(_, id)| *id)
        };
        let walked = walk_by(root, &AtomicBool::new(false), &id_of).unwrap();
        let paths: Vec<&str> = walked.files.iter().map(|seen| seen.path.as_str()).collect();
        assert_eq!(
            paths,
            ["a/01.flac", "b/c/03.flac", "b/loop/05.flac", "n/m/06.flac"]
        );
        let inside = "the folder is inside itself";
        assert_eq!(
            walked.skipped,
            [("a/loop".to_owned(), inside), ("b/c/up".to_owned(), inside)]
        );
        // What a scan knew in them is not taken for gone.
        assert_eq!(walked.unreadable, ["a/loop", "b/c/up"]);
    }

    #[test]
    fn a_zero_file_index_is_not_taken_for_a_cycle() {
        let scratch = Scratch::new();
        let root = scratch.path();
        for path in ["a/01.flac", "a/b/02.flac", "c/03.flac"] {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"12345").unwrap();
        }
        // As an SMB redirector, FUSE or overlayfs returning a zero file
        // index for every folder: subfolders must still be walked, and
        // nothing is skipped as inside itself.
        for id in [(1, 0), (0, 0)] {
            let walked = walk_by(root, &AtomicBool::new(false), &|_| Some(id)).unwrap();
            let paths: Vec<&str> = walked.files.iter().map(|seen| seen.path.as_str()).collect();
            assert_eq!(paths, ["a/01.flac", "a/b/02.flac", "c/03.flac"]);
            assert!(walked.skipped.is_empty(), "{:?}", walked.skipped);
            assert!(walked.unreadable.is_empty(), "{:?}", walked.unreadable);
        }
    }

    #[test]
    fn a_root_with_no_identity_is_offline() {
        let scratch = Scratch::new();
        let root = scratch.path();
        std::fs::write(root.join("01.flac"), b"12345").unwrap();
        // As a volume the system gives no identity for: what the scan
        // later finds there could be of any disk mounted at the path.
        let refused = walk_by(root, &AtomicBool::new(false), &|_| None);
        assert!(
            matches!(&refused, Err(Error::RootOffline { path }) if path == root),
            "{refused:?}"
        );
        assert!(walk(root, &AtomicBool::new(false)).is_ok());
    }

    #[test]
    fn a_root_whose_identity_changes_during_the_walk_is_offline() {
        use std::sync::atomic::AtomicUsize;
        let scratch = Scratch::new();
        let root = scratch.path();
        std::fs::write(root.join("01.flac"), b"12345").unwrap();
        let calls = AtomicUsize::new(0);
        let id_of = |path: &Path| {
            if path == root {
                let n = calls.fetch_add(1, Ordering::Relaxed);
                Some((1, 1 + n as u64))
            } else {
                folder_id(path)
            }
        };
        let refused = walk_by(root, &AtomicBool::new(false), &id_of);
        assert!(
            matches!(&refused, Err(Error::RootOffline { path }) if path == root),
            "{refused:?}"
        );
    }

    #[test]
    fn a_queued_folder_replaced_by_a_link_before_it_is_visited_is_not_followed() {
        let scratch = Scratch::new();
        let root = scratch.path().join("root");
        let outside = scratch.path().join("outside");
        std::fs::create_dir_all(root.join("a")).unwrap();
        std::fs::create_dir_all(root.join("b")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(root.join("a/01.flac"), b"12345").unwrap();
        std::fs::write(root.join("b/02.flac"), b"12345").unwrap();
        std::fs::write(outside.join("escaped.flac"), b"12345").unwrap();
        let swapped = AtomicBool::new(false);
        let id_of = |path: &Path| {
            // Once the walk visits one sibling, replace the other queued
            // sibling with a link to a folder outside the root.
            if (path == root.join("a") || path == root.join("b"))
                && !swapped.swap(true, Ordering::Relaxed)
            {
                let other = if path == root.join("a") {
                    root.join("b")
                } else {
                    root.join("a")
                };
                std::fs::remove_dir_all(&other).unwrap();
                crate::platform::link_folder(&outside, &other);
            }
            folder_id(path)
        };
        let walked = walk_by(&root, &AtomicBool::new(false), &id_of).unwrap();
        let paths: Vec<&str> = walked.files.iter().map(|seen| seen.path.as_str()).collect();
        assert!(swapped.load(Ordering::Relaxed));
        assert!(
            !paths.iter().any(|path| path.ends_with("escaped.flac")),
            "{paths:?}"
        );
        assert_eq!(paths.len(), 1);
    }

    /// Makes `files` under `root`, each a copy of the fixture FLAC.
    fn flacs(root: &Path, files: &[&str]) {
        let flac = std::fs::read(crate::media_fixtures::fixtures().join("a.flac")).unwrap();
        for path in files {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, &flac).unwrap();
        }
    }

    /// Replaces the folder `rel` under `root` with a link to `outside`,
    /// moving the folder to `moved`.
    fn replace_with_link(root: &Path, rel: &str, outside: &Path, moved: &Path) {
        std::fs::rename(under(root, rel), moved).unwrap();
        crate::platform::link_folder(outside, &under(root, rel));
    }

    #[test]
    fn a_folder_above_a_queued_one_replaced_by_a_link_is_not_followed() {
        let scratch = Scratch::new();
        let root = scratch.path().join("root");
        let outside = scratch.path().join("outside");
        flacs(
            &root,
            &["top.flac", "Artist/Album/01.flac", "Artist/Other/02.flac"],
        );
        flacs(&outside, &["Album/escaped.flac", "Other/escaped.flac"]);
        let (album, other) = (root.join("Artist/Album"), root.join("Artist/Other"));
        let swapped = AtomicBool::new(false);
        let id_of = |path: &Path| {
            // Once `Artist` is listed and both its folders are queued, and
            // before either is listed, `Artist` becomes a link to a folder
            // outside the root that holds folders of the same names.
            if (path == album || path == other) && !swapped.swap(true, Ordering::Relaxed) {
                replace_with_link(&root, "Artist", &outside, &scratch.path().join("moved"));
            }
            folder_id(path)
        };
        let walked = walk_by(&root, &AtomicBool::new(false), &id_of).unwrap();
        assert!(swapped.load(Ordering::Relaxed));
        let paths: Vec<&str> = walked.files.iter().map(|seen| seen.path.as_str()).collect();
        assert_eq!(paths, ["top.flac"]);
        assert!(walked.skipped.is_empty(), "{:?}", walked.skipped);
    }

    #[test]
    fn a_file_whose_folder_above_became_a_link_after_the_walk_is_not_read() {
        use crate::media::{UNAVAILABLE, read_file, read_folder_image};
        let scratch = Scratch::new();
        let root = scratch.path().join("root");
        let outside = scratch.path().join("outside");
        let files = ["Album/01.flac", "Album/cover.jpg", "Album/rip.log"];
        flacs(&root.join("Artist"), &files);
        flacs(&outside, &files);
        let walked = walk(&root, &AtomicBool::new(false)).unwrap();
        assert_eq!(walked.files.len(), 1);
        assert_eq!(walked.images.len(), 1);
        assert_eq!(walked.sidecars.len(), 1);
        let read = |rel: &str| read_file(&root, rel, 0, 0, Vec::new());
        assert!(read("Artist/Album/01.flac").audio.is_some());
        assert!(read_folder_image(&root, "Artist/Album/cover.jpg", 0).is_some());
        assert!(read_under(&root, "Artist/Album/rip.log", 1 << 20).is_ok());

        // The scan reads what the walk found after it has walked: by then
        // `Artist` is a link to a folder outside the root.
        replace_with_link(&root, "Artist", &outside, &scratch.path().join("moved"));
        let payload = read("Artist/Album/01.flac");
        assert_eq!(payload.error.unwrap().kind, UNAVAILABLE);
        assert_eq!(payload.audio, None);
        assert!(read_folder_image(&root, "Artist/Album/cover.jpg", 0).is_none());
        let refused = read_under(&root, "Artist/Album/rip.log", 1 << 20).unwrap_err();
        assert!(met_a_link(&refused), "{refused:?}");
        // A path that climbs out is refused before anything is opened.
        let climbs = read_under(&root, "../outside/Album/rip.log", 1 << 20).unwrap_err();
        assert_eq!(climbs.kind(), ErrorKind::InvalidInput);
    }

    #[test]
    fn links_inside_the_root_are_passed_over_or_followed_as_they_were() {
        use crate::platform::{link_file, link_folder};
        let scratch = Scratch::new();
        let root = scratch.path();
        flacs(root, &["a/01.flac", "c/02.flac"]);
        // A link to a file is followed, a link to a folder is not, and a
        // link that leads nowhere is passed over: none stops the walk.
        link_file(&root.join("a/01.flac"), &root.join("a/linked.flac"));
        std::fs::create_dir(root.join("b")).unwrap();
        link_folder(&root.join("a"), &root.join("b/loop"));
        link_file(&root.join("gone.flac"), &root.join("c/dangling.flac"));
        let walked = walk(root, &AtomicBool::new(false)).unwrap();
        let paths: Vec<&str> = walked.files.iter().map(|seen| seen.path.as_str()).collect();
        assert_eq!(paths, ["a/01.flac", "a/linked.flac", "c/02.flac"]);
        assert!(walked.skipped.is_empty(), "{:?}", walked.skipped);
        assert!(walked.unreadable.is_empty(), "{:?}", walked.unreadable);
        // The scan reads a linked file as the walk found it.
        let payload = crate::media::read_file(root, "a/linked.flac", 0, 0, Vec::new());
        assert!(payload.audio.is_some(), "{:?}", payload.error);
    }

    #[test]
    fn a_file_in_a_folder_that_cannot_be_read_is_unavailable_to_the_scan() {
        use crate::platform::set_listable;
        let scratch = Scratch::new();
        let root = scratch.path();
        flacs(root, &["a/01.flac", "a/rip.log"]);
        set_listable(&root.join("a"), false);
        let payload = crate::media::read_file(root, "a/01.flac", 0, 0, Vec::new());
        let sidecar = read_under(root, "a/rip.log", 1 << 20);
        set_listable(&root.join("a"), true);
        assert_eq!(payload.error.unwrap().kind, crate::media::UNAVAILABLE);
        assert_eq!(payload.audio, None);
        let denied = sidecar.unwrap_err();
        assert_eq!(denied.kind(), ErrorKind::PermissionDenied, "{denied:?}");
    }

    #[test]
    fn the_walk_gives_each_file_the_size_and_time_the_system_does() {
        let scratch = Scratch::new();
        let root = scratch.path();
        flacs(root, &["a/01.flac", "02.flac"]);
        let walked = walk(root, &AtomicBool::new(false)).unwrap();
        assert_eq!(walked.files.len(), 2);
        // A rescan tells a changed file by these, so they must be what
        // `std` would have said.
        for seen in &walked.files {
            let metadata = std::fs::metadata(under(root, &seen.path)).unwrap();
            assert_eq!(seen.size, metadata.len());
            assert_eq!(seen.mtime_ms, mtime_ms(&metadata));
        }
    }

    // Only Linux keeps a name that is not UTF-8 as it was given.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_name_that_is_not_utf8_is_named_by_its_path_under_the_root() {
        use std::os::unix::ffi::OsStrExt;
        let scratch = Scratch::new();
        let root = scratch.path();
        std::fs::create_dir(root.join("a")).unwrap();
        std::fs::write(root.join("a/01.flac"), b"12345").unwrap();
        let name = std::ffi::OsStr::from_bytes(b"\xff.flac");
        std::fs::write(root.join(name), b"12345").unwrap();
        std::fs::write(root.join("a").join(name), b"12345").unwrap();
        let walked = walk(root, &AtomicBool::new(false)).unwrap();
        assert_eq!(
            walked.skipped,
            [
                ("a/\u{fffd}.flac".to_owned(), "its name is not UTF-8"),
                ("\u{fffd}.flac".to_owned(), "its name is not UTF-8"),
            ]
        );
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
