//! The access lists these tests make are checked by [`is_private`] and by
//! Windows AuthZ for one synthetic signed-in user (`another_user_may_open`,
//! belonging to `Everyone`, `Authenticated Users`, `Users`, and
//! `Interactive`), since a test cannot create a real second local account
//! without an administrator. That cross-checks descriptors granting those
//! groups (or owned by that synthetic SID) on the object itself, not grants
//! to unrelated user SIDs or inherit-only entries on a folder (which the
//! tests check on a file made in it).

use std::io::Write;
use std::process::Command;

use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    SetKernelObjectSecurity, UNPROTECTED_DACL_SECURITY_INFORMATION,
};
use windows_sys::core::PWSTR;

use super::*;

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = tempfile::Builder::new()
        .prefix(&format!("ritornel-windows-private-{name}-"))
        .tempdir()
        .unwrap()
        .keep();
    // Break inheritance from `%TEMP%` so tests do not inherit Windows 11's
    // `ALL APPLICATION PACKAGES` entry unless they explicitly grant access.
    restrict(&dir).unwrap();
    dir
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
    assert!(another_user_may_open(path).unwrap(), "{}", path.display());
}

/// Whether `path` is private, asserting that Windows AuthZ for the
/// synthetic signed-in user never lets them open what [`path_is_private`]
/// calls private.
fn private(path: &Path) -> bool {
    let private = path_is_private(path).unwrap();
    let open = another_user_may_open(path).unwrap();
    assert!(!(private && open), "{}", path.display());
    private
}

/// A descriptor written in SDDL, in which `ME` stands for this user.
fn descriptor(sddl: &str) -> Descriptor {
    let user = User::current().unwrap();
    let mut text: PWSTR = std::ptr::null_mut();
    // SAFETY: the SID is valid; the string is allocated by the call.
    let converted = unsafe { ConvertSidToStringSidW(user.sid().0, &mut text) };
    assert_ne!(converted, 0);
    let _text = Local(text.cast());
    // SAFETY: the call wrote a string ending in a NUL.
    let len = unsafe { (0..).take_while(|&i| *text.add(i) != 0).count() };
    // SAFETY: those `len` units are the string.
    let me = String::from_utf16(unsafe { std::slice::from_raw_parts(text, len) }).unwrap();
    let sddl: Vec<u16> = sddl.replace("ME", &me).encode_utf16().chain([0]).collect();
    let mut made: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: `sddl` ends in a NUL; the descriptor is allocated by the call.
    let converted = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut made,
            std::ptr::null_mut(),
        )
    };
    assert_ne!(converted, 0, "{}", io::Error::last_os_error());
    Descriptor(made)
}

fn judged(sddl: &str, folder: bool) -> bool {
    judge(&descriptor(sddl), folder, &User::current().unwrap()).unwrap()
}

