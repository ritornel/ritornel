//! Unix: a file's owner alone may read it when its mode says so.

#[cfg(test)]
use std::path::Path;

/// Whether only the file's owner may read or write it: no permission bit
/// for the group or others.
pub(crate) fn private(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o077 == 0
}

/// Makes the file at `path` one only its owner may read: mode `0600`.
#[cfg(test)]
pub(crate) fn close_to_others(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use crate::credentials::tests::dir;
    use crate::credentials::{ApiCredentials, CREDENTIALS_FILE};

    #[test]
    fn a_file_others_can_read_is_refused_without_saying_what_is_in_it() {
        let dir = dir();
        let none = |_: &str| None;
        let path = dir.join(CREDENTIALS_FILE);
        std::fs::write(
            &path,
            "SCROBL_API_KEY=SENTINEL_KEY\nSCROBL_API_SECRET=SENTINEL_SECRET\n",
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let refused = ApiCredentials::find(none, &dir).unwrap_err();
        assert!(refused.contains("chmod 600"), "{refused}");
        assert!(!refused.contains("SENTINEL"), "{refused}");
        std::fs::write(&path, [0xff]).unwrap();
        let refused = ApiCredentials::find(none, &dir).unwrap_err();
        assert!(refused.contains("chmod 600"), "{refused}");
        std::fs::write(&path, "SCROBL_API_KEY=test\nSCROBL_API_SECRET=test").unwrap();
        super::close_to_others(&path);
        assert!(ApiCredentials::find(none, &dir).unwrap().is_some());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
