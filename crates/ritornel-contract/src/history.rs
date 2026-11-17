//! Reading history: recent listening, "on this day", and the rebuild of
//! the derived tables. See "Queries M1 must answer" and "Rebuild" in
//! docs/design/history.md.
//!
//! History in M1 is meant to be read through `list.open` with `kind:
//! "history"` (docs/design/contract.md). Until list views are built, these
//! two direct calls read it, for `ritornelctl history`.
//!
//! What a page says of your history with an artist or an album is one call
//! each, `history.artist` and `history.album`, answered with a
//! [`HistoryOf`]: the sentence in its banner and the plays by year for its
//! picture (docs/design/ui.md, "History in the interface"). What Lean back
//! says about the song that is playing is `history.track`, answered with
//! a [`TrackHistory`].

use serde::{Deserialize, Serialize};

use crate::{AlbumTile, Id, Receipt};

/// What became of a play.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum PlayOutcome {
    /// It counts.
    Listened,
    /// It does not count, and the listener moved on.
    Skipped,
    /// It does not count, and the listener did not move on.
    Partial,
    /// Nothing says.
    Unknown,
}

/// What a play was of: its track as the library shows it. For a play whose
/// file is bound to no track yet, the fields are those of the track that
/// was queued, and the IDs are left out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct PlayedTrack {
    /// The appearance the play is bound to. Left out while the file that
    /// was heard is bound to no track.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub appearance_id: Option<Id>,
    /// The recording the play is bound to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub recording_id: Option<Id>,
    /// The title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub title: Option<String>,
    /// The artist credit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub artist: Option<String>,
    /// The album.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub album: Option<String>,
}

/// One listen behind a play.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct PlayedListen {
    /// The listen.
    pub listen_id: Id,
    /// When it began, by the playing server's clock.
    pub started_at: i64,
    /// When it ended, by the same clock.
    pub ended_at: i64,
    /// Why it began: `user_pick`, `auto_advance`, `repeat` or `resume`.
    pub start_reason: String,
    /// Why it ended: `finished`, `next`, `previous`, `jump`, `replaced`,
    /// `removed`, `stopped`, `error`, `output_lost`, `shutdown` or `crash`.
    pub end_reason: String,
    /// Where in the track it began.
    pub start_position_ms: u64,
    /// Where it ended.
    pub end_position_ms: u64,
    /// Audio actually rendered.
    pub ms_played: u64,
    /// The listen it continues after a restart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub continues: Option<Id>,
    /// The zone the playing server was in at its start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub zone: Option<String>,
}

/// One play.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct Play {
    /// The play. Its ID is computed, and a client that meets one it does
    /// not know reads again.
    pub play_id: Id,
    /// When it began. Left out for a play with no date.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub at: Option<i64>,
    /// Its local date, as `YYYY-MM-DD`, in `zone`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub local_date: Option<String>,
    /// Its local hour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub local_hour: Option<u32>,
    /// The zone its local date is in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub zone: Option<String>,
    /// That zone's offset from UTC then, in minutes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub utc_offset_minutes: Option<i32>,
    /// Which clock its time rests on: `hub_age` or `server_wall` for a
    /// listen, `source_utc` for a play another service reported.
    pub clock_basis: String,
    /// What it was of.
    pub track: PlayedTrack,
    /// Audio rendered, over every listen of it. Left out for a play
    /// another service reported, whose played time is not known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub ms_played: Option<u64>,
    /// The track's length.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub duration_ms: Option<u64>,
    /// What became of it.
    pub outcome: PlayOutcome,
    /// Whether it counts, by the play rule.
    pub counts: bool,
    /// Whether it is excluded.
    pub excluded: bool,
    /// The listens behind it, the first first.
    pub listens: Vec<PlayedListen>,
}

/// Where a page of `history.recent` ended: its last play's instant and
/// ID. The next page is asked for with it, and begins with the play after
/// that one, even one of the same instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct PlayCursor {
    /// The play's instant. Left out for a play with no date.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub at: Option<i64>,
    /// The play's ID.
    pub play_id: Id,
}

/// The params of `history.recent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct HistoryRecentParams {
    /// How many plays, at most 500. The default is 20.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub limit: Option<u32>,
    /// Only plays after this one, newest first: the `next` of the page
    /// before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub before: Option<PlayCursor>,
}

/// The result of `history.recent`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct RecentPlays {
    /// The plays, newest first.
    pub plays: Vec<Play>,
    /// Where the next page begins, when this one was full.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub next: Option<PlayCursor>,
    /// Listens this server has finished and not yet delivered to history.
    pub undelivered: u64,
}

/// The params of `history.onThisDay`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct OnThisDayParams {
    /// The day, as `MM-DD`. The default is today, where the server is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub day: Option<String>,
}

/// The result of `history.onThisDay`: the plays that count on that day of
/// any year, newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct OnThisDay {
    /// The day, as `MM-DD`.
    pub day: String,
    /// The plays.
    pub plays: Vec<Play>,
}

/// The params of `history.rebuild`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct HistoryRebuildParams {
    /// The command's ID, made by the client.
    pub idempotency_key: Id,
    /// Say what would change, and change nothing.
    #[serde(default)]
    pub dry_run: bool,
}

/// The result of `history.rebuild`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct HistoryRebuilt {
    /// The receipt.
    #[serde(flatten)]
    pub receipt: Receipt,
    /// Whether nothing was changed.
    pub dry_run: bool,
    /// Plays before.
    pub before: u64,
    /// Plays after.
    pub after: u64,
    /// Plays that are new, gone or different.
    pub changed: u64,
}

