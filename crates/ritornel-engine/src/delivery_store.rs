//! Scrobbling, as the library records it: which plays are eligible to be
//! sent to an account, the row that records each before it is sent, and
//! what became of it. See "Last.fm" in docs/internals/history.md.
//!
//! A delivery is durable and names the play's primary observation, a
//! listen, never the derived play. It is written as `pending`, becomes
//! `sending` in its own transaction before the request leaves, and ends as
//! what the service answered: `accepted` or `ignored`, which are final and
//! kept apart, or `refused`. One that may have been sent and got no answer
//! is `unknown` and is never sent again. One whose play is no longer
//! eligible when its turn comes is `withdrawn`, with why, and is never
//! sent. Because it is recorded before it
//! is sent, its echo, when the import brings it back, is matched to it
//! exactly (`history_store::echo_candidates`).
//!
//! Eligible to scrobble: counted, dated, a trusted clock, `observed`, not
//! excluded, with no observation from that same account, and recent enough
//! for the service to take it. The rule is its own, pinned to the
//! service's, and equals play rule 1 today because a play counts by that
//! rule.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use rusqlite::{Connection, OptionalExtension};

use ritornel_domain::history::PlayRule;
use ritornel_domain::scrobble::{corrected_track, same_track};

use crate::history_store::{rederive_delivery, rederive_delivery_by};
use crate::{Error, Id, LibraryStore, WriteRequest};

/// What a write that records or settles deliveries is called.
pub const DELIVERY_METHOD: &str = "history.deliver";

/// The version of the eligibility rule.
pub const ELIGIBILITY_VERSION: i64 = 1;

/// The oldest a play may be and still be sent. Last.fm ignores a scrobble
/// dated more than about two weeks back and documents no number, so a day
/// is kept in hand.
pub const SEND_WITHIN_MS: i64 = 13 * 24 * 60 * 60 * 1000;

/// One delivery whose turn has come, as it is to be sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Due {
    /// The delivery's row.
    pub delivery: i64,
    /// The artist.
    pub artist: String,
    /// The title.
    pub title: String,
    /// The album.
    pub album: Option<String>,
    /// The album's artist, where it is not the track's.
    pub album_artist: Option<String>,
    /// The track's place on the album.
    pub track_number: Option<i64>,
    /// The track's length.
    pub duration_ms: Option<i64>,
    /// When the play started, in seconds.
    pub timestamp_s: i64,
}

/// What became of one delivery that was sent, or was to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Settled {
    /// The service recorded it, under these names.
    Accepted {
        /// The artist as the service took it.
        artist: String,
        /// The title as the service took it.
        title: String,
    },
    /// The service did not record it, for this reason.
    Ignored {
        /// The service's code.
        code: i64,
        /// What it said.
        message: String,
    },
    /// Nothing was recorded; it is sent again from `at_ms`.
    Later {
        /// Why.
        detail: String,
        /// When it is next due.
        at_ms: i64,
    },
    /// The service refused it, and would again.
    Refused(String),
    /// It may have been recorded, and nothing says. It is never sent again.
    Unknown(String),
}

/// What a settled delivery's row is set to: its state, the names the
/// service took, its code, its detail and when it is next due.
type Columns<'a> = (
    &'a str,
    Option<(&'a str, &'a str)>,
    Option<i64>,
    Option<&'a str>,
    Option<i64>,
);

