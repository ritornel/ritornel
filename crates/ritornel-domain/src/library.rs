//! What the resolver is given and what it returns. See "Recording
//! resolution" in docs/internals/identity.md: the resolver is a pure
//! function over claims, a catalog snapshot, user assignments, exclusions,
//! learned aliases and a policy, processing items in first-seen order.
//!
//! Rows are named by their integer keys in `library.db`. The resolver
//! never makes one: something it wants created is a [`Ref::New`] that the
//! writer gives a row.

use std::collections::{HashMap, HashSet};

use serde::Serialize;

use crate::extract::Facts;

/// How a binding was decided: `identity_decision.method`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Method {
    /// The owner made it.
    User,
    /// The audio hash equals an asset bound through another item.
    SameAudio,
    /// A recording MusicBrainz ID equals one an existing recording carries.
    Identifier,
    /// Tags and duration, within the policy's thresholds.
    TagsDuration,
    /// Nothing bound, so the item has a recording of its own.
    New,
}

impl Method {
    /// The method as the schema writes it.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::SameAudio => "same_audio",
            Self::Identifier => "identifier",
            Self::TagsDuration => "tags_duration",
            Self::New => "new",
        }
    }

    /// The method the schema writes as `text`.
    pub fn parse(text: &str) -> Option<Self> {
        [
            Self::User,
            Self::SameAudio,
            Self::Identifier,
            Self::TagsDuration,
            Self::New,
        ]
        .into_iter()
        .find(|method| method.as_str() == text)
    }
}

/// A row that exists, or one the resolution asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Ref {
    /// The row with this key.
    Row(i64),
    /// A new row: the entry at this index of the resolution's list for its
    /// kind.
    New(usize),
}

/// An item's current assignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Current {
    /// The recording it names, which may since have been merged away.
    pub recording: i64,
    /// The appearance it names.
    pub appearance: Option<i64>,
    /// How its decision was made.
    pub method: Method,
    /// Whether the owner made it. Only the owner supersedes such a one.
    pub by_user: bool,
    /// Whether its decision is marked for review. A resolution that would
    /// mark it otherwise writes a new decision, so that the review queue is
    /// always what the policy in force says.
    pub needs_review: bool,
    /// Whether its evidence says it binds across albums by the exception
    /// for an all but equal length. A resolution that would say otherwise
    /// writes a new decision, so that every such binding is marked.
    pub across_albums: bool,
}

/// One source item, as the resolver sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// The `source_item` row.
    pub id: i64,
    /// The folder the file is in, with its root, so that two roots' folders
    /// never look the same.
    pub folder: String,
    /// The `audio_asset` row of its current revision. `None` for a file
    /// that could not be read as audio.
    pub audio: Option<i64>,
    /// Whether that audio is lossless.
    pub lossless: bool,
    /// Whether a different song has taken its path. A retired item keeps
    /// its binding and is never resolved again.
    pub retired: bool,
    /// When it was first seen, as Unix milliseconds.
    pub first_seen_ms: i64,
    /// The `picture` row of its cover, when it has one.
    pub cover: Option<i64>,
    /// What its claims say, with any tag values an answer gave put in place
    /// of its own.
    pub facts: Facts,
    /// The fields of `facts` such a value gave, as claims name them. No
    /// rule rewrites them: see [`crate::values`].
    pub held: Vec<&'static str>,
    /// Its current assignment.
    pub current: Option<Current>,
    /// The appearance it is to be kept on, when not its current one: where
    /// it was before an answer now undone moved it. It votes for its release
    /// and appearance rows in place of its current one, so the undo gives
    /// back the rows it had.
    pub prior: Option<i64>,
    /// The recording it was on before an answer now undone: the one it
    /// takes again when it is parted from the recording it is on now.
    pub prior_recording: Option<i64>,
}

/// What a yes put together: release and release group rows, which the
/// guards that keep rows with input of their own apart count as one while
/// it stands.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Join {
    /// The album the yes said its files are, by its release group ID. The
    /// groups of every standing yes to one album count as one too.
    pub album: String,
    /// Release rows, each the end of its redirect chain.
    pub releases: Vec<i64>,
    /// Release group rows.
    pub groups: Vec<i64>,
}

