//! The real `ritornelctl` scanning a folder on the real `ritornel-server`,
//! and a scan that the server's death cuts off being taken up by the next
//! run. See "Jobs and outbox" in docs/internals/server.md.
//!
//! As in `status.rs`, `ritornel-server` is taken from beside `ritornelctl`,
//! where a build of the workspace puts it, and nothing here sleeps: each
//! wait is for a process to print a line or to end, and is bounded by
//! [`WAIT`].

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::support::{ABORTED, Scratch, Server, json, logs, ok, ritornelctl};

fn scratch() -> Scratch {
    Scratch::new("ritornelctl-scan-")
}

/// A folder of the audio fixtures: three files that are audio, and one
/// that is not.
fn music(scratch: &Scratch) -> PathBuf {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/audio");
    // The path a scan is given is made real, so the test names it so.
    let folder = scratch.0.canonicalize().unwrap().join("music");
    for (to, fixture) in [
        ("Tin Weather/02.flac", "a.flac"),
        ("Tin Weather/copy.mp3", "a.mp3"),
        ("loose.ogg", "a.retagged.ogg"),
        ("broken.flac", "not-audio.flac"),
    ] {
        let to = folder.join(to);
        std::fs::create_dir_all(to.parent().unwrap()).unwrap();
        std::fs::copy(fixtures.join(fixture), to).unwrap();
    }
    folder
}

#[test]
fn scan_adds_the_folder_scans_it_and_the_stats_say_what_came_out() {
    let scratch = scratch();
    let (data_dir, music) = (scratch.data_dir(), music(&scratch));
    let folder = music.to_str().unwrap();
    let server = Server::start(&data_dir, &[]);

    // With no library there is nothing to scan into.
    let (code, _, stderr) = ritornelctl(&data_dir, &["scan", folder]);
    assert_eq!(code, Some(1));
    assert!(stderr.contains("this server holds no library"), "{stderr}");
    ok(&data_dir, &["library", "create"]);
    assert_eq!(ok(&data_dir, &["jobs"]), "No jobs.\n");

    let (code, stdout, stderr) = ritornelctl(&data_dir, &["scan", folder]);
    assert_eq!((code, stderr.as_str()), (Some(0), ""), "{stdout}");
    assert!(stdout.contains("succeeded: 4 of 4 files read"), "{stdout}");
    for line in [
        "State             succeeded\n",
        "Files found       4\n",
        "New               4\n",
        "Unreadable        1\n",
        "Bindings written  3\n",
        "Recordings made   2\n",
        "Releases made     1\n",
        "Could not be read:\n  broken.flac  ",
    ] {
        assert!(stdout.contains(line), "{line}: {stdout}");
    }

    let stdout = ok(&data_dir, &["library", "stats"]);
    for line in [
        "Library revision  4\n",
        "Files             4\n",
        "Unreadable        1 (25.0%)\n",
        "With an MBID      2 (50.0%)\n",
        "Artists           2\n",
        "Release groups    1\n",
        "Releases          1\n",
        "Recordings        2\n",
        "Track appearances 3\n",
        "Loose tracks      1\n",
        "Need review       0 (0.0%)\n",
        "Groups with kind  1 (100.0%)\n",
        "  from a tag      1 (100.0%)\n",
        "  new             2 (66.7%)\n",
        "  tags_duration   1 (33.3%)\n",
        "  flac            1 (33.3%)\n",
        "  ep              1 (100.0%)\n",
        "       1  unsupported: ",
    ] {
        assert!(stdout.contains(line), "{line}: {stdout}");
    }

    // The folder is a source now, so a second scan adds nothing: it finds
    // every file unchanged, and the library stays at its revision.
    let again = json(&data_dir, &["scan", folder]);
    assert_eq!(again["job"]["state"], "succeeded");
    assert_eq!(again["job"]["scan"]["unchanged"], 4);
    assert_eq!(json(&data_dir, &["library", "stats"])["libraryRevision"], 4);
    // The server watches its sources, and the system may tell it of a
    // change in the folder after the scans, so it may have rescanned it as
    // well: every job listed is a scan, and the second scan is one.
    let listed = json(&data_dir, &["jobs"]);
    let jobs = listed["jobs"].as_array().unwrap();
    assert!(jobs.len() >= 2, "{listed}");
    assert!(jobs.iter().all(|job| job["kind"] == "scan"), "{listed}");
    assert!(
        jobs.iter().any(|job| job["jobId"] == again["job"]["jobId"]),
        "{listed}"
    );
    let one = json(
        &data_dir,
        &["jobs", again["job"]["jobId"].as_str().unwrap()],
    );
    assert_eq!(one["job"], again["job"]);

    // `--no-wait` returns the job at once, and it is the server's to
    // finish: stopping the server waits for the step in hand and no more.
    let started = json(&data_dir, &["scan", folder, "--no-wait"]);
    assert!(started["job"]["jobId"].is_string());
    // Nothing was written under the folder.
    assert_eq!(std::fs::read_dir(&music).unwrap().count(), 3);
    server.stop();
    assert!(!data_dir.join("library.db-wal").exists());
}

