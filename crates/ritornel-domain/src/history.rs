//! From observations to plays: history derivation for listens, the play
//! rule, and the time a play is dated at. See "Observations and derivation"
//! and "Time and time zones" in docs/internals/history.md.
//!
//! Derivation is a pure function of the observations, the corrections, the
//! listener's zone periods, the current bindings and the policy versions.
//! It currently runs four of the six derivation stages:
//!
//! 1. **Bind.** Each chain takes the recording and appearance of its first
//!    listen's source item, through the item's current assignment. No
//!    assignment gives an unbound play.
//! 2. **Chain.** Listens linked by `continues` are joined when the later one
//!    starts within 24 hours of the end of the earlier one, and a chain is
//!    judged as one: its time from the first listen, its played time
//!    summed.
//! 5. **Judge.** [`PlayRule`] decides whether a chain counts, and the
//!    outcome follows from that and from how the last listen ended.
//! 6. **Exclude.** A play with an observation an active `exclude` targets
//!    is marked excluded.
//!
//! Plays another service reported, such as a Last.fm scrobble, are imports
//! ([`Import`]). Each is a play of its own, which counts by the source's
//! verdict and is dated in the listener's zone for that date, except the
//! echo of a play this library sent there itself: it joins that play, matched
//! exactly through the record of what was sent, with no window of time.
//! Grouping across sources by a window (step 3) and rank (step 4) arrive with
//! a second source that reports the same plays. The other correction verbs
//! are not applied yet: no call records them.
//!
//! Local dates come from the IANA time zone database built into this crate
//! (jiff's bundled copy), never from the machine's, so a derivation gives
//! the same answer wherever it runs.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use jiff::Timestamp;
use jiff::tz::{TimeZone, TimeZoneDatabase};

/// The grouper's version. Grouper 1 joins `continues` chains and groups
/// nothing across sources. Version 2 uses known zones and the full clock
/// difference when dating a chain, and bounds its accumulated play time.
/// Version 3 reads Spotify's streams, and joins a scrobble to the stream it
/// is the report of by the stream matcher, which is part of the grouper: a
/// change to the matcher is a new version, which derives every play again
/// and refuses a dry run made before it. Version 4 pairs same-second
/// returning echoes 1-to-1 per `(endpoint, listen)` across distinct
/// delivered listens.
/// Version 5 joins explicit continuations between players.
pub const GROUPER_VERSION: i64 = 5;

/// How long after the end of a listen one that continues it may start and
/// still be joined to it.
pub const CHAIN_WINDOW_MS: i64 = 24 * 60 * 60 * 1000;

/// A clock skew at or below this is not corrected for: the two clocks are
/// as good as the same.
pub const SKEW_TOLERANCE_MS: i64 = 5_000;

/// How far a time dated by its age from the hub's clock may be off: the
/// time the send took, which is not measured.
pub const HUB_AGE_PRECISION_MS: i64 = 1_000;

/// How far a time a source gives to the second may be off.
pub const SOURCE_SECOND_PRECISION_MS: i64 = 1_000;

/// How far a time dated by the sender's wall clock may be off, once a skew
/// above [`SKEW_TOLERANCE_MS`] has been taken out: the skew that was let
/// stand, and whatever the clock was wrong by when the listen was made.
pub const SERVER_WALL_PRECISION_MS: i64 = SKEW_TOLERANCE_MS;

/// The rule that says whether a play counts. See "Observations and
/// derivation" in docs/internals/history.md: the rule is versioned, and a
/// play stores the version that judged it, so a new rule is a rebuild.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlayRule {
    /// The version stored on each play it judges.
    pub version: i64,
    /// A track must be longer than this to count at all.
    pub longer_than_ms: i64,
    /// The share of the track that must be played, as a fraction.
    pub share_numerator: i64,
    /// See `share_numerator`.
    pub share_denominator: i64,
    /// Playing this much counts whatever the track's length.
    pub enough_ms: i64,
}

impl PlayRule {
    /// Play rule 1, Last.fm's: the track is over 30 seconds and at least
    /// half of it or 240 seconds was played.
    pub const V1: Self = Self {
        version: 1,
        longer_than_ms: 30_000,
        share_numerator: 1,
        share_denominator: 2,
        enough_ms: 240_000,
    };

    /// Play rule 2: rule 1 for a listen, and a stream with no scrobble
    /// judged by the length its track's finished streams give it
    /// (`streams::judge_alone`).
    pub const V2: Self = Self {
        version: 2,
        ..Self::V1
    };

    /// The rule in force.
    pub const CURRENT: Self = Self::V2;

    /// Whether `ms_played` of a track `track_ms` long counts.
    pub fn counts(&self, ms_played: i64, track_ms: i64) -> bool {
        let share = i128::from(ms_played) * i128::from(self.share_denominator)
            >= i128::from(track_ms) * i128::from(self.share_numerator);
        track_ms > self.longer_than_ms && (share || ms_played >= self.enough_ms)
    }
}

/// Defines an enum whose values are the text the schema writes.
macro_rules! text_enum {
    ($(#[$doc:meta])* $name:ident { $($(#[$vdoc:meta])* $variant:ident => $text:literal,)* }) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum $name {
            $($(#[$vdoc])* $variant,)*
        }

        impl $name {
            /// Every value.
            pub const ALL: &[Self] = &[$(Self::$variant,)*];

            /// The value as the schema writes it.
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $text,)*
                }
            }

            /// The value the schema writes as `text`.
            pub fn parse(text: &str) -> Option<Self> {
                Self::ALL.iter().copied().find(|value| value.as_str() == text)
            }
        }
    };
}

text_enum! {
    /// Why a listen began. See "Recording a listen" in
    /// docs/internals/playback.md.
    StartReason {
        /// Someone chose it: play, a skip, or playing again after a stop.
        UserPick => "user_pick",
        /// The track before it ended and the player moved on.
        AutoAdvance => "auto_advance",
        /// Repeat one started the entry again.
        Repeat => "repeat",
        /// Playback carried on after the server restarted.
        Resume => "resume",
    }
}

text_enum! {
    /// Why a listen ended, as the player reports it.
    EndReason {
        /// The track played to its end.
        Finished => "finished",
        /// Skipped forward.
        Next => "next",
        /// Skipped back.
        Previous => "previous",
        /// Another entry of the queue was chosen.
        Jump => "jump",
        /// The queue was replaced.
        Replaced => "replaced",
        /// The entry was taken out of the queue.
        Removed => "removed",
        /// The player was stopped.
        Stopped => "stopped",
        /// The track could not be played on.
        Error => "error",
        /// The output went away in the middle of the track.
        OutputLost => "output_lost",
        /// The server stopped cleanly.
        Shutdown => "shutdown",
        /// The server died, and the next run closed the listen.
        Crash => "crash",
    }
}

