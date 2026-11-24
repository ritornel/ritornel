//! Album proposals through the engine, on an invented album whose bonus
//! disc is filed apart under another title: what a proposal rests on, and
//! a preview that writes nothing. See "Album proposals" in
//! docs/internals/corrections.md.

use ritornel_contract::{Choice, DiscsParams, ProposalChange};

use crate::scan_tests::Rig;
use crate::{Asked, Command, Fingerprint, Id, KeptReply, WriteRequest};

/// An AIFF file of CD audio exactly `sectors` long, its samples made from
/// `seed`, tagged in an ID3 chunk.
pub(crate) fn aiff(sectors: u32, seed: u32, tags: &[(&str, &str)]) -> Vec<u8> {
    let frames = sectors * 588;
    let mut id3_frames = Vec::new();
    for (id, text) in tags {
        id3_frames.extend_from_slice(id.as_bytes());
        id3_frames.extend_from_slice(&(text.len() as u32 + 1).to_be_bytes());
        id3_frames.extend_from_slice(&[0, 0, 0]);
        id3_frames.extend_from_slice(text.as_bytes());
    }
    let size = id3_frames.len() as u32;
    let synchsafe = [
        (size >> 21) & 0x7f,
        (size >> 14) & 0x7f,
        (size >> 7) & 0x7f,
        size & 0x7f,
    ]
    .map(|b| b as u8);
    let mut id3 = b"ID3\x03\x00\x00".to_vec();
    id3.extend_from_slice(&synchsafe);
    id3.extend_from_slice(&id3_frames);
    if id3.len() % 2 == 1 {
        id3.push(0);
    }
    let mut samples = Vec::with_capacity(frames as usize * 4);
    for frame in 0..frames {
        let sample = (frame.wrapping_mul(seed.wrapping_mul(2_654_435_761)) >> 16) as u16;
        samples.extend_from_slice(&sample.to_be_bytes());
        samples.extend_from_slice(&sample.to_be_bytes());
    }
    let mut chunks = Vec::new();
    chunks.extend_from_slice(b"COMM");
    chunks.extend_from_slice(&18u32.to_be_bytes());
    chunks.extend_from_slice(&2u16.to_be_bytes());
    chunks.extend_from_slice(&frames.to_be_bytes());
    chunks.extend_from_slice(&16u16.to_be_bytes());
    chunks.extend_from_slice(&[0x40, 0x0E, 0xAC, 0x44, 0, 0, 0, 0, 0, 0]);
    chunks.extend_from_slice(b"SSND");
    chunks.extend_from_slice(&(8 + samples.len() as u32).to_be_bytes());
    chunks.extend_from_slice(&[0; 8]);
    chunks.extend_from_slice(&samples);
    chunks.extend_from_slice(b"ID3 ");
    chunks.extend_from_slice(&(id3.len() as u32).to_be_bytes());
    chunks.extend_from_slice(&id3);
    let mut file = b"FORM".to_vec();
    file.extend_from_slice(&(4 + chunks.len() as u32).to_be_bytes());
    file.extend_from_slice(b"AIFF");
    file.extend_from_slice(&chunks);
    file
}

pub(crate) fn write(rig: &mut Rig, path: &str, bytes: &[u8]) {
    let to = rig.root().join(path);
    std::fs::create_dir_all(to.parent().unwrap()).unwrap();
    std::fs::write(&to, bytes).unwrap();
    rig.stamp(&to);
}

/// An EAC table of these track lengths.
fn log(lengths: &[u32]) -> String {
    let mut text = String::from(
        "Exact Audio Copy V1.6\n Track | Start | Length | Start sector | End sector\n",
    );
    let mut start = 0;
    for (n, length) in (1..).zip(lengths) {
        text.push_str(&format!(
            "  {n} | 0:00.00 | 0:01.00 | {start} | {}\n",
            start + length - 1
        ));
        start += length;
    }
    text
}

const MAIN: &str = "YuBQA_w4y7cbiR1haQP4UVic4Ko-";
const BONUS: &str = "oAA1ZY.4QOAq6tpdobDdb4M9qyc-";
pub(crate) const GROUP: &str = "00000000-0000-4000-8000-0000000000b1";

