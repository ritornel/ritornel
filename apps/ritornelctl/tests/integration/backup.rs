//! The real `ritornelctl backup` and `export` on the real
//! `ritornel-server`. A backup taken while the server plays is restored
//! into another data directory, where the history it holds is the history
//! a rebuild makes; with no backup folder named there is no backup,
//! nothing is written beside the data in its place, and the status says
//! so. An export is read back as plain JSON Lines and CSV. A listen that
//! cannot be saved stops the record, and the status says so while the
//! music goes on. See "Backups and pre-migration snapshots" in
//! docs/internals/server.md and "Export" in
//! docs/internals/history.md.
//!
//! As in `play.rs`, `ritornel-server` is taken from beside `ritornelctl`,
//! and nothing here sleeps: each wait is for a process to print a line or
//! to end, or for an answer from the server, and is bounded.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::support::{Scratch, Server, json, ok, ritornelctl, until, write_wav};

fn scratch() -> Scratch {
    Scratch::new("ritornelctl-backup-")
}

/// A folder of two invented tracks of these lengths in seconds.
fn album(scratch: &Scratch, name: &str, seconds: [u32; 2]) -> PathBuf {
    let folder = scratch.0.canonicalize().unwrap().join("music").join(name);
    std::fs::create_dir_all(&folder).unwrap();
    let names = [("01 First.wav", 440.0), ("02 Second.wav", 660.0)];
    for ((name, hz), seconds) in names.into_iter().zip(seconds) {
        write_wav(&folder.join(name), seconds, hz);
    }
    folder
}

fn start(data_dir: &Path, variables: &[(&str, &str)]) -> Server {
    Server::start_null_output(data_dir, variables)
}

#[test]
fn with_no_backup_folder_there_is_no_backup_and_nothing_is_written_beside_the_data() {
    let scratch = scratch();
    let data_dir = scratch.data_dir();
    let server = start(&data_dir, &[]);
    ok(&data_dir, &["library", "create"]);

    let status = json(&data_dir, &["status"]);
    assert_eq!(status["backup"], serde_json::json!({}), "{status}");
    let stdout = ok(&data_dir, &["status"]);
    assert!(
        stdout.contains("Backup            none: no backup folder is named"),
        "{stdout}"
    );

    let (code, stdout, stderr) = ritornelctl(&data_dir, &["backup"]);
    assert_eq!((code, stdout.as_str()), (Some(1), ""));
    assert!(stderr.contains("no backup folder is named"), "{stderr}");
    server.stop();

    // Nothing was written in its place: the data directory holds what it
    // held, and no backups.
    assert!(!data_dir.join("backups").exists());
    let mut names: Vec<_> = std::fs::read_dir(&data_dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| !name.starts_with("server.") && !name.starts_with("library."))
        .collect();
    names.sort();
    assert_eq!(names, ["cache", "credential", "logs"]);
}

