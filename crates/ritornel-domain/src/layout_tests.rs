//! Layouts from invented sidecars: every kind of log and encoding, a
//! pregap, a data session, a whole-disc image, and layouts that disagree.

use super::*;

/// MusicBrainz's worked example: six tracks.
const SPEC_STARTS: [u64; 6] = [0, 15213, 32164, 46442, 63264, 80339];
const SPEC_LEAD_OUT: u64 = 95312;
const SPEC_ID: &str = "49HHV7Eb8UKF3aQiNmu1GR8vKTY-";

fn spec() -> Toc {
    Toc {
        starts: SPEC_STARTS.to_vec(),
        lead_out: SPEC_LEAD_OUT,
    }
}

/// The length of each track of a layout, in sectors.
fn lengths(toc: &Toc) -> Vec<u64> {
    let ends = toc
        .starts
        .iter()
        .skip(1)
        .chain(std::iter::once(&toc.lead_out));
    toc.starts.iter().zip(ends).map(|(a, b)| b - a).collect()
}

/// A folder of files with the spec's lengths, named `01 Track.flac` on.
fn spec_files() -> Vec<AudioFile> {
    lengths(&spec())
        .into_iter()
        .zip(1..)
        .map(|(sectors, n)| AudioFile {
            item: 100 + n as i64,
            name: format!("{n:02} Track {n}.flac"),
            track: Some(n),
            title: Some(format!("Track {n}")),
            sectors: Some(sectors),
        })
        .collect()
}

fn sidecar(revision: i64, name: &str, text: &str) -> Sidecar {
    Sidecar {
        revision,
        name: name.to_owned(),
        bytes: text.as_bytes().to_vec(),
    }
}

/// An EAC-style table of the spec's layout, with this many sectors added
/// to every row.
fn eac_table(shift: u64) -> String {
    let toc = spec();
    let mut text = String::from(
        "Exact Audio Copy V1.6 from 23. October 2020\n\nTOC of the extracted CD\n\n     Track |   Start  |  Length  | Start sector | End sector \n    ---------------------------------------------------------\n",
    );
    for (n, (start, length)) in (1..).zip(toc.starts.iter().zip(lengths(&toc))) {
        text.push_str(&format!(
            "        {n:>2}  |  0:00.00 |  0:00.00 |    {:>6}    |   {:>6}   \n",
            start + shift,
            start + shift + length - 1
        ));
    }
    text
}

fn one(folder: &str, files: Vec<AudioFile>, sidecars: Vec<(&str, Sidecar)>) -> Vec<Layout> {
    let mut by_folder: BTreeMap<String, Vec<Sidecar>> = BTreeMap::new();
    for (path, sidecar) in sidecars {
        by_folder.entry(path.to_owned()).or_default().push(sidecar);
    }
    let found = layouts(
        &[Folder {
            path: folder.to_owned(),
            files,
        }],
        &by_folder,
    );
    found
        .into_iter()
        .flat_map(|folder| folder.layouts)
        .collect()
}

#[test]
fn the_worked_disc_id_is_reproduced() {
    assert_eq!(spec().disc_id().as_deref(), Some(SPEC_ID));
}

#[test]
fn a_layout_no_cd_can_have_has_no_disc_id() {
    let empty = Toc {
        starts: vec![],
        lead_out: 10,
    };
    let backwards = Toc {
        starts: vec![0, 500, 400],
        lead_out: 1000,
    };
    let past_the_end = Toc {
        starts: vec![0, 1000],
        lead_out: 1000,
    };
    let too_many = Toc {
        starts: (0..100).map(|n| n * 100).collect(),
        lead_out: 100_000,
    };
    for toc in [empty, backwards, past_the_end, too_many] {
        assert_eq!(toc.disc_id(), None, "{toc:?}");
    }
}

