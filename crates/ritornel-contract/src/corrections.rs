//! Corrections: the evidence a library keeps for correcting itself from
//! MusicBrainz. See docs/internals/corrections.md.
//!
//! The calls name files and folders by their path under their root, so
//! every one needs a `full` session.

use serde::{Deserialize, Serialize};

use crate::{Id, Receipt};

/// The params of `corrections.discs`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct DiscsParams {
    /// One folder, by its path under its root, with `/` between its parts.
    /// Its discs come with the file at each position. Left out, every
    /// disc of the library comes, without its files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub folder: Option<String>,
}

/// How a disc's layout was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum LayoutMethod {
    /// A drive's table of sectors in a rip log.
    Log,
    /// A CUE sheet, with the exact lengths of the files it names.
    Cue,
    /// A CUE sheet whose files were renamed since it was written.
    RenamedCue,
    /// The files' lengths in order, checked against an AccurateRip disc ID
    /// printed beside them.
    #[serde(rename = "accuraterip")]
    AccurateRip,
    /// The files' lengths in order, checked against the CDDB ID a CUE
    /// sheet prints.
    Cddb,
}

/// One file at one position of a disc.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct DiscFile {
    /// The track's position on the disc, from 1, or 0 for a file that holds
    /// the whole disc.
    pub position: u32,
    /// The file, by its path under its root.
    pub path: String,
}

/// One layout of one folder's files: one CD, as its sidecars give it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct Disc {
    /// The root the folder is in, by its name.
    pub root: String,
    /// The folder, by its path under its root: empty for the root.
    pub folder: String,
    /// The MusicBrainz disc ID the layout gives.
    pub disc_id: String,
    /// How many tracks it has.
    pub tracks: u32,
    /// How long it is, in CD sectors.
    pub sectors: u64,
    /// Each way it was found, in the order found.
    pub methods: Vec<LayoutMethod>,
    /// Whether the layout may not be the disc's: a CUE sheet that disagrees
    /// with the CDDB ID it prints.
    pub provisional: bool,
    /// Whether one file holds the whole disc.
    pub whole_file: bool,
    /// Whether every track was matched to one file by its length.
    pub matched: bool,
    /// The file at each position, when one folder was asked for. Empty
    /// otherwise.
    pub files: Vec<DiscFile>,
}

/// The result of `corrections.discs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct DiscList {
    /// The library revision it read.
    pub library_revision: u64,
    /// How many folders hold audio, of those asked about.
    pub folders: u64,
    /// How many of those have at least one layout that is not provisional.
    pub with_layout: u64,
    /// Each layout, by root, folder and disc ID.
    pub discs: Vec<Disc>,
}

/// The params of `corrections.lookup`. It names one album: a release, or a
/// folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct LookupParams {
    /// The command's ID, made by the client.
    pub idempotency_key: Id,
    /// A release, by its ID: the discs of the folders its files are in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub album: Option<Id>,
    /// A folder, by its path under its root: its discs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub folder: Option<String>,
    /// The root the folder is in, by its name. Needed only when more than
    /// one root has a folder of that path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub root: Option<String>,
    /// Ask again about a disc that has a reply already.
    #[serde(default)]
    pub again: bool,
}

/// What became of one disc a lookup was about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum LookupOutcome {
    /// It was asked about, and the reply names releases.
    Replied,
    /// It was asked about, and the catalogue does not know it.
    NotFound,
    /// It had a reply already, and was not asked about again.
    Kept,
    /// Asking failed. Nothing was kept for it.
    Failed,
}

/// One album a reply names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct CatalogueAlbum {
    /// Its MusicBrainz release group ID.
    pub release_group: String,
    /// Its title.
    pub title: String,
    /// Who it is credited to, as written.
    pub artist: String,
    /// The year it was first released, when the catalogue knows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub first_year: Option<u32>,
    /// How many of the releases the reply names are of this album.
    pub releases: u32,
}

