//! The real `ritornelctl` and `ritornel-server` against a fake Last.fm on a
//! loopback socket, with an invented account: signing in, the zones a play
//! is dated by, the import of a history killed part way and carried on, a
//! second sync that writes nothing, the loved tracks, the report, and a
//! play scrobbled once whose echo joins it when it comes back. No request
//! leaves this machine, and no key, secret or session key reaches a log.
//!
//! As in `play.rs`, `ritornel-server` is taken from beside `ritornelctl`,
//! and nothing here sleeps but to wait for something, bounded.

#[path = "../../../../fixtures/support/tone.rs"]
mod tone;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tone::write_aiff;

use ritornel_lastfm::fake::{API_KEY, API_SECRET, FakeLastfm, FakeScrobble, SESSION_KEY, TOKEN};
use serde_json::Value;

use super::support::{
    ABORTED, Scratch, Server, WAIT, json, logs, null_output_command, ok, ritornelctl, until_every,
};

/// How long a track of the invented album is.
const TRACK_SECONDS: u32 = 40;

/// How long a track is where nothing may play long enough to be
/// scrobbled: half of it is two minutes, far longer than the test takes.
const LONG_TRACK_SECONDS: u32 = 240;

/// 2009-06-30 23:30 UTC: 1 July in Helsinki, 30 June in New York.
const MIDSUMMER_2009: u64 = 1_246_404_600;

fn scratch() -> Scratch {
    Scratch::new("ritornelctl-lastfm-")
}

/// A folder of two invented tracks by an invented artist: `seconds` of
/// tone each, as 16-bit AIFF at 8 kHz with ID3 tags.
fn music(scratch: &Scratch, seconds: u32) -> PathBuf {
    let folder = scratch
        .0
        .canonicalize()
        .unwrap()
        .join("music/Invented Album");
    std::fs::create_dir_all(&folder).unwrap();
    for (name, title, number, hz) in [
        ("01 First.aiff", "First Light", "1", 440.0),
        ("02 Second.aiff", "Second Wind", "2", 660.0),
    ] {
        write_aiff(&folder.join(name), title, number, hz, seconds);
    }
    folder
}

/// Asks again and again until `done` says so, for at most [`WAIT`].
fn until(data_dir: &Path, args: &[&str], done: impl Fn(&Value) -> bool) -> Value {
    until_every(data_dir, args, Duration::from_millis(100), done)
}

/// Starts a `ritornel-server` on the null output, sending everything for
/// Last.fm to `fake`.
fn start(data_dir: &Path, fake: &FakeLastfm, more: &[(&str, &str)]) -> Server {
    let url = fake.base_url();
    let mut command = null_output_command(data_dir, &[]);
    command
        .env("SCROBL_API_KEY", API_KEY)
        .env("SCROBL_API_SECRET", API_SECRET)
        .env("RITORNEL_TEST_LASTFM_URL", &url)
        .env("RITORNEL_TEST_LASTFM_CHUNK", "7")
        .env("RITORNEL_TEST_LASTFM_PAGE", "3")
        .env("RITORNEL_TEST_NULL_OUTPUT_SPEED", "50")
        .envs(more.iter().copied());
    Server::start_command(command, false)
}

/// An invented history: plays of the invented album and of an artist the
/// library does not have, every hour back from yesterday, and one at
/// midsummer 2009.
fn history(now: u64) -> Vec<FakeScrobble> {
    let mut scrobbles: Vec<FakeScrobble> = (0..40)
        .map(|i| {
            let at = now - 86_400 - i * 3_600;
            match i % 4 {
                0 => FakeScrobble::new(at, "Invented Artist", "Invented Album", "First Light"),
                1 => FakeScrobble::new(at, "invented artist", "Invented Album", "SECOND WIND"),
                2 => FakeScrobble::new(at, "Invented Artist", "", "Second Wind (Live)"),
                _ => FakeScrobble::new(at, "Nobody Here", "Elsewhere", "Not Owned"),
            }
        })
        .collect();
    scrobbles.push(FakeScrobble::new(
        MIDSUMMER_2009,
        "Invented Artist",
        "Invented Album",
        "First Light",
    ));
    scrobbles
}

