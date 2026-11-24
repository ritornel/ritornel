//! Files and folders only their owner can open: what mode `0600` and `0700`
//! are on Unix.
//!
//! Windows says who may open a file in its access list, not in mode bits.
//! [`create_restricted`] and [`create_restricted_dir`] make a file or a
//! folder whose list has one entry, for the user this process runs as, and
//! inherits nothing from its folder, from the moment it exists.
//! [`restrict`] gives something already there that list. On a folder the
//! entry is inherited by what is made in it, so a file created there, by
//! this code or by SQLite, is closed from the moment it exists.
//!
//! [`is_private`] is the check: the file is owned by this user, and no
//! entry lets anyone else do more than see that it is there. On a folder
//! that counts the entries it passes on to what is made in it too. The
//! system and the administrators count as the owner, as root does on Unix,
//! since they can take any file whatever its list says.

use std::fs::{File, OpenOptions};
use std::io;
use std::marker::PhantomData;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;

use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_INSUFFICIENT_BUFFER, HANDLE, HLOCAL, INVALID_HANDLE_VALUE, LUID,
    LocalFree,
};
use windows_sys::Win32::Security::Authorization::{
    AUTHZ_ACCESS_REPLY, AUTHZ_ACCESS_REQUEST, AUTHZ_RM_FLAG_NO_AUDIT, AUTHZ_SKIP_TOKEN_GROUPS,
    AuthzAccessCheck, AuthzAddSidsToContext, AuthzFreeContext, AuthzFreeResourceManager,
    AuthzInitializeContextFromSid, AuthzInitializeResourceManager, ConvertStringSidToSidW,
    GetSecurityInfo, SE_FILE_OBJECT, SetSecurityInfo,
};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, ACL_REVISION, AddAccessAllowedAceEx,
    CONTAINER_INHERIT_ACE, CreateWellKnownSid, DACL_SECURITY_INFORMATION, EqualSid,
    GROUP_SECURITY_INFORMATION, GetAce, GetLengthSid, GetSecurityDescriptorControl,
    GetSecurityDescriptorDacl, GetSecurityDescriptorOwner, GetTokenInformation, INHERIT_ONLY_ACE,
    INHERITED_ACE, InitializeAcl, InitializeSecurityDescriptor, IsWellKnownSid, OBJECT_INHERIT_ACE,
    OBJECT_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR, PSID, SE_DACL_PROTECTED, SECURITY_ATTRIBUTES, SECURITY_DESCRIPTOR,
    SECURITY_MAX_SID_SIZE, SID_AND_ATTRIBUTES, SetSecurityDescriptorControl,
    SetSecurityDescriptorDacl, TOKEN_QUERY, TOKEN_USER, TokenUser, WELL_KNOWN_SID_TYPE,
    WinAuthenticatedUserSid, WinBuiltinAdministratorsSid, WinBuiltinAnyPackageSid,
    WinBuiltinUsersSid, WinCreatorOwnerRightsSid, WinCreatorOwnerSid, WinInteractiveSid,
    WinLocalSystemSid, WinWorldSid,
};
use windows_sys::Win32::Storage::FileSystem::{
    CREATE_NEW, CreateDirectoryW, CreateFileW, FILE_ALL_ACCESS, FILE_ATTRIBUTE_NORMAL,
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_GENERIC_WRITE, FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_READ_EA, FILE_SHARE_DELETE,
    FILE_SHARE_READ, FILE_SHARE_WRITE, GetVolumeInformationByHandleW, READ_CONTROL, SYNCHRONIZE,
    WRITE_DAC, WRITE_OWNER,
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

/// The revision of a security descriptor made here, the access that asks
/// for all that would be granted, a group that counts, and the volume flag
/// saying the file system keeps access lists: `winnt.h`'s values.
const SECURITY_DESCRIPTOR_REVISION: u32 = 1;
const MAXIMUM_ALLOWED: u32 = 0x0200_0000;
const SE_GROUP_ENABLED: u32 = 4;
const FILE_PERSISTENT_ACLS: u32 = 0x0000_0008;

/// What anyone may be let do without being let in: see that the file is
/// there, read its attributes and its access list, and wait on it.
const HARMLESS: u32 = SYNCHRONIZE | READ_CONTROL | FILE_READ_ATTRIBUTES | FILE_READ_EA;

/// What lets someone change who may open a folder: its list, its owner, or
/// everything (`GENERIC_ALL`).
const CHANGE: u32 = WRITE_DAC | WRITE_OWNER | 0x1000_0000;

/// Opens what is at `path`, file or folder, to read its attributes and its
/// access list and, with `change`, to set the list.
fn open(path: &Path, change: bool) -> io::Result<File> {
    let mut access = READ_CONTROL | FILE_READ_ATTRIBUTES;
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

/// A SID borrowed from its owner, such as a [`User`] or a [`Descriptor`].
/// Tying the pointer to that lifetime stops it outliving the buffer it
/// points into.
#[derive(Clone, Copy)]
struct Sid<'a>(PSID, PhantomData<&'a ()>);

/// An access list borrowed from a [`Descriptor`].
#[derive(Clone, Copy)]
struct Acl<'a>(*const ACL, PhantomData<&'a Descriptor>);

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
    fn sid(&self) -> Sid<'_> {
        // SAFETY: the buffer was filled with a TOKEN_USER by the system.
        Sid(
            unsafe { (*self.0.as_ptr().cast::<TOKEN_USER>()).User.Sid },
            PhantomData,
        )
    }
}