/// A release that exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExistingRelease {
    /// The end of its redirect chain.
    pub canonical: i64,
    /// The owner's choice of group.
    pub chosen_group: Option<i64>,
    /// The group in effect.
    pub group: i64,
}

/// A release group that exists.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExistingGroup {
    /// The owner's preferred release for a collapsed tile.
    pub preferred_release: Option<i64>,
    /// Whether the owner explicitly set a presentation ('collapsed' or
    /// 'expanded') on this group.
    pub has_presentation: bool,
}

/// What kind of durable key an artist row owns (`artist_key.key_kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ArtistKeyKind {
    /// A MusicBrainz artist ID.
    Mbid,
    /// A mechanically normalised artist name.
    Name,
}

impl ArtistKeyKind {
    /// The kind as the schema writes it.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Mbid => "mbid",
            Self::Name => "name",
        }
    }

    /// The kind the schema writes as `text`.
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "mbid" => Some(Self::Mbid),
            "name" => Some(Self::Name),
            _ => None,
        }
    }
}

/// One durable artist key ownership row (`artist_key`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExistingArtistKey {
    /// Whether `value` is a MusicBrainz ID or a normalised name.
    pub kind: ArtistKeyKind,
    /// The MBID or normalised name.
    pub value: String,
    /// The artist row that owns this key, which may redirect to another via
    /// [`Library::artist_canonical`].
    pub artist: i64,
    /// The normaliser version that made `value` when `kind` is
    /// [`ArtistKeyKind::Name`]; `None` for [`ArtistKeyKind::Mbid`].
    pub normalizer_version: Option<i64>,
}

/// A credit that exists: its text, and each artist with the name as
/// credited and the join phrase after it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExistingCredit {
    /// The `artist_credit` row.
    pub id: i64,
    /// The whole credit as one string.
    pub text: String,
    /// Its names, in order.
    pub names: Vec<(i64, String, String)>,
}

/// What an alias or an edit is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// A recording, and for an alias a title.
    Recording,
    /// A track appearance.
    TrackAppearance,
    /// A release.
    Release,
    /// A release group.
    ReleaseGroup,
    /// An artist.
    Artist,
}

/// The catalog snapshot and everything else the resolver reads.
#[derive(Debug, Clone, Default)]
pub struct Library {
    /// Every source item, in first-seen order.
    pub items: Vec<Item>,
    /// Each merged recording and the end of its redirect chain.
    pub recording_canonical: HashMap<i64, i64>,
    /// Each appearance: its recording and its release.
    pub appearances: HashMap<i64, (i64, Option<i64>)>,
    /// Each merged track appearance and the end of its redirect chain.
    pub appearance_canonical: HashMap<i64, i64>,
    /// Each release.
    pub releases: HashMap<i64, ExistingRelease>,
    /// Each release group, with its owner-set presentation and preferred release.
    pub groups: HashMap<i64, ExistingGroup>,
    /// Each durable artist key and the artist row that owns it, in row order.
    pub artist_keys: Vec<ExistingArtistKey>,
    /// Each merged artist and the end of its redirect chain.
    pub artist_canonical: HashMap<i64, i64>,
    /// The version of the normaliser the stored name keys were made by, when
    /// it is not this one's.
    pub artist_keys_by: Option<i64>,
    /// Each credit.
    pub credits: Vec<ExistingCredit>,
    /// Each "this item is not that recording".
    pub exclusions: HashSet<(i64, i64)>,
    /// Pairs of normalised names the owner's actions showed to be the same
    /// thing, for titles ([`Kind::Recording`]) and artists.
    pub aliases: Vec<(Kind, String, String)>,
    /// The group each of these releases is to be kept in, when not the one
    /// it is in: the group it was in before an answer now undone moved it.
    /// It votes for that group's row, since a group has no redirect.
    pub prior_groups: HashMap<i64, i64>,
    /// Release rows an answer led on to where their files went, as it left
    /// them. They are the answer's, not merges made by hand.
    pub answer_redirects: HashSet<i64>,
    /// What each standing answer put together. Rows one joins count as one
    /// to the guards that keep rows with input of their own apart.
    pub answer_joins: Vec<Join>,
    /// The owner's edits in force: entity kind, row and field, to value.
    pub edits: HashMap<(Kind, i64, String), String>,
}

