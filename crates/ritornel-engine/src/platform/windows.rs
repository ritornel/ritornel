//! Windows: the precise interrupt time, a boot ID made from the registry, a
//! folder's volume and file index, and folders under a root held open so
//! that no link on the way is followed. What the standard library cannot
//! ask is in `ritornel-windows`.

use std::ffi::{OsStr, OsString};
use std::fs::{File, Metadata};
use std::path::{Path, PathBuf};

use super::{Described, Kind};
use crate::Id;
use crate::artist_pictures::refused;

/// Milliseconds on the precise interrupt time, which only goes forward and
/// counts through sleep, as `CLOCK_BOOTTIME` does. The tick count moves in
/// steps of 10 to 16 ms.
pub(crate) fn mono_ms() -> i64 {
    i64::try_from(ritornel_windows::clock::since_boot_ms()).unwrap_or(i64::MAX)
}

/// The ID of this boot. Windows names no boot with a UUID. It counts them:
/// `BootId` under the prefetcher's parameters goes up by one at each start.
/// With the machine's own GUID, which tells this machine from another that
/// has started as often, it makes an ID that is the same for the whole of
/// one boot and another at the next. Both are asked of the system's `reg`
/// tool, once, as macOS's `sysctl` is.
pub(crate) fn read_boot() -> Option<Id> {
    let boot = reg_value(
        r"HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Memory Management\PrefetchParameters",
        "BootId",
    )?;
    let machine = reg_value(r"HKLM\SOFTWARE\Microsoft\Cryptography", "MachineGuid")?;
    Some(super::windows_boot_id(&machine, &boot))
}

/// One value under a registry key, as `reg query` prints it: the last word
/// of the line that names it.
fn reg_value(key: &str, name: &str) -> Option<String> {
    let done = std::process::Command::new("reg")
        .args(["query", key, "/v", name])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|done| done.status.success())?;
    let text = String::from_utf8(done.stdout).ok()?;
    super::reg_line(&text, name)
}

/// Which file or folder is at `path`: its volume's serial number and its
/// index on the volume.
pub(crate) fn file_id(path: &Path, _metadata: &Metadata) -> Option<(u64, u64)> {
    ritornel_windows::file::id(path).ok()
}

// FILE_ATTRIBUTE_REPARSE_POINT: a symbolic link, a junction, or another
// thing that sends the system elsewhere.
const REPARSE_POINT: u32 = 0x400;
// FILE_FLAG_OPEN_REPARSE_POINT and FILE_FLAG_BACKUP_SEMANTICS, which a
// folder is opened with.
const OPEN_REPARSE_POINT: u32 = 0x0020_0000;
const BACKUP_SEMANTICS: u32 = 0x0200_0000;

/// A folder under a root, open, with every folder on the way to it from
/// the root. Windows opens nothing relative to an open folder, so each is
/// held instead: opened without following a link, refused if it is one or
/// is not where its parent really is, and shared with no one who would
/// delete it, so that while it is held it cannot be renamed, removed or
/// replaced by a link. A path from the root then leads only through the
/// folders that were checked.
#[derive(Debug)]
pub(crate) struct Folder {
    /// The folder's path, from the root as it was given.
    path: PathBuf,
    /// Where the folder really is, in the `\\?\` form.
    real: PathBuf,
    /// The root and each folder from it, held open.
    held: Vec<File>,
}

/// Holds the folder at `path` open, following it if it is a link only if
/// `follow` says to.
fn hold(path: &Path, follow: bool) -> std::io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    // FILE_TRAVERSE and FILE_READ_ATTRIBUTES: going through it counts as
    // reading it, which sharing is checked for.
    const TRAVERSE_AND_ATTRIBUTES: u32 = 0x20 | 0x80;
    // FILE_SHARE_READ and FILE_SHARE_WRITE, and not FILE_SHARE_DELETE,
    // which a rename needs as well.
    const SHARE_BUT_DELETE: u32 = 0x1 | 0x2;
    let flags = if follow {
        BACKUP_SEMANTICS
    } else {
        BACKUP_SEMANTICS | OPEN_REPARSE_POINT
    };
    std::fs::OpenOptions::new()
        .access_mode(TRAVERSE_AND_ATTRIBUTES)
        .share_mode(SHARE_BUT_DELETE)
        .custom_flags(flags)
        .open(path)
}

