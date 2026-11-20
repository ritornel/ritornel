//! A lock on a range of a file, which Windows enforces: while one handle
//! holds a range exclusively, no other handle may read or write those bytes.
//! The standard library's `File::lock` takes the whole file, so a lock file
//! that says who holds it could not be read. A range past the file's end
//! leaves what it holds readable, and still conflicts with the whole-file
//! locks the standard library takes, which cover every byte.
//!
//! The system lets a lock go when its handle closes, however the process
//! ends.

use std::fs::File;
use std::io;
use std::os::windows::io::AsRawHandle;

use windows_sys::Win32::Foundation::{ERROR_IO_PENDING, ERROR_LOCK_VIOLATION, HANDLE};
use windows_sys::Win32::Storage::FileSystem::{
    LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY, LockFileEx, UnlockFileEx,
};
use windows_sys::Win32::System::IO::OVERLAPPED;

/// Which way a range is held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hold {
    /// By this handle alone.
    Exclusive,
    /// By any number of handles, none of them exclusively.
    Shared,
}

/// Where a range starts, as the call takes it.
fn at(offset: u64) -> OVERLAPPED {
    // SAFETY: OVERLAPPED is plain data, for which all zeroes is valid.
    let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
    overlapped.Anonymous.Anonymous.Offset = offset as u32;
    overlapped.Anonymous.Anonymous.OffsetHigh = (offset >> 32) as u32;
    overlapped
}

/// Tries to lock `len` bytes of `file` from `offset`, without waiting.
/// `Ok(false)` when another handle holds a range that conflicts.
pub fn try_lock(file: &File, offset: u64, len: u64, hold: Hold) -> io::Result<bool> {
    let mut flags = LOCKFILE_FAIL_IMMEDIATELY;
    if hold == Hold::Exclusive {
        flags |= LOCKFILE_EXCLUSIVE_LOCK;
    }
    let mut overlapped = at(offset);
    // SAFETY: the handle is the file's own and open for the call. The
    // standard library opens files for synchronous I/O, so the call
    // finishes before it returns and `overlapped` outlives it.
    let locked = unsafe {
        LockFileEx(
            file.as_raw_handle() as HANDLE,
            flags,
            0,
            len as u32,
            (len >> 32) as u32,
            &mut overlapped,
        )
    };
    if locked != 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error().map(|code| code as u32) {
        Some(ERROR_LOCK_VIOLATION | ERROR_IO_PENDING) => Ok(false),
        _ => Err(error),
    }
}

/// Lets go of a range this handle locked, exactly as it was locked.
pub fn unlock(file: &File, offset: u64, len: u64) -> io::Result<()> {
    let mut overlapped = at(offset);
    // SAFETY: as in `try_lock`.
    let unlocked = unsafe {
        UnlockFileEx(
            file.as_raw_handle() as HANDLE,
            0,
            len as u32,
            (len >> 32) as u32,
            &mut overlapped,
        )
    };
    if unlocked == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs::TryLockError;
    use std::io::{Read, Write};

    use super::*;

    const FAR: u64 = u64::MAX - 1;

    fn scratch(name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "ritornel-windows-lock-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        path
    }

    #[test]
    fn a_far_byte_held_leaves_the_file_readable_and_refuses_a_whole_file_lock() {
        let path = scratch("far");
        let mut holder = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        assert!(try_lock(&holder, FAR, 1, Hold::Exclusive).unwrap());
        writeln!(holder, "4312").unwrap();

        // Another handle reads what it holds, and cannot take the file.
        let mut other = File::open(&path).unwrap();
        let mut text = String::new();
        other.read_to_string(&mut text).unwrap();
        assert_eq!(text, "4312\n");
        assert!(matches!(
            other.try_lock_shared(),
            Err(TryLockError::WouldBlock)
        ));
        assert!(!try_lock(&other, FAR, 1, Hold::Exclusive).unwrap());
        assert!(!try_lock(&other, FAR, 1, Hold::Shared).unwrap());

        // Let go, it can be had shared by many, and then not exclusively.
        unlock(&holder, FAR, 1).unwrap();
        other.try_lock_shared().unwrap();
        assert!(try_lock(&holder, FAR, 1, Hold::Shared).unwrap());
        let third = File::open(&path).unwrap();
        assert!(!try_lock(&third, FAR, 1, Hold::Exclusive).unwrap());
        drop((holder, other, third));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn closing_the_handle_lets_the_lock_go() {
        let path = scratch("close");
        let holder = File::create(&path).unwrap();
        assert!(try_lock(&holder, FAR, 1, Hold::Exclusive).unwrap());
        drop(holder);
        let next = File::open(&path).unwrap();
        assert!(try_lock(&next, FAR, 1, Hold::Exclusive).unwrap());
        drop(next);
        let _ = std::fs::remove_file(&path);
    }
}