#[test]
fn a_backup_taken_while_the_server_plays_is_restored_elsewhere_with_its_history_whole() {
    let scratch = scratch();
    let data_dir = scratch.data_dir();
    let place = scratch.0.join("backups elsewhere");
    std::fs::create_dir_all(&place).unwrap();
    let short = album(&scratch, "Short Album", [40, 40]);
    let long = album(&scratch, "Long Album", [600, 600]);

    // Twenty times real time: the short album is over in four seconds and
    // the long one plays on for a minute.
    let server = start(
        &data_dir,
        &[
            ("RITORNEL_TEST_NULL_OUTPUT_SPEED", "20"),
            ("RITORNEL_BACKUP_DIR", place.to_str().unwrap()),
        ],
    );
    ok(&data_dir, &["library", "create"]);
    for folder in [&short, &long] {
        ok(&data_dir, &["scan", folder.to_str().unwrap()]);
    }
    let status = json(&data_dir, &["status"]);
    assert_eq!(status["backup"]["place"], place.to_str().unwrap());
    assert!(status["backup"].get("last").is_none(), "{status}");

    let (code, _, stderr) = ritornelctl(&data_dir, &["play", short.to_str().unwrap(), "--follow"]);
    assert_eq!((code, stderr.as_str()), (Some(0), ""));
    until(&data_dir, &["history"], |recent| recent["undelivered"] == 0);

    // The long album plays, and the backup is taken while it does.
    ok(&data_dir, &["play", long.to_str().unwrap()]);
    until(&data_dir, &["now"], |now| {
        now["status"] == "playing" && now["positionMs"].as_u64() > Some(0)
    });
    let backed_up = json(&data_dir, &["backup"]);
    let now = json(&data_dir, &["now"]);
    assert_eq!(now["status"], "playing", "playback goes on: {now}");
    let backup = PathBuf::from(backed_up["backup"]["path"].as_str().unwrap());
    assert_eq!(backup.parent(), Some(place.as_path()));
    let status = json(&data_dir, &["status"]);
    assert_eq!(
        status["backup"]["last"]["path"],
        backed_up["backup"]["path"]
    );
    assert_eq!(status["backup"]["last"]["reason"], "on_demand");
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(backup.join("backup.json")).unwrap()).unwrap();
    assert_eq!(
        manifest["files"],
        serde_json::json!(["server.db", "library.db"])
    );
    assert_eq!(manifest["libraryId"], status["library"]["libraryId"]);
    let history = json(&data_dir, &["history"]);
    let played = history["plays"].as_array().unwrap().len();
    assert_eq!(played, 2);
    let before_backup: Vec<Value> = history["plays"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|play| play["listens"].as_array().unwrap())
        .map(|listen| listen["listenId"].clone())
        .collect();
    server.stop();
    // The backup is not beside the data.
    assert!(!data_dir.join("backups").exists());

    // Another machine: an empty data directory with the backup's two
    // databases put in it.
    let restored = scratch.0.join("restored");
    std::fs::create_dir_all(&restored).unwrap();
    for name in ["server.db", "library.db"] {
        std::fs::copy(backup.join(name), restored.join(name)).unwrap();
    }
    let server = start(&restored, &[]);
    let status = json(&restored, &["status"]);
    assert_eq!(status["serverId"], manifest["serverId"]);
    assert_eq!(status["library"]["libraryId"], manifest["libraryId"]);
    // The listen that was playing when the backup was taken was in
    // server.db, and arrives too. Then the history the server kept is the
    // one a rebuild would make.
    let recent = until(&restored, &["history"], |recent| recent["undelivered"] == 0);
    let plays = recent["plays"].as_array().unwrap();
    assert_eq!(plays.len(), played + 1, "{recent}");
    // Newest first: the long album's first track, which was playing, and
    // which the backup's server.db still held as a listen in progress.
    let [listen] = plays[0]["listens"].as_array().unwrap().as_slice() else {
        panic!("one listen: {recent}");
    };
    assert_eq!(plays[0]["durationMs"], 600_000);
    assert_eq!(listen["endReason"], "crash");
    assert!(
        !before_backup.contains(&listen["listenId"]),
        "it was not in the library before: {recent}"
    );
    let rebuilt = json(&restored, &["history", "rebuild", "--dry-run"]);
    assert_eq!(rebuilt["changed"].as_u64(), Some(0), "{rebuilt}");
    assert_eq!(rebuilt["before"], rebuilt["after"]);
    server.stop();
    // Nothing needed migrating, so nothing was copied beside it.
    assert!(!restored.join("backups").exists());
}