fn now_s() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// What `history artist` or `history album` prints for `name` is the
/// sentence its page shows, `history.artist` or `history.album` as the
/// desktop reads it, and both agree with `plays`, every play as
/// `ritornelctl history` lists it: their count, their first and last days
/// and their years, of the plays that count and are bound to the invented
/// album, as only those reach a page. Every invented play here is of that
/// album and that artist, so the two say the same.
fn page_agrees_with_the_plays(data_dir: &Path, what: &str, name: &str, plays: &Value) {
    let page = json(data_dir, &["history", what, name]);
    let text = ok(data_dir, &["history", what, name]);
    let said: String = page["sentence"]
        .as_array()
        .unwrap()
        .iter()
        .map(|phrase| phrase["text"].as_str().unwrap())
        .collect();
    assert_eq!(text.lines().next(), Some(said.as_str()), "{text}");

    // The plays a page counts, from the list of every play.
    let mine: Vec<&Value> = plays["plays"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|play| {
            play["counts"] == true
                && play["excluded"] == false
                && play["track"]["recordingId"].is_string()
                && play["track"]["artist"] == "Invented Artist"
        })
        .collect();
    let mut days: Vec<&str> = mine
        .iter()
        .map(|play| play["localDate"].as_str().unwrap())
        .collect();
    days.sort_unstable();
    assert_eq!(page["plays"], mine.len(), "{page}");
    assert_eq!(
        mine.len(),
        23,
        "the album's two songs ten times each, the play of 2009 and the two heard here"
    );
    assert_eq!(page["firstPlayed"], days[0]);
    assert_eq!(page["lastPlayed"], *days.last().unwrap());
    let mut years = std::collections::BTreeMap::<String, u64>::new();
    for day in &days {
        *years.entry(day[..4].to_owned()).or_default() += 1;
    }
    let paged: std::collections::BTreeMap<String, u64> = page["years"]
        .as_array()
        .unwrap()
        .iter()
        .map(|year| (year["year"].to_string(), year["plays"].as_u64().unwrap()))
        .collect();
    assert_eq!(paged, years);
    for (year, count) in &years {
        assert!(
            text.lines()
                .any(|line| line.starts_with(year.as_str()) && line.contains(&format!(" {count} "))),
            "{year}: {count} in {text}"
        );
    }

    // The play of 2009 began the history, so the album was there from
    // your first weeks; and the plays of the last day are in the last year.
    assert!(
        said.starts_with("With you since your first weeks, July 2009"),
        "{said}"
    );
    let recent = mine
        .iter()
        .filter(|play| play["at"].as_i64().unwrap() > (now_s() as i64 - 365 * 86_400) * 1000)
        .count();
    assert_eq!(page["lastYear"], recent);
    if said.contains("in the last year") {
        assert!(
            said.ends_with(&format!(" {recent} plays in the last year.")),
            "{said}"
        );
    }
}