#[test]
fn a_length_cut_to_the_millisecond_gives_back_its_sectors() {
    for n in [1, 2, 3, 14_953, 15_213, 95_312, 330_000] {
        let ms = n * 588 * 1000 / 44_100;
        assert_eq!(sectors(ms, 44_100, 2, true), Some(n), "{n}");
    }
    // Most lengths that are not whole sectors are seen not to be.
    assert_eq!(sectors(14, 44_100, 2, true), None);
    // Only lossless stereo at 44.1 kHz is a CD's.
    assert_eq!(sectors(13, 48_000, 2, true), None);
    assert_eq!(sectors(13, 44_100, 1, true), None);
    assert_eq!(sectors(13, 44_100, 2, false), None);
}

#[test]
fn an_eac_log_in_utf16_gives_the_layout_and_places_each_file() {
    let text = eac_table(0);
    let mut bytes = vec![0xFF, 0xFE];
    bytes.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
    let found = one(
        "Artist/Album",
        spec_files(),
        vec![(
            "Artist/Album",
            Sidecar {
                revision: 7,
                name: "Album.log".to_owned(),
                bytes,
            },
        )],
    );
    assert_eq!(found.len(), 1);
    let layout = &found[0];
    assert_eq!(layout.disc_id, SPEC_ID);
    assert_eq!(layout.found, vec![(Method::Log, 7)]);
    assert_eq!(layout.tracks, (101..=106).collect::<Vec<_>>());
    assert!(!layout.provisional);
    assert_eq!(layout.whole_file, None);
}

#[test]
fn a_log_in_utf16_big_endian_and_in_windows_1252_reads_too() {
    let text = eac_table(0).replace("Exact Audio Copy", "Exact Audio Copy — Café");
    let mut big = vec![0xFE, 0xFF];
    big.extend(text.encode_utf16().flat_map(u16::to_be_bytes));
    assert_eq!(decode(&big), text);
    // Windows-1252: é is E9 and — is 97.
    let latin: Vec<u8> = text
        .chars()
        .map(|c| match c {
            'é' => 0xE9,
            '—' => 0x97,
            c => c as u8,
        })
        .collect();
    assert_eq!(decode(&latin), text);
    let found = one(
        "a",
        spec_files(),
        vec![(
            "a",
            Sidecar {
                revision: 1,
                name: "rip.log".to_owned(),
                bytes: latin,
            },
        )],
    );
    assert_eq!(found[0].disc_id, SPEC_ID);
}

#[test]
fn an_xld_log_in_a_text_file_is_a_sidecar_and_a_text_file_that_is_not_is_not() {
    let xld = eac_table(0).replace(
        "Exact Audio Copy V1.6",
        "X Lossless Decoder version 20230627",
    );
    assert!(keeps("rip.txt", xld.as_bytes()));
    assert!(!keeps("lyrics.txt", b"la la la"));
    assert!(keeps("notes.cue", b"anything"));
    assert!(!keeps("cover.jpg", b"jpeg"));
    assert!(!keeps(
        "huge.log",
        &vec![b'a'; SIDECAR_MAX_BYTES as usize + 1]
    ));
    let found = one("a", spec_files(), vec![("a", sidecar(3, "rip.txt", &xld))]);
    assert_eq!(found[0].found, vec![(Method::Log, 3)]);
}

#[test]
fn a_log_of_an_enhanced_cd_ends_the_audio_before_the_data_session() {
    let mut text = eac_table(0);
    let data_start = SPEC_LEAD_OUT + 11_400;
    text.push_str(&format!(
        "         7  |  0:00.00 |  0:00.00 |    {data_start}    |   {}   \n",
        data_start + 5000
    ));
    let found = one("a", spec_files(), vec![("a", sidecar(1, "a.log", &text))]);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].disc_id, SPEC_ID);
}

