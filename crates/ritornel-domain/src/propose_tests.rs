//! Album proposals on an invented album whose bonus disc is filed apart.

use super::*;
use crate::Facts;
use crate::library::Item;
use crate::values::{AlbumShape, preview};

const GROUP: &str = "00000000-0000-4000-8000-0000000000b1";

fn file(id: i64, folder: &str, album: &str, track: i64, title: &str) -> Item {
    Item {
        id,
        folder: format!("1/{folder}"),
        audio: Some(id),
        lossless: true,
        retired: false,
        first_seen_ms: id,
        cover: None,
        facts: Facts {
            title: Some(title.to_owned()),
            artist: vec!["Moss Arcade".to_owned()],
            album: Some(album.to_owned()),
            album_artist: vec!["Moss Arcade".to_owned()],
            track: Some(track),
            duration_ms: Some(100_000 + id * 1000),
            ..Facts::default()
        },
        held: Vec::new(),
        current: None,
        prior: None,
        prior_recording: None,
    }
}

/// The main disc of three songs in one folder, and the bonus disc of two in
/// another, under another title.
fn library() -> Library {
    Library {
        items: vec![
            file(
                1,
                "Moss Arcade/Tin Weather",
                "Tin Weather",
                1,
                "Paper Kites",
            ),
            file(2, "Moss Arcade/Tin Weather", "Tin Weather", 2, "Low Tide"),
            file(3, "Moss Arcade/Tin Weather", "Tin Weather", 3, "Salt Roads"),
            file(
                4,
                "Moss Arcade/Tin Weather Bonus",
                "Tin Weather (Bonus Disc)",
                1,
                "Glass Harbour",
            ),
            file(
                5,
                "Moss Arcade/Tin Weather Bonus",
                "Tin Weather (Bonus Disc)",
                2,
                "Far Lights",
            ),
        ],
        ..Library::default()
    }
}

fn release(id: &str, position: u32) -> ReplyRelease {
    ReplyRelease {
        reply: 1,
        release: id.to_owned(),
        group: GROUP.to_owned(),
        group_title: Some("Tin Weather".to_owned()),
        artist: "Moss Arcade".to_owned(),
        primary_type: Some("Album".to_owned()),
        first_date: Some("2004-05-11".to_owned()),
        title: Some(format!("Tin Weather ({id})")),
        date: Some("2004-05-11".to_owned()),
        country: Some("XW".to_owned()),
        media: Some(2),
        positions: vec![position],
    }
}

fn discs() -> Vec<FolderDiscs> {
    vec![
        FolderDiscs {
            folder: "1/Moss Arcade/Tin Weather".to_owned(),
            layouts: vec![DiscLayout {
                disc_id: "main".to_owned(),
                provisional: false,
                found: String::new(),
                items: vec![1, 2, 3],
                positions: vec![1, 2, 3],
            }],
        },
        FolderDiscs {
            folder: "1/Moss Arcade/Tin Weather Bonus".to_owned(),
            layouts: vec![DiscLayout {
                disc_id: "bonus".to_owned(),
                provisional: false,
                found: String::new(),
                items: vec![4, 5],
                positions: vec![1, 2],
            }],
        },
    ]
}

fn replies() -> HashMap<String, Vec<ReplyRelease>> {
    HashMap::from([
        (
            "main".to_owned(),
            vec![release("standard", 1), release("deluxe", 1)],
        ),
        ("bonus".to_owned(), vec![release("deluxe", 2)]),
    ])
}

/// The album proposals alone, leaving out any edition.
fn albums(proposals: Vec<Proposal>) -> Vec<Proposal> {
    proposals
        .into_iter()
        .filter(|proposal| proposal.kind == "album")
        .collect()
}

fn ids() -> HashMap<i64, [u8; 16]> {
    (1..=5).map(|id| (id, [id as u8; 16])).collect()
}

fn values_of(candidate: &Candidate, item: i64) -> Vec<(&'static str, &str)> {
    candidate
        .values
        .iter()
        .filter(|value| value.item == item)
        .map(|value| (value.field, value.value.as_deref().unwrap_or("")))
        .collect()
}