#[test]
fn a_last_fm_history_is_imported_through_a_kill_and_a_play_is_scrobbled_once() {
    let scratch = scratch();
    let (data_dir, music) = (scratch.data_dir(), music(&scratch, TRACK_SECONDS));
    let folder = music.to_str().unwrap();
    let fake = FakeLastfm::start("someone", history(now_s()));
    fake.set_loved(vec![(
        "Invented Artist",
        "First Light",
        now_s() - 7 * 86_400,
    )]);
    fake.approve_after(1);

    // The server dies straight after the library commits the second batch
    // of the sync.
    let dying = start(
        &data_dir,
        &fake,
        &[("RITORNEL_TEST_FAIL_AFTER_IMPORT_BATCH", "2")],
    );
    ok(&data_dir, &["library", "create"]);
    ok(&data_dir, &["scan", folder]);

    // Signing in prints the address to approve, then the account.
    let status = json(&data_dir, &["lastfm", "status"]);
    assert_eq!(
        (
            status["available"].as_bool(),
            status["configured"].as_bool()
        ),
        (Some(true), Some(true))
    );
    assert_eq!(status.get("account"), None);
    let (code, stdout, stderr) = ritornelctl(&data_dir, &["lastfm", "signin"]);
    assert_eq!((code, stderr.as_str()), (Some(0), ""), "{stdout}");
    assert!(stdout.contains(TOKEN), "the address to approve: {stdout}");
    assert!(
        stdout.ends_with("Signed in to Last.fm as someone.\n"),
        "{stdout}"
    );
    let status = ok(&data_dir, &["lastfm", "status"]);
    assert!(status.contains("Account           someone\n"), "{status}");

    // A sync waits for the zones.
    let (code, _, stderr) = ritornelctl(&data_dir, &["lastfm", "sync"]);
    assert_eq!(code, Some(1));
    assert!(stderr.contains("history zone add"), "{stderr}");
    for zone in [
        &["history", "zone", "add", "America/New_York"][..],
        &[
            "history",
            "zone",
            "add",
            "Europe/Helsinki",
            "--from",
            "2009-06-01",
            "--until",
            "2009-08-01",
        ],
    ] {
        ok(&data_dir, zone);
    }
    let zones = json(&data_dir, &["history", "zones"]);
    assert_eq!(zones["periods"][1]["from"], "2009-06-01");

    // Started, it dies part way.
    ok(&data_dir, &["lastfm", "sync", "--no-wait"]);
    let ended = dying.ended();
    assert_eq!(ended.code(), ABORTED, "the server ended with {ended}");

    // The next run carries the sync on from its cursor, by itself.
    let server = start(&data_dir, &fake, &[]);
    let jobs = until(&data_dir, &["jobs"], |jobs| {
        jobs["jobs"][0]["state"] == "succeeded"
    });
    let summary = &jobs["jobs"][0]["sync"];
    assert_eq!(summary["sourceTotal"], 41);
    assert_eq!(summary["stored"], 41, "{summary}");
    assert!(summary["new"].as_u64().unwrap() < 41, "{summary}");
    assert_eq!(summary["loved"], 1);

    // A second sync with nothing new writes nothing.
    let revision = json(&data_dir, &["status"])["library"]["revision"].clone();
    let again = json(&data_dir, &["lastfm", "sync"]);
    assert_eq!(again["state"], "succeeded");
    assert_eq!(again["sync"]["new"], 0);
    assert_eq!(
        json(&data_dir, &["status"])["library"]["revision"],
        revision
    );

    // The play of 2009 falls on 1 July, in Helsinki.
    let day = json(&data_dir, &["history", "on-this-day", "--day", "07-01"]);
    let play = &day["plays"][0];
    assert_eq!(play["localDate"], "2009-07-01");
    assert_eq!(play["zone"], "Europe/Helsinki");
    assert_eq!(play["clockBasis"], "source_utc");
    assert_eq!(play["track"]["title"], "First Light");

    // The report: the album's two songs match, whatever their case; the
    // live one is doubtful; the stranger is not in the library.
    let report = json(&data_dir, &["lastfm", "report"]);
    assert_eq!(
        (report["plays"].as_u64(), report["matchedPlays"].as_u64()),
        (Some(41), Some(21))
    );
    assert_eq!(report["doubtful"][0]["title"], "Second Wind (Live)");
    assert_eq!(
        report["doubtful"][0]["candidates"][0]["title"],
        "Second Wind"
    );
    assert_eq!(report["absentPlays"], 10);
    let text = ok(&data_dir, &["lastfm", "report"]);
    assert!(
        text.contains("Plays matched     21 of 41 (51.2%)"),
        "{text}"
    );

    // The loved track shows as loved.
    let track = play["track"]["appearanceId"].as_str().unwrap();
    let shown = ok(&data_dir, &["show", track]);
    assert!(
        shown.contains("Loved             by lastfm:someone"),
        "{shown}"
    );

    // Scrobbling on, the album played: each play that counts is sent once,
    // and its echo joins it when the next sync brings it back.
    json(&data_dir, &["lastfm", "scrobbling", "on"]);
    ok(&data_dir, &["play", folder, "--follow"]);
    let deadline = Instant::now() + WAIT;
    while fake.scrobbled().len() < 2 {
        assert!(Instant::now() < deadline, "{:?}", fake.scrobbled());
        // sleep: between looks; the deadline bounds them.
        std::thread::sleep(Duration::from_millis(100));
    }
    let sent = fake.scrobbled();
    assert_eq!(
        sent.iter().map(|s| s.title.as_str()).collect::<Vec<_>>(),
        ["First Light", "Second Wind"]
    );
    assert_eq!(sent[0].artist, "Invented Artist");
    assert_eq!(sent[0].album, "Invented Album");
    // Each is recorded as accepted once its answer is in, and the counts
    // come by state, so wait until neither is still on its way.
    let status = until(&data_dir, &["lastfm", "status"], |status| {
        status["deliveries"]
            .as_array()
            .unwrap()
            .iter()
            .all(|count| !matches!(count["state"].as_str(), Some("pending" | "sending")))
    });
    assert_eq!(
        status["deliveries"],
        serde_json::json!([{ "state": "accepted", "count": 2 }])
    );

    let synced = json(&data_dir, &["lastfm", "sync"]);
    assert_eq!(synced["sync"]["new"], 2);
    let report = json(&data_dir, &["lastfm", "report"]);
    assert_eq!(report["echoes"], 2);
    assert_eq!(report["plays"], 41, "the echoes add no play");
    let recent = json(&data_dir, &["history", "--limit", "2"]);
    for play in recent["plays"].as_array().unwrap() {
        assert_eq!(play["clockBasis"], "hub_age", "{play}");
    }
    // Sent once: nothing more goes after the echoes are back.
    assert_eq!(fake.scrobbled().len(), 2);

    // What the artist's page and the album's say agrees with the plays.
    let plays = json(&data_dir, &["history", "--limit", "500"]);
    for (what, name) in [("artist", "invented artist"), ("album", "Invented Album")] {
        page_agrees_with_the_plays(&data_dir, what, name, &plays);
    }
    let (code, _, stderr) = ritornelctl(&data_dir, &["history", "artist", "Nobody Here"]);
    assert_eq!(code, Some(1), "a stranger has no page: {stderr}");
    server.stop();

    // Nothing the server keeps secret reached a log.
    let written = logs(&data_dir);
    for secret in [API_KEY, API_SECRET, SESSION_KEY, TOKEN] {
        assert!(!written.contains(secret), "a secret in the log");
    }
}

