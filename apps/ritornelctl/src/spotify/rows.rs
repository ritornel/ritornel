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

use ritornel_client::contract::{DeviceKind, Platform, PlayedVia, SpotifyStream};
use serde::de::{DeserializeSeed, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde_json::Value;
use serde_json::value::RawValue;

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
/// was.
const DROPPED: [&str; 5] = [
    "ip_addr",
    "ip_addr_decrypted",
    "user_agent_decrypted",
    "username",
    "conn_country",
];

/// The fields of an episode of a show: a row that holds any is not music.
const EPISODE: [&str; 3] = ["episode_name", "episode_show_name", "spotify_episode_uri"];

/// The fields of a chapter of an audiobook: a row that holds any is not
/// music.
const AUDIOBOOK: [&str; 4] = [
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
    /// The list of rows is followed by something else.
    AfterRows,
}

impl fmt::Display for FileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotRows(0) => f.write_str("it is not a list of rows"),
            Self::NotRows(row) => write!(f, "row {row} is not a row of the history"),
            Self::AfterRows => f.write_str("something follows its list of rows"),
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
    result.map_err(|_| FileError::NotRows(seen))?;
    deserializer.end().map_err(|_| FileError::AfterRows)?;
    Ok(rows)
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

/// A row's kept fields, as the text of each before it is read, and whether
/// the fields of a row that is not music held anything. Each kept value is
/// read on its own, so that one that will not read is told by its field.
#[derive(Default)]
struct Raw {
    kept: [Option<Box<RawValue>>; KEPT.len()],
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
                let value = map.next_value::<Box<RawValue>>()?;
                if let Some(slot) = raw.kept.get_mut(at) {
                    *slot = Some(value);
                }
                continue;
            }
            // Only whether it held anything: `null` or a value, never what.
            let held = map.next_value::<Option<IgnoredAny>>()?.is_some();
            if EPISODE.contains(&name.as_str()) {
                raw.episode |= held;
            } else if AUDIOBOOK.contains(&name.as_str()) {
                raw.audiobook |= held;
            } else if !DROPPED.contains(&name.as_str()) {
                self.unknown.insert(name);
            }
        }
        Ok(raw)
    }
}

impl Raw {
    /// A kept field's value, or nothing for a field that is not there or
    /// is `null`. A value that will not read, as a number too large for
    /// any type, is told by the field's name.
    fn take(&mut self, name: &'static str) -> Result<Option<Value>, &'static str> {
        let Some(raw) = KEPT
            .iter()
            .position(|kept| *kept == name)
            .and_then(|at| self.kept.get_mut(at)?.take())
        else {
            return Ok(None);
        };
        let value: Value = serde_json::from_str(raw.get()).map_err(|_| name)?;
        Ok(Some(value).filter(|value| !value.is_null()))
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
        let (platform, device_kind, via) = text(self, "platform")?
            .as_deref()
            .map_or((None, None, None), device_words);
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
            platform,
            device_kind,
            via,
        }))
    }
}

