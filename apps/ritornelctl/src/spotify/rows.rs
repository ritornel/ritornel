//! The rows of one file of Spotify's extended streaming history, read into
//! the fields Ritornel keeps. See "Spotify streams" in
//! docs/internals/history.md.
//!
//! A row's fields are taken by name. A field that is not kept is skipped as
//! the file is parsed, so its value is never held, and a field the reader
//! does not know is remembered by its name alone. A value is never printed:
//! a row that will not read is told by its file, its number and the name of
//! the field that was wrong.

use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;

use ritornel_client::contract::{SpotifyPlatform, SpotifyStream};
use serde::de::{DeserializeSeed, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde_json::Value;

/// The fields that are kept, by the name Spotify gives each.
const KEPT: [&str; 14] = [
    "ts",
    "ms_played",
    "master_metadata_track_name",
    "master_metadata_album_artist_name",
    "master_metadata_album_album_name",
    "spotify_track_uri",
    "reason_start",
    "reason_end",
    "shuffle",
    "skipped",
    "offline",
    "offline_timestamp",
    "incognito_mode",
    "platform",
];

/// The fields that are known and never kept: who and where the listener
/// was, and what a row that is not music names.
const DROPPED: [&str; 12] = [
    "ip_addr",
    "ip_addr_decrypted",
    "user_agent_decrypted",
    "username",
    "conn_country",
    "episode_name",
    "episode_show_name",
    "spotify_episode_uri",
    "audiobook_title",
    "audiobook_uri",
    "audiobook_chapter_uri",
    "audiobook_chapter_title",
];

/// What a row that is not music is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum NotMusic {
    /// An episode of a podcast or show.
    Episode,
    /// A chapter of an audiobook.
    Audiobook,
    /// A row whose URI is no `spotify:track:`, or that has none.
    NoTrack,
    /// A track that names no artist or no title.
    Unnamed,
}

impl NotMusic {
    /// Every kind, in order.
    pub(crate) const ALL: [Self; 4] =
        [Self::Episode, Self::Audiobook, Self::NoTrack, Self::Unnamed];

    /// The kind, as one word.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Episode => "episode",
            Self::Audiobook => "audiobook",
            Self::NoTrack => "no_track",
            Self::Unnamed => "unnamed",
        }
    }
}

/// What one row is.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Row {
    /// A stream of a track, as it is kept.
    Music(SpotifyStream),
    /// A row that is not music, counted and not kept.
    NotMusic(NotMusic),
    /// A row with a field that will not read, by the field's name.
    Wrong(&'static str),
}

/// What a file held: its rows in order, and the names of the fields the
/// reader does not know.
#[derive(Debug, Default)]
pub(crate) struct FileRows {
    pub(crate) rows: Vec<Row>,
    pub(crate) unknown: BTreeSet<String>,
}

/// Why a file could not be read. It holds no value from the file and none
/// of the parser's own words, which quote what it choked on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FileError {
    /// The file is not a JSON list of rows: it broke at this row, from 1,
    /// or before the first when 0.
    NotRows(usize),
}

impl fmt::Display for FileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotRows(0) => f.write_str("it is not a list of rows"),
            Self::NotRows(row) => write!(f, "row {row} is not a row of the history"),
        }
    }
}

/// Reads one file's rows.
pub(crate) fn read(bytes: &[u8]) -> Result<FileRows, FileError> {
    let mut rows = FileRows::default();
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let mut seen = 0;
    let result = deserializer.deserialize_seq(FileVisitor {
        rows: &mut rows,
        seen: &mut seen,
    });
    match result.and_then(|()| deserializer.end()) {
        Ok(()) => Ok(rows),
        Err(_) => Err(FileError::NotRows(seen)),
    }
}

struct FileVisitor<'a> {
    rows: &'a mut FileRows,
    /// The number of the row being read, from 1, so that a failure can say
    /// where it was.
    seen: &'a mut usize,
}

impl<'de> Visitor<'de> for FileVisitor<'_> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a list of rows")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        loop {
            *self.seen += 1;
            let Some(raw) = seq.next_element_seed(RawSeed {
                unknown: &mut self.rows.unknown,
            })?
            else {
                return Ok(());
            };
            self.rows.rows.push(raw.row());
        }
    }
}

