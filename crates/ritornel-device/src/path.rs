//! A path under a device's storage root that cannot leave it.

use std::fmt;

/// A path relative to a device's storage root, with `/` between its parts.
/// Every part is a plain name: none is empty, `.` or `..`, and none holds a
/// separator, a NUL or a drive prefix. So joining it to a root, a part at a
/// time, stays under that root as long as no part is a link, which a
/// family checks as it walks.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RelPath(String);

/// Why a string is no [`RelPath`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsafePath(pub String);

impl fmt::Display for UnsafePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?} is not a path under the storage root", self.0)
    }
}

impl std::error::Error for UnsafePath {}

fn is_plain(part: &str) -> bool {
    !part.is_empty()
        && part != "."
        && part != ".."
        && !part.contains(['/', '\\', '\0'])
        // `C:name` on Windows is relative to another drive's directory.
        && !part.contains(':')
}

impl RelPath {
    /// Reads `path`, refusing one with a part that is not a plain name.
    pub fn new(path: impl Into<String>) -> Result<Self, UnsafePath> {
        let path = path.into();
        if path.split('/').all(is_plain) {
            Ok(Self(path))
        } else {
            Err(UnsafePath(path))
        }
    }

    /// The path, with `/` between its parts.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Its parts, outermost first.
    pub fn parts(&self) -> impl DoubleEndedIterator<Item = &str> {
        self.0.split('/')
    }

    /// The path with `name` under it.
    pub fn join(&self, name: &str) -> Result<Self, UnsafePath> {
        Self::new(format!("{}/{name}", self.0))
    }

    /// Whether it is `folder` or lies under it.
    pub fn starts_with(&self, folder: &Self) -> bool {
        self.0 == folder.0
            || self
                .0
                .strip_prefix(&folder.0)
                .is_some_and(|rest| rest.starts_with('/'))
    }
}

impl fmt::Display for RelPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::RelPath;

    #[test]
    fn a_path_that_could_leave_the_root_is_refused() {
        for bad in [
            "",
            "/Music",
            "Music/",
            "Music//a.flac",
            "Music/../x",
            "..",
            "./Music",
            "Music\\x",
            "C:Music",
            "Music/a\0b",
        ] {
            assert!(RelPath::new(bad).is_err(), "{bad:?}");
        }
        let path = RelPath::new("Music/Ada Vale/Low Tide/01 Shoreline.flac").unwrap();
        assert_eq!(path.parts().count(), 4);
        assert!(path.starts_with(&RelPath::new("Music").unwrap()));
        assert!(!path.starts_with(&RelPath::new("Mus").unwrap()));
    }
}