/// A field that is text, or nothing.
fn text(raw: &mut Raw, name: &'static str) -> Result<Option<String>, &'static str> {
    match raw.take(name)? {
        None => Ok(None),
        Some(Value::String(text)) => Ok(Some(text)),
        Some(_) => Err(name),
    }
}

/// A field that is true or false, or nothing.
fn flag(raw: &mut Raw, name: &'static str) -> Result<Option<bool>, &'static str> {
    match raw.take(name)? {
        None => Ok(None),
        Some(Value::Bool(flag)) => Ok(Some(flag)),
        Some(_) => Err(name),
    }
}

/// A field that is a whole number, or nothing.
fn whole(raw: &mut Raw, name: &'static str) -> Result<Option<i64>, &'static str> {
    match raw.take(name)? {
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

/// What a platform's text says of the device that played the stream: its
/// operating system, the hardware the sound came out of, and how it played
/// it. Each is left out where the text does not say. Only these words are
/// kept, and the text is read only for them.
pub(crate) type Words = (Option<Platform>, Option<DeviceKind>, Option<PlayedVia>);

/// The words for a platform's text. Spotify's text names the system first
/// and the device after it, or, for a Spotify Connect device, `Partner`
/// and its maker and model. A device's own name never decides a word: the
/// system, a model of the maker's or a kind the maker names does.
pub(crate) fn device_words(text: &str) -> Words {
    use DeviceKind::{Car, Computer, Phone, Tablet, Watch};
    use Platform::{Android, Ios, Linux, Mac, Windows};
    let text = text.trim().to_ascii_lowercase();
    let app = Some(PlayedVia::App);
    if text.is_empty() || text == "not_applicable" || text == "unknown" {
        return (None, None, None);
    }
    if text.starts_with("partner") {
        return partner(&text);
    }
    if text.starts_with("cast") {
        return (None, None, Some(PlayedVia::Cast));
    }
    if let Some(host) = ["webplayer", "web_player", "web player"]
        .iter()
        .find_map(|prefix| text.strip_prefix(prefix))
    {
        // An older web player names the system it ran on, a computer's.
        let host = host.trim_start();
        let platform =
            if host.starts_with("osx") || host.starts_with("os x") || host.starts_with("mac") {
                Some(Mac)
            } else if host.starts_with("windows") {
                Some(Windows)
            } else if host.starts_with("linux") {
                Some(Linux)
            } else {
                None
            };
        return (platform, platform.map(|_| Computer), Some(PlayedVia::Web));
    }
    if text.starts_with("android") {
        let kind = (text.contains("android auto") || text.contains("android_auto")).then_some(Car);
        return (Some(Android), kind, app);
    }
    if text.starts_with("ios") {
        // iOS names the model after its version.
        let kind = if text.contains("iphone") {
            Some(Phone)
        } else if text.contains("ipad") {
            Some(Tablet)
        } else if text.contains("watch") {
            Some(Watch)
        } else {
            None
        };
        return (Some(Ios), kind, app);
    }
    for (prefix, platform) in [
        ("os x", Mac),
        ("osx", Mac),
        ("macos", Mac),
        ("windows", Windows),
        ("linux", Linux),
    ] {
        if text.starts_with(prefix) {
            return (Some(platform), Some(Computer), app);
        }
    }
    (Some(Platform::Other), None, None)
}

/// The words for a Spotify Connect device: `Partner`, its integration, and
/// its maker, model and more after it. A television, a set-top box or a
/// console plays Spotify in an app of its own; Google's speakers, displays
/// and dongles are cast to; a receiver or a network speaker with no app is a
/// Connect receiver.
fn partner(text: &str) -> Words {
    use DeviceKind::{Car, Console, Group, Speaker, Tv};
    use PlayedVia::{App, Cast, Connect};
    let mut words = text.split_whitespace();
    let _partner = words.next();
    let integration = words.next().unwrap_or_default();
    let rest: String = words.collect::<Vec<_>>().join(" ");
    let mut fields = rest.split(';');
    let maker = fields.next().unwrap_or_default();
    let model = fields.next().unwrap_or_default();
    let named = |token: &str| maker.contains(token) || model.contains(token);
    let google = integration.contains("google") || named("google") || named("chromecast");
    let other = Some(Platform::Other);
    if named("avr") || named("receiver") {
        return (None, Some(Speaker), Some(Connect));
    }
    if named("xbox") || named("playstation") {
        return (other, Some(Console), Some(App));
    }
    if model == "car" || named("car thing") || named("carplay") || named("android auto") {
        return (None, Some(Car), None);
    }
    if named("group") {
        return (None, Some(Group), Some(if google { Cast } else { Connect }));
    }
    if named("cast_audio") || named("chromecast audio") {
        return (None, Some(Speaker), Some(Cast));
    }
    if named("cast_tv") || named("chromecast") {
        return (None, Some(Tv), Some(Cast));
    }
    if google && (named("home") || named("nest") || named("display")) {
        return (None, Some(Speaker), Some(Cast));
    }
    if named("echo") || named("sonos") || named("speaker") {
        return (None, Some(Speaker), Some(Connect));
    }
    if integration.contains("roku")
        || named("roku")
        || named("tv")
        || named("tizen")
        || named("webos")
        || named("player")
    {
        return (other, Some(Tv), Some(App));
    }
    (None, None, None)
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
                platform: Some(Platform::Ios),
                device_kind: None,
                via: Some(PlayedVia::App),
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
            ("shuffle", json!([true])),
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
    fn a_number_too_large_to_read_is_told_by_its_field_and_the_file_reads_on() {
        let good = serde_json::to_string(&row()).unwrap();
        let huge = good.replace("\"ms_played\":221000", "\"ms_played\":1e400");
        assert_ne!(huge, good);
        let rows = read(format!("[{huge}, {good}]").as_bytes()).unwrap();
        assert_eq!(rows.rows.len(), 2);
        assert_eq!(rows.rows[0], Row::Wrong("ms_played"));
        assert!(matches!(rows.rows[1], Row::Music(_)));
    }

    #[test]
    fn a_new_field_that_begins_like_an_episodes_is_unknown_and_drops_no_row() {
        let mut new = row();
        new["episode_playback_setting"] = json!(false);
        new["audiobook_like_setting"] = json!("MARKED");
        let bytes = serde_json::to_vec(&json!([new])).unwrap();
        let rows = read(&bytes).unwrap();
        assert!(matches!(rows.rows[0], Row::Music(_)));
        assert_eq!(rows.unknown.len(), 2);
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
        assert_eq!(
            read(format!("[{good}] MARKED").as_bytes()).unwrap_err(),
            FileError::AfterRows
        );
    }

    #[test]
    fn a_platform_is_kept_as_words_for_its_system_its_device_and_how_it_played() {
        use DeviceKind::{Car, Computer, Console, Group, Phone, Speaker, Tablet, Tv, Watch};
        use Platform::{Android, Ios, Linux, Mac, Other, Windows};
        use PlayedVia::{App, Cast, Connect, Web};
        for (text, words) in [
            (
                "OS X 10.9.5 [x86 4]",
                (Some(Mac), Some(Computer), Some(App)),
            ),
            ("osx", (Some(Mac), Some(Computer), Some(App))),
            (
                "Windows 10 (10.0.19045; x64)",
                (Some(Windows), Some(Computer), Some(App)),
            ),
            ("Linux [x86-0]", (Some(Linux), Some(Computer), Some(App))),
            // A phone or a tablet is not guessed.
            (
                "Android OS 9 API 28 (Invented, Model)",
                (Some(Android), None, Some(App)),
            ),
            ("android", (Some(Android), None, Some(App))),
            ("ios", (Some(Ios), None, Some(App))),
            ("iOS 17.1 (iPhone15,2)", (Some(Ios), Some(Phone), Some(App))),
            ("iOS 12.1 (iPad7,5)", (Some(Ios), Some(Tablet), Some(App))),
            ("iOS 10.0 (Watch3,4)", (Some(Ios), Some(Watch), Some(App))),
            (
                "web_player osx 10.15.7;chrome 120.0",
                (Some(Mac), Some(Computer), Some(Web)),
            ),
            (
                "web_player windows 10;firefox 120.0",
                (Some(Windows), Some(Computer), Some(Web)),
            ),
            ("WebPlayer (websocket RFC6455)", (None, None, Some(Web))),
            ("web_player", (None, None, Some(Web))),
            ("cast", (None, None, Some(Cast))),
            ("cast_to_device", (None, None, Some(Cast))),
            (
                "Partner google cast_audio;Chromecast Audio;;",
                (None, Some(Speaker), Some(Cast)),
            ),
            (
                "Partner google cast_tv;Chromecast;;",
                (None, Some(Tv), Some(Cast)),
            ),
            (
                "Partner google cast_group;Google Cast Group;;",
                (None, Some(Group), Some(Cast)),
            ),
            (
                "Partner google cast;Google Home Mini;;",
                (None, Some(Speaker), Some(Cast)),
            ),
            (
                "Partner google cast;Google Nest Hub;;",
                (None, Some(Speaker), Some(Cast)),
            ),
            (
                "Partner invented avr;Model;;",
                (None, Some(Speaker), Some(Connect)),
            ),
            (
                "Partner amazon_salmon Amazon;Echo Dot;;",
                (None, Some(Speaker), Some(Connect)),
            ),
            (
                "Partner invented Microsoft;Xbox One;;",
                (Some(Other), Some(Console), Some(App)),
            ),
            (
                "Partner roku_tv Invented;TV;;",
                (Some(Other), Some(Tv), Some(App)),
            ),
            (
                "Partner invented Apple;Apple TV;;",
                (Some(Other), Some(Tv), Some(App)),
            ),
            ("Partner invented Invented;Car;;", (None, Some(Car), None)),
            ("Partner invented Invented;Model;;", (None, None, None)),
            // A device's own name never decides them.
            (
                "Android OS 9 (Invented Broadcast)",
                (Some(Android), None, Some(App)),
            ),
            ("iOS 17.1 (Webster's Phone)", (Some(Ios), None, Some(App))),
            ("webOS TV", (Some(Other), None, None)),
            ("something new", (Some(Other), None, None)),
            ("not_applicable", (None, None, None)),
            ("unknown", (None, None, None)),
            ("", (None, None, None)),
        ] {
            assert_eq!(device_words(text), words, "{text}");
        }
    }
}
