//! The real `ritornelctl` playing a folder on the real `ritornel-server`
//! through the null output, which makes no sound, and the server killed in
//! the middle of a track: on the next run the queue and the position are
//! back, nothing plays, the listen is closed as a crash with at most five
//! seconds lost, and it reaches history once, however often it is sent.
//! See "Recording a listen" in docs/internals/playback.md.
//!
//! As in `scan.rs`, `ritornel-server` is taken from beside `ritornelctl`,
//! and nothing here sleeps: each wait is for a process to print a line or
//! to end, or for an answer from the server, and is bounded.

use std::path::{Path, PathBuf};

use super::support::{
    ABORTED, Scratch, Server, WAIT, json, lines, null_output_command, ok, rest, ritornelctl, until,
    write_wav,
};

/// How long a track of the invented album is.
const TRACK_SECONDS: u32 = 40;

/// Where the dying server dies: past this position in the track.
const DIES_AT_MS: u64 = 7_000;

/// How often a listen is saved while it plays.
const CHECKPOINT_MS: u64 = 5_000;

fn scratch() -> Scratch {
    Scratch::new("ritornelctl-play-")
}

/// A folder of two invented tracks: forty seconds of tone each, as
/// 16-bit WAV at 8 kHz, written here so that no fixture has to be that
/// long.
fn music(scratch: &Scratch) -> PathBuf {
    album(scratch, [TRACK_SECONDS, TRACK_SECONDS])
}

