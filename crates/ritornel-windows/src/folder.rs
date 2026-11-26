//! Opening a name relative to an open folder, as Unix's `openat` does, and
//! listing a folder by its handle. Nothing is found again by a path from
//! elsewhere, so a folder above that is renamed, or replaced by a link or
//! a junction, changes nothing that is reached through the handle.

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle};

use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
use windows_sys::Wdk::Storage::FileSystem::{
    FILE_DIRECTORY_FILE, FILE_OPEN, FILE_OPEN_REPARSE_POINT, FILE_SYNCHRONOUS_IO_NONALERT,
    NtCreateFile,
};
use windows_sys::Win32::Foundation::{
    ERROR_NO_MORE_FILES, HANDLE, OBJ_CASE_INSENSITIVE, RtlNtStatusToDosError, UNICODE_STRING,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FULL_DIR_INFO, FILE_GENERIC_READ,
    FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, FILE_TRAVERSE, FileFullDirectoryInfo, FileFullDirectoryRestartInfo,
    GetFileInformationByHandleEx, SYNCHRONIZE,
};
use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

/// What a name is opened for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Open {
    /// A folder, to list and to open names in. Anything else is refused,
    /// as `ERROR_DIRECTORY`.
    Folder,
    /// Anything, to read.
    Read,
    /// Anything, to read its attributes only.
    Attributes,
}

/// Opens `name`, a single name, in the folder `dir` is open on, relative to
/// that handle. A link or a junction at `name` is followed only if
/// `follow` says to: if not, the link itself is opened. What is opened may
/// still be read, written, renamed and deleted by others. A name that is
/// empty, `.` or `..`, or holds a separator, a colon or a NUL, is refused.
pub fn open_in(dir: &File, name: &OsStr, open: Open, follow: bool) -> io::Result<File> {
    let wide: Vec<u16> = name.encode_wide().collect();
    let plain = !wide.is_empty()
        && name != "."
        && name != ".."
        && !wide
            .iter()
            .any(|&unit| [0, u16::from(b'\\'), u16::from(b'/'), u16::from(b':')].contains(&unit));
    let bytes = u16::try_from(wide.len() * 2).ok().filter(|_| plain);
    let Some(bytes) = bytes else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a single name",
        ));
    };
    let object = UNICODE_STRING {
        Length: bytes,
        MaximumLength: bytes,
        Buffer: wide.as_ptr().cast_mut(),
    };
    let attributes = OBJECT_ATTRIBUTES {
        Length: u32::try_from(std::mem::size_of::<OBJECT_ATTRIBUTES>()).unwrap_or(u32::MAX),
        RootDirectory: dir.as_raw_handle() as HANDLE,
        ObjectName: &object,
        // Win32 names are matched without regard to case.
        Attributes: OBJ_CASE_INSENSITIVE,
        SecurityDescriptor: std::ptr::null(),
        SecurityQualityOfService: std::ptr::null(),
    };
    let (access, mut options) = match open {
        Open::Folder => (
            FILE_LIST_DIRECTORY | FILE_TRAVERSE | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
            FILE_DIRECTORY_FILE,
        ),
        Open::Read => (FILE_GENERIC_READ, 0),
        Open::Attributes => (FILE_READ_ATTRIBUTES | SYNCHRONIZE, 0),
    };
    // Synchronous, as the standard library's own handles are.
    options |= FILE_SYNCHRONOUS_IO_NONALERT;
    if !follow {
        options |= FILE_OPEN_REPARSE_POINT;
    }
    let mut handle: HANDLE = std::ptr::null_mut();
    // SAFETY: plain data, for which all zeroes is valid.
    let mut status: IO_STATUS_BLOCK = unsafe { std::mem::zeroed() };
    // SAFETY: every pointer is to a value that outlives the call, and the
    // name's buffer holds `bytes` bytes.
    let done = unsafe {
        NtCreateFile(
            &mut handle,
            access,
            &attributes,
            &mut status,
            std::ptr::null(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            FILE_OPEN,
            options,
            std::ptr::null(),
            0,
        )
    };
    if done < 0 {
        // SAFETY: a plain conversion of one status code.
        let code = unsafe { RtlNtStatusToDosError(done) };
        return Err(io::Error::from_raw_os_error(
            i32::try_from(code).unwrap_or(i32::MAX),
        ));
    }
    // SAFETY: the call succeeded, so `handle` is open and now owned here.
    Ok(unsafe { File::from_raw_handle(handle as _) })
}

/// One entry in a folder's listing, as the folder records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: OsString,
    /// Its `FILE_ATTRIBUTE_*` flags.
    pub attributes: u32,
    /// Its reparse tag, if it is a reparse point.
    pub reparse_tag: Option<u32>,
}

impl Entry {
    /// Whether it is a link: a symbolic link, a junction, or another
    /// reparse point that names somewhere else, as `file::is_link` says.
    pub fn is_link(&self) -> bool {
        self.reparse_tag.is_some_and(crate::file::names_elsewhere)
    }

    /// Whether it is a folder, and not a link to one.
    pub fn is_folder(&self) -> bool {
        self.attributes & FILE_ATTRIBUTE_DIRECTORY != 0 && !self.is_link()
    }
}

