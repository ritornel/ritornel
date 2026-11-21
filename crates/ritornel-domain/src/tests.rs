//! The resolver, the catalog and the projector on invented libraries. A
//! fixture is a few lines of what files claim: no audio file is read.
//!
//! [`World`] stands in for the writer: it gives every row the resolution
//! asks for a key and records each new assignment, so a test can resolve
//! the same library again and see that nothing changes.

use std::collections::BTreeMap;

use proptest::prelude::*;

use crate::extract::Facts;
use crate::library::{
    AcrossAlbums, Current, Decision, ExistingCredit, ExistingRelease, Item, Kind, Library, Method,
    Ref, Resolution, WithheldCause,
};
use crate::resolve::{Policy, Replacement, judge_replacements, markers_of};
use crate::{catalog, normalize};

/// One invented file. `path` gives the folder and, with no title, the name.
#[derive(Debug, Clone)]
pub(crate) struct File {
    path: &'static str,
    audio: i64,
    facts: Facts,
}

pub(crate) fn file(path: &'static str, audio: i64) -> File {
    File {
        path,
        audio,
        facts: Facts::default(),
    }
}

impl File {
    fn song(mut self, title: &str, artist: &str, seconds: i64) -> Self {
        self.facts.title = Some(title.to_owned());
        self.facts.artist = vec![artist.to_owned()];
        self.facts.duration_ms = Some(seconds * 1000);
        self
    }

    fn on(mut self, album: &str, disc: Option<i64>, track: i64) -> Self {
        self.facts.album = Some(album.to_owned());
        self.facts.disc = disc;
        self.facts.track = Some(track);
        self
    }

    fn by(mut self, album_artist: &str) -> Self {
        self.facts.album_artist = vec![album_artist.to_owned()];
        self
    }

    fn with(mut self, change: impl FnOnce(&mut Facts)) -> Self {
        change(&mut self.facts);
        self
    }
}

#[derive(Debug, Default)]
pub(crate) struct World {
    pub(crate) library: Library,
    next: i64,
    /// What each row shows, as the projector last wrote it.
    shown: BTreeMap<(Kind, i64), String>,
    /// The policy it settles by: the one scans use unless a test says
    /// otherwise.
    policy: Policy,
}

impl World {
    /// A world that settles by the given policy.
    pub(crate) fn under(policy: Policy) -> Self {
        Self {
            policy,
            ..Self::default()
        }
    }

    pub(crate) fn add(&mut self, file: File) -> i64 {
        self.next += 1;
        let (folder, _) = file.path.rsplit_once('/').unwrap_or(("", file.path));
        self.library.items.push(Item {
            id: self.next,
            folder: format!("main/{folder}"),
            audio: Some(file.audio),
            lossless: true,
            retired: false,
            first_seen_ms: self.next,
            cover: None,
            facts: file.facts,
            current: None,
        });
        self.next
    }

    fn row(&mut self, r: Ref) -> i64 {
        match r {
            Ref::Row(row) => row,
            Ref::New(_) => {
                self.next += 1;
                self.next
            }
        }
    }

    /// Resolves, and writes what the resolution asks for.
    pub(crate) fn settle(&mut self) -> Resolution {
        let resolution = catalog::resolve(&self.library, &self.policy);
        let recordings: Vec<i64> = resolution
            .recordings
            .iter()
            .map(|out| self.row(out.r))
            .collect();
        let artists: Vec<i64> = resolution
            .artists
            .iter()
            .map(|out| self.row(out.r))
            .collect();
        for (out, row) in resolution.artists.iter().zip(&artists) {
            if matches!(out.r, Ref::New(_)) {
                self.library.artists.push((*row, out.match_name.clone()));
            }
            // What the store reads back from the claims of the files whose
            // credits name the row.
            if let Some(mbid) = &out.mbid {
                self.library.artist_mbids.insert(*row, mbid.clone());
            }
            if let Some(entry) = self
                .library
                .artists
                .iter_mut()
                .find(|(existing, _)| existing == row)
            {
                entry.1 = out.match_name.clone();
            }
            self.shown.insert((Kind::Artist, *row), out.name.clone());
        }
        let credits: Vec<i64> = resolution
            .credits
            .iter()
            .map(|out| self.row(out.r))
            .collect();
        for (out, row) in resolution.credits.iter().zip(&credits) {
            if matches!(out.r, Ref::New(_)) {
                self.library.credits.push(ExistingCredit {
                    id: *row,
                    text: out.text.clone(),
                    names: out
                        .names
                        .iter()
                        .map(|(artist, name, join)| (artists[*artist], name.clone(), join.clone()))
                        .collect(),
                });
            }
        }
        let groups: Vec<i64> = resolution
            .groups
            .iter()
            .map(|out| self.row(out.r))
            .collect();
        for (out, row) in resolution.groups.iter().zip(&groups) {
            self.library.groups.entry(*row).or_insert(None);
            self.shown.insert(
                (Kind::ReleaseGroup, *row),
                format!(
                    "{:?} {:?} {:?} shows {:?}",
                    out.primary_type, out.secondary_types, out.first_date, out.display_release
                ),
            );
        }
        let releases: Vec<i64> = resolution
            .releases
            .iter()
            .map(|out| self.row(out.r))
            .collect();
        for (out, row) in resolution.releases.iter().zip(&releases) {
            let chosen_group = self
                .library
                .releases
                .get(row)
                .and_then(|release| release.chosen_group);
            self.library.releases.insert(
                *row,
                ExistingRelease {
                    canonical: *row,
                    chosen_group,
                    group: groups[out.group],
                },
            );
            self.shown.insert(
                (Kind::Release, *row),
                format!(
                    "{:?} by {:?} {:?}",
                    out.title,
                    out.credit.map(|at| credits[at]),
                    out.date
                ),
            );
        }
        let recording = |r: Ref| match r {
            Ref::Row(row) => row,
            Ref::New(at) => recordings[at],
        };
        let appearances: Vec<i64> = resolution
            .appearances
            .iter()
            .map(|out| self.row(out.r))
            .collect();
        for (out, row) in resolution.appearances.iter().zip(&appearances) {
            self.library.appearances.insert(
                *row,
                (recording(out.recording), out.release.map(|at| releases[at])),
            );
            self.shown.insert(
                (Kind::TrackAppearance, *row),
                format!("{:?} {:?}.{:?}", out.title, out.disc, out.track),
            );
        }
        for (item, out) in self.library.items.iter_mut().zip(&resolution.items) {
            if !out.changed {
                continue;
            }
            item.current = Some(Current {
                recording: recording(out.recording.unwrap()),
                appearance: out.appearance.map(|r| match r {
                    Ref::Row(row) => row,
                    Ref::New(at) => appearances[at],
                }),
                method: out.decision.as_ref().unwrap().method,
                by_user: false,
                needs_review: out.decision.as_ref().unwrap().needs_review,
                across_albums: out
                    .decision
                    .as_ref()
                    .unwrap()
                    .evidence
                    .across_albums
                    .is_some(),
            });
        }
        resolution
    }

    /// Fails unless a second resolution asks for nothing: no new
    /// assignment, no new row, and nothing shown differently.
    pub(crate) fn assert_settled(&mut self) {
        let shown = self.shown.clone();
        let again = self.settle();
        assert!(
            again.items.iter().all(|item| !item.changed),
            "{:#?}",
            again.items
        );
        let new = |r: &Ref| matches!(r, Ref::New(_));
        assert!(again.recordings.iter().all(|out| !new(&out.r)));
        assert!(again.releases.iter().all(|out| !new(&out.r)));
        assert!(again.groups.iter().all(|out| !new(&out.r)));
        assert!(again.appearances.iter().all(|out| !new(&out.r)));
        assert!(again.artists.iter().all(|out| !new(&out.r)));
        assert!(again.credits.iter().all(|out| !new(&out.r)));
        assert_eq!(self.shown, shown);
    }

    fn item(&self, id: i64) -> &Item {
        self.library
            .items
            .iter()
            .find(|item| item.id == id)
            .unwrap()
    }

    fn recording(&self, id: i64) -> i64 {
        self.item(id).current.unwrap().recording
    }

    fn release(&self, id: i64) -> Option<i64> {
        self.library.appearances[&self.item(id).current.unwrap().appearance.unwrap()].1
    }

    fn group(&self, id: i64) -> i64 {
        self.library.releases[&self.release(id).unwrap()].group
    }
}

fn decision(resolution: &Resolution, id: i64) -> &crate::library::Decision {
    resolution
        .items
        .iter()
        .find(|out| out.item == id)
        .unwrap()
        .decision
        .as_ref()
        .unwrap()
}

/// An album of three songs in one folder, each file with audio of its own.
fn album(
    world: &mut World,
    folder: &'static str,
    artist: &str,
    title: &str,
    audio: i64,
) -> Vec<i64> {
    let names = ["Paper Kites", "Low Tide", "Harbour Lights"];
    names
        .iter()
        .enumerate()
        .map(|(index, name)| {
            let n = index as i64;
            let path: &'static str =
                Box::leak(format!("{folder}/{:02}.flac", n + 1).into_boxed_str());
            world.add(
                file(path, audio + n)
                    .song(name, artist, 200 + 10 * n)
                    .on(title, Some(1), n + 1)
                    .by(artist),
            )
        })
        .collect()
}

#[test]
fn an_album_tagged_with_musicbrainz_ids_is_one_release_of_its_recordings() {
    let mut world = World::default();
    let ids = album(
        &mut world,
        "Moss Arcade/Tin Weather",
        "Moss Arcade",
        "Tin Weather",
        10,
    );
    for item in &mut world.library.items {
        item.facts.recording_mbid = Some(format!("rec-{}", item.id));
        item.facts.release_mbid = Some("rel-1".to_owned());
        item.facts.release_group_mbid = Some("grp-1".to_owned());
        item.facts.artist_mbids = vec!["art-1".to_owned()];
        item.facts.album_artist_mbids = vec!["art-1".to_owned()];
    }
    let resolution = world.settle();
    assert_eq!(resolution.recordings.len(), 3);
    assert_eq!(resolution.releases.len(), 1);
    assert_eq!(resolution.groups.len(), 1);
    assert_eq!(resolution.artists.len(), 1);
    assert_eq!(resolution.artists[0].name, "Moss Arcade");
    assert_eq!(resolution.credits.len(), 1);
    assert_eq!(resolution.releases[0].title.as_deref(), Some("Tin Weather"));
    for id in &ids {
        assert_eq!(decision(&resolution, *id).method, Method::New);
        assert!(!decision(&resolution, *id).needs_review);
    }
    let titles: Vec<_> = resolution
        .appearances
        .iter()
        .map(|out| (out.track, out.title.clone()))
        .collect();
    assert_eq!(titles[1], (Some(2), Some("Low Tide".to_owned())));
    world.assert_settled();

    // A second copy with the same recording IDs binds by identifier.
    let again = world.add(
        file("Elsewhere/01.flac", 99)
            .song("Paper Kites", "Moss Arcade", 203)
            .with(|facts| facts.recording_mbid = Some(format!("rec-{}", ids[0]))),
    );
    let resolution = world.settle();
    assert_eq!(decision(&resolution, again).method, Method::Identifier);
    assert_eq!(world.recording(again), world.recording(ids[0]));
    world.assert_settled();
}

