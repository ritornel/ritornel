//! Windows: the precise interrupt time, a boot ID made from the registry, a
//! folder's volume and file index, and links refused by looking before the
//! open and checking where the handle is after. What the standard library
//! cannot ask is in `ritornel-windows`.

use std::fs::{File, Metadata};
use std::path::Path;

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

/// Opens `name` in the folders `dirs` under `root`. Windows opens nothing
/// relative to a folder that is open, so each folder on the way is looked
/// at without following it, and a link or a junction is refused, as on
/// Unix; the file is opened without following a link either. What was
/// looked at may change before the open, so the handle is then asked where
/// the file really is, and that must be inside where the root really is.
pub(crate) fn open_inside(root: &Path, dirs: &[&str], name: &str) -> std::io::Result<File> {
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    // FILE_ATTRIBUTE_REPARSE_POINT: a symbolic link, a junction, or another
    // thing that sends the system elsewhere.
    const REPARSE_POINT: u32 = 0x400;
    // FILE_FLAG_OPEN_REPARSE_POINT and FILE_FLAG_BACKUP_SEMANTICS.
    const OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const BACKUP_SEMANTICS: u32 = 0x0200_0000;
    let mut path = root.to_owned();
    for part in dirs {
        path.push(part);
        let meta = std::fs::symlink_metadata(&path)?;
        if meta.file_attributes() & REPARSE_POINT != 0 {
            return Err(refused("a link"));
        }
        if !meta.is_dir() {
            return Err(std::io::ErrorKind::NotADirectory.into());
        }
    }
    path.push(name);
    if std::fs::symlink_metadata(&path)?.is_dir() {
        return Err(refused("not a regular file"));
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(OPEN_REPARSE_POINT)
        .open(&path)?;
    if file.metadata()?.file_attributes() & REPARSE_POINT != 0 {
        return Err(refused("a link"));
    }
    let real_root = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(BACKUP_SEMANTICS)
        .open(root)
        .and_then(|root| ritornel_windows::file::final_path(&root))?;
    if !ritornel_windows::file::final_path(&file)?.starts_with(&real_root) {
        return Err(refused("a link"));
    }
    Ok(file)
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
