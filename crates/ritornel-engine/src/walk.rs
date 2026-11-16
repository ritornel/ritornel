//! Walking a root: every audio file under it, with its size and when it
//! was last modified, and each folder's own images. Nothing is opened, and
//! nothing is ever written: a root is read-only to the server. See
//! "Scanning one file" step 1 and "Rescans" in docs/design/identity.md.

use std::collections::BTreeMap;
use std::path::Path;
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
    /// Files passed over, with why: a name that is not UTF-8.
    pub(crate) skipped: Vec<(String, &'static str)>,
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

fn walk_into(dir: &Path, rel: &str, walked: &mut Walked, stop: &AtomicBool) -> Result<(), Error> {
    for entry in std::fs::read_dir(dir).map_err(Error::Io)? {
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
        let kind = entry.file_type().map_err(Error::Io)?;
        if kind.is_dir() {
            walk_into(&entry.path(), &path, walked, stop)?;
            continue;
        }
        // A link to a file is followed. A link to a folder is not: it
        // could lead back to where the walk came from.
        let Ok(metadata) = std::fs::metadata(entry.path()) else {
            continue;
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
    let mut walked = Walked::default();
    walk_into(root, "", &mut walked, stop)?;
    // A walk that was asked to stop has not looked everywhere.
    if walked.files.is_empty() && !stop.load(Ordering::Relaxed) {
        return Err(offline());
    }
    walked.files.sort_by(|a, b| a.path.cmp(&b.path));
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