/// An access list of one entry: the user this process runs as, with full
/// control, passed on to what is made in it when it is a folder's. The
/// entry holds a copy of the SID.
struct OwnerOnly(Vec<u64>);

impl OwnerOnly {
    fn new(folder: bool) -> io::Result<Self> {
        let user = User::current()?;
        let sid = user.sid();
        // SAFETY: `sid` is a valid SID for as long as `user` lives.
        let sid_len = unsafe { GetLengthSid(sid.0) } as usize;
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
        // SAFETY: the buffer is at least `len` bytes and aligned for an
        // ACL, and the length passed is no more than it holds.
        if unsafe { InitializeAcl(acl, len as u32, ACL_REVISION) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the ACL was made with room for exactly this entry.
        if unsafe { AddAccessAllowedAceEx(acl, ACL_REVISION, inherit, FILE_ALL_ACCESS, sid.0) } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(buffer))
    }

    fn acl(&mut self) -> *mut ACL {
        self.0.as_mut_ptr().cast()
    }
}

/// Calls `make` with security attributes that give what it creates an
/// [`OwnerOnly`] list and nothing inherited from the folder it is made in,
/// so it is closed from the moment it exists.
fn owner_only<T>(
    folder: bool,
    make: impl FnOnce(*const SECURITY_ATTRIBUTES) -> io::Result<T>,
) -> io::Result<T> {
    let mut list = OwnerOnly::new(folder)?;
    // SAFETY: plain data, for which all zeroes is valid; it is initialized
    // below before it is used.
    let mut descriptor: SECURITY_DESCRIPTOR = unsafe { std::mem::zeroed() };
    let pointer: PSECURITY_DESCRIPTOR = (&raw mut descriptor).cast();
    // SAFETY: `pointer` is a SECURITY_DESCRIPTOR this function owns.
    if unsafe { InitializeSecurityDescriptor(pointer, SECURITY_DESCRIPTOR_REVISION) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the list outlives the descriptor, which points to it.
    if unsafe { SetSecurityDescriptorDacl(pointer, 1, list.acl(), 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: as above; this stops the folder's entries being added.
    if unsafe { SetSecurityDescriptorControl(pointer, SE_DACL_PROTECTED, SE_DACL_PROTECTED) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: pointer,
        bInheritHandle: 0,
    };
    make(&attributes)
}

/// Gives what `file` is open on the access list `acl`, inheriting nothing.
/// The handle must be open with `WRITE_DAC`.
fn set_list(file: &File, acl: *mut ACL) -> io::Result<()> {
    // SAFETY: the handle is open with WRITE_DAC, and the list is valid for
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

/// The error for something another user owns: its owner may always change
/// its list again.
fn owned_by_another() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "another user owns it, so it cannot be closed to others",
    )
}

/// Closes `path` to everyone but the user this process runs as: its access
/// list becomes that one user, with full control, and inherits nothing. A
/// folder passes the entry on to what is made in it, and Windows gives it
/// to what is already in it that inherits its list. It fails if another
/// user owns it.
pub fn restrict(path: &Path) -> io::Result<()> {
    let file = open(path, true)?;
    let folder = file.metadata()?.is_dir();
    let user = User::current()?;
    let descriptor = Descriptor::of(&file, OWNER_SECURITY_INFORMATION)?;
    let Some(owner) = descriptor.owner()? else {
        return Err(owned_by_another());
    };
    if !trusted_owner(owner, &user) {
        return Err(owned_by_another());
    }
    let mut list = OwnerOnly::new(folder)?;
    set_list(&file, list.acl())
}

/// Whether the volume holding `file` keeps access lists.
fn supports_acls(file: &File) -> io::Result<bool> {
    let mut flags = 0u32;
    // SAFETY: the handle is valid, and every optional pointer is null
    // except `flags`, which is written by the call.
    let got = unsafe {
        GetVolumeInformationByHandleW(
            file.as_raw_handle() as HANDLE,
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut flags,
            std::ptr::null_mut(),
            0,
        )
    };
    if got == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(flags & FILE_PERSISTENT_ACLS != 0)
}

/// Fails with [`not_kept`] before anything is created if the parent folder
/// of `path` sits on a volume that keeps no access lists.
fn check_parent_supports_acls(path: &Path) -> io::Result<()> {
    let abs = std::path::absolute(path)?;
    let Some(parent) = abs.parent() else {
        return Ok(());
    };
    let folder = OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(parent)?;
    if !supports_acls(&folder)? {
        return Err(not_kept());
    }
    Ok(())
}

/// Checks without changing anything that `path` is a folder on a volume
/// that keeps access lists, owned by a trusted owner, and whose access list
/// no untrusted user may change.
pub fn check_folder_usable(path: &Path) -> io::Result<()> {
    let folder = open(path, false)?;
    if !folder.metadata()?.is_dir() {
        return Err(io::Error::from(io::ErrorKind::NotADirectory));
    }
    if !supports_acls(&folder)? {
        return Err(not_kept());
    }
    let user = User::current()?;
    let descriptor = Descriptor::of(
        &folder,
        OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
    )?;
    let Some(owner) = descriptor.owner()? else {
        return Err(owned_by_another());
    };
    if !trusted_owner(owner, &user) {
        return Err(owned_by_another());
    }
    let Some(acl) = descriptor.dacl()? else {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "other users may change who can open it, so it cannot be closed to them",
        ));
    };
    if lets_others_change(acl, &user)? {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "other users may change who can open it, so it cannot be closed to them",
        ));
    }
    Ok(())
}