/// Opens the folders `dirs` under `root`, each held, none followed if it is
/// a link, and each checked to be in the one before it. The root is
/// followed, as it was given. A folder on the way that is a link, or is
/// not a folder, is `NotADirectory`, as [`met_a_link`] says.
pub(crate) fn open_folder(root: &Path, dirs: &[&str]) -> std::io::Result<Folder> {
    let first = hold(root, true)?;
    let mut real = ritornel_windows::file::final_path(&first)?;
    let mut held = vec![first];
    let mut path = root.to_owned();
    for part in dirs {
        path.push(part);
        let dir = hold(&path, false)?;
        let at = ritornel_windows::file::final_path(&dir)?;
        if ritornel_windows::file::is_link(&dir)?
            || !dir.metadata()?.is_dir()
            || at.parent() != Some(real.as_path())
        {
            return Err(std::io::ErrorKind::NotADirectory.into());
        }
        real = at;
        held.push(dir);
    }
    Ok(Folder { path, real, held })
}

impl Folder {
    /// Every entry in the folder, with what it is.
    pub(crate) fn list(&self) -> std::io::Result<Vec<(OsString, Kind)>> {
        let mut entries = Vec::new();
        for entry in std::fs::read_dir(&self.path)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            let kind = if kind.is_dir() {
                Kind::Folder
            } else if kind.is_symlink() {
                Kind::Link
            } else {
                Kind::Other
            };
            entries.push((entry.file_name(), kind));
        }
        // A held folder cannot be removed, but one emptied may be made a
        // link: what was listed then was not in it.
        if let Some(dir) = self.held.last()
            && ritornel_windows::file::is_link(dir)?
        {
            return Err(std::io::ErrorKind::NotADirectory.into());
        }
        Ok(entries)
    }

    /// What the file `name` in the folder is, following it if it is a
    /// link.
    pub(crate) fn describe(&self, name: &OsStr) -> std::io::Result<Described> {
        let metadata = std::fs::metadata(self.path.join(name))?;
        Ok(Described {
            is_file: metadata.is_file(),
            len: metadata.len(),
            mtime_ms: crate::walk::mtime_ms(&metadata),
        })
    }

    /// Opens the file `name` in the folder, following it if it is a link
    /// only if `follow` says to. The file is opened without following it
    /// first, and must be in the folder where it really is, as its handle
    /// says.
    pub(crate) fn open(&self, name: &str, follow: bool) -> std::io::Result<File> {
        use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
        let path = self.path.join(name);
        if std::fs::symlink_metadata(&path)?.is_dir() {
            return Err(refused("not a regular file"));
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(OPEN_REPARSE_POINT)
            .open(&path)?;
        if ritornel_windows::file::final_path(&file)?.parent() != Some(self.real.as_path()) {
            return Err(refused("a link"));
        }
        if file.metadata()?.file_attributes() & REPARSE_POINT == 0 {
            return Ok(file);
        }
        // A link to a file, or a cloud file's placeholder, is followed from
        // the folder that is held.
        if follow {
            return std::fs::File::open(&path);
        }
        Err(refused("a link"))
    }
}

/// Opens `name` in the folders `dirs` under `root`: each folder on the way
/// is held as [`open_folder`] holds it, and the file is opened without
/// following a link, as on Unix.
pub(crate) fn open_inside(root: &Path, dirs: &[&str], name: &str) -> std::io::Result<File> {
    open_folder(root, dirs)?.open(name, false)
}

/// Whether [`open_inside`] failed on a link where a folder or the file
/// should be: a folder on the way that is not one.
pub(crate) fn met_a_link(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::NotADirectory
}

/// Reads into `buffer` from `at` in `file`. Windows moves the file's own
/// place too, which nothing else reads.
pub(crate) fn read_at(file: &File, buffer: &mut [u8], at: u64) -> std::io::Result<usize> {
    std::os::windows::fs::FileExt::seek_read(file, buffer, at)
}

/// Makes `link` a symbolic link to the file `target`. Windows lets only an
/// administrator, or anyone on a machine in developer mode, make one.
#[cfg(test)]
pub(crate) fn link_file(target: &Path, link: &Path) {
    std::os::windows::fs::symlink_file(target, link)
        .expect("a symbolic link needs developer mode or an administrator on Windows");
}