impl EndReason {
    /// Whether the listener moved on to something else: a play that does
    /// not count and ended this way is a skip.
    pub const fn moved_on(self) -> bool {
        matches!(
            self,
            Self::Next | Self::Previous | Self::Jump | Self::Replaced | Self::Removed
        )
    }
}

text_enum! {
    /// Why a track was queued.
    ContextKind {
        /// By hand.
        Manual => "manual",
        /// As part of an album.
        Album => "album",
        /// From an artist's page.
        Artist => "artist",
        /// From a playlist.
        Playlist => "playlist",
        /// From a list query.
        Query => "query",
        /// By an agent.
        Agent => "agent",
    }
}

text_enum! {
    /// What became of a play.
    Outcome {
        /// It counts.
        Listened => "listened",
        /// It does not count, and the listener moved on.
        Skipped => "skipped",
        /// It does not count, and the listener did not move on.
        Partial => "partial",
        /// Nothing says.
        Unknown => "unknown",
    }
}

text_enum! {
    /// Which clock a play's instant rests on.
    ClockBasis {
        /// Its age, from the hub's own clock.
        HubAge => "hub_age",
        /// The playing server's wall clock, less the measured skew.
        ServerWall => "server_wall",
        /// A device's wall clock.
        DeviceWall => "device_wall",
        /// A device time with an explicit offset.
        DeviceOffset => "device_offset",
        /// A time the source gave in UTC.
        SourceUtc => "source_utc",
        /// No clock.
        None => "none",
    }
}

text_enum! {
    /// Where a zone came from.
    ZoneBasis {
        /// The source knew it.
        Captured => "captured",
        /// The listener's zone for that date.
        Assumed => "assumed",
        /// Only an offset is known.
        OffsetOnly => "offset_only",
        /// None is known.
        None => "none",
    }
}

text_enum! {
    /// Where a play's local date came from.
    CalendarBasis {
        /// The instant, in the play's zone.
        Zone => "zone",
        /// A device's own wall date.
        DeviceWall => "device_wall",
        /// The instant, at an explicit offset.
        Offset => "offset",
        /// None is known.
        None => "none",
    }
}

text_enum! {
    /// What the source's time is the time of.
    TimeAnchor {
        /// The start.
        Start => "start",
        /// The end.
        End => "end",
        /// It has none.
        None => "none",
    }
}

text_enum! {
    /// What decided whether a play counts.
    CountBasis {
        /// The play rule, from the time played.
        PlayRule => "play_rule",
        /// The source's own verdict: it counted the play itself.
        SourceVerdict => "source_verdict",
        /// A device's position at the end.
        DevicePosition => "device_position",
    }
}

text_enum! {
    /// What an observation is to a play.
    Role {
        /// It supplies the time and the outcome: for a chain, its first
        /// listen.
        Primary => "primary",
        /// It supports the play: for a chain, every listen after the first.
        Corroborating => "corroborating",
    }
}

text_enum! {
    /// How an observation that is not a play's primary came to join it.
    Joined {
        /// A listen that carries on the one before it.
        Chain => "chain",
        /// The scrobble a listen of this library's came back as.
        Echo => "echo",
        /// A stream that repeats the one that speaks for the play.
        Repeat => "repeat",
        /// A scrobble joined to a stream by tier A of the stream matcher.
        Start => "start",
        /// By tier B.
        During => "during",
        /// By tier C.
        Renamed => "renamed",
        /// By a correction that joined them.
        Pin => "pin",
    }
}

text_enum! {
    /// What a correction does.
    Verb {
        /// The play does not count.
        Exclude => "exclude",
        /// The plays were another listener's.
        SetListener => "set_listener",
        /// These are not the same listen.
        Split => "split",
        /// These are the same listen.
        Join => "join",
        /// The endpoint's clock was off.
        SetClockOffset => "set_clock_offset",
        /// The targeted observations were of another recording, or of none.
        SetRecording => "set_recording",
        /// Undo another correction.
        Revoke => "revoke",
    }
}

/// The clocks an observation was stamped with when it was sent and taken
/// in. See "Clocks and monotonic age dating" in docs/internals/history.md.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Received {
    /// The hub's clock when it took the observation in.
    pub received_at_ms: i64,
    /// The sender's monotonic clock at the send.
    pub sent_mono_ms: Option<i64>,
    /// The sender's boot at the send.
    pub sent_boot: Option<[u8; 16]>,
    /// The sender's wall clock less the hub's, measured at the send.
    pub clock_skew_ms: Option<i64>,
}

/// One listen, as derivation reads it: its envelope and its detail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listen {
    /// The observation's row.
    pub observation: i64,
    /// The endpoint that reported it.
    pub endpoint: i64,
    /// Its key at that endpoint: the listen's ID.
    pub source_key: Vec<u8>,
    /// Whose listen it is, as claimed at ingest.
    pub listener: i64,
    /// The source item of the file that was heard.
    pub source_item: i64,
    /// How it was sent and taken in.
    pub received: Received,
    /// The zone the sender captured at the start, by its IANA name.
    pub zone: Option<String>,
    /// The sender's wall clock at the start.
    pub started_at_ms: i64,
    /// The sender's wall clock at the end.
    pub ended_at_ms: i64,
    /// The sender's monotonic clock at the start.
    pub started_mono_ms: i64,
    /// The same clock at the end, when the listen kept it.
    pub ended_mono_ms: Option<i64>,
    /// The boot of the machine that clock counts from.
    pub boot: [u8; 16],
    /// The key of the listen this one resumes, possibly on another player.
    pub continues: Option<Vec<u8>>,
    /// Audio actually rendered.
    pub ms_played: i64,
    /// The track's length.
    pub track_duration_ms: i64,
    /// How it ended.
    pub end_reason: EndReason,
    /// Why it was queued.
    pub context_kind: ContextKind,
    /// What it was queued from, by public ID.
    pub context_ref: Option<[u8; 16]>,
    /// Whether it was played in a shuffled order.
    pub shuffle: bool,
}