#[test]
fn an_untagged_remaster_binds_by_tags_and_duration_and_joins_its_originals_group() {
    let mut world = World::default();
    let original = album(
        &mut world,
        "Moss Arcade/Tin Weather",
        "Moss Arcade",
        "Tin Weather",
        10,
    );
    let remaster = album(
        &mut world,
        "Moss Arcade/Tin Weather (2019)",
        "Moss Arcade",
        "Tin Weather",
        20,
    );
    let resolution = world.settle();
    for (first, second) in original.iter().zip(&remaster) {
        let decided = decision(&resolution, *second);
        assert_eq!(decided.method, Method::TagsDuration);
        assert_eq!((decided.score, decided.runner_up_gap), (0.0, None));
        assert_eq!(world.recording(*first), world.recording(*second));
    }
    // Its disc 1 track 1 collides with the original's, so it is a second
    // release, and sharing every recording it joins the original's group.
    assert_eq!(resolution.recordings.len(), 3);
    assert_eq!(resolution.releases.len(), 2);
    assert_ne!(world.release(original[0]), world.release(remaster[0]));
    assert_eq!(resolution.groups.len(), 1);
    assert_eq!(world.group(original[0]), world.group(remaster[0]));
    assert_eq!(resolution.appearances.len(), 6);
    world.assert_settled();
}

#[test]
fn two_self_titled_albums_stay_apart() {
    let mut world = World::under(Policy::v1());
    let add = |world: &mut World, folder: &'static str, songs: [&str; 2], audio| -> Vec<i64> {
        songs
            .iter()
            .enumerate()
            .map(|(n, song)| {
                let path: &'static str = Box::leak(format!("{folder}/{n}.flac").into_boxed_str());
                world.add(
                    file(path, audio + n as i64)
                        .song(song, "Ninefold", 180)
                        .on("Ninefold", None, n as i64 + 1)
                        .by("Ninefold"),
                )
            })
            .collect()
    };
    let blue = add(
        &mut world,
        "Ninefold/Ninefold (1994)",
        ["Glass Pier", "Salt Road"],
        10,
    );
    let green = add(
        &mut world,
        "Ninefold/Ninefold (2001)",
        ["Coal Light", "Third Rail"],
        20,
    );
    let resolution = world.settle();
    assert_eq!(resolution.recordings.len(), 4);
    assert_eq!(resolution.releases.len(), 2);
    assert_eq!(resolution.groups.len(), 2);
    assert_ne!(world.group(blue[0]), world.group(green[0]));
    assert_eq!(world.release(blue[0]), world.release(blue[1]));
    world.assert_settled();

    // In sibling folders with no disc tag, colliding on track numbers and
    // sharing no title, they look to policy 2's folder rule like two discs
    // of one album; their folders name no disc, so it leaves them apart.
    assert_eq!(discs(&world, &Policy::v2()), (2, vec![None; 4]));

    // Nor are they joined when their folders do look like discs, if their
    // dates say they are two records.
    let mut world = World::default();
    let ids = two_folders(
        &mut world,
        ["Ninefold/Ninefold/CD1", "Ninefold/Ninefold/CD2"],
        ["Ninefold"; 2],
        false,
    );
    for (at, item) in world.library.items.iter_mut().enumerate() {
        item.facts.date = Some(if at < 2 { "1994" } else { "2001-05-01" }.to_owned());
    }
    world.settle();
    assert_eq!(discs(&world, &Policy::v2()), (2, vec![None; 4]));
    // The same folders dated alike are discs of one release.
    for item in &mut world.library.items {
        item.facts.date = Some("1994".to_owned());
    }
    assert_eq!(
        discs(&world, &Policy::v2()),
        (1, vec![Some(1), Some(1), Some(2), Some(2)])
    );
    assert_eq!(ids.len(), 4);
}

#[test]
fn under_policy_1_a_song_on_an_album_and_on_a_compilation_is_one_recording() {
    let mut world = World::under(Policy::v1());
    let on_album = world.add(
        file("Ada Wren/Field Notes/03.flac", 1)
            .song("Coal Light", "Ada Wren", 215)
            .on("Field Notes", None, 3)
            .by("Ada Wren"),
    );
    let on_compilation = world.add(
        file("Compilations/Night Drives/07.flac", 2)
            .song("Coal Light", "Ada Wren", 216)
            .on("Night Drives", None, 7)
            .by("Various Artists")
            .with(|facts| facts.compilation = true),
    );
    let resolution = world.settle();
    let decided = decision(&resolution, on_compilation);
    assert_eq!(decided.method, Method::TagsDuration);
    assert!(
        decided.score > 0.0 && decided.score <= 0.04,
        "{}",
        decided.score
    );
    assert_eq!(decided.penalties.unwrap().album, 1.0);
    assert_eq!(world.recording(on_album), world.recording(on_compilation));
    assert_eq!(resolution.appearances.len(), 2);
    assert_eq!(resolution.releases.len(), 2);
    assert_eq!(resolution.groups.len(), 2);
    world.assert_settled();
}

#[test]
fn an_album_of_many_artists_with_no_album_artist_is_one_release_by_various_artists() {
    let mut world = World::default();
    let ids: Vec<i64> = [
        ("Glass Pier", "Ada Wren"),
        ("Salt Road", "Ninefold"),
        ("Third Rail", "Moss Arcade"),
    ]
    .iter()
    .enumerate()
    .map(|(n, (song, artist))| {
        let path: &'static str = Box::leak(format!("Night Drives/{n}.flac").into_boxed_str());
        world.add(file(path, n as i64).song(song, artist, 200).on(
            "Night Drives",
            None,
            n as i64 + 1,
        ))
    })
    .collect();
    let resolution = world.settle();
    assert_eq!(resolution.releases.len(), 1);
    assert_eq!(world.release(ids[0]), world.release(ids[2]));
    let credit = &resolution.credits[resolution.releases[0].credit.unwrap()];
    assert_eq!(credit.text, "Various Artists");
    assert_eq!(resolution.artists.len(), 4);
    world.assert_settled();

    // One artist throughout, and still no album artist tag: the album is
    // that artist's.
    let mut world = World::default();
    for n in 0..2 {
        let path: &'static str = Box::leak(format!("Field Notes/{n}.flac").into_boxed_str());
        world.add(
            file(path, n)
                .song(&format!("Song {n}"), "Ada Wren", 200)
                .on("Field Notes", None, n + 1),
        );
    }
    let resolution = world.settle();
    let credit = &resolution.credits[resolution.releases[0].credit.unwrap()];
    assert_eq!(credit.text, "Ada Wren");
    assert_eq!(resolution.artists.len(), 1);
}

#[test]
fn a_multi_disc_album_in_disc_folders_is_one_release_when_the_discs_are_numbered() {
    let discs = |numbered: bool, policy: Policy| {
        let mut world = World::under(policy);
        let mut ids = Vec::new();
        for disc in 1..=2 {
            for track in 1..=2 {
                let path: &'static str = Box::leak(
                    format!("Ninefold/Long Field/CD{disc}/{track}.flac").into_boxed_str(),
                );
                ids.push(
                    world.add(
                        file(path, disc * 10 + track)
                            .song(&format!("Part {disc}-{track}"), "Ninefold", 240)
                            .on("Long Field", numbered.then_some(disc), track)
                            .by("Ninefold"),
                    ),
                );
            }
        }
        let resolution = world.settle();
        world.assert_settled();
        (
            resolution.releases.len(),
            resolution.groups.len(),
            resolution.appearances.len(),
        )
    };
    assert_eq!(discs(true, Policy::v1()), (1, 1, 4));
    assert_eq!(discs(true, Policy::v2()), (1, 1, 4));
    // With no disc numbers the two folders claim the same tracks, so under
    // policy 1 they are two releases. They share no recording, so two
    // groups as well. Policy 2 takes the folders for the discs they are.
    assert_eq!(discs(false, Policy::v1()), (2, 2, 4));
    assert_eq!(discs(false, Policy::v2()), (1, 1, 4));
}

#[test]
fn a_copy_elsewhere_parts_from_an_album_whose_disc_folders_stay_together() {
    let mut world = World::default();
    let mut add = |path: &'static str, audio, disc, track| {
        world.add(
            file(path, audio)
                .song(&format!("Part {disc}-{track}"), "Ninefold", 240)
                .on("Long Field", Some(disc), track)
                .by("Ninefold"),
        )
    };
    let one = add("Ninefold/Long Field/CD1/1.flac", 1, 1, 1);
    let two = add("Ninefold/Long Field/CD2/1.flac", 2, 2, 1);
    let copy = add("Backup/Long Field/1.flac", 3, 1, 1);
    world.settle();
    assert_eq!(world.release(one), world.release(two));
    assert_ne!(world.release(one), world.release(copy));
    assert_eq!(world.group(one), world.group(copy));
    world.assert_settled();
}

#[test]
fn a_file_with_no_album_has_an_appearance_and_no_release() {
    let mut world = World::default();
    let loose = world.add(file("Loose/harbour.flac", 1).song("Harbour Lights", "Ada Wren", 200));
    let untagged = world.add(file("Loose/07 - Glass Pier.flac", 2).with(|facts| {
        facts.title_inferred = Some("Glass Pier".to_owned());
        facts.track = Some(7);
        facts.duration_ms = Some(190_000);
    }));
    let resolution = world.settle();
    assert_eq!(resolution.releases.len(), 0);
    assert_eq!(world.release(loose), None);
    assert_eq!(world.release(untagged), None);
    assert_eq!(resolution.appearances.len(), 2);
    assert_eq!(
        resolution.appearances[1].title.as_deref(),
        Some("Glass Pier")
    );
    assert_eq!(resolution.recordings[1].credit, None);
    world.assert_settled();
}

#[test]
fn two_files_claiming_one_disc_and_track_in_one_folder_are_two_appearances_on_one_release() {
    let mut world = World::default();
    let a = world.add(
        file("X/a.flac", 1)
            .song("Glass Pier", "Ninefold", 200)
            .on("Long Field", Some(1), 1)
            .by("Ninefold"),
    );
    let b = world.add(
        file("X/b.flac", 2)
            .song("Salt Road", "Ninefold", 210)
            .on("Long Field", Some(1), 1)
            .by("Ninefold"),
    );
    let resolution = world.settle();
    assert_eq!(resolution.releases.len(), 1);
    assert_eq!(world.release(a), world.release(b));
    assert_eq!(resolution.appearances.len(), 2);
    assert_ne!(world.recording(a), world.recording(b));
    world.assert_settled();
}

#[test]
fn the_same_audio_at_two_paths_is_one_recording_and_shares_its_appearance() {
    let mut world = World::default();
    let song = |path| {
        file(path, 7)
            .song("Glass Pier", "Ninefold", 200)
            .on("Long Field", None, 1)
            .by("Ninefold")
    };
    let first = world.add(song("X/1.flac"));
    let copy = world.add(song("X/1 copy.flac"));
    let resolution = world.settle();
    assert_eq!(decision(&resolution, copy).method, Method::SameAudio);
    assert_eq!(decision(&resolution, copy).score, 0.0);
    assert_eq!(world.recording(first), world.recording(copy));
    assert_eq!(resolution.appearances.len(), 1);
    assert_eq!(
        world.item(first).current.unwrap().appearance,
        world.item(copy).current.unwrap().appearance
    );
    world.assert_settled();
}

#[test]
fn a_doubtful_match_becomes_a_new_recording_marked_for_review() {
    let mut world = World::default();
    let first = world.add(file("A/1.flac", 1).song("Glass Pier", "Ninefold", 200).on(
        "Long Field",
        None,
        1,
    ));
    // The same title and artist, six seconds longer.
    let longer = world.add(file("B/1.flac", 2).song("Glass Pier", "Ninefold", 206).on(
        "Long Field",
        None,
        1,
    ));
    // Twenty-five seconds longer is another recording, and nothing to
    // look at.
    let other = world.add(
        file("C/1.flac", 3)
            .song("Glass Pier", "Ninefold", 231)
            .on("Other", None, 1),
    );
    let resolution = world.settle();
    let decided = decision(&resolution, longer);
    assert_eq!(decided.method, Method::New);
    assert!(decided.needs_review);
    assert_eq!(decided.evidence.candidates.len(), 1);
    assert_eq!(decided.evidence.candidates[0].item, first);
    assert!((decided.evidence.candidates[0].distance - 0.2 * 3.0 / 9.3).abs() < 1e-9);
    assert_ne!(world.recording(first), world.recording(longer));
    assert!(!decision(&resolution, other).needs_review);
    assert_eq!(resolution.recordings.len(), 3);
    world.assert_settled();
}