#[test]
fn a_cdda2wav_log_gives_its_table() {
    let toc = spec();
    let mut text = String::from("cdda2wav version 3.02\nTable of Contents: starting sectors\n");
    for (n, start) in (1..).zip(&toc.starts) {
        text.push_str(&format!("  {n}.(    {start}),"));
    }
    text.push_str(&format!(" lead-out({})\n", toc.lead_out));
    let found = one(
        "a",
        spec_files(),
        vec![("a", sidecar(1, "audio.log", &text))],
    );
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].disc_id, SPEC_ID);
}

/// A CUE sheet of one file per track, with each track's INDEX 01 at its
/// file's start.
fn cue_per_track(files: &[AudioFile]) -> String {
    let mut text = String::from("PERFORMER \"Someone\"\nTITLE \"An Album\"\n");
    for (n, file) in (1..).zip(files) {
        text.push_str(&format!(
            "FILE \"{}\" WAVE\n  TRACK {n:02} AUDIO\n    INDEX 01 00:00:00\n",
            file.name.replace(".flac", ".wav")
        ));
    }
    text
}

#[test]
fn a_cue_sheet_and_the_files_exact_lengths_give_the_layout() {
    let files = spec_files();
    let found = one(
        "a",
        files.clone(),
        vec![("a", sidecar(4, "a.cue", &cue_per_track(&files)))],
    );
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].disc_id, SPEC_ID);
    assert_eq!(found[0].found, vec![(Method::Cue, 4)]);
}

#[test]
fn a_pregap_the_sheet_declares_and_no_file_holds_is_added() {
    let files = spec_files();
    let toc = spec();
    let mut text = String::new();
    for (n, file) in (1..).zip(&files) {
        text.push_str(&format!(
            "FILE \"{}\" WAVE\n  TRACK {n:02} AUDIO\n",
            file.name
        ));
        if n == 3 {
            // A pregap the sheet declares, which no file holds.
            text.push_str("    PREGAP 00:02:00\n");
        }
        text.push_str("    INDEX 01 00:00:00\n");
    }
    // With the pregap declared, every track from the third on starts two
    // seconds later.
    let expected = Toc {
        starts: toc
            .starts
            .iter()
            .enumerate()
            .map(|(i, s)| if i >= 2 { s + 150 } else { *s })
            .collect(),
        lead_out: toc.lead_out + 150,
    };
    let found = one("a", files.clone(), vec![("a", sidecar(1, "a.cue", &text))]);
    assert_eq!(found[0].toc, expected);
}

#[test]
fn a_pregap_kept_at_the_end_of_the_file_before_leaves_the_tracks_where_the_drive_put_them() {
    // EAC's sheet for gaps appended to the previous track: track 2's
    // INDEX 00 is in track 1's file, and its INDEX 01 starts its own.
    let files = spec_files();
    let mut text = String::new();
    for (n, file) in (1..).zip(&files) {
        text.push_str(&format!("FILE \"{}\" WAVE\n", file.name));
        if n == 1 {
            text.push_str("  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n");
        } else {
            text.push_str("    INDEX 01 00:00:00\n");
        }
        if n < 6 {
            let length = file.sectors.unwrap();
            let gap = length - 150;
            let (m, s, f) = (gap / 75 / 60, gap / 75 % 60, gap % 75);
            text.push_str(&format!(
                "  TRACK {:02} AUDIO\n    INDEX 00 {m:02}:{s:02}:{f:02}\n",
                n + 1
            ));
        }
    }
    let found = one("a", files, vec![("a", sidecar(1, "a.cue", &text))]);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].disc_id, SPEC_ID);
    assert_eq!(found[0].tracks.len(), 6);
}

