//! The identity calls of M1's first slice: the dry run of the resolver and
//! the review listing. See "The proof" and "Resolver policy 1" in
//! docs/design/identity.md. Policy 2 is policy 1 with four values changed;
//! scans resolve by it, and a dry run can still ask about policy 1.
//!
//! Both name files by their path under their root, because what the owner
//! judges is a file. That is the only path they carry, and both need a
//! `full` session.

use serde::{Deserialize, Serialize};

use crate::{Count, Id};

/// The policy one dry run decides by: a named policy, and values to put in
/// place of its own. A value left out is the named policy's.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct PolicyOverrides {
    /// The named policy, 1 or 2. Left out, it is the one scans resolve by.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub version: Option<u64>,
    /// A tags-and-duration match binds at this distance or less: 0 to 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub bind_max: Option<f64>,
    /// And only if the runner-up is at least this much further: 0 to 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub min_gap: Option<f64>,
    /// A new recording is marked for review if a candidate scored under
    /// this: 0 to 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub review_below: Option<f64>,
    /// A difference in duration costs nothing up to this, in milliseconds:
    /// at most a minute.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub duration_grace_ms: Option<u64>,
    /// Whether candidate text has its accents taken off before it is
    /// compared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub fold_accents: Option<bool>,
    /// Whether sibling folders of one album whose track numbers collide,
    /// which share no title and no year, and whose names each give a disc,
    /// are discs of one release.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub discs_from_folders: Option<bool>,
    /// Whether a trailing "Disc 2" or "CD 2" in an album title is the
    /// disc number.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub disc_from_album_title: Option<bool>,
    /// Whether a match on tags and length alone may bind a file to a
    /// recording on another album.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub tags_bind_across_albums: Option<bool>,
    /// When they may not, the largest difference in length, in
    /// milliseconds, at which they still may: at most a minute, and 0 for
    /// none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub across_albums_within_ms: Option<u64>,
    /// Whether two different tracks of one release are never one recording
    /// by tags.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub tracks_of_one_release_differ: Option<bool>,
    /// Which list of version markers: 1 or 2.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub marker_list: Option<u64>,
}

/// The policy a dry run decided by: a named policy with any values put in
/// its place.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct PolicyValues {
    /// The named policy the values are, or are a variant of: the resolver
    /// version its decisions record.
    pub resolver_version: u64,
    /// Whether every value is that policy's own.
    pub exact: bool,
    /// The bind distance.
    pub bind_max: f64,
    /// The gap to the runner-up.
    pub min_gap: f64,
    /// The review threshold.
    pub review_below: f64,
    /// The free duration band, in milliseconds.
    pub duration_grace_ms: u64,
    /// Whether candidate text is accent-folded.
    pub fold_accents: bool,
    /// Whether sibling folders can be discs of one release.
    pub discs_from_folders: bool,
    /// Whether a disc marker in an album title is the disc number.
    pub disc_from_album_title: bool,
    /// Whether a match on tags alone may bind across albums.
    pub tags_bind_across_albums: bool,
    /// When it may not, the difference in length within which it still
    /// may, in milliseconds. 0 is none.
    pub across_albums_within_ms: u64,
    /// Whether two different tracks of one release are never one
    /// recording by tags.
    pub tracks_of_one_release_differ: bool,
    /// The list of version markers: 1, or 2 with every spelling of "a
    /// cappella" and part numbers.
    pub marker_list: u64,
}

/// The params of `identity.dryRun`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct DryRunParams {
    /// What to decide by: the policy scans use, unless it names another
    /// or puts values in place of its own.
    #[serde(default)]
    pub policy: PolicyOverrides,
    /// Whether to list every item that is not unchanged. Without it the
    /// answer is counts alone.
    #[serde(default)]
    pub detail: bool,
}

/// What the dry run says of one item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum ItemOutcome {
    /// The result is the assignment it has.
    Unchanged,
    /// The result differs: another recording, or another place on a
    /// release.
    WouldChange,
    /// The owner's own assignment holds it where it is.
    Pinned,
    /// An exclusion of the owner's took a candidate away from it, and it
    /// stays where it is.
    Blocked,
}

/// A file as the owner judges it: where it is and what its tags say.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct FileFacts {
    /// The source item's ID.
    pub item_id: Id,
    /// The file's path under its root.
    pub path: String,
    /// The title its tags give, or its file name gives.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub title: Option<String>,
    /// The artist its tags give.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub artist: Option<String>,
    /// The album its tags give.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub album: Option<String>,
    /// The disc number its tags give.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub disc: Option<u64>,
    /// The track number its tags give.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub track: Option<u64>,
    /// Its length in milliseconds, from the audio read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub duration_ms: Option<u64>,
}

/// What made a tags-and-duration distance, each from 0 to 1.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct Penalties {
    /// The titles differ in a bracketed note.
    pub title: f64,
    /// How far apart the artists are.
    pub artist: f64,
    /// How far apart the lengths are, beyond the free band.
    pub duration: f64,
    /// Whether the albums differ: 1, or 0.5 when one has none.
    pub album: f64,
    /// Whether a shared ISRC or AcoustID halved the distance.
    pub shared_identifier: bool,
}