#[test]
fn a_recording_id_carried_by_audio_far_longer_is_a_contradiction_and_binds_nothing() {
    let mut world = World::default();
    let tagged = |path, audio, seconds| {
        file(path, audio)
            .song("Glass Pier", "Ninefold", seconds)
            .with(|facts| facts.recording_mbid = Some("rec-1".to_owned()))
    };
    let first = world.add(tagged("A/1.flac", 1, 200));
    let far = world.add(tagged("B/1.flac", 2, 260));
    let resolution = world.settle();
    let decided = decision(&resolution, far);
    assert_eq!(decided.method, Method::New);
    assert!(decided.needs_review);
    assert_eq!(decided.evidence.contradictions.len(), 1);
    assert!(decided.evidence.contradictions[0].contains("60000 ms"));
    assert_ne!(world.recording(first), world.recording(far));
    world.assert_settled();
}

#[test]
fn a_version_marker_can_only_stop_a_match() {
    let mut world = World::default();
    let studio = world.add(file("A/1.flac", 1).song("Glass Pier", "Ninefold", 200).on(
        "Long Field",
        None,
        1,
    ));
    let live = world.add(
        file("A/9.flac", 2)
            .song("Glass Pier (Live)", "Ninefold", 200)
            .on("Long Field", None, 9),
    );
    let on_live_album = world.add(file("L/1.flac", 3).song("Glass Pier", "Ninefold", 200).on(
        "Live at the Harbour",
        None,
        1,
    ));
    let remastered = world.add(
        file("R/1.flac", 4)
            .song("Glass Pier (Remastered 2019)", "Ninefold", 200)
            .on("Long Field", None, 1),
    );
    let resolution = world.settle();
    for other in [live, on_live_album] {
        assert_eq!(decision(&resolution, other).method, Method::New);
        assert!(!decision(&resolution, other).needs_review);
        assert_ne!(world.recording(studio), world.recording(other));
    }
    // A remaster shares the recording.
    assert_eq!(
        decision(&resolution, remastered).method,
        Method::TagsDuration
    );
    assert_eq!(world.recording(studio), world.recording(remastered));
    world.assert_settled();
}

#[test]
fn a_version_marker_stuck_to_a_word_still_stops_a_match() {
    let mut world = World::default();
    let studio = world.add(file("A/1.flac", 1).song("Lantern", "Ninefold", 200).on(
        "Long Field",
        None,
        1,
    ));
    let live = world.add(
        file("B/1.flac", 2)
            .song("Lantern(Live)", "Ninefold", 200)
            .on("Long Field", None, 1),
    );
    let mixed = world.add(
        file("C/1.flac", 3)
            .song("Lantern-Remix", "Ninefold", 200)
            .on("Long Field", None, 1),
    );
    let live_album = world.add(file("D/1.flac", 4).song("Lantern", "Ninefold", 200).on(
        "Long Field(Live)",
        None,
        1,
    ));
    world.settle();
    for other in [live, mixed, live_album] {
        assert_ne!(world.recording(studio), world.recording(other), "{other}");
    }
    world.assert_settled();
}

#[test]
fn a_version_marker_on_one_side_stops_a_recording_id_from_binding() {
    let mut world = World::default();
    let tagged = |path, audio, title: &str| {
        file(path, audio)
            .song(title, "Ninefold", 200)
            .with(|facts| facts.recording_mbid = Some("rec-1".to_owned()))
    };
    let studio = world.add(tagged("A/1.flac", 1, "Lantern"));
    let live = world.add(tagged("B/1.flac", 2, "Lantern (Live)"));
    let resolution = world.settle();
    let decided = decision(&resolution, live);
    assert_eq!(decided.method, Method::New);
    assert!(decided.needs_review);
    assert_eq!(decided.evidence.contradictions.len(), 1);
    assert!(
        decided.evidence.contradictions[0].contains("version"),
        "{:?}",
        decided.evidence.contradictions
    );
    assert_ne!(world.recording(studio), world.recording(live));
    world.assert_settled();
}

#[test]
fn kind_and_first_year_come_from_tags_and_fall_back_to_unknown_and_the_earliest_date() {
    let mut world = World::default();
    world.add(
        file("E/1.flac", 1)
            .song("Glass Pier", "Ninefold", 200)
            .on("Short Field", None, 1)
            .with(|facts| {
                facts.primary_type = Some("ep".to_owned());
                facts.secondary_types = vec!["live".to_owned()];
                facts.date = Some("2011-05-02".to_owned());
            }),
    );
    world.add(
        file("R/1.flac", 2)
            .song("Salt Road", "Ninefold", 200)
            .on("Long Field", None, 1)
            .with(|facts| {
                facts.date = Some("2019".to_owned());
                facts.original_date = Some("1994-03-01".to_owned());
            }),
    );
    let neither_old = world.add(
        file("N1/1.flac", 3)
            .song("Third Rail", "Ninefold", 200)
            .on("Plain", None, 1)
            .with(|facts| {
                facts.date = Some("2003".to_owned());
            }),
    );
    let neither_new = world.add(
        file("N2/1.flac", 4)
            .song("Third Rail", "Ninefold", 200)
            .on("Plain", None, 1)
            .with(|facts| {
                facts.date = Some("2015-01-01".to_owned());
            }),
    );
    let resolution = world.settle();
    let by_title = |title: &str| {
        let release = resolution
            .releases
            .iter()
            .find(|release| release.title.as_deref() == Some(title))
            .unwrap();
        &resolution.groups[release.group]
    };
    let ep_group = by_title("Short Field");
    assert_eq!(ep_group.primary_type.as_deref(), Some("ep"));
    assert_eq!(ep_group.secondary_types, ["live"]);
    assert_eq!(
        (ep_group.first_date.as_deref(), ep_group.first_year),
        (Some("2011-05-02"), Some(2011))
    );
    let reissue_group = by_title("Long Field");
    assert_eq!(reissue_group.primary_type, None);
    assert_eq!(
        (
            reissue_group.first_date.as_deref(),
            reissue_group.first_year
        ),
        (Some("1994-03-01"), Some(1994))
    );
    // Neither tag: no kind, and the earliest date among its two releases.
    let plain = by_title("Plain");
    assert_eq!(world.group(neither_old), world.group(neither_new));
    assert_eq!(plain.primary_type, None);
    assert!(plain.secondary_types.is_empty());
    assert_eq!(
        (plain.first_date.as_deref(), plain.first_year),
        (Some("2003"), Some(2003))
    );
    world.assert_settled();
}

#[test]
fn artists_bind_by_id_then_by_exact_normalised_name_and_strings_are_not_parsed() {
    let mut world = World::default();
    world.add(
        file("A/1.flac", 1)
            .song("One", "Ada Wren", 200)
            .with(|facts| {
                facts.artist_mbids = vec!["art-ada".to_owned()];
            }),
    );
    world.add(file("A/2.flac", 2).song("Two", "ADA  WREN", 200));
    world.add(file("A/3.flac", 3).song("Three", "Ada Wren & Ninefold", 200));
    world.add(
        file("A/4.flac", 4)
            .song("Four", "Ada Wren feat. Ninefold", 200)
            .with(|facts| {
                facts.artists = vec!["Ada Wren".to_owned(), "Ninefold".to_owned()];
                facts.artist_mbids = vec!["art-ada".to_owned(), "art-nine".to_owned()];
            }),
    );
    world.add(file("A/5.flac", 5).song("Five", "x", 200).with(|facts| {
        facts.artist = vec!["Low Tide Choir".to_owned(), "Ninefold".to_owned()];
    }));
    let resolution = world.settle();
    let names: Vec<&str> = resolution
        .artists
        .iter()
        .map(|artist| artist.name.as_str())
        .collect();
    assert_eq!(
        names,
        [
            "Ada Wren",
            "Ada Wren & Ninefold",
            "Ninefold",
            "Low Tide Choir"
        ]
    );
    let credit = |at: usize| &resolution.credits[resolution.appearances[at].credit.unwrap()];
    // The untagged spelling is the same artist, credited as written.
    assert_eq!(
        credit(1).names,
        [(0, "ADA  WREN".to_owned(), String::new())]
    );
    assert_eq!(credit(2).names.len(), 1);
    assert_eq!(credit(3).text, "Ada Wren feat. Ninefold");
    assert_eq!(
        credit(3).names,
        [
            (0, "Ada Wren".to_owned(), " feat. ".to_owned()),
            (2, "Ninefold".to_owned(), String::new())
        ]
    );
    assert_eq!(credit(4).text, "Low Tide Choir, Ninefold");
    assert_eq!(credit(4).names.len(), 2);
    world.assert_settled();
}

#[test]
fn after_a_change_of_normaliser_an_artist_takes_its_own_row_before_another_s() {
    let mut world = World::default();
    world.add(file("A/1.flac", 1).song("Lanterns", "Tin-Harbour", 200));
    world.add(file("B/1.flac", 2).song("Salt Road", "Tin & Harbour", 200));
    world.settle();
    let rows: Vec<i64> = world.library.artists.iter().map(|(row, _)| *row).collect();
    // As normaliser 1 keyed them: "tinharbour" and "tin harbour". Today's
    // key of the first is the second's old one.
    world.library.artists = vec![
        (rows[0], "tinharbour".to_owned()),
        (rows[1], "tin harbour".to_owned()),
    ];
    world.library.artist_keys_by = Some(1);
    let resolution = world.settle();
    let found: Vec<(Ref, &str)> = resolution
        .artists
        .iter()
        .map(|out| (out.r, out.name.as_str()))
        .collect();
    assert_eq!(
        found,
        [
            (Ref::Row(rows[0]), "Tin-Harbour"),
            (Ref::Row(rows[1]), "Tin & Harbour")
        ]
    );
}

#[test]
fn after_a_change_of_normaliser_no_artist_takes_a_row_by_today_s_key() {
    let mut world = World::default();
    world.add(file("A/1.flac", 1).song("One", "Tin'Harbour", 200));
    world.add(file("A/2.flac", 2).song("Two", "Tin-Harbour", 200));
    world.add(file("A/3.flac", 3).song("Three", "Tin & Harbour", 200));
    // As normaliser 1 left them: the first two one artist, "tinharbour",
    // and the third another, "tin harbour".
    world.library.artists = vec![(1, "tinharbour".to_owned()), (2, "tin harbour".to_owned())];
    world.next = 10;
    world.library.artist_keys_by = Some(1);
    let resolution = world.settle();
    let found: Vec<(Ref, &str)> = resolution
        .artists
        .iter()
        .map(|out| (out.r, out.name.as_str()))
        .collect();
    // Today the first two are two artists; the second takes a row of its
    // own, not the third's, whose today's key is the third's old one.
    assert_eq!(
        found,
        [
            (Ref::Row(1), "Tin'Harbour"),
            (Ref::New(1), "Tin-Harbour"),
            (Ref::Row(2), "Tin & Harbour")
        ]
    );
}