fn endpoint_of(connection: &Connection, account: Id) -> Result<(i64, i64), Error> {
    connection
        .query_row(
            "SELECT endpoint_id, listener_id FROM service_account WHERE public_id = ?1",
            [account.as_bytes()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?
        .ok_or(Error::NotFound("no such service account"))
}

/// Why a play is not sent when the account has it from another player.
pub(crate) const ALREADY_SCROBBLED: &str = "the account already has this play from another player";

/// Whether the account already has an active scrobble of this track within a
/// second of `seconds` that is not an echo of ours: one another player
/// sent. Matched by normalised artist and title, as an echo is. An import
/// that is excluded (such as a `shifted_read` duplicate) or that is already
/// part of a play some listen here made, as its echo or beside it, says
/// nothing of another play. This is not cross-source grouping by a window of
/// time; it only keeps a play from being scrobbled twice.
///
/// One such scrobble stands for one play: plays already withdrawn as
/// [`ALREADY_SCROBBLED`] claim the scrobbles first, one each, and this play
/// is already scrobbled only if one is left for it. The matching takes in
/// every scrobble and withdrawn play linked to this one by a chain of
/// seconds a second apart, so no claim is missed at its edge.
fn already_scrobbled(
    connection: &Connection,
    endpoint: i64,
    seconds: i64,
    artist: &str,
    title: &str,
) -> Result<bool, Error> {
    if foreign_scrobbles(
        connection,
        endpoint,
        seconds - 1,
        seconds + 1,
        artist,
        title,
    )?
    .is_empty()
    {
        return Ok(false);
    }
    // The scrobbles and the plays withdrawn for them that this play's
    // claim can reach: each within a second of another, as far as they go.
    let (mut lo, mut hi) = (seconds, seconds);
    let (scrobbled, mut plays) = loop {
        let scrobbled = foreign_scrobbles(connection, endpoint, lo - 1, hi + 1, artist, title)?;
        let plays = withdrawn_plays(connection, endpoint, lo - 1, hi + 1, artist, title)?;
        let (wide_lo, wide_hi) = scrobbled
            .iter()
            .chain(&plays)
            .fold((lo, hi), |(lo, hi), at| (lo.min(*at), hi.max(*at)));
        if (wide_lo, wide_hi) == (lo, hi) {
            break (scrobbled, plays);
        }
        (lo, hi) = (wide_lo, wide_hi);
    };
    plays.push(seconds);
    let edges: Vec<Vec<usize>> = plays
        .iter()
        .map(|play| {
            scrobbled
                .iter()
                .enumerate()
                .filter(|(_, at)| at.abs_diff(*play) <= 1)
                .map(|(index, _)| index)
                .collect()
        })
        .collect();
    Ok(last_is_matched(&edges))
}

/// The seconds, from `lo` to `hi`, of the account's active scrobbles of
/// this track that no listen here made, as [`already_scrobbled`] counts
/// them.
fn foreign_scrobbles(
    connection: &Connection,
    endpoint: i64,
    lo: i64,
    hi: i64,
    artist: &str,
    title: &str,
) -> Result<Vec<i64>, Error> {
    let mut statement = connection.prepare_cached(
        "SELECT i.played_at_utc_ms, i.artist, i.title FROM observation_import i
         JOIN history_observation o ON o.id = i.observation_id
         WHERE o.endpoint_id = ?1 AND i.played_at_utc_ms BETWEEN ?2 AND ?3
           AND NOT EXISTS (
             SELECT 1 FROM play_event_observation x
             JOIN play_event pe ON pe.id = x.play_event_id
             WHERE x.observation_id = i.observation_id
               AND (pe.excluded = 1
                    OR EXISTS (SELECT 1 FROM play_event_observation y
                               JOIN history_observation yo
                                 ON yo.id = y.observation_id AND yo.kind = 'listen'
                               WHERE y.play_event_id = pe.id))
           )
         ORDER BY i.played_at_utc_ms, o.id",
    )?;
    let rows = statement.query_map((endpoint, lo * 1000, hi * 1000 + 999), |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut found = Vec::new();
    for row in rows {
        let (at_ms, their_artist, their_title) = row?;
        if same_track(artist, title, &their_artist, &their_title) {
            found.push(at_ms.div_euclid(1000));
        }
    }
    Ok(found)
}

/// The seconds, from `lo` to `hi`, of plays of this track withdrawn as
/// [`ALREADY_SCROBBLED`] to the account.
fn withdrawn_plays(
    connection: &Connection,
    endpoint: i64,
    lo: i64,
    hi: i64,
    artist: &str,
    title: &str,
) -> Result<Vec<i64>, Error> {
    let mut statement = connection.prepare_cached(
        "SELECT sent_timestamp_s, artist, title FROM history_delivery
         WHERE endpoint_id = ?1 AND state = 'withdrawn' AND detail = ?2
           AND sent_timestamp_s BETWEEN ?3 AND ?4
         ORDER BY sent_timestamp_s, id",
    )?;
    let rows = statement.query_map((endpoint, ALREADY_SCROBBLED, lo, hi), |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut found = Vec::new();
    for row in rows {
        let (at, their_artist, their_title) = row?;
        if same_track(artist, title, &their_artist, &their_title) {
            found.push(at);
        }
    }
    Ok(found)
}

/// Whether, matching each left vertex in turn to a right one of its own
/// along `edges`, as many as can be, the last is matched. Those before it
/// keep theirs: an augmenting path never unmatches a vertex.
fn last_is_matched(edges: &[Vec<usize>]) -> bool {
    fn augment(
        left: usize,
        edges: &[Vec<usize>],
        seen: &mut BTreeSet<usize>,
        owner: &mut HashMap<usize, usize>,
    ) -> bool {
        for &right in edges.get(left).into_iter().flatten() {
            if !seen.insert(right) {
                continue;
            }
            let free = match owner.get(&right).copied() {
                None => true,
                Some(other) => augment(other, edges, seen, owner),
            };
            if free {
                owner.insert(right, left);
                return true;
            }
        }
        false
    }
    let mut owner = HashMap::new();
    let mut matched = false;
    for left in 0..edges.len() {
        matched = augment(left, edges, &mut BTreeSet::new(), &mut owner);
    }
    matched
}

/// A play's scrobble metadata snapshot, used when refreshing a never-attempted
/// pending delivery or reconciling one after a backup restore.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Snapshot {
    timestamp_s: i64,
    artist: String,
    title: String,
    album: Option<String>,
    album_artist: Option<String>,
    track_number: Option<i64>,
    duration_ms: Option<i64>,
}

/// What checking a pending delivery right before sending comes to.
enum PendingCheck {
    /// Its play is no longer eligible (or its track names changed after an
    /// earlier send attempt): withdraw it and never send.
    Withdraw(&'static str),
    /// The account has its play from another player, at this second and
    /// under these names, which a never-tried delivery takes from its play
    /// as it is now: withdraw it under them, so that the scrobble it claims
    /// is known by them ([`already_scrobbled`]), and never send.
    Scrobbled {
        timestamp_s: i64,
        artist: String,
        title: String,
    },
    /// Eligible to send at `timestamp_s` with the queued snapshot as stored.
    Send { timestamp_s: i64 },
    /// Eligible to send after refreshing the never-attempted delivery's stored
    /// snapshot in place.
    Refresh(Snapshot),
}

/// Checks a pending delivery against its play as it is now, its age against
/// what Last.fm takes, and `from_ms`, the time scrobbling was last turned on.
///
/// When metadata changed before any send attempt (`attempts == 0`), the
/// delivery is refreshed in place instead of withdrawn so edits never drop
/// unsent plays; once an attempt has been made (`attempts > 0`), changed
/// track names (`artist`, `title`, `album`, `album_artist`, `track_number`)
/// still withdraw the delivery so a play whose prior attempt may have reached
/// the service is never re-sent under different track metadata.
fn check_pending(
    connection: &Connection,
    delivery: i64,
    endpoint: i64,
    from_ms: i64,
    now_ms: i64,
) -> Result<PendingCheck, Error> {
    struct PlayNow {
        counts: bool,
        excluded: bool,
        at: Option<i64>,
        clock: String,
        evidence: String,
        account_has_it: bool,
        sent_from_another: bool,
        recording_id: Option<i64>,
        title: Option<String>,
        artist: Option<String>,
        album: Option<String>,
        album_artist: Option<String>,
        track_number: Option<i64>,
        duration_ms: Option<i64>,
    }
    let play: Option<PlayNow> = connection
        .prepare_cached(&format!(
            "SELECT pe.counts, pe.excluded, pe.event_at_utc_ms, pe.clock_basis,
                    pe.evidence_class,
                    EXISTS (SELECT 1 FROM play_event_observation x
                            JOIN history_observation xo ON xo.id = x.observation_id
                            WHERE x.play_event_id = pe.id AND xo.endpoint_id = ?2),
                    EXISTS (SELECT 1 FROM history_delivery other
                            JOIN play_event_observation y ON y.observation_id = other.observation_id
                            WHERE y.play_event_id = pe.id AND other.endpoint_id = ?2
                              AND other.id <> d.id
                              AND other.state NOT IN ('pending', 'withdrawn')),
                    pe.recording_id, {NAMES}, pe.track_duration_ms
             FROM history_delivery d
             JOIN play_event_observation peo
               ON peo.observation_id = d.observation_id AND peo.role = 'primary'
             JOIN play_event pe ON pe.id = peo.play_event_id
             LEFT JOIN recording r ON r.id = pe.recording_id
             LEFT JOIN track_appearance ta ON ta.id = pe.appearance_id
             {NAME_JOINS}
             WHERE d.id = ?1"
        ))?
        .query_row((delivery, endpoint), |row| {
            Ok(PlayNow {
                counts: row.get(0)?,
                excluded: row.get(1)?,
                at: row.get(2)?,
                clock: row.get(3)?,
                evidence: row.get(4)?,
                account_has_it: row.get(5)?,
                sent_from_another: row.get(6)?,
                recording_id: row.get(7)?,
                title: row.get(8)?,
                artist: row.get(9)?,
                album: row.get(10)?,
                album_artist: row.get(11)?,
                track_number: row.get(12)?,
                duration_ms: row.get(13)?,
            })
        })
        .optional()?;
    struct Queued {
        attempts: i64,
        snapshot: Snapshot,
    }
    let queued = connection
        .prepare_cached(
            "SELECT attempts, sent_timestamp_s, artist, title, album, album_artist,
                    track_number, duration_ms
             FROM history_delivery WHERE id = ?1",
        )?
        .query_row([delivery], |row| {
            Ok(Queued {
                attempts: row.get(0)?,
                snapshot: Snapshot {
                    timestamp_s: row.get(1)?,
                    artist: row.get(2)?,
                    title: row.get(3)?,
                    album: row.get(4)?,
                    album_artist: row.get(5)?,
                    track_number: row.get(6)?,
                    duration_ms: row.get(7)?,
                },
            })
        })?;
    let (check_seconds, check_artist, check_title) = play
        .as_ref()
        .filter(|_| queued.attempts == 0)
        .and_then(|play| {
            Some((
                play.at?.div_euclid(1000),
                play.artist.as_deref()?,
                play.title.as_deref()?,
            ))
        })
        .unwrap_or((
            queued.snapshot.timestamp_s,
            queued.snapshot.artist.as_str(),
            queued.snapshot.title.as_str(),
        ));
    if already_scrobbled(
        connection,
        endpoint,
        check_seconds,
        check_artist,
        check_title,
    )? {
        return Ok(PendingCheck::Scrobbled {
            timestamp_s: check_seconds,
            artist: check_artist.to_owned(),
            title: check_title.to_owned(),
        });
    }
    let Some(play) = play else {
        return Ok(PendingCheck::Withdraw(
            "its play is no longer derived from this listen",
        ));
    };
    let Some(at) = play.at else {
        return Ok(PendingCheck::Withdraw("its play has no date"));
    };
    if !play.counts {
        return Ok(PendingCheck::Withdraw("its play no longer counts"));
    }
    if play.excluded {
        return Ok(PendingCheck::Withdraw("its play is excluded"));
    }
    if play.clock != "hub_age" || play.evidence != "observed" {
        return Ok(PendingCheck::Withdraw(
            "its play is not dated by a trusted clock",
        ));
    }
    if play.account_has_it {
        return Ok(PendingCheck::Withdraw("the account already has this play"));
    }
    if play.sent_from_another {
        return Ok(PendingCheck::Withdraw(
            "its play was sent from another of its listens",
        ));
    }
    if play.recording_id.is_none() {
        return Ok(PendingCheck::Withdraw(
            "its play is no longer bound to a recording",
        ));
    }
    let play_seconds = at.div_euclid(1000);
    let names_changed = play.artist.as_deref() != Some(queued.snapshot.artist.as_str())
        || play.title.as_deref() != Some(queued.snapshot.title.as_str())
        || play.album != queued.snapshot.album
        || play.album_artist != queued.snapshot.album_artist
        || play.track_number != queued.snapshot.track_number;
    if queued.attempts > 0 && names_changed {
        return Ok(PendingCheck::Withdraw(
            "its play's track changed since it was queued",
        ));
    }
    if at < now_ms - SEND_WITHIN_MS {
        return Ok(PendingCheck::Withdraw(
            "its play is too old for Last.fm to take",
        ));
    }
    if at < from_ms {
        return Ok(PendingCheck::Withdraw(
            "its play is from before scrobbling was turned on",
        ));
    }
    if queued.attempts == 0 {
        let Some(current) = (match (play.artist, play.title) {
            (Some(artist), Some(title)) => Some(Snapshot {
                timestamp_s: play_seconds,
                artist,
                title,
                album: play.album,
                album_artist: play.album_artist,
                track_number: play.track_number,
                duration_ms: play.duration_ms,
            }),
            _ => None,
        }) else {
            return Ok(PendingCheck::Withdraw(
                "its play's track changed since it was queued",
            ));
        };
        if current != queued.snapshot {
            return Ok(PendingCheck::Refresh(current));
        }
    }
    Ok(PendingCheck::Send {
        timestamp_s: queued.snapshot.timestamp_s,
    })
}

/// The names a play is sent under, of a track appearance `ta` and its
/// recording `r`: its title, artist, album, the album's artist where it
/// is not the track's, and its place on the album.
const NAMES: &str = "coalesce(ta.display_title, r.display_title), ac.display_text,
    rel.display_title, nullif(rac.display_text, ac.display_text), ta.display_track_number";

/// What [`NAMES`] reads beside `ta` and `r`.
const NAME_JOINS: &str = "LEFT JOIN artist_credit ac
      ON ac.id = coalesce(ta.display_artist_credit_id, r.display_artist_credit_id)
    LEFT JOIN release rel ON rel.id = ta.release_id
    LEFT JOIN artist_credit rac ON rac.id = rel.display_artist_credit_id";

/// A track as Last.fm's "now playing" shows it: the names its scrobble
/// would carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Playing {
    /// The current track appearance after following its redirect.
    pub appearance: Id,
    /// The artist.
    pub artist: String,
    /// The title.
    pub title: String,
    /// The album.
    pub album: Option<String>,
    /// The album's artist, where it is not the track's.
    pub album_artist: Option<String>,
    /// The track's place on the album.
    pub track_number: Option<i64>,
}

/// One unplanned listen play, as it would be sent or reconciled.
struct UnplannedListen {
    observation: i64,
    at_ms: i64,
    clock_basis: String,
    time_precision_ms: i64,
    artist: String,
    title: String,
    album: Option<String>,
    album_artist: Option<String>,
    track_number: Option<i64>,
    duration_ms: Option<i64>,
}

/// Which clock bases `unplanned_listens` accepts.
#[derive(Clone, Copy)]
enum ClockScope {
    /// Normal planning for outward sends: trusted `hub_age` plays only.
    HubAgeOnly,
    /// Post-restore reconciliation: also include `server_wall` plays because
    /// a staged/outbox listen closed after a reboot falls back to `server_wall`
    /// even though the pre-restore server may have dated it `hub_age` and sent it.
    HubAgeOrWall,
}

/// The plays eligible to be sent to the account's endpoint that have no
/// delivery there yet, played from `from_ms` on. A play that comes to be
/// led by another of its listens, such as a resumed listen's predecessor
/// arriving late, has a delivery already if any of its listens was sent,
/// or answered, or refused. One only waiting or withdrawn does not count:
/// it is withdrawn when its turn comes, as its listen no longer leads the
/// play, and the play is sent once, from the listen that does.
fn eligible(
    connection: &Connection,
    endpoint: i64,
    listener: i64,
    from_ms: i64,
) -> Result<Vec<UnplannedListen>, Error> {
    unplanned_listens(
        connection,
        endpoint,
        listener,
        from_ms,
        ClockScope::HubAgeOnly,
    )
}

fn unplanned_listens(
    connection: &Connection,
    endpoint: i64,
    listener: i64,
    from_ms: i64,
    scope: ClockScope,
) -> Result<Vec<UnplannedListen>, Error> {
    let clock_clause = match scope {
        ClockScope::HubAgeOnly => "pe.clock_basis = 'hub_age'",
        ClockScope::HubAgeOrWall => "pe.clock_basis IN ('hub_age', 'server_wall')",
    };
    let mut statement = connection.prepare(&format!(
        "SELECT peo.observation_id, pe.event_at_utc_ms, pe.clock_basis, pe.time_precision_ms,
                {NAMES}, pe.track_duration_ms
         FROM play_event pe
         JOIN play_event_observation peo ON peo.play_event_id = pe.id AND peo.role = 'primary'
         JOIN history_observation o ON o.id = peo.observation_id AND o.kind = 'listen'
         JOIN recording r ON r.id = pe.recording_id
         LEFT JOIN track_appearance ta ON ta.id = pe.appearance_id
         {NAME_JOINS}
         WHERE pe.listener_id = ?1 AND pe.event_at_utc_ms >= ?3
           AND pe.counts = 1 AND pe.excluded = 0 AND pe.evidence_class = 'observed'
           AND {clock_clause}
           AND NOT EXISTS (SELECT 1 FROM play_event_observation x
                           JOIN history_observation xo ON xo.id = x.observation_id
                           WHERE x.play_event_id = pe.id AND xo.endpoint_id = ?2)
           AND NOT EXISTS (SELECT 1 FROM history_delivery d
                           JOIN play_event_observation y ON y.observation_id = d.observation_id
                           WHERE y.play_event_id = pe.id AND d.endpoint_id = ?2
                             AND (d.observation_id = peo.observation_id
                                  OR d.state NOT IN ('pending', 'withdrawn')))
         ORDER BY pe.event_at_utc_ms, peo.observation_id"
    ))?;
    struct RowData {
        observation: i64,
        at_ms: i64,
        clock_basis: String,
        time_precision_ms: i64,
        title: Option<String>,
        artist: Option<String>,
        album: Option<String>,
        album_artist: Option<String>,
        track_number: Option<i64>,
        duration_ms: Option<i64>,
    }
    let rows = statement.query_map((listener, endpoint, from_ms), |row| {
        Ok(RowData {
            observation: row.get(0)?,
            at_ms: row.get(1)?,
            clock_basis: row.get(2)?,
            time_precision_ms: row.get(3)?,
            title: row.get(4)?,
            artist: row.get(5)?,
            album: row.get(6)?,
            album_artist: row.get(7)?,
            track_number: row.get(8)?,
            duration_ms: row.get(9)?,
        })
    })?;
    let mut found = Vec::new();
    for row in rows {
        let row = row?;
        // A play whose track shows no artist or no title cannot be sent.
        if let (Some(title), Some(artist)) = (row.title, row.artist) {
            found.push(UnplannedListen {
                observation: row.observation,
                at_ms: row.at_ms,
                clock_basis: row.clock_basis,
                time_precision_ms: row.time_precision_ms,
                artist,
                title,
                album: row.album,
                album_artist: row.album_artist,
                track_number: row.track_number,
                duration_ms: row.duration_ms,
            });
        }
    }
    Ok(found)
}

impl LibraryStore {
    /// The names a play of this track appearance would be scrobbled under,
    /// followed through its redirect. `None` for one the library no longer
    /// has, or that shows no artist or no title, which cannot be sent.
    pub fn playing(&self, appearance: Id) -> Result<Option<Playing>, Error> {
        self.read(move |connection| {
            let found = connection
                .query_row(
                    &format!(
                        "SELECT {NAMES}, ta.public_id FROM track_appearance ta
                         JOIN recording r ON r.id = (SELECT canonical_id FROM recording WHERE id = ta.recording_id)
                         {NAME_JOINS}
                         WHERE ta.id =
                           (SELECT canonical_id FROM track_appearance WHERE public_id = ?1)"
                    ),
                    [appearance.as_bytes()],
                    |row| {
                        Ok((
                            row.get::<_, Option<String>>(0)?,
                            row.get::<_, Option<String>>(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                            Id::from_bytes(row.get(5)?),
                        ))
                    },
                )
                .optional()?;
            Ok(
                found.and_then(|(title, artist, album, album_artist, track_number, appearance)| {
                    Some(Playing {
                        appearance,
                        artist: artist?,
                        title: title?,
                        album,
                        album_artist,
                        track_number,
                    })
                }),
            )
        })
    }

    /// Records a pending delivery to the account for every play eligible
    /// to be sent there that has none, played from `from_ms` on and within
    /// [`SEND_WITHIN_MS`] of now. Returns how many it recorded; with none
    /// it writes nothing.
    pub fn plan_deliveries(&self, account: Id, from_ms: i64) -> Result<usize, Error> {
        let from_ms = from_ms.max(self.clock_now_ms() - SEND_WITHIN_MS);
        let found = self.read(move |connection| {
            let (endpoint, listener) = endpoint_of(connection, account)?;
            eligible(connection, endpoint, listener, from_ms)
        })?;
        if found.is_empty() {
            return Ok(0);
        }
        let written = self.write(
            WriteRequest {
                method: DELIVERY_METHOD,
                command: None,
                client: None,
            },
            move |write| {
                let connection = write.connection();
                let (endpoint, listener) = endpoint_of(connection, account)?;
                // While a restore hold is active for this account, defer the
                // `already_scrobbled` withdrawal check until `release_hold`
                // clears the hold: batches committed mid-sync before
                // `release_hold` runs may be echoes of plays the pre-restore
                // server sent after the backup, which `release_hold` reconciles
                // as `accepted` instead of leaving separate from the listen.
                let is_held = held_of(connection, account)?.is_some();
                // Read again inside the transaction: what was eligible a
                // moment ago and is still.
                let found = eligible(connection, endpoint, listener, from_ms)?;
                let mut pending = 0;
                for play in &found {
                    // A play another player already scrobbled to the account
                    // is recorded as withdrawn, so that it is not looked at
                    // again, and is never sent.
                    let seconds = play.at_ms.div_euclid(1000);
                    let (state, detail) = if !is_held
                        && already_scrobbled(
                            connection,
                            endpoint,
                            seconds,
                            &play.artist,
                            &play.title,
                        )? {
                        ("withdrawn", Some(ALREADY_SCROBBLED))
                    } else {
                        pending += 1;
                        ("pending", None)
                    };
                    connection
                        .prepare_cached(
                            "INSERT INTO history_delivery
                               (observation_id, endpoint_id, state, sent_timestamp_s, artist, title,
                                album, album_artist, track_number, duration_ms, created_at_ms,
                                updated_at_ms, rev, detail)
                             VALUES (?1, ?2, ?13, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10, ?11, ?12)",
                        )?
                        .execute(rusqlite::params![
                            play.observation,
                            endpoint,
                            seconds,
                            play.artist,
                            play.title,
                            play.album,
                            play.album_artist,
                            play.track_number,
                            play.duration_ms,
                            write.now_ms(),
                            write.rev(),
                            detail,
                            state,
                        ])?;
                }
                Ok(pending.to_string())
            },
        )?;
        Ok(written.result_json.parse().unwrap_or(0))
    }

    /// Takes up to `limit` pending deliveries to the account that are due,
    /// oldest first and each of its own second, and marks them `sending`
    /// in the same transaction, before anything is sent. Each is checked
    /// again there against its play as it is now and against `from_ms`,
    /// the time scrobbling was last turned on: a never-attempted delivery
    /// whose metadata changed is refreshed in place, while one that may no
    /// longer be sent (or changed after an earlier attempt) is `withdrawn`,
    /// with why, and is never sent. While the account is held after a backup
    /// restore, or with none due, it writes nothing.
    pub fn take_due_deliveries(
        &self,
        account: Id,
        from_ms: i64,
        limit: usize,
    ) -> Result<Vec<Due>, Error> {
        let now = self.clock_now_ms();
        let due = move |connection: &Connection| -> Result<Vec<i64>, Error> {
            let (endpoint, _) = endpoint_of(connection, account)?;
            if held_of(connection, account)?.is_some() {
                return Ok(Vec::new());
            }
            let mut statement = connection.prepare_cached(
                "SELECT id
                 FROM history_delivery
                 WHERE endpoint_id = ?1 AND state = 'pending'
                   AND (next_attempt_at_ms IS NULL OR next_attempt_at_ms <= ?2)
                 ORDER BY sent_timestamp_s, id",
            )?;
            let rows = statement.query_map((endpoint, now), |row| row.get(0))?;
            Ok(rows.collect::<Result<_, _>>()?)
        };
        if self.read(due)?.is_empty() {
            return Ok(Vec::new());
        }
        let hub = self.hub();
        let written = self.write(
            WriteRequest {
                method: DELIVERY_METHOD,
                command: None,
                client: None,
            },
            move |write| {
                let connection = write.connection();
                let (endpoint, _) = endpoint_of(connection, account)?;
                let mut seconds = std::collections::BTreeSet::new();
                let mut taken = Vec::new();
                for delivery in due(connection)? {
                    if taken.len() == limit {
                        break;
                    }
                    let (timestamp_s, refresh) =
                        match check_pending(connection, delivery, endpoint, from_ms, now)? {
                            PendingCheck::Withdraw(why) => {
                                connection
                                    .prepare_cached(
                                        "UPDATE history_delivery
                                         SET state = 'withdrawn', detail = ?2, updated_at_ms = ?3,
                                             rev = ?4
                                         WHERE id = ?1",
                                    )?
                                    .execute((delivery, why, write.now_ms(), write.rev()))?;
                                continue;
                            }
                            PendingCheck::Scrobbled {
                                timestamp_s,
                                artist,
                                title,
                            } => {
                                connection
                                    .prepare_cached(
                                        "UPDATE history_delivery
                                         SET state = 'withdrawn', detail = ?2, updated_at_ms = ?3,
                                             rev = ?4, sent_timestamp_s = ?5, artist = ?6,
                                             title = ?7
                                         WHERE id = ?1",
                                    )?
                                    .execute(rusqlite::params![
                                        delivery,
                                        ALREADY_SCROBBLED,
                                        write.now_ms(),
                                        write.rev(),
                                        timestamp_s,
                                        artist,
                                        title,
                                    ])?;
                                continue;
                            }
                            PendingCheck::Send { timestamp_s } => (timestamp_s, None),
                            PendingCheck::Refresh(snapshot) => {
                                (snapshot.timestamp_s, Some(snapshot))
                            }
                        };
                    // The service ties an answer to a play by its time alone.
                    if !seconds.insert(timestamp_s) {
                        continue;
                    }
                    // Sent by this server, the hub: the row says so, for the
                    // day a satellite may send one itself.
                    if let Some(snapshot) = refresh {
                        connection
                            .prepare_cached(
                                "UPDATE history_delivery
                                 SET state = 'sending', attempts = attempts + 1,
                                     sent_timestamp_s = ?2, artist = ?3, title = ?4,
                                     album = ?5, album_artist = ?6, track_number = ?7,
                                     duration_ms = ?8, updated_at_ms = ?9, rev = ?10,
                                     sent_by_server_id = (SELECT id FROM server WHERE public_id = ?11)
                                 WHERE id = ?1",
                            )?
                            .execute(rusqlite::params![
                                delivery,
                                snapshot.timestamp_s,
                                snapshot.artist,
                                snapshot.title,
                                snapshot.album,
                                snapshot.album_artist,
                                snapshot.track_number,
                                snapshot.duration_ms,
                                write.now_ms(),
                                write.rev(),
                                hub.as_bytes(),
                            ])?;
                    } else {
                        connection
                            .prepare_cached(
                                "UPDATE history_delivery
                                 SET state = 'sending', attempts = attempts + 1, updated_at_ms = ?2,
                                     rev = ?3,
                                     sent_by_server_id = (SELECT id FROM server WHERE public_id = ?4)
                                 WHERE id = ?1",
                            )?
                            .execute((delivery, write.now_ms(), write.rev(), hub.as_bytes()))?;
                    }
                    rederive_delivery(write, delivery)?;
                    taken.push(delivery);
                }
                Ok(serde_json::to_string(&taken)?)
            },
        )?;
        let taken: Vec<i64> = serde_json::from_str(&written.result_json)?;
        let rows = self.read(move |connection| due_rows(connection, &taken))?;
        Ok(rows)
    }

    /// Records what became of deliveries that were taken to be sent.
    pub fn settle_deliveries(&self, settled: Vec<(i64, Settled)>) -> Result<(), Error> {
        if settled.is_empty() {
            return Ok(());
        }
        self.write(
            WriteRequest {
                method: DELIVERY_METHOD,
                command: None,
                client: None,
            },
            move |write| {
                let connection = write.connection();
                for (delivery, outcome) in &settled {
                    let (state, accepted, code, detail, next): Columns<'_> = match outcome {
                        Settled::Accepted { artist, title } => {
                            ("accepted", Some((artist, title)), None, None, None)
                        }
                        Settled::Ignored { code, message } => {
                            ("ignored", None, Some(*code), Some(message), None)
                        }
                        Settled::Later { detail, at_ms } => {
                            ("pending", None, None, Some(detail), Some(*at_ms))
                        }
                        Settled::Refused(detail) => ("refused", None, None, Some(detail), None),
                        Settled::Unknown(detail) => ("unknown", None, None, Some(detail), None),
                    };
                    let changed = connection
                        .prepare_cached(
                            "UPDATE history_delivery
                             SET state = ?2, accepted_artist = ?3, accepted_title = ?4,
                                 ignored_code = ?5, detail = ?6, next_attempt_at_ms = ?7,
                                 updated_at_ms = ?8, rev = ?9
                             WHERE id = ?1 AND state = 'sending'",
                        )?
                        .execute(rusqlite::params![
                            delivery,
                            state,
                            accepted.map(|(artist, _)| artist),
                            accepted.map(|(_, title)| title),
                            code,
                            detail,
                            next,
                            write.now_ms(),
                            write.rev(),
                        ])?;
                    // One no longer sending, as a start marks one an earlier
                    // run left, keeps what it was found to be.
                    if changed == 0 {
                        continue;
                    }
                    // What an echo is matched on may have changed.
                    rederive_delivery(write, *delivery)?;
                }
                Ok(String::new())
            },
        )?;
        Ok(())
    }

    /// Marks every delivery a run left `sending` as `unknown`: it may have
    /// reached the service, and it is never sent again. Run at start.
    pub fn abandon_sending(&self) -> Result<usize, Error> {
        let stuck: i64 = self.read(|connection| {
            Ok(connection.query_row(
                "SELECT count(*) FROM history_delivery WHERE state = 'sending'",
                [],
                |row| row.get(0),
            )?)
        })?;
        if stuck == 0 {
            return Ok(0);
        }
        self.write(
            WriteRequest {
                method: DELIVERY_METHOD,
                command: None,
                client: None,
            },
            |write| {
                write.connection().execute(
                    "UPDATE history_delivery
                     SET state = 'unknown', detail = 'the server stopped while it was being sent',
                         updated_at_ms = ?1, rev = ?2
                     WHERE state = 'sending'",
                    (write.now_ms(), write.rev()),
                )?;
                Ok(String::new())
            },
        )?;
        Ok(usize::try_from(stuck).unwrap_or(0))
    }

    /// How many deliveries to the account are in each state.
    pub fn delivery_counts(&self, account: Id) -> Result<Vec<(String, i64)>, Error> {
        self.read(move |connection| {
            let (endpoint, _) = endpoint_of(connection, account)?;
            let mut statement = connection.prepare(
                "SELECT state, count(*) FROM history_delivery WHERE endpoint_id = ?1
                 GROUP BY state ORDER BY state",
            )?;
            let rows = statement.query_map([endpoint], |row| Ok((row.get(0)?, row.get(1)?)))?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
    }
}

/// The legacy global setting key written by older backups before restore
/// holds were scoped per account. Retained for reading and expanding older
/// backups cleanly on open or release.
pub(crate) const HELD_SETTING: &str = "scrobbling.held_until_import";

/// Prefix for account-scoped restore reconciliation holds in
/// `library_setting`: `scrobbling.held_until_import.<account-public-id>`.
const HELD_PREFIX: &str = "scrobbling.held_until_import.";

/// Exclusive upper bound for range queries over [`HELD_PREFIX`] keys
/// (`'/'` immediately follows `'.'` in ASCII, avoiding SQLite `LIKE`
/// wildcard issues on `'_'`).
const HELD_PREFIX_END: &str = "scrobbling.held_until_import/";

/// Prefix for bounded post-restore reconciliation windows in `library_setting`:
/// `scrobbling.restored.<account-public-id>`. Kept for `SEND_WITHIN_MS` after
/// `release_hold` so satellite listens or deferred local outbox listens that
/// arrive after the first sync still reconcile against imported scrobbles.
const RESTORED_PREFIX: &str = "scrobbling.restored.";

/// Exclusive upper bound for range queries over [`RESTORED_PREFIX`] keys.
const RESTORED_PREFIX_END: &str = "scrobbling.restored/";

/// Detail recorded on deliveries reconciled as already present at the
/// service after a backup restore.
const RESTORED_DETAIL: &str = "the service had it already when the library was restored";

fn held_key(account: Id) -> String {
    format!("{HELD_PREFIX}{account}")
}

fn restored_key(account: Id) -> String {
    format!("{RESTORED_PREFIX}{account}")
}

fn service_accounts(connection: &Connection) -> Result<Vec<Id>, Error> {
    let mut statement =
        connection.prepare_cached("SELECT public_id FROM service_account ORDER BY id")?;
    let rows = statement.query_map([], |row| Ok(Id::from_bytes(row.get(0)?)))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn held_row(connection: &Connection, key: &str) -> Result<Option<Held>, Error> {
    Ok(connection
        .query_row(
            "SELECT json_extract(value_json, '$.takenAtMs'),
                    json_extract(value_json, '$.sinceMs')
             FROM library_setting WHERE key = ?1",
            [key],
            |row| {
                Ok(Held {
                    taken_at_ms: row.get(0)?,
                    since_ms: row.get(1)?,
                })
            },
        )
        .optional()?)
}

fn held_of(connection: &Connection, account: Id) -> Result<Option<Held>, Error> {
    if let Some(held) = held_row(connection, &held_key(account))? {
        return Ok(Some(held));
    }
    held_row(connection, HELD_SETTING)
}

fn any_held(connection: &Connection) -> Result<Option<Held>, Error> {
    let mut statement = connection.prepare_cached(
        "SELECT json_extract(value_json, '$.takenAtMs'),
                json_extract(value_json, '$.sinceMs')
         FROM library_setting
         WHERE key = ?1 OR (key >= ?2 AND key < ?3)
         ORDER BY key",
    )?;
    let rows: Vec<Held> = statement
        .query_map((HELD_SETTING, HELD_PREFIX, HELD_PREFIX_END), |row| {
            Ok(Held {
                taken_at_ms: row.get(0)?,
                since_ms: row.get(1)?,
            })
        })?
        .collect::<Result<_, _>>()?;
    let Some(first) = rows.first().copied() else {
        return Ok(None);
    };
    let taken_at_ms = rows
        .iter()
        .map(|held| held.taken_at_ms)
        .min()
        .unwrap_or(first.taken_at_ms);
    let since_ms = if rows.iter().any(|held| held.since_ms.is_none()) {
        None
    } else {
        rows.iter().filter_map(|held| held.since_ms).min()
    };
    Ok(Some(Held {
        taken_at_ms,
        since_ms,
    }))
}

/// Upserts a per-account hold row at `key`, keeping the earliest `takenAtMs`
/// and earliest `sinceMs` across any existing row and the incoming values.
fn upsert_hold(
    connection: &Connection,
    key: &str,
    taken_at_ms: i64,
    since_ms: Option<i64>,
    rev: i64,
) -> Result<(), Error> {
    let existing = held_row(connection, key)?;
    let taken = existing.map_or(taken_at_ms, |held| held.taken_at_ms.min(taken_at_ms));
    let since = match (existing.and_then(|held| held.since_ms), since_ms) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(a), None) | (None, Some(a)) => Some(a),
        (None, None) => None,
    };
    let value = match since {
        Some(since_ms) => serde_json::json!({ "takenAtMs": taken, "sinceMs": since_ms }),
        None => serde_json::json!({ "takenAtMs": taken }),
    };
    connection
        .prepare_cached(
            "INSERT INTO library_setting (key, value_json, rev)
             VALUES (?1, ?2, ?3)
             ON CONFLICT (key) DO UPDATE SET value_json = excluded.value_json, rev = excluded.rev",
        )?
        .execute((key, value.to_string(), rev))?;
    Ok(())
}

/// Expands a legacy global [`HELD_SETTING`] row into per-account holds for
/// every `service_account`, preserving the earliest `takenAtMs` and any
/// `sinceMs`, then deletes the global key.
fn expand_legacy_hold(write: &crate::library_store::Write<'_>) -> Result<(), Error> {
    let connection = write.connection();
    let Some(legacy) = held_row(connection, HELD_SETTING)? else {
        return Ok(());
    };
    let accounts = service_accounts(connection)?;
    if accounts.is_empty() {
        return Ok(());
    }
    for account in accounts {
        upsert_hold(
            connection,
            &held_key(account),
            legacy.taken_at_ms,
            legacy.since_ms,
            write.rev(),
        )?;
    }
    connection.execute("DELETE FROM library_setting WHERE key = ?1", [HELD_SETTING])?;
    Ok(())
}

/// Writes per-account restore holds into the library at `connection`, a
/// backup's copy taken at `taken_at_ms`, for every `service_account` present:
/// a library with any account may have pending deliveries, unplanned listens,
/// staged listens in `server.db`, or listens sitting in a satellite's outbox
/// that the service accepts after the backup is taken. With no service
/// accounts, nothing is written. Its revision is the copy's last, since the
/// copy is not a write of the library's own. Returns whether any hold was
/// written.
pub(crate) fn hold_sending(connection: &Connection, taken_at_ms: i64) -> Result<bool, Error> {
    // A file the server would not open as a library is copied as it is.
    let is_library: bool = connection.query_row(
        "SELECT count(*) = 3 FROM sqlite_schema
         WHERE type = 'table'
           AND name IN ('library_setting', 'history_delivery', 'service_account')",
        [],
        |row| row.get(0),
    )?;
    if !is_library {
        return Ok(false);
    }
    let accounts = service_accounts(connection)?;
    if accounts.is_empty() {
        return Ok(false);
    }
    let rev: i64 = connection.query_row(
        "SELECT coalesce(max(rev), 0) FROM library_revision",
        [],
        |row| row.get(0),
    )?;
    let legacy_taken = held_row(connection, HELD_SETTING)?.map(|held| held.taken_at_ms);
    let base_taken = legacy_taken.map_or(taken_at_ms, |legacy| legacy.min(taken_at_ms));
    let mut earliest_taken = base_taken;
    for account in accounts {
        let key = held_key(account);
        let existing_taken = held_row(connection, &key)?.map(|held| held.taken_at_ms);
        let effective_taken =
            existing_taken.map_or(base_taken, |existing| existing.min(base_taken));
        earliest_taken = earliest_taken.min(effective_taken);
        // On a fresh backup copy, clear any prior `sinceMs` so the restored
        // server stamps its own start time when opened.
        connection
            .prepare_cached(
                "INSERT INTO library_setting (key, value_json, rev)
                 VALUES (?1, json_object('takenAtMs', ?2), ?3)
                 ON CONFLICT (key) DO UPDATE SET value_json = excluded.value_json, rev = excluded.rev",
            )?
            .execute((&key, effective_taken, rev))?;
    }
    // Also write the legacy global hold key so older binaries (or a revert)
    // still hold scrobbling on restore; newer binaries expand and remove it
    // on first open (`opened_restored` / `release_hold`).
    connection
        .prepare_cached(
            "INSERT INTO library_setting (key, value_json, rev)
             VALUES (?1, json_object('takenAtMs', ?2), ?3)
             ON CONFLICT (key) DO UPDATE SET value_json = excluded.value_json, rev = excluded.rev",
        )?
        .execute((HELD_SETTING, earliest_taken, rev))?;
    // Clear any post-restore window from an earlier restore so a restore of
    // this new backup reconciles cleanly after its own import finishes.
    connection.execute(
        "DELETE FROM library_setting WHERE key >= ?1 AND key < ?2",
        (RESTORED_PREFIX, RESTORED_PREFIX_END),
    )?;
    Ok(true)
}

/// Scrobbling held back after a restore, until an import has run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Held {
    /// When the backup restored was taken.
    pub taken_at_ms: i64,
    /// When this server first opened the library restored: since then,
    /// nothing is sent. `None` until it has.
    pub since_ms: Option<i64>,
}

/// An active, non-excluded import observation not yet corroborating any listen.
#[derive(Clone)]
struct UnmatchedImport {
    observation: i64,
    seconds: i64,
    artist: String,
    title: String,
}

/// Active, non-excluded whole-second imports on `endpoint` within
/// `[min_s, max_s]` that do not already corroborate any listen play.
fn unmatched_imports_in(
    connection: &Connection,
    endpoint: i64,
    min_s: i64,
    max_s: i64,
) -> Result<Vec<UnmatchedImport>, Error> {
    let min_ms = min_s.saturating_mul(1000);
    let max_ms = max_s.saturating_mul(1000);
    let mut statement = connection.prepare_cached(
        "SELECT o.id, i.played_at_utc_ms, i.artist, i.title
         FROM observation_import i
         JOIN history_observation o ON o.id = i.observation_id
         WHERE o.endpoint_id = ?1
           AND i.played_at_utc_ms BETWEEN ?2 AND ?3
           AND i.played_at_utc_ms % 1000 = 0
           AND NOT EXISTS (
             SELECT 1 FROM play_event_observation x
             JOIN play_event pe ON pe.id = x.play_event_id
             WHERE x.observation_id = i.observation_id
               AND (pe.excluded = 1
                    OR EXISTS (SELECT 1 FROM play_event_observation y
                               JOIN history_observation yo
                                 ON yo.id = y.observation_id AND yo.kind = 'listen'
                               WHERE y.play_event_id = pe.id))
           )
         ORDER BY o.id",
    )?;
    let rows = statement.query_map((endpoint, min_ms, max_ms), |row| {
        let at_ms: i64 = row.get(1)?;
        Ok(UnmatchedImport {
            observation: row.get(0)?,
            seconds: at_ms.div_euclid(1000),
            artist: row.get(2)?,
            title: row.get(3)?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// The current track snapshot of a delivery's play, when that delivery's
/// listen still leads a counted, non-excluded, bound, dated play.
fn current_play_snapshot(
    connection: &Connection,
    delivery: i64,
) -> Result<Option<Snapshot>, Error> {
    let found = connection
        .prepare_cached(&format!(
            "SELECT pe.event_at_utc_ms, {NAMES}, pe.track_duration_ms
             FROM history_delivery d
             JOIN play_event_observation peo
               ON peo.observation_id = d.observation_id AND peo.role = 'primary'
             JOIN play_event pe ON pe.id = peo.play_event_id
             JOIN recording r ON r.id = pe.recording_id
             LEFT JOIN track_appearance ta ON ta.id = pe.appearance_id
             {NAME_JOINS}
             WHERE d.id = ?1 AND pe.counts = 1 AND pe.excluded = 0
               AND pe.event_at_utc_ms IS NOT NULL"
        ))?
        .query_row([delivery], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<i64>>(5)?,
                row.get::<_, Option<i64>>(6)?,
            ))
        })
        .optional()?;
    Ok(found.and_then(
        |(at, title, artist, album, album_artist, track_number, duration_ms)| {
            Some(Snapshot {
                timestamp_s: at.div_euclid(1000),
                artist: artist?,
                title: title?,
                album,
                album_artist,
                track_number,
                duration_ms,
            })
        },
    ))
}

/// One candidate play to reconcile during `release_hold`.
enum ReconcileCandidate {
    Pending {
        delivery: i64,
        queued_s: i64,
        queued_artist: String,
        queued_title: String,
        refresh: Option<Snapshot>,
    },
    Unplanned(UnplannedListen),
}

/// Deterministic maximum bipartite matching between restore candidates and
/// unmatched imports. Maximizes total matched plays first, breaking ties by
/// each candidate's ordered edge preference (refreshed metadata over queued
/// metadata; closer timestamp over farther; earlier import observation first).
fn match_reconcile_candidates(
    edges: &[Vec<(usize, Option<Snapshot>)>],
) -> BTreeMap<usize, (usize, Option<Snapshot>)> {
    let mut matched_import: HashMap<usize, (usize, Option<Snapshot>)> = HashMap::new();

    fn augment(
        u: usize,
        edges: &[Vec<(usize, Option<Snapshot>)>],
        seen: &mut BTreeSet<usize>,
        matched_import: &mut HashMap<usize, (usize, Option<Snapshot>)>,
    ) -> bool {
        let Some(adj) = edges.get(u) else {
            return false;
        };
        for (imp, refresh) in adj {
            if !matched_import.contains_key(imp) && seen.insert(*imp) {
                matched_import.insert(*imp, (u, refresh.clone()));
                return true;
            }
        }
        for (imp, refresh) in adj {
            if !seen.insert(*imp) {
                continue;
            }
            if let Some((prev, _)) = matched_import.get(imp).cloned()
                && augment(prev, edges, seen, matched_import)
            {
                matched_import.insert(*imp, (u, refresh.clone()));
                return true;
            }
        }
        false
    }

    for u in 0..edges.len() {
        if edges.get(u).is_some_and(|adj| !adj.is_empty()) {
            let mut seen = BTreeSet::new();
            augment(u, edges, &mut seen, &mut matched_import);
        }
    }

    matched_import
        .into_iter()
        .map(|(imp, (u, refresh))| (u, (imp, refresh)))
        .collect()
}

/// A second a restore candidate may have been sent at, with the names it
/// would have been sent under.
struct SentAt<'a> {
    second: i64,
    /// How far either side its clock may have put it.
    tolerance: i64,
    artist: &'a str,
    title: &'a str,
    /// The play's names now, when they are what it would have been sent
    /// under.
    refresh: Option<&'a Snapshot>,
}

/// Where a restore candidate may have been sent: a delivery at its own
/// second and names, and at its play's as they are now if it was never
/// tried; an unplanned listen at its second, within its clock's tolerance.
fn sent_at(candidate: &ReconcileCandidate) -> Vec<SentAt<'_>> {
    match candidate {
        ReconcileCandidate::Pending {
            queued_s,
            queued_artist,
            queued_title,
            refresh,
            ..
        } => {
            let mut found = vec![SentAt {
                second: *queued_s,
                tolerance: 0,
                artist: queued_artist,
                title: queued_title,
                refresh: None,
            }];
            if let Some(current) = refresh {
                found.push(SentAt {
                    second: current.timestamp_s,
                    tolerance: 0,
                    artist: &current.artist,
                    title: &current.title,
                    refresh: Some(current),
                });
            }
            found
        }
        ReconcileCandidate::Unplanned(listen) => vec![SentAt {
            second: listen.at_ms.div_euclid(1000),
            tolerance: restore_tolerance_s(&listen.clock_basis, listen.time_precision_ms),
            artist: &listen.artist,
            title: &listen.title,
            refresh: None,
        }],
    }
}

/// The second tier of restore matching, for what the first left: a play
/// and a scrobble that are alone in one exact second, whose names may be
/// one track the service corrected ([`corrected_track`]), as when Last.fm
/// corrected a title after the backup was taken and before its echo was
/// imported. A second that holds another candidate, by its time or within
/// its clock's tolerance, or another unmatched scrobble, is passed over, so
/// that a scrobble another player sent beside ours is never taken for it.
fn match_corrected(
    candidates: &[ReconcileCandidate],
    imports: &[UnmatchedImport],
    matched: &mut BTreeMap<usize, (usize, Option<Snapshot>)>,
) {
    let taken: BTreeSet<usize> = matched.values().map(|(import, _)| *import).collect();
    let mut claims: BTreeMap<i64, BTreeSet<usize>> = BTreeMap::new();
    for (index, candidate) in candidates.iter().enumerate() {
        for at in sent_at(candidate) {
            for near in
                at.second.saturating_sub(at.tolerance)..=at.second.saturating_add(at.tolerance)
            {
                claims.entry(near).or_default().insert(index);
            }
        }
    }
    let mut at_second: BTreeMap<i64, Vec<usize>> = BTreeMap::new();
    for (index, import) in imports.iter().enumerate() {
        at_second.entry(import.seconds).or_default().push(index);
    }
    for (index, candidate) in candidates.iter().enumerate() {
        if matched.contains_key(&index) {
            continue;
        }
        for at in sent_at(candidate) {
            if at.tolerance != 0 || claims.get(&at.second).map(BTreeSet::len) != Some(1) {
                continue;
            }
            let Some(&[import]) = at_second.get(&at.second).map(Vec::as_slice) else {
                continue;
            };
            let Some(found) = imports.get(import) else {
                continue;
            };
            if !taken.contains(&import)
                && corrected_track(at.artist, at.title, &found.artist, &found.title)
            {
                matched.insert(index, (import, at.refresh.cloned()));
                break;
            }
        }
    }
}

/// Tolerance in whole seconds for matching an unplanned listen during restore
/// reconciliation: exact second for `hub_age`, bounded by `time_precision_ms`
/// (5 seconds) for `server_wall` listens whose dating can shift after a reboot.
fn restore_tolerance_s(clock_basis: &str, time_precision_ms: i64) -> i64 {
    if clock_basis == "server_wall" {
        (time_precision_ms / 1000).max(0)
    } else {
        0
    }
}

/// Inserts an `accepted` delivery row for an unplanned listen reconciled
/// against an imported scrobble during or after restore, and returns its row ID.
fn insert_reconciled_delivery(
    write: &crate::library_store::Write<'_>,
    listen: &UnplannedListen,
    endpoint: i64,
    import: &UnmatchedImport,
) -> Result<i64, Error> {
    let connection = write.connection();
    connection
        .prepare_cached(
            "INSERT INTO history_delivery
               (observation_id, endpoint_id, state, sent_timestamp_s, artist,
                title, album, album_artist, track_number, duration_ms,
                accepted_artist, accepted_title, created_at_ms, updated_at_ms,
                rev, detail)
             VALUES (?1, ?2, 'accepted', ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11,
                     ?12, ?12, ?13, ?14)",
        )?
        .execute(rusqlite::params![
            listen.observation,
            endpoint,
            import.seconds,
            listen.artist,
            listen.title,
            listen.album,
            listen.album_artist,
            listen.track_number,
            listen.duration_ms,
            import.artist,
            import.title,
            write.now_ms(),
            write.rev(),
            RESTORED_DETAIL,
        ])?;
    Ok(connection.last_insert_rowid())
}

/// Records a bounded post-restore reconciliation window for `account` so late
/// satellite listens or deferred local outbox listens arriving after
/// `release_hold` still reconcile against already-imported scrobbles, and
/// prunes any expired windows.
fn record_restored_window(
    write: &crate::library_store::Write<'_>,
    account: Id,
    taken_at_ms: i64,
) -> Result<(), Error> {
    let connection = write.connection();
    let now = write.now_ms();
    let key = restored_key(account);
    let existing: Option<(i64, i64)> = connection
        .query_row(
            "SELECT json_extract(value_json, '$.takenAtMs'),
                    json_extract(value_json, '$.reconciledAtMs')
             FROM library_setting WHERE key = ?1",
            [&key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let (taken, reconciled) = existing.map_or((taken_at_ms, now), |(prev_taken, prev_rec)| {
        (prev_taken.min(taken_at_ms), prev_rec.max(now))
    });
    let value = serde_json::json!({
        "takenAtMs": taken,
        "reconciledAtMs": reconciled,
    });
    connection.execute(
        "INSERT INTO library_setting (key, value_json, rev)
         VALUES (?1, ?2, ?3)
         ON CONFLICT (key) DO UPDATE SET value_json = excluded.value_json, rev = excluded.rev",
        (&key, value.to_string(), write.rev()),
    )?;
    prune_expired_restored_windows(connection, now)?;
    Ok(())
}

fn prune_expired_restored_windows(connection: &Connection, now_ms: i64) -> Result<(), Error> {
    let cutoff = now_ms.saturating_sub(SEND_WITHIN_MS);
    connection.execute(
        "DELETE FROM library_setting
         WHERE key >= ?1 AND key < ?2
           AND json_extract(value_json, '$.reconciledAtMs') < ?3",
        (RESTORED_PREFIX, RESTORED_PREFIX_END, cutoff),
    )?;
    Ok(())
}

/// Reconciles a newly ingested listen against any active post-restore window
/// (for satellites reconnecting or deferred local outbox rows delivered after
/// `release_hold`). Runs inside the listen-ingest transaction before
/// `plan_deliveries` can observe the listen.
pub(crate) fn reconcile_late_listen(
    write: &crate::library_store::Write<'_>,
    observation: i64,
    rule: PlayRule,
) -> Result<(), Error> {
    let connection = write.connection();
    let windows: Vec<(String, i64, i64)> = connection
        .prepare_cached(
            "SELECT key,
                    json_extract(value_json, '$.takenAtMs'),
                    json_extract(value_json, '$.reconciledAtMs')
             FROM library_setting
             WHERE key >= ?1 AND key < ?2
             ORDER BY key",
        )?
        .query_map((RESTORED_PREFIX, RESTORED_PREFIX_END), |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?
        .collect::<Result<_, _>>()?;
    if windows.is_empty() {
        return Ok(());
    }
    let now = write.now_ms();
    let cutoff = now.saturating_sub(SEND_WITHIN_MS);
    let mut had_expired = false;
    for (key, taken_at_ms, reconciled_at_ms) in windows {
        if reconciled_at_ms < cutoff {
            had_expired = true;
            continue;
        }
        let Some(suffix) = key.strip_prefix(RESTORED_PREFIX) else {
            continue;
        };
        let Ok(account) = suffix.parse::<Id>() else {
            continue;
        };
        // If a newer backup hold is active for this account, wait for its
        // `release_hold` instead of reconciling prematurely.
        if held_of(connection, account)?.is_some() {
            continue;
        }
        let Ok((endpoint, listener)) = endpoint_of(connection, account) else {
            continue;
        };
        let min_at_ms = taken_at_ms.saturating_sub(SEND_WITHIN_MS);
        let listen = connection
            .prepare_cached(&format!(
                "SELECT primary_peo.observation_id, pe.event_at_utc_ms, pe.clock_basis,
                        pe.time_precision_ms, {NAMES}, pe.track_duration_ms
                 FROM play_event_observation mine
                 JOIN play_event pe ON pe.id = mine.play_event_id
                 JOIN play_event_observation primary_peo
                   ON primary_peo.play_event_id = pe.id AND primary_peo.role = 'primary'
                 JOIN history_observation o
                   ON o.id = primary_peo.observation_id AND o.kind = 'listen'
                 JOIN recording r ON r.id = pe.recording_id
                 LEFT JOIN track_appearance ta ON ta.id = pe.appearance_id
                 {NAME_JOINS}
                 WHERE mine.observation_id = ?1
                   AND pe.listener_id = ?2
                   AND pe.event_at_utc_ms BETWEEN ?4 AND ?5
                   AND pe.counts = 1 AND pe.excluded = 0 AND pe.evidence_class = 'observed'
                   AND pe.clock_basis IN ('hub_age', 'server_wall')
                   AND NOT EXISTS (SELECT 1 FROM play_event_observation x
                                   JOIN history_observation xo ON xo.id = x.observation_id
                                   WHERE x.play_event_id = pe.id AND xo.endpoint_id = ?3)
                   AND NOT EXISTS (SELECT 1 FROM history_delivery d
                                   JOIN play_event_observation y
                                     ON y.observation_id = d.observation_id
                                   WHERE y.play_event_id = pe.id AND d.endpoint_id = ?3)"
            ))?
            .query_row(
                (observation, listener, endpoint, min_at_ms, reconciled_at_ms),
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, Option<String>>(6)?,
                        row.get::<_, Option<String>>(7)?,
                        row.get::<_, Option<i64>>(8)?,
                        row.get::<_, Option<i64>>(9)?,
                    ))
                },
            )
            .optional()?;
        let Some((
            primary_obs,
            at_ms,
            clock_basis,
            time_precision_ms,
            Some(title),
            Some(artist),
            album,
            album_artist,
            track_number,
            duration_ms,
        )) = listen
        else {
            continue;
        };
        let play_s = at_ms.div_euclid(1000);
        let tolerance_s = restore_tolerance_s(&clock_basis, time_precision_ms);
        let imports = unmatched_imports_in(
            connection,
            endpoint,
            play_s.saturating_sub(tolerance_s),
            play_s.saturating_add(tolerance_s),
        )?;
        let named = imports
            .iter()
            .filter(|imp| same_track(&artist, &title, &imp.artist, &imp.title))
            .min_by_key(|imp| (imp.seconds.abs_diff(play_s), imp.observation));
        // As `match_corrected` does: a scrobble alone in the exact second of
        // a play alone in it may be the play's, under names the service
        // corrected.
        let corrected = || -> Result<Option<&UnmatchedImport>, Error> {
            let [only] = imports.as_slice() else {
                return Ok(None);
            };
            if tolerance_s != 0
                || only.seconds != play_s
                || !corrected_track(&artist, &title, &only.artist, &only.title)
            {
                return Ok(None);
            }
            let plays: i64 = connection.query_row(
                "SELECT count(*) FROM play_event pe
                 JOIN play_event_observation peo
                   ON peo.play_event_id = pe.id AND peo.role = 'primary'
                 JOIN history_observation o ON o.id = peo.observation_id AND o.kind = 'listen'
                 WHERE pe.listener_id = ?1 AND pe.event_at_utc_ms BETWEEN ?2 AND ?3
                   AND pe.counts = 1 AND pe.excluded = 0",
                (listener, play_s * 1000, play_s * 1000 + 999),
                |row| row.get(0),
            )?;
            Ok((plays == 1).then_some(only))
        };
        let matched = match named {
            Some(import) => Some(import.clone()),
            None => corrected()?.cloned(),
        };
        if let Some(import) = matched {
            let candidate = UnplannedListen {
                observation: primary_obs,
                at_ms,
                clock_basis,
                time_precision_ms,
                artist,
                title,
                album,
                album_artist,
                track_number,
                duration_ms,
            };
            let delivery = insert_reconciled_delivery(write, &candidate, endpoint, &import)?;
            rederive_delivery_by(write, delivery, rule)?;
        }
    }
    if had_expired {
        prune_expired_restored_windows(connection, now)?;
    }
    Ok(())
}

impl LibraryStore {
    /// Whether scrobbling for `account` is held back until an import has run
    /// for that account, as it is in a library restored from a backup.
    pub fn held_for_import(&self, account: Id) -> Result<Option<Held>, Error> {
        self.read(move |connection| held_of(connection, account))
    }

    /// Whether any account's scrobbling is held back until an import has run.
    pub fn any_held_for_import(&self) -> Result<Option<Held>, Error> {
        self.read(any_held)
    }

    /// At start: every account held in a library restored from a backup is
    /// stamped as held from now on, if it was not already (and any legacy
    /// global hold is expanded to per-account holds).
    ///
    /// Returns the hold relevant to this startup: if `active_account` is
    /// `Some`, that account's hold (if still active); if `active_account` is
    /// `None`, the earliest hold only when this open newly stamped or expanded
    /// a hold. That keeps dormant holds on unused accounts persisted without
    /// reporting a restore on every subsequent start.
    pub fn opened_restored(&self, active_account: Option<Id>) -> Result<Option<Held>, Error> {
        let needs_write = self.read(|connection| {
            let has_legacy_to_expand = held_row(connection, HELD_SETTING)?.is_some()
                && !service_accounts(connection)?.is_empty();
            let has_unstamped: bool = connection.query_row(
                "SELECT EXISTS (
                   SELECT 1 FROM library_setting
                   WHERE (key = ?1 OR (key >= ?2 AND key < ?3))
                     AND json_extract(value_json, '$.sinceMs') IS NULL
                 )",
                (HELD_SETTING, HELD_PREFIX, HELD_PREFIX_END),
                |row| row.get(0),
            )?;
            Ok(has_legacy_to_expand || has_unstamped)
        })?;
        if needs_write {
            self.write(
                WriteRequest {
                    method: DELIVERY_METHOD,
                    command: None,
                    client: None,
                },
                |write| {
                    expand_legacy_hold(write)?;
                    write.connection().execute(
                        "UPDATE library_setting
                         SET value_json = json_set(value_json, '$.sinceMs', ?4), rev = ?5
                         WHERE (key = ?1 OR (key >= ?2 AND key < ?3))
                           AND json_extract(value_json, '$.sinceMs') IS NULL",
                        (
                            HELD_SETTING,
                            HELD_PREFIX,
                            HELD_PREFIX_END,
                            write.now_ms(),
                            write.rev(),
                        ),
                    )?;
                    Ok(String::new())
                },
            )?;
        }
        match active_account {
            Some(account) => self.held_for_import(account),
            None if needs_write => self.any_held_for_import(),
            None => Ok(None),
        }
    }

    /// An import from `account` has run and brought back what the service
    /// has: scrobbling for `account` is held back no longer (other accounts'
    /// holds stay untouched). First, pending deliveries and unplanned listens
    /// for `account` within `SEND_WITHIN_MS` of the backup are matched
    /// 1-to-1 against active, non-excluded returning imports so every matched
    /// play is recorded as accepted and joins its echo without being sent
    /// again. A bounded post-restore window is kept so late-arriving satellite
    /// or deferred outbox listens still reconcile on ingest. Returns whether
    /// `account` was held.
    pub fn release_hold(&self, account: Id) -> Result<bool, Error> {
        self.release_hold_with(account, None)
    }

    /// As [`release_hold`](Self::release_hold), checking `cancel` inside
    /// `BEGIN IMMEDIATE` before any mutation.
    pub fn release_hold_guarded(
        &self,
        account: Id,
        cancel: crate::JobCancel,
    ) -> Result<bool, Error> {
        self.release_hold_with(account, Some(cancel))
    }

    fn release_hold_with(
        &self,
        account: Id,
        cancel: Option<crate::JobCancel>,
    ) -> Result<bool, Error> {
        if let Some(cancel) = &cancel {
            cancel.check()?;
        }
        if self.held_for_import(account)?.is_none() {
            return Ok(false);
        }
        let written = self.write_guarded(
            WriteRequest {
                method: DELIVERY_METHOD,
                command: None,
                client: None,
            },
            cancel,
            move |write| {
                expand_legacy_hold(write)?;
                let connection = write.connection();
                let Some(held) = held_of(connection, account)? else {
                    return Ok("0".to_owned());
                };
                let (endpoint, listener) = endpoint_of(connection, account)?;

                // 1. Gather all pending deliveries (on primary listens) and
                //    unplanned counted listens before mutating any row so
                //    equal-second and edited-metadata plays match globally.
                let waiting: Vec<(i64, i64, i64, String, String)> = connection
                    .prepare(
                        "SELECT d.id, d.attempts, d.sent_timestamp_s, d.artist, d.title
                         FROM history_delivery d
                         JOIN play_event_observation peo
                           ON peo.observation_id = d.observation_id AND peo.role = 'primary'
                         WHERE d.endpoint_id = ?1 AND d.state = 'pending'
                           AND NOT EXISTS (
                             SELECT 1 FROM play_event_observation x
                             JOIN history_observation xo ON xo.id = x.observation_id
                             WHERE x.play_event_id = peo.play_event_id
                               AND xo.endpoint_id = ?1
                           )
                         ORDER BY d.sent_timestamp_s, d.id",
                    )?
                    .query_map([endpoint], |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                        ))
                    })?
                    .collect::<Result<_, _>>()?;

                let mut candidates = Vec::with_capacity(waiting.len());
                for (delivery, attempts, queued_s, queued_artist, queued_title) in waiting {
                    let refresh = if attempts == 0 {
                        current_play_snapshot(connection, delivery)?.filter(|current| {
                            current.timestamp_s != queued_s
                                || !same_track(
                                    &current.artist,
                                    &current.title,
                                    &queued_artist,
                                    &queued_title,
                                )
                        })
                    } else {
                        None
                    };
                    candidates.push(ReconcileCandidate::Pending {
                        delivery,
                        queued_s,
                        queued_artist,
                        queued_title,
                        refresh,
                    });
                }

                let unplanned = unplanned_listens(
                    connection,
                    endpoint,
                    listener,
                    held.taken_at_ms.saturating_sub(SEND_WITHIN_MS),
                    ClockScope::HubAgeOrWall,
                )?;
                candidates.extend(unplanned.into_iter().map(ReconcileCandidate::Unplanned));

                // 2. Fetch active, non-excluded unmatched imports covering the
                //    candidate time range and compute a maximum 1-to-1 matching.
                let mut bounds: Option<(i64, i64)> = None;
                let mut widen = |lo: i64, hi: i64| {
                    bounds = Some(match bounds {
                        Some((cur_lo, cur_hi)) => (cur_lo.min(lo), cur_hi.max(hi)),
                        None => (lo, hi),
                    });
                };
                for candidate in &candidates {
                    match candidate {
                        ReconcileCandidate::Pending {
                            queued_s, refresh, ..
                        } => {
                            widen(*queued_s, *queued_s);
                            if let Some(snapshot) = refresh {
                                widen(snapshot.timestamp_s, snapshot.timestamp_s);
                            }
                        }
                        ReconcileCandidate::Unplanned(listen) => {
                            let play_s = listen.at_ms.div_euclid(1000);
                            let tol =
                                restore_tolerance_s(&listen.clock_basis, listen.time_precision_ms);
                            widen(play_s.saturating_sub(tol), play_s.saturating_add(tol));
                        }
                    }
                }

                let mut touched = Vec::new();
                if let Some((min_s, max_s)) = bounds {
                    let imports = unmatched_imports_in(connection, endpoint, min_s, max_s)?;
                    let mut by_second: BTreeMap<i64, Vec<usize>> = BTreeMap::new();
                    for (idx, imp) in imports.iter().enumerate() {
                        by_second.entry(imp.seconds).or_default().push(idx);
                    }

                    let edges: Vec<Vec<(usize, Option<Snapshot>)>> = candidates
                        .iter()
                        .map(|candidate| match candidate {
                            ReconcileCandidate::Pending {
                                queued_s,
                                queued_artist,
                                queued_title,
                                refresh,
                                ..
                            } => {
                                let mut adj = Vec::new();
                                let mut seen_imp = BTreeSet::new();
                                // Prefer refreshed current metadata first, then
                                // fall back to the queued snapshot.
                                if let Some(current) = refresh
                                    && let Some(at_second) = by_second.get(&current.timestamp_s)
                                {
                                    for &imp_idx in at_second {
                                        if let Some(imp) = imports.get(imp_idx)
                                            && same_track(
                                                &current.artist,
                                                &current.title,
                                                &imp.artist,
                                                &imp.title,
                                            )
                                            && seen_imp.insert(imp_idx)
                                        {
                                            adj.push((imp_idx, Some(current.clone())));
                                        }
                                    }
                                }
                                if let Some(at_second) = by_second.get(queued_s) {
                                    for &imp_idx in at_second {
                                        if let Some(imp) = imports.get(imp_idx)
                                            && same_track(
                                                queued_artist,
                                                queued_title,
                                                &imp.artist,
                                                &imp.title,
                                            )
                                            && seen_imp.insert(imp_idx)
                                        {
                                            adj.push((imp_idx, None));
                                        }
                                    }
                                }
                                adj
                            }
                            ReconcileCandidate::Unplanned(listen) => {
                                let play_s = listen.at_ms.div_euclid(1000);
                                let tol = restore_tolerance_s(
                                    &listen.clock_basis,
                                    listen.time_precision_ms,
                                );
                                let lo = play_s.saturating_sub(tol);
                                let hi = play_s.saturating_add(tol);
                                let mut matched: Vec<(u64, i64, usize)> = Vec::new();
                                for (&sec, indices) in by_second.range(lo..=hi) {
                                    for &imp_idx in indices {
                                        if let Some(imp) = imports.get(imp_idx)
                                            && same_track(
                                                &listen.artist,
                                                &listen.title,
                                                &imp.artist,
                                                &imp.title,
                                            )
                                        {
                                            matched.push((
                                                sec.abs_diff(play_s),
                                                imp.observation,
                                                imp_idx,
                                            ));
                                        }
                                    }
                                }
                                matched.sort_unstable();
                                matched
                                    .into_iter()
                                    .map(|(_, _, imp_idx)| (imp_idx, None))
                                    .collect()
                            }
                        })
                        .collect();

                    let mut matched = match_reconcile_candidates(&edges);
                    match_corrected(&candidates, &imports, &mut matched);
                    for (cand_idx, (imp_idx, chosen_refresh)) in matched {
                        let (Some(candidate), Some(import)) =
                            (candidates.get(cand_idx), imports.get(imp_idx))
                        else {
                            continue;
                        };
                        match candidate {
                            ReconcileCandidate::Pending { delivery, .. } => {
                                if let Some(current) = chosen_refresh {
                                    connection
                                        .prepare_cached(
                                            "UPDATE history_delivery
                                             SET state = 'accepted', sent_timestamp_s = ?2,
                                                 artist = ?3, title = ?4, album = ?5,
                                                 album_artist = ?6, track_number = ?7,
                                                 duration_ms = ?8, accepted_artist = ?9,
                                                 accepted_title = ?10, detail = ?11,
                                                 next_attempt_at_ms = NULL, updated_at_ms = ?12,
                                                 rev = ?13
                                             WHERE id = ?1",
                                        )?
                                        .execute(rusqlite::params![
                                            delivery,
                                            current.timestamp_s,
                                            current.artist,
                                            current.title,
                                            current.album,
                                            current.album_artist,
                                            current.track_number,
                                            current.duration_ms,
                                            import.artist,
                                            import.title,
                                            RESTORED_DETAIL,
                                            write.now_ms(),
                                            write.rev(),
                                        ])?;
                                } else {
                                    connection
                                        .prepare_cached(
                                            "UPDATE history_delivery
                                             SET state = 'accepted', accepted_artist = ?2,
                                                 accepted_title = ?3, detail = ?4,
                                                 next_attempt_at_ms = NULL, updated_at_ms = ?5,
                                                 rev = ?6
                                             WHERE id = ?1",
                                        )?
                                        .execute(rusqlite::params![
                                            delivery,
                                            import.artist,
                                            import.title,
                                            RESTORED_DETAIL,
                                            write.now_ms(),
                                            write.rev(),
                                        ])?;
                                }
                                touched.push(*delivery);
                            }
                            ReconcileCandidate::Unplanned(listen) => {
                                let delivery =
                                    insert_reconciled_delivery(write, listen, endpoint, import)?;
                                touched.push(delivery);
                            }
                        }
                    }
                }

                for delivery in touched {
                    rederive_delivery(write, delivery)?;
                }

                write.connection().execute(
                    "DELETE FROM library_setting WHERE key = ?1",
                    [held_key(account)],
                )?;
                record_restored_window(write, account, held.taken_at_ms)?;
                Ok("1".to_owned())
            },
        )?;
        Ok(written.result_json == "1")
    }
}

fn due_rows(connection: &Connection, rows: &[i64]) -> Result<Vec<Due>, Error> {
    let mut statement = connection.prepare_cached(
        "SELECT id, artist, title, album, album_artist, track_number, duration_ms,
                sent_timestamp_s
         FROM history_delivery WHERE id = ?1",
    )?;
    rows.iter()
        .map(|row| {
            Ok(statement.query_row([row], |row| {
                Ok(Due {
                    delivery: row.get(0)?,
                    artist: row.get(1)?,
                    title: row.get(2)?,
                    album: row.get(3)?,
                    album_artist: row.get(4)?,
                    track_number: row.get(5)?,
                    duration_ms: row.get(6)?,
                    timestamp_s: row.get(7)?,
                })
            })?)
        })
        .collect()
}