/// A run of words in a sentence, set apart where it is `strong`: the facts
/// a sentence turns on, a month or a count, are.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct Phrase {
    /// The words.
    pub text: String,
    /// Whether they are set apart.
    pub strong: bool,
}

/// How many plays fell in one year, by their local dates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct YearPlays {
    /// The year.
    pub year: i32,
    /// The plays.
    pub plays: u64,
}

/// The years a listener's history runs over: from the year of their first
/// dated play to this one. Every picture of years is drawn across it, so
/// that a year stands in the same place on every page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct YearSpan {
    /// The first year.
    pub from: i32,
    /// This year.
    pub to: i32,
}

/// One of the records of an artist that was played most.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct RecordPlayed {
    /// The record, as an Albums tile shows it.
    #[serde(flatten)]
    pub tile: AlbumTile,
    /// Its plays.
    pub plays: u64,
    /// The year it was played most: the first such, when two tie.
    pub peak_year: i32,
}

/// The result of `history.artist` and `history.album`: what a page says of
/// the listener's history with an artist or an album.
///
/// It counts the plays that count and are not excluded, of songs the
/// library has bound them to: an artist's are the plays of their songs, by
/// each song's credit; an album's are those heard from one of its
/// versions. A play bound to no song reaches no page (docs/design/history.md,
/// "Last.fm"), so this is read again whenever plays change: a binding made
/// later moves them here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct HistoryOf {
    /// The plays, dated or not.
    pub plays: u64,
    /// The local date of the first dated play, as `YYYY-MM-DD`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub first_played: Option<String>,
    /// The local date of the last.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub last_played: Option<String>,
    /// The plays in the 365 days before now.
    pub last_year: u64,
    /// The dated plays by year, oldest first. A year with none is left out.
    pub years: Vec<YearPlays>,
    /// The years the listener's whole history runs over. Left out while
    /// there is no dated play at all, of anything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub span: Option<YearSpan>,
    /// For an artist, the records played most, most first, at most six. For
    /// an album, none.
    pub records: Vec<RecordPlayed>,
    /// The one sentence a page says of it, in its banner. Empty when there
    /// is nothing true to say, which is when there are no plays.
    pub sentence: Vec<Phrase>,
    /// The library revision this was read at.
    pub library_revision: u64,
}

/// The params of `history.track`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct TrackHistoryParams {
    /// The track: the appearance that is playing, which names its song and
    /// its record.
    pub appearance_id: Id,
}

/// What a fact Lean back shows is about, so that a client can tell them
/// apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum FactKind {
    /// How many times the song was played, and when last.
    SongPlays,
    /// When the record was first heard.
    FirstHeard,
    /// The year that held most of the record's plays.
    ItsYear,
    /// The time of day the record is mostly played at.
    TimeOfDay,
    /// The record was played on this day in an earlier year.
    OnThisDay,
    /// The record's song that is played most.
    PlayedMost,
    /// The record had not been played for over a year before today.
    NotSince,
}

/// One thing worth saying about what is playing, as Lean back sets it: a
/// line in large type and a smaller one under it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct Fact {
    /// What it is about.
    pub kind: FactKind,
    /// The line in large type, with what it turns on set apart.
    pub head: Vec<Phrase>,
    /// The line under it. Left out when there is none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub more: Option<String>,
}

/// The result of `history.track`: what Lean back says about the song that
/// is playing and its record, one fact at a time (docs/design/ui.md, "Lean
/// back"; the prototype's `leanFacts`).
///
/// It counts as a page does: the plays that count and are not excluded, of
/// songs the library has bound them to. The song's are the plays of its
/// recording, wherever it was heard; the record's are those heard from one
/// of its versions. A song with no such plays has no facts, and a client
/// shows its title and artist alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct TrackHistory {
    /// The facts, in the order they are shown.
    pub facts: Vec<Fact>,
    /// The library revision this was read at.
    pub library_revision: u64,
}

/// One layer of the picture of a listener's history: the artists first
/// heard in one year, and how much they were played in each month after.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct StreamLayer {
    /// The year its artists were first heard.
    pub year: i32,
    /// Their plays in each month of the history, the first first.
    pub plays: Vec<u64>,
    /// In each month, the artists of it played most, at most three, most
    /// first, by their place in [`HistoryStream::artists`].
    pub most: Vec<Vec<u32>>,
}

/// The result of `history.stream`: the listener's whole history as the
/// History tab draws it, a stream across the months in layers by the year
/// each artist was first heard (docs/design/ui.md, "History in the
/// interface").
///
/// It counts every dated play that counts and is not excluded, bound to the
/// library or not: History is the record of all of it. A play is the
/// artist's its source named, and an artist is known by their name,
/// whatever its case. A day of more than 300 plays is a device catching up,
/// not listening, and is left out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct HistoryStream {
    /// The first month, as `YYYY-MM`. Left out while there is no history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub from: Option<String>,
    /// How many months there are, from the first to this one.
    pub months: u32,
    /// All the plays in each month.
    pub total: Vec<u64>,
    /// The layers, the oldest year first.
    pub layers: Vec<StreamLayer>,
    /// The artists the layers name.
    pub artists: Vec<String>,
    /// The days left out, as devices catching up.
    pub days_left_out: u64,
    /// The library revision this was read at.
    pub library_revision: u64,
}
