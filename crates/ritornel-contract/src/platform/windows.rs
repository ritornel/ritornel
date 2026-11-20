//! Windows.

use std::path::PathBuf;

use crate::local::{Env, set};

/// `%LOCALAPPDATA%\Ritornel`, which is `C:\Users\<user>\AppData\Local\Ritornel`.
/// It is the local application data folder and not the roaming one
/// (`%APPDATA%`): a roaming profile is copied to every machine the user
/// signs in to, and `server.db` holds this server's own identity, which two
/// machines must never share, beside a library, a lock and a cache that
/// belong to this machine. A relative value is ignored.
pub(crate) fn default_data_dir(env: Env<'_>) -> Option<PathBuf> {
    set(env, "LOCALAPPDATA")
        .filter(|base| base.is_absolute())
        .map(|base| base.join("Ritornel"))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::local::tests::env;

    #[test]
    fn the_default_is_in_local_application_data() {
        assert_eq!(
            default_data_dir(&env(&[("LOCALAPPDATA", r"C:\Users\pat\AppData\Local")])).unwrap(),
            Path::new(r"C:\Users\pat\AppData\Local\Ritornel")
        );
        // Never the roaming folder, nor a relative one.
        assert_eq!(
            default_data_dir(&env(&[("APPDATA", r"C:\Users\pat\AppData\Roaming")])),
            None
        );
        assert_eq!(default_data_dir(&env(&[("LOCALAPPDATA", "AppData")])), None);
        assert_eq!(default_data_dir(&env(&[])), None);
    }
}
