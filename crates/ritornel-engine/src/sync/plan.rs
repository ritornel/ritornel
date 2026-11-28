//! The planner: a pure function of what is wanted, what was seen on the
//! target and what the hub recorded it wrote there. See "The plan" in
//! docs/internals/device-sync.md.
//!
//! The hub trusts nothing a device reports beyond what is there now. What
//! identifies a file on a target is the hub's own record of having written
//! it, the baseline, and never a tag. So a file with no baseline is not
//! Ritornel's, whatever it holds, and is left alone and counted as used.
//!
//! It opens no file and writes nothing: the same inputs give the same plan,
//! and a second run of a plan that was carried out is empty.

use std::collections::{HashMap, HashSet};

use ritornel_contract::{
    SyncActionKind, SyncAmount, SyncBasis, SyncCounts, SyncFigures, SyncHow, SyncPlanSource,
    SyncRoom, SyncSourceRole, SyncUnit,
};
use ritornel_device::{Capabilities, ObjectInfo, RelPath, Storage};

use super::naming::{self, Named};
use super::policy::{self, Blocked, Sent, Settings};
use super::select::{Art, CopyRow, Matcher, SourceRow, TrackRow};
use crate::Id;
use crate::transcode::{Codec, Profile, Tags};

/// What a file on a target was made from, as four parts: the master's
/// audio, the format, the tags and the cover. A plan compares it with the
/// one a file was written under: the same key is nothing to do, and the
/// first part that differs is why a file is sent again. It leaves out the
/// encoder's build and Ritornel's own identifier, so a newer encoder or a
/// regrouped record never sends a target's files again by itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProjectionKey(pub [u8; 32]);

/// The version of the tag writer, the cover's JPEG settings and the
/// projection together. Raising it sends every file again as a retag.
const TAGGING_VERSION: u32 = 1;

impl ProjectionKey {
    fn part(of: &impl serde::Serialize) -> [u8; 8] {
        let bytes = serde_json::to_vec(of).unwrap_or_default();
        let mut part = [0; 8];
        part.copy_from_slice(blake3::hash(&bytes).as_bytes().get(..8).unwrap_or(&[0; 8]));
        part
    }

    pub(crate) fn of(
        copy: &CopyRow,
        sent: Sent,
        tags: &Tags,
        cover: Option<(Art, u32)>,
        id3v1: bool,
    ) -> Self {
        let audio = Self::part(&(
            copy.hash_scheme,
            blake3::Hash::from(copy.audio_hash).to_hex().as_str(),
        ));
        let format = match sent {
            Sent::AsIs(codec) => Self::part(&("as_is", codec)),
            Sent::Encode(codec, profile) => Self::part(&("encode", codec, profile)),
        };
        let tags = Self::part(&(TAGGING_VERSION, tags, id3v1));
        let cover = Self::part(
            &cover.map(|(art, px)| (blake3::Hash::from(art.hash).to_hex().to_string(), px)),
        );
        let mut key = [0; 32];
        for (slot, part) in key
            .as_chunks_mut::<8>()
            .0
            .iter_mut()
            .zip([audio, format, tags, cover])
        {
            *slot = part;
        }
        Self(key)
    }

    /// Why a file written under `self` is sent again to become `wanted`:
    /// the first part that differs, format first.
    fn differs(&self, wanted: &Self) -> Option<&'static str> {
        let part = |key: &Self, at: usize| key.0.get(at * 8..at * 8 + 8).map(<[u8]>::to_vec);
        [(1, "format"), (0, "audio"), (2, "tags"), (3, "cover")]
            .into_iter()
            .find(|(at, _)| part(self, *at) != part(wanted, *at))
            .map(|(_, reason)| reason)
    }
}

/// What a row shows of its track, as it was when the plan was made.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Shown {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_artist: String,
    pub art: Option<Id>,
    /// The album it is listed under.
    pub group_key: String,
    pub album_tracks: u32,
}

impl Shown {
    fn of(track: &TrackRow) -> Self {
        Self {
            title: track.title.clone(),
            artist: track.artist.clone(),
            album: track.album.clone(),
            album_artist: track.album_artist.clone(),
            art: track.art.map(|art| art.id),
            group_key: track.group_key(),
            album_tracks: track.album_tracks,
        }
    }
}