#[test]
fn an_export_is_read_back_without_ritornel_and_holds_a_skipped_listen_with_where_it_was_played_from()
 {
    let scratch = scratch();
    let data_dir = scratch.data_dir();
    let album = album(&scratch, "Long Album", [600, 600]);
    let out = scratch.0.join("exports");
    std::fs::create_dir_all(&out).unwrap();
    let server = start(&data_dir, &[("RITORNEL_TEST_NULL_OUTPUT_SPEED", "20")]);
    ok(&data_dir, &["library", "create"]);
    let folder = album.to_str().unwrap();
    ok(&data_dir, &["scan", folder]);

    // The first track is skipped a little way in, and the second stopped.
    ok(&data_dir, &["play", folder]);
    until(&data_dir, &["now"], |now| {
        now["status"] == "playing" && now["positionMs"].as_u64() > Some(0)
    });
    ok(&data_dir, &["next"]);
    // A track stopped before any of it is heard leaves no listen: the
    // second is stopped once some of it has played.
    until(&data_dir, &["now"], |now| {
        now["index"] == 1 && now["positionMs"].as_u64() > Some(0)
    });
    ok(&data_dir, &["stop"]);
    until(&data_dir, &["history"], |recent| {
        recent["undelivered"] == 0 && recent["plays"].as_array().unwrap().len() == 2
    });

    let exported = json(&data_dir, &["export", out.to_str().unwrap()]);
    assert_eq!(exported["observations"], 2, "{exported}");
    let path = PathBuf::from(exported["path"].as_str().unwrap());
    assert_eq!(path.parent(), Some(out.as_path()));
    server.stop();

    // A listening history is its owner's alone: a file's mode is the
    // system's to say.
    #[cfg(unix)]
    for file in std::fs::read_dir(&path).unwrap() {
        use std::os::unix::fs::PermissionsExt;
        let file = file.unwrap();
        let mode = file.metadata().unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "{:?}", file.file_name());
    }

    // From here on, only what is in the folder.
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(path.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["format"], "ritornel-record");
    let lines: Vec<Value> = std::fs::read_to_string(path.join("observations.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let skipped = lines
        .iter()
        .find(|line| line["listen"]["endReason"] == "next")
        .unwrap();
    assert_eq!(skipped["kind"], "listen");
    assert_eq!(skipped["listen"]["startReason"], "user_pick");
    assert_eq!(
        skipped["listen"]["playedFrom"]["kind"],
        skipped["record"]["contextKind"]
    );
    assert!(skipped["listen"]["msPlayed"].as_u64() > Some(0));
    assert!(
        skipped["item"]["key"]
            .as_str()
            .unwrap()
            .ends_with("01 First.wav")
    );
    let plays = std::fs::read_to_string(path.join("plays.csv")).unwrap();
    let mut rows = plays.lines();
    assert!(
        rows.next()
            .unwrap()
            .starts_with("play,listener,played_at_utc,")
    );
    assert_eq!(rows.count(), 2);
}

#[test]
fn while_a_listen_cannot_be_saved_playback_goes_on_and_the_status_says_so_and_since_when() {
    let scratch = scratch();
    let data_dir = scratch.data_dir();
    let album = album(&scratch, "Long Album", [600, 600]);
    // Past a minute into a track, every save of its listen fails, as a full
    // disk's would. It plays in real time, so that the next track's first
    // minute, while its saves work again, outlasts any wait for the status
    // to say so.
    let server = start(
        &data_dir,
        &[
            ("RITORNEL_TEST_NULL_OUTPUT_SPEED", "1"),
            ("RITORNEL_TEST_FAIL_SAVES_AFTER_MS", "60000"),
        ],
    );
    ok(&data_dir, &["library", "create"]);
    let folder = album.to_str().unwrap();
    ok(&data_dir, &["scan", folder]);
    let status = json(&data_dir, &["status"]);
    assert_eq!(status["record"]["problems"], serde_json::json!([]));
    let stdout = ok(&data_dir, &["status"]);
    assert!(stdout.contains("Record            advancing"), "{stdout}");

    ok(&data_dir, &["play", folder]);
    // A seek is saved at once: there, the save fails.
    ok(&data_dir, &["seek", "1:01"]);
    let status = until(&data_dir, &["status"], |status| {
        !status["record"]["problems"].as_array().unwrap().is_empty()
    });
    let problem = &status["record"]["problems"][0];
    assert_eq!(problem["kind"], "listen_unsaved", "{status}");
    assert_eq!(problem["dismissable"], false);
    assert!(problem["since"].as_i64() > Some(0));
    // The music goes on.
    let before = json(&data_dir, &["now"])["positionMs"].as_u64().unwrap();
    let now = until(&data_dir, &["now"], |now| {
        now["positionMs"].as_u64().unwrap() > before
    });
    assert_eq!(now["status"], "playing");
    let stdout = ok(&data_dir, &["status"]);
    assert!(
        stdout.contains("Record            The listen playing can’t be saved, since "),
        "{stdout}"
    );
    // Dismissing sets aside only scrobbles: this stays until put right.
    ok(&data_dir, &["status", "--dismiss"]);
    let status = json(&data_dir, &["status"]);
    assert_eq!(status["record"]["problems"][0]["kind"], "listen_unsaved");

    // Stopped, the next track's first seconds save again, and the sign
    // goes.
    ok(&data_dir, &["next"]);
    until(&data_dir, &["status"], |status| {
        status["record"]["problems"] == serde_json::json!([])
    });
    server.stop();
}