/// The main disc, three songs tagged "Tin Weather", and the bonus disc of
/// two, tagged "Tin Weather (Bonus Disc)" in a folder of its own, each with
/// its rip log, scanned; and MusicBrainz's replies about both kept.
pub(crate) fn album() -> Rig {
    album_with(&[])
}

/// The same, with `main` tags also on each file of the main disc.
pub(crate) fn album_with(main: &[(&str, &str)]) -> Rig {
    let mut rig = Rig::new();
    let songs = [
        ("Moss Arcade/Tin Weather", "Tin Weather", "Paper Kites", 75),
        ("Moss Arcade/Tin Weather", "Tin Weather", "Low Tide", 120),
        ("Moss Arcade/Tin Weather", "Tin Weather", "Salt Roads", 90),
        (
            "Moss Arcade/Tin Weather Bonus",
            "Tin Weather (Bonus Disc)",
            "Glass Harbour",
            200,
        ),
        (
            "Moss Arcade/Tin Weather Bonus",
            "Tin Weather (Bonus Disc)",
            "Far Lights",
            300,
        ),
    ];
    let mut tracks = std::collections::HashMap::new();
    for (seed, (folder, album, title, sectors)) in (1..).zip(songs) {
        let track: &mut u32 = tracks.entry(folder).or_default();
        *track += 1;
        let number = track.to_string();
        let mut tags = vec![
            ("TIT2", title),
            ("TPE1", "Moss Arcade"),
            ("TPE2", "Moss Arcade"),
            ("TALB", album),
            ("TRCK", number.as_str()),
        ];
        if album == "Tin Weather" {
            tags.extend_from_slice(main);
        }
        let bytes = aiff(sectors, seed, &tags);
        write(
            &mut rig,
            &format!("{folder}/{number:0>2} {title}.aiff"),
            &bytes,
        );
    }
    write(
        &mut rig,
        "Moss Arcade/Tin Weather/rip.log",
        log(&[75, 120, 90]).as_bytes(),
    );
    write(
        &mut rig,
        "Moss Arcade/Tin Weather Bonus/rip.log",
        log(&[200, 300]).as_bytes(),
    );
    rig.scan();
    let discs: Vec<String> = rig
        .library
        .discs(&DiscsParams::default())
        .unwrap()
        .discs
        .into_iter()
        .map(|disc| disc.disc_id)
        .collect();
    assert_eq!(discs, [MAIN, BONUS]);
    let replies = vec![
        KeptReply {
            key: MAIN.to_owned(),
            status: 200,
            body: include_str!("../../../fixtures/musicbrainz/disc-tin-weather.json").to_owned(),
        },
        KeptReply {
            key: BONUS.to_owned(),
            status: 200,
            body: include_str!("../../../fixtures/musicbrainz/disc-tin-weather-bonus.json")
                .to_owned(),
        },
    ];
    let request = WriteRequest {
        method: "corrections.lookup",
        command: Some(Command {
            id: Id::from_bytes([0xe1; 16]),
            fingerprint: Fingerprint::of("keep", &[1]),
        }),
        client: None,
    };
    let asked = Asked {
        catalogue: "musicbrainz",
        kind: "disc",
        includes: "artist-credits+release-groups+recordings",
    };
    rig.library
        .keep_replies(request, asked, replies, vec![], |_, _| String::new())
        .unwrap();
    rig
}

