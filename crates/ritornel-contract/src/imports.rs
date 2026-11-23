//! Records imported from an archive a service gave the listener: today, a
//! stream of Spotify's extended streaming history. See "Spotify streams" in
//! docs/internals/history.md.

use serde::{Deserialize, Serialize};

/// One row of Spotify's extended streaming history, as Ritornel keeps it.
///
/// It is the whole of what is taken from a row. The row's IP address, user
/// agent, account name and country, its platform as text, and any field
/// this type does not name are never read into it, and the type refuses a
/// field it does not name, so nothing else can ride along with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SpotifyStream {
    /// When the stream ended, in Unix milliseconds, to the second.
    pub ended_at: i64,
    /// How long it played.
    pub ms_played: i64,
    /// The track, as `spotify:track:` and its ID.
    pub track_uri: String,
    /// The track's title.
    pub title: String,
    /// The artist, as Spotify named it.
    pub artist: String,
    /// The album, as Spotify named it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub album: Option<String>,
    /// Why it began, as Spotify says it, such as `clickrow`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub reason_start: Option<String>,
    /// Why it ended, as Spotify says it, such as `trackdone`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub reason_end: Option<String>,
    /// Whether shuffle was on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub shuffle: Option<bool>,
    /// Spotify's own mark that the track was skipped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub skipped: Option<bool>,
    /// Whether it played offline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub offline: Option<bool>,
    /// When offline mode was used, as the row gives it. Its unit is not
    /// documented, so it is kept as given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub offline_timestamp: Option<i64>,
    /// Whether it was played in a private session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub incognito: Option<bool>,
    /// What kind of device played it, as one word.
    pub platform: SpotifyPlatform,
}

/// What kind of device played a stream. Spotify's own text names the device
/// and its system, so only this word is kept. A word it does not know is
/// refused with an error that does not repeat it, so that text sent in its
/// place is never echoed back or logged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum SpotifyPlatform {
    /// A Mac.
    Mac,
    /// A Windows computer.
    Windows,
    /// A Linux computer.
    Linux,
    /// An Android phone or tablet.
    Android,
    /// An iPhone or iPad.
    Ios,
    /// Spotify's web player.
    Web,
    /// A speaker, television or other device Spotify casts to.
    Cast,
    /// Anything else, or nothing said.
    Other,
}

impl<'de> Deserialize<'de> for SpotifyPlatform {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Word;
        impl serde::de::Visitor<'_> for Word {
            type Value = SpotifyPlatform;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a platform word")
            }
            fn visit_str<E: serde::de::Error>(self, word: &str) -> Result<SpotifyPlatform, E> {
                SpotifyPlatform::ALL
                    .into_iter()
                    .find(|platform| platform.as_str() == word)
                    .ok_or_else(|| E::custom("a platform is one of its words"))
            }
        }
        deserializer.deserialize_str(Word)
    }
}

impl SpotifyPlatform {
    /// Every word, in order.
    pub const ALL: [Self; 8] = [
        Self::Mac,
        Self::Windows,
        Self::Linux,
        Self::Android,
        Self::Ios,
        Self::Web,
        Self::Cast,
        Self::Other,
    ];

    /// The word, as it is on the wire.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Mac => "mac",
            Self::Windows => "windows",
            Self::Linux => "linux",
            Self::Android => "android",
            Self::Ios => "ios",
            Self::Web => "web",
            Self::Cast => "cast",
            Self::Other => "other",
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn stream() -> serde_json::Value {
        json!({
            "endedAt": 1_552_684_449_000_i64,
            "msPlayed": 221_000,
            "trackUri": "spotify:track:0000000000000000000001",
            "title": "Low Tide",
            "artist": "Calla Brandt",
            "album": "Glasshouse",
            "reasonStart": "trackdone",
            "reasonEnd": "trackdone",
            "shuffle": false,
            "skipped": false,
            "offline": false,
            "incognito": false,
            "platform": "ios",
        })
    }

    #[test]
    fn a_stream_reads_and_writes_the_same() {
        let wire = stream();
        let read: SpotifyStream = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(read.platform, SpotifyPlatform::Ios);
        assert_eq!(serde_json::to_value(read).unwrap(), wire);
    }

    #[test]
    fn a_stream_refuses_any_field_it_does_not_name() {
        for field in ["ipAddr", "userAgentDecrypted", "username", "connCountry"] {
            let mut wire = stream();
            wire[field] = json!("anything");
            assert!(
                serde_json::from_value::<SpotifyStream>(wire).is_err(),
                "{field}"
            );
        }
        let mut wire = stream();
        wire["platform"] = json!("iOS 17.1 (MARKED_DEVICE)");
        let error = serde_json::from_value::<SpotifyStream>(wire)
            .unwrap_err()
            .to_string();
        assert!(!error.contains("MARKED"), "{error}");
        for platform in SpotifyPlatform::ALL {
            let word = serde_json::to_value(platform).unwrap();
            assert_eq!(word, json!(platform.as_str()));
            assert_eq!(
                serde_json::from_value::<SpotifyPlatform>(word).unwrap(),
                platform
            );
        }
    }
}