/// One play another service reported: an import record, as derivation
/// reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Import {
    /// The observation's row.
    pub observation: i64,
    /// The endpoint that reported it.
    pub endpoint: i64,
    /// Whose play it is, as claimed at ingest.
    pub listener: i64,
    /// The text-only source item that stands for what it was of.
    pub source_item: i64,
    /// When the source dates it, in UTC.
    pub played_at_ms: i64,
    /// The listens whose play this may be the echo of: deliveries this
    /// library sent to the source, recorded before they were sent, and
    /// matched exactly on their time and names, ordered by listen
    /// observation. Empty for a play of the source's own.
    pub echo_of: Vec<i64>,
    /// Its key at its endpoint, which orders it among ties.
    pub key: Vec<u8>,
    /// The artist and the title, as the source spelled them.
    pub artist: String,
    /// See `artist`.
    pub title: String,
}

/// One of Spotify's streams in force, as derivation reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamImport {
    /// The observation's row.
    pub observation: i64,
    /// The endpoint that reported it.
    pub endpoint: i64,
    /// Whose play it is.
    pub listener: i64,
    /// The source item of its track.
    pub source_item: i64,
    /// Its key at its endpoint.
    pub key: Vec<u8>,
    /// The stream as it was kept.
    pub record: ritornel_contract::SpotifyStream,
}

/// What a source item is bound to now: its current assignment, with the
/// recording read through its redirects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Binding {
    /// The canonical recording.
    pub recording: i64,
    /// The appearance the assignment names.
    pub appearance: Option<i64>,
}

/// The zone a listener was in over a range of time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZonePeriod {
    /// Whose.
    pub listener: i64,
    /// The zone, by its IANA name.
    pub zone: String,
    /// From when, as Unix milliseconds. `None` is open.
    pub from_ms: Option<i64>,
    /// Until when, not included. `None` is open.
    pub to_ms: Option<i64>,
}

/// A time window on one endpoint, optionally narrowed to one source item.
/// Bounds are half-open `[from_ms, to_ms)` compared against the time the
/// observation's own source reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    /// The endpoint whose observations the window covers.
    pub endpoint: i64,
    /// Earliest reported instant, inclusive. `None` is open.
    pub from_ms: Option<i64>,
    /// Latest reported instant, exclusive. `None` is open.
    pub to_ms: Option<i64>,
    /// Narrow to observations of this source item, when set.
    pub source_item: Option<i64>,
}

impl Window {
    /// Whether an observation reported on `endpoint` for `source_item` at
    /// `reported_at_ms` falls in this window.
    pub fn matches(&self, endpoint: i64, source_item: i64, reported_at_ms: i64) -> bool {
        self.endpoint == endpoint
            && self.source_item.is_none_or(|item| item == source_item)
            && self.from_ms.is_none_or(|from| reported_at_ms >= from)
            && self.to_ms.is_none_or(|to| reported_at_ms < to)
    }
}

/// What a `set_recording` correction says its target observations were of:
/// `Some(binding)` binds them to that canonical recording (and optional
/// canonical appearance); `None` leaves them unbound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordingOverride {
    /// The binding in force for the matched observations.
    pub binding: Option<Binding>,
}

/// One correction: an observation of its own, with what it targets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Correction {
    /// The correction's observation.
    pub observation: i64,
    /// What it does.
    pub verb: Verb,
    /// The observations it targets: for `revoke`, the corrections it
    /// undoes.
    pub targets: Vec<i64>,
    /// A target by endpoint and time range, when not listed in `targets`.
    pub window: Option<Window>,
    /// For `set_recording`, the recording override it applies.
    pub recording: Option<RecordingOverride>,
}

/// Everything derivation reads.
#[derive(Debug, Clone, Copy)]
pub struct Inputs<'a> {
    /// The listens to derive plays from. A `continues` chain is joined only
    /// if every listen of it is here.
    pub listens: &'a [Listen],
    /// The imports to derive plays from. An echo joins its listen's play
    /// only if that listen is here.
    pub imports: &'a [Import],
    /// The current binding of each source item that has one.
    pub bindings: &'a HashMap<i64, Binding>,
    /// Every correction.
    pub corrections: &'a [Correction],
    /// The zones listeners were in.
    pub zone_periods: &'a [ZonePeriod],
    /// The play rule in force.
    pub rule: PlayRule,
    /// Spotify's streams in force. A scrobble joins one when the stream
    /// matcher pairs them, and only when both are here.
    pub streams: &'a [StreamImport],
    /// Each track's length as its streams give it, by track URI, for
    /// judging a stream that has no scrobble.
    pub lengths: &'a HashMap<String, i64>,
}

/// A date and an hour on a wall clock somewhere, and the offset from UTC
/// they were read at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalTime {
    /// The date as YYYYMMDD.
    pub date: i64,
    /// The month and day as MMDD, for "on this day".
    pub month_day: i64,
    /// The hour, 0 to 23.
    pub hour: i64,
    /// The day of the week, 0 for Sunday.
    pub weekday: i64,
    /// The offset from UTC, in minutes.
    pub offset_minutes: i64,
}

/// When a play was, as derivation dates it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayTime {
    /// The start, as Unix milliseconds in UTC.
    pub event_at_ms: Option<i64>,
    /// What the source's time is the time of.
    pub anchor: TimeAnchor,
    /// Which clock the instant rests on.
    pub clock_basis: ClockBasis,
    /// How far the instant may be off.
    pub precision_ms: Option<i64>,
    /// The zone the local date is read in.
    pub zone: Option<String>,
    /// Where that zone came from.
    pub zone_basis: ZoneBasis,
    /// The local date and hour.
    pub local: Option<LocalTime>,
    /// Where the local date came from.
    pub calendar_basis: CalendarBasis,
}

/// One play, derived.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Play {
    /// The observations that support it, the primary first.
    pub observations: Vec<(i64, Role)>,
    /// Whose play it is.
    pub listener: i64,
    /// The recording, when the source item is bound.
    pub recording: Option<i64>,
    /// The appearance, when the binding names one.
    pub appearance: Option<i64>,
    /// The source item of the file that was heard first, or of what an
    /// import names.
    pub source_item: i64,
    /// When.
    pub time: PlayTime,
    /// Audio rendered, over the whole chain. Unknown for an import.
    pub ms_played: Option<i64>,
    /// The track's length, where it is known.
    pub track_duration_ms: Option<i64>,
    /// What became of it.
    pub outcome: Outcome,
    /// Whether it counts.
    pub counts: bool,
    /// What decided that.
    pub count_basis: CountBasis,
    /// The version of the rule that judged it.
    pub play_rule_version: i64,
    /// The version of the grouper that formed it.
    pub grouper_version: i64,
    /// Whether an active `exclude` targets one of its observations.
    pub excluded: bool,
    /// Why its first listen was queued. Unknown for an import.
    pub context_kind: Option<ContextKind>,
    /// What it was queued from.
    pub context_ref: Option<[u8; 16]>,
    /// Whether it was played in a shuffled order. Unknown for a scrobble.
    pub shuffle: Option<bool>,
    /// How each observation that is not the primary joined it.
    pub joined: Vec<(i64, Joined)>,
}