#[test]
fn after_a_change_of_normaliser_an_artist_first_takes_the_row_of_its_unchanged_spelling() {
    let mut world = World::default();
    world.add(file("A/1.flac", 1).song("One", "Here/There", 200));
    world.add(file("A/2.flac", 2).song("Two", "Here / There", 200));
    world.add(file("A/3.flac", 3).song("Three", "Here'There", 200));
    // Normaliser 1 made the first and third one artist, "herethere", and
    // the second another, "here there". Today the first two are one.
    world.library.artists = vec![(1, "herethere".to_owned()), (2, "here there".to_owned())];
    world.next = 10;
    world.library.artist_keys_by = Some(1);
    let resolution = world.settle();
    let found: Vec<(Ref, &str)> = resolution
        .artists
        .iter()
        .map(|out| (out.r, out.match_name.as_str()))
        .collect();
    assert_eq!(
        found,
        [(Ref::Row(2), "here there"), (Ref::Row(1), "herethere")]
    );
}

#[test]
fn an_artist_with_no_id_keeps_its_row_across_a_change_of_normaliser() {
    for edited in [false, true] {
        let mut world = World::default();
        world.add(file("A/1.flac", 1).song("Lanterns", "Tin-Harbour", 200));
        let first = world.settle();
        let row = match first.artists[0].r {
            Ref::Row(row) => row,
            Ref::New(_) => world.library.artists[0].0,
        };
        // As normaliser 1 left it: its key, which joined the letters
        // round the dash, and maybe the owner's name for it.
        world.library.artists = vec![(row, "tinharbour".to_owned())];
        world.library.artist_keys_by = Some(1);
        if edited {
            world.library.edits.insert(
                (Kind::Artist, row, "name".to_owned()),
                "Harbour of Tin".to_owned(),
            );
        }
        let resolution = world.settle();
        assert_eq!(resolution.artists[0].r, Ref::Row(row), "edited: {edited}");
        assert_eq!(resolution.artists[0].match_name, "tin harbour");
        // Without the earlier version named, the row is not found.
        world.library.artists = vec![(row, "tinharbour".to_owned())];
        world.library.artist_keys_by = None;
        assert!(matches!(
            catalog::resolve(&world.library, &world.policy).artists[0].r,
            Ref::New(_)
        ));
    }
}

#[test]
fn a_name_written_as_an_artists_sort_name_is_that_artist() {
    for mbid in [Some("art-iris"), None] {
        let mut world = World::default();
        for (at, path) in ["A/1.flac", "A/2.flac"].into_iter().enumerate() {
            world.add(
                file(path, at as i64 + 1)
                    .song(&format!("Song {at}"), "Iris Vane", 200)
                    .on("Tin Weather", None, at as i64 + 1)
                    .with(|facts| {
                        facts.artist_sort = Some("Vane, Iris".to_owned());
                        facts.artist_mbids = mbid.into_iter().map(str::to_owned).collect();
                    }),
            );
        }
        // Another album whose artist tag holds the sort name.
        world.add(
            file("B/1.flac", 3)
                .song("Lanterns", "Vane, Iris", 200)
                .on("Low Field", None, 1),
        );
        let resolution = world.settle();
        let names: Vec<&str> = resolution.artists.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["Iris Vane"], "{mbid:?}");
        // Credited as written.
        let credit = &resolution.credits[resolution.appearances[2].credit.unwrap()];
        assert_eq!(credit.names, [(0, "Vane, Iris".to_owned(), String::new())]);
        world.assert_settled();
    }
}

#[test]
fn a_sort_name_two_artists_claim_names_neither() {
    let mut world = World::default();
    for (at, (artist, mbid)) in [("Iris Vane", "art-iris"), ("Iris  Vane", "art-other")]
        .into_iter()
        .enumerate()
    {
        world.add(
            file(["A/1.flac", "C/1.flac"][at], at as i64 + 1)
                .song("Song", artist, 200)
                .with(|facts| {
                    facts.artist_sort = Some("Vane, Iris".to_owned());
                    facts.artist_mbids = vec![mbid.to_owned()];
                }),
        );
    }
    world.add(file("B/1.flac", 3).song("Lanterns", "Vane, Iris", 200));
    let resolution = world.settle();
    assert_eq!(resolution.artists.len(), 3);
    world.assert_settled();
}

#[test]
fn the_owners_assignment_is_kept_and_an_exclusion_removes_a_candidate() {
    let mut world = World::default();
    let first = world.add(file("A/1.flac", 1).song("Glass Pier", "Ninefold", 200));
    let second = world.add(file("B/1.flac", 2).song("Glass Pier", "Ninefold", 200));
    let third = world.add(file("C/1.flac", 3).song("Salt Road", "Ninefold", 200));
    world.settle();
    let (shared, other) = (world.recording(first), world.recording(third));
    assert_eq!(world.recording(second), shared);

    // The owner says the second file is the third's recording.
    world.library.items[1].current = Some(Current {
        recording: other,
        appearance: None,
        method: Method::User,
        by_user: true,
        needs_review: false,
        across_albums: false,
    });
    let resolution = world.settle();
    assert!(resolution.items[1].pinned && !resolution.items[1].changed);
    assert_eq!(resolution.items[1].decision, None);
    assert_eq!(world.recording(second), other);

    // Never-link: the second file is not the first's recording, so with the
    // pin gone it gets one of its own.
    world.library.items[1].current = None;
    world.library.exclusions.insert((second, shared));
    let resolution = world.settle();
    assert_eq!(decision(&resolution, second).method, Method::New);
    assert!(![shared, other].contains(&world.recording(second)));
    world.assert_settled();
}

#[test]
fn a_file_never_keeps_the_recording_made_for_it_once_it_is_never_linked_to_it() {
    let mut world = World::default();
    let first = world.add(file("A/1.flac", 1).song("Glass Pier", "Ninefold", 200));
    let other = world.add(file("B/1.flac", 2).song("Salt Road", "Ninefold", 200));
    world.settle();
    let made = world.recording(first);
    // The owner says the file is not the recording that was made for it.
    world.library.exclusions.insert((first, made));
    world.settle();
    assert_ne!(world.recording(first), made);

    // The same when that recording has since been merged into another.
    let merged = world.recording(first);
    let into = world.recording(other);
    world.library.recording_canonical.insert(merged, into);
    world.library.exclusions.insert((first, into));
    world.settle();
    assert!(![merged, into].contains(&world.recording(first)));
    world.assert_settled();
}

#[test]
fn a_merge_leaves_assignments_alone_and_the_merged_recording_is_no_candidate() {
    let mut world = World::default();
    let first = world.add(file("A/1.flac", 1).song("Glass Pier", "Ninefold", 200));
    let second = world.add(file("B/1.flac", 2).song("Glass Pier", "Ninefold", 230));
    world.settle();
    let (kept, merged) = (world.recording(first), world.recording(second));
    assert_ne!(kept, merged);
    world.library.recording_canonical.insert(merged, kept);
    let resolution = world.settle();
    assert!(resolution.items.iter().all(|item| !item.changed));
    assert_eq!(world.recording(second), merged);
    assert_eq!(resolution.recordings.len(), 1);

    let third = world.add(file("C/1.flac", 3).song("Glass Pier", "Ninefold", 230));
    world.settle();
    assert_eq!(world.recording(third), kept);
}

#[test]
fn names_that_the_same_audio_showed_to_be_one_widen_the_candidates() {
    let mut world = World::default();
    let first = world.add(file("A/1.flac", 1).song("Glass Pier", "Ne$ta", 200));
    let same_audio = world.add(file("B/1.flac", 1).song("Glass Pier", "Nesta", 200));
    let later = world.add(file("C/1.flac", 2).song("Salt Road", "Ne$ta", 200));
    let by_alias = world.add(file("D/1.flac", 3).song("Salt Road", "Nesta", 200));
    let resolution = world.settle();
    assert_eq!(
        decision(&resolution, same_audio).aliases,
        [(Kind::Artist, "nesta".to_owned(), "neta".to_owned())]
    );
    let decided = decision(&resolution, by_alias);
    assert_eq!(decided.method, Method::TagsDuration);
    assert_eq!(decided.penalties.unwrap().artist, 0.0);
    assert_eq!(world.recording(later), world.recording(by_alias));
    assert_eq!(world.recording(first), world.recording(same_audio));
    world.assert_settled();
}

#[test]
fn replaced_audio_keeps_its_item_only_if_it_lands_on_the_same_recording() {
    let mut world = World::default();
    world.add(
        file("A/1.flac", 1)
            .song("Glass Pier", "Ninefold", 200)
            .on("Long Field", None, 1),
    );
    world.add(
        file("A/2.flac", 2)
            .song("Salt Road", "Ninefold", 210)
            .on("Long Field", None, 2),
    );
    world.settle();
    // The owner pinned the first file, which must not hold a new song.
    world.library.items[0].current.as_mut().unwrap().by_user = true;
    let facts = |title: &str, seconds: i64| {
        file("A/1.flac", 0)
            .song(title, "Ninefold", seconds)
            .on("Long Field", None, 1)
            .facts
    };
    let replacement = |facts, audio| Replacement {
        index: 0,
        audio,
        facts,
    };
    let judged = judge_replacements(
        &world.library,
        &[
            // The same song, encoded again.
            replacement(facts("Glass Pier", 201), None),
            // A different song saved over the old name.
            replacement(facts("Third Rail", 200), None),
            // The other file's audio, copied over this one.
            replacement(facts("Salt Road", 210), Some(2)),
            // Untagged audio.
            replacement(Facts::default(), None),
        ],
        &Policy::v1(),
    );
    assert_eq!(judged, [true, false, false, false]);
}

#[test]
fn a_retag_moves_the_appearance_and_keeps_the_recording_made_for_the_file() {
    let mut world = World::default();
    let id = world.add(file("A/1.flac", 1).song("Glass Pier", "Ninefold", 200).on(
        "Long Feild",
        None,
        1,
    ));
    world.settle();
    let (recording, release) = (world.recording(id), world.release(id));
    world.library.items[0].facts.album = Some("Long Field".to_owned());
    world.library.items[0].facts.title = Some("Glass Pier!".to_owned());
    let resolution = world.settle();
    // The release is still the one its file is on, shown under its new
    // title, so nothing about the binding changed.
    assert!(!resolution.items[0].changed);
    assert_eq!(
        (world.recording(id), world.release(id)),
        (recording, release)
    );
    assert_eq!(resolution.releases[0].title.as_deref(), Some("Long Field"));
    assert_eq!(
        resolution.appearances[0].title.as_deref(),
        Some("Glass Pier!")
    );
    world.assert_settled();
}

#[test]
fn the_normalised_name_is_what_an_artist_row_is_found_by() {
    let mut world = World::default();
    world.add(file("A/1.flac", 1).song("One", "Ada Wren", 200));
    let resolution = world.settle();
    assert_eq!(
        resolution.artists[0].match_name,
        normalize::normalize("ADA WREN")
    );
    world.library.items[0].facts.artist = vec!["ADA WREN".to_owned()];
    let resolution = world.settle();
    assert!(matches!(resolution.artists[0].r, Ref::Row(_)));
    assert_eq!(resolution.artists[0].name, "ADA WREN");
}

