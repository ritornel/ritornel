//! What every Unix shares: its clocks, a file's device and inode, listing,
//! describing and opening relative to an open folder, and reading at an
//! offset.

use std::ffi::{OsStr, OsString};
use std::fs::{File, Metadata};
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags, open, openat, statat};
use rustix::time::ClockId;

use super::{Described, Kind};

/// Milliseconds on the clock `clock`.
pub(super) fn clock_ms(clock: ClockId) -> i64 {
    let now = rustix::time::clock_gettime(clock);
    now.tv_sec
        .saturating_mul(1000)
        .saturating_add(now.tv_nsec / 1_000_000)
}

/// Which file or folder `metadata` is: its device and inode.
pub(crate) fn file_id(_path: &Path, metadata: &Metadata) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    Some((metadata.dev(), metadata.ino()))
}

/// A folder under a root, open. What is in it is listed, described and
/// opened relative to the open folder, never by a path from the root, so
/// a folder on the way that is replaced by a link meanwhile leads nowhere
/// else.
#[derive(Debug)]
pub(crate) struct Folder(OwnedFd);

/// Opens the folders `dirs` under `root`, each from the last without
/// following a link. The root is followed, as it was given.
pub(crate) fn open_folder(root: &Path, dirs: &[&str]) -> std::io::Result<Folder> {
    let into = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
    let mut dir = open(root, into, Mode::empty())?;
    for part in dirs {
        dir = openat(&dir, *part, into | OFlags::NOFOLLOW, Mode::empty())?;
    }
    Ok(Folder(dir))
}

impl Folder {
    /// Every entry in the folder but `.` and `..`, with what it is.
    pub(crate) fn list(&self) -> std::io::Result<Vec<(OsString, Kind)>> {
        let mut entries = Vec::new();
        for entry in Dir::read_from(&self.0)? {
            let entry = entry?;
            let name = entry.file_name();
            if [&b"."[..], b".."].contains(&name.to_bytes()) {
                continue;
            }
            // Some file systems leave the kind out of a listing.
            let kind = match entry.file_type() {
                FileType::Unknown => statat(&self.0, name, AtFlags::SYMLINK_NOFOLLOW)
                    .map_or(FileType::Unknown, |stat| {
                        FileType::from_raw_mode(stat.st_mode)
                    }),
                kind => kind,
            };
            let kind = match kind {
                FileType::Directory => Kind::Folder,
                FileType::Symlink => Kind::Link,
                _ => Kind::Other,
            };
            entries.push((OsStr::from_bytes(name.to_bytes()).to_owned(), kind));
        }
        Ok(entries)
    }

    /// What the file `name` in the folder is, following it if it is a
    /// link. Nothing is opened.
    // The fields' types differ from one Unix to another.
    #[allow(clippy::useless_conversion)]
    pub(crate) fn describe(&self, name: &OsStr) -> std::io::Result<Described> {
        let stat = statat(&self.0, name, AtFlags::empty())?;
        let seconds = i64::from(stat.st_mtime);
        let nanos = i64::try_from(stat.st_mtime_nsec).unwrap_or(0);
        // As `std` counts it: the whole milliseconds since the epoch.
        let mtime_ms = if seconds < 0 {
            0
        } else {
            seconds
                .saturating_mul(1000)
                .saturating_add(nanos / 1_000_000)
        };
        Ok(Described {
            is_file: FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile,
            len: u64::try_from(stat.st_size).unwrap_or(0),
            mtime_ms,
        })
    }

    /// Opens the file `name` in the folder, following it if it is a link
    /// only if `follow` says to, and without waiting, so a pipe cannot hold
    /// the read up.
    pub(crate) fn open(&self, name: &str, follow: bool) -> std::io::Result<File> {
        let mut flags = OFlags::RDONLY | OFlags::NONBLOCK | OFlags::CLOEXEC;
        if !follow {
            flags |= OFlags::NOFOLLOW;
        }
        Ok(File::from(openat(&self.0, name, flags, Mode::empty())?))
    }
}

/// Opens `name` in the folders `dirs` under `root`. Each part is opened
/// from the last without following a link, and the file without waiting,
/// so a pipe cannot hold the read up.
pub(crate) fn open_inside(root: &Path, dirs: &[&str], name: &str) -> std::io::Result<File> {
    open_folder(root, dirs)?.open(name, false)
}

/// Whether [`open_inside`] failed on a link where a folder or the file
/// should be.
pub(crate) fn met_a_link(error: &std::io::Error) -> bool {
    [rustix::io::Errno::LOOP, rustix::io::Errno::NOTDIR]
        .iter()
        .any(|errno| error.raw_os_error() == Some(errno.raw_os_error()))
}

/// Reads into `buffer` from `at` in `file`, leaving no place of its own.
pub(crate) fn read_at(file: &File, buffer: &mut [u8], at: u64) -> std::io::Result<usize> {
    std::os::unix::fs::FileExt::read_at(file, buffer, at)
}

/// Makes `link` a symbolic link to the file `target`.
#[cfg(test)]
pub(crate) fn link_file(target: &Path, link: &Path) {
    std::os::unix::fs::symlink(target, link).unwrap();
}

/// Makes `link` a symbolic link to the folder `target`.
#[cfg(test)]
pub(crate) fn link_folder(target: &Path, link: &Path) {
    std::os::unix::fs::symlink(target, link).unwrap();
}

/// Lets the folder `dir` be listed, or not.
#[cfg(test)]
pub(crate) fn set_listable(dir: &Path, listable: bool) {
    use std::os::unix::fs::PermissionsExt;
    let bits = if listable { 0o755 } else { 0o000 };
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(bits)).unwrap();
}

/// A file's mode, for a listing.
#[cfg(test)]
pub(crate) fn mode_of(metadata: &Metadata) -> String {
    use std::os::unix::fs::PermissionsExt;
    format!("{:o}", metadata.permissions().mode())
}

/// Makes nothing under `root` writable, or everything again.
#[cfg(test)]
pub(crate) fn seal(root: &Path, sealed: bool) {
    use std::os::unix::fs::PermissionsExt;
    let (folder, file) = if sealed {
        (0o555, 0o444)
    } else {
        (0o755, 0o644)
    };
    let mut folders = vec![root.to_owned()];
    while let Some(path) = folders.pop() {
        for entry in std::fs::read_dir(&path).unwrap() {
            let entry = entry.unwrap().path();
            if entry.is_dir() {
                folders.push(entry);
            } else {
                std::fs::set_permissions(&entry, std::fs::Permissions::from_mode(file)).unwrap();
            }
        }
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(folder)).unwrap();
    }
}