#[test]
fn the_bonus_disc_filed_apart_is_proposed_as_the_album_and_says_what_it_rests_on() {
    let rig = album();
    let summary = rig.library.corrections_summary().unwrap();
    assert_eq!((summary.open, summary.groups, summary.moves), (2, 1, 2));
    let list = rig.library.proposals().unwrap().proposals;
    assert_eq!(list.len(), 2);
    assert_eq!(list[0].group_id, list[1].group_id);
    let bonus = list
        .iter()
        .find(|proposal| proposal.folder.ends_with("Bonus"))
        .unwrap();
    let (_, whole) = rig.library.proposal(bonus.proposal_id).unwrap();
    assert_eq!(whole.files.len(), 2);
    // Its own disc, and the main disc its join was worked out with.
    let rests_on: Vec<(&str, u32)> = whole
        .rests_on
        .iter()
        .map(|rest| (rest.disc_id.as_str(), rest.releases))
        .collect();
    assert_eq!(rests_on, [(MAIN, 2), (BONUS, 1)]);
    let candidate = &whole.candidates[0];
    assert_eq!(candidate.release_group, GROUP);
    assert_eq!(candidate.change, ProposalChange::Moves);
    let disc_numbers: Vec<&str> = candidate
        .values
        .iter()
        .filter(|value| value.field == "disc_number")
        .map(|value| value.value.as_deref().unwrap_or_default())
        .collect();
    assert_eq!(disc_numbers, ["2", "2"]);
    assert!(
        candidate
            .values
            .iter()
            .any(|value| value.field == "album" && value.value.as_deref() == Some("Tin Weather"))
    );
}

#[test]
fn the_preview_is_one_album_of_two_discs_and_writes_nothing() {
    let rig = album();
    let list = rig.library.proposals().unwrap().proposals;
    let (revision, before) = (rig.revision(), rig.dump());
    let choices: Vec<Choice> = list
        .iter()
        .map(|proposal| Choice {
            proposal_id: proposal.proposal_id,
            release_group: None,
        })
        .collect();
    let shown = rig.library.preview(&choices).unwrap();
    let titles: Vec<(Option<&str>, u32, u32)> = shown
        .now
        .iter()
        .map(|album| (album.title.as_deref(), album.discs, album.songs))
        .collect();
    assert_eq!(
        titles,
        [
            (Some("Tin Weather"), 1, 3),
            (Some("Tin Weather (Bonus Disc)"), 1, 2)
        ]
    );
    let [after] = shown.after.as_slice() else {
        panic!("{:?}", shown.after);
    };
    assert_eq!(
        (after.title.as_deref(), after.discs, after.songs),
        (Some("Tin Weather"), 2, 5)
    );
    assert_eq!(
        (after.primary_type.as_deref(), after.first_year),
        (Some("album"), Some(2004))
    );
    // The bonus disc's proposal alone gives the same album.
    let bonus = list
        .iter()
        .find(|proposal| proposal.folder.ends_with("Bonus"))
        .unwrap();
    let alone = rig
        .library
        .preview(&[Choice {
            proposal_id: bonus.proposal_id,
            release_group: Some(GROUP.to_owned()),
        }])
        .unwrap();
    assert_eq!(
        (
            alone.after.len(),
            alone.after[0].discs,
            alone.after[0].songs
        ),
        (1, 2, 5)
    );
    assert_eq!((rig.revision(), rig.dump()), (revision, before));
}

#[test]
fn proposals_derived_again_keep_their_ids_and_write_nothing_new() {
    let mut rig = album();
    let ids: Vec<Id> = rig
        .library
        .proposals()
        .unwrap()
        .proposals
        .iter()
        .map(|p| p.proposal_id)
        .collect();
    let before = rig.rows("SELECT public_id, rev FROM proposal ORDER BY id");
    // A file changes: the layouts are due, and the proposals are worked
    // out again with them.
    write(
        &mut rig,
        "Moss Arcade/Tin Weather Bonus/notes.cue",
        b"REM nothing",
    );
    rig.scan();
    let again: Vec<Id> = rig
        .library
        .proposals()
        .unwrap()
        .proposals
        .iter()
        .map(|p| p.proposal_id)
        .collect();
    assert_eq!(again, ids);
    assert_eq!(
        rig.rows("SELECT public_id, rev FROM proposal ORDER BY id"),
        before
    );
}