#[test]
fn a_resolution_names_what_the_owner_pinned_and_what_an_exclusion_blocked() {
    let mut world = World::default();
    let first = world.add(file("A/1.flac", 1).song("Glass Pier", "Ninefold", 200));
    let second = world.add(file("B/1.flac", 2).song("Glass Pier", "Ninefold", 200));
    let third = world.add(file("C/1.flac", 3).song("Salt Road", "Ninefold", 200));
    world.settle();
    let shared = world.recording(first);
    assert_eq!(world.recording(second), shared);

    // A confirmation: the owner's assignment to the recording it has.
    world.library.items[2].current.as_mut().unwrap().by_user = true;
    // A never-link, written with the re-resolution that a later slice's
    // call will make: the second file is not the first's recording.
    world.library.exclusions.insert((second, shared));
    world.settle();
    assert_ne!(world.recording(second), shared);

    // Asked again, nothing changes, and each is named.
    let again = catalog::resolve(&world.library, &Policy::v1());
    assert!(again.items.iter().all(|item| !item.changed));
    let flags = |id: i64| {
        let out = again.items.iter().find(|out| out.item == id).unwrap();
        (out.pinned, out.blocked)
    };
    assert_eq!(flags(first), (false, false));
    assert_eq!(flags(second), (false, true));
    assert_eq!(flags(third), (true, false));
    world.assert_settled();
}

#[test]
fn an_artist_with_an_id_keeps_its_row_by_the_id_and_never_takes_a_namesakes() {
    let mut world = World::default();
    let by = |path, audio, title: &str, mbid: &str| {
        let mbid = mbid.to_owned();
        file(path, audio)
            .song(title, "Ada Wren", 200)
            .with(move |facts| facts.artist_mbids = vec![mbid])
    };
    world.add(by("A/1.flac", 1, "Glass Pier", "art-x"));
    world.add(by("B/1.flac", 2, "Salt Road", "art-y"));
    let resolution = world.settle();
    assert_eq!(resolution.artists.len(), 2);
    let row = |mbid: &str| {
        *world
            .library
            .artist_mbids
            .iter()
            .find(|(_, id)| id.as_str() == mbid)
            .unwrap()
            .0
    };
    let (x, y) = (row("art-x"), row("art-y"));
    assert_ne!(x, y);

    // The elder's files go, and the younger keeps its own row.
    world.library.items[0].retired = true;
    let resolution = world.settle();
    assert_eq!(resolution.artists.len(), 1);
    assert_eq!(resolution.artists[0].r, Ref::Row(y));

    // Renamed in its tags with the same ID, it is the same row.
    world.library.items[1].facts.artist = vec!["Ada Wren-Holt".to_owned()];
    let resolution = world.settle();
    assert_eq!(resolution.artists[0].r, Ref::Row(y));
    assert_eq!(resolution.artists[0].name, "Ada Wren-Holt");
    world.assert_settled();
}

#[test]
fn a_merged_release_stays_one_release_whatever_the_tags_say() {
    let mut world = World::default();
    let album = world.add(
        file("X/1.flac", 1)
            .song("Glass Pier", "Ninefold", 200)
            .on("Long Field", None, 1)
            .by("Ninefold"),
    );
    let deluxe = world.add(
        file("Y/2.flac", 2)
            .song("Salt Road", "Ninefold", 210)
            .on("Long Field (Deluxe)", None, 2)
            .by("Ninefold"),
    );
    world.settle();
    let (kept, merged) = (
        world.release(album).unwrap(),
        world.release(deluxe).unwrap(),
    );
    assert_ne!(kept, merged);
    // The owner merges the deluxe edition into the album.
    world.library.releases.get_mut(&merged).unwrap().canonical = kept;
    let resolution = world.settle();
    assert!(
        resolution.items.iter().all(|item| !item.changed),
        "{:#?}",
        resolution.items
    );
    assert_eq!(resolution.releases.len(), 1);
    assert_eq!(resolution.releases[0].r, Ref::Row(kept));
    world.assert_settled();
}

#[test]
fn a_confirmed_file_stays_where_it_is_and_still_shows_what_its_tags_say() {
    let mut world = World::default();
    let id = world.add(
        file("A/1.flac", 1)
            .song("Glass Pier", "Ninefold", 200)
            .on("Long Field", None, 1)
            .by("Ninefold"),
    );
    world.settle();
    let current = world.item(id).current.unwrap();
    let (recording, appearance, release) = (
        current.recording,
        current.appearance.unwrap(),
        world.release(id),
    );
    // The owner confirms the binding, and later retags the file.
    world.library.items[0].current = Some(Current {
        method: Method::User,
        by_user: true,
        ..current
    });
    world.library.items[0].facts.title = Some("Glass Pier (Live at Home)".to_owned());
    world.library.items[0].facts.album = Some("Long Field (Expanded)".to_owned());
    let resolution = world.settle();
    let out = &resolution.items[0];
    assert!(out.pinned && !out.changed);
    assert_eq!(out.appearance, Some(Ref::Row(appearance)));
    assert_eq!(world.release(id), release);
    let shown = resolution
        .appearances
        .iter()
        .find(|out| out.r == Ref::Row(appearance))
        .expect("the confirmed file's appearance is projected");
    assert_eq!(shown.title.as_deref(), Some("Glass Pier (Live at Home)"));
    let projected = resolution
        .recordings
        .iter()
        .find(|out| out.r == Ref::Row(recording))
        .unwrap();
    assert!(projected.projected);
    assert_eq!(
        projected.title.as_deref(),
        Some("Glass Pier (Live at Home)")
    );
    let held = resolution
        .releases
        .iter()
        .find(|out| out.r == Ref::Row(release.unwrap()))
        .expect("the confirmed file's release is projected");
    assert_eq!(held.title.as_deref(), Some("Long Field (Expanded)"));
    world.assert_settled();
}

/// What a policy other than policy 1 would change about a settled world:
/// for each item, whether it changed and the method it was decided by.
fn under(world: &World, policy: &Policy) -> Vec<(bool, Option<Method>)> {
    catalog::resolve(&world.library, policy)
        .items
        .iter()
        .map(|out| (out.changed, out.decision.as_ref().map(|d| d.method)))
        .collect()
}

#[test]
fn folding_accents_binds_a_spelling_that_policy_1_leaves_for_review() {
    let mut world = World::under(Policy::v1());
    let first = world.add(file("A/1.flac", 1).song("Glass Pier", "Ada Wrén", 200));
    let plain = world.add(file("B/1.flac", 2).song("Glass Pier", "Ada Wren", 200));
    let resolution = world.settle();
    assert_eq!(decision(&resolution, plain).method, Method::New);
    assert!(decision(&resolution, plain).needs_review);
    assert_ne!(world.recording(first), world.recording(plain));
    assert!(
        under(&world, &Policy::v1())
            .iter()
            .all(|(changed, _)| !changed)
    );

    let folded = Policy {
        fold_accents: true,
        ..Policy::v1()
    };
    assert_eq!(
        under(&world, &folded),
        [
            (false, Some(Method::New)),
            (true, Some(Method::TagsDuration))
        ]
    );
    let resolution = catalog::resolve(&world.library, &folded);
    assert_eq!(
        resolution.items[1].recording,
        Some(Ref::Row(world.recording(first)))
    );
    let decided = decision(&resolution, plain);
    assert_eq!(decided.score, 0.0);
    assert!(!decided.needs_review);
}

#[test]
fn a_wider_free_duration_band_binds_lengths_policy_1_keeps_apart() {
    let mut world = World::under(Policy::v1());
    world.add(file("A/1.flac", 1).song("Glass Pier", "Ninefold", 200));
    // Seven seconds longer: five beyond the free two.
    let longer = world.add(file("B/1.flac", 2).song("Glass Pier", "Ninefold", 207));
    let resolution = world.settle();
    let decided = decision(&resolution, longer);
    assert_eq!(decided.method, Method::New);
    assert!(decided.needs_review);
    let penalties = decided.evidence.candidates[0].penalties.unwrap();
    assert!((penalties.duration - 0.25).abs() < 1e-9);
    assert_eq!((penalties.title, penalties.artist), (0.0, 0.0));

    let band = |seconds: i64| Policy {
        duration_grace_ms: seconds * 1000,
        ..Policy::v1()
    };
    assert!(under(&world, &band(3)).iter().all(|(changed, _)| !changed));
    assert_eq!(
        under(&world, &band(6))[1],
        (true, Some(Method::TagsDuration))
    );
    // The thresholds move the same line from the other side.
    let loose = Policy {
        bind_max: 0.1,
        ..Policy::v1()
    };
    assert_eq!(under(&world, &loose)[1], (true, Some(Method::TagsDuration)));
    let strict = Policy {
        review_below: 0.05,
        ..Policy::v1()
    };
    // A stricter review line keeps the binding and clears the mark, which
    // is a change: the decision in force would be written again.
    let resolution = catalog::resolve(&world.library, &strict);
    assert!(resolution.items[1].changed);
    assert_eq!(
        resolution.items[1].recording,
        Some(Ref::Row(world.recording(longer)))
    );
    assert!(!decision(&resolution, longer).needs_review);
}

/// An album in two folders, with two songs each and no disc number.
fn two_folders(world: &mut World, folders: [&str; 2], albums: [&str; 2], shared: bool) -> Vec<i64> {
    let mut ids = Vec::new();
    for (at, (folder, album)) in folders.iter().zip(albums).enumerate() {
        for track in 1..=2_i64 {
            let path: &'static str = Box::leak(format!("{folder}/{track}.flac").into_boxed_str());
            let title = if shared {
                format!("Part {track}")
            } else {
                format!("Part {at}-{track}")
            };
            ids.push(
                world.add(
                    file(path, at as i64 * 10 + track)
                        .song(&title, "Ninefold", 240 + 10 * at as i64)
                        .on(album, None, track)
                        .by("Ninefold"),
                ),
            );
        }
    }
    ids
}

/// The releases and the discs a policy gives a settled world.
fn discs(world: &World, policy: &Policy) -> (usize, Vec<Option<i64>>) {
    let resolution = catalog::resolve(&world.library, policy);
    let discs = resolution
        .items
        .iter()
        .map(|out| {
            resolution
                .appearances
                .iter()
                .find(|appearance| Some(appearance.r) == out.appearance)
                .and_then(|appearance| appearance.disc)
        })
        .collect();
    (resolution.releases.len(), discs)
}

#[test]
fn a_disc_marker_in_the_album_title_can_be_taken_as_the_disc() {
    let mut world = World::default();
    two_folders(
        &mut world,
        ["Ninefold/Long Field 1", "Ninefold/Long Field 2"],
        ["Long Field (Disc 1)", "Long Field (Disc 2)"],
        false,
    );
    world.settle();
    let off = discs(&world, &Policy::v1());
    assert_eq!(off, (2, vec![None; 4]));
    let on = Policy {
        disc_from_album_title: true,
        ..Policy::v1()
    };
    assert_eq!(
        discs(&world, &on),
        (1, vec![Some(1), Some(1), Some(2), Some(2)])
    );
    let resolution = catalog::resolve(&world.library, &on);
    assert_eq!(resolution.releases[0].title.as_deref(), Some("Long Field"));
    // The recordings stay as they are; only where the files sit moves.
    assert!(
        resolution
            .items
            .iter()
            .all(|out| out.decision.as_ref().unwrap().method == Method::New)
    );

    // Settled under it, it asks for nothing more.
    world.policy = on;
    world.settle();
    world.assert_settled();
}

