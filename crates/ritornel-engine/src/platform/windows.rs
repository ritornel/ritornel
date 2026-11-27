//! Windows: the precise interrupt time, a boot ID made from the registry, a
//! folder's volume and file index, and folders opened and listed relative
//! to an open one. What the standard library cannot ask is in
//! `ritornel-windows`.

use std::ffi::{OsStr, OsString};
use std::fs::{File, Metadata};
use std::path::Path;

use ritornel_windows::folder::Open;

use super::{Described, Kind, refused};
use crate::Id;

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

/// A folder, open. Folders are opened in it, and what is in it listed,
/// described and opened, relative to its handle (`ritornel_windows::folder`,
/// as `openat` does on Unix), never by a path from elsewhere: a folder
/// above it that is renamed or replaced by a link or a junction meanwhile
/// changes nothing reached through it.
#[derive(Debug)]
pub(crate) struct Folder(File);

/// The error for a link where a folder or the file should be: a link that
/// is known to be one, never a check that could not be made.
fn a_link() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::NotADirectory, "a link")
}

/// Whether `file` is a reparse point of any kind.
fn is_reparse_point(file: &File) -> std::io::Result<bool> {
    use std::os::windows::fs::MetadataExt;
    // FILE_ATTRIBUTE_REPARSE_POINT.
    Ok(file.metadata()?.file_attributes() & 0x400 != 0)
}

/// `name` in `dir`, opened as `open` says without following it. A link is
/// refused. Another reparse point, as a cloud file's placeholder is, is
/// opened again as the file or folder it stands for, which must be the
/// same one by its file ID: if it is not, or the volume gives no ID, that
/// is an error, never taken for a link.
fn open_unfollowed(dir: &File, name: &OsStr, open: Open) -> std::io::Result<File> {
    let file = ritornel_windows::folder::open_in(dir, name, open, false)?;
    if !is_reparse_point(&file)? {
        return Ok(file);
    }
    if ritornel_windows::file::is_link(&file)? {
        return Err(a_link());
    }
    let real = ritornel_windows::folder::open_in(dir, name, open, true)?;
    // Not known to be a link, so not passed over as one: the walk fails
    // rather than take what is in it for gone.
    let unproven = || std::io::Error::other("a reparse point that cannot be told to be itself");
    let was = ritornel_windows::file::identity(&file)?.ok_or_else(unproven)?;
    if ritornel_windows::file::identity(&real)? != Some(was) {
        return Err(unproven());
    }
    Ok(real)
}

impl Folder {
    /// Opens the folder at `root`, following it if it is a link, as it was
    /// given.
    pub(crate) fn open_root(root: &Path) -> std::io::Result<Folder> {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_LIST_DIRECTORY, FILE_TRAVERSE, FILE_READ_ATTRIBUTES and
        // SYNCHRONIZE, as `ritornel_windows::folder::Open::Folder` opens one;
        // and FILE_FLAG_BACKUP_SEMANTICS, without which no folder opens.
        let access = 0x1 | 0x20 | 0x80 | 0x0010_0000;
        let root = std::fs::OpenOptions::new()
            .access_mode(access)
            .custom_flags(0x0200_0000)
            .open(root)?;
        // A file opens too, which `O_DIRECTORY` refuses on Unix.
        if !root.metadata()?.is_dir() {
            return Err(std::io::ErrorKind::NotADirectory.into());
        }
        Ok(Folder(root))
    }

    /// Opens the folders `dirs` in this one, each relative to the last and
    /// none followed if it is a link: one that is a link, or is not a
    /// folder, is `NotADirectory`, as [`met_a_link`] says.
    pub(crate) fn open_folder(&self, dirs: &[&str]) -> std::io::Result<Folder> {
        let mut dir = self.0.try_clone()?;
        for part in dirs {
            dir = open_unfollowed(&dir, OsStr::new(part), Open::Folder)?;
        }
        Ok(Folder(dir))
    }

    /// Which folder this is: its volume's serial number and its index on
    /// the volume.
    pub(crate) fn id(&self) -> Option<(u64, u64)> {
        ritornel_windows::file::id_of(&self.0).ok()
    }

    /// Every entry in the folder, with what it is, read from its handle.
    pub(crate) fn list(&self) -> std::io::Result<Vec<(OsString, Kind)>> {
        Ok(ritornel_windows::folder::list(&self.0)?
            .into_iter()
            .map(|entry| {
                let kind = if entry.is_folder() {
                    Kind::Folder
                } else if entry.is_link() {
                    Kind::Link
                } else {
                    Kind::Other
                };
                (entry.name, kind)
            })
            .collect())
    }