#[test]
fn each_disc_is_proposed_as_the_album_and_the_bonus_disc_joins_the_first() {
    let proposals = albums(propose(
        &library(),
        &discs(),
        &replies(),
        &ids(),
        &crate::Policy::current(),
    ));
    assert_eq!(proposals.len(), 2);
    let (main, bonus) = (&proposals[0], &proposals[1]);
    assert_eq!((main.anchor, bonus.anchor), (1, 4));
    assert_eq!((main.group, bonus.group), (1, 1), "one group");
    assert_eq!(main.items, [1, 2, 3]);
    // The bonus disc's own evidence, and the main disc's its join rests on.
    let rests: Vec<&str> = bonus
        .rests_on
        .iter()
        .map(|rest| rest.disc_id.as_str())
        .collect();
    assert_eq!(rests, ["bonus", "main"]);
    // The bonus disc is joined onto the main disc, the album's first.
    assert_eq!(bonus.candidates[0].onto, [1, 2, 3]);
    assert!(main.candidates[0].onto.is_empty());
    let candidate = &bonus.candidates[0];
    assert_eq!(candidate.release_group, GROUP);
    assert_eq!(candidate.change, Change::Moves);
    assert_eq!(
        values_of(candidate, 4),
        [
            ("album", "Tin Weather"),
            ("album_artist", "Moss Arcade"),
            ("disc_number", "2"),
            ("mb_release_group", GROUP),
            ("original_date", "2004-05-11"),
            ("primary_type", "album"),
        ]
    );
    // The main disc keeps its title and takes its number.
    assert_eq!(
        values_of(&main.candidates[0], 1),
        [
            ("disc_number", "1"),
            ("mb_release_group", GROUP),
            ("original_date", "2004-05-11"),
            ("primary_type", "album"),
        ]
    );
    // Asked again, the same proposals, IDs and fingerprints.
    assert_eq!(
        albums(propose(
            &library(),
            &discs(),
            &replies(),
            &ids(),
            &crate::Policy::current()
        )),
        proposals
    );
}

#[test]
fn the_preview_of_the_group_is_one_album_of_two_discs() {
    let library = library();
    let proposals = albums(propose(
        &library,
        &discs(),
        &replies(),
        &ids(),
        &crate::Policy::current(),
    ));
    let values: Vec<_> = proposals
        .iter()
        .flat_map(|proposal| proposal.candidates[0].values.clone())
        .collect();
    let shown = preview(&library, &values, &[], &crate::Policy::current());
    let shape = |title: &str, discs, songs| AlbumShape {
        title: Some(title.to_owned()),
        artist: Some("Moss Arcade".to_owned()),
        primary_type: None,
        first_year: None,
        releases: 1,
        discs,
        songs,
    };
    assert_eq!(
        shown.now,
        [
            shape("Tin Weather", 1, 3),
            shape("Tin Weather (Bonus Disc)", 1, 2)
        ]
    );
    assert_eq!(
        shown.after,
        [AlbumShape {
            primary_type: Some("album".to_owned()),
            first_year: Some(2004),
            ..shape("Tin Weather", 2, 5)
        }]
    );
    assert_eq!((shown.songs_joined, shown.songs_parted), (0, 0));
    // The bonus disc's proposal alone joins it too: its files take the
    // first disc's title and the second disc's number.
    let alone = preview(
        &library,
        &proposals[1].candidates[0].values,
        &[],
        &crate::Policy::current(),
    );
    assert_eq!(alone.after.len(), 1);
    assert_eq!((alone.after[0].discs, alone.after[0].songs), (2, 5));
}

#[test]
fn a_disc_with_no_reply_or_only_a_provisional_layout_is_not_proposed_for() {
    let mut discs = discs();
    discs[1].layouts[0].provisional = true;
    let mut replies = replies();
    replies.remove("main");
    assert!(
        propose(
            &library(),
            &discs,
            &replies,
            &ids(),
            &crate::Policy::current()
        )
        .is_empty()
    );
}

#[test]
fn two_copies_of_the_same_disc_are_not_joined() {
    let mut replies = replies();
    replies.insert("bonus".to_owned(), vec![release("standard", 1)]);
    let proposals = albums(propose(
        &library(),
        &discs(),
        &replies,
        &ids(),
        &crate::Policy::current(),
    ));
    for proposal in &proposals {
        let candidate = &proposal.candidates[0];
        assert!(
            candidate
                .values
                .iter()
                .all(|value| value.field != "disc_number")
        );
        // Not joined, but one album ID would put both copies on one tile.
        assert_eq!(candidate.change, Change::Moves);
    }
}