#[test]
fn sibling_folders_that_collide_and_share_no_title_can_be_discs_of_one_release() {
    let on = Policy {
        discs_from_folders: true,
        ..Policy::v1()
    };
    // Named by a marker: the disc is the folder's.
    let mut world = World::default();
    two_folders(
        &mut world,
        ["Ninefold/Long Field/CD2", "Ninefold/Long Field/CD1"],
        ["Long Field"; 2],
        false,
    );
    world.settle();
    assert_eq!(discs(&world, &Policy::v1()).0, 2);
    assert_eq!(
        discs(&world, &on),
        (1, vec![Some(2), Some(2), Some(1), Some(1)])
    );

    // Folders whose names give no disc are never joined: two different
    // records of one title would look the same.
    let mut world = World::default();
    two_folders(
        &mut world,
        ["Ninefold/Long Field/Side B", "Ninefold/Long Field/Side A"],
        ["Long Field"; 2],
        false,
    );
    world.settle();
    assert_eq!(discs(&world, &on), (2, vec![None; 4]));
    // Nor are they when only one of them does.
    let mut world = World::default();
    two_folders(
        &mut world,
        ["Ninefold/Long Field/CD2", "Ninefold/Long Field/Bonus"],
        ["Long Field"; 2],
        false,
    );
    world.settle();
    assert_eq!(discs(&world, &on).0, 2);

    // Two copies of one album share their titles, and stay apart.
    let mut world = World::default();
    two_folders(
        &mut world,
        ["Ninefold/Long Field", "Ninefold/Long Field (FLAC)"],
        ["Long Field"; 2],
        true,
    );
    world.settle();
    assert_eq!(discs(&world, &on), (2, vec![None; 4]));

    // Folders that are not siblings stay apart too.
    let mut world = World::default();
    two_folders(
        &mut world,
        ["Ninefold/Long Field/CD1", "Elsewhere/Long Field/CD2"],
        ["Long Field"; 2],
        false,
    );
    world.settle();
    assert_eq!(discs(&world, &on).0, 2);
}

#[test]
fn policy_2_is_policy_1_with_four_choices_changed_and_is_what_scans_use() {
    let (one, two) = (Policy::v1(), Policy::v2());
    assert_eq!((one.version, two.version), (1, 2));
    assert_eq!(Policy::current(), two);
    assert_eq!(Policy::default(), two);
    assert_eq!(crate::RESOLVER_VERSION, two.version);
    assert_eq!(Policy::of_version(1), Some(one.clone()));
    assert_eq!(Policy::of_version(2), Some(two.clone()));
    assert_eq!(Policy::of_version(3), None);
    // The free band stays at two seconds, and every number is policy 1's.
    assert_eq!(two.duration_grace_ms, 2_000);
    assert_eq!(
        Policy {
            version: 1,
            fold_accents: false,
            discs_from_folders: false,
            disc_from_album_title: false,
            tags_bind_across_albums: true,
            across_albums_within_ms: 0,
            tracks_of_one_release_differ: false,
            marker_list: 1,
            ..two
        },
        one
    );
}

/// What `second` is decided as when `first` was there before it, under a
/// policy: the two settled in a world of their own.
fn after_first(first: File, second: File, policy: Policy) -> (World, i64, i64, Decision) {
    let mut world = World::under(policy);
    let first = world.add(first);
    let second = world.add(second);
    let resolution = world.settle();
    let decided = decision(&resolution, second).clone();
    world.assert_settled();
    (world, first, second, decided)
}

fn on_album() -> File {
    file("Ada Wren/Field Notes/03.flac", 1)
        .song("Coal Light", "Ada Wren", 215)
        .on("Field Notes", None, 3)
        .by("Ada Wren")
}

fn on_compilation() -> File {
    file("Compilations/Night Drives/07.flac", 2)
        .song("Coal Light", "Ada Wren", 216)
        .on("Night Drives", None, 7)
        .by("Various Artists")
        .with(|facts| facts.compilation = true)
}

#[test]
fn under_policy_2_a_match_on_tags_alone_never_binds_across_albums() {
    let (world, album, compilation, decided) =
        after_first(on_album(), on_compilation(), Policy::v2());
    assert_eq!(decided.method, Method::New);
    assert!(decided.needs_review);
    assert_ne!(world.recording(album), world.recording(compilation));
    // The near match is in the evidence: the candidate, what made up its
    // distance, and the cause and the recording it was held back from.
    let nearest = &decided.evidence.candidates[0];
    assert_eq!(nearest.item, album);
    assert!((nearest.distance - 0.3 / 9.3).abs() < 1e-9);
    assert_eq!(nearest.penalties.unwrap().album, 1.0);
    let withheld = decided.evidence.withheld.unwrap();
    assert_eq!(
        (withheld.cause, withheld.item),
        (WithheldCause::OtherAlbum, album)
    );
    assert_eq!(withheld.cause.as_str(), "other_album");
    // Resolved again, it names the recording as the row it now is.
    let again = catalog::resolve(&world.library, &Policy::v2());
    assert_eq!(
        decision(&again, compilation)
            .evidence
            .withheld
            .unwrap()
            .recording,
        Some(Ref::Row(world.recording(album)))
    );

    // The switch alone, on policy 1, does the same.
    let alone = Policy {
        tags_bind_across_albums: false,
        ..Policy::v1()
    };
    let (_, _, _, decided) = after_first(on_album(), on_compilation(), alone);
    assert_eq!((decided.method, decided.needs_review), (Method::New, true));
    assert!(decided.evidence.withheld.is_some());

    // And the dry run can ask what binding across albums would do.
    let across = Policy {
        tags_bind_across_albums: true,
        ..Policy::v2()
    };
    assert_eq!(
        under(&world, &across),
        [
            (false, Some(Method::New)),
            (true, Some(Method::TagsDuration))
        ]
    );
}

#[test]
fn a_song_on_three_albums_is_held_back_from_each_whatever_the_runner_up() {
    let mut world = World::default();
    let album = world.add(on_album());
    let compilation = world.add(on_compilation());
    let sampler = world.add(
        file("Compilations/Harbour Sampler/01.flac", 3)
            .song("Coal Light", "Ada Wren", 214)
            .on("Harbour Sampler", None, 1)
            .by("Various Artists"),
    );
    let resolution = world.settle();
    // The sampler's song is near both earlier recordings, each on another
    // album: one is no clear winner over the other, and still the cause is
    // that tags do not bind across albums.
    let decided = decision(&resolution, sampler);
    assert_eq!((decided.method, decided.needs_review), (Method::New, true));
    assert_eq!(decided.evidence.candidates.len(), 2);
    assert!(decided.runner_up_gap.unwrap() < 0.01);
    let withheld = decided.evidence.withheld.unwrap();
    assert_eq!(
        (withheld.cause, withheld.item),
        (WithheldCause::OtherAlbum, album)
    );
    let recordings = [album, compilation, sampler].map(|id| world.recording(id));
    assert!(recordings[0] != recordings[1] && recordings[1] != recordings[2]);
    world.assert_settled();
}

#[test]
fn under_policy_2_tags_bind_across_albums_when_the_lengths_all_but_agree() {
    let at = |ms: i64| on_compilation().with(move |facts| facts.duration_ms = Some(215_000 + ms));
    // Within 20 ms: bound, and the evidence says by which exception.
    for apart in [0, 13, 20] {
        let (world, album, compilation, decided) = after_first(on_album(), at(apart), Policy::v2());
        assert_eq!(decided.method, Method::TagsDuration, "{apart}");
        assert!(!decided.needs_review);
        assert_eq!(world.recording(album), world.recording(compilation));
        assert_eq!(
            decided.evidence.across_albums,
            Some(AcrossAlbums {
                length_difference_ms: apart
            })
        );
        assert_eq!(decided.evidence.withheld, None);
    }
    // Beyond it, held back for review as before.
    let (_, _, _, decided) = after_first(on_album(), at(21), Policy::v2());
    assert_eq!((decided.method, decided.needs_review), (Method::New, true));
    assert!(decided.evidence.withheld.is_some());
    assert_eq!(decided.evidence.across_albums, None);
    // 0 makes no exception, even for an equal length.
    let none = Policy {
        across_albums_within_ms: 0,
        ..Policy::v2()
    };
    let (_, _, _, decided) = after_first(on_album(), at(0), none);
    assert_eq!((decided.method, decided.needs_review), (Method::New, true));
    // A file on the same album needs no exception, and policy 1 binds
    // across albums without one: neither marks its evidence.
    let (_, _, _, decided) = after_first(on_album(), at(0), Policy::v1());
    assert_eq!(decided.method, Method::TagsDuration);
    assert_eq!(decided.evidence.across_albums, None);
    let copy = file("Backup/Field Notes/03.flac", 3)
        .song("Coal Light", "Ada Wren", 216)
        .on("Field Notes", None, 3)
        .by("Ada Wren");
    let (_, _, _, decided) = after_first(on_album(), copy, Policy::v2());
    assert_eq!(decided.method, Method::TagsDuration);
    assert_eq!(decided.evidence.across_albums, None);
}

#[test]
fn a_binding_policy_1_made_across_albums_at_an_equal_length_is_marked_by_policy_2() {
    let mut world = World::under(Policy::v1());
    let album = world.add(on_album());
    let equal = world.add(on_compilation().with(|facts| facts.duration_ms = Some(215_000)));
    world.settle();
    assert_eq!(world.recording(album), world.recording(equal));
    // Policy 2 keeps the binding, and writes it again so that its evidence
    // says it rests on the exception.
    assert_eq!(
        differences(&world, &Policy::v2()),
        [(equal, false, Method::TagsDuration, false)]
    );
    world.policy = Policy::v2();
    let resolution = world.settle();
    assert!(
        decision(&resolution, equal)
            .evidence
            .across_albums
            .is_some()
    );
    assert_eq!(world.recording(album), world.recording(equal));
    world.assert_settled();
}

/// A track of an album, by its folder, audio, title, position and length.
fn track(
    path: &'static str,
    audio: i64,
    title: &str,
    disc: i64,
    number: i64,
    seconds: i64,
) -> File {
    file(path, audio)
        .song(title, "Ninefold", seconds)
        .on("Long Field", Some(disc), number)
        .by("Ninefold")
}

#[test]
fn under_policy_2_two_tracks_of_one_release_are_never_one_recording_by_tags() {
    // An alternate take later on the same album, at the same length.
    let first = || track("Ninefold/Long Field/01.flac", 1, "Glass Pier", 1, 1, 200);
    let later = || track("Ninefold/Long Field/09.flac", 2, "Glass Pier", 1, 9, 200);
    let (world, one, nine, decided) = after_first(first(), later(), Policy::v2());
    assert_eq!((decided.method, decided.needs_review), (Method::New, true));
    assert_ne!(world.recording(one), world.recording(nine));
    let withheld = decided.evidence.withheld.unwrap();
    assert_eq!(
        (withheld.cause, withheld.item),
        (WithheldCause::SameRelease, one)
    );
    assert_eq!(withheld.cause.as_str(), "same_release");
    // Policy 1, or policy 2 with the switch off, binds it.
    for policy in [
        Policy::v1(),
        Policy {
            tracks_of_one_release_differ: false,
            ..Policy::v2()
        },
    ] {
        let (_, _, _, decided) = after_first(first(), later(), policy);
        assert_eq!(decided.method, Method::TagsDuration);
    }
    // The same on another disc of the release, in a folder of its own.
    let (_, _, _, decided) = after_first(
        track(
            "Ninefold/Long Field/Disc 1/08.flac",
            1,
            "Glass Pier",
            1,
            8,
            200,
        ),
        track(
            "Ninefold/Long Field/Disc 2/08.flac",
            2,
            "Glass Pier",
            2,
            8,
            201,
        ),
        Policy::v2(),
    );
    assert_eq!(
        decided.evidence.withheld.unwrap().cause,
        WithheldCause::SameRelease
    );
    // A copy of that disc that lost its disc tag, at the same track, is
    // not another position.
    let (_, _, _, decided) = after_first(
        track(
            "Ninefold/Long Field/Disc 2/08.flac",
            1,
            "Glass Pier",
            2,
            8,
            200,
        ),
        track(
            "Ninefold/Long Field/CD2/08.flac",
            2,
            "Glass Pier",
            2,
            8,
            200,
        )
        .with(|facts| facts.disc = None),
        Policy::v2(),
    );
    assert_eq!(decided.method, Method::TagsDuration);
    // A file with no track number has no position, and binds.
    let (_, _, _, decided) = after_first(
        first(),
        later().with(|facts| facts.track = None),
        Policy::v2(),
    );
    assert_eq!(decided.method, Method::TagsDuration);
    // The same audio twice on one release is one recording.
    let mut twice = later();
    twice.audio = 1;
    let (_, _, _, decided) = after_first(first(), twice, Policy::v2());
    assert_eq!(decided.method, Method::SameAudio);
}

