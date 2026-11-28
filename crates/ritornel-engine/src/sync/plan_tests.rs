//! The planner, against what the design says it emits: wanted against
//! seen against recorded.

use std::collections::HashSet;

use ritornel_contract::{
    SyncActionKind, SyncBasis, SyncFolderKind, SyncMadeFor, SyncSelector, SyncSourceRole, SyncUnit,
};
use ritornel_device::{Capabilities, ObjectId, ObjectInfo, Plays, RelPath, Storage, Transport};

use super::plan::{Baseline, Encodes, Plan, Seen, Shown, Speed, plan};
use super::policy::{FormatPolicy, Settings};
use super::select::{CopyRow, Matcher, SourceRow, TrackRow};
use crate::Id;
use crate::transcode::{Codec, Mp3, Profile, Target};

const MB: u64 = 1_000_000;

fn id(n: u8) -> Id {
    Id::from_bytes([n; 16])
}

fn copy(n: i64, codec: &str, size: u64) -> CopyRow {
    CopyRow {
        source_item: n,
        source_item_pid: id(n as u8),
        revision_no: 1,
        revision: n,
        asset: n,
        codec: codec.to_owned(),
        lossless: codec == "flac",
        sample_rate_hz: 44_100,
        bit_depth: (codec == "flac").then_some(16),
        bitrate_kbps: (codec != "flac").then_some(256),
        missing: false,
        root: "music".to_owned(),
        rel_path: format!("{n}.{codec}"),
        size_bytes: size,
        mtime_ms: 1,
        hash_scheme: 1,
        audio_hash: [n as u8; 32],
    }
}

/// Track `n` of album `album` by `artist`, a 30 MB FLAC four minutes long.
fn track(n: i64, artist: &str, album: i64, title: &str) -> TrackRow {
    TrackRow {
        appearance: n,
        appearance_pid: id(n as u8),
        title: title.to_owned(),
        artist: artist.to_owned(),
        album: format!("Album {album}"),
        album_artist: artist.to_owned(),
        year: Some(2020),
        disc: Some(1),
        track: u32::try_from(n).ok(),
        duration_ms: 240_000,
        added_at_ms: Some(1_000),
        release: Some(album),
        release_pid: Some(id(200 + album as u8)),
        art: None,
        copies: vec![copy(n, "flac", 30 * MB)],
        discs: false,
        album_tracks: 2,
    }
}

/// Two albums of two tracks each by Ada Vale, and one by Corin Marsh.
fn library() -> Vec<TrackRow> {
    vec![
        track(1, "Ada Vale", 1, "Shoreline"),
        track(2, "Ada Vale", 1, "Lamp Oil"),
        track(3, "Ada Vale", 2, "Tidewater"),
        track(4, "Ada Vale", 2, "Salt Hours"),
        track(5, "Corin Marsh", 3, "Glass Orchard"),
        track(6, "Corin Marsh", 3, "Night Ferry"),
    ]
}

fn source(n: u8, role: SyncSourceRole, matcher: Matcher) -> (SourceRow, Matcher) {
    (
        SourceRow {
            id: id(100 + n),
            role,
            selector: SyncSelector::Everything,
            unit: SyncUnit::Albums,
            label: String::new(),
            detail: None,
            art: None,
        },
        matcher,
    )
}

fn everything() -> Vec<(SourceRow, Matcher)> {
    vec![source(1, SyncSourceRole::Rule, Matcher::Everything)]
}

fn albums(releases: &[i64]) -> Matcher {
    Matcher::Records {
        releases: releases.iter().copied().collect(),
        loose: HashSet::new(),
    }
}

fn capabilities() -> Capabilities {
    Capabilities {
        transport: Transport::Folder,
        plays: Plays::None,
        ejectable: false,
        can_sample: true,
        codecs: vec!["flac", "mp3", "opus"],
        cover_sizes: vec![500],
        path_bytes: 250,
        write_bps: 10 * MB,
    }
}

/// Every encoder is there, and nothing is cached.
struct Encoders;