/// The zone database every local date is read from.
fn database() -> &'static TimeZoneDatabase {
    static DATABASE: std::sync::OnceLock<TimeZoneDatabase> = std::sync::OnceLock::new();
    DATABASE.get_or_init(TimeZoneDatabase::bundled)
}

/// The zone of this IANA name, if the database knows it.
fn zone(name: &str) -> Option<TimeZone> {
    database().get(name).ok()
}

/// Whether the zone database knows this IANA name.
pub fn known_zone(name: &str) -> bool {
    zone(name).is_some()
}

/// The instant a day began in a zone: midnight of `date` (YYYYMMDD) there,
/// as Unix milliseconds, or the first instant of the day where midnight was
/// skipped. `None` for a date or a zone there is not.
pub fn start_of_day_ms(date: i64, name: &str) -> Option<i64> {
    let zone = zone(name)?;
    let (year, month, day) = (
        i16::try_from(date / 10_000).ok()?,
        i8::try_from(date / 100 % 100).ok()?,
        i8::try_from(date % 100).ok()?,
    );
    let day = jiff::civil::Date::new(year, month, day).ok()?;
    let start = day.to_zoned(zone).ok()?.start_of_day().ok()?;
    Some(start.timestamp().as_millisecond())
}

/// Whether a zone says nothing about where its listener was: UTC under any
/// of its names, which is what a server in a container reports when nobody
/// set its zone.
pub fn says_nothing(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    let name = name.strip_prefix("etc/").unwrap_or(&name);
    matches!(
        name,
        "utc"
            | "uct"
            | "universal"
            | "zulu"
            | "gmt"
            | "gmt0"
            | "gmt+0"
            | "gmt-0"
            | "greenwich"
            | "unknown"
    )
}

/// The wall clock in the zone `name` at `utc_ms`. `None` when the
/// database does not know the zone, or the instant is out of range.
pub fn local_time(utc_ms: i64, name: &str) -> Option<LocalTime> {
    let zone = zone(name)?;
    let instant = Timestamp::from_millisecond(utc_ms).ok()?;
    let zoned = instant.to_zoned(zone);
    let date = zoned.date();
    let (year, month, day) = (
        i64::from(date.year()),
        i64::from(date.month()),
        i64::from(date.day()),
    );
    Some(LocalTime {
        date: year * 10_000 + month * 100 + day,
        month_day: month * 100 + day,
        hour: i64::from(zoned.hour()),
        weekday: i64::from(date.weekday().to_sunday_zero_offset()),
        offset_minutes: i64::from(zoned.offset().seconds()) / 60,
    })
}

/// When a listen started, by the hub's reckoning. See "Clocks and monotonic
/// age dating" in docs/internals/history.md.
///
/// Sent in the boot it was heard in, a listen is dated by its age: the
/// hub's clock at receipt less the time the sender's monotonic clock says
/// passed since the start. The sender's wall clock plays no part, so one
/// that is wrong does not matter. Otherwise the sender's wall clock is
/// used, shifted by the skew measured at the send when that is over
/// [`SKEW_TOLERANCE_MS`], which is right only if the clock was wrong by the
/// same amount when the listen was made.
pub fn date_listen(listen: &Listen) -> (i64, ClockBasis, i64) {
    let received = &listen.received;
    if let (Some(boot), Some(sent_mono)) = (received.sent_boot, received.sent_mono_ms)
        && boot == listen.boot
        && sent_mono >= listen.started_mono_ms
    {
        let age = sent_mono.abs_diff(listen.started_mono_ms);
        return (
            received.received_at_ms.saturating_sub_unsigned(age),
            ClockBasis::HubAge,
            HUB_AGE_PRECISION_MS,
        );
    }
    let skew = received.clock_skew_ms.unwrap_or(0);
    let shift = if skew.unsigned_abs() > SKEW_TOLERANCE_MS.unsigned_abs() {
        skew
    } else {
        0
    };
    (
        listen.started_at_ms.saturating_sub(shift),
        ClockBasis::ServerWall,
        SERVER_WALL_PRECISION_MS,
    )
}

/// The zone `listener` was in at `at_ms`, by their zone periods: of those
/// that hold then, the one that began last, which is the most particular.
pub fn assumed_zone(listener: i64, at_ms: i64, periods: &[ZonePeriod]) -> Option<&ZonePeriod> {
    periods
        .iter()
        .filter(|period| {
            period.listener == listener
                && period.from_ms.is_none_or(|from| from <= at_ms)
                && period.to_ms.is_none_or(|to| at_ms < to)
        })
        .max_by_key(|period| (period.from_ms, period.to_ms.map(|to| -to)))
}

/// Prefer a recognised zone from the sender. When it is missing, unknown
/// or UTC, use the listener's zone for that date. A captured UTC is the
/// fallback when the listener has no zone for the date.
fn zone_of(
    listen: &Listen,
    event_at_ms: i64,
    periods: &[ZonePeriod],
) -> (Option<String>, ZoneBasis) {
    let captured = listen.zone.as_ref().filter(|name| known_zone(name));
    if let Some(captured) = captured
        && !says_nothing(captured)
    {
        return (Some(captured.clone()), ZoneBasis::Captured);
    }
    if let Some(period) = assumed_zone(listen.listener, event_at_ms, periods) {
        return (Some(period.zone.clone()), ZoneBasis::Assumed);
    }
    match captured {
        Some(captured) => (Some(captured.clone()), ZoneBasis::Captured),
        None => (None, ZoneBasis::None),
    }
}

/// When an import was: the time its source gives, in UTC, read in the zone
/// the listener was in then. With no zone known for that date it has an
/// instant and no local date.
fn import_time(import: &Import, periods: &[ZonePeriod]) -> PlayTime {
    let zone = assumed_zone(import.listener, import.played_at_ms, periods);
    let local = zone.and_then(|period| local_time(import.played_at_ms, &period.zone));
    PlayTime {
        event_at_ms: Some(import.played_at_ms),
        anchor: TimeAnchor::Start,
        clock_basis: ClockBasis::SourceUtc,
        precision_ms: Some(SOURCE_SECOND_PRECISION_MS),
        zone: zone.map(|period| period.zone.clone()),
        zone_basis: if zone.is_some() {
            ZoneBasis::Assumed
        } else {
            ZoneBasis::None
        },
        calendar_basis: if local.is_some() {
            CalendarBasis::Zone
        } else {
            CalendarBasis::None
        },
        local,
    }
}