#[test]
fn two_copies_of_one_album_are_two_releases_and_bind_track_to_track_in_any_order() {
    let mut world = World::default();
    let original = album(
        &mut world,
        "Moss Arcade/Tin Weather",
        "Moss Arcade",
        "Tin Weather",
        10,
    );
    // A reissue in another folder, its tracks in another order.
    let names = ["Paper Kites", "Low Tide", "Harbour Lights"];
    let reissue: Vec<i64> = [3_i64, 1, 2]
        .iter()
        .enumerate()
        .map(|(at, number)| {
            let path: &'static str = Box::leak(
                format!("Moss Arcade/Tin Weather (2010)/{number:02}.flac").into_boxed_str(),
            );
            world.add(
                file(path, 20 + at as i64)
                    .song(names[at], "Moss Arcade", 200 + 10 * at as i64)
                    .on("Tin Weather", Some(1), *number)
                    .by("Moss Arcade"),
            )
        })
        .collect();
    let resolution = world.settle();
    for (one, other) in original.iter().zip(&reissue) {
        assert_eq!(decision(&resolution, *other).method, Method::TagsDuration);
        assert_eq!(world.recording(*one), world.recording(*other));
    }
    assert_eq!(resolution.releases.len(), 2);
    world.assert_settled();
}

#[test]
fn policy_2_reads_every_spelling_of_a_cappella_and_part_numbers_as_markers() {
    let two = |title: &str| markers_of(title, 2);
    for title in [
        "Lantern (A Capella)",
        "Lantern (a cappella)",
        "Lantern - Acapella",
        "Lantern [Acappella]",
    ] {
        assert_eq!(two(title), ["a cappella"], "{title}");
    }
    for title in [
        "Suite (Part II)",
        "Suite, Pt. 2",
        "Suite Part Two",
        "Suite - Part 2",
        "Suite pt 2",
    ] {
        assert_eq!(two(title), ["part 2"], "{title}");
    }
    assert_eq!(two("Suite (Part I)"), ["part 1"]);
    assert_eq!(two("Suite (Part IV)"), ["part 4"]);
    for title in [
        "Part of Me",
        "Party Line",
        "Departure",
        "Suite",
        "The Parting",
    ] {
        assert!(two(title).is_empty(), "{title}");
    }
    // A marker written across punctuation inside a word is still one.
    assert_eq!(two("Lantern (Re-recorded)"), ["rerecorded"]);
    assert_eq!(two("Lantern (Re-Recorded Version)"), ["rerecorded"]);
    // Policy 1's list is as it was: two spellings are two markers, and a
    // part is none.
    assert_eq!(markers_of("Lantern (Acapella)", 1), ["acapella"]);
    assert_eq!(markers_of("Lantern (A Cappella)", 1), ["cappella"]);
    assert!(markers_of("Lantern (A Capella)", 1).is_empty());
    assert!(markers_of("Suite (Part II)", 1).is_empty());

    let binds = |first: &str, second: &str, policy: Policy| {
        let (world, a, b, _) = after_first(
            file("A/1.flac", 1).song(first, "Ninefold", 200),
            file("B/1.flac", 2).song(second, "Ninefold", 200),
            policy,
        );
        world.recording(a) == world.recording(b)
    };
    // A different part, or a part against none, is another recording; the
    // same part on both sides is no difference.
    assert!(!binds(
        "The Don (Part II)",
        "The Don (Part I)",
        Policy::v2()
    ));
    assert!(binds("The Don (Part II)", "The Don (Part I)", Policy::v1()));
    assert!(!binds("The Don", "The Don (Part 2)", Policy::v2()));
    assert!(binds("The Don (Part 2)", "The Don (Part 2)", Policy::v2()));
    // An a cappella take is another recording, however it is spelled, and
    // two spellings of one are the same version.
    assert!(!binds(
        "Writing On A Wall",
        "Writing On A Wall (A Capella)",
        Policy::v2()
    ));
    assert!(binds(
        "Writing On A Wall",
        "Writing On A Wall (A Capella)",
        Policy::v1()
    ));
    assert!(binds(
        "Writing On A Wall (Acapella)",
        "Writing On A Wall (A Cappella)",
        Policy::v2()
    ));
    assert!(!binds(
        "Writing On A Wall (Acapella)",
        "Writing On A Wall (A Cappella)",
        Policy::v1()
    ));
}

#[test]
fn a_match_too_far_to_bind_on_another_album_is_reviewed_as_before() {
    // Six seconds apart on another album: no binding by either policy, so
    // nothing was held back, and the review is policy 1's.
    let longer = on_compilation().with(|facts| facts.duration_ms = Some(221_000));
    for policy in [Policy::v1(), Policy::v2()] {
        let (_, _, _, decided) = after_first(on_album(), longer.clone(), policy);
        assert_eq!((decided.method, decided.needs_review), (Method::New, true));
        assert_eq!(decided.evidence.withheld, None);
    }
}

#[test]
fn two_copies_of_one_album_still_bind_by_tags_whatever_else_is_near() {
    let mut world = World::default();
    let album = world.add(on_album());
    let compilation = world.add(on_compilation());
    // A second copy of the album in another folder, and a second copy of
    // the compilation tagged with no album artist.
    let copy = world.add(
        file("Backup/Field Notes/03.flac", 3)
            .song("Coal Light", "Ada Wren", 215)
            .on("Field Notes", None, 3)
            .by("Ada Wren"),
    );
    let compilation_copy = world.add(
        file("Backup/Night Drives/07.flac", 4)
            .song("Coal Light", "Ada Wren", 217)
            .on("Night Drives", None, 7),
    );
    let resolution = world.settle();
    assert_eq!(world.recording(copy), world.recording(album));
    assert_eq!(
        world.recording(compilation_copy),
        world.recording(compilation)
    );
    assert_ne!(world.recording(album), world.recording(compilation));
    // The other album's recording is near, and is no runner-up: tags never
    // bind to it.
    let decided = decision(&resolution, copy);
    assert_eq!(decided.method, Method::TagsDuration);
    assert_eq!((decided.score, decided.runner_up_gap), (0.0, None));
    assert_eq!(decided.evidence.candidates.len(), 2);
    assert!(!decided.needs_review);
    assert_eq!(
        decision(&resolution, compilation_copy).method,
        Method::TagsDuration
    );
    world.assert_settled();
}

#[test]
fn a_file_with_no_album_tag_is_on_no_other_album() {
    let mut world = World::default();
    let tagged = world.add(file("A/1.flac", 1).song("Glass Pier", "Ninefold", 200).on(
        "Long Field",
        None,
        1,
    ));
    let loose = world.add(file("Loose/1.flac", 2).song("Glass Pier", "Ninefold", 201));
    let also_loose = world.add(file("Loose/2.flac", 3).song("Glass Pier", "Ninefold", 200));
    let resolution = world.settle();
    for id in [loose, also_loose] {
        assert_eq!(decision(&resolution, id).method, Method::TagsDuration);
        assert_eq!(world.recording(id), world.recording(tagged));
    }
    assert_eq!(decision(&resolution, loose).penalties.unwrap().album, 0.5);
    world.assert_settled();
}

#[test]
fn which_albums_are_one_album_by_release_group_title_and_album_artist() {
    let song = |path: &'static str, audio, album: &str, by: &str| {
        file(path, audio)
            .song("Glass Pier", "Ninefold", 200)
            .on(album, None, 1)
            .by(by)
    };
    let group = |id: &str| {
        let id = id.to_owned();
        move |facts: &mut Facts| facts.release_group_mbid = Some(id)
    };
    // A second apart, so that no length is near enough for the exception.
    let binds = |first: File, second: File| {
        let second = second.with(|facts| facts.duration_ms = Some(201_000));
        let (world, first, second, decided) = after_first(first, second, Policy::v2());
        let bound = world.recording(first) == world.recording(second);
        assert_eq!(bound, decided.method == Method::TagsDuration, "{decided:?}");
        assert_eq!(!bound, decided.evidence.withheld.is_some(), "{decided:?}");
        bound
    };
    // A remaster titled otherwise, in the original's release group, is the
    // same album; with no group to say so, its title makes it another.
    let original = || song("A/1.flac", 1, "Long Field", "Ninefold");
    let remaster = || song("R/1.flac", 2, "Long Field 2019 Remaster", "Ninefold");
    assert!(binds(
        original().with(group("grp-1")),
        remaster().with(group("grp-1"))
    ));
    assert!(!binds(original(), remaster()));
    // Two self-titled albums in groups of their own are two albums, though
    // their titles and album artists are one.
    assert!(!binds(
        song("A/1.flac", 1, "Ninefold", "Ninefold").with(group("grp-a")),
        song("B/1.flac", 2, "Ninefold", "Ninefold").with(group("grp-b")),
    ));
    assert!(binds(
        song("A/1.flac", 1, "Ninefold", "Ninefold"),
        song("B/1.flac", 2, "Ninefold", "Ninefold"),
    ));
    // One release ID is one album.
    let release = |facts: &mut Facts| facts.release_mbid = Some("rel-1".to_owned());
    assert!(binds(
        original().with(release),
        song("R/1.flac", 2, "Long Field (2019)", "Ninefold").with(release),
    ));
    // An edition named in brackets is the same album.
    assert!(binds(
        original(),
        song("D/1.flac", 2, "Long Field (Deluxe Edition)", "Ninefold")
    ));
    // The same title by other album artists is another album; with the
    // album artist on one side only, the title decides.
    assert!(!binds(
        song("A/1.flac", 1, "Greatest Hits", "Ninefold"),
        song("B/1.flac", 2, "Greatest Hits", "Various Artists"),
    ));
    assert!(binds(
        song("A/1.flac", 1, "Greatest Hits", "Ninefold"),
        file("B/1.flac", 2)
            .song("Glass Pier", "Ninefold", 200)
            .on("Greatest Hits", None, 1),
    ));
    // A learned alias of the album artist counts as the same artist.
    let mut world = World::default();
    world.library.aliases.push((
        Kind::Artist,
        "ninefold".to_owned(),
        "the ninefold".to_owned(),
    ));
    let first = world.add(original());
    let second = world.add(song("B/1.flac", 2, "Long Field", "The Ninefold"));
    world.settle();
    assert_eq!(world.recording(first), world.recording(second));
}

#[test]
fn identifiers_still_bind_across_albums_under_policy_2() {
    // The same audio.
    let mut same = on_compilation();
    same.audio = 1;
    let (world, album, compilation, decided) = after_first(on_album(), same, Policy::v2());
    assert_eq!(decided.method, Method::SameAudio);
    assert_eq!(world.recording(album), world.recording(compilation));

    // A recording MBID both carry.
    let tagged = |file: File| file.with(|facts| facts.recording_mbid = Some("rec-1".to_owned()));
    let (world, album, compilation, decided) =
        after_first(tagged(on_album()), tagged(on_compilation()), Policy::v2());
    assert_eq!(decided.method, Method::Identifier);
    assert_eq!(world.recording(album), world.recording(compilation));

    // Unless the claims contradict it, as under policy 1.
    let far = tagged(on_compilation()).with(|facts| facts.duration_ms = Some(260_000));
    let (world, album, compilation, decided) = after_first(tagged(on_album()), far, Policy::v2());
    assert_eq!((decided.method, decided.needs_review), (Method::New, true));
    assert_eq!(decided.evidence.contradictions.len(), 1);
    assert_eq!(decided.evidence.withheld, None);
    assert_ne!(world.recording(album), world.recording(compilation));
}