impl Encodes for Encoders {
    fn has(&self, _: Codec) -> bool {
        true
    }
    fn key(&self, copy: &CopyRow, _: Profile) -> Option<blake3::Hash> {
        Some(blake3::hash(&copy.audio_hash))
    }
    fn size(&self, _: blake3::Hash) -> (Option<u64>, bool) {
        (None, false)
    }
}

fn seen(objects: Vec<ObjectInfo>, capacity: u64, free: u64) -> Seen {
    Seen {
        objects,
        storage: Storage {
            capacity_bytes: capacity,
            free_bytes: free,
            cluster_bytes: None,
        },
        basis: SyncBasis::Live,
    }
}

fn settings() -> Settings {
    Settings {
        // No reserve, so the arithmetic below is the capacity's own.
        reserve_bytes: Some(0),
        cover_px: 0,
        ..Settings::default()
    }
}

fn run(
    tracks: &[TrackRow],
    sources: &[(SourceRow, Matcher)],
    baselines: &[Baseline],
    seen: &Seen,
    settings: &Settings,
) -> Plan {
    plan(
        tracks,
        sources,
        baselines,
        seen,
        settings,
        &RelPath::new("Music").unwrap(),
        &capabilities(),
        &Encoders,
        Speed {
            write_bps: 10 * MB,
            workers: 4,
        },
    )
}

fn object(path: &RelPath, size: u64) -> ObjectInfo {
    ObjectInfo {
        id: ObjectId::Path(path.clone()),
        path: path.clone(),
        size,
        modified_ms: None,
    }
}

/// What carrying a plan out leaves: a baseline and a file for each add and
/// replace, and none for a remove.
fn carried(plan: &Plan, baselines: &[Baseline], seen: &Seen) -> (Vec<Baseline>, Seen) {
    let mut baselines = baselines.to_vec();
    let mut objects = seen.objects.clone();
    for (at, action) in plan.actions.iter().enumerate() {
        if !action.runs() {
            continue;
        }
        if let Some(old) = &action.old {
            baselines.retain(|baseline| baseline.id != old.baseline);
            objects.retain(|object| object.path != old.path);
        }
        if let (Some(make), Some(path), Some(appearance)) =
            (&action.make, &action.path, action.appearance)
        {
            baselines.push(Baseline {
                id: 1000 + at as i64 + baselines.len() as i64 * 100,
                appearance,
                path: path.clone(),
                key: make.key,
                codec: make.sent.codec().extension().to_owned(),
                size: action.bytes,
                sampled: [7; 32],
                shown: Shown::default(),
            });
            objects.push(object(path, action.bytes));
        }
    }
    (
        baselines,
        Seen {
            objects,
            ..seen.clone()
        },
    )
}

fn kinds(plan: &Plan) -> Vec<(SyncActionKind, &'static str)> {
    plan.actions
        .iter()
        .map(|action| (action.kind, action.reason))
        .collect()
}

#[test]
fn a_first_sync_adds_everything_and_the_second_does_nothing() {
    let (tracks, sources) = (library(), everything());
    let empty = seen(Vec::new(), 1000 * MB, 1000 * MB);
    let first = run(&tracks, &sources, &[], &empty, &settings());
    assert_eq!(first.counts.add.tracks, 6);
    assert_eq!(first.counts.add.bytes, 180 * MB);
    assert_eq!(first.figures.to_copy_bytes, 180 * MB);
    assert_eq!(first.figures.free_after_bytes, 820 * MB);
    assert!(first.fits && !first.figures.exact);
    assert_eq!(
        first.actions[0].path.as_ref().unwrap().as_str(),
        "Music/Ada Vale/Album 1/01 Shoreline.flac"
    );
    // Carried out, the whole second run is nothing, and its sizes are known.
    let (baselines, after) = carried(&first, &[], &empty);
    let second = run(&tracks, &sources, &baselines, &after, &settings());
    assert!(second.actions.is_empty(), "{:?}", kinds(&second));
    assert_eq!((second.tracks_after, second.albums_after), (6, 3));
    assert_eq!(second.room.managed_bytes, 180 * MB);
    assert_ne!(first.revision, second.revision);
    // The same inputs give the same plan.
    assert_eq!(
        second.revision,
        run(&tracks, &sources, &baselines, &after, &settings()).revision
    );
}

