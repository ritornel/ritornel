//! macOS.

use std::path::PathBuf;

use crate::local::{Env, set};

/// `~/Library/Application Support/Ritornel`: where macOS keeps what an
/// application owns and the user does not handle.
pub(crate) fn default_data_dir(env: Env<'_>) -> Option<PathBuf> {
    Some(set(env, "HOME")?.join("Library/Application Support/Ritornel"))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::local::tests::env;

    #[test]
    fn the_default_is_in_application_support() {
        assert_eq!(
            default_data_dir(&env(&[("HOME", "/Users/pat")])).unwrap(),
            Path::new("/Users/pat/Library/Application Support/Ritornel")
        );
        assert_eq!(default_data_dir(&env(&[])), None);
    }
}
