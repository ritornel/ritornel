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
use windows_sys::core::PWSTR;

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

/// Whether `path` is private, asserting that the system's access check for
/// another user agrees: on a file both ways, and on a folder, whose list
/// may let others into only what is made in it, that one private lets
/// nobody in.
fn private(path: &Path) -> bool {
    let private = path_is_private(path).unwrap();
    let open = another_user_may_open(path).unwrap();
    if path.is_dir() {
        assert!(!(private && open), "{}", path.display());
    } else {
        assert_eq!(open, !private, "{}", path.display());
    }
    private
}

/// A descriptor written in SDDL, in which `ME` stands for this user.
fn descriptor(sddl: &str) -> Descriptor {
    let user = User::current().unwrap();
    let mut text: PWSTR = std::ptr::null_mut();
    // SAFETY: the SID is valid; the string is allocated by the call.
    assert_ne!(
        unsafe { ConvertSidToStringSidW(user.sid().0, &mut text) },
        0
    );
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
fn closing_a_folder_closes_what_is_made_in_it_and_leaves_its_files_as_they_were() {
    let dir = scratch("close");
    open_to_everyone(&dir);
    let inner = dir.join("data");
    std::fs::create_dir(&inner).unwrap();
    std::fs::create_dir(inner.join("logs")).unwrap();
    std::fs::write(inner.join("credential"), "x").unwrap();
    std::fs::write(inner.join("logs").join("old.log"), "x").unwrap();
    assert!(!private(&inner.join("credential")));
    close_folder(&inner).unwrap();
    assert!(private(&inner));
    // A file in it keeps the access it had, for its own check to see.
    assert!(!private(&inner.join("credential")));
    // What a folder in it holds takes the new list.
    assert!(private(&inner.join("logs")));
    assert!(private(&inner.join("logs").join("old.log")));
    std::fs::write(inner.join("made-after"), "x").unwrap();
    assert!(private(&inner.join("made-after")));
    // Closed, it is left as it is.
    close_folder(&inner).unwrap();
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
fn a_pinned_restricted_dir_cannot_be_removed_or_renamed_until_dropped() {
    let dir = scratch("pinned");
    open_to_everyone(&dir);
    let staging = dir.join(".staging");
    let finished = dir.join("finished");
    let pin = create_pinned_restricted_dir(&staging).unwrap();
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
    close_folder_preserving(&inner, &[nested.as_path()]).unwrap();
    assert!(private(&inner));
    // The preserved file kept its open ACL even though its parent subfolder took the new one.
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
    assert!(open_lock_file(&link).is_err());
    let _ = std::fs::remove_dir(&link);
    let _ = std::fs::remove_dir_all(&dir);
}