#[test]
fn a_value_holds_its_disc_number_and_title_against_the_disc_rules() {
    let mut library = library();
    for item in &mut library.items {
        item.facts.album = Some("Tin Weather (Disc 2)".to_owned());
    }
    let values = vec![Value {
        item: 1,
        field: "album",
        ordinal: 0,
        value: Some("Tin Weather (Disc 2)".to_owned()),
        origin: crate::values::Origin::Library,
    }];
    crate::values::apply(&mut library, &values);
    let implied = crate::discs::apply(&library, &crate::Policy::current()).unwrap();
    assert_eq!(
        implied.items[0].facts.album.as_deref(),
        Some("Tin Weather (Disc 2)")
    );
    assert_eq!(implied.items[1].facts.album.as_deref(), Some("Tin Weather"));
}

#[test]
fn two_discs_in_one_folder_are_two_proposals_and_a_log_that_fits_no_file_joins_neither() {
    let mut library = library();
    for item in &mut library.items {
        item.folder = "1/Moss Arcade/Tin Weather".to_owned();
    }
    let discs = vec![FolderDiscs {
        folder: "1/Moss Arcade/Tin Weather".to_owned(),
        layouts: vec![
            DiscLayout {
                disc_id: "main".to_owned(),
                provisional: false,
                found: String::new(),
                items: vec![1, 2, 3],
                positions: vec![1, 2, 3],
            },
            DiscLayout {
                disc_id: "bonus".to_owned(),
                provisional: false,
                found: String::new(),
                items: vec![4, 5],
                positions: vec![1, 2],
            },
            // A log of some other disc, whose lengths fit none of these.
            DiscLayout {
                disc_id: "stray".to_owned(),
                provisional: false,
                found: String::new(),
                items: vec![],
                positions: vec![],
            },
        ],
    }];
    let mut replies = replies();
    replies.insert(
        "stray".to_owned(),
        vec![ReplyRelease {
            group: "elsewhere".to_owned(),
            ..release("other", 1)
        }],
    );
    let proposals = albums(propose(
        &library,
        &discs,
        &replies,
        &ids(),
        &crate::Policy::current(),
    ));
    assert_eq!(proposals.len(), 2);
    assert_eq!(
        (proposals[0].items.as_slice(), proposals[1].items.as_slice()),
        (&[1, 2, 3][..], &[4, 5][..])
    );
    for proposal in &proposals {
        assert!(
            proposal
                .candidates
                .iter()
                .all(|candidate| candidate.release_group != "elsewhere")
        );
    }
}

#[test]
fn a_disc_two_releases_place_differently_is_not_joined() {
    let mut replies = replies();
    // One release holds the bonus disc as its second medium, another as its
    // third: which medium it is, is open.
    replies.insert(
        "bonus".to_owned(),
        vec![release("deluxe", 2), release("box", 3)],
    );
    let proposals = albums(propose(
        &library(),
        &discs(),
        &replies,
        &ids(),
        &crate::Policy::current(),
    ));
    for proposal in &proposals {
        assert!(
            proposal.candidates[0]
                .values
                .iter()
                .all(|value| value.field != "disc_number")
        );
    }
}

#[test]
fn a_join_takes_the_first_discs_whole_credit_and_clears_ids_it_lacks() {
    let mut library = library();
    for item in library.items.iter_mut().skip(3) {
        item.facts.album_artist_mbids = vec!["00000000-0000-4000-8000-0000000000ff".to_owned()];
    }
    let proposals = albums(propose(
        &library,
        &discs(),
        &replies(),
        &ids(),
        &crate::Policy::current(),
    ));
    let bonus = &proposals[1].candidates[0];
    let cleared: Vec<&Value> = bonus
        .values
        .iter()
        .filter(|value| value.item == 4 && value.field == "mb_album_artist")
        .collect();
    assert_eq!(cleared.len(), 1);
    assert_eq!(cleared[0].value, None);
}

#[test]
fn proposals_of_different_first_albums_are_not_one_group() {
    let mut replies = replies();
    replies.insert(
        "bonus".to_owned(),
        vec![
            ReplyRelease {
                group: "another".to_owned(),
                ..release("x", 1)
            },
            ReplyRelease {
                group: "another".to_owned(),
                ..release("y", 1)
            },
            release("deluxe", 2),
        ],
    );
    let proposals = albums(propose(
        &library(),
        &discs(),
        &replies,
        &ids(),
        &crate::Policy::current(),
    ));
    assert_eq!(proposals[1].candidates[0].release_group, "another");
    assert_ne!(proposals[0].group, proposals[1].group);
}

