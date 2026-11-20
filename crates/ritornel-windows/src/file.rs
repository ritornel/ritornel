//! What a handle says about the file it is open on: which file it is, and
//! the path it really has; and the room left on a disk.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};

use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS, FILE_NAME_NORMALIZED,
    FILE_READ_ATTRIBUTES, GetDiskFreeSpaceExW, GetFileInformationByHandle,
    GetFinalPathNameByHandleW, VOLUME_NAME_DOS,
};

/// Which file or folder is at `path`: the serial number of its volume and
/// its index on that volume, as a device and an inode are on Unix. A disk
/// mounted again at the same letter or folder is another volume.
pub fn id(path: &Path) -> io::Result<(u64, u64)> {
    let file = OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?;
    // SAFETY: plain data, for which all zeroes is valid.
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: the handle is open for the call and `info` is written by it.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle() as HANDLE, &mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let index = (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow);
    Ok((u64::from(info.dwVolumeSerialNumber), index))
}

/// The path `file` is really open at, with every link and junction on the
/// way resolved, in the `\\?\` form.
pub fn final_path(file: &File) -> io::Result<PathBuf> {
    let mut buffer = vec![0u16; 512];
    loop {
        let capacity = u32::try_from(buffer.len()).unwrap_or(u32::MAX);
        // SAFETY: the buffer holds `capacity` UTF-16 units.
        let len = unsafe {
            GetFinalPathNameByHandleW(
                file.as_raw_handle() as HANDLE,
                buffer.as_mut_ptr(),
                capacity,
                FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
            )
        } as usize;
        if len == 0 {
            return Err(io::Error::last_os_error());
        }
        // Too small: `len` is what it needs, with the NUL.
        if len > buffer.len() {
            buffer.resize(len, 0);
            continue;
        }
        buffer.truncate(len);
        use std::os::windows::ffi::OsStringExt;
        return Ok(PathBuf::from(std::ffi::OsString::from_wide(&buffer)));
    }
}

/// The bytes this user may still write on the disk that holds `path`.
pub fn free_bytes(path: &Path) -> io::Result<u64> {
    let wide = crate::wide(path);
    let mut free = 0u64;
    // SAFETY: `wide` ends in a NUL, and the totals not asked for are null.
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut free,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(free)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "ritornel-windows-file-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn a_folder_keeps_its_id_through_a_rename_and_another_has_its_own() {
        let dir = scratch("id");
        let first = dir.join("first");
        std::fs::create_dir(&first).unwrap();
        std::fs::create_dir(dir.join("second")).unwrap();
        let before = id(&first).unwrap();
        assert_ne!(before, id(&dir.join("second")).unwrap());
        let renamed = dir.join("FIRST-renamed");
        std::fs::rename(&first, &renamed).unwrap();
        assert_eq!(id(&renamed).unwrap(), before);
        assert!(id(&first).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_final_path_is_the_one_on_disk_whatever_it_was_opened_by() {
        let dir = scratch("final");
        std::fs::write(dir.join("Mixed.flac"), "x").unwrap();
        let opened = File::open(dir.join("mixed.FLAC")).unwrap();
        let real = final_path(&opened).unwrap();
        assert_eq!(real.file_name().unwrap(), "Mixed.flac");
        assert!(real.to_string_lossy().starts_with(r"\\?\"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_disk_has_room_and_a_missing_folder_has_no_answer() {
        let dir = scratch("free");
        assert!(free_bytes(&dir).unwrap() > 0);
        assert!(free_bytes(&dir.join("missing")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