/// When a play whose first listen is `head` was.
fn play_time(head: &Listen, periods: &[ZonePeriod]) -> PlayTime {
    let (event_at_ms, clock_basis, precision_ms) = date_listen(head);
    let (zone, zone_basis) = zone_of(head, event_at_ms, periods);
    let local = zone
        .as_deref()
        .and_then(|name| local_time(event_at_ms, name));
    PlayTime {
        event_at_ms: Some(event_at_ms),
        anchor: TimeAnchor::Start,
        clock_basis,
        precision_ms: Some(precision_ms),
        zone,
        zone_basis,
        calendar_basis: if local.is_some() {
            CalendarBasis::Zone
        } else {
            CalendarBasis::None
        },
        local,
    }
}

/// The corrections that are active: a correction is active unless an active
/// `revoke` targets it, so revoking a revoke restores what it revoked. A
/// revoke can only name a correction that exists before it, so going from
/// the newest down settles each one before anything it could undo.
pub fn active_corrections(corrections: &[Correction]) -> BTreeSet<i64> {
    let mut by_newest: Vec<&Correction> = corrections.iter().collect();
    by_newest.sort_by_key(|correction| std::cmp::Reverse(correction.observation));
    let mut revoked = BTreeSet::new();
    let mut active = BTreeSet::new();
    for correction in by_newest {
        if revoked.contains(&correction.observation) {
            continue;
        }
        active.insert(correction.observation);
        if correction.verb == Verb::Revoke {
            revoked.extend(correction.targets.iter().copied());
        }
    }
    active
}

/// The observations that active `exclude` corrections target.
pub fn excluded(corrections: &[Correction]) -> BTreeSet<i64> {
    let active = active_corrections(corrections);
    corrections
        .iter()
        .filter(|correction| correction.verb == Verb::Exclude)
        .filter(|correction| active.contains(&correction.observation))
        .flat_map(|correction| correction.targets.iter().copied())
        .collect()
}

/// Active `set_recording` corrections, newest first, so the first match
/// wins.
pub fn active_recording_overrides(corrections: &[Correction]) -> Vec<&Correction> {
    let active = active_corrections(corrections);
    let mut overrides: Vec<&Correction> = corrections
        .iter()
        .filter(|correction| {
            correction.verb == Verb::SetRecording
                && correction.recording.is_some()
                && active.contains(&correction.observation)
        })
        .collect();
    overrides.sort_by_key(|correction| std::cmp::Reverse(correction.observation));
    overrides
}

/// Indexed active `set_recording` overrides: O(1) lookup for explicit
/// observation targets, plus active window corrections newest first.
#[derive(Debug)]
pub struct RecordingOverrides<'a> {
    by_target: HashMap<i64, (i64, RecordingOverride)>,
    windows: Vec<&'a Correction>,
}

impl<'a> RecordingOverrides<'a> {
    /// Builds the lookup index once from all corrections.
    pub fn build(corrections: &'a [Correction]) -> Self {
        let overrides = active_recording_overrides(corrections);
        let mut by_target: HashMap<i64, (i64, RecordingOverride)> = HashMap::new();
        let mut windows = Vec::new();
        for correction in overrides {
            let Some(chosen) = correction.recording else {
                continue;
            };
            // `overrides` is sorted newest first, so the first entry for a
            // target wins.
            for &target in &correction.targets {
                by_target
                    .entry(target)
                    .or_insert((correction.observation, chosen));
            }
            if correction.window.is_some() {
                windows.push(correction);
            }
        }
        Self { by_target, windows }
    }

    /// The newest active `set_recording` correction that matches an
    /// observation, returning `(correction_observation_id, override)`.
    pub fn for_observation(
        &self,
        observation: i64,
        endpoint: i64,
        source_item: i64,
        reported_at_ms: i64,
    ) -> Option<(i64, RecordingOverride)> {
        let mut best = self.by_target.get(&observation).copied();
        for correction in &self.windows {
            if best.is_some_and(|(best_id, _)| best_id >= correction.observation) {
                break;
            }
            if correction
                .window
                .is_some_and(|window| window.matches(endpoint, source_item, reported_at_ms))
                && let Some(chosen) = correction.recording
            {
                best = Some((correction.observation, chosen));
                break;
            }
        }
        best
    }
}

/// When a listen began and ended, on a timeline that does not take the
/// sender's wall clock at its word: its date ([`date_listen`]), and that
/// date plus how long it lasted by its monotonic clock, or by its wall
/// clock where it did not keep the monotonic clock's end.
fn span(listen: &Listen) -> (i64, i64) {
    let (start, _, _) = date_listen(listen);
    let lasted = match listen.ended_mono_ms {
        Some(end) => end.saturating_sub(listen.started_mono_ms),
        None => listen.ended_at_ms.saturating_sub(listen.started_at_ms),
    };
    (start, start.saturating_add(lasted.max(0)))
}

/// How long after `earlier` ended `later` began, and how far the two
/// clocks it was measured on can be out: the time the machine's monotonic
/// clock counted between them when both were heard in one boot and the
/// first kept its end, which is exact; otherwise the time between their
/// spans, which is as good as their dates.
fn gap(earlier: &Listen, later: &Listen) -> (i64, i64) {
    if earlier.endpoint == later.endpoint
        && earlier.boot == later.boot
        && let Some(end) = earlier.ended_mono_ms
    {
        return (later.started_mono_ms.saturating_sub(end), 0);
    }
    let ((_, ended), (began, _)) = (span(earlier), span(later));
    (began.saturating_sub(ended), SKEW_TOLERANCE_MS)
}

/// What decides which of two listens is the earlier: when it began, on
/// the timeline of [`span`], then the key.
fn order(listen: &Listen) -> (i64, &[u8]) {
    (span(listen).0, listen.source_key.as_slice())
}

