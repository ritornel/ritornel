//! What registering the background service takes on every system that has
//! one: launchd's agent on a Mac, a systemd user unit on Linux.

use std::path::{Path, PathBuf};

use crate::data_dir::{self, DataDir};
use crate::{Env, Error};

/// What a service needs before it is registered: the data directory it
/// serves, with its log directory, which must be there before the service
/// manager opens the log in it, and this program, which it runs.
pub(crate) fn prepare(data_dir: Option<&Path>, env: Env<'_>) -> Result<(DataDir, PathBuf), Error> {
    let root = data_dir::resolve(data_dir, env)?;
    let dir = DataDir::prepare(root)?;
    let program = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|error| Error::Service(format!("cannot find this program: {error}")))?;
    Ok((dir, program))
}

/// What `install` says once the service is registered.
pub(crate) fn installed(program: &Path, dir: &DataDir) -> String {
    format!(
        "Installed the background service: {} serve --data-dir {}\n\
         It runs now and at each login, and the desktop app uses it. If another \
         server is running on that directory, the service starts once it stops.\n\
         Its log is in {}",
        program.display(),
        dir.root().display(),
        dir.logs().display()
    )
}