#[test]
fn what_changed_is_sent_again_and_says_why() {
    let (mut tracks, sources) = (library(), everything());
    let empty = seen(Vec::new(), 1000 * MB, 1000 * MB);
    let first = run(&tracks, &sources, &[], &empty, &settings());
    let (baselines, after) = carried(&first, &[], &empty);

    // A corrected title: the file is sent again as a retag, at its own
    // path, which is stored and never worked out again.
    tracks[0].title = "Shore Line".to_owned();
    let retag = run(&tracks, &sources, &baselines, &after, &settings());
    assert_eq!(kinds(&retag), [(SyncActionKind::Replace, "tags")]);
    assert!(retag.actions[0].retag);
    assert_eq!(
        retag.actions[0].path.as_ref().unwrap().as_str(),
        "Music/Ada Vale/Album 1/01 Shoreline.flac"
    );
    assert_eq!(retag.counts.retag.tracks, 1);
    assert_eq!(retag.figures_skipping_retags.to_copy_bytes, 0);
    tracks[0].title = "Shoreline".to_owned();

    // Another format: every file is replaced, at a path of its own.
    let mp3 = Settings {
        format: FormatPolicy::encode(Target::Mp3 { mode: Mp3::V0 }),
        ..settings()
    };
    let format = run(&tracks, &sources, &baselines, &after, &mp3);
    assert!(
        kinds(&format)
            .iter()
            .all(|kind| *kind == (SyncActionKind::Replace, "format"))
    );
    assert_eq!(format.actions.len(), 6);
    assert!(format.actions.iter().all(|action| !action.retag));
    let replaced = &format.actions[0];
    assert!(replaced.path.as_ref().unwrap().as_str().ends_with(".mp3"));
    assert!(
        replaced
            .old
            .as_ref()
            .unwrap()
            .path
            .as_str()
            .ends_with(".flac")
    );
    // A smaller format shrinks, and what it frees is counted.
    assert!(format.figures.free_after_bytes > after.storage.free_bytes);
    assert_eq!(format.encodes_needed, 6);

    // A cover that arrived: the cover's part of the key, and nothing else.
    let covered = Settings {
        cover_px: 500,
        ..settings()
    };
    let mut with_art = tracks.clone();
    with_art[0].art = Some(super::Art {
        id: id(9),
        hash: [9; 32],
    });
    let cover = run(&with_art, &sources, &baselines, &after, &covered);
    assert_eq!(kinds(&cover), [(SyncActionKind::Replace, "cover")]);
}

#[test]
fn what_is_no_longer_wanted_is_removed_and_what_went_missing_is_sent_again() {
    let (tracks, sources) = (library(), everything());
    let empty = seen(Vec::new(), 1000 * MB, 1000 * MB);
    let first = run(&tracks, &sources, &[], &empty, &settings());
    let (baselines, mut after) = carried(&first, &[], &empty);

    // Corin Marsh is left out: their files go, removes first.
    let without = vec![
        source(1, SyncSourceRole::Rule, Matcher::Everything),
        source(2, SyncSourceRole::LeaveOut, albums(&[3])),
    ];
    let removal = run(&tracks, &without, &baselines, &after, &settings());
    assert_eq!(
        kinds(&removal),
        [
            (SyncActionKind::Remove, "not_wanted"),
            (SyncActionKind::Remove, "not_wanted")
        ]
    );
    assert_eq!(removal.counts.remove.bytes, 60 * MB);
    assert_eq!(
        removal.figures.free_after_bytes,
        after.storage.free_bytes + 60 * MB
    );
    assert!(
        removal
            .actions
            .iter()
            .all(|action| !action.masters.is_empty())
    );
    assert_eq!(removal.sources[1].claimed.tracks, 2);

    // A managed file that went missing is sent again, at its own path.
    let lost = baselines[0].path.clone();
    after.objects.retain(|object| object.path != lost);
    let again = run(&tracks, &sources, &baselines, &after, &settings());
    assert_eq!(kinds(&again), [(SyncActionKind::Add, "missing")]);
    assert_eq!(again.actions[0].path.as_ref(), Some(&lost));
    // Missing and no longer wanted: nothing to do on the device, and the
    // record is closed by the next run.
    let dropped = run(&tracks, &[], &baselines, &after, &settings());
    assert_eq!(dropped.gone, [baselines[0].id]);
    assert_eq!(dropped.counts.remove.tracks, 5);
}

