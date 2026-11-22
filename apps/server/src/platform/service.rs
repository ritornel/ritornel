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

    let program = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|error| Error::Service(format!("cannot find this program: {error}")))?;
    for path in [&root, &program] {
        check_path(path)?;
    }
    Ok((DataDir::prepare(root)?, program))
}

// These paths are written as text in a service definition.
fn check_path(path: &Path) -> Result<(), Error> {
    if path
        .to_str()
        .is_none_or(|p| p.chars().any(char::is_control))
    {
        return Err(Error::Service("The service path contains a control character or invalid text. Choose a plain-text path without control characters.".into()));
    }
    Ok(())
}

/// Put the new definition in place before asking the manager to stop or
/// reload anything. Keep the original file beside it until registration succeeds.
pub(super) struct Replacement<'a> {
    path: &'a Path,
    staged: tempfile::TempDir,
    had_file: bool,
    _lock: std::fs::File,
}

impl<'a> Replacement<'a> {
    pub(super) fn install(path: &'a Path, contents: &str) -> Result<Self, Error> {
        use crate::error::io_error;
        let parent = path
            .parent()
            .ok_or_else(|| Error::Service("Missing service directory".into()))?;
        std::fs::create_dir_all(parent).map_err(io_error("create", parent))?;
        let lock = std::fs::File::open(parent).map_err(io_error("open", parent))?;
        lock.lock().map_err(io_error("lock", parent))?;
        let staged = tempfile::tempdir_in(parent).map_err(io_error("stage", path))?;
        let had_file = match std::fs::hard_link(path, staged.path().join("original")) {
            Ok(()) => true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => return Err(io_error("save", path)(e)),
        };
        let new = staged.path().join("new");
        std::fs::write(&new, contents).map_err(io_error("write", path))?;
        std::fs::rename(new, path).map_err(io_error("install", path))?;
        Ok(Self {
            path,
            staged,
            had_file,
            _lock: lock,
        })
    }

    pub(super) fn protect_running(self, registered: bool) -> Result<Self, Error> {
        if registered && !self.had_file {
            let error = Error::Service(format!(
                "The service has no saved definition at {}. Restore that file or stop the service before installing.",
                self.path.display()
            ));
            return Err(self.rollback(error, || Ok(())));
        }
        Ok(self)
    }

    pub(super) fn rollback(
        self,
        error: Error,
        reload: impl FnOnce() -> Result<(), Error>,
    ) -> Error {
        let restored = if self.had_file {
            std::fs::rename(self.staged.path().join("original"), self.path)
        } else {
            std::fs::remove_file(self.path)
        };
        if let Err(failed) = restored {
            let backup = self.staged.keep();
            return Error::Service(format!(
                "{error}. Restoration failed ({failed}). Check the saved files in {}.",
                backup.display()
            ));
        }
        match reload() {
            Ok(()) => error,
            Err(failed) => Error::Service(format!(
                "{error}. Restoring the service failed ({failed}). Run service status before trying again."
            )),
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_that_cannot_be_written_literally_are_refused() {
        for path in ["/music/first\nsecond", "/music/first\rsecond"] {
            assert!(check_path(Path::new(path)).is_err());
        }
        assert!(check_path(Path::new("/music/first second")).is_ok());
    }
}