#[test]
fn the_hub_imports_on_its_own_when_it_starts_and_a_failure_waits_for_the_next_trigger() {
    let scratch = scratch();
    let data_dir = scratch.data_dir();
    let fake = FakeLastfm::start("someone", history(now_s()));

    // Set up, and never imported: the next is due now.
    let server = start(&data_dir, &fake, &[]);
    ok(&data_dir, &["library", "create"]);
    let (code, stdout, stderr) = ritornelctl(&data_dir, &["lastfm", "signin", "--json"]);
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(
        serde_json::from_str::<Value>(&stdout).unwrap()["account"],
        "someone"
    );
    assert!(stderr.contains(TOKEN), "{stderr}");
    let text = ok(&data_dir, &["lastfm", "status"]);
    assert!(
        text.contains("Next import       only by hand: no zones"),
        "{text}"
    );
    let zone = ["history", "zone", "add", "America/New_York"];
    ok(&data_dir, &zone);
    let status = json(&data_dir, &["lastfm", "status"]);
    assert!(status["import"]["next"].is_i64(), "{status}");
    server.stop();

    // Last.fm cannot be reached: the import the hub starts with fails,
    // once, and the next waits for a trigger.
    // Each connection is closed unanswered. The listener is kept, so that
    // no other test's server can be given its port meanwhile.
    let dead = {
        let unanswering = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/2.0/", unanswering.local_addr().unwrap());
        std::thread::spawn(move || unanswering.incoming().for_each(drop));
        url
    };
    let server = start(
        &data_dir,
        &fake,
        &[("RITORNEL_TEST_LASTFM_URL", dead.as_str())],
    );
    until(&data_dir, &["jobs"], |jobs| {
        jobs["jobs"][0]["state"] == "failed"
    });
    let status = json(&data_dir, &["lastfm", "status"]);
    let import = &status["import"];
    let ended = import["last"]["ended"].as_i64().unwrap();
    assert_eq!(import["last"]["state"], "failed", "{status}");
    assert!(import["last"]["failure"].is_string(), "{status}");
    assert_eq!(import["next"], ended + 6 * 3_600_000, "{status}");
    assert_eq!(import["onOpenAfter"], ended + 3_600_000, "{status}");
    let text = ok(&data_dir, &["lastfm", "status"]);
    assert!(text.contains(", failed: Last.fm: "), "{text}");
    assert!(text.contains(", or when the app opens after "), "{text}");
    let jobs = json(&data_dir, &["jobs"]);
    assert_eq!(
        jobs["jobs"][0]["state"], "failed",
        "nothing is tried again before a trigger: {jobs}"
    );
    server.stop();

    // Reachable again, the hub imports the history when it starts, with
    // no command.
    let server = start(&data_dir, &fake, &[]);
    let jobs = until(&data_dir, &["jobs"], |jobs| {
        jobs["jobs"][0]["state"] == "succeeded"
    });
    assert_eq!(jobs["jobs"][0]["sync"]["stored"], 41, "{jobs}");
    let status = json(&data_dir, &["lastfm", "status"]);
    assert_eq!(status["imported"], 41);
    assert_eq!(status["import"]["last"]["state"], "succeeded", "{status}");
    let text = ok(&data_dir, &["lastfm", "status"]);
    assert!(text.contains(" UTC, succeeded\n"), "{text}");
    server.stop();

    let written = logs(&data_dir);
    assert!(written.contains("a Last.fm import starts on its own"));
    assert!(written.contains("a Last.fm sync failed: the next import tries again"));
    for secret in [API_KEY, API_SECRET, SESSION_KEY, TOKEN] {
        assert!(!written.contains(secret), "a secret in the log");
    }
}