#[test]
fn a_file_ritornel_did_not_write_or_that_changed_is_never_touched() {
    let (mut tracks, sources) = (library(), everything());
    let empty = seen(Vec::new(), 1000 * MB, 1000 * MB);
    let first = run(&tracks, &sources, &[], &empty, &settings());
    let (baselines, mut after) = carried(&first, &[], &empty);

    // Changed on the device: a conflict, wanted or not, and it stays.
    let edited = baselines[0].path.clone();
    for object in &mut after.objects {
        if object.path == edited {
            object.size += 5;
        }
    }
    let conflict = run(&tracks, &sources, &baselines, &after, &settings());
    assert_eq!(
        kinds(&conflict),
        [(SyncActionKind::Conflict, "changed_on_device")]
    );
    assert!(conflict.actions[0].old.is_none() && !conflict.actions[0].runs());
    let unwanted = run(&tracks, &[], &baselines, &after, &settings());
    assert_eq!(unwanted.counts.conflict.tracks, 1);
    assert_eq!(unwanted.counts.remove.tracks, 5);

    // A file there with no record is not Ritornel's: counted as used, and
    // a track planned at its name is blocked rather than written over it.
    let podcast = RelPath::new("Music/podcast.mp3").unwrap();
    let squatter = RelPath::new("Music/Ada Vale/Album 1/01 Shoreline.flac").unwrap();
    let foreign = seen(
        vec![object(&podcast, 9 * MB), object(&squatter, 3 * MB)],
        1000 * MB,
        988 * MB,
    );
    let beside = run(&tracks, &sources, &[], &foreign, &settings());
    assert_eq!(beside.room.unmanaged_bytes, 12 * MB);
    assert_eq!(beside.unmanaged.len(), 2);
    assert_eq!(beside.counts.add.tracks, 5);
    assert_eq!(
        kinds(&beside).last(),
        Some(&(SyncActionKind::Blocked, "path_taken"))
    );

    // A master that is gone: its file on the device is kept, since the
    // library could not make it again. An emptied library empties nothing.
    let kept = run(
        &[],
        &sources,
        &baselines,
        &carried(&first, &[], &empty).1,
        &settings(),
    );
    assert!(
        kinds(&kept)
            .iter()
            .all(|kind| *kind == (SyncActionKind::Conflict, "master_gone"))
    );
    assert_eq!(kept.actions.len(), 6);
    // A track whose only copy the target cannot play is blocked, and says
    // why.
    tracks[5].copies = vec![copy(6, "aac", 8 * MB)];
    let blocked = run(&tracks, &sources, &[], &empty, &settings());
    assert_eq!(
        kinds(&blocked).last(),
        Some(&(SyncActionKind::Blocked, "no_format"))
    );
    assert_eq!(blocked.counts.add.tracks, 5);
}