#[test]
fn a_cue_sheet_of_an_enhanced_cd_ends_at_its_data_track() {
    let files = spec_files();
    let mut text = cue_per_track(&files);
    let last = files.last().unwrap().sectors.unwrap();
    let (m, s, f) = (last / 75 / 60, last / 75 % 60, last % 75);
    text.push_str(&format!(
        "  TRACK 07 MODE1/2352\n    INDEX 00 {m:02}:{s:02}:{f:02}\n    INDEX 01 {m:02}:{s:02}:{f:02}\n"
    ));
    // A CDDB ID that counts the data track as a seventh.
    let cddb = format!("REM DISCID {}{:02x}\n", &spec().cddb()[..6], 7);
    text.insert_str(0, &cddb);
    let found = one("a", files, vec![("a", sidecar(1, "a.cue", &text))]);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].disc_id, SPEC_ID);
    assert!(!found[0].provisional);
}

#[test]
fn a_cue_sheet_whose_cddb_id_disagrees_is_provisional() {
    let files = spec_files();
    let text = format!("REM DISCID 0badf00d\n{}", cue_per_track(&files));
    let found = one("a", files.clone(), vec![("a", sidecar(1, "a.cue", &text))]);
    assert_eq!(found.len(), 1);
    assert!(found[0].provisional);
    // And one whose ID agrees is not.
    let text = format!("REM DISCID {}\n{}", spec().cddb(), cue_per_track(&files));
    let found = one("a", files, vec![("a", sidecar(1, "a.cue", &text))]);
    assert!(!found[0].provisional);
}

#[test]
fn a_whole_disc_image_and_its_cue_sheet_give_the_layout() {
    let file = AudioFile {
        item: 9,
        name: "Album.flac".to_owned(),
        track: None,
        title: None,
        sectors: Some(SPEC_LEAD_OUT),
    };
    let mut text = String::from("FILE \"Album.wav\" WAVE\n");
    for (n, start) in (1..).zip(&SPEC_STARTS) {
        let (m, s, f) = (start / 75 / 60, start / 75 % 60, start % 75);
        text.push_str(&format!(
            "  TRACK {n:02} AUDIO\n    INDEX 01 {m:02}:{s:02}:{f:02}\n"
        ));
    }
    let found = one("a", vec![file], vec![("a", sidecar(1, "Album.cue", &text))]);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].disc_id, SPEC_ID);
    assert_eq!(found[0].whole_file, Some(9));
    assert!(found[0].tracks.is_empty());
}

#[test]
fn a_drive_log_and_gap_stripped_files_give_two_layouts_of_one_disc() {
    // The files lost the drive's gaps: each is 100 sectors shorter, so the
    // log's layout and the files' disagree.
    let mut files = spec_files();
    for file in &mut files {
        file.sectors = file.sectors.map(|n| n - 100);
    }
    let found = one(
        "a",
        files.clone(),
        vec![
            ("a", sidecar(1, "a.log", &eac_table(0))),
            ("a", sidecar(2, "a.cue", &cue_per_track(&files))),
        ],
    );
    assert_eq!(found.len(), 2);
    assert_eq!(found[0].disc_id, SPEC_ID);
    assert_eq!(found[0].found, vec![(Method::Log, 1)]);
    assert!(found[0].tracks.is_empty(), "the log's lengths fit no file");
    assert_ne!(found[1].disc_id, SPEC_ID);
    assert_eq!(found[1].found, vec![(Method::Cue, 2)]);
    assert_eq!(found[1].tracks.len(), 6);
}

#[test]
fn a_log_and_a_cue_sheet_that_agree_are_one_layout_found_twice() {
    let files = spec_files();
    let found = one(
        "a",
        files.clone(),
        vec![
            ("a", sidecar(1, "a.cue", &cue_per_track(&files))),
            ("a", sidecar(2, "a.log", &eac_table(0))),
        ],
    );
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].found, vec![(Method::Log, 2), (Method::Cue, 1)]);
}