/// Opens or creates the lock file at `path` without following a reparse
/// point, refusing links and non-regular files before anything is locked
/// or written.
pub fn open_lock_file(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    let meta = file.metadata()?;
    if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 || !meta.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the lock file must be a regular file, not a link",
        ));
    }
    Ok(file)
}

/// Makes the folder at `path` one only this user can open, with what is
/// made in it from now on, unless it is so already: owned by a trusted
/// owner, with a list that inherits nothing, lets nobody else in, and
/// passes this user's entry on to new files.
///
/// The files directly in it keep the access they had, where Windows would
/// give them the new list, so that a check of one of them still sees
/// whether others could read it; what the folders in it hold takes the new
/// list. It fails on a folder another user owns, or one whose list others
/// may change, since a handle they opened for that outlasts the new list.
pub fn close_folder(path: &Path) -> io::Result<()> {
    close_folder_preserving(path, &[])
}

/// Like [`close_folder`], and also ensures that any existing file in
/// `keep_open` that others could read beforehand (even through a link into
/// a subfolder) stays visibly open after the folder's list changes, failing
/// closed if that evidence would be lost.
pub fn close_folder_preserving(path: &Path, keep_open: &[&Path]) -> io::Result<()> {
    let folder = open(path, true)?;
    if !folder.metadata()?.is_dir() {
        return Err(io::Error::from(io::ErrorKind::NotADirectory));
    }
    if !supports_acls(&folder)? {
        return Err(not_kept());
    }
    let user = User::current()?;
    let descriptor = Descriptor::of(
        &folder,
        OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
    )?;
    let Some(owner) = descriptor.owner()? else {
        return Err(owned_by_another());
    };
    if !trusted_owner(owner, &user) {
        return Err(owned_by_another());
    }
    let Some(acl) = descriptor.dacl()? else {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "other users may change who can open it, so it cannot be closed to them",
        ));
    };
    if lets_others_change(acl, &user)? {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "other users may change who can open it, so it cannot be closed to them",
        ));
    }
    if descriptor.protected()?
        && !lets_others(acl, &user, !HARMLESS, true)?
        && passes_on(acl, &user)?
    {
        return Ok(());
    }
    let mut was_open = Vec::new();
    for sensitive in keep_open {
        match open(sensitive, false) {
            Ok(file) => {
                if !is_private(&file)? {
                    was_open.push(*sensitive);
                    let _ = freeze(sensitive);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            // A file this user cannot change, Windows cannot change either
            // as it passes the new list on.
            let _ = freeze(&entry.path());
        }
    }
    let mut list = OwnerOnly::new(true)?;
    set_list(&folder, list.acl())?;
    let closed = Descriptor::of(
        &folder,
        OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
    )?;
    let passes = closed
        .dacl()?
        .map(|acl| passes_on(acl, &user))
        .transpose()?
        .unwrap_or(false);
    if !(judge(&closed, true, &user)? && closed.protected()? && passes) {
        return Err(io::Error::other("it is still open to others once closed"));
    }
    for sensitive in was_open {
        if path_is_private(sensitive)? {
            return Err(io::Error::other(
                "closing the folder hid that a sensitive file was open to others",
            ));
        }
    }
    Ok(())
}

/// Stops the file at `path` inheriting its list, keeping the entries it
/// has now as its own.
fn freeze(path: &Path) -> io::Result<()> {
    let file = open(path, true)?;
    let descriptor = Descriptor::of(&file, DACL_SECURITY_INFORMATION)?;
    let Some(acl) = descriptor.dacl()? else {
        return Ok(());
    };
    if descriptor.protected()? {
        return Ok(());
    }
    // SAFETY: `acl` is valid while `descriptor` lives.
    let (size, count) = unsafe { (usize::from((*acl.0).AclSize), (*acl.0).AceCount) };
    let mut copy = vec![0u64; size.div_ceil(8)];
    // SAFETY: the list is `size` bytes long, and the copy holds as many.
    unsafe { std::ptr::copy_nonoverlapping(acl.0.cast::<u8>(), copy.as_mut_ptr().cast(), size) };
    let copied = copy.as_mut_ptr().cast::<ACL>();
    for index in 0..u32::from(count) {
        let mut entry: *mut std::ffi::c_void = std::ptr::null_mut();
        // SAFETY: the index is below the copy's count.
        if unsafe { GetAce(copied, index, &mut entry) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: every entry starts with its header, in the copy this
        // function owns.
        unsafe { (*entry.cast::<ACE_HEADER>()).AceFlags &= !(INHERITED_ACE as u8) };
    }
    set_list(&file, copied)
}

/// Creates a new file at `path`, open for writing, that only this user can
/// open from the moment it exists. It fails if something is there, a link
/// included, and where the file system keeps no access list, removing what
/// it made.
pub fn create_restricted(path: &Path) -> io::Result<File> {
    check_parent_supports_acls(path)?;
    let wide = crate::wide_absolute(path)?;
    let file = owner_only(false, |attributes| {
        // SAFETY: `wide` ends in a NUL, and `attributes` is valid for the
        // call.
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                FILE_GENERIC_WRITE | READ_CONTROL | FILE_READ_ATTRIBUTES,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                attributes,
                CREATE_NEW,
                // A link at `path` is something there, as the standard
                // library takes it, and is not followed.
                FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the call succeeded, so the handle is this code's to own.
        Ok(unsafe { File::from_raw_handle(handle) })
    })?;
    let descriptor = Descriptor::of(
        &file,
        OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
    );
    match descriptor.and_then(|descriptor| judge(&descriptor, false, &User::current()?)) {
        Ok(true) => Ok(file),
        checked => {
            drop(file);
            let _ = std::fs::remove_file(path);
            Err(checked.err().unwrap_or_else(not_kept))
        }
    }
}

/// Creates the one folder at `path` that only this user can open from the
/// moment it exists, and passes that on to what is made in it. It fails if
/// something is there, if its parent is not, and where the file system
/// keeps no access list, removing what it made.
pub fn create_restricted_dir(path: &Path) -> io::Result<()> {
    create_pinned_restricted_dir(path).map(|_| ())
}

/// Like [`create_restricted_dir`], and returns an open handle to the new
/// folder without `FILE_SHARE_DELETE` so another user cannot delete or
/// swap the folder at `path` while the handle is held.
pub fn create_pinned_restricted_dir(path: &Path) -> io::Result<File> {
    check_parent_supports_acls(path)?;
    let wide = crate::wide_absolute(path)?;
    owner_only(true, |attributes| {
        // SAFETY: `wide` ends in a NUL, and `attributes` is valid for the
        // call.
        if unsafe { CreateDirectoryW(wide.as_ptr(), attributes) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    })?;
    let checked = OpenOptions::new()
        // FILE_LIST_DIRECTORY makes NTFS enforce the share mode below;
        // metadata-only handles (READ_CONTROL | FILE_READ_ATTRIBUTES alone)
        // bypass sharing checks and would not block rename or delete.
        .access_mode(FILE_LIST_DIRECTORY | READ_CONTROL | FILE_READ_ATTRIBUTES)
        // Without FILE_SHARE_DELETE, the folder cannot be removed or renamed
        // while this handle lives, even if its parent grants delete-child.
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .and_then(|folder| {
            let meta = folder.metadata()?;
            if !meta.is_dir() || meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                return Ok((folder, false));
            }
            let user = User::current()?;
            let descriptor = Descriptor::of(
                &folder,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            )?;
            let passes = descriptor
                .dacl()?
                .map(|acl| passes_on(acl, &user))
                .transpose()?
                .unwrap_or(false);
            let ok = judge(&descriptor, true, &user)? && descriptor.protected()? && passes;
            Ok((folder, ok))
        });
    match checked {
        Ok((folder, true)) => Ok(folder),
        Ok((folder, false)) => {
            drop(folder);
            let _ = std::fs::remove_dir(path);
            Err(not_kept())
        }
        Err(error) => {
            let _ = std::fs::remove_dir(path);
            Err(error)
        }
    }
}

/// The error for a file system that did not keep the list asked for, as
/// FAT and exFAT keep none.
fn not_kept() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "the file system here does not keep who may open a file",
    )
}