/// The penalties behind a tags-and-duration distance, each from 0 to 1.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Penalties {
    /// The titles differ in a bracketed note.
    pub title: f64,
    /// How far apart the artists are.
    pub artist: f64,
    /// How far apart the durations are, beyond the grace.
    pub duration: f64,
    /// Whether the albums differ.
    pub album: f64,
    /// Whether a shared ISRC or AcoustID lowered the distance.
    pub shared_identifier: bool,
}

/// One candidate the resolver weighed, by the item whose evidence it is.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Candidate {
    /// The source item whose claims were compared.
    pub item: i64,
    /// The distance.
    pub distance: f64,
    /// What made the distance up. It is not stored with the decision,
    /// whose evidence keeps the shape it has always had: a review listing
    /// asks the resolver again.
    #[serde(skip)]
    pub penalties: Option<Penalties>,
}

/// What a decision rested on: `identity_decision.evidence_json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Evidence {
    /// The claims used, by field or namespace and value.
    pub claims: Vec<(String, String)>,
    /// The candidates, nearest first.
    pub candidates: Vec<Candidate>,
    /// What stopped an identifier from binding.
    pub contradictions: Vec<String>,
    /// Set on a binding by tags to a recording on another album, which
    /// only the exception for an all but equal length allows: so that every
    /// such binding can be found again if the exception proves wrong.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub across_albums: Option<AcrossAlbums>,
    /// A match the policy held back from binding. The writer stores it
    /// with the recording it names as a row, so this field is not
    /// serialised here.
    #[serde(skip)]
    pub withheld: Option<Withheld>,
}

/// A binding by tags across two albums, made because the lengths agree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct AcrossAlbums {
    /// How far apart the two lengths were, in milliseconds.
    pub length_difference_ms: i64,
}

/// Why a match that was near enough to bind was held back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WithheldCause {
    /// It was a match on tags and duration alone, to a recording on another
    /// album, and the policy binds by tags only within one album.
    OtherAlbum,
    /// It was a match on tags and duration alone, to a recording that
    /// another track of the file's own release is bound to.
    SameRelease,
}

impl WithheldCause {
    /// The cause as the evidence writes it.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OtherAlbum => "other_album",
            Self::SameRelease => "same_release",
        }
    }
}

/// A match the policy held back from binding: the decision was marked for
/// review instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Withheld {
    /// Why it was held back.
    pub cause: WithheldCause,
    /// The source item whose claims it matched: the first candidate, or
    /// for [`WithheldCause::SameRelease`] the other track of the release.
    pub item: i64,
    /// The recording it nearly matched: a row, or one the resolution
    /// makes. Filled in when the catalog is formed.
    pub recording: Option<Ref>,
}

/// A pair of differing names a binding showed to be the same thing, carrying
/// both the normalised keys (`name_a < name_b`) and the raw spellings they
/// came from so a future normaliser version can recompute the keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LearnedAlias {
    /// Whether the pair is of titles ([`Kind::Recording`]) or artists ([`Kind::Artist`]).
    pub kind: Kind,
    /// Normalised key (lesser of the pair).
    pub name_a: String,
    /// Normalised key (greater of the pair).
    pub name_b: String,
    /// Raw spelling that produced `name_a`.
    pub spelling_a: String,
    /// Raw spelling that produced `name_b`.
    pub spelling_b: String,
}

/// One decision, with the full score shape. See "Recording resolution" in
/// docs/internals/identity.md.
#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    /// How it was decided.
    pub method: Method,
    /// The distance: 0 for an exact method.
    pub score: f64,
    /// The gap to the runner-up, when there was one.
    pub runner_up_gap: Option<f64>,
    /// The penalties, for a tags-and-duration decision.
    pub penalties: Option<Penalties>,
    /// What it rested on.
    pub evidence: Evidence,
    /// Whether the owner should look at it.
    pub needs_review: bool,
    /// Pairs of differing normalised names this binding showed to be the
    /// same thing, with the raw spellings they came from.
    pub aliases: Vec<LearnedAlias>,
}

