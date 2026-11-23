//! Files and folders only their owner can open: what mode `0600` and `0700`
//! are on Unix.
//!
//! Windows says who may open a file in its access list, not in mode bits.
//! [`restrict`] replaces a file's list with one entry, for the user this
//! process runs as, and stops it inheriting any from its folder. On a folder
//! the entry is inherited by what is made in it, so a file created there
//! is closed from the moment it exists. [`is_private`] is the check: no entry
//! lets anyone else do more than see that the file is there. The system and
//! the administrators count as the owner, as root does on Unix, since they
//! can take any file whatever its list says.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;

use windows_sys::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, HANDLE, HLOCAL, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    GetSecurityInfo, SE_FILE_OBJECT, SetSecurityInfo,
};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, ACL_REVISION, AddAccessAllowedAceEx,
    CONTAINER_INHERIT_ACE, DACL_SECURITY_INFORMATION, EqualSid, GetAce, GetLengthSid,
    GetTokenInformation, INHERIT_ONLY_ACE, InitializeAcl, IsWellKnownSid, OBJECT_INHERIT_ACE,
    PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, TOKEN_QUERY, TOKEN_USER,
    TokenUser, WinBuiltinAdministratorsSid, WinCreatorOwnerRightsSid, WinLocalSystemSid,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ALL_ACCESS, FILE_FLAG_BACKUP_SEMANTICS, FILE_GENERIC_WRITE, FILE_READ_ATTRIBUTES,
    FILE_READ_EA, READ_CONTROL, SYNCHRONIZE, WRITE_DAC,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// The kinds of entry that let someone in. Only the plain one is read; the
/// others carry conditions or object types this does not weigh, so a file
/// that has one is taken to be open.
const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
const ACCESS_ALLOWED_COMPOUND_ACE_TYPE: u8 = 4;
const ACCESS_ALLOWED_OBJECT_ACE_TYPE: u8 = 5;
const ACCESS_ALLOWED_CALLBACK_ACE_TYPE: u8 = 9;
const ACCESS_ALLOWED_CALLBACK_OBJECT_ACE_TYPE: u8 = 11;

/// What anyone may be let do without being let in: see that the file is
/// there, read its attributes and its access list, and wait on it.
const HARMLESS: u32 = SYNCHRONIZE | READ_CONTROL | FILE_READ_ATTRIBUTES | FILE_READ_EA;

/// Opens what is at `path`, file or folder, to read its access list and,
/// with `change`, to set it.
fn open(path: &Path, change: bool) -> io::Result<File> {
    let mut access = READ_CONTROL;
    if change {
        access |= WRITE_DAC;
    }
    OpenOptions::new()
        .access_mode(access)
        // A folder can only be opened with this, and it changes nothing for
        // a file.
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
}

/// The user this process runs as: a `TOKEN_USER` and the SID it points
/// into, in one buffer aligned for both.
struct User(Vec<u64>);

impl User {
    fn current() -> io::Result<Self> {
        let mut raw: HANDLE = std::ptr::null_mut();
        // SAFETY: the pseudo-handle of this process needs no closing, and
        // `raw` is written by the call.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the call succeeded, so `raw` is a handle this code owns.
        let token = unsafe { OwnedHandle::from_raw_handle(raw) };
        let mut len = 0u32;
        // SAFETY: a null buffer of length 0 asks only for the length.
        let asked = unsafe {
            GetTokenInformation(
                token.as_raw_handle() as HANDLE,
                TokenUser,
                std::ptr::null_mut(),
                0,
                &mut len,
            )
        };
        let error = io::Error::last_os_error();
        if asked != 0 || error.raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32) {
            return Err(error);
        }
        let mut buffer = vec![0u64; (len as usize).div_ceil(8)];
        // SAFETY: the buffer holds at least `len` bytes, and is aligned for
        // the pointer `TOKEN_USER` starts with.
        let filled = unsafe {
            GetTokenInformation(
                token.as_raw_handle() as HANDLE,
                TokenUser,
                buffer.as_mut_ptr().cast(),
                len,
                &mut len,
            )
        };
        if filled == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(buffer))
    }

    /// The user's SID. It points into the buffer, so it lives as long as
    /// `self`.
    fn sid(&self) -> PSID {
        // SAFETY: the buffer was filled with a TOKEN_USER by the system.
        unsafe { (*self.0.as_ptr().cast::<TOKEN_USER>()).User.Sid }
    }
}