/// A row's kept fields, before they are checked, and whether the fields of
/// a row that is not music held anything.
#[derive(Default)]
struct Raw {
    kept: [Option<Value>; KEPT.len()],
    episode: bool,
    audiobook: bool,
}

struct RawSeed<'a> {
    unknown: &'a mut BTreeSet<String>,
}

impl<'de> DeserializeSeed<'de> for RawSeed<'_> {
    type Value = Raw;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Raw, D::Error> {
        deserializer.deserialize_map(self)
    }
}

impl<'de> Visitor<'de> for RawSeed<'_> {
    type Value = Raw;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a row")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Raw, A::Error> {
        let mut raw = Raw::default();
        while let Some(name) = map.next_key::<String>()? {
            if let Some(at) = KEPT.iter().position(|kept| *kept == name) {
                let value = map.next_value::<Value>()?;
                if let Some(slot) = raw.kept.get_mut(at) {
                    *slot = Some(value);
                }
                continue;
            }
            // Only whether it held anything: `null` or a value, never what.
            let held = map.next_value::<Option<IgnoredAny>>()?.is_some();
            if name.starts_with("episode_") || name == "spotify_episode_uri" {
                raw.episode |= held;
            } else if name.starts_with("audiobook_") {
                raw.audiobook |= held;
            } else if !DROPPED.contains(&name.as_str()) {
                self.unknown.insert(name);
            }
        }
        Ok(raw)
    }
}

impl Raw {
    fn take(&mut self, name: &'static str) -> Option<Value> {
        let at = KEPT.iter().position(|kept| *kept == name)?;
        self.kept
            .get_mut(at)?
            .take()
            .filter(|value| !value.is_null())
    }

    fn row(mut self) -> Row {
        if self.episode {
            return Row::NotMusic(NotMusic::Episode);
        }
        if self.audiobook {
            return Row::NotMusic(NotMusic::Audiobook);
        }
        match self.stream() {
            Ok(stream) => stream,
            Err(field) => Row::Wrong(field),
        }
    }

    fn stream(&mut self) -> Result<Row, &'static str> {
        let track_uri = text(self, "spotify_track_uri")?;
        let Some(track_uri) = track_uri.filter(|uri| is_track(uri)) else {
            return Ok(Row::NotMusic(NotMusic::NoTrack));
        };
        let title = text(self, "master_metadata_track_name")?.filter(|name| !name.is_empty());
        let artist =
            text(self, "master_metadata_album_artist_name")?.filter(|name| !name.is_empty());
        let (Some(title), Some(artist)) = (title, artist) else {
            return Ok(Row::NotMusic(NotMusic::Unnamed));
        };
        let ended_at = text(self, "ts")?.and_then(|ts| instant(&ts)).ok_or("ts")?;
        let ms_played = whole(self, "ms_played")?
            .filter(|ms| *ms >= 0)
            .ok_or("ms_played")?;
        let platform = text(self, "platform")?;
        Ok(Row::Music(SpotifyStream {
            ended_at,
            ms_played,
            track_uri,
            title,
            artist,
            album: text(self, "master_metadata_album_album_name")?.filter(|name| !name.is_empty()),
            reason_start: text(self, "reason_start")?,
            reason_end: text(self, "reason_end")?,
            shuffle: flag(self, "shuffle")?,
            skipped: flag(self, "skipped")?,
            offline: flag(self, "offline")?,
            offline_timestamp: whole(self, "offline_timestamp")?,
            incognito: flag(self, "incognito_mode")?,
            platform: platform
                .as_deref()
                .map_or(SpotifyPlatform::Other, platform_word),
        }))
    }
}