/// The listens joined into chains, each as indexes into `listens`, first
/// listen first. Chains are in the order of their first listen's
/// observation.
///
/// A listen joins the one it continues on the same endpoint, or on a different
/// endpoint for the same listener, when the earlier listen ended no later than it began and at most
/// [`CHAIN_WINDOW_MS`] before: by the monotonic clock when both were heard
/// in one boot, and otherwise by their dates, within what those dates can
/// be out. The sender's wall clock decides none of it, so a clock set back
/// or forward between two listens neither splits a chain nor joins one. Of
/// two listens that continue the same one, the earlier joins and the other
/// begins a chain of its own. Listens that continue one another round in a
/// loop, which only bad keys can make, are cut before the earliest of
/// them. Every rule looks only at the listens, never at the order they
/// arrived in.
pub fn chains(listens: &[Listen]) -> Vec<Vec<usize>> {
    let by_key: HashMap<(i64, &[u8]), usize> = listens
        .iter()
        .enumerate()
        .map(|(index, listen)| ((listen.endpoint, listen.source_key.as_slice()), index))
        .collect();
    // IDs are minted by players. Keep endpoint-local identity when available,
    // and only follow a key across endpoints when its owner is unambiguous.
    let mut across = HashMap::new();
    for (index, listen) in listens.iter().enumerate() {
        across
            .entry((listen.listener, listen.source_key.as_slice()))
            .and_modify(|candidate| *candidate = None)
            .or_insert(Some(index));
    }
    // The successor each listen keeps, by the earliest claimant.
    let mut next: BTreeMap<usize, usize> = BTreeMap::new();
    let mut has_previous = vec![false; listens.len()];
    let mut claimants: Vec<(usize, usize)> = listens
        .iter()
        .enumerate()
        .filter_map(|(index, listen)| {
            let key = listen.continues.as_deref()?;
            let previous = by_key
                .get(&(listen.endpoint, key))
                .copied()
                .or_else(|| across.get(&(listen.listener, key)).copied().flatten())?;
            let earlier = listens.get(previous)?;
            let (after, slack) = gap(earlier, listen);
            let joins = previous != index && after >= -slack && after <= CHAIN_WINDOW_MS;
            joins.then_some((previous, index))
        })
        .collect();
    claimants.sort_by_key(|(_, index)| listens.get(*index).map(order));
    let mut previous_of: BTreeMap<usize, usize> = BTreeMap::new();
    for (previous, index) in claimants {
        if let std::collections::btree_map::Entry::Vacant(slot) = next.entry(previous) {
            slot.insert(index);
            previous_of.insert(index, previous);
            if let Some(flag) = has_previous.get_mut(index) {
                *flag = true;
            }
        }
    }
    // Each listen has one successor and one predecessor at most, so what
    // no chain reaches from a listen without a predecessor is a loop. Each
    // loop is cut before its earliest listen, which begins its chain.
    let mut reached = vec![false; listens.len()];
    for start in 0..listens.len() {
        if !has_previous.get(start).copied().unwrap_or(false) {
            let mut at = Some(start);
            while let Some(index) = at {
                if let Some(seen) = reached.get_mut(index) {
                    *seen = true;
                }
                at = next.get(&index).copied();
            }
        }
    }
    for start in 0..listens.len() {
        if reached.get(start).copied().unwrap_or(true) {
            continue;
        }
        let mut members = vec![start];
        let mut at = next.get(&start).copied();
        while let Some(index) = at.filter(|index| *index != start) {
            members.push(index);
            at = next.get(&index).copied();
        }
        let earliest = members
            .iter()
            .copied()
            .min_by_key(|index| listens.get(*index).map(order));
        if let Some(earliest) = earliest {
            if let Some(previous) = previous_of.remove(&earliest) {
                next.remove(&previous);
            }
            if let Some(flag) = has_previous.get_mut(earliest) {
                *flag = false;
            }
        }
        for member in members {
            if let Some(seen) = reached.get_mut(member) {
                *seen = true;
            }
        }
    }
    let mut heads: Vec<usize> = (0..listens.len())
        .filter(|index| !has_previous.get(*index).copied().unwrap_or(false))
        .collect();
    heads.sort_by_key(|index| listens.get(*index).map(|listen| listen.observation));
    heads
        .into_iter()
        .map(|head| {
            let mut chain = vec![head];
            let mut at = head;
            while let Some(&following) = next.get(&at) {
                chain.push(following);
                at = following;
            }
            chain
        })
        .collect()
}

/// Judges a chain whose listens played `ms_played` of a track `track_ms`
/// long, and whose last listen ended for `ended`.
pub fn judge(rule: &PlayRule, ms_played: i64, track_ms: i64, ended: EndReason) -> (bool, Outcome) {
    let counts = rule.counts(ms_played, track_ms);
    let outcome = if counts {
        Outcome::Listened
    } else if ended.moved_on() {
        Outcome::Skipped
    } else {
        Outcome::Partial
    };
    (counts, outcome)
}

/// Pairs returning echo imports with their delivered listens 1-to-1 per
/// `(endpoint, listen)`, deterministically.
///
/// Non-excluded imports are visited before excluded imports (each group in
/// ascending observation order) so an active scrobble takes precedence over
/// an excluded duplicate (such as a `shifted_read` exclusion) competing for
/// the same listen, while an exclude targeting a listen's only echo still
/// joins and excludes that play. Each import's candidate listens are visited
/// in ascending observation order, taking a free candidate directly before
/// trying an augmenting path so earlier imports keep their earliest listen
/// whenever a distinct listen is free while asymmetric name matches still
/// achieve a maximum pairing.
fn pair_echoes(
    imports: &[&Import],
    by_listen: &HashMap<i64, usize>,
    excluded: &BTreeSet<i64>,
) -> HashMap<i64, usize> {
    let candidates: Vec<Vec<(i64, i64)>> = imports
        .iter()
        .map(|import| {
            let mut list: Vec<(i64, i64)> = import
                .echo_of
                .iter()
                .copied()
                .filter(|listen| by_listen.contains_key(listen))
                .map(|listen| (import.endpoint, listen))
                .collect();
            list.sort_unstable();
            list.dedup();
            list
        })
        .collect();

    let mut matched_left: HashMap<(i64, i64), usize> = HashMap::new();

    fn augment(
        u: usize,
        candidates: &[Vec<(i64, i64)>],
        seen: &mut BTreeSet<(i64, i64)>,
        matched_left: &mut HashMap<(i64, i64), usize>,
    ) -> bool {
        let Some(adj) = candidates.get(u) else {
            return false;
        };
        for &right in adj {
            if !matched_left.contains_key(&right) && seen.insert(right) {
                matched_left.insert(right, u);
                return true;
            }
        }
        for &right in adj {
            if !seen.insert(right) {
                continue;
            }
            if let Some(prev) = matched_left.get(&right).copied()
                && augment(prev, candidates, seen, matched_left)
            {
                matched_left.insert(right, u);
                return true;
            }
        }
        false
    }

    let order = (0..imports.len())
        .filter(|&u| {
            imports
                .get(u)
                .is_some_and(|i| !excluded.contains(&i.observation))
        })
        .chain((0..imports.len()).filter(|&u| {
            imports
                .get(u)
                .is_some_and(|i| excluded.contains(&i.observation))
        }));
    for u in order {
        if candidates.get(u).is_some_and(|list| !list.is_empty()) {
            let mut seen = BTreeSet::new();
            augment(u, &candidates, &mut seen, &mut matched_left);
        }
    }

    matched_left
        .into_iter()
        .filter_map(|((_, listen), u)| {
            let import = imports.get(u)?;
            let play = *by_listen.get(&listen)?;
            Some((import.observation, play))
        })
        .collect()
}