/// The candidate an item was nearest to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct Nearest {
    /// The file whose claims it was compared with.
    pub file: FileFacts,
    /// The distance, from 0 to 1.
    pub distance: f64,
    /// What made the distance up, when the resolver can say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub penalties: Option<Penalties>,
    /// The recording it nearly matched, after redirects, where one exists:
    /// for a review, the one the decision names, or else the one that file
    /// is bound to now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub recording_id: Option<Id>,
}

/// One side of a dry run's comparison: what an item is bound to, and how
/// that was decided.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct Binding {
    /// The recording, after redirects. Left out for one the run would
    /// create.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub recording_id: Option<Id>,
    /// How it was decided: `user`, `same_audio`, `identifier`,
    /// `tags_duration` or `new`. Left out where nothing was decided.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub method: Option<String>,
    /// The distance it was decided at: 0 for an exact method.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub score: Option<f64>,
    /// Whether it is marked for review.
    pub needs_review: bool,
    /// The release it is on. Left out for a loose track, and for one the
    /// run would create.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub release_id: Option<Id>,
    /// That release's title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub release_title: Option<String>,
    /// The disc it is on there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub disc: Option<u64>,
    /// The candidate it was nearest to, for a decision by tags.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub nearest: Option<Nearest>,
}

/// One item that the dry run does not find unchanged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ItemChange {
    /// What the run says of it.
    pub outcome: ItemOutcome,
    /// The file.
    pub file: FileFacts,
    /// What it is bound to now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub now: Option<Binding>,
    /// What the run would bind it to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub after: Option<Binding>,
}

/// The result of `identity.dryRun`: the resolver run again over the stored
/// revisions, compared with the assignments in force. Nothing is written.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct DryRun {
    /// The policy it decided by.
    pub policy: PolicyValues,
    /// The library revision it read.
    pub library_revision: u64,
    /// Whether the claims it read were made by another version of the
    /// extractor, which the next scan rebuilds before it resolves.
    pub claims_stale: bool,
    /// Whether applying the result would write anything at all, what the
    /// catalog shows included.
    pub would_write: bool,
    /// Files the library has, retired ones included.
    pub items: u64,
    /// Of them, those whose result is the assignment they have. A file
    /// that cannot be read, or is retired, is one.
    pub unchanged: u64,
    /// Those whose result differs.
    pub would_change: u64,
    /// Of those, the ones bound to another recording. The rest keep their
    /// recording and move on a release.
    pub recordings_changed: u64,
    /// Those the owner's own assignment holds.
    pub pinned: u64,
    /// Those an owner's exclusion kept from a candidate.
    pub blocked: u64,
    /// Recordings that would be joined to another: for each recording of
    /// the result, the recordings its files have now, less one.
    pub merges: u64,
    /// Recordings that would be parted: for each recording files have now,
    /// the recordings of the result they would have, less one.
    pub splits: u64,
    /// Files whose binding in force is marked for review.
    pub reviews_now: u64,
    /// Files the run's decisions mark for review.
    pub reviews_after: u64,
    /// Files marked now and not by the run.
    pub reviews_cleared: u64,
    /// Files marked by the run and not now.
    pub reviews_added: u64,
    /// Releases with a file on them now.
    pub releases_now: u64,
    /// Releases the run's files would be on.
    pub releases_after: u64,
    /// With `detail`, every item that is not unchanged, in first-seen
    /// order.
    pub changes: Vec<ItemChange>,
}

/// Why a decision was marked for review, from what made up the distance
/// to the candidate it nearly matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum ReviewCause {
    /// A recording MusicBrainz ID was carried by a file of another length
    /// or title.
    Contradiction,
    /// Near enough by tags and length to bind, to a recording on another
    /// album: policy 2 binds by tags only within one album.
    OtherAlbum,
    /// Near enough by tags and length to bind, to a recording that
    /// another track of the same release is bound to: policy 2 never makes
    /// two tracks of one release one recording by tags.
    SameRelease,
    /// Near enough to bind, with a runner-up too close behind it.
    CloseRunnerUp,
    /// The artists are spelled differently.
    ArtistSpelling,
    /// The titles differ in a bracketed note.
    TitleNote,
    /// The same song on another album, at another length.
    LengthOtherAlbum,
    /// The same song on the same album, at another length.
    LengthSameAlbum,
    /// None of these.
    Other,
}

impl ReviewCause {
    /// The cause as it is written on the wire.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Contradiction => "contradiction",
            Self::OtherAlbum => "other_album",
            Self::SameRelease => "same_release",
            Self::CloseRunnerUp => "close_runner_up",
            Self::ArtistSpelling => "artist_spelling",
            Self::TitleNote => "title_note",
            Self::LengthOtherAlbum => "length_other_album",
            Self::LengthSameAlbum => "length_same_album",
            Self::Other => "other",
        }
    }
}

/// One decision marked for review.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct Review {
    /// Why it was marked.
    pub cause: ReviewCause,
    /// The file.
    pub file: FileFacts,
    /// The candidate it nearly matched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub nearest: Option<Nearest>,
    /// The gap from that candidate to the runner-up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub runner_up_gap: Option<f64>,
    /// How many candidates were weighed, at most five.
    pub candidates: u64,
    /// What stopped an identifier from binding.
    pub contradictions: Vec<String>,
}

/// The result of `identity.review`: every current binding marked for
/// review, grouped by cause.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ReviewList {
    /// The library revision it read.
    pub library_revision: u64,
    /// How many there are by cause, in the order they are listed.
    pub causes: Vec<Count>,
    /// Each one, by cause and then by path.
    pub reviews: Vec<Review>,
}