/// Makes `link` a junction to the folder `target`, which anyone may make.
#[cfg(test)]
pub(crate) fn link_folder(target: &Path, link: &Path) {
    let done = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .output()
        .unwrap();
    assert!(done.status.success(), "{done:?}");
}

/// Lets the folder `dir` be listed, or not. Windows lets a process pass
/// through a folder it may not list, so reading what is in it is denied
/// too, by an entry each file inherits.
#[cfg(test)]
pub(crate) fn set_listable(dir: &Path, listable: bool) {
    let mut icacls = std::process::Command::new("icacls");
    icacls.arg(dir);
    if listable {
        icacls.args(["/remove:d", "*S-1-1-0"]);
    } else {
        icacls.args(["/deny", "*S-1-1-0:(OI)(CI)(RD)"]);
    }
    let done = icacls.output().unwrap();
    assert!(done.status.success(), "{done:?}");
}

/// Whether a file is read-only, for a listing: Windows has no mode.
#[cfg(test)]
pub(crate) fn mode_of(metadata: &Metadata) -> String {
    format!("{}", metadata.permissions().readonly())
}

/// Makes nothing under `root` writable, or everything again. A read-only
/// folder can still be written in, so everyone, this user too, is denied
/// writing, deleting and renaming under it. The entry is added through
/// .NET, which leaves `SYNCHRONIZE` out of a denial: icacls puts it in
/// every entry it makes, and denying it denies every open, reading
/// included.
#[cfg(test)]
pub(crate) fn seal(root: &Path, sealed: bool) {
    const DENY: &str = "$folder = [System.IO.DirectoryInfo]::new($env:RITORNEL_SEAL); \
        $acl = $folder.GetAccessControl(); \
        $acl.AddAccessRule([System.Security.AccessControl.FileSystemAccessRule]::new( \
            [System.Security.Principal.SecurityIdentifier]::new('S-1-1-0'), \
            'WriteData, AppendData, WriteExtendedAttributes, WriteAttributes, Delete, DeleteSubdirectoriesAndFiles', \
            'ContainerInherit, ObjectInherit', 'None', 'Deny')); \
        $folder.SetAccessControl($acl)";
    let done = if sealed {
        // A module path from PowerShell 7, when the tests run in it,
        // would only confuse Windows PowerShell.
        std::process::Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", DENY])
            .env("RITORNEL_SEAL", root)
            .env_remove("PSModulePath")
            .output()
    } else {
        std::process::Command::new("icacls")
            .arg(root)
            .args(["/remove:d", "*S-1-1-0"])
            .output()
    }
    .unwrap();
    assert!(done.status.success() && done.stderr.is_empty(), "{done:?}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_held_open_and_each_above_it_cannot_be_renamed_until_let_go() {
        let scratch = crate::testing::Scratch::new();
        let root = scratch.path();
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        let held = open_folder(root, &["a", "b"]).unwrap();
        assert!(std::fs::rename(root.join("a/b"), root.join("a/moved")).is_err());
        assert!(std::fs::rename(root.join("a"), root.join("moved")).is_err());
        // What is in a held folder may still change.
        std::fs::write(root.join("a/b/01.flac"), b"12345").unwrap();
        std::fs::remove_file(root.join("a/b/01.flac")).unwrap();
        drop(held);
        std::fs::rename(root.join("a"), root.join("moved")).unwrap();
    }

    #[test]
    fn a_junction_on_the_way_is_not_a_folder() {
        let scratch = crate::testing::Scratch::new();
        let root = scratch.path().join("root");
        std::fs::create_dir_all(root.join("a")).unwrap();
        std::fs::create_dir_all(scratch.path().join("outside/b")).unwrap();
        link_folder(&scratch.path().join("outside"), &root.join("a/link"));
        let refused = open_folder(&root, &["a", "link", "b"]).unwrap_err();
        assert!(met_a_link(&refused), "{refused:?}");
        let refused = open_folder(&root, &["a", "link"]).unwrap_err();
        assert!(met_a_link(&refused), "{refused:?}");
        assert!(open_folder(&root, &["a"]).is_ok());
    }
}