#[test]
fn a_cue_sheet_of_renamed_files_is_matched_by_track_number() {
    let files = spec_files();
    let mut text = String::new();
    for n in 1..=6 {
        text.push_str(&format!(
            "FILE \"Someone - {n:02} - Track {n}.wav\" WAVE\n  TRACK {n:02} AUDIO\n    INDEX 01 00:00:00\n"
        ));
    }
    let found = one("a", files.clone(), vec![("a", sidecar(1, "a.cue", &text))]);
    assert_eq!(found[0].found, vec![(Method::RenamedCue, 1)]);
    assert_eq!(found[0].disc_id, SPEC_ID);
    // Titles that do not end the names it gave, and no CDDB ID: nothing.
    let mut wrong = files;
    for file in &mut wrong {
        file.title = Some("Something Else".to_owned());
    }
    assert!(one("a", wrong, vec![("a", sidecar(1, "a.cue", &text))]).is_empty());
}

#[test]
fn lengths_in_order_count_when_a_printed_accuraterip_id_agrees() {
    let files = spec_files();
    let bounds: Vec<u64> = SPEC_STARTS.iter().copied().chain([SPEC_LEAD_OUT]).collect();
    let first: u64 = bounds.iter().sum();
    let second: u64 = bounds.iter().zip(1..).map(|(n, i)| i * n.max(&1)).sum();
    let report = format!(
        "[CUETools log; Date: 1/2/2020]\nAccurateRip ID: {first:08x}-{second:08x}-560a5f07\n"
    );
    let found = one(
        "a",
        files.clone(),
        vec![("a", sidecar(5, "a.accurip", &report))],
    );
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].found, vec![(Method::AccurateRip, 5)]);
    assert_eq!(found[0].disc_id, SPEC_ID);
    // A report that prints another ID gives nothing.
    let other = one(
        "a",
        files,
        vec![(
            "a",
            sidecar(5, "a.accurip", "AccurateRip ID: 00000001-00000002-03"),
        )],
    );
    assert!(other.is_empty());
}

#[test]
fn sidecars_in_a_subfolder_without_audio_count_for_the_folder() {
    let found = one(
        "Album",
        spec_files(),
        vec![("Album/Misc", sidecar(1, "rip.log", &eac_table(0)))],
    );
    assert_eq!(found.len(), 1);
}

#[test]
fn a_log_above_two_disc_folders_counts_for_the_disc_it_fits() {
    // One log of two discs, in the folder above both: disc 2's table is
    // the spec's, disc 1's another.
    let mut other = String::new();
    for n in 0..3 {
        other.push_str(&format!(
            "  {} | 0:00.00 | 0:00.00 | {} | {}\n",
            n + 1,
            n * 1000,
            n * 1000 + 999
        ));
    }
    let log = format!("{other}\n\n{}", eac_table(0));
    let mut sidecars = BTreeMap::new();
    sidecars.insert("Set".to_owned(), vec![sidecar(1, "Set.log", &log)]);
    let disc1 = Folder {
        path: "Set/CD1".to_owned(),
        files: spec_files()
            .into_iter()
            .map(|f| AudioFile {
                sectors: Some(1),
                ..f
            })
            .collect(),
    };
    let disc2 = Folder {
        path: "Set/CD2".to_owned(),
        files: spec_files(),
    };
    let found = layouts(&[disc1, disc2], &sidecars);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].folder, "Set/CD2");
    assert_eq!(found[0].layouts[0].disc_id, SPEC_ID);
}

#[test]
fn a_folder_with_no_sidecars_has_no_layout() {
    assert!(one("a", spec_files(), vec![]).is_empty());
}

#[test]
fn lengths_in_order_count_when_the_cddb_id_of_a_sheet_for_other_files_agrees() {
    // The sheet names one image the folder no longer holds; the folder
    // holds the tracks.
    let text = format!(
        "REM DISCID {}\nFILE \"Album.wav\" WAVE\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n",
        spec().cddb()
    );
    let found = one(
        "a",
        spec_files(),
        vec![("a", sidecar(2, "Album.cue", &text))],
    );
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].found, vec![(Method::Cddb, 2)]);
    assert_eq!(found[0].disc_id, SPEC_ID);
}

