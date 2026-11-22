//! The API key and secret: where they are found, and how they are kept out
//! of sight.
//!
//! `scrobl`'s own tool takes its key from the environment, as
//! `SCROBL_API_KEY`, and never from the command line, where other users of
//! the machine could see it. This does the same, with `SCROBL_API_SECRET`
//! beside it, since signing in and scrobbling need the secret too. A server
//! that something else starts, such as the desktop app, may not have the
//! environment of a shell, so the two can also be in a file in the data
//! directory, which must be readable by its owner alone.

use std::io::Read;
use std::path::Path;

use scrobl::{ApiKey, ApiSecret};

/// The variable the API key is read from: the name `scrobl`'s tool reads.
pub const KEY_VARIABLE: &str = "SCROBL_API_KEY";

/// The variable the API secret is read from.
pub const SECRET_VARIABLE: &str = "SCROBL_API_SECRET";

/// The file in the data directory that may hold both instead, one
/// `NAME=value` a line, with the two names above.
pub const CREDENTIALS_FILE: &str = "lastfm.env";

/// The API key and secret. Neither has `Display`, and `Debug` is redacted.
#[derive(Debug, Clone)]
pub struct ApiCredentials {
    key: ApiKey,
    secret: ApiSecret,
}

impl ApiCredentials {
    /// Made from the two values.
    pub fn new(key: impl Into<String>, secret: impl Into<String>) -> Self {
        Self {
            key: ApiKey::new(key),
            secret: ApiSecret::new(secret),
        }
    }

    /// Finds the key and the secret: in the environment when both
    /// variables are set there, otherwise in [`CREDENTIALS_FILE`] in
    /// `data_dir`. `None` when neither has both. A file that others may
    /// read, or that cannot be read, is an error whose text names the file
    /// and never what is in it.
    pub fn find(
        env: impl Fn(&str) -> Option<String>,
        data_dir: &Path,
    ) -> Result<Option<Self>, String> {
        let set = |name: &str| env(name).filter(|value| !value.trim().is_empty());
        if let (Some(key), Some(secret)) = (set(KEY_VARIABLE), set(SECRET_VARIABLE)) {
            return Ok(Some(Self::new(key.trim(), secret.trim())));
        }
        let path = data_dir.join(CREDENTIALS_FILE);
        let unreadable =
            |error: std::io::Error| format!("cannot read {}: {}", path.display(), error.kind());
        let mut file = match std::fs::File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(unreadable(error)),
        };
        let metadata = file.metadata().map_err(unreadable)?;
        if !metadata.is_file() {
            return Err(format!("{} must be a regular file", path.display()));
        }
        if !crate::platform::private(&metadata) {
            return Err(format!(
                "{} can be read by other users; make it readable by its owner alone \
                 (chmod 600) and start the server again",
                path.display()
            ));
        }
        let mut text = String::new();
        file.read_to_string(&mut text).map_err(unreadable)?;
        let (mut key, mut secret) = (None, None);
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((name, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim();
            let value = ['"', '\'']
                .into_iter()
                .find_map(|quote| value.strip_prefix(quote)?.strip_suffix(quote))
                .unwrap_or(value)
                .to_owned();
            match name.trim().trim_start_matches("export ").trim() {
                KEY_VARIABLE => key = Some(value),
                SECRET_VARIABLE => secret = Some(value),
                _ => {}
            }
        }
        Ok(match (key, secret) {
            (Some(key), Some(secret)) if !key.is_empty() && !secret.is_empty() => {
                Some(Self::new(key, secret))
            }
            _ => None,
        })
    }

    pub(crate) fn into_parts(self) -> (ApiKey, ApiSecret) {
        (self.key, self.secret)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A folder of its own for each test, even for two that start at the
    /// same instant in parallel.
    pub(crate) fn dir() -> std::path::PathBuf {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "ritornel-lastfm-credentials-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_environment_wins_and_a_file_is_the_fallback() {
        let dir = dir();
        let none = |_: &str| None;
        assert!(ApiCredentials::find(none, &dir).unwrap().is_none());

        let path = dir.join(CREDENTIALS_FILE);
        std::fs::write(
            &path,
            "# Last.fm\nSCROBL_API_KEY='SENTINEL_KEY'\nexport SCROBL_API_SECRET=\"SENTINEL_SECRET\"\n",
        )
        .unwrap();
        crate::platform::close_to_others(&path);
        let found = ApiCredentials::find(none, &dir).unwrap().unwrap();
        let (key, secret) = found.clone().into_parts();
        assert_eq!(
            (key.expose(), secret.expose()),
            ("SENTINEL_KEY", "SENTINEL_SECRET")
        );
        assert!(!format!("{found:?}").contains("SENTINEL"));

        let env = |name: &str| Some(format!("ENV_{name}"));
        let (key, _) = ApiCredentials::find(env, &dir)
            .unwrap()
            .unwrap()
            .into_parts();
        assert_eq!(key.expose(), "ENV_SCROBL_API_KEY");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