#[test]
fn policy_2_keeps_the_free_band_at_two_seconds() {
    // Five seconds apart on one album: three beyond the free band, which
    // is too far to bind, so both policies leave it for review. A band of
    // three seconds would bind it.
    let first = || {
        file("A/1.flac", 1)
            .song("Salt Road", "Ninefold", 200)
            .on("Low Tide", None, 1)
    };
    let later = || {
        file("B/1.flac", 2)
            .song("Salt Road", "Ninefold", 205)
            .on("Low Tide", None, 1)
    };
    for policy in [Policy::v1(), Policy::v2()] {
        let (_, _, _, decided) = after_first(first(), later(), policy);
        assert_eq!((decided.method, decided.needs_review), (Method::New, true));
        assert_eq!(decided.evidence.withheld, None);
    }
    let wider = Policy {
        duration_grace_ms: 3_000,
        ..Policy::v2()
    };
    let (_, _, _, decided) = after_first(first(), later(), wider);
    assert_eq!(decided.method, Method::TagsDuration);
}

#[test]
fn policy_2_folds_accents_and_takes_discs_from_titles_and_folders() {
    // Each choice on its own is tested above as a variant of policy 1;
    // here policy 2 makes all three.
    let (world, first, plain, decided) = after_first(
        file("A/1.flac", 1).song("Glass Pier", "Ada Wrén", 200),
        file("B/1.flac", 2).song("Glass Pier", "Ada Wren", 200),
        Policy::v2(),
    );
    assert_eq!(decided.method, Method::TagsDuration);
    assert_eq!(world.recording(first), world.recording(plain));

    let mut world = World::default();
    two_folders(
        &mut world,
        ["Ninefold/Long Field 1", "Ninefold/Long Field 2"],
        ["Long Field (Disc 1)", "Long Field (Disc 2)"],
        false,
    );
    world.settle();
    assert_eq!(
        discs(&world, &Policy::v2()),
        (1, vec![Some(1), Some(1), Some(2), Some(2)])
    );
    world.assert_settled();

    let mut world = World::default();
    two_folders(
        &mut world,
        ["Ninefold/Long Field/CD1", "Ninefold/Long Field/CD2"],
        ["Long Field"; 2],
        false,
    );
    world.settle();
    assert_eq!(
        discs(&world, &Policy::v2()),
        (1, vec![Some(1), Some(1), Some(2), Some(2)])
    );
    world.assert_settled();
}

/// For each item a policy would decide otherwise than its assignment: the
/// item, whether its recording would change, and the method and review
/// mark of the new decision.
fn differences(world: &World, policy: &Policy) -> Vec<(i64, bool, Method, bool)> {
    catalog::resolve(&world.library, policy)
        .items
        .iter()
        .zip(&world.library.items)
        .filter(|(out, _)| out.changed)
        .map(|(out, item)| {
            let decided = out.decision.as_ref().unwrap();
            let now = item.current.map(|current| Ref::Row(current.recording));
            (
                out.item,
                out.recording != now,
                decided.method,
                decided.needs_review,
            )
        })
        .collect()
}

#[test]
fn from_policy_1_to_policy_2_exactly_the_four_choices_change_a_library() {
    let mut world = World::under(Policy::v1());
    // An artist spelled with an accent and without.
    world.add(file("A/1.flac", 1).song("Glass Pier", "Ada Wrén", 200));
    let unaccented = world.add(file("B/1.flac", 2).song("Glass Pier", "Ada Wren", 200));
    // A song on an album and on a compilation.
    let album = world.add(on_album());
    let compilation = world.add(on_compilation());
    // An album split by a disc marker in its title.
    let titled = two_folders(
        &mut world,
        ["Ninefold/Long Field 1", "Ninefold/Long Field 2"],
        ["Long Field (Disc 1)", "Long Field (Disc 2)"],
        false,
    );
    // An album split by disc folders.
    let foldered = two_folders(
        &mut world,
        ["Moss Arcade/Tin Weather/CD1", "Moss Arcade/Tin Weather/CD2"],
        ["Tin Weather"; 2],
        false,
    );
    // Audio of their own.
    for item in &mut world.library.items {
        item.audio = Some(100 + item.id);
    }
    // By another artist, so that no song of one album matches the other's.
    for item in &mut world.library.items[8..] {
        item.facts.artist = vec!["Moss Arcade".to_owned()];
        item.facts.album_artist = vec!["Moss Arcade".to_owned()];
    }
    // Two lengths three seconds apart on one album, left for review by
    // both policies.
    world.add(
        file("Ninefold/Low Tide/1.flac", 200)
            .song("Salt Road", "Ninefold", 200)
            .on("Low Tide", None, 1),
    );
    world.add(
        file("Backup/Low Tide/1.flac", 201)
            .song("Salt Road", "Ninefold", 203)
            .on("Low Tide", None, 1),
    );
    let resolution = world.settle();
    assert_eq!(
        decision(&resolution, compilation).method,
        Method::TagsDuration
    );
    assert_eq!(world.recording(album), world.recording(compilation));
    assert!(differences(&world, &Policy::v1()).is_empty());

    // The accented spelling binds, the compilation's song parts from the
    // album's for review, and the second disc of each split album moves
    // onto the first disc's release. Nothing else.
    let mut expected = vec![
        (unaccented, true, Method::TagsDuration, false),
        (compilation, true, Method::New, true),
    ];
    expected.extend(
        titled[2..]
            .iter()
            .map(|id| (*id, false, Method::New, false)),
    );
    expected.extend(
        foldered[2..]
            .iter()
            .map(|id| (*id, false, Method::New, false)),
    );
    assert_eq!(differences(&world, &Policy::v2()), expected);
    let before = catalog::resolve(&world.library, &Policy::v1())
        .releases
        .len();
    let after = catalog::resolve(&world.library, &Policy::v2())
        .releases
        .len();
    assert_eq!((before, after), (8, 6));

    // Settled by policy 2, it asks for nothing more.
    world.policy = Policy::v2();
    world.settle();
    world.assert_settled();
    assert!(differences(&world, &Policy::v2()).is_empty());
}

/// A library of a few files drawn from a small vocabulary, so that titles,
/// artists, albums, audio and identifiers collide often.
fn files() -> impl Strategy<Value = Vec<File>> {
    let one = (
        0..4_usize,
        0..4_usize,
        0..3_usize,
        0..4_usize,
        0..3_i64,
        0..12_i64,
        proptest::option::of(0..3_usize),
        (
            0..4_usize,
            0..3_usize,
            proptest::bool::ANY,
            proptest::option::of(1..3_i64),
            1..4_i64,
            proptest::option::of(0..2_usize),
        ),
    )
        .prop_map(
            |(
                folder,
                title,
                artist,
                album,
                length,
                audio,
                mbid,
                (note, aa, tagged, disc, track, group),
            )| {
                let folders = ["A/One", "A/Two", "B/One/CD1", "B/One/CD2"];
                let titles = ["Glass Pier", "Salt Road", "Third Rail", "Coal Light"];
                let notes = ["", " (Live)", " (Remastered)", " [Demo]"];
                let artists = ["Ninefold", "Ada Wren", "ADA WREN"];
                let albums = [
                    None,
                    Some("Long Field"),
                    Some("Long Field (Deluxe)"),
                    Some("Night Drives"),
                ];
                let mut made = file(folders[folder], audio);
                if tagged {
                    made = made.song(
                        &format!("{}{}", titles[title], notes[note]),
                        artists[artist],
                        200 + 3 * length,
                    );
                } else {
                    made.facts.title_inferred = Some(titles[title].to_owned());
                    made.facts.duration_ms = Some(200_000);
                }
                if let Some(album) = albums[album] {
                    made = made.on(album, disc, track);
                }
                if aa > 0 {
                    made.facts.album_artist = vec![artists[aa - 1].to_owned()];
                }
                made.facts.recording_mbid = mbid.map(|n| format!("rec-{n}"));
                made.facts.release_group_mbid = group.map(|n| format!("grp-{n}"));
                made
            },
        );
    proptest::collection::vec(one, 1..14)
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    /// Resolving twice gives the same result: a library that was resolved
    /// and written asks for nothing when it is resolved again, and adding
    /// files never changes that.
    #[test]
    fn resolving_twice_gives_the_same_result(first in files(), second in files()) {
        let mut world = World::default();
        for file in first {
            world.add(file);
        }
        for policy in [Policy::v1(), Policy::v2()] {
            prop_assert_eq!(
                catalog::resolve(&world.library, &policy),
                catalog::resolve(&world.library, &policy)
            );
        }
        world.settle();
        world.assert_settled();
        for file in second {
            world.add(file);
        }
        world.settle();
        world.assert_settled();
    }

    /// Under any of the switches and thresholds the dry run can ask about,
    /// a library settled by that policy asks for nothing when it is
    /// resolved again.
    #[test]
    fn resolving_twice_under_any_variant_gives_the_same_result(
        files in files(),
        switches in proptest::array::uniform4(proptest::bool::ANY),
        grace in 0..8_i64,
        within in proptest::sample::select(vec![0_i64, 20, 3_000]),
        more in proptest::array::uniform2(proptest::bool::ANY),
    ) {
        let mut world = World::under(Policy {
            fold_accents: switches[0],
            discs_from_folders: switches[1],
            disc_from_album_title: switches[2],
            tags_bind_across_albums: switches[3],
            across_albums_within_ms: within,
            tracks_of_one_release_differ: more[0],
            marker_list: if more[1] { 2 } else { 1 },
            duration_grace_ms: grace * 1000,
            ..Policy::v1()
        });
        for file in files {
            world.add(file);
        }
        world.settle();
        world.assert_settled();
    }

    /// A library settled by policy 1 and then resolved by policy 2, as the
    /// first scan after the version changes does, asks for nothing when it
    /// is resolved by policy 2 again, and adding files never changes that.
    #[test]
    fn a_library_settled_by_policy_1_settles_by_policy_2(first in files(), second in files()) {
        let mut world = World::under(Policy::v1());
        for file in first {
            world.add(file);
        }
        world.settle();
        world.policy = Policy::v2();
        world.settle();
        world.assert_settled();
        for file in second {
            world.add(file);
        }
        world.settle();
        world.assert_settled();
    }

    /// Scanning in two parts decides what scanning at once decides: the
    /// same files share a recording, a release and a group either way.
    #[test]
    fn a_library_scanned_in_two_parts_is_the_one_scanned_at_once(all in files(), cut in 0..14_usize) {
        let shape = |world: &World| -> Vec<(Vec<bool>, Vec<bool>)> {
            let items = &world.library.items;
            items
                .iter()
                .map(|item| {
                    let same = |of: &dyn Fn(i64) -> Option<i64>| -> Vec<bool> {
                        items.iter().map(|other| of(other.id) == of(item.id)).collect()
                    };
                    (same(&|id| Some(world.recording(id))), same(&|id| world.release(id)))
                })
                .collect()
        };
        let mut at_once = World::default();
        let mut in_parts = World::default();
        for (index, file) in all.into_iter().enumerate() {
            at_once.add(file.clone());
            if index == cut {
                in_parts.settle();
            }
            in_parts.add(file);
        }
        at_once.settle();
        in_parts.settle();
        prop_assert_eq!(shape(&at_once), shape(&in_parts));
    }
}