#[test]
fn a_disc_answered_alone_is_proposed_for_no_more_and_the_other_still_joins_it() {
    let mut library = library();
    // The main disc's own proposal, said yes to first.
    let first = propose(
        &library,
        &discs(),
        &replies(),
        &ids(),
        &crate::Policy::current(),
    );
    crate::values::apply(&mut library, &first[0].candidates[0].values);
    let proposals = albums(propose(
        &library,
        &discs(),
        &replies(),
        &ids(),
        &crate::Policy::current(),
    ));
    assert_eq!(proposals.len(), 1);
    assert_eq!(proposals[0].anchor, 4);
    let candidate = &proposals[0].candidates[0];
    assert_eq!(
        values_of(candidate, 4),
        [
            ("album", "Tin Weather"),
            ("album_artist", "Moss Arcade"),
            ("disc_number", "2"),
            ("mb_release_group", GROUP),
            ("original_date", "2004-05-11"),
            ("primary_type", "album"),
        ]
    );
}

#[test]
fn a_disc_with_one_file_answered_is_proposed_for_the_others() {
    let mut library = library();
    let first = propose(
        &library,
        &discs(),
        &replies(),
        &ids(),
        &crate::Policy::current(),
    );
    let one: Vec<Value> = first[1].candidates[0]
        .values
        .iter()
        .filter(|value| value.item == 4)
        .cloned()
        .collect();
    crate::values::apply(&mut library, &one);
    let proposals = albums(propose(
        &library,
        &discs(),
        &replies(),
        &ids(),
        &crate::Policy::current(),
    ));
    let bonus = proposals
        .iter()
        .find(|proposal| proposal.folder.ends_with("Bonus"))
        .unwrap();
    assert_eq!(bonus.items, [5]);
}

#[test]
fn a_disc_answered_as_another_album_is_not_joined_to_this_one() {
    let mut library = library();
    // The main disc was answered as an album the replies also name, but not
    // the one the bonus disc is proposed as.
    for id in 1..=3 {
        crate::values::apply(
            &mut library,
            &[Value {
                item: id,
                field: "mb_release_group",
                ordinal: 0,
                value: Some("00000000-0000-4000-8000-0000000000b9".to_owned()),
                origin: Origin::Catalogue,
            }],
        );
    }
    let proposals = albums(propose(
        &library,
        &discs(),
        &replies(),
        &ids(),
        &crate::Policy::current(),
    ));
    assert_eq!(proposals.len(), 1);
    let candidate = &proposals[0].candidates[0];
    assert!(
        candidate
            .values
            .iter()
            .all(|value| value.field != "disc_number")
    );
}

#[test]
fn a_layout_found_another_way_changes_the_fingerprint_and_nothing_else() {
    let shown = propose(
        &library(),
        &discs(),
        &replies(),
        &ids(),
        &crate::Policy::current(),
    );
    let mut found = discs();
    found[1].layouts[0].found = r#"[{"method":"cue","sidecarRevisionId":7}]"#.to_owned();
    let now = propose(
        &library(),
        &found,
        &replies(),
        &ids(),
        &crate::Policy::current(),
    );
    assert_eq!(now[1].id, shown[1].id);
    assert_eq!(now[1].candidates, shown[1].candidates);
    assert_ne!(now[1].fingerprint, shown[1].fingerprint);
}

#[test]
fn the_preview_counts_the_songs_two_editions_would_hold_as_one() {
    // A second edition of the main disc, its songs two seconds longer, is
    // another album, and its songs other songs, until both are said to be
    // of one album.
    let mut library = library();
    library.items.truncate(3);
    for (id, title) in [(11, "Paper Kites"), (12, "Low Tide"), (13, "Salt Roads")] {
        let mut copy = file(
            id,
            "Moss Arcade/Tin Weather Deluxe",
            "Tin Weather Expanded",
            id - 10,
            title,
        );
        copy.facts.duration_ms = Some(100_000 + (id - 10) * 1000 + 2000);
        library.items.push(copy);
    }
    let values: Vec<Value> = [1, 2, 3, 11, 12, 13]
        .into_iter()
        .map(|item| Value {
            item,
            field: "mb_release_group",
            ordinal: 0,
            value: Some(GROUP.to_owned()),
            origin: Origin::Catalogue,
        })
        .collect();
    let shown = preview(&library, &values, &[], &crate::Policy::current());
    assert_eq!((shown.songs_joined, shown.songs_parted), (3, 0));
    // And the other way round: the values gone, they are parted again.
    let mut joined = library.clone();
    crate::values::apply(&mut joined, &values);
    let undone: Vec<Value> = [11, 12, 13]
        .into_iter()
        .map(|item| Value {
            item,
            field: "mb_release_group",
            ordinal: 0,
            value: None,
            origin: Origin::Catalogue,
        })
        .collect();
    let parted = preview(&joined, &undone, &[], &crate::Policy::current());
    assert_eq!((parted.songs_joined, parted.songs_parted), (0, 3));
}