/// Waits until the fake has been told of `count` tracks playing now, for
/// at most [`WAIT`], and returns what each sent.
fn shown(fake: &FakeLastfm, count: usize) -> Vec<std::collections::HashMap<String, String>> {
    let deadline = Instant::now() + WAIT;
    loop {
        let sent = fake.now_playing();
        if sent.len() >= count {
            return sent;
        }
        assert!(Instant::now() < deadline, "{sent:?}");
        // sleep: between looks; the deadline bounds them.
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Whether `ritornelctl now` has the playing track at `ms` or past it.
fn past(ms: u64) -> impl Fn(&Value) -> bool {
    move |now| now["positionMs"].as_u64().is_some_and(|at| at >= ms)
}

#[test]
fn what_begins_to_play_is_shown_as_playing_now_only_while_scrobbling_is_on() {
    let scratch = scratch();
    let (data_dir, music) = (scratch.data_dir(), music(&scratch, LONG_TRACK_SECONDS));
    let folder = music.to_str().unwrap();
    let fake = FakeLastfm::start("someone", Vec::new());
    // At real time, so that a track is heard long enough to be shown.
    let server = start(
        &data_dir,
        &fake,
        &[("RITORNEL_TEST_NULL_OUTPUT_SPEED", "1")],
    );
    ok(&data_dir, &["library", "create"]);
    ok(&data_dir, &["scan", folder]);
    ok(&data_dir, &["lastfm", "signin"]);

    // Scrobbling off: a track heard for seconds is shown nowhere.
    ok(&data_dir, &["play", folder]);
    until(&data_dir, &["now"], past(2_500));
    assert!(fake.now_playing().is_empty(), "{:?}", fake.now_playing());

    // Turned on, the next track to begin is shown once, under the names
    // its scrobble would carry.
    json(&data_dir, &["lastfm", "scrobbling", "on"]);
    json(&data_dir, &["next"]);
    let sent = shown(&fake, 1);
    let field = |name: &str| sent[0].get(name).map(String::as_str);
    assert_eq!(field("artist"), Some("Invented Artist"));
    assert_eq!(field("track"), Some("Second Wind"));
    assert_eq!(field("album"), Some("Invented Album"));
    assert_eq!(
        field("albumArtist"),
        None,
        "the album's artist is the track's"
    );
    assert_eq!(field("trackNumber"), Some("2"));
    assert_eq!(
        field("duration"),
        Some(LONG_TRACK_SECONDS.to_string().as_str())
    );
    assert_eq!(field("sk"), Some(SESSION_KEY));
    until(&data_dir, &["now"], past(3_000));
    assert_eq!(fake.now_playing().len(), 1, "shown once while it plays");

    // A pause shows nothing; playing again shows it again.
    json(&data_dir, &["pause"]);
    // sleep: time for a wrong "now playing" to be sent. A slow machine can
    // only make this pass when it should not, never fail when it should not.
    std::thread::sleep(Duration::from_millis(1_500));
    assert_eq!(fake.now_playing().len(), 1);
    json(&data_dir, &["resume"]);
    let sent = shown(&fake, 2);
    assert_eq!(
        sent[1].get("track").map(String::as_str),
        Some("Second Wind")
    );
    json(&data_dir, &["stop"]);
    // sleep: as above, time for a wrong one to be sent.
    std::thread::sleep(Duration::from_millis(1_500));
    assert_eq!(fake.now_playing().len(), 2);
    assert!(
        fake.scrobbled().is_empty(),
        "nothing played long enough to count"
    );
    server.stop();

    let written = logs(&data_dir);
    for secret in [API_KEY, API_SECRET, SESSION_KEY, TOKEN] {
        assert!(!written.contains(secret), "a secret in the log");
    }
}

/// Copies a stopped server's data directory, as `cp -R` does.
fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let to = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &to);
        } else {
            std::fs::copy(entry.path(), &to).unwrap();
        }
    }
}