/// One disc a lookup was about, and what its current reply says.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct DiscLookup {
    /// The disc ID asked about.
    pub disc_id: String,
    /// What became of it.
    pub outcome: LookupOutcome,
    /// Why asking failed: one line that can be shown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub failure: Option<String>,
    /// The albums its current reply names, if it has one.
    pub albums: Vec<CatalogueAlbum>,
}

/// The result of `corrections.lookup`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct LookupDone {
    /// The receipt.
    #[serde(flatten)]
    pub receipt: Receipt,
    /// Each disc, in the order asked.
    pub discs: Vec<DiscLookup>,
}

/// What a yes to a candidate would change, worked out cheaply. A preview
/// says exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum ProposalChange {
    /// Only an ID.
    Knows,
    /// A kind or a first year as well.
    Gains,
    /// Files join or part: an album title, a credit or a disc number.
    Moves,
}

/// The result of `corrections.summary`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct CorrectionsSummary {
    /// The library revision it read.
    pub library_revision: u64,
    /// Whether the library may be asked about at MusicBrainz.
    pub lookups: bool,
    /// Open proposals.
    pub open: u64,
    /// How many groups they make.
    pub groups: u64,
    /// Of the open proposals, those whose likeliest candidate moves files.
    pub moves: u64,
    /// Those that add a kind or a first year.
    pub gains: u64,
    /// Those that add only an ID.
    pub knows: u64,
}

/// An album a proposal's disc may be on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ProposalCandidate {
    /// Its MusicBrainz release group ID.
    pub release_group: String,
    /// Its title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub title: Option<String>,
    /// Who it is credited to.
    pub artist: String,
    /// Its kind, such as `Album`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub primary_type: Option<String>,
    /// The year it first came out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub first_year: Option<u32>,
    /// How many of the replies' releases are of it.
    pub releases: u32,
    /// What a yes would change.
    pub change: ProposalChange,
    /// The values a yes would put on the files. Only from
    /// `corrections.get`; empty in a list.
    pub values: Vec<FileValue>,
}

/// One tag value a yes would put on one file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct FileValue {
    /// The file's ID.
    pub file_id: Id,
    /// The root the file is in, by its name.
    pub root: String,
    /// The file, by its path under its root.
    pub path: String,
    /// The field, named as tags are read: `album`, `album_artist`,
    /// `mb_album_artist`, `disc_number`, `mb_release_group`,
    /// `primary_type` or `original_date`.
    pub field: String,
    /// Its position among the field's values, from 0.
    pub ordinal: u32,
    /// The value, or nothing when the yes clears the field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub value: Option<String>,
    /// What the file says for it now, as the catalog reads it. Only from
    /// `corrections.preview`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub now: Option<String>,
}

/// What a proposal rests on: one disc, by its layout and the reply about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ProposalEvidence {
    /// The disc ID.
    pub disc_id: String,
    /// How its layout was found.
    pub methods: Vec<LayoutMethod>,
    /// How many releases the current reply about it names.
    pub releases: u32,
    /// When that reply came, as Unix milliseconds.
    pub replied_at_ms: u64,
}

/// One proposal: that one disc of the library is one album.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct Proposal {
    /// Its ID. Worked out again, the same proposal keeps it.
    pub proposal_id: Id,
    /// Its kind: `album`.
    pub kind: String,
    /// The proposal its group is named by. Proposals about the same album
    /// share it, and are shown and answered together.
    pub group_id: Id,
    /// The root its folder is in, by its name.
    pub root: String,
    /// The folder, by its path under its root.
    pub folder: String,
    /// How many files it is about.
    pub file_count: u32,
    /// Its candidates, the likeliest first.
    pub candidates: Vec<ProposalCandidate>,
    /// A digest of all of it, which a yes names.
    pub fingerprint: String,
    /// The files, by their paths. Only from `corrections.get`.
    pub files: Vec<String>,
    /// What it rests on. Only from `corrections.get`.
    pub rests_on: Vec<ProposalEvidence>,
}