#[test]
fn a_disc_whose_files_name_no_album_and_no_artist_is_not_proposed_for() {
    // Files with no album title but an artist take the album's title.
    let mut library = library();
    for item in library.items.iter_mut().filter(|item| item.id >= 4) {
        item.facts.album = None;
    }
    let proposals = albums(propose(
        &library,
        &discs(),
        &replies(),
        &ids(),
        &crate::Policy::current(),
    ));
    assert_eq!(proposals.len(), 2);
    // One that names no artist either would show under none: its disc
    // gets no proposal, and the other disc is proposed for alone.
    for item in library.items.iter_mut().filter(|item| item.id == 5) {
        item.facts.artist.clear();
        item.facts.album_artist.clear();
    }
    let proposals = albums(propose(
        &library,
        &discs(),
        &replies(),
        &ids(),
        &crate::Policy::current(),
    ));
    assert_eq!(proposals.len(), 1);
    assert_eq!(proposals[0].items, [1, 2, 3]);
    assert!(proposals[0].candidates[0].onto.is_empty());
}

#[test]
fn answering_the_second_disc_alone_keeps_the_release_the_disc_rules_joined() {
    // Two discs titled with a disc marker before an edition note, which
    // the disc rules make one release, and a reply about the second only.
    let mut library = library();
    library.items.truncate(5);
    for item in &mut library.items {
        let disc = if item.id <= 3 { 1 } else { 2 };
        item.folder = format!("1/Moss Arcade/White Field Disk {disc}");
        item.facts.album = Some(format!("White Field Disk {disc} [Half-Speed]"));
        item.facts.track = Some(if item.id <= 3 { item.id } else { item.id - 3 });
    }
    let discs = vec![
        FolderDiscs {
            folder: "1/Moss Arcade/White Field Disk 1".to_owned(),
            layouts: vec![DiscLayout {
                disc_id: "unknown".to_owned(),
                provisional: false,
                found: String::new(),
                items: vec![1, 2, 3],
                positions: vec![1, 2, 3],
            }],
        },
        FolderDiscs {
            folder: "1/Moss Arcade/White Field Disk 2".to_owned(),
            layouts: vec![DiscLayout {
                disc_id: "bonus".to_owned(),
                provisional: false,
                found: String::new(),
                items: vec![4, 5],
                positions: vec![1, 2],
            }],
        },
    ];
    let replies = HashMap::from([("bonus".to_owned(), vec![release("half", 2)])]);
    let proposals = albums(propose(
        &library,
        &discs,
        &replies,
        &ids(),
        &crate::Policy::current(),
    ));
    assert_eq!(proposals.len(), 1, "only the second disc is proposed for");
    let values = &proposals[0].candidates[0].values;
    let shown = preview(&library, values, &[], &crate::Policy::current());
    let shape = |albums: &[AlbumShape]| -> Vec<(u32, u32, u32)> {
        albums
            .iter()
            .map(|album| (album.releases, album.discs, album.songs))
            .collect()
    };
    assert_eq!(shape(&shown.now), [(1, 2, 5)]);
    assert_eq!(shape(&shown.after), [(1, 2, 5)], "still one release");
    assert_eq!((shown.songs_joined, shown.songs_parted), (0, 0));
}

/// The edition proposals alone.
fn editions_of(proposals: Vec<Proposal>) -> Vec<Proposal> {
    proposals
        .into_iter()
        .filter(|proposal| proposal.kind == "edition")
        .collect()
}