/// Something the system allocated with `LocalAlloc`, freed when dropped.
struct Local(*mut std::ffi::c_void);

impl Drop for Local {
    fn drop(&mut self) {
        // SAFETY: the system allocated it with LocalAlloc, and it is freed
        // once.
        unsafe { LocalFree(self.0 as HLOCAL) };
    }
}

/// A security descriptor the system allocated, freed when dropped.
struct Descriptor(PSECURITY_DESCRIPTOR);

impl Descriptor {
    /// The parts of the descriptor of what `file` is open on that `what`
    /// names.
    fn of(file: &File, what: OBJECT_SECURITY_INFORMATION) -> io::Result<Self> {
        let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: the handle is the file's own, and `descriptor` is written
        // by the call; the parts not asked for here are read from it.
        let got = unsafe {
            GetSecurityInfo(
                file.as_raw_handle() as HANDLE,
                SE_FILE_OBJECT,
                what,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut descriptor,
            )
        };
        if got != 0 {
            return Err(io::Error::from_raw_os_error(got as i32));
        }
        Ok(Self(descriptor))
    }

    /// Its owner, or `None` if it has none. It points into the descriptor.
    fn owner(&self) -> io::Result<Option<Sid<'_>>> {
        let mut owner: PSID = std::ptr::null_mut();
        let mut defaulted = 0;
        // SAFETY: the descriptor is valid while `self` lives, and the rest
        // are written by the call.
        if unsafe { GetSecurityDescriptorOwner(self.0, &mut owner, &mut defaulted) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok((!owner.is_null()).then_some(Sid(owner, PhantomData)))
    }

