//! `ritornelctl spotify`: Spotify's extended streaming history, read where
//! it sits. See "Spotify streams" in docs/internals/history.md.
//!
//! `spotify read <archive>` reads the zip file Spotify sent, or the folder
//! it was unpacked to, keeps each row's kept fields, and says what it read.
//! It stores nothing and sends nothing.

mod archive;
mod rows;

pub(crate) use self::archive::Error as ArchiveError;
use self::archive::shown;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Write;
use std::path::Path;

use ritornel_client::contract::{DeviceKind, Platform, PlayedVia, SpotifyStream};
use serde_json::json;

use self::rows::{NotMusic, Row};
use crate::cli::{Error, SpotifyReadArgs};

/// Thirty seconds: the time played that Spotify itself counts a stream at,
/// which the counts of what was read are split by.
const THIRTY_SECONDS_MS: i64 = 30_000;

/// How many rows that would not read are named, at most.
const WRONG_NAMED: usize = 20;

/// What an archive held: every stream of music, in the order of its files
/// and rows, and the counts of what was read.
#[derive(Debug, Default)]
pub(crate) struct Archive {
    pub(crate) streams: Vec<SpotifyStream>,
    pub(crate) tally: Tally,
}

/// The word a tally gives a stream whose row did not say.
const UNKNOWN: &str = "unknown";

/// The counts of what an archive held. None holds a value from a row.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Tally {
    /// The files of the history read.
    pub(crate) files: u64,
    /// Every row of them.
    pub(crate) rows: u64,
    /// The streams of music.
    pub(crate) music: u64,
    /// The rows that are not music, by [`NotMusic`].
    pub(crate) not_music: [u64; NotMusic::ALL.len()],
    /// The rows with a field that would not read.
    pub(crate) wrong: u64,
    /// The first of those, by file, row from 1, and field.
    pub(crate) wrong_named: Vec<(String, usize, &'static str)>,
    /// The streams that repeat an earlier one: the same end, time played
    /// and track.
    pub(crate) repeats: u64,
    /// Of those, the ones that differ from it in another kept field.
    pub(crate) repeats_differing: u64,
    /// The streams that played thirty seconds or more.
    pub(crate) thirty: u64,
    /// Of those, the ones that repeat no earlier stream.
    pub(crate) thirty_distinct: u64,
    /// The first and the last stream's end.
    pub(crate) span: Option<(i64, i64)>,
    /// The streams by their platform's word, `unknown` when the row did
    /// not say.
    pub(crate) platforms: BTreeMap<&'static str, u64>,
    /// The streams by their kind of device's word, `unknown` likewise.
    pub(crate) devices: BTreeMap<&'static str, u64>,
    /// The streams by how they were played, `unknown` likewise.
    pub(crate) vias: BTreeMap<&'static str, u64>,
    /// The names of fields the reader does not know.
    pub(crate) unknown: BTreeSet<String>,
}

/// Reads an archive.
pub(crate) fn read(path: &Path) -> Result<Archive, archive::Error> {
    let mut read = Archive::default();
    let mut first: HashMap<(i64, i64, String), usize> = HashMap::new();
    archive::each_file(path, |name, bytes| {
        let rows =
            rows::read(bytes).map_err(|error| archive::Error::File(name.to_owned(), error))?;
        read.add(name, rows, &mut first);
        Ok(())
    })?;
    Ok(read)
}

impl Archive {
    fn add(
        &mut self,
        name: &str,
        file: rows::FileRows,
        first: &mut HashMap<(i64, i64, String), usize>,
    ) {
        let tally = &mut self.tally;
        tally.files += 1;
        tally.unknown.extend(file.unknown);
        for (at, row) in file.rows.into_iter().enumerate() {
            tally.rows += 1;
            let stream = match row {
                Row::Music(stream) => stream,
                Row::NotMusic(kind) => {
                    let at = NotMusic::ALL.iter().position(|each| *each == kind);
                    if let Some(count) = at.and_then(|at| tally.not_music.get_mut(at)) {
                        *count += 1;
                    }
                    continue;
                }
                Row::Wrong(field) => {
                    tally.wrong += 1;
                    if tally.wrong_named.len() < WRONG_NAMED {
                        tally.wrong_named.push((name.to_owned(), at + 1, field));
                    }
                    continue;
                }
            };
            tally.music += 1;
            let thirty = stream.ms_played >= THIRTY_SECONDS_MS;
            tally.thirty += u64::from(thirty);
            *tally
                .platforms
                .entry(stream.platform.map_or(UNKNOWN, Platform::as_str))
                .or_default() += 1;
            *tally
                .vias
                .entry(stream.via.map_or(UNKNOWN, PlayedVia::as_str))
                .or_default() += 1;
            *tally
                .devices
                .entry(stream.device_kind.map_or(UNKNOWN, DeviceKind::as_str))
                .or_default() += 1;
            tally.span = Some(match tally.span {
                None => (stream.ended_at, stream.ended_at),
                Some((from, to)) => (from.min(stream.ended_at), to.max(stream.ended_at)),
            });
            let signature = (stream.ended_at, stream.ms_played, stream.track_uri.clone());
            match first.get(&signature) {
                Some(&earlier) => {
                    tally.repeats += 1;
                    if self.streams.get(earlier) != Some(&stream) {
                        tally.repeats_differing += 1;
                    }
                }
                None => {
                    first.insert(signature, self.streams.len());
                    tally.thirty_distinct += u64::from(thirty);
                }
            }
            self.streams.push(stream);
        }
    }
}