#[test]
fn a_scan_cut_off_when_the_server_dies_is_taken_up_by_the_next_run_and_finishes() {
    let scratch = scratch();
    let (data_dir, music) = (scratch.data_dir(), music(&scratch));
    let folder = music.to_str().unwrap();

    // A server that reads one file a step, and dies straight after the
    // library commit of the scan's second step, before that step's
    // checkpoint is saved: a kill at the worst moment.
    let dying = Server::start(
        &data_dir,
        &[
            ("RITORNEL_TEST_SCAN_BATCH_FILES", "1"),
            ("RITORNEL_TEST_FAIL_AFTER_SCAN_STEP", "2"),
        ],
    );
    ok(&data_dir, &["library", "create"]);
    let started = json(&data_dir, &["scan", folder, "--no-wait"]);
    let job_id = started["job"]["jobId"].as_str().unwrap().to_owned();
    // It was killed by a signal, not stopped: no exit code, and it left
    // `server.json` behind.
    let ended = dying.ended();
    assert_eq!(ended.code(), ABORTED, "the server ended with {ended}");
    assert!(data_dir.join("server.json").exists());
    assert_eq!(ritornelctl(&data_dir, &["status"]).0, Some(4));

    // The next run finds the job claimed by a run that is gone, queues it
    // again, and carries it on from what it had committed. Asking for a
    // scan of the same folder waits for whatever scan there is.
    let server = Server::start(&data_dir, &[]);
    let (code, _, stderr) = ritornelctl(&data_dir, &["scan", folder]);
    assert_eq!((code, stderr.as_str()), (Some(0), ""));
    assert!(logs(&data_dir).contains("jobs that an earlier run left are queued again"));

    let resumed = json(&data_dir, &["jobs", &job_id]);
    assert_eq!(resumed["job"]["state"], "succeeded");
    // Two files had been committed when the server died. The job found
    // them unchanged, and read only the other two.
    assert_eq!(resumed["job"]["scan"]["files"], 4);
    assert_eq!(resumed["job"]["scan"]["unchanged"], 2);
    assert_eq!(resumed["job"]["scan"]["unreadable"], 1);
    let stats = json(&data_dir, &["library", "stats"]);
    assert_eq!(stats["files"], 4);
    assert_eq!(stats["unresolved"], 0);
    assert_eq!(
        (
            &stats["recordings"],
            &stats["releases"],
            &stats["appearances"]
        ),
        (&Value::from(2), &Value::from(1), &Value::from(3))
    );
    // No step was done twice. The run that died wrote two files in two
    // steps, and this one wrote the other two in one and then resolved the
    // library: four library writes by the job, after the library's creation
    // and its source.
    assert_eq!(stats["libraryRevision"], 6);
    server.stop();
}

/// The bytes of the library file and of its write-ahead log, which is
/// where a running server's writes are until a checkpoint.
fn library_bytes(data_dir: &Path) -> (Vec<u8>, Option<Vec<u8>>) {
    (
        std::fs::read(data_dir.join("library.db")).unwrap(),
        std::fs::read(data_dir.join("library.db-wal")).ok(),
    )
}

#[test]
fn a_dry_run_after_a_scan_changes_nothing_and_leaves_the_library_file_as_it_was() {
    let scratch = scratch();
    let (data_dir, music) = (scratch.data_dir(), music(&scratch));
    let server = Server::start(&data_dir, &[]);
    ok(&data_dir, &["library", "create"]);
    ok(&data_dir, &["scan", music.to_str().unwrap()]);
    let before = library_bytes(&data_dir);

    let run = json(
        &data_dir,
        &["identity", "re-resolve", "--dry-run", "--detail"],
    );
    assert_eq!(run["policy"]["exact"], true);
    assert_eq!(run["policy"]["resolverVersion"], 3);
    assert_eq!(run["wouldWrite"], false);
    assert_eq!(
        (&run["items"], &run["unchanged"]),
        (&Value::from(4), &Value::from(4))
    );
    assert_eq!(run["wouldChange"], 0);
    assert_eq!(run["changes"], Value::Array(Vec::new()));
    let stdout = ok(&data_dir, &["identity", "re-resolve", "--dry-run"]);
    for line in [
        "Policy            policy 3\n",
        "Would change      0 (0 to another recording)\n",
        "Would write       no\n",
    ] {
        assert!(stdout.contains(line), "{line}: {stdout}");
    }
    // Other values are asked about, and nothing is applied.
    let variant = json(
        &data_dir,
        &[
            "identity",
            "re-resolve",
            "--dry-run",
            "--fold-accents",
            "--duration-grace-ms",
            "3000",
        ],
    );
    assert_eq!(variant["policy"]["exact"], false);
    assert_eq!(variant["policy"]["durationGraceMs"], 3000);
    assert_eq!(variant["wouldChange"], 0);
    let review = json(&data_dir, &["identity", "review"]);
    assert_eq!(review["reviews"], Value::Array(Vec::new()));
    assert_eq!(
        ok(&data_dir, &["identity", "review"]),
        "0 marked for review.\n"
    );

    // Without --dry-run, and with a value out of range, nothing is asked.
    for (line, message) in [
        (
            &["identity", "re-resolve"][..],
            "re-resolve only runs with --dry-run: a scan resolves the library at its end",
        ),
        (
            &["identity", "re-resolve", "--dry-run", "--bind-max", "2"],
            "--bind-max needs a number from 0 to 1",
        ),
    ] {
        let (code, stdout, stderr) = ritornelctl(&data_dir, line);
        assert_eq!((code, stdout.as_str()), (Some(2), ""), "{line:?}");
        assert_eq!(stderr, format!("ritornelctl: {message}\n"));
    }

    // The library file and its log are byte for byte what they were.
    assert!(
        before == library_bytes(&data_dir),
        "the dry run changed library.db"
    );
    server.stop();
}