    /// Its access list, or `None` if it has none. It points into the
    /// descriptor.
    fn dacl(&self) -> io::Result<Option<Acl<'_>>> {
        let (mut present, mut acl, mut defaulted) = (0, std::ptr::null_mut(), 0);
        // SAFETY: the descriptor is valid while `self` lives, and the rest
        // are written by the call.
        if unsafe { GetSecurityDescriptorDacl(self.0, &mut present, &mut acl, &mut defaulted) } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok((present != 0 && !acl.is_null()).then_some(Acl(acl, PhantomData)))
    }

    /// Whether its access list inherits nothing.
    fn protected(&self) -> io::Result<bool> {
        let (mut control, mut revision) = (0, 0);
        // SAFETY: the descriptor is valid while `self` lives, and the rest
        // are written by the call.
        if unsafe { GetSecurityDescriptorControl(self.0, &mut control, &mut revision) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(control & SE_DACL_PROTECTED != 0)
    }
}

impl Drop for Descriptor {
    fn drop(&mut self) {
        // SAFETY: the system allocated it with LocalAlloc, and it is freed
        // once.
        unsafe { LocalFree(self.0 as HLOCAL) };
    }
}

/// Whether `file` is owned by the user this process runs as (or by the
/// system, or the administrators), and nobody else may do more with it
/// than see that it is there. On a folder, what it passes on to what is
/// made in it counts too.
pub fn is_private(file: &File) -> io::Result<bool> {
    let folder = file.metadata()?.is_dir();
    let descriptor = Descriptor::of(file, OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION)?;
    judge(&descriptor, folder, &User::current()?)
}

