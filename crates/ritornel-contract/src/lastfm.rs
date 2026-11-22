//! Last.fm: signing in, the import of a history as a standing sync, the
//! report of how much of it matched the library, and scrobbling. Also the
//! zones the listener was in by date, which dates an imported play, and one
//! track as `ritornelctl show` gives it. See "Last.fm" in
//! docs/internals/history.md.
//!
//! A server built without Last.fm answers each `lastfm.*` call with
//! `unavailable`; `lastfm.status` says so in `available`.

use serde::{Deserialize, Serialize};

use crate::{Id, Job, JobState, Receipt};

/// The result of `lastfm.status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct LastfmStatus {
    /// Whether this server was built with Last.fm.
    pub available: bool,
    /// Whether it has an API key and secret, from its environment or its
    /// data directory. The values are never sent.
    pub configured: bool,
    /// The account it is signed in to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub account: Option<String>,
    /// Last.fm refused the session: sign in again.
    #[serde(default)]
    pub sign_in_again: bool,
    /// Plays from this time on are scrobbled. Left out while scrobbling is
    /// off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub scrobbling_since: Option<i64>,
    /// Since when nothing is scrobbled until an import from Last.fm has
    /// run, because the library was restored from a backup and Last.fm may
    /// already have plays it holds waiting. The hub imports by itself; the
    /// plays waiting go once it has. Left out when nothing waits for that.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub waiting_for_import_since: Option<i64>,
    /// Scrobbles recorded, by state: `pending`, `sending`, `accepted`,
    /// `ignored`, `refused` or `unknown`.
    #[serde(default)]
    pub deliveries: Vec<DeliveryCount>,
    /// Plays imported from the account.
    #[serde(default)]
    pub imported: u64,
    /// Whether the listener has a zone for any date, without which an
    /// imported play has no local date. A sync is refused until they have.
    #[serde(default)]
    pub zones_set: bool,
    /// When the history was last imported, and when it next is.
    #[serde(default)]
    pub import: ImportSchedule,
}

/// When the account's history is imported. Only the library's hub imports:
/// when it starts, when a client such as the desktop app opens and the
/// last import ended more than an hour before, and six hours after the
/// last import ended. A sync by hand counts as an import.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ImportSchedule {
    /// The last sync of the history to end, started by hand or on its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub last: Option<LastImport>,
    /// Whether a sync is queued or running now.
    #[serde(default)]
    pub running: bool,
    /// When the server next starts one on its own. Left out while one runs
    /// and while none will start on its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub next: Option<i64>,
    /// From this time on, a client opening starts one. Left out as `next`
    /// is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub on_open_after: Option<i64>,
    /// Why none starts on its own, when none will: this server is not the
    /// hub, has no API key, is not signed in, was refused its session, or
    /// the listener has no zones.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub off: Option<String>,
}

/// How the last sync of a history ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct LastImport {
    /// When it ended.
    pub ended: i64,
    /// `succeeded`, `failed` or `cancelled`.
    pub state: JobState,
    /// Why it failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub failure: Option<String>,
}

/// How many scrobbles are in one state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct DeliveryCount {
    /// The state.
    pub state: String,
    /// How many.
    pub count: u64,
}

/// The result of `lastfm.signInStart`: where to approve the sign-in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SignInStarted {
    /// The sign-in, for `lastfm.signInFinish`.
    pub sign_in: Id,
    /// The address at Last.fm where the listener approves it. It carries
    /// the API key and a one-hour token, as Last.fm's desktop flow has it.
    pub approval_url: String,
}

/// The params of `lastfm.signInFinish`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SignInFinishParams {
    /// The command's ID, made by the client.
    pub idempotency_key: Id,
    /// The sign-in `lastfm.signInStart` began.
    pub sign_in: Id,
}

/// The result of `lastfm.signInFinish`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SignInFinished {
    /// The receipt: `accepted` while the listener has not approved yet,
    /// and the call is made again; `done` once the session is kept.
    #[serde(flatten)]
    pub receipt: Receipt,
    /// The account signed in to, once approved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub account: Option<String>,
}

/// The params of a command that takes nothing but its key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct KeyParams {
    /// The command's ID, made by the client.
    pub idempotency_key: Id,
}

/// The result of `lastfm.signOut`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SignedOut {
    /// The receipt.
    #[serde(flatten)]
    pub receipt: Receipt,
    /// Whether there was a session to forget.
    pub was_signed_in: bool,
}

/// The params of `lastfm.sync`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct LastfmSyncParams {
    /// The command's ID, made by the client.
    pub idempotency_key: Id,
    /// Read the whole history again rather than what is new. What is
    /// stored already writes nothing.
    #[serde(default)]
    pub full: bool,
}

/// The result of `lastfm.sync`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct LastfmSyncStarted {
    /// The receipt.
    #[serde(flatten)]
    pub receipt: Receipt,
    /// The job that syncs. A sync that is running already is returned.
    pub job: Job,
}

