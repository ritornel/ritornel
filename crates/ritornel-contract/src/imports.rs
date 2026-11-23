//! Records imported from an archive a service gave the listener: today, a
//! stream of Spotify's extended streaming history. See "Spotify streams" in
//! docs/internals/history.md.

use serde::{Deserialize, Serialize};

use crate::{DeviceKind, Id, Job, Platform, PlayedVia, Receipt};

/// One row of Spotify's extended streaming history, as Ritornel keeps it.
///
/// It is the whole of what is taken from a row. The row's IP address, user
/// agent, account name and country, its platform as text, and any field
/// this type does not name are never read into it, and the type refuses a
/// field it does not name, so nothing else can ride along with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SpotifyStream {
    /// When the stream ended, in Unix milliseconds, to the second.
    pub ended_at: i64,
    /// How long it played.
    pub ms_played: i64,
    /// The track, as `spotify:track:` and its ID.
    pub track_uri: String,
    /// The track's title.
    pub title: String,
    /// The artist, as Spotify named it.
    pub artist: String,
    /// The album, as Spotify named it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub album: Option<String>,
    /// Why it began, as Spotify says it, such as `clickrow`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub reason_start: Option<String>,
    /// Why it ended, as Spotify says it, such as `trackdone`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub reason_end: Option<String>,
    /// Whether shuffle was on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub shuffle: Option<bool>,
    /// Spotify's own mark that the track was skipped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub skipped: Option<bool>,
    /// Whether it played offline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub offline: Option<bool>,
    /// When offline mode was used, as the row gives it. Its unit is not
    /// documented, so it is kept as given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub offline_timestamp: Option<i64>,
    /// Whether it was played in a private session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub incognito: Option<bool>,
    /// The operating system of the device that played it, when the row
    /// says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub platform: Option<Platform>,
    /// The hardware the sound came out of, when the row says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub device_kind: Option<DeviceKind>,
    /// How that device played it, when the row says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub via: Option<PlayedVia>,
}

/// The latest end a stream may have: the start of 2100, in Unix
/// milliseconds.
pub const LATEST_STREAM_END: i64 = 4_102_444_800_000;

/// The longest a stream may have played: a day.
pub const LONGEST_STREAM_MS: i64 = 24 * 60 * 60 * 1000;

impl SpotifyStream {
    /// Whether its times are ones a stream can have: an end from 1970 to
    /// 2100 and a time played from nothing to a day, so that no sum or
    /// difference of two overflows. `Err` names the field that is not.
    pub fn plausible(&self) -> Result<(), &'static str> {
        if !(0..LATEST_STREAM_END).contains(&self.ended_at) {
            return Err("ts");
        }
        if !(0..=LONGEST_STREAM_MS).contains(&self.ms_played) {
            return Err("ms_played");
        }
        Ok(())
    }
}

/// The most streams one piece of a staged run holds. A stream is about
/// four hundred bytes on the wire, so a piece stays well under a message's
/// limit of 1 MiB.
pub const MAX_STREAMS_PER_PIECE: usize = 1_000;

/// The params of `imports.stage`: one piece of a run of streams read from
/// an archive, which the hub keeps until the run is dry run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImportsStageParams {
    /// The command's ID, made by the client.
    pub idempotency_key: Id,
    /// The run, made by the client for its first piece and named by every
    /// piece after.
    pub run_id: Id,
    /// Which piece this is, from 0.
    pub piece: u32,
    /// How many pieces the run has.
    pub pieces: u32,
    /// Its streams, at most [`MAX_STREAMS_PER_PIECE`].
    pub streams: Vec<SpotifyStream>,
}

/// The result of `imports.stage`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ImportsStaged {
    /// The receipt. It names no revision: the library is not written.
    #[serde(flatten)]
    pub receipt: Receipt,
    /// The run as the hub now holds it.
    pub run: ImportRun,
}

/// The params of `imports.dryRun`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ImportsDryRunParams {
    /// The command's ID, made by the client.
    pub idempotency_key: Id,
    /// The run.
    pub run_id: Id,
}

/// The params of `imports.get`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ImportsGetParams {
    /// The run.
    pub run_id: Id,
}

/// The result of `imports.dryRun`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ImportsDryRunStarted {
    /// The receipt.
    #[serde(flatten)]
    pub receipt: Receipt,
    /// The job that works the dry run out. Its report is the run's, from
    /// `imports.get`, once the job has succeeded.
    pub job: Job,
}

/// A run of streams staged on the hub. It is removed a day after it was
/// begun, and when the hub starts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ImportRun {
    /// The run.
    pub run_id: Id,
    /// How many pieces it has.
    pub pieces: u32,
    /// How many of them the hub holds.
    pub received: u32,
    /// The streams in those pieces.
    pub streams: u64,
    /// When the hub removes it.
    pub expires_at: i64,
    /// The last dry run's report, once one has finished.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub report: Option<ImportReport>,
}

/// A rule that joins a scrobble to a stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum StreamTier {
    /// The same names, stamped within ten seconds of the stream's start.
    Start,
    /// The same names, stamped while the stream played.
    During,
    /// The artist or the title renamed, stamped as in `start`.
    Renamed,
}

/// What a tier joins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct TierJoins {
    /// The tier.
    pub tier: StreamTier,
    /// Whether it is in force: whether applying would join what it finds.
    pub in_force: bool,
    /// Scrobbles it joins as a stream's own.
    pub joined: u64,
    /// Scrobbles it joins as a second report of a stream that has one.
    pub second: u64,
}