#[test]
fn a_restricted_file_is_private_and_one_open_to_everyone_is_not() {
    let dir = scratch("file");
    let path = dir.join("secret");
    std::fs::write(&path, "x").unwrap();
    open_to_everyone(&path);
    assert!(!private(&path));
    restrict(&path).unwrap();
    assert!(private(&path));
    // It can still be read and written by its owner.
    std::fs::write(&path, "y").unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "y");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn restricting_a_folder_closes_what_is_made_in_it_and_what_it_held() {
    let dir = scratch("folder");
    open_to_everyone(&dir);
    let inner = dir.join("data");
    std::fs::create_dir(&inner).unwrap();
    std::fs::write(inner.join("made-before"), "x").unwrap();
    assert!(!private(&inner));
    assert!(!private(&inner.join("made-before")));
    restrict(&inner).unwrap();
    assert!(private(&inner));
    // What it held took its list from it, and takes the new one.
    assert!(private(&inner.join("made-before")));
    std::fs::write(inner.join("made-after"), "x").unwrap();
    std::fs::create_dir(inner.join("logs")).unwrap();
    assert!(private(&inner.join("made-after")));
    assert!(private(&inner.join("logs")));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_file_created_restricted_is_private_before_it_holds_anything() {
    let dir = scratch("create");
    open_to_everyone(&dir);
    // Beside it, a file made plainly takes the folder's list.
    std::fs::write(dir.join("plain"), "x").unwrap();
    assert!(!private(&dir.join("plain")));
    let path = dir.join("token");
    let mut file = create_restricted(&path).unwrap();
    assert!(private(&path));
    assert!(is_private(&file).unwrap());
    writeln!(file, "token").unwrap();
    drop(file);
    assert_eq!(
        create_restricted(&path).unwrap_err().kind(),
        io::ErrorKind::AlreadyExists
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "token\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_folder_created_restricted_closes_what_is_made_in_it_from_the_start() {
    let dir = scratch("create-dir");
    open_to_everyone(&dir);
    let staging = dir.join(".staging");
    create_restricted_dir(&staging).unwrap();
    assert!(private(&staging));
    // As SQLite makes a log beside a database, with no list of its own.
    std::fs::write(staging.join("server.db-wal"), "x").unwrap();
    std::fs::create_dir(staging.join("inner")).unwrap();
    assert!(private(&staging.join("server.db-wal")));
    assert!(private(&staging.join("inner")));
    assert_eq!(
        create_restricted_dir(&staging).unwrap_err().kind(),
        io::ErrorKind::AlreadyExists
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_owner_who_is_someone_else_makes_a_file_open() {
    // The list names this user alone, but the owner may always read and
    // change it: here the invented user owns the file and is let in.
    let theirs = "O:S-1-5-21-1-2-3-1001G:SYD:P(A;;FA;;;ME)";
    assert!(!judged(theirs, false));
    let granted = granted_to_another(&descriptor(theirs)).unwrap();
    assert_ne!(granted & WRITE_DAC, 0, "{granted:#x}");
    // The owner rights stand for whoever owns it, so they let in nobody
    // untrusted only when the owner is trusted.
    assert!(!judged("O:BGD:P(A;;FA;;;ME)(A;;FA;;;OW)", false));
    assert!(judged("O:MED:P(A;;FA;;;ME)(A;;FA;;;OW)", false));
    assert!(judged("O:SYD:P(A;;FA;;;ME)", false));
    assert!(judged("O:BAD:P(A;;FA;;;ME)", false));
    let mine = "O:MEG:SYD:P(A;;FA;;;ME)";
    assert!(judged(mine, false));
    assert_eq!(
        granted_to_another(&descriptor(mine)).unwrap() & !HARMLESS,
        0
    );
}

#[test]
fn a_folder_is_judged_by_what_it_passes_on_and_a_file_is_not() {
    // Everyone may read what is made in it, though not the folder itself.
    let passes_on = "O:MED:P(A;OICI;FA;;;ME)(A;OICIIO;FR;;;WD)";
    assert!(!judged(passes_on, true));
    assert!(judged(passes_on, false));
    // The creator owner of what is made in it is whoever makes it, and
    // only this user may.
    assert!(judged("O:MED:P(A;OICI;FA;;;ME)(A;OICIIO;FA;;;CO)", true));
    // Seeing that what is made in it is there is harmless.
    assert!(judged(
        "O:MED:P(A;OICI;FA;;;ME)(A;OICIIO;0x100080;;;WD)",
        true
    ));
}

#[test]
fn no_list_at_all_lets_everyone_in() {
    assert!(!judged("O:ME", false));
    assert!(!judged("O:MED:NO_ACCESS_CONTROL", false));
}

#[test]
fn callback_and_object_access_entries_fail_closed() {
    assert!(!judged(
        "O:MED:P(A;;FA;;;ME)(XA;;FA;;;WD;(Member_of {SID(BA)}))",
        false
    ));
    assert!(!judged(
        "O:MED:P(A;;FA;;;ME)(OA;;FA;1131f6aa-9c07-11d1-f79f-00c04fc2dcd2;;WD)",
        false
    ));
}

#[test]
fn closing_a_folder_closes_existing_and_new_files_except_keep_open() {
    let dir = scratch("close");
    open_to_everyone(&dir);
    let inner = dir.join("data");
    std::fs::create_dir(&inner).unwrap();
    std::fs::create_dir(inner.join("logs")).unwrap();
    let credential = inner.join("credential");
    let config = inner.join("config.toml");
    let old_log = inner.join("logs").join("old.log");
    std::fs::write(&credential, "x").unwrap();
    std::fs::write(&config, "x").unwrap();
    std::fs::write(&old_log, "x").unwrap();
    assert!(!private(&credential));
    assert!(!private(&config));
    let hidden = close_folder_preserving(&inner, &[credential.as_path()]).unwrap();
    assert!(hidden.is_empty());
    assert!(private(&inner));
    // Only the named keep_open file keeps its prior open access.
    assert!(!private(&credential));
    // Other existing files and subfolders that inherit their list take the new one.
    assert!(private(&config));
    assert!(private(&inner.join("logs")));
    assert!(private(&old_log));
    std::fs::write(inner.join("made-after"), "x").unwrap();
    assert!(private(&inner.join("made-after")));
    // Closed, it is left as it is.
    close_folder(&inner).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_failed_freeze_leaves_the_folder_unclosed() {
    let dir = scratch("freeze-fail");
    open_to_everyone(&dir);
    let inner = dir.join("data");
    std::fs::create_dir(&inner).unwrap();
    let credential = inner.join("credential");
    std::fs::write(&credential, "x").unwrap();
    // Leave inheritance on (`AI`), deny WRITE_DAC (`0x00040000`) to `ME`
    // before the inherited full-control entry from `inner`, and replace
    // implicit owner WRITE_DAC with OWNER RIGHTS allowing read and delete
    // (for cleanup) but not WRITE_DAC.
    let unfreezable = descriptor("O:MED:AI(D;;0x00040000;;;ME)(A;;0x00130089;;;OW)");
    let acl = unfreezable.dacl().unwrap().unwrap();
    let file = open(&credential, true).unwrap();
    // SAFETY: `file` is open with WRITE_DAC and `acl` lives as long as `unfreezable`.
    let set = unsafe {
        SetSecurityInfo(
            file.as_raw_handle() as HANDLE,
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | UNPROTECTED_DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            acl.0.cast_mut(),
            std::ptr::null(),
        )
    };
    assert_eq!(set, 0);
    drop(file);
    let before = Descriptor::of(
        &open(&credential, false).unwrap(),
        DACL_SECURITY_INFORMATION,
    )
    .unwrap();
    assert!(!before.protected().unwrap());
    assert!(!private(&credential));

    let refused = close_folder_preserving(&inner, &[credential.as_path()]).unwrap_err();
    assert_eq!(
        refused.source.kind(),
        io::ErrorKind::PermissionDenied,
        "{refused}"
    );
    assert_eq!(refused.path, credential);
    // The folder was not closed when freezing the open sensitive file failed.
    assert!(!private(&inner));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_protected_open_file_without_write_dac_is_left_alone() {
    let dir = scratch("freeze-protected");
    open_to_everyone(&dir);
    let inner = dir.join("data");
    std::fs::create_dir(&inner).unwrap();
    let credential = inner.join("credential");
    std::fs::write(&credential, "x").unwrap();
    // Already protected (`P`) and open to Everyone, without owner WRITE_DAC:
    // freeze checks READ_CONTROL first and never asks for WRITE_DAC.
    let locked = descriptor("O:MED:P(A;;FR;;;WD)(A;;0x00130089;;;OW)");
    let acl = locked.dacl().unwrap().unwrap();
    set_list(&open(&credential, true).unwrap(), acl.0.cast_mut()).unwrap();

    let hidden = close_folder_preserving(&inner, &[credential.as_path()]).unwrap();
    assert!(hidden.is_empty());
    assert!(private(&inner));
    assert!(!private(&credential));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_null_dacl_file_stays_open_when_its_folder_is_closed() {
    let dir = scratch("freeze-null-dacl");
    open_to_everyone(&dir);
    let inner = dir.join("data");
    std::fs::create_dir(&inner).unwrap();
    let credential = inner.join("credential");
    std::fs::write(&credential, "x").unwrap();
    let file = open(&credential, true).unwrap();
    // Write a raw security descriptor with a NULL DACL and SE_DACL_PROTECTED
    // clear via SetKernelObjectSecurity (SetSecurityInfo auto-protects a
    // NULL DACL).
    // SAFETY: plain data, initialized below before use.
    let mut sd: SECURITY_DESCRIPTOR = unsafe { std::mem::zeroed() };
    let ptr: PSECURITY_DESCRIPTOR = (&raw mut sd).cast();
    // SAFETY: `ptr` points to `sd`, owned by this function.
    let initialized = unsafe { InitializeSecurityDescriptor(ptr, SECURITY_DESCRIPTOR_REVISION) };
    assert_ne!(initialized, 0);
    // SAFETY: `ptr` is an initialized security descriptor; NULL sets a NULL DACL.
    let dacl_set = unsafe { SetSecurityDescriptorDacl(ptr, 1, std::ptr::null(), 0) };
    assert_ne!(dacl_set, 0);
    // SAFETY: `file` is open with WRITE_DAC and `ptr` is a valid descriptor.
    let applied = unsafe {
        SetKernelObjectSecurity(
            file.as_raw_handle() as HANDLE,
            DACL_SECURITY_INFORMATION,
            ptr,
        )
    };
    assert_ne!(applied, 0);
    drop(file);
    let before = Descriptor::of(
        &open(&credential, false).unwrap(),
        DACL_SECURITY_INFORMATION,
    )
    .unwrap();
    assert!(before.dacl().unwrap().is_none());
    assert!(!before.protected().unwrap());
    assert!(!private(&credential));

    let hidden = close_folder_preserving(&inner, &[credential.as_path()]).unwrap();
    assert!(hidden.is_empty());
    assert!(private(&inner));
    assert!(!private(&credential));
    let after = Descriptor::of(
        &open(&credential, false).unwrap(),
        DACL_SECURITY_INFORMATION,
    )
    .unwrap();
    assert!(after.protected().unwrap());
    assert!(after.dacl().unwrap().is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_folder_others_may_change_is_not_closed() {
    let dir = scratch("change");
    let done = Command::new("icacls")
        .arg(&dir)
        .args(["/grant", "*S-1-1-0:(OI)(CI)(F)"])
        .output()
        .unwrap();
    assert!(done.status.success(), "{done:?}");
    let refused = close_folder(&dir).unwrap_err();
    assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied, "{refused}");
    assert!(!private(&dir));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_folder_that_passes_nothing_on_is_closed_again() {
    let dir = scratch("passes-nothing");
    let inner = dir.join("data");
    std::fs::create_dir(&inner).unwrap();
    // This user alone, but for the folder itself: a file made in it would
    // take the process's default list.
    let done = Command::new("icacls")
        .arg(&inner)
        .args(["/inheritance:r", "/grant:r"])
        .arg(format!("{}:(F)", std::env::var("USERNAME").unwrap()))
        .output()
        .unwrap();
    assert!(done.status.success(), "{done:?}");
    close_folder(&inner).unwrap();
    std::fs::write(inner.join("made-after"), "x").unwrap();
    assert!(private(&inner.join("made-after")));
    let descriptor =
        Descriptor::of(&open(&inner, false).unwrap(), DACL_SECURITY_INFORMATION).unwrap();
    let acl = descriptor.dacl().unwrap().unwrap();
    assert!(passes_on(acl, &User::current().unwrap()).unwrap());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn check_folder_usable_refuses_a_folder_others_may_change_without_modifying_it() {
    let dir = scratch("usable");
    check_folder_usable(&dir).unwrap();
    let done = Command::new("icacls")
        .arg(&dir)
        .args(["/grant", "*S-1-1-0:(OI)(CI)(F)"])
        .output()
        .unwrap();
    assert!(done.status.success(), "{done:?}");
    let refused = check_folder_usable(&dir).unwrap_err();
    assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied, "{refused}");
    assert!(!private(&dir));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_pinned_backup_dir_cannot_be_removed_or_renamed_until_dropped() {
    let dir = scratch("pinned");
    open_to_everyone(&dir);
    let staging = dir.join(".staging");
    let finished = dir.join("finished");
    let pin = create_pinned_backup_dir(&staging).unwrap();
    assert!(private(&staging));
    // Files can be written inside while the folder is pinned, and inherit its ACL.
    std::fs::write(staging.join("db"), "x").unwrap();
    assert!(private(&staging.join("db")));
    // Removing or renaming the pinned folder itself is refused while the pin lives.
    assert!(std::fs::rename(&staging, &finished).is_err());
    drop(pin);
    std::fs::rename(&staging, &finished).unwrap();
    assert!(private(&finished));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn close_folder_preserving_keeps_a_nested_open_file_open() {
    let dir = scratch("preserve");
    open_to_everyone(&dir);
    let inner = dir.join("data");
    let sub = inner.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    let nested = sub.join("secret");
    std::fs::write(&nested, "x").unwrap();
    assert!(!private(&nested));
    let hidden = close_folder_preserving(&inner, &[nested.as_path()]).unwrap();
    assert!(hidden.is_empty());
    assert!(private(&inner));
    // The preserved file kept its open list even though its parent subfolder took the new one.
    assert!(!private(&nested));
    assert!(private(&sub));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn open_lock_file_refuses_a_directory_junction() {
    let dir = scratch("lock-junction");
    let target = dir.join("target");
    std::fs::create_dir(&target).unwrap();
    let link = dir.join("server.lock");
    let done = Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(&link)
        .arg(&target)
        .output()
        .unwrap();
    assert!(done.status.success(), "{done:?}");
    let refused = open_lock_file(&link).unwrap_err();
    assert_eq!(refused.kind(), io::ErrorKind::InvalidInput, "{refused}");
    let _ = std::fs::remove_dir(&link);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn all_application_packages_with_change_rights_is_refused() {
    let user = User::current().unwrap();
    let inherited_ac = descriptor("O:MED:AI(A;OICIID;FA;;;ME)(A;OICIID;GA;;;AC)");
    assert!(!judge(&inherited_ac, true, &user).unwrap());
    assert!(lets_others(inherited_ac.dacl().unwrap().unwrap(), &user, CHANGE, false).unwrap());

    let dir = scratch("app-packages");
    let done = Command::new("icacls")
        .arg(&dir)
        .args(["/grant", "*S-1-15-2-1:(OI)(CI)(F)"])
        .output()
        .unwrap();
    assert!(done.status.success(), "{done:?}");
    let refused = check_folder_usable(&dir).unwrap_err();
    assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied, "{refused}");
    assert!(
        refused.to_string().contains("another user or app"),
        "{refused}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
