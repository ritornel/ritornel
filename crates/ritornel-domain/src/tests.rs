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
    Current, ExistingCredit, ExistingRelease, Item, Kind, Library, Method, Ref, Resolution,
};
use crate::resolve::{Policy, Replacement, judge_replacements};
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
}

impl World {
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
        let resolution = catalog::resolve(&self.library, &Policy::v1());
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
    let mut world = World::default();
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
}

#[test]
fn a_song_on_an_album_and_on_a_compilation_is_one_recording_with_two_appearances() {
    let mut world = World::default();
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
    let discs = |numbered: bool| {
        let mut world = World::default();
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
    assert_eq!(discs(true), (1, 1, 4));
    // With no disc numbers the two folders claim the same tracks, so they
    // are two releases. They share no recording, so two groups as well.
    assert_eq!(discs(false), (2, 2, 4));
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
                (note, aa, tagged, disc, track),
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
        prop_assert_eq!(
            catalog::resolve(&world.library, &Policy::v1()),
            catalog::resolve(&world.library, &Policy::v1())
        );
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