/// What a dry run found for one year, by each stream's worked-out start in
/// UTC. The joins and the plays after are by the tiers in force.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ImportYear {
    /// The year.
    pub year: i32,
    /// Listens Spotify reported: streams, each repeat counted once.
    pub streams: u64,
    /// Of those, the ones joined to a scrobble by tier A.
    pub joined_start: u64,
    /// By tier B.
    pub joined_during: u64,
    /// By tier C.
    pub joined_renamed: u64,
    /// By a correction that joins them.
    pub joined_pinned: u64,
    /// With no scrobble, and counted by the play rule.
    pub alone_counted: u64,
    /// With no scrobble, and not counted: skipped or partial.
    pub alone_not_counted: u64,
    /// With no scrobble, and no length for its track to judge it by.
    pub alone_unknown: u64,
    /// Scrobbles of the year that no stream took, of those offered.
    pub scrobbles_alone: u64,
    /// Plays of the year now, by their start in UTC.
    pub plays_before: u64,
    /// Plays of the year once applied.
    pub plays_after: u64,
    /// Of the plays now, the ones that count.
    pub counted_before: u64,
    /// Of the plays once applied, the ones that count.
    pub counted_after: u64,
    /// Streams that start where no zone the listener was in covers, so
    /// that their local date is unknown.
    pub no_zone: u64,
}

/// One join a dry run shows for judging a tier: both records' names and how
/// far apart they were.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct JoinSample {
    /// The tier that joined them.
    pub tier: StreamTier,
    /// Whether the scrobble is a second report.
    pub second: bool,
    /// The stream's artist and title.
    pub stream_artist: String,
    /// See `stream_artist`.
    pub stream_title: String,
    /// The scrobble's artist and title.
    pub scrobble_artist: String,
    /// See `scrobble_artist`.
    pub scrobble_title: String,
    /// The scrobble's stamp less the stream's worked-out start.
    pub from_start_ms: i64,
    /// How long the stream played.
    pub ms_played: i64,
}

/// What stands in the way of applying a run, and what to do about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum ImportProblem {
    /// Streams fall where no zone covers: they would have no local date.
    /// Say where the listener was then (`history.setZones`).
    NoZone,
    /// A tier finds joins and is not yet in force: what it would join
    /// stands as plays of their own until it is.
    TierNotInForce,
}

/// What a dry run found: what the run holds, and what applying it would
/// do, by year. Nothing was written to find it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    /// When it was worked out.
    pub made_at: i64,
    /// The grouper's version, which the stream matcher is part of.
    pub grouper_version: i64,
    /// Streams in the run, repeats included.
    pub streams: u64,
    /// Listens they report: each repeat counted once.
    pub events: u64,
    /// Scrobbles offered to the streams: Last.fm's, less those set aside
    /// and those that are this library's own listens come back.
    pub scrobbles: u64,
    /// Scrobbles set aside, which no stream may take.
    pub scrobbles_excluded: u64,
    /// Scrobbles that are this library's own listens come back.
    pub scrobbles_echoes: u64,
    /// What each tier joins.
    pub tiers: Vec<TierJoins>,
    /// By year.
    pub years: Vec<ImportYear>,
    /// Streams played offline, and of those, the ones with the time
    /// offline mode was used.
    pub offline: u64,
    /// See `offline`.
    pub offline_timestamped: u64,
    /// Of those, the ones whose offline time reads as seconds near the
    /// stream's end, and as milliseconds near it; the rest are neither.
    pub offline_seconds: u64,
    /// See `offline_seconds`.
    pub offline_milliseconds: u64,
    /// Scrobbles no stream took, stamped between the stream before and the
    /// worked-out start of a stream of the same names that has no
    /// scrobble: where a pause would put them.
    pub paused: u64,
    /// What stands in the way of applying.
    pub problems: Vec<ImportProblem>,
    /// A sample of the joins of each tier not in force, for judging it.
    pub samples: Vec<JoinSample>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn stream() -> serde_json::Value {
        json!({
            "endedAt": 1_552_684_449_000_i64,
            "msPlayed": 221_000,
            "trackUri": "spotify:track:0000000000000000000001",
            "title": "Low Tide",
            "artist": "Calla Brandt",
            "album": "Glasshouse",
            "reasonStart": "trackdone",
            "reasonEnd": "trackdone",
            "shuffle": false,
            "skipped": false,
            "offline": false,
            "incognito": false,
            "platform": "ios",
            "deviceKind": "phone",
            "via": "app",
        })
    }

    #[test]
    fn a_stream_reads_and_writes_the_same() {
        let wire = stream();
        let read: SpotifyStream = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            (read.platform, read.device_kind, read.via),
            (
                Some(Platform::Ios),
                Some(DeviceKind::Phone),
                Some(PlayedVia::App)
            )
        );
        assert_eq!(serde_json::to_value(read).unwrap(), wire);
    }

    #[test]
    fn a_stream_refuses_any_field_it_does_not_name() {
        for field in ["ipAddr", "userAgentDecrypted", "username", "connCountry"] {
            let mut wire = stream();
            wire[field] = json!("anything");
            assert!(
                serde_json::from_value::<SpotifyStream>(wire).is_err(),
                "{field}"
            );
        }
        // A word it does not know is read as `other`, and never repeated.
        for field in ["platform", "deviceKind", "via"] {
            let mut wire = stream();
            wire[field] = json!("iOS 17.1 (MARKED_DEVICE)");
            let read: SpotifyStream = serde_json::from_value(wire).unwrap();
            let written = serde_json::to_string(&read).unwrap();
            assert!(!written.contains("MARKED"), "{written}");
            assert!(written.contains("\"other\""), "{written}");
        }
    }
}