/// [`is_private`] on a descriptor.
fn judge(descriptor: &Descriptor, folder: bool, user: &User) -> io::Result<bool> {
    // The owner may always read and change the list, so one who is not
    // trusted can let themselves in whatever it says. No list at all lets
    // everyone in.
    let (Some(owner), Some(acl)) = (descriptor.owner()?, descriptor.dacl()?) else {
        return Ok(false);
    };
    if !trusted_owner(owner, user) {
        return Ok(false);
    }
    Ok(!lets_others(acl, user, !HARMLESS, folder)?)
}

/// Each plain entry of `acl` that lets someone in: its flags, its access
/// and its SID, or `None` for an entry of a kind this does not weigh, which
/// is taken to let anyone do anything. Entries of the kinds that let
/// nobody in are left out.
fn entries<'a>(acl: Acl<'a>) -> io::Result<Vec<Option<(u32, u32, Sid<'a>)>>> {
    // SAFETY: `acl` is borrowed from a live `Descriptor`.
    let count = unsafe { (*acl.0).AceCount };
    let mut found = Vec::new();
    for index in 0..u32::from(count) {
        let mut entry: *mut std::ffi::c_void = std::ptr::null_mut();
        // SAFETY: the index is below the list's count.
        if unsafe { GetAce(acl.0, index, &mut entry) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: every entry starts with its header.
        let header = unsafe { *entry.cast::<ACE_HEADER>() };
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
                found.push(Some((
                    u32::from(header.AceFlags),
                    mask,
                    Sid(sid, PhantomData),
                )));
            }
            ACCESS_ALLOWED_COMPOUND_ACE_TYPE
            | ACCESS_ALLOWED_OBJECT_ACE_TYPE
            | ACCESS_ALLOWED_CALLBACK_ACE_TYPE
            | ACCESS_ALLOWED_CALLBACK_OBJECT_ACE_TYPE => found.push(None),
            // Entries that deny, audit, raise an alarm, label, or carry
            // an attribute, a policy or a filter.
            1..=3 | 6..=8 | 10 | 12..=21 => {}
            // A kind newer than these is taken to let anyone in.
            _ => found.push(None),
        }
    }
    Ok(found)
}

/// Whether an entry of `acl` lets anyone untrusted do any of `mask` with
/// what the list is on, or, with `passed_on`, with what is made in it: an
/// entry only passed on applies to nothing but what a folder holds.
fn lets_others(acl: Acl<'_>, user: &User, mask: u32, passed_on: bool) -> io::Result<bool> {
    Ok(entries(acl)?.into_iter().any(|entry| match entry {
        Some((flags, allowed, sid)) => {
            (passed_on || flags & INHERIT_ONLY_ACE == 0)
                && allowed & mask != 0
                && !trusted(sid, user)
        }
        None => true,
    }))
}