/// Closes `path` to everyone but the user this process runs as: its access
/// list becomes that one user, with full control, and inherits nothing. A
/// folder passes the entry on to what is made in it.
pub fn restrict(path: &Path) -> io::Result<()> {
    let file = open(path, true)?;
    let folder = file.metadata()?.is_dir();
    restrict_handle(&file, folder)
}

/// [`restrict`] on a file already open with `WRITE_DAC` access, as one
/// just created with [`create_restricted`] is.
fn restrict_handle(file: &File, folder: bool) -> io::Result<()> {
    let user = User::current()?;
    let sid = user.sid();
    // SAFETY: `sid` is a valid SID for as long as `user` lives.
    let sid_len = unsafe { GetLengthSid(sid) } as usize;
    // An ACL header and one entry, whose SID replaces its last field.
    let len = std::mem::size_of::<ACL>() + std::mem::size_of::<ACCESS_ALLOWED_ACE>()
        - std::mem::size_of::<u32>()
        + sid_len;
    let len = len.next_multiple_of(4);
    let mut buffer = vec![0u64; len.div_ceil(8)];
    let acl = buffer.as_mut_ptr().cast::<ACL>();
    let inherit = if folder {
        OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE
    } else {
        0
    };
    // SAFETY: the buffer is at least `len` bytes and aligned for an ACL,
    // and the length passed is no more than it holds.
    if unsafe { InitializeAcl(acl, len as u32, ACL_REVISION) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the ACL was made with room for exactly this entry.
    if unsafe { AddAccessAllowedAceEx(acl, ACL_REVISION, inherit, FILE_ALL_ACCESS, sid) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the handle is open with WRITE_DAC, and the ACL is valid for
    // the call.
    let set = unsafe {
        SetSecurityInfo(
            file.as_raw_handle() as HANDLE,
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            acl,
            std::ptr::null(),
        )
    };
    if set != 0 {
        return Err(io::Error::from_raw_os_error(set as i32));
    }
    Ok(())
}

/// Creates a new file at `path` and closes it to everyone but this user
/// before anything is written to it. It fails if something is there.
pub fn create_restricted(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .access_mode(FILE_GENERIC_WRITE | READ_CONTROL | WRITE_DAC)
        .open(path)?;
    restrict_handle(&file, false)?;
    Ok(file)
}

/// A security descriptor the system allocated, freed when dropped.
struct Descriptor(PSECURITY_DESCRIPTOR);

impl Drop for Descriptor {
    fn drop(&mut self) {
        // SAFETY: the system allocated it with LocalAlloc, and it is freed
        // once.
        unsafe { LocalFree(self.0 as HLOCAL) };
    }
}

/// Whether nobody but the user this process runs as (or the system, or the
/// administrators) may do more with `file` than see that it is there.
pub fn is_private(file: &File) -> io::Result<bool> {
    let user = User::current()?;
    let mut acl: *mut ACL = std::ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: the handle is the file's own; `acl` and `descriptor` are
    // written by the call, and `acl` points into `descriptor`.
    let got = unsafe {
        GetSecurityInfo(
            file.as_raw_handle() as HANDLE,
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut acl,
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    if got != 0 {
        return Err(io::Error::from_raw_os_error(got as i32));
    }
    let _descriptor = Descriptor(descriptor);
    // No list at all lets everyone in.
    if acl.is_null() {
        return Ok(false);
    }
    // SAFETY: `acl` is valid while `_descriptor` lives.
    let count = unsafe { (*acl).AceCount };
    for index in 0..u32::from(count) {
        let mut entry: *mut std::ffi::c_void = std::ptr::null_mut();
        // SAFETY: the index is below the list's count.
        if unsafe { GetAce(acl, index, &mut entry) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: every entry starts with its header.
        let header = unsafe { *entry.cast::<ACE_HEADER>() };
        if u32::from(header.AceFlags) & INHERIT_ONLY_ACE != 0 {
            continue;
        }
        match header.AceType {
            ACCESS_ALLOWED_ACE_TYPE => {
                let allowed = entry.cast::<ACCESS_ALLOWED_ACE>();
                // SAFETY: an entry of this type is an ACCESS_ALLOWED_ACE,
                // whose SID starts at its last field.
                let (mask, sid) = unsafe {
                    (
                        (*allowed).Mask,
                        std::ptr::addr_of_mut!((*allowed).SidStart).cast(),
                    )
                };
                if mask & !HARMLESS != 0 && !trusted(sid, &user) {
                    return Ok(false);
                }
            }
            ACCESS_ALLOWED_COMPOUND_ACE_TYPE
            | ACCESS_ALLOWED_OBJECT_ACE_TYPE
            | ACCESS_ALLOWED_CALLBACK_ACE_TYPE
            | ACCESS_ALLOWED_CALLBACK_OBJECT_ACE_TYPE => return Ok(false),
            // Entries that deny, audit or label let nobody in.
            _ => {}
        }
    }
    Ok(true)
}

/// Whether an entry for `sid` lets in nobody but the owner, or those who
/// can take any file anyway.
fn trusted(sid: PSID, user: &User) -> bool {
    // SAFETY: both SIDs are valid for the call.
    unsafe {
        EqualSid(sid, user.sid()) != 0
            || IsWellKnownSid(sid, WinLocalSystemSid) != 0
            || IsWellKnownSid(sid, WinBuiltinAdministratorsSid) != 0
            || IsWellKnownSid(sid, WinCreatorOwnerRightsSid) != 0
    }
}

/// Whether what is at `path` is private, as [`is_private`] says.
pub fn path_is_private(path: &Path) -> io::Result<bool> {
    is_private(&open(path, false)?)
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::process::Command;

    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        tempfile::Builder::new()
            .prefix(&format!("ritornel-windows-private-{name}-"))
            .tempdir()
            .unwrap()
            .keep()
    }

    /// Lets everyone read `path`, as a copy from another machine might. On a
    /// folder the entry is inherited, so what is made in it is open too
    /// whatever the temporary folder's own list says.
    fn open_to_everyone(path: &Path) {
        let grant = if path.is_dir() {
            "*S-1-1-0:(OI)(CI)(R)"
        } else {
            "*S-1-1-0:(R)"
        };
        let done = Command::new("icacls")
            .arg(path)
            .args(["/grant", grant])
            .output()
            .unwrap();
        assert!(done.status.success(), "{done:?}");
    }

    #[test]
    fn a_restricted_file_is_private_and_one_open_to_everyone_is_not() {
        let dir = scratch("file");
        let path = dir.join("secret");
        std::fs::write(&path, "x").unwrap();
        open_to_everyone(&path);
        assert!(!path_is_private(&path).unwrap());
        restrict(&path).unwrap();
        assert!(path_is_private(&path).unwrap());
        // It can still be read and written by its owner.
        std::fs::write(&path, "y").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "y");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn what_is_made_in_a_restricted_folder_is_private_from_the_start() {
        let dir = scratch("folder");
        open_to_everyone(&dir);
        let inner = dir.join("data");
        std::fs::create_dir(&inner).unwrap();
        assert!(!path_is_private(&inner).unwrap());
        restrict(&inner).unwrap();
        assert!(path_is_private(&inner).unwrap());
        std::fs::write(inner.join("made-after"), "x").unwrap();
        std::fs::create_dir(inner.join("logs")).unwrap();
        assert!(path_is_private(&inner.join("made-after")).unwrap());
        assert!(path_is_private(&inner.join("logs")).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_created_restricted_is_private_before_it_holds_anything() {
        let dir = scratch("create");
        open_to_everyone(&dir);
        let path = dir.join("token");
        let mut file = create_restricted(&path).unwrap();
        assert!(path_is_private(&path).unwrap());
        writeln!(file, "token").unwrap();
        drop(file);
        assert!(create_restricted(&path).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "token\n");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