/// Derives the plays of these listens and imports: one play for each chain,
/// in the order of each chain's first observation, then one for each import
/// that is not an echo of one of those plays, in the order of their
/// observations. Returning echoes are paired 1-to-1 with their delivered
/// listens as corroborating observations; any extra import beyond the
/// matching delivered listens, which only a repeated scrobble can make,
/// stands as a play of its own.
pub fn derive(inputs: &Inputs<'_>) -> Vec<Play> {
    let excluded = excluded(inputs.corrections);
    let overrides = RecordingOverrides::build(inputs.corrections);
    let (mut plays, mut override_ids) = derive_listens(inputs, &excluded, &overrides);
    let by_listen: HashMap<i64, usize> = plays
        .iter()
        .enumerate()
        .flat_map(|(at, play)| play.observations.iter().map(move |(obs, _)| (*obs, at)))
        .collect();
    let mut imports: Vec<&Import> = inputs.imports.iter().collect();
    imports.sort_by_key(|import| import.observation);
    let taken = derive_streams(inputs, &excluded, &overrides, &mut plays);
    let remaining: Vec<&Import> = imports
        .into_iter()
        .filter(|import| !taken.contains(&import.observation))
        .collect();
    let paired = pair_echoes(&remaining, &by_listen, &excluded);
    for import in remaining {
        let import_override = overrides.for_observation(
            import.observation,
            import.endpoint,
            import.source_item,
            import.played_at_ms,
        );
        if let Some(&at) = paired.get(&import.observation)
            && let Some(play) = plays.get_mut(at)
        {
            play.observations
                .push((import.observation, Role::Corroborating));
            play.joined.push((import.observation, Joined::Echo));
            play.excluded |= excluded.contains(&import.observation);
            if let Some((corr_id, chosen)) = import_override
                && override_ids
                    .get(at)
                    .copied()
                    .flatten()
                    .is_none_or(|prev| corr_id > prev)
            {
                play.recording = chosen.binding.map(|binding| binding.recording);
                play.appearance = chosen.binding.and_then(|binding| binding.appearance);
                if let Some(slot) = override_ids.get_mut(at) {
                    *slot = Some(corr_id);
                }
            }
            continue;
        }
        let binding = match import_override {
            Some((_, chosen)) => chosen.binding,
            None => inputs.bindings.get(&import.source_item).copied(),
        };
        plays.push(Play {
            observations: vec![(import.observation, Role::Primary)],
            listener: import.listener,
            recording: binding.map(|binding| binding.recording),
            appearance: binding.and_then(|binding| binding.appearance),
            source_item: import.source_item,
            time: import_time(import, inputs.zone_periods),
            ms_played: None,
            track_duration_ms: None,
            outcome: Outcome::Listened,
            counts: true,
            count_basis: CountBasis::SourceVerdict,
            play_rule_version: inputs.rule.version,
            grouper_version: GROUPER_VERSION,
            excluded: excluded.contains(&import.observation),
            context_kind: None,
            context_ref: None,
            shuffle: None,
            joined: Vec::new(),
        });
    }
    plays
}

/// When a stream was: its end less the time played, in UTC, read in the
/// zone the listener was in then, and marked as dated from its end, since
/// a pause would make the start it gives late.
fn stream_time(stream: &StreamImport, periods: &[ZonePeriod]) -> PlayTime {
    let start = stream
        .record
        .ended_at
        .saturating_sub(stream.record.ms_played);
    let zone = assumed_zone(stream.listener, start, periods);
    let local = zone.and_then(|period| local_time(start, &period.zone));
    PlayTime {
        event_at_ms: Some(start),
        anchor: TimeAnchor::End,
        clock_basis: ClockBasis::SourceUtc,
        precision_ms: Some(SOURCE_SECOND_PRECISION_MS),
        zone: zone.map(|period| period.zone.clone()),
        zone_basis: if zone.is_some() {
            ZoneBasis::Assumed
        } else {
            ZoneBasis::None
        },
        calendar_basis: if local.is_some() {
            CalendarBasis::Zone
        } else {
            CalendarBasis::None
        },
        local,
    }
}

/// The plays of Spotify's streams: one for each listen Spotify reported,
/// with the scrobbles the stream matcher joins to it, which the stream
/// speaks for. A joined play counts by the scrobble's verdict; a stream
/// alone is judged by its track's length. A listener's streams are offered
/// that listener's scrobbles alone. Returns the scrobbles joined.
fn derive_streams(
    inputs: &Inputs<'_>,
    excluded: &BTreeSet<i64>,
    overrides: &RecordingOverrides<'_>,
    plays: &mut Vec<Play>,
) -> BTreeSet<i64> {
    let listeners: BTreeSet<i64> = inputs
        .streams
        .iter()
        .map(|stream| stream.listener)
        .collect();
    let mut taken = BTreeSet::new();
    for listener in listeners {
        let streams: Vec<&StreamImport> = inputs
            .streams
            .iter()
            .filter(|stream| stream.listener == listener)
            .collect();
        // A scrobble set aside, or a listen of this library's own come
        // back, is offered to no stream.
        let mut offered: Vec<&Import> = inputs
            .imports
            .iter()
            .filter(|import| {
                import.listener == listener
                    && import.echo_of.is_empty()
                    && !excluded.contains(&import.observation)
            })
            .collect();
        offered.sort_by_key(|import| import.observation);
        taken.extend(derive_streams_of(
            &streams, &offered, inputs, excluded, overrides, plays,
        ));
    }
    taken
}