/// Every entry in the folder `dir` is open on, but `.` and `..`, read from
/// the handle. `dir` must be open to list, as `Open::Folder` opens it.
pub fn list(dir: &File) -> io::Result<Vec<Entry>> {
    use std::mem::offset_of;
    // Room for many entries at once, in words, so that each record is
    // aligned as the call asks.
    let mut buffer = vec![0u64; 8 * 1024];
    let size = u32::try_from(buffer.len() * 8).unwrap_or(u32::MAX);
    let mut class = FileFullDirectoryRestartInfo;
    let mut entries = Vec::new();
    let malformed = || io::Error::new(io::ErrorKind::InvalidData, "a malformed folder listing");
    loop {
        // SAFETY: the handle is open for the call, and the buffer holds
        // `size` bytes.
        let ok = unsafe {
            GetFileInformationByHandleEx(
                dir.as_raw_handle() as HANDLE,
                class,
                buffer.as_mut_ptr().cast(),
                size,
            )
        };
        if ok == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == i32::try_from(ERROR_NO_MORE_FILES).ok() {
                return Ok(entries);
            }
            return Err(error);
        }
        class = FileFullDirectoryInfo;
        let bytes: Vec<u8> = buffer.iter().flat_map(|word| word.to_ne_bytes()).collect();
        let mut at = 0;
        loop {
            let field = |offset: usize| {
                let start = at + offset;
                bytes
                    .get(start..start + 4)
                    .and_then(|field| field.try_into().ok())
                    .map(u32::from_ne_bytes)
                    .ok_or_else(malformed)
            };
            let next = field(offset_of!(FILE_FULL_DIR_INFO, NextEntryOffset))?;
            let attributes = field(offset_of!(FILE_FULL_DIR_INFO, FileAttributes))?;
            let length = field(offset_of!(FILE_FULL_DIR_INFO, FileNameLength))? as usize;
            // The reparse tag of a reparse point is where the size of its
            // extended attributes would be.
            let tag = field(offset_of!(FILE_FULL_DIR_INFO, EaSize))?;
            let start = at + offset_of!(FILE_FULL_DIR_INFO, FileName);
            let name: Vec<u16> = bytes
                .get(start..start + length)
                .ok_or_else(malformed)?
                .as_chunks::<2>()
                .0
                .iter()
                .map(|unit| u16::from_ne_bytes(*unit))
                .collect();
            let name = OsString::from_wide(&name);
            if name != "." && name != ".." {
                entries.push(Entry {
                    name,
                    attributes,
                    reparse_tag: (attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0).then_some(tag),
                });
            }
            if next == 0 {
                break;
            }
            at += next as usize;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS;

    fn scratch() -> std::path::PathBuf {
        tempfile::Builder::new()
            .prefix("ritornel-windows-folder-")
            .tempdir()
            .unwrap()
            .keep()
    }

    fn open_dir(path: &std::path::Path) -> File {
        std::fs::OpenOptions::new()
            .access_mode(FILE_LIST_DIRECTORY | FILE_TRAVERSE | FILE_READ_ATTRIBUTES | SYNCHRONIZE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
            .unwrap()
    }

    #[test]
    fn a_folder_is_listed_and_opened_in_by_its_handle_after_it_is_renamed() {
        let dir = scratch();
        std::fs::create_dir_all(dir.join("held/inner")).unwrap();
        std::fs::write(dir.join("held/01.flac"), b"12345").unwrap();
        std::fs::write(dir.join("held/Long name with spaces.flac"), b"x").unwrap();
        let held = open_dir(&dir.join("held"));
        // Renamed, and another folder made at its old path: the handle
        // still reaches the one it was opened on.
        std::fs::rename(dir.join("held"), dir.join("moved")).unwrap();
        std::fs::create_dir(dir.join("held")).unwrap();
        let mut names: Vec<(OsString, bool)> = list(&held)
            .unwrap()
            .into_iter()
            .map(|entry| (entry.name.clone(), entry.is_folder()))
            .collect();
        names.sort();
        assert_eq!(
            names,
            [
                (OsString::from("01.flac"), false),
                (OsString::from("Long name with spaces.flac"), false),
                (OsString::from("inner"), true),
            ]
        );
        let mut file = open_in(&held, OsStr::new("01.FLAC"), Open::Read, false).unwrap();
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut file, &mut bytes).unwrap();
        assert_eq!(bytes, b"12345");
        let inner = open_in(&held, OsStr::new("inner"), Open::Folder, false).unwrap();
        assert!(list(&inner).unwrap().is_empty());
        let error = open_in(&held, OsStr::new("01.flac"), Open::Folder, false).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotADirectory, "{error:?}");
        let error = open_in(&held, OsStr::new("gone.flac"), Open::Read, false).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound, "{error:?}");
        for name in ["", ".", "..", "inner\\x", "a/b", "01.flac:stream"] {
            let error = open_in(&held, OsStr::new(name), Open::Read, false).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{name}");
        }
        drop((held, inner, file));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_junction_is_listed_as_a_link_and_opened_as_one_unless_followed() {
        let dir = scratch();
        std::fs::create_dir_all(dir.join("outside")).unwrap();
        std::fs::write(dir.join("outside/escaped.flac"), b"x").unwrap();
        std::fs::create_dir(dir.join("root")).unwrap();
        let done = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(dir.join("root").join("link"))
            .arg(dir.join("outside"))
            .output()
            .unwrap();
        assert!(done.status.success(), "{done:?}");
        let root = open_dir(&dir.join("root"));
        let entries = list(&root).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].is_link() && !entries[0].is_folder());
        let link = open_in(&root, OsStr::new("link"), Open::Folder, false).unwrap();
        assert!(crate::file::is_link(&link).unwrap());
        let followed = open_in(&root, OsStr::new("link"), Open::Folder, true).unwrap();
        assert!(!crate::file::is_link(&followed).unwrap());
        assert_eq!(list(&followed).unwrap()[0].name, "escaped.flac");
        drop((root, link, followed));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