#[test]
fn proposals_of_another_proposer_are_worked_out_again_when_the_server_starts() {
    let rig = album();
    let before = rig.rows("SELECT public_id, fingerprint FROM proposal ORDER BY public_id");
    rig.library
        .write(
            WriteRequest {
                method: "test",
                command: None,
                client: None,
            },
            |write| {
                write.connection().execute_batch(
                    "DELETE FROM proposal_candidate; DELETE FROM proposal_item;
                     DELETE FROM proposal_evidence; DELETE FROM proposal;
                     UPDATE library_setting SET value_json = '0'
                     WHERE key = 'corrections.proposer_version';",
                )?;
                Ok(String::new())
            },
        )
        .unwrap();
    let request = WriteRequest {
        method: crate::FACTS_METHOD,
        command: None,
        client: None,
    };
    assert!(rig.library.refresh_corrections(request).unwrap().is_some());
    assert_eq!(
        rig.rows("SELECT public_id, fingerprint FROM proposal ORDER BY public_id"),
        before
    );
    assert!(rig.library.refresh_corrections(request).unwrap().is_none());
}

#[test]
fn a_missing_file_keeps_its_place_and_its_proposal() {
    let mut rig = album();
    let ids: Vec<Id> = rig
        .library
        .proposals()
        .unwrap()
        .proposals
        .iter()
        .map(|p| p.proposal_id)
        .collect();
    let gone = rig
        .root()
        .join("Moss Arcade/Tin Weather Bonus/02 Far Lights.aiff");
    let kept = std::fs::read(&gone).unwrap();
    std::fs::remove_file(&gone).unwrap();
    rig.scan();
    let again: Vec<Id> = rig
        .library
        .proposals()
        .unwrap()
        .proposals
        .iter()
        .map(|p| p.proposal_id)
        .collect();
    assert_eq!(again, ids);
    std::fs::write(&gone, kept).unwrap();
    rig.stamp(&gone);
    rig.scan();
    assert_eq!(rig.library.proposals().unwrap().proposals.len(), 2);
}

#[test]
fn a_preview_refuses_a_proposal_twice_or_choices_that_disagree() {
    let rig = album();
    let list = rig.library.proposals().unwrap().proposals;
    let twice = [
        Choice {
            proposal_id: list[0].proposal_id,
            release_group: None,
        },
        Choice {
            proposal_id: list[0].proposal_id,
            release_group: Some(GROUP.to_owned()),
        },
    ];
    assert!(matches!(
        rig.library.preview(&twice),
        Err(crate::Error::Invalid(_))
    ));
    let shown = rig
        .library
        .preview(&[Choice {
            proposal_id: list[1].proposal_id,
            release_group: None,
        }])
        .unwrap();
    // What the bonus disc's files say now, beside what they would say.
    let album = shown
        .values
        .iter()
        .find(|value| value.field == "album")
        .unwrap();
    assert_eq!(album.now.as_deref(), Some("Tin Weather (Bonus Disc)"));
    assert_eq!(album.value.as_deref(), Some("Tin Weather"));
    assert_eq!(album.root, "main");
}

/// A song of the invented album as an AIFF file, tagged.
pub(crate) fn aiff_for_tests(
    sectors: u32,
    seed: u32,
    title: &str,
    album: &str,
    track: &str,
) -> Vec<u8> {
    aiff(
        sectors,
        seed,
        &[
            ("TIT2", title),
            ("TPE1", "Moss Arcade"),
            ("TPE2", "Moss Arcade"),
            ("TALB", album),
            ("TRCK", track),
        ],
    )
}

#[test]
fn a_field_of_one_value_has_it_once_and_a_credit_as_many_as_it_names() {
    use crate::proposal_store::{now_of, values_of};
    let facts = ritornel_domain::Facts {
        album: Some("Tin Weather".to_owned()),
        album_artist: vec!["Moss Arcade".to_owned(), "Salt Road".to_owned()],
        release_group_mbid: Some(GROUP.to_owned()),
        ..ritornel_domain::Facts::default()
    };
    // Past the first, a field of one value has none: what reads a field
    // value by value comes to its end.
    for field in ["album", "mb_release_group", "disc_number"] {
        assert_eq!(now_of(&facts, field, 1), None, "{field}");
        assert!(values_of(&facts, field) <= 1, "{field}");
    }
    assert_eq!(values_of(&facts, "album_artist"), 2);
    assert_eq!(
        now_of(&facts, "album_artist", 1).as_deref(),
        Some("Salt Road")
    );
    assert_eq!(now_of(&facts, "album_artist", 2), None);
}