/// The plays of one listener's streams, `theirs`, with the scrobbles
/// offered to them.
fn derive_streams_of(
    theirs: &[&StreamImport],
    offered: &[&Import],
    inputs: &Inputs<'_>,
    excluded: &BTreeSet<i64>,
    overrides: &RecordingOverrides<'_>,
    plays: &mut Vec<Play>,
) -> BTreeSet<i64> {
    use crate::streams::{Scrobble, Stream, Tier, Tiers, judge_alone, match_streams, pins};
    let streams: Vec<Stream> = theirs
        .iter()
        .map(|stream| Stream {
            key: stream.key.clone(),
            record: stream.record.clone(),
        })
        .collect();
    let scrobbles: Vec<Scrobble> = offered
        .iter()
        .map(|import| Scrobble {
            observation: import.observation,
            key: import.key.clone(),
            at_ms: import.played_at_ms,
            artist: import.artist.clone(),
            title: import.title.clone(),
        })
        .collect();
    let pins = pins(
        inputs.corrections,
        theirs
            .iter()
            .zip(&streams)
            .map(|(stored, stream)| (stored.observation, stream)),
    );
    let matched = match_streams(&streams, &scrobbles, Tiers::ALL, &pins);
    let mut joins: HashMap<usize, Vec<(usize, Option<Tier>, bool)>> = HashMap::new();
    let mut taken = BTreeSet::new();
    for join in &matched.joins {
        joins
            .entry(join.event)
            .or_default()
            .push((join.scrobble, join.tier, join.second));
        if let Some(scrobble) = offered.get(join.scrobble) {
            taken.insert(scrobble.observation);
        }
    }
    for (index, event) in matched.events.iter().enumerate() {
        let members: Vec<&StreamImport> = event
            .streams
            .iter()
            .filter_map(|at| theirs.get(*at).copied())
            .collect();
        let Some(&head) = members.first() else {
            continue;
        };
        let mut observations = vec![(head.observation, Role::Primary)];
        let mut joined = Vec::new();
        for repeat in members.iter().skip(1) {
            observations.push((repeat.observation, Role::Corroborating));
            joined.push((repeat.observation, Joined::Repeat));
        }
        let mut own = false;
        let mut scrobbled: Vec<&Import> = Vec::new();
        for (scrobble, tier, second) in joins.get(&index).map(Vec::as_slice).unwrap_or_default() {
            let Some(&import) = offered.get(*scrobble) else {
                continue;
            };
            own |= !second;
            observations.push((import.observation, Role::Corroborating));
            joined.push((
                import.observation,
                match tier {
                    Some(Tier::Start) => Joined::Start,
                    Some(Tier::During) => Joined::During,
                    Some(Tier::Renamed) => Joined::Renamed,
                    None => Joined::Pin,
                },
            ));
            scrobbled.push(import);
        }
        let record = &head.record;
        let (counts, outcome, count_basis) = if own {
            // Its sender knew the track's length, which the export lacks.
            (true, Outcome::Listened, CountBasis::SourceVerdict)
        } else {
            let (counts, outcome) = judge_alone(
                record.ms_played,
                inputs.lengths.get(&record.track_uri).copied(),
                record.reason_end.as_deref(),
            );
            (counts, outcome, CountBasis::PlayRule)
        };
        let start = record.ended_at.saturating_sub(record.ms_played);
        // Each record answers a correction's window by its own time, as
        // the store finds what a correction touches.
        let chosen = members
            .iter()
            .map(|stream| {
                (
                    stream.observation,
                    stream.endpoint,
                    stream.source_item,
                    start,
                )
            })
            .chain(scrobbled.iter().map(|import| {
                (
                    import.observation,
                    import.endpoint,
                    import.source_item,
                    import.played_at_ms,
                )
            }))
            .filter_map(|(observation, endpoint, item, at_ms)| {
                overrides.for_observation(observation, endpoint, item, at_ms)
            })
            .max_by_key(|(corr_id, _)| *corr_id);
        let binding = match chosen {
            Some((_, chosen)) => chosen.binding,
            None => inputs.bindings.get(&head.source_item).copied(),
        };
        plays.push(Play {
            excluded: observations
                .iter()
                .any(|(observation, _)| excluded.contains(observation)),
            observations,
            listener: head.listener,
            recording: binding.map(|binding| binding.recording),
            appearance: binding.and_then(|binding| binding.appearance),
            source_item: head.source_item,
            time: stream_time(head, inputs.zone_periods),
            ms_played: Some(record.ms_played),
            track_duration_ms: None,
            outcome,
            counts,
            count_basis,
            play_rule_version: inputs.rule.version,
            grouper_version: GROUPER_VERSION,
            context_kind: None,
            context_ref: None,
            shuffle: record.shuffle,
            joined,
        });
    }
    taken
}

/// The plays of the listens: one for each chain, paired with the newest
/// `set_recording` correction ID that applied to each play, if any.
fn derive_listens(
    inputs: &Inputs<'_>,
    excluded: &BTreeSet<i64>,
    overrides: &RecordingOverrides<'_>,
) -> (Vec<Play>, Vec<Option<i64>>) {
    let mut plays = Vec::new();
    let mut override_ids = Vec::new();
    for chain in chains(inputs.listens) {
        let listens: Vec<&Listen> = chain
            .iter()
            .filter_map(|index| inputs.listens.get(*index))
            .collect();
        let (Some(&head), Some(&last)) = (listens.first(), listens.last()) else {
            continue;
        };
        let ms_played = listens.iter().fold(0_i64, |total, listen| {
            total.saturating_add(listen.ms_played.max(0))
        });
        let (counts, outcome) = judge(
            &inputs.rule,
            ms_played,
            head.track_duration_ms,
            last.end_reason,
        );
        let chain_override = listens
            .iter()
            .filter_map(|listen| {
                overrides.for_observation(
                    listen.observation,
                    listen.endpoint,
                    listen.source_item,
                    listen.started_at_ms,
                )
            })
            .max_by_key(|(corr_id, _)| *corr_id);
        let binding = match chain_override {
            Some((_, chosen)) => chosen.binding,
            None => inputs.bindings.get(&head.source_item).copied(),
        };
        plays.push(Play {
            observations: listens
                .iter()
                .enumerate()
                .map(|(at, listen)| {
                    let role = if at == 0 {
                        Role::Primary
                    } else {
                        Role::Corroborating
                    };
                    (listen.observation, role)
                })
                .collect(),
            listener: head.listener,
            recording: binding.map(|binding| binding.recording),
            appearance: binding.and_then(|binding| binding.appearance),
            source_item: head.source_item,
            time: play_time(head, inputs.zone_periods),
            ms_played: Some(ms_played),
            track_duration_ms: Some(head.track_duration_ms),
            outcome,
            counts,
            count_basis: CountBasis::PlayRule,
            play_rule_version: inputs.rule.version,
            grouper_version: GROUPER_VERSION,
            excluded: listens
                .iter()
                .any(|listen| excluded.contains(&listen.observation)),
            context_kind: Some(head.context_kind),
            context_ref: head.context_ref,
            shuffle: Some(head.shuffle),
            joined: listens
                .iter()
                .skip(1)
                .map(|listen| (listen.observation, Joined::Chain))
                .collect(),
        });
        override_ids.push(chain_override.map(|(corr_id, _)| corr_id));
    }
    (plays, override_ids)
}
