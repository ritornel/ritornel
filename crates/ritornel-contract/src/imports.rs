//! Records imported from an archive a service gave the listener: today, a
//! stream of Spotify's extended streaming history. See "Spotify streams" in
//! docs/internals/history.md.

use serde::{Deserialize, Serialize};

use crate::{DeviceKind, Platform, PlayedVia};

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
    /// The operating system of the device that played it, when the row
    /// says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub platform: Option<Platform>,
    /// The hardware the sound came out of, when the row says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub device_kind: Option<DeviceKind>,
    /// How that device played it, when the row says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub via: Option<PlayedVia>,
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
            "deviceKind": "phone",
            "via": "app",
        })
    }

    #[test]
    fn a_stream_reads_and_writes_the_same() {
        let wire = stream();
        let read: SpotifyStream = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            (read.platform, read.device_kind, read.via),
            (
                Some(Platform::Ios),
                Some(DeviceKind::Phone),
                Some(PlayedVia::App)
            )
        );
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
        // A word it does not know is read as `other`, and never repeated.
        for field in ["platform", "deviceKind", "via"] {
            let mut wire = stream();
            wire[field] = json!("iOS 17.1 (MARKED_DEVICE)");
            let read: SpotifyStream = serde_json::from_value(wire).unwrap();
            let written = serde_json::to_string(&read).unwrap();
            assert!(!written.contains("MARKED"), "{written}");
            assert!(written.contains("\"other\""), "{written}");
        }
    }
}