/// Whether an entry of `acl` lets another user change who may open a
/// folder. An inherited entry for `ALL APPLICATION PACKAGES` (which
/// Windows puts on `%TEMP%` by default) is allowed to be stripped when
/// closing the folder, though [`judge`] never treats it as private.
fn lets_others_change(acl: Acl<'_>, user: &User) -> io::Result<bool> {
    Ok(entries(acl)?.into_iter().any(|entry| match entry {
        Some((flags, allowed, sid)) => {
            flags & INHERIT_ONLY_ACE == 0
                && allowed & CHANGE != 0
                && !trusted(sid, user)
                && !(flags & INHERITED_ACE != 0 && is_all_app_packages(sid))
        }
        None => true,
    }))
}

/// Whether `sid` is `ALL APPLICATION PACKAGES` (`S-1-15-2-1`).
fn is_all_app_packages(sid: Sid<'_>) -> bool {
    // SAFETY: the SID is valid for the call.
    unsafe { IsWellKnownSid(sid.0, WinBuiltinAnyPackageSid) != 0 }
}

/// Whether `acl` passes this user's entry on to the files made in it, so
/// that they never fall back on the process's default list.
fn passes_on(acl: Acl<'_>, user: &User) -> io::Result<bool> {
    Ok(entries(acl)?.into_iter().flatten().any(|(flags, _, sid)| {
        // SAFETY: both SIDs are valid for the call.
        flags & OBJECT_INHERIT_ACE != 0 && unsafe { EqualSid(sid.0, user.sid().0) } != 0
    }))
}

/// Whether `sid` may own a private file: this user, or those who can take
/// any file anyway.
fn trusted_owner(sid: Sid<'_>, user: &User) -> bool {
    // SAFETY: both SIDs are valid for the call.
    unsafe {
        EqualSid(sid.0, user.sid().0) != 0
            || IsWellKnownSid(sid.0, WinLocalSystemSid) != 0
            || IsWellKnownSid(sid.0, WinBuiltinAdministratorsSid) != 0
    }
}

/// Whether an entry for `sid` lets in nobody but a trusted owner. The owner
/// rights stand for the owner, checked before, and the creator owner, in an
/// entry a folder passes on, for whoever makes something in it, who must
/// already be let in to make it.
fn trusted(sid: Sid<'_>, user: &User) -> bool {
    trusted_owner(sid, user)
        // SAFETY: the SID is valid for the call.
        || unsafe {
            IsWellKnownSid(sid.0, WinCreatorOwnerRightsSid) != 0
                || IsWellKnownSid(sid.0, WinCreatorOwnerSid) != 0
        }
}

/// Whether what is at `path` is private, as [`is_private`] says.
pub fn path_is_private(path: &Path) -> io::Result<bool> {
    is_private(&open(path, false)?)
}

/// Whether Windows AuthZ grants a synthetic signed-in user (an invented
/// SID in `Everyone`, `Authenticated Users`, `Users`, and `Interactive`)
/// more than [`HARMLESS`] access to the security descriptor at `path`.
///
/// This cross-checks [`is_private`] against Windows' own access evaluator
/// for fixtures granting those groups or owned by that synthetic SID. It
/// evaluates one synthetic identity on the object itself, so it does not
/// detect grants to an unrelated named user SID, explicit deny rules that
/// only block this identity, or inherit-only entries on a folder.
pub fn another_user_may_open(path: &Path) -> io::Result<bool> {
    let descriptor = Descriptor::of(
        &open(path, false)?,
        OWNER_SECURITY_INFORMATION | GROUP_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
    )?;
    Ok(granted_to_another(&descriptor)? & !HARMLESS != 0)
}

/// A SID buffer, aligned for one.
type SidBuffer = [u32; SECURITY_MAX_SID_SIZE.div_ceil(4) as usize];

/// The well-known SID `kind`.
fn well_known(kind: WELL_KNOWN_SID_TYPE) -> io::Result<SidBuffer> {
    let mut sid = SidBuffer::default();
    let mut len = SECURITY_MAX_SID_SIZE;
    // SAFETY: the buffer holds `len` bytes, and a well-known SID of the
    // machine needs no domain.
    let made = unsafe {
        CreateWellKnownSid(
            kind,
            std::ptr::null_mut(),
            sid.as_mut_ptr().cast(),
            &mut len,
        )
    };
    if made == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(sid)
}