#[test]
fn a_copied_or_restored_data_directory_scrobbles_nothing_until_scrobbling_is_turned_on_there() {
    let scratch = scratch();
    let (data_dir, music) = (scratch.data_dir(), music(&scratch, TRACK_SECONDS));
    let folder = music.to_str().unwrap();
    let place = scratch.0.join("backups");
    std::fs::create_dir_all(&place).unwrap();
    let backups = [("RITORNEL_BACKUP_DIR", place.to_str().unwrap())];
    let fake = FakeLastfm::start("someone", Vec::new());
    let since = |dir: &Path| json(dir, &["lastfm", "status"])["scrobblingSince"].clone();
    let plays = |dir: &Path| json(dir, &["history"])["plays"].as_array().unwrap().len();

    let server = start(&data_dir, &fake, &backups);
    ok(&data_dir, &["library", "create"]);
    ok(&data_dir, &["scan", folder]);
    ok(&data_dir, &["lastfm", "signin"]);
    json(&data_dir, &["lastfm", "scrobbling", "on"]);
    let on = since(&data_dir);
    assert!(on.is_number(), "{on}");
    server.stop();

    // The album played, on a run that cannot reach Last.fm: two plays
    // wait to be scrobbled when the backup is taken and the directory is
    // copied.
    let mut command = null_output_command(&data_dir, &backups);
    command.env("RITORNEL_TEST_NULL_OUTPUT_SPEED", "50");
    let server = Server::start_command(command, false);
    ok(&data_dir, &["play", folder, "--follow"]);
    until(&data_dir, &["history"], |recent| recent["undelivered"] == 0);
    assert_eq!(plays(&data_dir), 2);
    let backed_up = json(&data_dir, &["backup"]);
    let backup = PathBuf::from(backed_up["backup"]["path"].as_str().unwrap());
    let readme = std::fs::read_to_string(backup.join("README.txt")).unwrap();
    assert!(
        readme.contains("ritornelctl lastfm scrobbling on"),
        "{readme}"
    );
    server.stop();

    // A copy of the data directory has the same server and session, and
    // scrobbling is off there, so nothing is planned or sent.
    let copy = scratch.0.join("copy");
    copy_dir(&data_dir, &copy);
    let server = start(&copy, &fake, &[]);
    let status = json(&copy, &["lastfm", "status"]);
    assert_eq!(status.get("scrobblingSince"), None, "{status}");
    assert_eq!(status["deliveries"], serde_json::json!([]), "{status}");
    assert!(logs(&copy).contains("copied or moved from"));
    // Turned on there, it scrobbles what is played from then on, and never
    // the plays the copy came with, which stay in its history.
    json(&copy, &["lastfm", "scrobbling", "on"]);
    let rearmed = since(&copy).as_u64().unwrap();
    ok(&copy, &["play", folder, "--follow"]);
    let deadline = Instant::now() + WAIT;
    while fake.scrobbled().len() < 2 {
        assert!(Instant::now() < deadline, "{:?}", fake.scrobbled());
        // sleep: between looks; the deadline bounds them.
        std::thread::sleep(Duration::from_millis(100));
    }
    until(&copy, &["lastfm", "status"], |status| {
        status["deliveries"]
            .as_array()
            .unwrap()
            .iter()
            .all(|count| !matches!(count["state"].as_str(), Some("pending" | "sending")))
    });
    let sent = fake.scrobbled();
    assert_eq!(sent.len(), 2, "{sent:?}");
    assert!(
        sent.iter().all(|scrobble| scrobble.at_s >= rearmed / 1000),
        "only what was played after it was turned on: {sent:?}"
    );
    assert_eq!(plays(&copy), 4);
    server.stop();
    let server = start(&copy, &fake, &[]);
    assert!(
        since(&copy).is_number(),
        "turned on there, it stays on there"
    );
    server.stop();

    // The data directory it was copied from goes on as it was.
    let server = start(&data_dir, &fake, &[]);
    assert_eq!(since(&data_dir), on);
    server.stop();

    // Put back from the backup, even where it was taken, it scrobbles
    // nothing until it is turned on again.
    for name in ["server.db", "library.db"] {
        for sidecar in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(data_dir.join(format!("{name}{sidecar}")));
        }
        std::fs::copy(backup.join(name), data_dir.join(name)).unwrap();
    }
    let server = start(&data_dir, &fake, &[]);
    assert_eq!(since(&data_dir), Value::Null);
    assert!(logs(&data_dir).contains("put back from a backup"));
    server.stop();
}
