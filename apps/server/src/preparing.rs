//! `preparing.json`: what the server is doing before it listens, while that
//! takes more than a moment, so that a client can say so in place of
//! "not connected". See "Process lifecycle" in docs/internals/server.md.

use std::path::{Path, PathBuf};

use ritornel_contract::local::{self, Preparing};
use tracing::warn;

use crate::server_json;

/// Says each step in `preparing.json`. Dropping it removes the file, which
/// the server does once `server.json` is written.
#[derive(Debug)]
pub(crate) struct Steps {
    path: PathBuf,
}

impl Steps {
    pub(crate) fn new(path: &Path) -> Self {
        Self {
            path: path.to_owned(),
        }
    }

    /// Says that the server is now at `step`. A file that cannot be
    /// written only costs a client the words, so it is logged, and the
    /// server starts all the same.
    pub(crate) fn say(&self, step: Preparing) {
        let contents = local::PreparingJson {
            pid: std::process::id(),
            step,
        };
        if let Err(error) = server_json::write_whole(&self.path, &contents) {
            warn!(%error, "could not say what the server is preparing");
        }
    }
}

impl Drop for Steps {
    fn drop(&mut self) {
        let _ = server_json::remove(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Scratch;

    #[test]
    fn it_says_each_step_and_is_gone_when_dropped() {
        let scratch = Scratch::new();
        let path = scratch.path().join(local::PREPARING_JSON);
        let steps = Steps::new(&path);
        assert!(!path.exists(), "nothing is said before a step");

        steps.say(Preparing::Backup);
        steps.say(Preparing::Migration);
        let said: local::PreparingJson =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            said,
            local::PreparingJson {
                pid: std::process::id(),
                step: Preparing::Migration,
            }
        );

        drop(steps);
        assert!(!path.exists());
    }
}