#[test]
fn an_accuraterip_id_counts_the_pregap_its_report_prints() {
    // A disc whose first track starts after a 32-frame pregap.
    let shift = 32;
    let bounds: Vec<u64> = SPEC_STARTS
        .iter()
        .copied()
        .chain([SPEC_LEAD_OUT])
        .map(|n| n + shift)
        .collect();
    let first: u64 = bounds.iter().sum();
    let second: u64 = bounds.iter().zip(1..).map(|(n, i)| i * n.max(&1)).sum();
    let report =
        format!("Pregap length 00:00:32\nAccurateRip ID: {first:08x}-{second:08x}-560a5f07\n");
    let found = one(
        "a",
        spec_files(),
        vec![("a", sidecar(5, "a.accurip", &report))],
    );
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].toc.starts[0], 32);
    assert_eq!(found[0].toc.lead_out, SPEC_LEAD_OUT + 32);
}

#[test]
fn a_disc_of_one_track_is_that_track_and_not_an_image() {
    let file = AudioFile {
        item: 7,
        name: "01 Long.flac".to_owned(),
        track: Some(1),
        title: None,
        sectors: Some(1000),
    };
    let log = "Exact Audio Copy\n  1 | 0:00.00 | 0:13.25 | 0 | 999\n";
    let found = one("a", vec![file], vec![("a", sidecar(1, "a.log", log))]);
    assert_eq!(found[0].tracks, [7]);
    assert_eq!(found[0].whole_file, None);
}

#[test]
fn a_sheet_is_checked_by_its_own_cddb_id_and_not_another_sheets() {
    let files = spec_files();
    // This sheet fits the files but prints a CDDB ID that disagrees; the
    // other names files the folder does not hold and prints the right one.
    let wrong = format!("REM DISCID 0badf00d\n{}", cue_per_track(&files));
    let other = format!(
        "REM DISCID {}\nFILE \"elsewhere.wav\" WAVE\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n",
        spec().cddb()
    );
    let found = one(
        "a",
        files,
        vec![
            ("a", sidecar(1, "a.cue", &wrong)),
            ("a", sidecar(2, "b.cue", &other)),
        ],
    );
    // The first sheet does not count: its own ID disagrees. The files'
    // lengths in order do, by the ID the other sheet prints.
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].disc_id, SPEC_ID);
    assert_eq!(found[0].found, vec![(Method::Cddb, 2)]);
    assert!(!found[0].provisional);
}

#[test]
fn numbers_no_cd_can_hold_are_read_as_nothing_and_never_overflow() {
    let huge_log = format!(
        "Exact Audio Copy\n  1 | 0:00.00 | 0:01.00 | 0 | {}\n  1 | 0:00.00 | 0:01.00 | 0 | 18446744073709551615\n",
        u64::from(u32::MAX)
    );
    assert!(
        one(
            "a",
            spec_files(),
            vec![("a", sidecar(1, "a.log", &huge_log))]
        )
        .is_empty()
    );
    let huge_cue = "FILE \"01 Track 1.wav\" WAVE\n  TRACK 01 AUDIO\n    PREGAP 99999999999999:00:00\n    INDEX 01 00:00:00\n";
    // The pregap that cannot be is passed over, and the sheet read without it.
    let found = one(
        "a",
        spec_files(),
        vec![("a", sidecar(1, "a.cue", huge_cue))],
    );
    assert!(found.iter().all(|layout| layout.toc.lead_out <= MAX_SECTOR));
    let huge_report =
        "Pregap length 99999999999999999:00:00\nAccurateRip ID: 00000000-00000000-00000000\n";
    let _ = one(
        "a",
        spec_files(),
        vec![("a", sidecar(1, "a.accurip", huge_report))],
    );
    assert_eq!(sectors(u64::MAX, 44_100, 2, true), None);
    let past = Toc {
        starts: vec![0],
        lead_out: u64::from(u32::MAX),
    };
    assert_eq!(past.disc_id(), None);
}
