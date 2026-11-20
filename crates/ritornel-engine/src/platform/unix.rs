//! What every Unix shares: its clocks, a file's device and inode, opening
//! relative to an open folder, and reading at an offset.

use std::fs::{File, Metadata};
use std::path::Path;

use rustix::time::ClockId;

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

/// Opens `name` in the folders `dirs` under `root`. Each part is opened
/// from the last without following a link, and the file without waiting,
/// so a pipe cannot hold the read up.
pub(crate) fn open_inside(root: &Path, dirs: &[&str], name: &str) -> std::io::Result<File> {
    use rustix::fs::{Mode, OFlags, open, openat};
    let into = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
    let mut dir = open(root, into, Mode::empty())?;
    for part in dirs {
        dir = openat(&dir, *part, into | OFlags::NOFOLLOW, Mode::empty())?;
    }
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    Ok(File::from(openat(&dir, name, flags, Mode::empty())?))
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