/// What the resolver decided for one item.
#[derive(Debug, Clone, PartialEq)]
pub struct ItemOut {
    /// The `source_item` row.
    pub item: i64,
    /// The recording to bind it to. `None` for a file with no audio that
    /// was never bound.
    pub recording: Option<Ref>,
    /// The appearance on that recording.
    pub appearance: Option<Ref>,
    /// The decision. `None` where the resolver decided nothing: the
    /// assignment is the owner's, or the item is retired or unreadable.
    pub decision: Option<Decision>,
    /// Whether the result differs from the current assignment, so that a
    /// new one is to be written.
    pub changed: bool,
    /// Whether the owner's own assignment held it where it is.
    pub pinned: bool,
    /// Whether an exclusion of the owner's took a candidate away from it.
    pub blocked: bool,
}

/// An artist the catalog should have.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtistOut {
    /// The row, or a new one.
    pub r: Ref,
    /// The name to show.
    pub name: String,
    /// The name to sort by.
    pub sort_name: String,
    /// The mechanically normalised name.
    pub match_name: String,
    /// Its MusicBrainz ID, when it is known by one.
    pub mbid: Option<String>,
    /// The durable key (`artist_key`) this artist row owns. `None` only for
    /// a canonical merge-target row emitted solely so its display is kept
    /// when it has no direct files of its own.
    pub key: Option<(ArtistKeyKind, String)>,
}

/// A credit the catalog should have.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreditOut {
    /// The row, or a new one.
    pub r: Ref,
    /// The whole credit as one string.
    pub text: String,
    /// Each artist (an index into the resolution's artists), the name as
    /// credited, and the join phrase after it.
    pub names: Vec<(usize, String, String)>,
}

/// A recording with its projected display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordingOut {
    /// The row, or a new one.
    pub r: Ref,
    /// The title, projected from every item bound to it.
    pub title: Option<String>,
    /// The credit, as an index into the resolution's credits.
    pub credit: Option<usize>,
    /// Whether the display was projected. It was not, and what the row
    /// shows is to be left alone, when every item bound to the recording
    /// is one the resolver does not touch.
    pub projected: bool,
}

/// A release group with its projected display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupOut {
    /// The row, or a new one.
    pub r: Ref,
    /// The release a collapsed tile shows, as an index into the
    /// resolution's releases.
    pub display_release: Option<usize>,
    /// What kind of record it is. `None` is unknown.
    pub primary_type: Option<String>,
    /// The marks beside the kind, sorted.
    pub secondary_types: Vec<String>,
    /// When the record first came out.
    pub first_date: Option<String>,
    /// The year of that date.
    pub first_year: Option<i64>,
}

/// A release with its projected display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseOut {
    /// The row, or a new one.
    pub r: Ref,
    /// Its group, as an index into the resolution's groups.
    pub group: usize,
    /// The album title.
    pub title: Option<String>,
    /// The album artist's credit.
    pub credit: Option<usize>,
    /// The release date, as precise as the claims are.
    pub date: Option<String>,
    /// The year of that date.
    pub year: Option<i64>,
    /// The `picture` row of the cover.
    pub picture: Option<i64>,
}

/// A track appearance with its projected display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppearanceOut {
    /// The row, or a new one.
    pub r: Ref,
    /// Its recording: a row, or an index into the resolution's recordings.
    pub recording: Ref,
    /// Its release, as an index into the resolution's releases. `None` for
    /// a loose file.
    pub release: Option<usize>,
    /// The title.
    pub title: Option<String>,
    /// The artist credit.
    pub credit: Option<usize>,
    /// The disc number.
    pub disc: Option<i64>,
    /// The track number.
    pub track: Option<i64>,
    /// The duration in milliseconds.
    pub duration_ms: Option<i64>,
    /// When its oldest source item was first seen.
    pub added_at_ms: Option<i64>,
}

/// What the catalog should be, given everything the items claim.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Resolution {
    /// One entry per item, in the library's order.
    pub items: Vec<ItemOut>,
    /// Every artist in use.
    pub artists: Vec<ArtistOut>,
    /// Every credit in use.
    pub credits: Vec<CreditOut>,
    /// Every recording that has an item.
    pub recordings: Vec<RecordingOut>,
    /// Every release group in use.
    pub groups: Vec<GroupOut>,
    /// Every release in use.
    pub releases: Vec<ReleaseOut>,
    /// Every appearance in use.
    pub appearances: Vec<AppearanceOut>,
}