/// The hub's record of one file it wrote on a target and verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Baseline {
    pub id: i64,
    pub appearance: i64,
    pub path: RelPath,
    pub key: ProjectionKey,
    pub codec: String,
    pub size: u64,
    pub sampled: [u8; 32],
    pub shown: Shown,
}

/// What was read of a target, or what was last recorded of one that is
/// away.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seen {
    /// Every file under the music root.
    pub objects: Vec<ObjectInfo>,
    pub storage: Storage,
    pub basis: SyncBasis,
}

/// Everything a transfer needs to make one file, frozen with the plan.
#[derive(Debug, Clone)]
pub struct Make {
    /// The track appearance the file is of.
    pub appearance: Id,
    pub copy: CopyRow,
    pub sent: Sent,
    pub tags: Tags,
    pub cover: Option<(Art, u32)>,
    pub id3v1: bool,
    pub key: ProjectionKey,
    /// The key of its encode, where it has one.
    pub encode: Option<blake3::Hash>,
    pub encode_cached: bool,
    pub duration_ms: u64,
}

/// The file an action may destroy: the baseline, and the signature the
/// device's file must still have.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Old {
    pub baseline: i64,
    pub path: RelPath,
    pub size: u64,
    pub sampled: [u8; 32],
}

/// One thing a plan would do, or a row saying why it does nothing.
#[derive(Debug, Clone)]
pub struct Action {
    pub kind: SyncActionKind,
    pub reason: &'static str,
    pub source: Option<Id>,
    pub appearance: Option<i64>,
    pub shown: Shown,
    /// Where the file is, or goes.
    pub path: Option<RelPath>,
    pub old: Option<Old>,
    pub make: Option<Make>,
    /// What it copies, or for a removal what it frees.
    pub bytes: u64,
    pub estimated: bool,
    /// A replace whose only reason is tags or cover.
    pub retag: bool,
    /// For a removal, where the track's masters are, by root and path
    /// under it: one of them is looked at just before the file is removed,
    /// since nothing is destroyed that the library cannot make again.
    pub masters: Vec<(String, String)>,
}

impl Action {
    pub fn how(&self) -> Option<SyncHow> {
        self.make.as_ref().map(|make| match make.sent {
            Sent::AsIs(_) => SyncHow::AsIs,
            Sent::Encode(..) => SyncHow::Encode,
        })
    }

    pub fn codec(&self) -> Option<&'static str> {
        self.make.as_ref().map(|make| make.sent.codec().extension())
    }

    /// Whether a run carries it out, rather than only showing it.
    pub fn runs(&self) -> bool {
        matches!(
            self.kind,
            SyncActionKind::Add | SyncActionKind::Replace | SyncActionKind::Remove
        )
    }
}

/// What a source comes to in a plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SourceTotals {
    pub albums: u64,
    pub claimed: SyncAmount,
    pub cut: SyncAmount,
}

/// How fast a target is written and the hub encodes, for the estimate.
#[derive(Debug, Clone, Copy)]
pub struct Speed {
    /// Bytes a second the target takes.
    pub write_bps: u64,
    /// How many encodes run at once.
    pub workers: u64,
}

/// What the planner asks of the hub's encoders and their cache. It holds
/// nothing.
pub trait Encodes {
    /// Whether the hub has an encoder that makes `codec`.
    fn has(&self, codec: Codec) -> bool;
    /// The key of the encode of `copy` with `profile`, or none where the
    /// hub has no encoder for it.
    fn key(&self, copy: &CopyRow, profile: Profile) -> Option<blake3::Hash>;
    /// The exact size of that encode where it is cached or remembered, and
    /// whether it is cached now.
    fn size(&self, key: blake3::Hash) -> (Option<u64>, bool);
}

/// The plan of a target's next sync.
#[derive(Debug, Clone)]
pub struct Plan {
    /// Names what the plan would do. It moves only when that does.
    pub revision: String,
    pub basis: SyncBasis,
    /// What a run carries out, in the order it does: removes, replaces
    /// that shrink, replaces that grow, then adds. After them, the rows
    /// that only say something: conflicts and blocked tracks.
    pub actions: Vec<Action>,
    /// Baselines whose files are gone and are no longer wanted: closed by
    /// the next run, with nothing to do on the target.
    pub gone: Vec<i64>,
    pub sources: Vec<SourceTotals>,
    pub by_source: Vec<SyncPlanSource>,
    pub counts: SyncCounts,
    pub figures: SyncFigures,
    pub figures_skipping_retags: SyncFigures,
    pub fits: bool,
    pub cut: SyncAmount,
    pub room: SyncRoom,
    pub encodes_needed: u64,
    pub encodes_cached: u64,
    /// Every file in the music root that is not Ritornel's, by path.
    pub unmanaged: Vec<ObjectInfo>,
    /// The tracks and albums that would be on it after the sync.
    pub tracks_after: u64,
    pub albums_after: u64,
    /// What everything wanted would take, before any cut.
    pub wanted_bytes: u64,
}