#[test]
fn the_two_discs_are_proposed_as_the_one_release_that_holds_both() {
    let library = library();
    let all = propose(
        &library,
        &discs(),
        &replies(),
        &ids(),
        &crate::Policy::current(),
    );
    let editions = editions_of(all.clone());
    assert_eq!(editions.len(), 1);
    let edition = &editions[0];
    assert_eq!(edition.items, [1, 2, 3, 4, 5]);
    assert_eq!(edition.group, 1, "in the album's group");
    // Only the deluxe release holds the bonus disc.
    assert_eq!(edition.candidates.len(), 1);
    let candidate = &edition.candidates[0];
    assert_eq!(candidate.release.as_deref(), Some("deluxe"));
    assert_eq!(candidate.title.as_deref(), Some("Tin Weather (deluxe)"));
    assert_eq!(
        values_of(candidate, 5),
        [
            ("disc_number", "2"),
            ("mb_release", "deluxe"),
            ("track_number", "2")
        ]
    );
    // With the album's yeses, one release of two discs.
    let values: Vec<_> = all
        .iter()
        .flat_map(|proposal| proposal.candidates[0].values.clone())
        .collect();
    let shown = preview(&library, &values, &[], &crate::Policy::current());
    assert_eq!(shown.after.len(), 1);
    assert_eq!(
        (
            shown.after[0].releases,
            shown.after[0].discs,
            shown.after[0].songs
        ),
        (1, 2, 5)
    );
}

#[test]
fn an_edition_several_releases_hold_alike_has_each_and_copies_of_one_disc_have_none() {
    let mut replies = replies();
    replies.insert(
        "bonus".to_owned(),
        vec![release("deluxe", 2), release("japan", 2)],
    );
    replies.insert(
        "main".to_owned(),
        vec![
            release("standard", 1),
            release("deluxe", 1),
            release("japan", 1),
        ],
    );
    let editions = editions_of(propose(
        &library(),
        &discs(),
        &replies,
        &ids(),
        &crate::Policy::current(),
    ));
    let releases: Vec<_> = editions[0]
        .candidates
        .iter()
        .map(|candidate| candidate.release.as_deref().unwrap())
        .collect();
    assert_eq!(releases, ["deluxe", "japan"]);

    // A second copy of the main disc: two discs are the same medium of
    // every release, so no edition is proposed.
    let mut library = library();
    for id in [6, 7, 8] {
        let mut copy = file(
            id,
            "Moss Arcade/Tin Weather (copy)",
            "Tin Weather",
            id - 5,
            "Copy",
        );
        copy.first_seen_ms = 100 + id;
        library.items.push(copy);
    }
    let mut discs = discs();
    discs.push(FolderDiscs {
        folder: "1/Moss Arcade/Tin Weather (copy)".to_owned(),
        layouts: vec![DiscLayout {
            disc_id: "main".to_owned(),
            provisional: false,
            found: String::new(),
            items: vec![6, 7, 8],
            positions: vec![1, 2, 3],
        }],
    });
    let mut ids = ids();
    ids.extend((6..=8).map(|id| (id, [id as u8; 16])));
    assert!(
        editions_of(propose(
            &library,
            &discs,
            &replies,
            &ids,
            &crate::Policy::current()
        ))
        .is_empty()
    );
}

#[test]
fn a_file_an_answer_gave_a_release_is_in_no_edition() {
    let mut library = library();
    for item in &mut library.items {
        item.facts.release_mbid = Some("deluxe".to_owned());
        item.held.push("mb_release");
    }
    assert!(
        editions_of(propose(
            &library,
            &discs(),
            &replies(),
            &ids(),
            &crate::Policy::current()
        ))
        .is_empty()
    );
}

#[test]
fn two_albums_proposed_for_the_same_discs_get_two_editions() {
    let mut replies = replies();
    let other = |id: &str, position: u32| ReplyRelease {
        group: "other-album".to_owned(),
        ..release(id, position)
    };
    replies.get_mut("main").unwrap().push(other("reissue", 1));
    replies.get_mut("bonus").unwrap().push(other("reissue", 2));
    let editions = editions_of(propose(
        &library(),
        &discs(),
        &replies,
        &ids(),
        &crate::Policy::current(),
    ));
    assert_eq!(editions.len(), 2);
    assert_eq!(editions[0].anchor, editions[1].anchor);
    assert_ne!(editions[0].id, editions[1].id);
}