#[test]
fn sources_claim_in_order_and_the_cut_is_a_strict_prefix_of_whole_albums() {
    let tracks = library();
    // Always claims first, whatever its place; then the rules in order.
    let sources = vec![
        source(1, SyncSourceRole::Rule, albums(&[1, 2])),
        source(2, SyncSourceRole::Rule, Matcher::Everything),
        source(3, SyncSourceRole::Always, albums(&[3])),
    ];
    // Room for four tracks of the six.
    let tight = seen(Vec::new(), 130 * MB, 130 * MB);
    let cut = run(&tracks, &sources, &[], &tight, &settings());
    assert!(!cut.fits);
    assert_eq!(cut.cut.tracks, 2);
    assert_eq!(cut.room.over_bytes, 50 * MB);
    // Always took album 3, then the first rule took album 1 and was cut
    // short of album 2, and the second rule had nothing left to claim.
    let added: Vec<i64> = cut.actions.iter().filter_map(|a| a.appearance).collect();
    assert_eq!(added, [5, 6, 1, 2]);
    assert_eq!(cut.sources[2].claimed.tracks, 2);
    assert_eq!(
        (cut.sources[0].claimed.tracks, cut.sources[0].cut.tracks),
        (2, 2)
    );
    assert_eq!(cut.sources[1].claimed.tracks, 0);
    assert_eq!(cut.by_source.len(), 2);

    // A user's limit is the capacity, whatever the volume has.
    let limited = Settings {
        limit_bytes: Some(70 * MB),
        folder: Some((SyncMadeFor::PlainCopy, SyncFolderKind::AlwaysHere)),
        reserve_bytes: None,
        ..settings()
    };
    let roomy = seen(Vec::new(), 100_000 * MB, 90_000 * MB);
    let within = run(&tracks, &everything(), &[], &roomy, &limited);
    assert_eq!(within.room.capacity_bytes, 70 * MB);
    assert_eq!(within.room.reserve_bytes, 0);
    assert_eq!(within.counts.add.tracks, 2);
    // A removable drive has its own room, and no limit.
    let removable = Settings {
        folder: Some((SyncMadeFor::PlainCopy, SyncFolderKind::Removable)),
        ..limited
    };
    assert!(run(&tracks, &everything(), &[], &roomy, &removable).fits);

    // What the cut leaves off is removed where it was on the device.
    let all = run(
        &tracks,
        &everything(),
        &[],
        &seen(Vec::new(), 1000 * MB, 1000 * MB),
        &settings(),
    );
    let (baselines, after) = carried(&all, &[], &seen(Vec::new(), 190 * MB, 10 * MB));
    let shrunk = Settings {
        limit_bytes: Some(130 * MB),
        folder: Some((SyncMadeFor::PlainCopy, SyncFolderKind::AlwaysHere)),
        ..settings()
    };
    let trimmed = run(&tracks, &everything(), &baselines, &after, &shrunk);
    assert_eq!(trimmed.counts.remove.tracks, 2);
    assert_eq!(trimmed.counts.add.tracks, 0);
}

#[test]
fn paths_that_differ_only_by_case_are_told_apart_and_lossy_masters_go_as_they_are() {
    let mut tracks = vec![
        track(1, "Ada Vale", 1, "Shoreline"),
        track(2, "ADA VALE", 1, "SHORELINE"),
        track(3, "Corin Marsh", 3, "Night Ferry"),
    ];
    tracks[1].track = Some(1);
    tracks[1].album = "ALBUM 1".to_owned();
    tracks[2].copies = vec![copy(3, "mp3", 8 * MB)];
    let empty = seen(Vec::new(), 1000 * MB, 1000 * MB);
    let mp3 = Settings {
        format: FormatPolicy::encode(Target::Mp3 { mode: Mp3::V0 }),
        ..settings()
    };
    let made = run(&tracks, &everything(), &[], &empty, &mp3);
    let paths: Vec<&str> = made
        .actions
        .iter()
        .map(|action| action.path.as_ref().unwrap().as_str())
        .collect();
    assert_eq!(paths.len(), 3);
    let folded: HashSet<String> = paths.iter().map(|path| path.to_lowercase()).collect();
    assert_eq!(folded.len(), 3, "{paths:?}");
    // Lossy is never encoded to lossy: the MP3 master is sent as it is,
    // at its own size, and needs no encode.
    let lossy = made
        .actions
        .iter()
        .find(|a| a.appearance == Some(3))
        .unwrap();
    assert_eq!(lossy.how(), Some(ritornel_contract::SyncHow::AsIs));
    assert_eq!(lossy.bytes, 8 * MB);
    assert_eq!(made.encodes_needed, 2);
    // An encode is estimated from its nominal bitrate until it is made.
    let encoded = made
        .actions
        .iter()
        .find(|a| a.appearance == Some(1))
        .unwrap();
    assert!(encoded.estimated);
    assert!(
        (7 * MB..8 * MB).contains(&encoded.bytes),
        "{}",
        encoded.bytes
    );
}