/// A claimed track, with how it would be sent.
struct Want<'a> {
    track: &'a TrackRow,
    source: usize,
    sent: Result<(&'a CopyRow, Sent), Blocked>,
    make: Option<Make>,
    /// Its size on the target after the sync.
    bytes: u64,
    estimated: bool,
    cut: bool,
}

/// What a file is rounded up to on the target.
fn on_disk(bytes: u64, cluster: Option<u64>) -> u64 {
    match cluster {
        Some(cluster) if cluster > 1 => bytes.div_ceil(cluster).saturating_mul(cluster),
        _ => bytes,
    }
}

fn amount(amount: &mut SyncAmount, bytes: u64) {
    amount.tracks += 1;
    amount.bytes = amount.bytes.saturating_add(bytes);
}

/// The per-file cost of a write beyond its bytes, in milliseconds.
const FILE_MS: u64 = 15;
/// How many seconds of audio one encoder gets through in a second, as a
/// floor for the estimate before any encode has been timed.
const ENCODE_SPEED: u64 = 40;

fn figures(actions: &[&Action], free_now: u64, speed: Speed) -> SyncFigures {
    let mut copy = 0u64;
    let mut freed = 0u64;
    let mut estimated = 0u64;
    let mut files = 0u64;
    let mut encode_ms = 0u64;
    for action in actions {
        match action.kind {
            SyncActionKind::Add | SyncActionKind::Replace => {
                copy = copy.saturating_add(action.bytes);
                files += 1;
                if action.estimated {
                    estimated = estimated.saturating_add(action.bytes);
                }
                if let Some(old) = &action.old {
                    freed = freed.saturating_add(old.size);
                }
                if let Some(make) = &action.make
                    && make.encode.is_some()
                    && !make.encode_cached
                {
                    encode_ms = encode_ms.saturating_add(make.duration_ms);
                }
            }
            SyncActionKind::Remove => freed = freed.saturating_add(action.bytes),
            SyncActionKind::Conflict | SyncActionKind::Blocked => {}
        }
    }
    let copy_ms = copy.saturating_mul(1000) / speed.write_bps.max(1) + files * FILE_MS;
    let encode_ms = encode_ms / (ENCODE_SPEED * speed.workers.max(1));
    SyncFigures {
        to_copy_bytes: copy,
        free_after_bytes: free_now.saturating_add(freed).saturating_sub(copy),
        // Never less than the time to encode what is not cached.
        estimated_ms: copy_ms.max(encode_ms),
        exact: estimated == 0,
        estimated_permille: estimated
            .saturating_mul(1000)
            .checked_div(copy)
            .map_or(0, |share| u32::try_from(share).unwrap_or(1000)),
    }
}