/// The result of `corrections.list`: every open proposal, by group, then
/// root and folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ProposalList {
    /// The library revision it read.
    pub library_revision: u64,
    /// Each proposal, without its files, evidence or values.
    pub proposals: Vec<Proposal>,
}

/// The params of `corrections.get`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ProposalGetParams {
    /// The proposal.
    pub proposal_id: Id,
}

/// The result of `corrections.get`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ProposalGot {
    /// The library revision it read.
    pub library_revision: u64,
    /// The proposal, whole.
    pub proposal: Proposal,
}

/// One proposal with the candidate to preview it with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct Choice {
    /// The proposal.
    pub proposal_id: Id,
    /// The candidate, by its release group ID. Left out, the likeliest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub release_group: Option<String>,
}

/// The params of `corrections.preview`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct PreviewParams {
    /// The proposals, each once with its candidate, previewed together as
    /// one. Choices whose values disagree for one field of one file are
    /// refused.
    #[serde(default)]
    pub choices: Vec<Choice>,
    /// Or answers, to preview their undo: what it would give back, and
    /// which answers resting on them it would undo too. Not with
    /// `choices`.
    #[serde(default)]
    pub undo: Vec<Id>,
}

/// One album as a preview shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct AlbumShape {
    /// The title its tile shows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub title: Option<String>,
    /// The credit its tile shows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub artist: Option<String>,
    /// Its kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub primary_type: Option<String>,
    /// The year it first came out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub first_year: Option<u32>,
    /// How many releases it holds.
    pub releases: u32,
    /// How many discs its fullest release has.
    pub discs: u32,
    /// How many songs are on it.
    pub songs: u32,
}

/// The result of `corrections.preview`: the albums the files are on now,
/// with everything else on them, and the albums they would be on. Nothing
/// is written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct PreviewShown {
    /// The library revision it read.
    pub library_revision: u64,
    /// The albums now.
    pub now: Vec<AlbumShape>,
    /// The albums after.
    pub after: Vec<AlbumShape>,
    /// For an undo: every answer it would undo, those asked for and those
    /// resting on them, oldest first. Empty for choices.
    pub undoes: Vec<Id>,
    /// How many songs would be joined into another: copies of one song,
    /// such as two editions' of it, that are two songs now and would be one.
    pub songs_joined: u32,
    /// How many songs would be parted from one they are now: copies that
    /// are one song now and would be two.
    pub songs_parted: u32,
    /// The values the choices would put on the files.
    pub values: Vec<FileValue>,
}

/// A yes to one proposal's candidate, naming the proposal as it was shown.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct Confirmation {
    /// The proposal.
    pub proposal_id: Id,
    /// The candidate, by its release group ID.
    pub release_group: String,
    /// The proposal's fingerprint as it was shown. A proposal that has
    /// changed since is refused, and nothing is written.
    pub fingerprint: String,
}

/// The params of `corrections.confirm`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ConfirmParams {
    /// The command's ID, made by the client.
    pub idempotency_key: Id,
    /// Each yes, all kept in one write or none.
    pub answers: Vec<Confirmation>,
}

/// One answer a command wrote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct AnswerWritten {
    /// The answer's ID.
    pub answer_id: Id,
    /// The proposal a yes answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub proposal_id: Option<Id>,
    /// The answer an undo undid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub undoes: Option<Id>,
}

/// The result of `corrections.confirm` and `corrections.undo`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct Answered {
    /// The receipt. Every answer is at its revision.
    #[serde(flatten)]
    pub receipt: Receipt,
    /// Each answer written.
    pub answers: Vec<AnswerWritten>,
    /// Whether anything shown changed: an album, a song or an artist.
    pub moved: bool,
}