    /// What the file `name` in the folder is, following it if it is a
    /// link, as `std::fs::metadata` would say. A link that leads round to
    /// itself is a link, as on Unix.
    pub(crate) fn describe(&self, name: &OsStr) -> std::io::Result<Described> {
        // ERROR_CANT_RESOLVE_FILENAME.
        const LOOP: i32 = 1921;
        let file = ritornel_windows::folder::open_in(&self.0, name, Open::Attributes, true)
            .map_err(|error| match error.raw_os_error() {
                Some(LOOP) => a_link(),
                _ => error,
            })?;
        let metadata = file.metadata()?;
        Ok(Described {
            is_file: metadata.is_file(),
            len: metadata.len(),
            mtime_ms: crate::walk::mtime_ms(&metadata),
        })
    }

    /// Opens the file `name` in the folder, following it if it is a link
    /// only if `follow` says to. A folder is refused.
    pub(crate) fn open(&self, name: &str, follow: bool) -> std::io::Result<File> {
        let name = OsStr::new(name);
        let file = if follow {
            ritornel_windows::folder::open_in(&self.0, name, Open::Read, true)?
        } else {
            open_unfollowed(&self.0, name, Open::Read)?
        };
        if file.metadata()?.is_dir() {
            return Err(refused("not a regular file"));
        }
        Ok(file)
    }
}

/// Whether opening failed on a link where a folder or the file should be,
/// or on a file where a folder should be: what [`Folder::open_folder`] says
/// of a link it does not follow.
pub(crate) fn met_a_link(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::NotADirectory
}

/// Whether a failure says that what was asked for is not there, and so is
/// gone. The standard library takes a network path or a drive that cannot
/// be reached for one not found too, and that is not gone: only a file or
/// a path that is not there is.
pub(crate) fn entry_missing(error: &std::io::Error) -> bool {
    // ERROR_FILE_NOT_FOUND and ERROR_PATH_NOT_FOUND.
    match error.raw_os_error() {
        Some(code) => code == 2 || code == 3,
        None => error.kind() == std::io::ErrorKind::NotFound,
    }
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
/// `mklink` takes a `/` for the start of a switch, so each path is written
/// again with `\` between its parts.
#[cfg(test)]
pub(crate) fn link_folder(target: &Path, link: &Path) {
    let backslashed = |path: &Path| path.components().collect::<std::path::PathBuf>();
    let done = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(backslashed(link))
        .arg(backslashed(target))
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
    fn a_folder_open_is_still_reached_after_it_or_its_root_is_renamed() {
        let scratch = crate::testing::Scratch::new();
        let root = scratch.path().join("root");
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        std::fs::write(root.join("a/b/01.flac"), b"12345").unwrap();
        let opened = Folder::open_root(&root).unwrap();
        // Rename the root while only its own handle is open (Windows refuses
        // renaming a parent while a handle to a descendant is open), then put
        // a decoy at the old path: `opened` still reaches the moved tree.
        let moved_root = scratch.path().join("moved-root");
        std::fs::rename(&root, &moved_root).unwrap();
        std::fs::create_dir_all(root.join("a/b")).unwrap();

        let held = opened.open_folder(&["a"]).unwrap();
        // Now rename `a` itself while its handle is open and place a decoy at
        // `moved-root/a/b`: `held` still reaches the moved `a`.
        std::fs::rename(moved_root.join("a"), scratch.path().join("moved-a")).unwrap();
        std::fs::create_dir_all(moved_root.join("a/b")).unwrap();

        let inner = held.open_folder(&["b"]).unwrap();
        let names: Vec<OsString> = inner
            .list()
            .unwrap()
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        assert_eq!(names, ["01.flac"]);
        assert!(inner.open("01.flac", false).is_ok());
        // The decoy created at `moved-root/a/b` is a different directory.
        let decoy = opened.open_folder(&["a", "b"]).unwrap();
        assert_ne!(decoy.id(), inner.id());
    }

    #[test]
    fn a_junction_on_the_way_is_a_link_and_a_folder_is_not() {
        let scratch = crate::testing::Scratch::new();
        let root = scratch.path().join("root");
        std::fs::create_dir_all(root.join("a")).unwrap();
        std::fs::create_dir_all(scratch.path().join("outside/b")).unwrap();
        std::fs::write(scratch.path().join("outside/b/01.flac"), b"12345").unwrap();
        link_folder(&scratch.path().join("outside"), &root.join("a/link"));
        let root = Folder::open_root(&root).unwrap();
        for dirs in [&["a", "link", "b"][..], &["a", "link"]] {
            let refused = root.open_folder(dirs).unwrap_err();
            assert!(met_a_link(&refused), "{refused:?}");
        }
        let folder = root.open_folder(&["a"]).unwrap();
        assert_eq!(
            folder.list().unwrap(),
            [(OsString::from("link"), Kind::Link)]
        );
        // A file where a folder should be is not one either.
        std::fs::write(scratch.path().join("root/a/file"), b"").unwrap();
        assert!(met_a_link(&root.open_folder(&["a", "file"]).unwrap_err()));
    }
}