/// The access the system's access check grants the invented user of
/// [`another_user_may_open`] by `descriptor`.
fn granted_to_another(descriptor: &Descriptor) -> io::Result<u32> {
    /// A handle of the access check, closed by `free` when dropped.
    struct Authz(
        *mut std::ffi::c_void,
        unsafe extern "system" fn(*mut std::ffi::c_void) -> i32,
    );
    impl Drop for Authz {
        fn drop(&mut self) {
            // SAFETY: the handle was made by the call `self.1` frees it for.
            unsafe { (self.1)(self.0) };
        }
    }
    let mut manager = std::ptr::null_mut();
    // SAFETY: no callbacks and no name; `manager` is written by the call.
    let made = unsafe {
        AuthzInitializeResourceManager(
            AUTHZ_RM_FLAG_NO_AUDIT,
            None,
            None,
            None,
            std::ptr::null(),
            &mut manager,
        )
    };
    if made == 0 {
        return Err(io::Error::last_os_error());
    }
    let manager = Authz(manager, AuthzFreeResourceManager);

    // A SID in the form of a local account's, given to no one: the
    // machine's own part of such SIDs is never this short.
    let text: Vec<u16> = "S-1-5-21-1-2-3-1001\0".encode_utf16().collect();
    let mut invented: PSID = std::ptr::null_mut();
    // SAFETY: `text` ends in a NUL; the SID is allocated by the call.
    if unsafe { ConvertStringSidToSidW(text.as_ptr(), &mut invented) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let invented = Local(invented);

    let mut alone = std::ptr::null_mut();
    // SAFETY: the SID and the manager are valid; with no groups looked up,
    // the SID need belong to no one.
    let made = unsafe {
        AuthzInitializeContextFromSid(
            AUTHZ_SKIP_TOKEN_GROUPS,
            invented.0,
            manager.0,
            std::ptr::null(),
            LUID {
                LowPart: 0,
                HighPart: 0,
            },
            std::ptr::null(),
            &mut alone,
        )
    };
    if made == 0 {
        return Err(io::Error::last_os_error());
    }
    let alone = Authz(alone, AuthzFreeContext);

    let mut groups = [
        WinWorldSid,
        WinAuthenticatedUserSid,
        WinBuiltinUsersSid,
        WinInteractiveSid,
    ]
    .into_iter()
    .map(well_known)
    .collect::<io::Result<Vec<_>>>()?;
    let entries: Vec<_> = groups
        .iter_mut()
        .map(|sid| SID_AND_ATTRIBUTES {
            Sid: sid.as_mut_ptr().cast(),
            Attributes: SE_GROUP_ENABLED,
        })
        .collect();
    let mut context = std::ptr::null_mut();
    // SAFETY: the entries point into `groups`, which outlives the call.
    let added = unsafe {
        AuthzAddSidsToContext(
            alone.0,
            entries.as_ptr(),
            entries.len() as u32,
            std::ptr::null(),
            0,
            &mut context,
        )
    };
    if added == 0 {
        return Err(io::Error::last_os_error());
    }
    let context = Authz(context, AuthzFreeContext);

    let request = AUTHZ_ACCESS_REQUEST {
        DesiredAccess: MAXIMUM_ALLOWED,
        PrincipalSelfSid: std::ptr::null_mut(),
        ObjectTypeList: std::ptr::null_mut(),
        ObjectTypeListLength: 0,
        OptionalArguments: std::ptr::null_mut(),
    };
    let (mut granted, mut sacl, mut error) = (0u32, 0u32, 0u32);
    let mut reply = AUTHZ_ACCESS_REPLY {
        ResultListLength: 1,
        GrantedAccessMask: &mut granted,
        SaclEvaluationResults: &mut sacl,
        Error: &mut error,
    };
    // SAFETY: the context and the descriptor are valid, and the reply has
    // room for the one result asked for.
    let checked = unsafe {
        AuthzAccessCheck(
            0,
            context.0,
            &request,
            std::ptr::null_mut(),
            descriptor.0,
            std::ptr::null(),
            0,
            &mut reply,
            std::ptr::null_mut(),
        )
    };
    if checked == 0 {
        return Err(io::Error::last_os_error());
    }
    // Denied is an answer; any other error is not one.
    if error != 0 && error != ERROR_ACCESS_DENIED {
        return Err(io::Error::from_raw_os_error(error as i32));
    }
    Ok(granted)
}

#[cfg(test)]
mod tests;