/// A field that is text, or nothing.
fn text(raw: &mut Raw, name: &'static str) -> Result<Option<String>, &'static str> {
    match raw.take(name) {
        None => Ok(None),
        Some(Value::String(text)) => Ok(Some(text)),
        Some(_) => Err(name),
    }
}

/// A field that is true or false, or nothing.
fn flag(raw: &mut Raw, name: &'static str) -> Result<Option<bool>, &'static str> {
    match raw.take(name) {
        None => Ok(None),
        Some(Value::Bool(flag)) => Ok(Some(flag)),
        Some(_) => Err(name),
    }
}

/// A field that is a whole number, or nothing.
fn whole(raw: &mut Raw, name: &'static str) -> Result<Option<i64>, &'static str> {
    match raw.take(name) {
        None => Ok(None),
        Some(Value::Number(number)) => number.as_i64().map(Some).ok_or(name),
        Some(_) => Err(name),
    }
}

/// Whether a URI is a track's, with an ID after the prefix.
fn is_track(uri: &str) -> bool {
    uri.strip_prefix("spotify:track:")
        .is_some_and(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric()))
}

/// An instant in UTC, as Unix milliseconds.
fn instant(ts: &str) -> Option<i64> {
    jiff::Timestamp::from_str(ts)
        .ok()
        .map(jiff::Timestamp::as_millisecond)
}

/// The kind of device a platform's text names. Only the word is kept.
pub(crate) fn platform_word(text: &str) -> SpotifyPlatform {
    let text = text.to_ascii_lowercase();
    let has = |word: &str| text.contains(word);
    if has("web_player") || has("web player") || text.starts_with("web") {
        SpotifyPlatform::Web
    } else if has("cast") || text.starts_with("partner") {
        SpotifyPlatform::Cast
    } else if has("android") {
        SpotifyPlatform::Android
    } else if text.starts_with("ios") || has("iphone") || has("ipad") || has("ipod") {
        SpotifyPlatform::Ios
    } else if text.starts_with("os x") || text.starts_with("osx") || text.starts_with("macos") {
        SpotifyPlatform::Mac
    } else if text.starts_with("windows") {
        SpotifyPlatform::Windows
    } else if text.starts_with("linux") {
        SpotifyPlatform::Linux
    } else {
        SpotifyPlatform::Other
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// An invented row with every field Spotify gives, the dropped ones
    /// holding a marked value.
    pub(crate) fn row() -> Value {
        json!({
            "ts": "2019-03-15T21:14:09Z",
            "username": "MARKED_USERNAME",
            "platform": "iOS 12.1 (MARKED_DEVICE)",
            "ms_played": 221_000,
            "conn_country": "MARKED_COUNTRY",
            "ip_addr": "MARKED_IP",
            "user_agent_decrypted": "MARKED_AGENT",
            "master_metadata_track_name": "Low Tide",
            "master_metadata_album_artist_name": "Calla Brandt",
            "master_metadata_album_album_name": "Glasshouse",
            "spotify_track_uri": "spotify:track:0000000000000000000001",
            "episode_name": null,
            "episode_show_name": null,
            "spotify_episode_uri": null,
            "audiobook_title": null,
            "audiobook_uri": null,
            "audiobook_chapter_uri": null,
            "audiobook_chapter_title": null,
            "reason_start": "trackdone",
            "reason_end": "trackdone",
            "shuffle": false,
            "skipped": false,
            "offline": false,
            "offline_timestamp": 1_552_684_000,
            "incognito_mode": false,
        })
    }

    fn one(row: &Value) -> Row {
        let bytes = serde_json::to_vec(&json!([row])).unwrap();
        let mut rows = read(&bytes).unwrap();
        assert_eq!(rows.rows.len(), 1);
        rows.rows.pop().unwrap()
    }

    #[test]
    fn a_row_keeps_its_kept_fields_and_nothing_else() {
        let Row::Music(stream) = one(&row()) else {
            panic!("not music");
        };
        assert_eq!(
            stream,
            SpotifyStream {
                ended_at: 1_552_684_449_000,
                ms_played: 221_000,
                track_uri: "spotify:track:0000000000000000000001".to_owned(),
                title: "Low Tide".to_owned(),
                artist: "Calla Brandt".to_owned(),
                album: Some("Glasshouse".to_owned()),
                reason_start: Some("trackdone".to_owned()),
                reason_end: Some("trackdone".to_owned()),
                shuffle: Some(false),
                skipped: Some(false),
                offline: Some(false),
                offline_timestamp: Some(1_552_684_000),
                incognito: Some(false),
                platform: SpotifyPlatform::Ios,
            }
        );
        let kept = serde_json::to_string(&stream).unwrap();
        assert!(!kept.contains("MARKED"), "{kept}");
    }

    #[test]
    fn rows_that_are_not_music_are_told_apart() {
        let mut episode = row();
        episode["episode_name"] = json!("MARKED_EPISODE");
        episode["spotify_track_uri"] = Value::Null;
        assert_eq!(one(&episode), Row::NotMusic(NotMusic::Episode));
        let mut book = row();
        book["audiobook_title"] = json!("MARKED_BOOK");
        assert_eq!(one(&book), Row::NotMusic(NotMusic::Audiobook));
        for uri in [
            Value::Null,
            json!("spotify:local:a:b:c:200"),
            json!("spotify:track:"),
        ] {
            let mut local = row();
            local["spotify_track_uri"] = uri;
            assert_eq!(one(&local), Row::NotMusic(NotMusic::NoTrack));
        }
        for field in [
            "master_metadata_track_name",
            "master_metadata_album_artist_name",
        ] {
            let mut unnamed = row();
            unnamed[field] = Value::Null;
            assert_eq!(one(&unnamed), Row::NotMusic(NotMusic::Unnamed), "{field}");
        }
    }

    #[test]
    fn a_wrong_field_is_told_by_its_name() {
        for (field, value) in [
            ("ts", json!("MARKED_NOT_A_TIME")),
            ("ts", Value::Null),
            ("ms_played", json!("MARKED")),
            ("ms_played", json!(-1)),
            ("shuffle", json!("MARKED")),
            ("offline_timestamp", json!(1.5)),
            ("reason_end", json!(7)),
        ] {
            let mut wrong = row();
            wrong[field] = value;
            assert_eq!(
                one(&wrong),
                Row::Wrong(KEPT.iter().find(|k| **k == field).unwrap())
            );
        }
    }

    #[test]
    fn an_unknown_field_is_remembered_by_its_name_alone() {
        let mut new = row();
        new["listener_mood"] = json!("MARKED_MOOD");
        let bytes = serde_json::to_vec(&json!([new])).unwrap();
        let rows = read(&bytes).unwrap();
        assert_eq!(
            rows.unknown.iter().collect::<Vec<_>>(),
            [&"listener_mood".to_owned()]
        );
    }

    #[test]
    fn a_file_that_is_not_rows_says_where_it_broke() {
        let good = serde_json::to_string(&row()).unwrap();
        assert_eq!(read(b"{}").unwrap_err(), FileError::NotRows(0));
        assert_eq!(
            read(format!("[{good}, {good}, 7]").as_bytes()).unwrap_err(),
            FileError::NotRows(3)
        );
        assert_eq!(
            read(format!("[{good}, {{\"ts\": MARKED").as_bytes()).unwrap_err(),
            FileError::NotRows(2)
        );
        assert_eq!(read(b"[]").unwrap().rows, []);
    }

    #[test]
    fn a_platform_is_kept_as_one_word() {
        for (text, word) in [
            ("OS X 10.9.5 [x86 4]", SpotifyPlatform::Mac),
            ("osx", SpotifyPlatform::Mac),
            ("Windows 10 (10.0.19045; x64)", SpotifyPlatform::Windows),
            ("windows", SpotifyPlatform::Windows),
            ("Linux [x86-0]", SpotifyPlatform::Linux),
            (
                "Android OS 9 API 28 (Invented, Model)",
                SpotifyPlatform::Android,
            ),
            ("android", SpotifyPlatform::Android),
            ("iOS 17.1 (iPhone15,2)", SpotifyPlatform::Ios),
            ("ios", SpotifyPlatform::Ios),
            ("web_player osx 10.15.7;chrome 120.0", SpotifyPlatform::Web),
            ("WebPlayer (websocket RFC6455)", SpotifyPlatform::Web),
            ("Partner google cast_tv;Invented;;", SpotifyPlatform::Cast),
            ("Partner invented_speaker;Model;;", SpotifyPlatform::Cast),
            ("cast_to_device", SpotifyPlatform::Cast),
            ("", SpotifyPlatform::Other),
            ("something new", SpotifyPlatform::Other),
        ] {
            assert_eq!(platform_word(text), word, "{text}");
        }
    }
}