/// Two invented tracks of these lengths in seconds.
fn album(scratch: &Scratch, seconds: [u32; 2]) -> PathBuf {
    let folder = scratch
        .0
        .canonicalize()
        .unwrap()
        .join("music/Invented Album");
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

/// Runs a server that is to die before or just after it is ready, and
/// waits for it to.
fn run_until_it_dies(data_dir: &Path, variables: &[(&str, &str)]) -> Option<i32> {
    let mut child = null_output_command(data_dir, variables).spawn().unwrap();
    let stdout = lines(child.stdout.take().unwrap());
    if rest(&stdout).is_none() {
        let _ = child.kill();
        let _ = child.wait();
        panic!("the server did not die within {WAIT:?}");
    }
    child.wait().unwrap().code()
}

/// A whole-hour zone in which it is now between noon and one, as `TZ`
/// names it: a server put there has eleven hours before its "today" ends.
fn a_zone_at_noon() -> String {
    let hour = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        / 3600
        % 24;
    // From UTC-11 to UTC+12; the Etc names count the other way.
    match 12 - i64::try_from(hour).unwrap() {
        0 => "Etc/UTC".to_owned(),
        east if east > 0 => format!("Etc/GMT-{east}"),
        west => format!("Etc/GMT+{}", -west),
    }
}

#[test]
fn ritornelctl_play_plays_a_folder_start_to_finish_and_every_listen_reaches_history() {
    let scratch = scratch();
    let (data_dir, music) = (scratch.data_dir(), music(&scratch));
    let folder = music.to_str().unwrap();
    // Ten times real time: each track lasts four seconds, through several
    // of the looks `play --follow` takes every half second, so a slow
    // machine misses neither. The server's zone is one far from midnight,
    // so that "today" cannot turn between the plays and the question of
    // what was played on it.
    let zone = a_zone_at_noon();
    let server = start(
        &data_dir,
        &[("RITORNEL_TEST_NULL_OUTPUT_SPEED", "10"), ("TZ", &zone)],
    );
    ok(&data_dir, &["library", "create"]);
    ok(&data_dir, &["scan", folder]);

    let (code, stdout, stderr) = ritornelctl(&data_dir, &["play", folder, "--follow"]);
    assert_eq!((code, stderr.as_str()), (Some(0), ""));
    assert!(stdout.contains("  1/2  "), "{stdout}");
    assert!(stdout.contains("  2/2  "), "{stdout}");
    assert!(stdout.contains("Status            Stopped"), "{stdout}");

    let recent = until(&data_dir, &["history"], |recent| recent["undelivered"] == 0);
    let plays = recent["plays"].as_array().unwrap();
    assert_eq!(plays.len(), 2);
    for play in plays {
        assert_eq!(play["msPlayed"], u64::from(TRACK_SECONDS) * 1000);
        assert_eq!(play["counts"], true);
        assert_eq!(play["outcome"], "listened");
        assert_eq!(play["clockBasis"], "hub_age");
        assert_eq!(play["zone"], zone.as_str());
        assert_eq!(play["listens"][0]["endReason"], "finished");
    }
    // Newest first: the second track began on its own when the first
    // ended.
    assert_eq!(plays[1]["listens"][0]["startReason"], "user_pick");
    assert_eq!(plays[0]["listens"][0]["startReason"], "auto_advance");
    assert!(plays[0]["at"].as_i64() > plays[1]["at"].as_i64());
    // "On this day" has both, under today.
    let today = json(&data_dir, &["history", "on-this-day"]);
    assert_eq!(today["plays"].as_array().unwrap().len(), 2);
    // A rebuild finds nothing to change.
    let rebuilt = json(&data_dir, &["history", "rebuild"]);
    assert_eq!(
        (rebuilt["changed"].as_u64(), rebuilt["after"].as_u64()),
        (Some(0), Some(2))
    );
    server.stop();
}

#[test]
fn a_server_killed_mid_track_comes_back_paused_and_its_listen_reaches_history_once_as_a_crash() {
    let scratch = scratch();
    let (data_dir, music) = (scratch.data_dir(), music(&scratch));
    let folder = music.to_str().unwrap();

    // At real-time speed, so that the five-second checkpoints are what
    // they are in use. The server dies seven seconds into the first track,
    // two after its last checkpoint, saving nothing more.
    let dies_at = DIES_AT_MS.to_string();
    let dying = start(
        &data_dir,
        &[("RITORNEL_TEST_FAIL_AT_POSITION_MS", dies_at.as_str())],
    );
    ok(&data_dir, &["library", "create"]);
    ok(&data_dir, &["scan", folder]);
    ok(&data_dir, &["play", folder]);
    let ended = dying.ended();
    assert_eq!(ended.code(), ABORTED, "the server ended with {ended}");

    // The next run closes the listen as a crash, and dies itself just
    // after the library has taken it in and before server.db lets it go.
    assert_eq!(
        run_until_it_dies(&data_dir, &[("RITORNEL_TEST_FAIL_AFTER_INGEST", "1")]),
        ABORTED
    );

    // The run after that is a normal one. At twice real time, the rest of
    // the first track lasts long after the test stops it, however slowly
    // the stop comes.
    let server = start(&data_dir, &[("RITORNEL_TEST_NULL_OUTPUT_SPEED", "2")]);
    // The queue and the position are back, and nothing plays.
    let now = json(&data_dir, &["now"]);
    assert_eq!(now["status"], "paused");
    assert_eq!(
        (now["index"].as_u64(), now["queueLength"].as_u64()),
        (Some(0), Some(2))
    );
    let position = now["positionMs"].as_u64().unwrap();
    assert!(
        (CHECKPOINT_MS..DIES_AT_MS).contains(&position),
        "the position saved with the last checkpoint: {position}"
    );
    assert_eq!(
        json(&data_dir, &["now"])["positionMs"].as_u64(),
        Some(position)
    );

    // The crash's listen was sent again by this run, and history has it
    // once: one play, one listen, closed as a crash with what its last
    // checkpoint saved.
    let recent = until(&data_dir, &["history"], |recent| recent["undelivered"] == 0);
    let [play] = recent["plays"].as_array().unwrap().as_slice() else {
        panic!("one play: {recent}");
    };
    let [listen] = play["listens"].as_array().unwrap().as_slice() else {
        panic!("one listen: {play}");
    };
    assert_eq!(listen["endReason"], "crash");
    // Sent by the next run on the same boot of the machine, it is still
    // dated by its age, where the kernel's boot ID can be read.
    let expected_basis = if cfg!(any(target_os = "linux", target_os = "macos", windows)) {
        "hub_age"
    } else {
        "server_wall"
    };
    assert_eq!(play["clockBasis"], expected_basis);
    let saved = listen["msPlayed"].as_u64().unwrap();
    assert!(
        saved + CHECKPOINT_MS >= DIES_AT_MS && saved <= DIES_AT_MS,
        "at most five seconds lost: {saved} saved of {DIES_AT_MS}"
    );
    assert_eq!(listen["endPositionMs"].as_u64(), Some(position));
    // Sent twice, it was the second send that found it there: the library
    // was not written again.
    let revision = json(&data_dir, &["status"])["library"]["revision"].clone();

    // Resumed, the track carries on from the checkpoint as a new listen
    // that continues the one the crash closed.
    json(&data_dir, &["resume"]);
    until(&data_dir, &["now"], |now| {
        now["positionMs"]
            .as_u64()
            .is_some_and(|at| at > position + 2_000)
    });
    json(&data_dir, &["stop"]);
    let recent = until(&data_dir, &["history"], |recent| {
        recent["undelivered"] == 0
            && recent["plays"][0]["listens"]
                .as_array()
                .is_some_and(|all| all.len() == 2)
    });
    let play = &recent["plays"][0];
    assert_eq!(play["listens"][1]["startReason"], "resume");
    assert_eq!(play["listens"][1]["continues"], listen["listenId"]);
    assert_eq!(
        play["listens"][1]["startPositionMs"].as_u64(),
        Some(position)
    );
    assert_eq!(play["listens"][1]["endReason"], "stopped");
    // The resumed play is dated where the first listen began.
    assert_eq!(play["listens"][0]["startedAt"], listen["startedAt"]);
    assert_ne!(
        json(&data_dir, &["status"])["library"]["revision"],
        revision
    );
    server.stop();
}

#[test]
fn a_server_killed_just_after_a_join_keeps_both_listens_and_the_boundary_between_them() {
    let scratch = scratch();
    let data_dir = scratch.data_dir();
    // A three-second track joined to a forty-second one, at real time. The
    // server dies four seconds into the second, which the first never
    // reaches, before the second's first five-second save.
    let music = album(&scratch, [3, TRACK_SECONDS]);
    let folder = music.to_str().unwrap();
    let dying = start(&data_dir, &[("RITORNEL_TEST_FAIL_AT_POSITION_MS", "4000")]);
    ok(&data_dir, &["library", "create"]);
    ok(&data_dir, &["scan", folder]);
    ok(&data_dir, &["play", folder]);
    let ended = dying.ended();
    assert_eq!(ended.code(), ABORTED, "the server ended with {ended}");

    let server = start(&data_dir, &[("RITORNEL_TEST_NULL_OUTPUT_SPEED", "20")]);
    let now = json(&data_dir, &["now"]);
    assert_eq!(
        (now["status"].as_str(), now["index"].as_u64()),
        (Some("paused"), Some(1))
    );
    // Each listen reaches history once: the first whole, the second closed
    // as a crash with what was saved as it began, beginning where the first
    // ended.
    let recent = until(&data_dir, &["history"], |recent| recent["undelivered"] == 0);
    let [second, first] = recent["plays"].as_array().unwrap().as_slice() else {
        panic!("two plays: {recent}");
    };
    let ([first], [second]) = (
        first["listens"].as_array().unwrap().as_slice(),
        second["listens"].as_array().unwrap().as_slice(),
    ) else {
        panic!("a listen each: {recent}");
    };
    assert_eq!(
        (first["endReason"].as_str(), first["msPlayed"].as_u64()),
        (Some("finished"), Some(3_000))
    );
    assert_eq!(second["endReason"], "crash");
    assert_eq!(second["startReason"], "auto_advance");
    let saved = second["msPlayed"].as_u64().unwrap();
    assert!(
        saved + CHECKPOINT_MS >= 4_000,
        "at most five seconds lost: {saved}"
    );
    let gap = second["startedAt"].as_i64().unwrap() - first["endedAt"].as_i64().unwrap();
    assert!(
        gap.abs() <= 1,
        "the second began {gap} ms after the first ended"
    );
    server.stop();
}

#[test]
fn on_routes_playback_and_queue_commands_to_one_named_player() {
    use ritornel_client::contract::local::{SERVER_JSON, ServerJson};
    use ritornel_client::contract::{
        CONTRACT_VERSION, ClientKind, HelloParams, NoParams, Topic, methods::PlayersListCall,
    };

    let scratch = scratch();
    let hub = scratch.data_dir();
    let desk = scratch.0.join("desk");
    let _hub_server = start(&hub, &[("RITORNEL_NAME", "Hub")]);
    let desk_server = start(&desk, &[("RITORNEL_NAME", "Desk")]);
    ok(&hub, &["library", "create"]);
    let named: ServerJson =
        serde_json::from_slice(&std::fs::read(hub.join(SERVER_JSON)).unwrap()).unwrap();
    let link = |data: &Path| {
        let invite = json(
            &hub,
            &["link", "invite", "--route", &named.endpoint.to_string()],
        );
        ok(data, &["link", "join", invite["link"].as_str().unwrap()]);
    };
    link(&desk);

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut observer = runtime.block_on(async {
        let c = ritornel_client::local::find(&hub)
            .await
            .unwrap()
            .connect()
            .await
            .unwrap();
        c.hello(&HelloParams {
            name: "cli-test".into(),
            kind: ClientKind::Cli,
            version: "1".into(),
            contract_version: CONTRACT_VERSION,
            presence: None,
        })
        .await
        .unwrap();
        c.subscribe(&[Topic::Players]).await.unwrap();
        c
    });
    let wait_for = async |observer: &mut ritornel_client::Client, known: usize, online: usize| {
        tokio::time::timeout(WAIT, async {
            loop {
                let list = observer
                    .call::<PlayersListCall>(&NoParams {})
                    .await
                    .unwrap();
                let desks: Vec<_> = list.players.iter().filter(|p| p.name == "Desk").collect();
                if desks.len() == known && desks.iter().filter(|p| p.reachable).count() == online {
                    break;
                }
                observer.next_event().await.unwrap();
            }
        })
        .await
        .unwrap();
    };
    runtime.block_on(wait_for(&mut observer, 1, 1));
    let initial = json(&hub, &["now"])["volume"].clone();
    json(&hub, &["--on", "Desk", "volume", "37"]);
    assert_eq!(json(&desk, &["now"])["volume"], 0.37);
    assert_eq!(json(&hub, &["now"])["volume"], initial);
    assert_eq!(
        json(&hub, &["queue", "--on", "Desk"]),
        json(&desk, &["queue"])
    );

    let refuses = |name: &str, why: &str| {
        let (code, _, stderr) = ritornelctl(&hub, &["now", "--on", name]);
        assert_ne!(code, Some(0));
        assert!(stderr.contains(why), "{stderr}");
    };
    refuses("Unknown", "no player has that name");
    desk_server.stop();
    runtime.block_on(wait_for(&mut observer, 1, 0));
    refuses("Desk", "the named player is away");
    let another = scratch.0.join("another");
    let _another_server = start(&another, &[("RITORNEL_NAME", "Desk")]);
    link(&another);
    runtime.block_on(wait_for(&mut observer, 2, 1));
    refuses("Desk", "more than one player has that name");
}