/// The params of `corrections.undo`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct UndoParams {
    /// The command's ID, made by the client.
    pub idempotency_key: Id,
    /// The answers to undo, all in one write or none.
    pub answers: Vec<Id>,
}

/// What an answer said.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum AnswerVerb {
    /// Yes to a proposal's candidate.
    Confirm,
    /// The undo of an earlier answer.
    Undo,
}

/// One answer in the ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct Answer {
    /// Its ID.
    pub answer_id: Id,
    /// What it said.
    pub verb: AnswerVerb,
    /// For a yes: the proposal it answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub proposal_id: Option<Id>,
    /// For a yes: the album it said yes to, by its release group ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub release_group: Option<String>,
    /// For a yes: the album's title, as it was shown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub title: Option<String>,
    /// For an undo: the answer it undid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub undoes: Option<Id>,
    /// For a yes that was undone: the undo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub undone_by: Option<Id>,
    /// How many files it put values on.
    pub files: u32,
    /// When it was given, as Unix milliseconds.
    pub answered_at_ms: u64,
}

/// The params of `corrections.answers`: one page of the ledger.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct AnswersParams {
    /// The answers older than this one; from the newest when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub before: Option<Id>,
    /// How many at most: 50 when left out, and never more than 500.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub limit: Option<u32>,
}

/// The result of `corrections.answers`: one page of the ledger, newest
/// first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct AnswerList {
    /// The library revision it read.
    pub library_revision: u64,
    /// Each answer, newest first.
    pub answers: Vec<Answer>,
    /// The `before` that reads the next page, while there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub next: Option<Id>,
}

/// The params of `corrections.answer`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct AnswerParams {
    /// The answer.
    pub answer_id: Id,
}

/// An album a yes was given among, as it was shown: all of it for the one
/// said yes to, its ID and title for the others.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct AnsweredCandidate {
    /// Its release group ID.
    pub release_group: String,
    /// Its title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub title: Option<String>,
    /// Who it is credited to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub artist: Option<String>,
    /// Its kind: `album`, `single`, `ep`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub primary_type: Option<String>,
    /// When it first came out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub first_date: Option<String>,
    /// How many of the replies' releases were of it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub releases: Option<u32>,
}

/// A disc a yes rested on: its disc ID, and when the reply it was read
/// from came.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct AnsweredReply {
    /// The disc ID.
    pub disc_id: String,
    /// When MusicBrainz replied, as Unix milliseconds.
    pub replied_at_ms: u64,
}

/// A layout of a disc a yes rested on, as it was then.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct AnsweredLayout {
    /// The disc ID it gives.
    pub disc_id: String,
    /// The folder it is of, by its path under its root.
    pub folder: String,
    /// Each way it was found, in the order found.
    pub methods: Vec<LayoutMethod>,
    /// Whether it may not be the disc's.
    pub provisional: bool,
}

/// What an answer rested on and said, kept by value when it was given.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct AnswerEvidence {
    /// For a yes: the album said yes to, as it was shown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub candidate: Option<AnsweredCandidate>,
    /// For a yes: the albums it was chosen among, in the order shown.
    pub candidates: Vec<AnsweredCandidate>,
    /// For a yes: each disc it rested on, with its reply.
    pub replies: Vec<AnsweredReply>,
    /// For a yes: each layout of those discs.
    pub layouts: Vec<AnsweredLayout>,
    /// For a yes: the values it put on the files, each with the file as it
    /// is named now.
    pub values: Vec<FileValue>,
    /// For an undo of an answer that rested on others: the answers it was
    /// undone with.
    pub rests_on: Vec<Id>,
}

/// The result of `corrections.answer`: one answer, and what it rested on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct AnswerGot {
    /// The library revision it read.
    pub library_revision: u64,
    /// The answer.
    pub answer: Answer,
    /// What it rested on and said.
    pub evidence: AnswerEvidence,
}