/// `spotify read <archive>`.
pub(crate) fn read_command(args: &SpotifyReadArgs, out: &mut dyn Write) -> Result<(), Error> {
    let read = read(&args.archive).map_err(Error::Archive)?;
    let text = if args.json {
        let mut text = serde_json::to_string_pretty(&to_json(&read.tally))
            .map_err(|error| Error::Output(error.into()))?;
        text.push('\n');
        text
    } else {
        render(&read.tally)
    };
    out.write_all(text.as_bytes())
        .and_then(|()| out.flush())
        .map_err(Error::Output)
}

fn day(ms: i64) -> String {
    jiff::Timestamp::from_millisecond(ms)
        .map(|at| at.strftime("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

fn to_json(tally: &Tally) -> serde_json::Value {
    let not_music: serde_json::Map<String, serde_json::Value> = NotMusic::ALL
        .iter()
        .zip(tally.not_music)
        .map(|(kind, count)| (kind.as_str().to_owned(), json!(count)))
        .collect();
    let wrong: Vec<serde_json::Value> = tally
        .wrong_named
        .iter()
        .map(|(file, row, field)| json!({ "file": shown(file), "row": row, "field": field }))
        .collect();
    let unknown: Vec<String> = tally.unknown.iter().map(|name| shown(name)).collect();
    json!({
        "files": tally.files,
        "rows": tally.rows,
        "music": tally.music,
        "notMusic": not_music,
        "wrong": tally.wrong,
        "wrongRows": wrong,
        "repeats": tally.repeats,
        "repeatsDiffering": tally.repeats_differing,
        "thirtySeconds": tally.thirty,
        "thirtySecondsDistinct": tally.thirty_distinct,
        "first": tally.span.map(|(from, _)| day(from)),
        "last": tally.span.map(|(_, to)| day(to)),
        "platforms": tally.platforms,
        "deviceKinds": tally.devices,
        "playedVia": tally.vias,
        "unknownFields": unknown,
    })
}

fn render(tally: &Tally) -> String {
    let not_music: u64 = tally.not_music.iter().sum();
    let mut lines = vec![
        ("Files", tally.files.to_string()),
        ("Rows", tally.rows.to_string()),
        ("Streams of music", tally.music.to_string()),
    ];
    if let Some((from, to)) = tally.span {
        lines.push(("From", format!("{} to {}", day(from), day(to))));
    }
    lines.push((
        "Repeated",
        format!(
            "{}: the same end, time played and track as an earlier stream; {} of them differ in another field",
            tally.repeats, tally.repeats_differing
        ),
    ));
    lines.push((
        "30 s or more",
        format!(
            "{}, {} once each repeat is counted once",
            tally.thirty, tally.thirty_distinct
        ),
    ));
    let kinds: Vec<String> = NotMusic::ALL
        .iter()
        .zip(tally.not_music)
        .filter(|(_, count)| *count > 0)
        .map(|(kind, count)| format!("{count} {}", kind.as_str()))
        .collect();
    lines.push((
        "Not music",
        if kinds.is_empty() {
            "0".to_owned()
        } else {
            format!("{not_music}: {}", kinds.join(", "))
        },
    ));
    lines.push(("Would not read", tally.wrong.to_string()));
    for (label, counts) in [
        ("By platform", &tally.platforms),
        ("By device", &tally.devices),
        ("Played via", &tally.vias),
    ] {
        if !counts.is_empty() {
            let words: Vec<String> = counts
                .iter()
                .map(|(word, count)| format!("{word} {count}"))
                .collect();
            lines.push((label, words.join(", ")));
        }
    }
    if !tally.unknown.is_empty() {
        let names: Vec<String> = tally.unknown.iter().map(|name| shown(name)).collect();
        lines.push(("Fields not kept", names.join(", ")));
    }
    let mut text = crate::server::facts(&lines);
    for (file, row, field) in &tally.wrong_named {
        text.push_str(&format!(
            "  {}, row {row}: {field} would not read\n",
            shown(file)
        ));
    }
    if tally.wrong > tally.wrong_named.len() as u64 {
        text.push_str(&format!(
            "  and {} more\n",
            tally.wrong - tally.wrong_named.len() as u64
        ));
    }
    text.push_str("Nothing was stored or sent.\n");
    text
}

#[cfg(test)]
mod tests {

    use serde_json::{Value, json};

    use super::*;

    /// A folder of its own for a test, always new, removed when it ends.
    struct Scratch(tempfile::TempDir);

    impl Scratch {
        fn new(name: &str) -> Self {
            Self(
                tempfile::Builder::new()
                    .prefix(&format!("ritornelctl-spotify-{name}-"))
                    .tempdir()
                    .unwrap(),
            )
        }

        fn join(&self, name: &str) -> std::path::PathBuf {
            self.0.path().join(name)
        }
    }

    /// An invented stream, every field Spotify gives filled in, each that
    /// is never kept with a marked value.
    fn row(ts: &str, ms: i64, track: u32, platform: &str) -> Value {
        json!({
            "ts": ts,
            "username": "MARKED_USERNAME",
            "platform": format!("{platform} MARKED_DEVICE"),
            "ms_played": ms,
            "conn_country": "MARKED_COUNTRY",
            "ip_addr": "MARKED_IP",
            "ip_addr_decrypted": "MARKED_IP_DECRYPTED",
            "user_agent_decrypted": "MARKED_AGENT",
            "master_metadata_track_name": format!("Song {track}"),
            "master_metadata_album_artist_name": "Calla Brandt",
            "master_metadata_album_album_name": "Glasshouse",
            "spotify_track_uri": format!("spotify:track:{track:022}"),
            "episode_name": null,
            "episode_show_name": null,
            "spotify_episode_uri": null,
            "audiobook_title": null,
            "audiobook_uri": null,
            "audiobook_chapter_uri": null,
            "audiobook_chapter_title": null,
            "reason_start": "clickrow",
            "reason_end": "trackdone",
            "shuffle": false,
            "skipped": null,
            "offline": false,
            "offline_timestamp": 0,
            "incognito_mode": false,
            "MARKED_FIELD": "MARKED_VALUE",
        })
    }

    fn episode() -> Value {
        let mut episode = row("2019-03-15T22:00:00Z", 600_000, 9, "Android OS 9");
        episode["spotify_track_uri"] = Value::Null;
        episode["master_metadata_track_name"] = Value::Null;
        episode["episode_name"] = json!("MARKED_EPISODE");
        episode["episode_show_name"] = json!("MARKED_SHOW");
        episode["spotify_episode_uri"] = json!("spotify:episode:MARKED");
        episode
    }

    fn audiobook() -> Value {
        let mut book = row("2019-03-15T23:00:00Z", 900_000, 10, "iOS 12.1");
        book["spotify_track_uri"] = Value::Null;
        book["audiobook_title"] = json!("MARKED_BOOK");
        book["audiobook_uri"] = json!("spotify:audiobook:MARKED");
        book["audiobook_chapter_uri"] = json!("spotify:chapter:MARKED");
        book["audiobook_chapter_title"] = json!("MARKED_CHAPTER");
        book
    }

    /// An invented archive: two years in two files, a repeated row, a row
    /// repeated with another reason, a short stream, an episode, an
    /// audiobook, a file of video history, whose music video is read as a
    /// song is, and a file that is not the history.
    fn files() -> Vec<(&'static str, Vec<u8>)> {
        let first = row("2019-03-15T21:14:09Z", 221_000, 1, "iOS 12.1");
        let mut other_reason = first.clone();
        other_reason["reason_end"] = json!("endplay");
        let year_one = json!([
            first,
            first.clone(),
            other_reason,
            row("2019-03-15T21:14:21Z", 12_000, 2, "iOS 12.1"),
            episode(),
            audiobook(),
        ]);
        let year_two = json!([
            row("2020-01-02T08:00:00Z", 30_000, 3, "Partner google cast_tv"),
            row("2020-01-02T08:04:00Z", 240_000, 1, "OS X 10.15.7"),
        ]);
        vec![
            (
                "Spotify Extended Streaming History/Streaming_History_Audio_2019.json",
                serde_json::to_vec(&year_one).unwrap(),
            ),
            (
                "Spotify Extended Streaming History/Streaming_History_Audio_2020.json",
                serde_json::to_vec(&year_two).unwrap(),
            ),
            (
                "Spotify Extended Streaming History/Streaming_History_Video_2020.json",
                serde_json::to_vec(&json!([row("2020-01-03T08:00:00Z", 200_000, 5, "ios")]))
                    .unwrap(),
            ),
            (
                "Spotify Extended Streaming History/ReadMeFirst_ExtendedStreamingHistory.pdf",
                b"MARKED_PDF".to_vec(),
            ),
        ]
    }

    fn zip_of(path: &Path, files: &[(&str, Vec<u8>)]) {
        let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        for (name, bytes) in files {
            zip.start_file(
                *name,
                zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated),
            )
            .unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap();
    }

    fn folder_of(path: &Path, files: &[(&str, Vec<u8>)]) {
        for (name, bytes) in files {
            let file = path.join(name);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, bytes).unwrap();
        }
    }

    fn expected() -> Tally {
        Tally {
            files: 3,
            rows: 9,
            music: 7,
            not_music: [1, 1, 0, 0],
            wrong: 0,
            wrong_named: Vec::new(),
            repeats: 2,
            repeats_differing: 1,
            thirty: 6,
            thirty_distinct: 4,
            span: Some((1_552_684_449_000, 1_578_038_400_000)),
            platforms: BTreeMap::from([("ios", 5), ("mac", 1), ("unknown", 1)]),
            devices: BTreeMap::from([("computer", 1), ("tv", 1), ("unknown", 5)]),
            vias: BTreeMap::from([("app", 6), ("cast", 1)]),
            unknown: BTreeSet::from(["MARKED_FIELD".to_owned()]),
        }
    }

    fn command(archive: &Path, json: bool) -> String {
        let mut out = Vec::new();
        read_command(
            &SpotifyReadArgs {
                archive: archive.to_owned(),
                json,
            },
            &mut out,
        )
        .unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn a_zip_and_its_folder_read_the_same() {
        let scratch = Scratch::new("same");
        let zip = scratch.join("my_spotify_data.zip");
        zip_of(&zip, &files());
        let folder = scratch.join("unpacked");
        folder_of(&folder, &files());
        let from_zip = read(&zip).unwrap();
        let from_folder = read(&folder).unwrap();
        assert_eq!(from_zip.tally, expected());
        assert_eq!(from_folder.tally, expected());
        assert_eq!(from_zip.streams, from_folder.streams);
        assert_eq!(from_zip.streams.len(), 7);
    }

    #[test]
    fn no_marked_value_reaches_what_is_kept_or_printed() {
        let scratch = Scratch::new("marks");
        let zip = scratch.join("history.zip");
        zip_of(&zip, &files());
        let read = read(&zip).unwrap();
        let kept = serde_json::to_string(&read.streams).unwrap();
        assert!(!kept.contains("MARKED"), "{kept}");
        // Each kept record has the kept fields and no other, by a list
        // written here rather than taken from the type.
        let allowed = [
            "endedAt",
            "msPlayed",
            "trackUri",
            "title",
            "artist",
            "album",
            "reasonStart",
            "reasonEnd",
            "shuffle",
            "skipped",
            "offline",
            "offlineTimestamp",
            "incognito",
            "platform",
            "deviceKind",
            "via",
        ];
        for stream in &read.streams {
            let value = serde_json::to_value(stream).unwrap();
            for key in value.as_object().unwrap().keys() {
                assert!(allowed.contains(&key.as_str()), "{key}");
            }
        }
        for json in [false, true] {
            let printed = command(&zip, json);
            // The one mark that is printed is the name of a field the
            // reader does not know, which is a name and no value; and no
            // kept value is printed either.
            let without_name = printed.replace("MARKED_FIELD", "");
            for value in ["MARKED", "Calla Brandt", "Glasshouse", "Song ", "spotify:"] {
                assert!(!without_name.contains(value), "{value}: {printed}");
            }
        }
    }

    #[test]
    fn a_row_that_will_not_read_is_named_by_file_row_and_field_alone() {
        let scratch = Scratch::new("wrong");
        let wrong = row("MARKED_TIME", 1_000, 4, "ios");
        let file = serde_json::to_vec(&json!([
            row("2019-03-15T21:14:09Z", 221_000, 1, "ios"),
            wrong
        ]))
        .unwrap();
        let zip = scratch.join("history.zip");
        zip_of(&zip, &[("Streaming_History_Audio_2019.json", file)]);
        let printed = command(&zip, false);
        assert!(
            printed.contains("Streaming_History_Audio_2019.json, row 2: ts would not read"),
            "{printed}"
        );
        assert!(
            !printed.replace("MARKED_FIELD", "").contains("MARKED"),
            "{printed}"
        );
        let json: Value = serde_json::from_str(&command(&zip, true)).unwrap();
        assert_eq!(
            json["wrongRows"],
            json!([{ "file": "Streaming_History_Audio_2019.json", "row": 2, "field": "ts" }])
        );
        assert!(
            !json
                .to_string()
                .replace("MARKED_FIELD", "")
                .contains("MARKED")
        );

        // A file that breaks is named with the row it broke at, and none
        // of what the parser choked on.
        let broken = br#"[{"ts": "2019-03-15T21:14:09Z", "ip_addr": MARKED_BROKEN}]"#.to_vec();
        zip_of(&zip, &[("Streaming_History_Audio_2019.json", broken)]);
        let error = read(&zip).unwrap_err().to_string();
        assert_eq!(
            error,
            "Streaming_History_Audio_2019.json: row 1 is not a row of the history"
        );
    }

    #[test]
    fn a_damaged_file_of_the_history_is_said_and_never_passed_over() {
        let scratch = Scratch::new("damaged");
        let zip = scratch.join("history.zip");
        zip_of(&zip, &files());
        // Break the second file's own header, which the archive's list of
        // names does not depend on.
        let mut bytes = std::fs::read(&zip).unwrap();
        let headers: Vec<usize> = bytes
            .windows(4)
            .enumerate()
            .filter(|(_, window)| *window == b"PK\x03\x04")
            .map(|(at, _)| at)
            .collect();
        bytes[headers[1]] = b'X';
        std::fs::write(&zip, bytes).unwrap();
        assert_eq!(
            read(&zip).unwrap_err().to_string(),
            "Streaming_History_Audio_2020.json: it cannot be read"
        );
    }

    #[test]
    fn the_account_data_and_anything_else_are_refused_by_name() {
        let scratch = Scratch::new("refused");
        let account = scratch.join("account.zip");
        zip_of(
            &account,
            &[(
                "Spotify Account Data/StreamingHistory_music_0.json",
                br#"[{"endTime": "2019-03-15 21:14", "artistName": "MARKED"}]"#.to_vec(),
            )],
        );
        let error = read(&account).unwrap_err().to_string();
        assert!(
            error.contains("ask Spotify for the extended streaming history"),
            "{error}"
        );
        assert!(!error.contains("MARKED"));
        let other = scratch.join("other.txt");
        std::fs::write(&other, "MARKED").unwrap();
        assert_eq!(
            read(&other).unwrap_err().to_string(),
            "that is neither a folder nor a zip file"
        );
        let empty = scratch.join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        assert!(matches!(read(&empty), Err(archive::Error::NoHistory)));
    }

    #[test]
    fn what_was_read_is_printed_as_counts() {
        let scratch = Scratch::new("printed");
        let zip = scratch.join("history.zip");
        zip_of(&zip, &files());
        let printed = command(&zip, false);
        for line in [
            "Files             3\n",
            "Rows              9\n",
            "Streams of music  7\n",
            "From              2019-03-15 to 2020-01-03\n",
            "Not music         2: 1 episode, 1 audiobook\n",
            "By platform       ios 5, mac 1, unknown 1\n",
            "By device         computer 1, tv 1, unknown 5\n",
            "Played via        app 6, cast 1\n",
            "Nothing was stored or sent.\n",
        ] {
            assert!(printed.contains(line), "{line}{printed}");
        }
        let json: Value = serde_json::from_str(&command(&zip, true)).unwrap();
        assert_eq!(json["rows"], 9);
        assert_eq!(json["repeats"], 2);
        assert_eq!(json["thirtySecondsDistinct"], 4);
        assert_eq!(json["notMusic"]["episode"], 1);
        assert_eq!(json["platforms"]["ios"], 5);
        assert_eq!(json["deviceKinds"]["tv"], 1);
        assert_eq!(json["playedVia"]["cast"], 1);
    }
}