#[test]
fn a_disc_whose_layouts_place_a_file_at_two_tracks_is_in_no_edition() {
    // A file one layout makes the whole disc and another a track of it.
    let mut image = discs();
    image[1].layouts.push(DiscLayout {
        disc_id: "bonus-image".to_owned(),
        provisional: false,
        found: String::new(),
        items: vec![4, 5],
        positions: vec![0, 2],
    });
    assert!(
        editions_of(propose(
            &library(),
            &image,
            &replies(),
            &ids(),
            &crate::Policy::current()
        ))
        .is_empty()
    );
    let mut discs = discs();
    discs[1].layouts.push(DiscLayout {
        disc_id: "bonus-other".to_owned(),
        provisional: false,
        found: String::new(),
        items: vec![4, 5],
        positions: vec![2, 1],
    });
    // Whether or not the other layout has been looked up.
    let mut replied = replies();
    replied.insert("bonus-other".to_owned(), vec![release("deluxe", 2)]);
    for replies in [replies(), replied] {
        assert!(
            editions_of(propose(
                &library(),
                &discs,
                &replies,
                &ids(),
                &crate::Policy::current()
            ))
            .is_empty()
        );
    }
}

#[test]
fn an_edition_for_a_disc_added_later_puts_it_with_those_answered_before() {
    // The main disc already said to be the deluxe release: the bonus
    // disc's edition is about its files, and puts them with the main's.
    let mut library = library();
    for item in library.items.iter_mut().filter(|item| item.id <= 3) {
        item.facts.release_mbid = Some("deluxe".to_owned());
        item.held.push("mb_release");
    }
    let editions = editions_of(propose(
        &library,
        &discs(),
        &replies(),
        &ids(),
        &crate::Policy::current(),
    ));
    assert_eq!(editions.len(), 1);
    assert_eq!(editions[0].items, [4, 5]);
    assert_eq!(editions[0].candidates[0].onto, [1, 2, 3]);
}

#[test]
fn a_disc_added_later_is_offered_only_the_release_those_answered_before_are() {
    let mut replies = replies();
    replies.insert(
        "bonus".to_owned(),
        vec![release("deluxe", 2), release("japan", 2)],
    );
    replies.insert(
        "main".to_owned(),
        vec![release("deluxe", 1), release("japan", 1)],
    );
    let mut library = library();
    for item in library.items.iter_mut().filter(|item| item.id <= 3) {
        item.facts.release_mbid = Some("japan".to_owned());
        item.held.push("mb_release");
    }
    let editions = editions_of(propose(
        &library,
        &discs(),
        &replies,
        &ids(),
        &crate::Policy::current(),
    ));
    let releases: Vec<_> = editions[0]
        .candidates
        .iter()
        .map(|candidate| candidate.release.as_deref().unwrap())
        .collect();
    assert_eq!(releases, ["japan"]);
}

#[test]
fn a_second_copy_of_an_edition_answered_before_is_proposed_as_a_release_of_its_own() {
    // The first copy said to be the deluxe release.
    let mut library = library();
    for item in &mut library.items {
        item.facts.release_mbid = Some("deluxe".to_owned());
        item.held.push("mb_release");
    }
    let copy = [
        (
            6,
            "Moss Arcade/Tin Weather (copy)",
            "Tin Weather",
            1,
            "Paper Kites",
        ),
        (
            7,
            "Moss Arcade/Tin Weather (copy)",
            "Tin Weather",
            2,
            "Low Tide",
        ),
        (
            8,
            "Moss Arcade/Tin Weather (copy)",
            "Tin Weather",
            3,
            "Salt Roads",
        ),
        (
            9,
            "Moss Arcade/Tin Weather Bonus (copy)",
            "Tin Weather (Bonus Disc)",
            1,
            "Glass Harbour",
        ),
        (
            10,
            "Moss Arcade/Tin Weather Bonus (copy)",
            "Tin Weather (Bonus Disc)",
            2,
            "Far Lights",
        ),
    ];
    for (id, folder, album, track, title) in copy {
        let mut file = file(id, folder, album, track, title);
        file.first_seen_ms = 100 + id;
        library.items.push(file);
    }
    let mut discs = discs();
    for (folder, disc_id, items) in [
        ("1/Moss Arcade/Tin Weather (copy)", "main", vec![6, 7, 8]),
        (
            "1/Moss Arcade/Tin Weather Bonus (copy)",
            "bonus",
            vec![9, 10],
        ),
    ] {
        let positions = (1..=items.len() as u32).collect();
        discs.push(FolderDiscs {
            folder: folder.to_owned(),
            layouts: vec![DiscLayout {
                disc_id: disc_id.to_owned(),
                provisional: false,
                found: String::new(),
                items,
                positions,
            }],
        });
    }
    let mut ids = ids();
    ids.extend((6..=10).map(|id| (id, [id as u8; 16])));
    let editions = editions_of(propose(
        &library,
        &discs,
        &replies(),
        &ids,
        &crate::Policy::current(),
    ));
    assert_eq!(editions.len(), 1);
    assert_eq!(editions[0].items, [6, 7, 8, 9, 10]);
    let candidate = &editions[0].candidates[0];
    assert_eq!(candidate.release.as_deref(), Some("deluxe"));
    assert!(candidate.onto.is_empty(), "not put with the first copy");
}