/// Plans a target's next sync.
///
/// `sources` are in their stored order, each with what its selector
/// selects. Claim order is every Always, then the rules by position, and a
/// Leave out vetoes both; a track belongs to the first source that claims
/// it. The cut is a strict prefix: units are walked in claim order and the
/// first that does not fit ends it. Quality is never lowered to fit.
#[allow(clippy::too_many_arguments)]
pub fn plan(
    tracks: &[TrackRow],
    sources: &[(SourceRow, Matcher)],
    baselines: &[Baseline],
    seen: &Seen,
    settings: &Settings,
    music_root: &RelPath,
    capabilities: &Capabilities,
    encodes: &dyn Encodes,
    speed: Speed,
) -> Plan {
    let cluster = seen.storage.cluster_bytes;

    // What is there, by path.
    let there: HashMap<&RelPath, &ObjectInfo> = seen
        .objects
        .iter()
        .map(|object| (&object.path, object))
        .collect();
    let by_appearance: HashMap<i64, &Baseline> = baselines
        .iter()
        .map(|baseline| (baseline.appearance, baseline))
        .collect();
    let held: HashSet<&RelPath> = baselines.iter().map(|baseline| &baseline.path).collect();

    // 1. Claims, in claim order.
    let vetoes: Vec<&Matcher> = sources
        .iter()
        .filter(|(source, _)| source.role == SyncSourceRole::LeaveOut)
        .map(|(_, matcher)| matcher)
        .collect();
    let order: Vec<usize> = [SyncSourceRole::Always, SyncSourceRole::Rule]
        .into_iter()
        .flat_map(|role| {
            sources
                .iter()
                .enumerate()
                .filter(move |(_, (source, _))| source.role == role)
                .map(|(at, _)| at)
        })
        .collect();
    let mut totals = vec![SourceTotals::default(); sources.len()];
    let mut owner: Vec<Option<usize>> = vec![None; tracks.len()];
    // Each unit is the tracks one source claims of one album, or one track.
    let mut units: Vec<Vec<usize>> = Vec::new();
    for at in &order {
        let Some((source, matcher)) = sources.get(*at) else {
            continue;
        };
        let mut albums: HashMap<String, usize> = HashMap::new();
        for (index, track) in tracks.iter().enumerate() {
            let free = owner.get(index).is_some_and(Option::is_none);
            if !free || !matcher.takes(track) || vetoes.iter().any(|veto| veto.takes(track)) {
                continue;
            }
            if let Some(slot) = owner.get_mut(index) {
                *slot = Some(*at);
            }
            let unit = match source.unit {
                SyncUnit::Tracks => {
                    units.push(Vec::new());
                    units.len() - 1
                }
                SyncUnit::Albums => *albums.entry(track.group_key()).or_insert_with(|| {
                    units.push(Vec::new());
                    units.len() - 1
                }),
            };
            if let Some(unit) = units.get_mut(unit) {
                unit.push(index);
            }
        }
        if let Some(total) = totals.get_mut(*at) {
            total.albums = albums.len() as u64;
        }
    }
    // What a Leave out vetoes, for its row.
    for (at, (source, matcher)) in sources.iter().enumerate() {
        if source.role != SyncSourceRole::LeaveOut {
            continue;
        }
        let vetoed: Vec<&TrackRow> = tracks.iter().filter(|track| matcher.takes(track)).collect();
        if let Some(total) = totals.get_mut(at) {
            total.claimed.tracks = vetoed.len() as u64;
            total.albums = vetoed
                .iter()
                .map(|track| track.group_key())
                .collect::<HashSet<_>>()
                .len() as u64;
        }
    }

    // 2. How each claimed track would be sent, and what it would take.
    let mut wants: Vec<Want<'_>> = Vec::new();
    let mut want_of: HashMap<usize, usize> = HashMap::new();
    for unit in &units {
        for index in unit {
            let (Some(track), Some(Some(source))) = (tracks.get(*index), owner.get(*index)) else {
                continue;
            };
            let candidates: Vec<_> = track
                .copies
                .iter()
                .map(|copy| (copy.candidate(), copy.codec.as_str()))
                .collect();
            let chosen = policy::choose(
                &candidates,
                settings.format,
                &capabilities.codecs,
                |codec| encodes.has(codec),
            )
            .and_then(|(item, sent)| {
                track
                    .copies
                    .iter()
                    .find(|copy| copy.source_item == item)
                    .map(|copy| (copy, sent))
                    .ok_or(Blocked::NoFormat)
            });
            let mut want = Want {
                track,
                source: *source,
                sent: chosen,
                make: None,
                bytes: 0,
                estimated: false,
                cut: false,
            };
            if let Ok((copy, sent)) = want.sent {
                let cover = track
                    .art
                    .filter(|_| settings.cover_px > 0)
                    .map(|art| (art, settings.cover_px));
                let id3v1 = settings.id3v1 && sent.codec() == Codec::Mp3;
                let tags = Tags {
                    title: Some(track.title.clone()).filter(|text| !text.is_empty()),
                    artist: Some(track.artist.clone()).filter(|text| !text.is_empty()),
                    album: Some(track.album.clone()).filter(|text| !text.is_empty()),
                    album_artist: Some(track.album_artist.clone()).filter(|text| !text.is_empty()),
                    track: track.track,
                    disc: track.disc,
                    year: track.year,
                    ..Tags::default()
                };
                let key = ProjectionKey::of(copy, sent, &tags, cover, id3v1);
                let (encode, hint, cached) = match sent {
                    Sent::Encode(_, profile) => match encodes.key(copy, profile) {
                        Some(key) => {
                            let (size, cached) = encodes.size(key);
                            (Some(key), size, cached)
                        }
                        None => (None, None, false),
                    },
                    Sent::AsIs(_) => (None, None, false),
                };
                // A file's size, best first: its baseline's, where it was
                // written under this key; its encode's, where that is known;
                // the master's own, sent as it is; else a nominal bitrate.
                let kept = by_appearance
                    .get(&track.appearance)
                    .filter(|baseline| baseline.key == key)
                    .map(|baseline| baseline.size);
                let overhead = policy::tag_overhead(settings.cover_px, cover.is_some());
                let (bytes, estimated) = match (kept, sent, hint) {
                    (Some(size), ..) => (size, false),
                    (None, Sent::AsIs(_), _) => (copy.size_bytes, true),
                    (None, Sent::Encode(..), Some(size)) => (size + overhead, true),
                    (None, Sent::Encode(_, profile), None) => (
                        policy::nominal_bytes(profile, track.duration_ms) + overhead,
                        true,
                    ),
                };
                want.bytes = on_disk(bytes, cluster);
                want.estimated = estimated;
                want.make = Some(Make {
                    appearance: track.appearance_pid,
                    copy: copy.clone(),
                    sent,
                    tags,
                    cover,
                    id3v1,
                    key,
                    encode,
                    encode_cached: cached,
                    duration_ms: track.duration_ms,
                });
            }
            want_of.insert(*index, wants.len());
            wants.push(want);
        }
    }

    // 3. Room. Unmanaged bytes are never reclaimable, and neither is a
    // managed file that must stay: one changed on the target, or one whose
    // track is wanted and blocked, or whose master is gone.
    let appearance_index: HashMap<i64, usize> = tracks
        .iter()
        .enumerate()
        .map(|(index, track)| (track.appearance, index))
        .collect();
    // Which claimed track, if any, is of an appearance.
    let wanted_at: HashMap<i64, usize> = wants
        .iter()
        .enumerate()
        .map(|(at, want)| (want.track.appearance, at))
        .collect();
    let master_there = |appearance: i64| {
        appearance_index
            .get(&appearance)
            .and_then(|index| tracks.get(*index))
            .is_some_and(|track| track.copies.iter().any(|copy| !copy.missing))
    };
    let mut managed = 0u64;
    let mut staying = 0u64;
    for baseline in baselines {
        let Some(object) = there.get(&baseline.path) else {
            continue;
        };
        managed = managed.saturating_add(object.size);
        let changed = object.size != baseline.size;
        let blocked = wanted_at
            .get(&baseline.appearance)
            .and_then(|at| wants.get(*at))
            .is_some_and(|want| want.sent.is_err());
        let orphaned = wanted_at
            .get(&baseline.appearance)
            .and_then(|at| wants.get(*at))
            .is_none()
            && !master_there(baseline.appearance);
        if changed || blocked || orphaned {
            staying = staying.saturating_add(object.size);
        }
    }
    let unmanaged: Vec<ObjectInfo> = {
        let mut files: Vec<ObjectInfo> = seen
            .objects
            .iter()
            .filter(|object| !held.contains(&object.path))
            .cloned()
            .collect();
        files.sort_by(|a, b| a.path.cmp(&b.path));
        files
    };
    let unmanaged_bytes: u64 = unmanaged.iter().map(|object| object.size).sum();
    // What the volume could hold of managed content: what is free, and
    // what Ritornel's own files take, less those that must stay.
    let on_volume = seen
        .storage
        .free_bytes
        .saturating_add(managed)
        .saturating_sub(staying);
    // A folder with a limit has that for a capacity: what is not
    // Ritornel's counts against it, and it never has more room than its
    // volume. One that already holds more than its limit has none free.
    let (capacity, free, holds) = match settings.limit() {
        Some(limit) => (
            limit,
            limit
                .saturating_sub(managed)
                .saturating_sub(unmanaged_bytes)
                .min(seen.storage.free_bytes),
            limit
                .saturating_sub(unmanaged_bytes)
                .saturating_sub(staying)
                .min(on_volume),
        ),
        None => (
            seen.storage.capacity_bytes,
            seen.storage.free_bytes,
            on_volume,
        ),
    };
    let reserve = settings.reserve(capacity);
    let room = holds.saturating_sub(reserve);

    // 4. The cut: a strict prefix of the units, in claim order.
    let mut used = 0u64;
    let mut wanted_bytes = 0u64;
    let mut cutting = false;
    let mut cut = SyncAmount::default();
    for unit in &units {
        let members: Vec<usize> = unit
            .iter()
            .filter_map(|index| want_of.get(index).copied())
            .collect();
        let size: u64 = members
            .iter()
            .filter_map(|at| wants.get(*at))
            .filter(|want| want.sent.is_ok())
            .map(|want| want.bytes)
            .sum();
        wanted_bytes = wanted_bytes.saturating_add(size);
        cutting |= used.saturating_add(size) > room;
        if !cutting {
            used = used.saturating_add(size);
        }
        for at in members {
            let Some(want) = wants.get_mut(at) else {
                continue;
            };
            want.cut = cutting && want.sent.is_ok();
            if let Some(total) = totals.get_mut(want.source) {
                if want.cut {
                    amount(&mut total.cut, want.bytes);
                    amount(&mut cut, want.bytes);
                } else if want.sent.is_ok() {
                    amount(&mut total.claimed, want.bytes);
                } else {
                    total.claimed.tracks += 1;
                }
            }
        }
    }

    // 5. Paths. A file keeps the path it was written at; a new one gets
    // the layout's, with a suffix where two would differ only by case.
    let mut taken: HashSet<String> = seen
        .objects
        .iter()
        .map(|object| &object.path)
        .chain(held.iter().copied())
        .map(naming::folded)
        .collect();
    let mut paths: HashMap<i64, Option<RelPath>> = HashMap::new();
    for want in wants.iter().filter(|want| !want.cut) {
        let Some(make) = &want.make else { continue };
        let extension = make.sent.codec().extension();
        let kept = by_appearance
            .get(&want.track.appearance)
            .filter(|baseline| baseline.codec == extension)
            .map(|baseline| baseline.path.clone());
        if let Some(path) = kept {
            paths.insert(want.track.appearance, Some(path));
            continue;
        }
        let named = Named {
            album_artist: &want.track.album_artist,
            album: &want.track.album,
            title: &want.track.title,
            disc: want.track.disc,
            track: want.track.track,
            discs: want.track.discs,
            extension,
        };
        let id = want.track.appearance_pid.to_string();
        let tail = id.get(id.len().saturating_sub(12)..).unwrap_or(&id);
        // A file there with no baseline is not ours: its name is taken,
        // and the track is blocked rather than given another name, so that
        // a second sync never writes a second copy beside it.
        let plain = naming::path(music_root, named, None, capabilities.path_bytes);
        let path = match plain {
            Some(path) if there.contains_key(&path) && !held.contains(&path) => Some(path),
            Some(path) if !taken.contains(&naming::folded(&path)) => Some(path),
            _ => [4usize, 8, 12].into_iter().find_map(|digits| {
                let suffix = tail.get(tail.len().saturating_sub(digits)..)?;
                naming::path(music_root, named, Some(suffix), capabilities.path_bytes)
                    .filter(|path| !taken.contains(&naming::folded(path)))
            }),
        };
        if let Some(path) = &path {
            taken.insert(naming::folded(path));
        }
        paths.insert(want.track.appearance, path);
    }

    // 6. Wanted against seen against recorded.
    let mut removes = Vec::new();
    let mut replaces = Vec::new();
    let mut adds = Vec::new();
    let mut notes = Vec::new();
    let mut gone = Vec::new();
    let mut after: HashSet<i64> = HashSet::new();
    let source_id = |want: &Want<'_>| sources.get(want.source).map(|(source, _)| source.id);
    for want in &wants {
        let track = want.track;
        let baseline = by_appearance.get(&track.appearance).copied();
        let seen_there = baseline.and_then(|baseline| there.get(&baseline.path).copied());
        let old = |baseline: &Baseline| Old {
            baseline: baseline.id,
            path: baseline.path.clone(),
            size: baseline.size,
            sampled: baseline.sampled,
        };
        let row = |kind, reason, path: Option<RelPath>| Action {
            kind,
            reason,
            source: source_id(want),
            appearance: Some(track.appearance),
            shown: Shown::of(track),
            path,
            old: None,
            make: None,
            bytes: 0,
            estimated: false,
            retag: false,
            masters: Vec::new(),
        };
        if want.cut {
            // What the cut leaves off is not wanted: step 7 removes it.
            continue;
        }
        let make = match (&want.sent, &want.make) {
            (Ok(_), Some(make)) => make,
            (Err(blocked), _) => {
                // Wanted, with no source or no format the target plays:
                // blocked with the reason, and what is there stays.
                notes.push(row(
                    SyncActionKind::Blocked,
                    blocked.reason(),
                    baseline.map(|baseline| baseline.path.clone()),
                ));
                if seen_there.is_some() {
                    after.insert(track.appearance);
                }
                continue;
            }
            (Ok(_), None) => continue,
        };
        // A file changed on the target is a conflict, wanted or not.
        if let (Some(baseline), Some(object)) = (baseline, seen_there)
            && object.size != baseline.size
        {
            let mut conflict = row(
                SyncActionKind::Conflict,
                "changed_on_device",
                Some(baseline.path.clone()),
            );
            conflict.bytes = object.size;
            notes.push(conflict);
            after.insert(track.appearance);
            continue;
        }
        let Some(path) = paths.get(&track.appearance).cloned().flatten() else {
            notes.push(row(SyncActionKind::Blocked, "path_too_long", None));
            continue;
        };
        let mut action = row(SyncActionKind::Add, "wanted", Some(path.clone()));
        action.make = Some(make.clone());
        action.bytes = want.bytes;
        action.estimated = want.estimated;
        match (baseline, seen_there) {
            (Some(baseline), Some(_)) if baseline.key == make.key => {
                // The whole second run: nothing.
                after.insert(track.appearance);
            }
            (Some(baseline), Some(_)) => {
                let reason = baseline.key.differs(&make.key).unwrap_or("tags");
                action.kind = SyncActionKind::Replace;
                action.reason = reason;
                action.retag = matches!(reason, "tags" | "cover");
                action.old = Some(old(baseline));
                after.insert(track.appearance);
                replaces.push(action);
            }
            (Some(baseline), None) => {
                // A managed file that went missing is sent again.
                action.reason = "missing";
                action.old = Some(old(baseline));
                after.insert(track.appearance);
                adds.push(action);
            }
            (None, _) if there.contains_key(&path) => {
                // A file Ritornel did not write at the planned path.
                let mut taken = row(SyncActionKind::Blocked, "path_taken", Some(path));
                taken.make = None;
                notes.push(taken);
            }
            (None, _) => {
                after.insert(track.appearance);
                adds.push(action);
            }
        }
    }

    // 7. What is recorded and no longer wanted.
    for baseline in baselines {
        let wanted = wanted_at
            .get(&baseline.appearance)
            .and_then(|at| wants.get(*at))
            .is_some_and(|want| !want.cut);
        if wanted {
            continue;
        }
        let Some(object) = there.get(&baseline.path) else {
            gone.push(baseline.id);
            continue;
        };
        let mut action = Action {
            kind: SyncActionKind::Remove,
            reason: "not_wanted",
            source: None,
            appearance: Some(baseline.appearance),
            shown: baseline.shown.clone(),
            path: Some(baseline.path.clone()),
            old: Some(Old {
                baseline: baseline.id,
                path: baseline.path.clone(),
                size: baseline.size,
                sampled: baseline.sampled,
            }),
            make: None,
            bytes: object.size,
            estimated: false,
            retag: false,
            masters: Vec::new(),
        };
        if object.size != baseline.size {
            action.kind = SyncActionKind::Conflict;
            action.reason = "changed_on_device";
            action.old = None;
            notes.push(action);
        } else if !master_there(baseline.appearance) {
            // Nothing is destroyed that the library cannot make again.
            action.kind = SyncActionKind::Conflict;
            action.reason = "master_gone";
            action.old = None;
            notes.push(action);
        } else {
            action.masters = appearance_index
                .get(&baseline.appearance)
                .and_then(|index| tracks.get(*index))
                .map(|track| {
                    track
                        .copies
                        .iter()
                        .filter(|copy| !copy.missing)
                        .map(|copy| (copy.root.clone(), copy.rel_path.clone()))
                        .collect()
                })
                .unwrap_or_default();
            removes.push(action);
        }
    }

    // Removes, then replaces that shrink, then replaces that grow, then adds.
    let shrinks = |action: &Action| {
        action
            .old
            .as_ref()
            .is_some_and(|old| action.bytes <= old.size)
    };
    replaces.sort_by_key(|action| !shrinks(action));
    let mut actions = removes;
    actions.extend(replaces);
    actions.extend(adds);
    actions.extend(notes);

    let mut counts = SyncCounts::default();
    let mut per_source: HashMap<Option<Id>, SyncPlanSource> = HashMap::new();
    let (mut encodes_needed, mut encodes_cached) = (0, 0);
    for action in &actions {
        let entry = per_source.entry(action.source).or_insert(SyncPlanSource {
            source_id: action.source,
            add: SyncAmount::default(),
            replace: SyncAmount::default(),
            remove: SyncAmount::default(),
        });
        match action.kind {
            SyncActionKind::Add => {
                amount(&mut counts.add, action.bytes);
                amount(&mut entry.add, action.bytes);
            }
            SyncActionKind::Replace => {
                amount(&mut counts.replace, action.bytes);
                amount(&mut entry.replace, action.bytes);
                if action.retag {
                    amount(&mut counts.retag, action.bytes);
                }
            }
            SyncActionKind::Remove => {
                amount(&mut counts.remove, action.bytes);
                amount(&mut entry.remove, action.bytes);
            }
            SyncActionKind::Conflict => amount(&mut counts.conflict, action.bytes),
            SyncActionKind::Blocked => amount(&mut counts.blocked, action.bytes),
        }
        if let Some(make) = &action.make
            && make.encode.is_some()
            && action.runs()
        {
            encodes_needed += 1;
            encodes_cached += u64::from(make.encode_cached);
        }
    }
    let mut by_source: Vec<SyncPlanSource> = sources
        .iter()
        .filter_map(|(source, _)| per_source.remove(&Some(source.id)))
        .collect();
    by_source.extend(per_source.remove(&None));

    let running: Vec<&Action> = actions.iter().filter(|action| action.runs()).collect();
    let without_retags: Vec<&Action> = running
        .iter()
        .copied()
        .filter(|action| !action.retag)
        .collect();
    let totals_figures = figures(&running, free, speed);
    let managed_after = managed
        .saturating_add(totals_figures.to_copy_bytes)
        .saturating_sub(
            running
                .iter()
                .map(|action| match action.kind {
                    SyncActionKind::Remove => action.bytes,
                    _ => action.old.as_ref().map_or(0, |old| old.size),
                })
                .sum(),
        );

    // The revision names what would be done, and not what it would take:
    // a size estimate that sharpens as encodes are cached does not move it.
    let mut hasher = blake3::Hasher::new();
    for action in &actions {
        hasher.update(format!("{:?}|{}|", action.kind, action.reason).as_bytes());
        hasher.update(&action.appearance.unwrap_or(0).to_le_bytes());
        hasher.update(action.path.as_ref().map_or("", RelPath::as_str).as_bytes());
        hasher.update(&action.make.as_ref().map_or([0; 32], |make| make.key.0));
        hasher.update(
            &action
                .old
                .as_ref()
                .map_or(0, |old| old.baseline)
                .to_le_bytes(),
        );
    }
    for baseline in &gone {
        hasher.update(&baseline.to_le_bytes());
    }
    let revision = hasher
        .finalize()
        .to_hex()
        .as_str()
        .get(..24)
        .unwrap_or_default()
        .to_owned();

    let albums_after = tracks
        .iter()
        .filter(|track| after.contains(&track.appearance))
        .map(TrackRow::group_key)
        .collect::<HashSet<_>>()
        .len() as u64;
    Plan {
        revision,
        basis: seen.basis,
        gone,
        sources: totals,
        by_source,
        counts,
        figures: totals_figures,
        figures_skipping_retags: figures(&without_retags, free, speed),
        fits: cut.tracks == 0,
        cut,
        room: SyncRoom {
            capacity_bytes: capacity,
            reserve_bytes: reserve,
            managed_bytes: managed,
            unmanaged_bytes,
            free_bytes: free,
            managed_after_bytes: managed_after,
            free_after_bytes: totals_figures.free_after_bytes,
            over_bytes: wanted_bytes.saturating_sub(room),
            basis: seen.basis,
        },
        encodes_needed,
        encodes_cached,
        unmanaged,
        tracks_after: after.len() as u64,
        albums_after,
        wanted_bytes,
        actions,
    }
}