/// What a sync of an account's history read and stored.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SyncSummary {
    /// Scrobbles read.
    pub read: u64,
    /// Of those, the ones that were new.
    pub new: u64,
    /// Scrobbles stored before that Last.fm no longer lists. They stay.
    pub gone: u64,
    /// How many scrobbles Last.fm counts in the account's whole history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub source_total: Option<u64>,
    /// How many this library holds, once the sync has ended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub stored: Option<u64>,
    /// Tracks loved now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub loved: Option<u64>,
    /// Tracks whose binding changed when every track was judged again at
    /// the end: newly bound, moved to another recording, or released.
    #[serde(default)]
    pub matched_after: u64,
}

/// The params of `lastfm.setScrobbling`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ScrobblingParams {
    /// The command's ID, made by the client.
    pub idempotency_key: Id,
    /// On, for plays from now on, or off.
    pub on: bool,
}

/// The result of `lastfm.setScrobbling`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ScrobblingSet {
    /// The receipt.
    #[serde(flatten)]
    pub receipt: Receipt,
    /// Plays from this time on are scrobbled. Left out when it is off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub scrobbling_since: Option<i64>,
}

/// The params of `lastfm.report`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct LastfmReportParams {
    /// How many doubtful tracks to list, at most 1000. The default is 50.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub limit: Option<u32>,
}

/// A track a doubtful scrobble nearly matched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct NearlyMatched {
    /// Its title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub title: Option<String>,
    /// Its artist credit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub artist: Option<String>,
}

/// A scrobbled track the matcher was not sure of.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct DoubtfulTrack {
    /// The artist, as Last.fm spells it.
    pub artist: String,
    /// The album.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub album: Option<String>,
    /// The title.
    pub title: String,
    /// How often it was played.
    pub plays: u64,
    /// Why it was not bound: `several`, when the artist and title name
    /// several recordings the album cannot tell apart; `album`, when they
    /// name one and the album is none it is on, so the play may be of
    /// another version; or `near`, when nothing matches exactly but
    /// something does without accents and bracketed notes.
    pub doubt: String,
    /// What it nearly matched.
    pub candidates: Vec<NearlyMatched>,
}

/// The result of `lastfm.report`: how much of the imported history matched
/// the library.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct LastfmReport {
    /// The account.
    pub account: String,
    /// Scrobbles imported.
    pub imported: u64,
    /// Plays they make, the echoes of plays scrobbled from here not
    /// counted.
    pub plays: u64,
    /// Of those, the ones bound to a recording.
    pub matched_plays: u64,
    /// Scrobbles that are echoes of plays scrobbled from here.
    pub echoes: u64,
    /// Distinct tracks the history names.
    pub tracks: u64,
    /// Of those, the ones bound.
    pub matched_tracks: u64,
    /// Tracks the matcher was not sure of.
    pub doubtful_tracks: u64,
    /// Their plays.
    pub doubtful_plays: u64,
    /// Tracks nothing in the library comes close to.
    pub absent_tracks: u64,
    /// Their plays.
    pub absent_plays: u64,
    /// The doubtful tracks, most played first.
    pub doubtful: Vec<DoubtfulTrack>,
    /// Tracks loved now.
    pub loved: u64,
    /// Of those, the ones bound.
    pub loved_matched: u64,
    /// The first and the last play imported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub first: Option<i64>,
    /// See `first`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub last: Option<i64>,
}

/// One stretch of time the listener spent in one zone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ZonePeriod {
    /// The zone, by its IANA name, such as `Europe/London`.
    pub zone: String,
    /// The first day, as `YYYY-MM-DD` in that zone. Left out for every day
    /// before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub from: Option<String>,
    /// The day after the last, as `YYYY-MM-DD` in that zone. Left out for
    /// every day after.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub until: Option<String>,
}

/// The result of `history.zones`. Where periods overlap, the one that
/// began last holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ZonePeriods {
    /// The periods.
    pub periods: Vec<ZonePeriod>,
}

/// The params of `history.setZones`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SetZonesParams {
    /// The command's ID, made by the client.
    pub idempotency_key: Id,
    /// Every period, in place of what there was.
    pub periods: Vec<ZonePeriod>,
}

/// The result of `history.setZones`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ZonesSet {
    /// The receipt.
    #[serde(flatten)]
    pub receipt: Receipt,
    /// Plays whose date or zone changed.
    pub changed: u64,
}

/// The params of `tracks.get`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct TrackGetParams {
    /// The track.
    pub track_id: Id,
}

/// A love that reaches a track.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct TrackLove {
    /// Who said it: a service and account, as `lastfm:name`, or `you`.
    pub by: String,
    /// When.
    pub at: i64,
}

/// The result of `tracks.get`: one track, its plays and its loves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct TrackDetail {
    /// The track.
    pub track_id: Id,
    /// Its recording.
    pub recording_id: Id,
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
    /// Plays of its recording that count, from every source.
    pub plays: u64,
    /// The last of them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub last_played: Option<i64>,
    /// The loves in force.
    pub loves: Vec<TrackLove>,
}
