//! Sidecars and disc layouts through the scan, on invented audio: what is
//! kept, what a layout places, and that none of it moves a wall. See "Disc
//! layouts" in docs/internals/corrections.md.

use std::path::Path;

use ritornel_contract::{DiscsParams, LayoutMethod};
use ritornel_domain::layout::Toc;

use crate::scan_tests::Rig;
use crate::{Error, JobState, ScanSettings, WriteRequest};

/// A WAV file of CD audio exactly `sectors` long, its samples made from
/// `seed` so that no two seeds share audio.
fn wav(sectors: u32, seed: u32) -> Vec<u8> {
    let frames = sectors * 588;
    let data = frames * 4;
    let mut bytes = Vec::with_capacity(44 + data as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&44_100u32.to_le_bytes());
    bytes.extend_from_slice(&(44_100u32 * 4).to_le_bytes());
    bytes.extend_from_slice(&4u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data.to_le_bytes());
    for frame in 0..frames {
        let sample = (frame.wrapping_mul(seed.wrapping_mul(2_654_435_761)) >> 16) as u16;
        bytes.extend_from_slice(&sample.to_le_bytes());
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    bytes
}

/// The lengths of the invented disc's three tracks, in sectors.
const LENGTHS: [u32; 3] = [75, 120, 90];

fn toc() -> Toc {
    Toc {
        starts: vec![0, 75, 195],
        lead_out: 285,
    }
}

/// An EAC table of the invented disc.
fn log() -> String {
    let mut text = String::from(
        "Exact Audio Copy V1.6\n\n     Track |   Start  |  Length  | Start sector | End sector\n",
    );
    for (n, (start, length)) in (1..).zip(toc().starts.iter().zip(LENGTHS)) {
        text.push_str(&format!(
            "        {n}  |  0:00.00 |  0:01.00 |    {start}    |   {}   \n",
            start + u64::from(length) - 1
        ));
    }
    text
}

fn write(rig: &mut Rig, path: &str, bytes: &[u8]) {
    let to = rig.root().join(path);
    std::fs::create_dir_all(to.parent().unwrap()).unwrap();
    std::fs::write(&to, bytes).unwrap();
    rig.stamp(&to);
}

/// A folder of the invented disc's three tracks.
fn disc(rig: &mut Rig, folder: &str, seed: u32) {
    for (n, length) in (1..).zip(LENGTHS) {
        write(
            rig,
            &format!("{folder}/{n:02} Track.wav"),
            &wav(length, seed + n),
        );
    }
}

/// What a scan writes besides the evidence: everything a wall could show.
fn shown(rig: &Rig) -> Vec<String> {
    rig.dump()
        .into_iter()
        .filter(|row| {
            let table = row.split('|').next().unwrap_or_default();
            !table.starts_with("sidecar")
                && !table.starts_with("evidence_")
                && !table.starts_with("library_")
        })
        .collect()
}

#[test]
fn a_rip_log_beside_the_files_gives_the_disc_and_places_each_file() {
    let mut rig = Rig::new();
    disc(&mut rig, "Someone/An Album", 1);
    write(&mut rig, "Someone/An Album/An Album.log", log().as_bytes());
    write(&mut rig, "Someone/An Album/lyrics.txt", b"la la la");
    write(
        &mut rig,
        "Someone/An Album/Misc/An Album.cue",
        b"REM nothing here",
    );
    rig.scan();

    // The log and the CUE sheet are kept; the text file that is no rip log
    // is not.
    assert_eq!(
        rig.rows("SELECT rel_path FROM sidecar ORDER BY rel_path"),
        [
            "Someone/An Album/An Album.log",
            "Someone/An Album/Misc/An Album.cue"
        ]
    );
    let list = rig.library.discs(&DiscsParams::default()).unwrap();
    assert_eq!((list.folders, list.with_layout), (1, 1));
    let [found] = list.discs.as_slice() else {
        panic!("{list:?}");
    };
    assert_eq!(found.disc_id, toc().disc_id().unwrap());
    assert_eq!(
        (found.root.as_str(), found.folder.as_str()),
        ("main", "Someone/An Album")
    );
    assert_eq!(found.methods, [LayoutMethod::Log]);
    assert!(found.matched && !found.provisional && !found.whole_file);
    assert!(found.files.is_empty(), "files only for one folder");

    let one = rig
        .library
        .discs(&DiscsParams {
            folder: Some("Someone/An Album".to_owned()),
        })
        .unwrap();
    let files: Vec<(u32, &str)> = one.discs[0]
        .files
        .iter()
        .map(|file| (file.position, file.path.as_str()))
        .collect();
    assert_eq!(
        files,
        [
            (1, "Someone/An Album/01 Track.wav"),
            (2, "Someone/An Album/02 Track.wav"),
            (3, "Someone/An Album/03 Track.wav"),
        ]
    );
    // The writes say who made them.
    assert_eq!(
        rig.rows("SELECT method FROM library_revision WHERE method LIKE '%.%' ORDER BY rev")
            .into_iter()
            .filter(|method| method != "sources.add" && method != "library.create")
            .collect::<Vec<_>>(),
        [
            "scan.step",
            "scan.sidecars",
            "scan.step",
            "corrections.layouts"
        ]
    );
}

#[test]
fn a_scan_that_finds_only_sidecars_moves_no_wall_and_a_quiet_rescan_writes_nothing() {
    let mut rig = Rig::new();
    disc(&mut rig, "a", 1);
    rig.scan();
    let before = shown(&rig);
    assert!(
        rig.library
            .discs(&DiscsParams::default())
            .unwrap()
            .discs
            .is_empty()
    );

    write(&mut rig, "a/rip.log", log().as_bytes());
    let summary = rig.scan();
    assert_eq!(summary.unchanged, 3);
    assert_eq!(shown(&rig), before, "only evidence was written");
    assert_eq!(
        rig.library
            .discs(&DiscsParams::default())
            .unwrap()
            .discs
            .len(),
        1
    );
    assert_eq!(
        rig.rows("SELECT method FROM library_revision ORDER BY rev DESC LIMIT 2"),
        ["corrections.layouts", "scan.sidecars"]
    );

    let (revision, dump) = (rig.revision(), rig.dump());
    rig.scan();
    assert_eq!((rig.revision(), rig.dump()), (revision, dump));
}

#[test]
fn a_changed_sidecar_is_a_new_revision_and_a_removed_one_takes_its_layout_away() {
    let mut rig = Rig::new();
    disc(&mut rig, "a", 1);
    write(&mut rig, "a/rip.log", b"an unfinished log");
    rig.scan();
    assert!(
        rig.library
            .discs(&DiscsParams::default())
            .unwrap()
            .discs
            .is_empty()
    );

    write(&mut rig, "a/rip.log", log().as_bytes());
    rig.scan();
    assert_eq!(rig.one("SELECT count(*) FROM sidecar_revision"), "2");
    assert_eq!(
        rig.library
            .discs(&DiscsParams::default())
            .unwrap()
            .discs
            .len(),
        1
    );

    std::fs::remove_file(rig.root().join("a/rip.log")).unwrap();
    rig.scan();
    assert_eq!(
        rig.one("SELECT missing_since_ms IS NOT NULL FROM sidecar"),
        "1"
    );
    assert_eq!(rig.one("SELECT count(*) FROM sidecar_revision"), "2");
    let list = rig.library.discs(&DiscsParams::default()).unwrap();
    assert!(list.discs.is_empty());
    assert_eq!((list.folders, list.with_layout), (1, 0));

    // Back as it was: the revision it had, read again.
    write(&mut rig, "a/rip.log", log().as_bytes());
    rig.scan();
    assert_eq!(
        rig.one("SELECT count(*), missing_since_ms IS NULL FROM sidecar_revision, sidecar"),
        "2|1"
    );
    assert_eq!(
        rig.library
            .discs(&DiscsParams::default())
            .unwrap()
            .discs
            .len(),
        1
    );
}

#[test]
fn a_log_above_two_disc_folders_counts_for_the_disc_it_fits() {
    let mut rig = Rig::new();
    disc(&mut rig, "Set/CD1", 1);
    write(&mut rig, "Set/CD2/01 Bonus.wav", &wav(300, 9));
    write(&mut rig, "Set/Set.log", log().as_bytes());
    // A sidecar far from any audio is not kept.
    write(&mut rig, "Elsewhere/deep/x.cue", b"FILE \"x.wav\" WAVE");
    rig.scan();
    assert_eq!(rig.rows("SELECT rel_path FROM sidecar"), ["Set/Set.log"]);
    let list = rig.library.discs(&DiscsParams::default()).unwrap();
    let folders: Vec<&str> = list.discs.iter().map(|disc| disc.folder.as_str()).collect();
    assert_eq!(folders, ["Set/CD1"]);
    assert_eq!((list.folders, list.with_layout), (2, 1));
}

#[test]
fn a_file_moved_away_takes_its_place_on_the_layout_with_it() {
    let mut rig = Rig::new();
    disc(&mut rig, "a", 1);
    write(&mut rig, "a/rip.log", log().as_bytes());
    rig.scan();
    assert!(rig.library.discs(&DiscsParams::default()).unwrap().discs[0].matched);
    std::fs::rename(
        rig.root().join("a/02 Track.wav"),
        rig.root().join("02 Track.wav"),
    )
    .unwrap();
    rig.scan();
    let list = rig.library.discs(&DiscsParams::default()).unwrap();
    assert_eq!(list.discs.len(), 1, "the log still gives the disc");
    assert!(!list.discs[0].matched, "its files no longer fill it");
}

#[test]
fn a_scan_cut_off_after_any_step_carries_on_to_the_layouts() {
    // Step 1 records the files, step 2 the sidecars, step 3 resolves and
    // step 4 derives the layouts.
    for cut in 1..=4 {
        let mut rig = Rig::new();
        disc(&mut rig, "a", 1);
        write(&mut rig, "a/rip.log", log().as_bytes());
        let source = rig.main.clone();
        let cut_off = rig.try_scan(
            &source,
            ScanSettings {
                fail_after_step: Some(cut),
                ..ScanSettings::default()
            },
        );
        assert!(
            matches!(cut_off, Err(Error::FailPoint)),
            "{cut}: {cut_off:?}"
        );
        // Once the resolution has committed, the root is not walked again,
        // so it may be offline by then.
        let root = rig.root();
        let away = root.with_file_name("unmounted");
        if cut >= 3 {
            std::fs::rename(&root, &away).unwrap();
        }
        let resumed = rig.resume(ScanSettings::default());
        if cut >= 3 {
            std::fs::rename(&away, &root).unwrap();
        }
        resumed.unwrap();
        assert_eq!(
            rig.library
                .discs(&DiscsParams::default())
                .unwrap()
                .discs
                .len(),
            1,
            "{cut}"
        );
        assert_eq!(rig.server.jobs(1).unwrap()[0].state, JobState::Succeeded);
        assert_eq!(
            rig.one(
                "SELECT count(*) FROM library_revision WHERE method IN ('scan.sidecars', 'corrections.layouts')"
            ),
            "2",
            "{cut}"
        );
    }
}

#[test]
fn a_sidecar_is_only_ever_read() {
    let mut rig = Rig::new();
    disc(&mut rig, "a", 1);
    write(&mut rig, "a/rip.log", log().as_bytes());
    let path = rig.root().join("a/rip.log");
    let before = (std::fs::read(&path).unwrap(), modified(&path));
    rig.scan();
    assert_eq!((std::fs::read(&path).unwrap(), modified(&path)), before);
}

fn modified(path: &Path) -> std::time::SystemTime {
    std::fs::metadata(path).unwrap().modified().unwrap()
}

#[test]
fn a_scan_that_stops_before_the_layouts_leaves_them_due_for_the_next_scan() {
    let mut rig = Rig::new();
    disc(&mut rig, "a", 1);
    rig.scan();
    write(&mut rig, "a/rip.log", log().as_bytes());
    let source = rig.main.clone();
    // Step 1 records the sidecar; the job stops there, as a cancelled one
    // would, and is not taken up again.
    let cut_off = rig.try_scan(
        &source,
        ScanSettings {
            fail_after_step: Some(1),
            ..ScanSettings::default()
        },
    );
    assert!(matches!(cut_off, Err(Error::FailPoint)), "{cut_off:?}");
    let job = rig.server.jobs(1).unwrap().remove(0);
    rig.server
        .finish_job(job.id, JobState::Cancelled, None)
        .unwrap();
    assert!(
        rig.library
            .discs(&DiscsParams::default())
            .unwrap()
            .discs
            .is_empty()
    );
    // The next scan finds nothing new on disk, and still derives them.
    assert_eq!(rig.scan().unchanged, 3);
    assert_eq!(
        rig.library
            .discs(&DiscsParams::default())
            .unwrap()
            .discs
            .len(),
        1
    );
    // And then they are settled.
    let revision = rig.revision();
    rig.scan();
    assert_eq!(rig.revision(), revision);
}

#[test]
fn a_rip_log_that_becomes_notes_is_gone_once_and_quiet_after() {
    let mut rig = Rig::new();
    disc(&mut rig, "a", 1);
    let xld = log().replace("Exact Audio Copy", "X Lossless Decoder");
    write(&mut rig, "a/rip.txt", xld.as_bytes());
    rig.scan();
    assert_eq!(
        rig.library
            .discs(&DiscsParams::default())
            .unwrap()
            .discs
            .len(),
        1
    );
    write(&mut rig, "a/rip.txt", b"Notes on the pressing.");
    rig.scan();
    assert_eq!(
        rig.one("SELECT missing_since_ms IS NOT NULL FROM sidecar"),
        "1"
    );
    assert!(
        rig.library
            .discs(&DiscsParams::default())
            .unwrap()
            .discs
            .is_empty()
    );
    let (revision, dump) = (rig.revision(), rig.dump());
    rig.scan();
    rig.scan();
    assert_eq!((rig.revision(), rig.dump()), (revision, dump));
}

#[test]
fn a_disc_folder_that_cannot_be_read_keeps_the_log_above_it() {
    use crate::platform::set_listable;
    let mut rig = Rig::new();
    disc(&mut rig, "Set/CD1", 1);
    write(&mut rig, "Set/Set.log", log().as_bytes());
    write(&mut rig, "Other/01 Track.wav", &wav(10, 7));
    rig.scan();
    assert_eq!(
        rig.library
            .discs(&DiscsParams::default())
            .unwrap()
            .discs
            .len(),
        1
    );
    set_listable(&rig.root().join("Set/CD1"), false);
    rig.scan();
    set_listable(&rig.root().join("Set/CD1"), true);
    assert_eq!(rig.one("SELECT missing_since_ms IS NULL FROM sidecar"), "1");
    assert_eq!(
        rig.library
            .discs(&DiscsParams::default())
            .unwrap()
            .discs
            .len(),
        1
    );
}

#[test]
fn a_library_with_no_sidecars_never_derives_layouts() {
    let mut rig = Rig::new();
    disc(&mut rig, "a", 1);
    rig.scan();
    write(&mut rig, "a/04 More.wav", &wav(20, 9));
    rig.scan();
    assert_eq!(
        rig.one("SELECT count(*) FROM library_revision WHERE method = 'corrections.layouts'"),
        "0"
    );
}

#[test]
fn a_kept_sidecar_revision_is_not_replaced_by_its_id_or_its_bytes() {
    let mut rig = Rig::new();
    disc(&mut rig, "a", 1);
    write(&mut rig, "a/rip.log", log().as_bytes());
    rig.scan();
    // An older revision, which nothing refers to.
    write(&mut rig, "a/rip.log", b"an unfinished log");
    rig.scan();
    let kept = rig.dump();
    for sql in [
        "INSERT OR REPLACE INTO sidecar_revision
           (id, sidecar_id, content_hash, content, observed_at_ms, rev)
         SELECT id, sidecar_id, content_hash, x'00', observed_at_ms, rev FROM sidecar_revision
         WHERE id NOT IN (SELECT current_revision_id FROM sidecar)",
        "INSERT OR REPLACE INTO sidecar_revision
           (id, sidecar_id, content_hash, content, observed_at_ms, rev)
         SELECT id + 100, sidecar_id, content_hash, x'00', observed_at_ms, rev FROM sidecar_revision
         WHERE id NOT IN (SELECT current_revision_id FROM sidecar)",
    ] {
        let request = WriteRequest {
            method: "test.replace",
            command: None,
            client: None,
        };
        let replaced = rig.library.write(request, move |write| {
            write.connection().execute(sql, [])?;
            Ok(String::new())
        });
        assert!(replaced.is_err(), "{sql}");
    }
    assert_eq!(rig.dump(), kept);
}