/// The main disc said to be `said`, and copies of discs with these disc
/// IDs, each in a folder of its own, as files from 6.
fn with_copies(
    said: &str,
    copies: &[(&str, &str, usize)],
) -> (Library, Vec<FolderDiscs>, HashMap<i64, [u8; 16]>) {
    let mut library = library();
    library.items.retain(|item| item.id <= 3);
    for item in &mut library.items {
        item.facts.release_mbid = Some(said.to_owned());
        item.held.push("mb_release");
    }
    let mut discs = discs();
    discs.truncate(1);
    let mut ids = ids();
    let mut next = 6;
    for (folder, disc_id, files) in copies {
        let items: Vec<i64> = (next..next + *files as i64).collect();
        next += *files as i64;
        for (track, id) in (1..).zip(&items) {
            let mut copy = file(*id, folder, "Tin Weather", track, "Song");
            copy.first_seen_ms = 100 + id;
            library.items.push(copy);
            ids.insert(*id, [*id as u8; 16]);
        }
        let positions = (1..=items.len() as u32).collect();
        discs.push(FolderDiscs {
            folder: format!("1/{folder}"),
            layouts: vec![DiscLayout {
                disc_id: (*disc_id).to_owned(),
                provisional: false,
                found: String::new(),
                items,
                positions,
            }],
        });
    }
    (library, discs, ids)
}

#[test]
fn a_disc_said_to_be_a_release_that_does_not_hold_these_leaves_their_edition_alone() {
    // The main disc is the one-disc release; a copy of the two-disc one,
    // its first disc mastered otherwise, is another release of the album.
    let (library, discs, ids) = with_copies(
        "single",
        &[
            ("Moss Arcade/Two Discs 1", "two-1", 3),
            ("Moss Arcade/Two Discs 2", "two-2", 2),
        ],
    );
    let mut replies = replies();
    replies.insert("main".to_owned(), vec![release("single", 1)]);
    replies.insert("two-1".to_owned(), vec![release("double", 1)]);
    replies.insert("two-2".to_owned(), vec![release("double", 2)]);
    let editions = editions_of(propose(
        &library,
        &discs,
        &replies,
        &ids,
        &crate::Policy::current(),
    ));
    assert_eq!(editions.len(), 1);
    assert_eq!(editions[0].candidates[0].release.as_deref(), Some("double"));
    assert!(editions[0].candidates[0].onto.is_empty());
}

#[test]
fn a_lone_second_copy_of_a_disc_is_put_with_none_of_the_first_copy_s_discs() {
    // The first copy whole, said to be the deluxe release, and later a
    // second copy of its bonus disc alone.
    let mut library = library();
    for item in &mut library.items {
        item.facts.release_mbid = Some("deluxe".to_owned());
        item.held.push("mb_release");
    }
    let mut discs = discs();
    let mut ids = ids();
    for (track, id) in [(1, 6), (2, 7)] {
        let mut copy = file(
            id,
            "Moss Arcade/Bonus Copy",
            "Tin Weather (Bonus Disc)",
            track,
            "Song",
        );
        copy.first_seen_ms = 100 + id;
        library.items.push(copy);
        ids.insert(id, [id as u8; 16]);
    }
    discs.push(FolderDiscs {
        folder: "1/Moss Arcade/Bonus Copy".to_owned(),
        layouts: vec![DiscLayout {
            disc_id: "bonus".to_owned(),
            provisional: false,
            found: String::new(),
            items: vec![6, 7],
            positions: vec![1, 2],
        }],
    });
    assert!(
        editions_of(propose(
            &library,
            &discs,
            &replies(),
            &ids,
            &crate::Policy::current()
        ))
        .is_empty()
    );
}
