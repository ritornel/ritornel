//! Scrobbling, as the library records it: which plays are eligible to be
//! sent to an account, the row that records each before it is sent, and
//! what became of it. See "Last.fm" and decision 3 in
//! docs/design/history.md.
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
//! exactly (`history_store::echo_of`).
//!
//! Eligible to scrobble: counted, dated, a trusted clock, `observed`, not
//! excluded, with no observation from that same account, and recent enough
//! for the service to take it. The rule is its own, pinned to the
//! service's, and equals play rule 1 today because a play counts by that
//! rule.

use rusqlite::{Connection, OptionalExtension};

use ritornel_domain::scrobble::same_track;

use crate::history_store::rederive_delivery;
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

/// Whether the account already has a scrobble of this track within a
/// second of `seconds` that is not an echo of ours: one another player
/// sent. Matched by normalised artist and title, as an echo is. An import
/// that is part of a play some listen here made, as its echo or beside it,
/// is that play's, and says nothing of another. This is not grouping by a
/// window of time, which M3 brings; it only keeps a play from being
/// scrobbled twice.
fn already_scrobbled(
    connection: &Connection,
    endpoint: i64,
    seconds: i64,
    artist: &str,
    title: &str,
) -> Result<bool, Error> {
    let mut statement = connection.prepare_cached(
        "SELECT i.artist, i.title FROM observation_import i
         JOIN history_observation o ON o.id = i.observation_id
         WHERE o.endpoint_id = ?1 AND i.played_at_utc_ms BETWEEN ?2 AND ?3
           AND NOT EXISTS (SELECT 1 FROM play_event_observation x
                           JOIN play_event_observation y ON y.play_event_id = x.play_event_id
                           JOIN history_observation yo
                             ON yo.id = y.observation_id AND yo.kind = 'listen'
                           WHERE x.observation_id = i.observation_id)",
    )?;
    let found: Vec<(String, String)> = statement
        .query_map(
            (endpoint, (seconds - 1) * 1000, (seconds + 1) * 1000),
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?
        .collect::<Result<_, _>>()?;
    Ok(found
        .iter()
        .any(|(their_artist, their_title)| same_track(artist, title, their_artist, their_title)))
}

/// Why a pending delivery may not be sent now, if it may not: the
/// eligibility rule asked again of its play as it is now, of its age
/// against what Last.fm takes, and of the time scrobbling was last turned
/// on.
fn not_eligible(
    connection: &Connection,
    delivery: i64,
    endpoint: i64,
    from_ms: i64,
    now_ms: i64,
) -> Result<Option<&'static str>, Error> {
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
                    pe.recording_id, {NAMES}
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
            })
        })
        .optional()?;
    let (seconds, artist, title, album, album_artist, track_number): (
        i64,
        String,
        String,
        Option<String>,
        Option<String>,
        Option<i64>,
    ) = connection
        .prepare_cached(
            "SELECT sent_timestamp_s, artist, title, album, album_artist, track_number
             FROM history_delivery WHERE id = ?1",
        )?
        .query_row([delivery], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        })?;
    if already_scrobbled(connection, endpoint, seconds, &artist, &title)? {
        return Ok(Some(ALREADY_SCROBBLED));
    }
    let Some(play) = play else {
        return Ok(Some("its play is no longer derived from this listen"));
    };
    let Some(at) = play.at else {
        return Ok(Some("its play has no date"));
    };
    Ok(if !play.counts {
        Some("its play no longer counts")
    } else if play.excluded {
        Some("its play is excluded")
    } else if play.clock != "hub_age" || play.evidence != "observed" {
        Some("its play is not dated by a trusted clock")
    } else if play.account_has_it {
        Some("the account already has this play")
    } else if play.sent_from_another {
        Some("its play was sent from another of its listens")
    } else if play.recording_id.is_none() {
        Some("its play is no longer bound to a recording")
    } else if play.artist.as_deref() != Some(artist.as_str())
        || play.title.as_deref() != Some(title.as_str())
        || play.album != album
        || play.album_artist != album_artist
        || play.track_number != track_number
    {
        Some("its play's track changed since it was queued")
    } else if at < now_ms - SEND_WITHIN_MS {
        Some("its play is too old for Last.fm to take")
    } else if at < from_ms {
        Some("its play is from before scrobbling was turned on")
    } else {
        None
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

/// One eligible play, as it would be sent.
type Eligible = (
    i64,
    i64,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<i64>,
    Option<i64>,
);

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
) -> Result<Vec<Eligible>, Error> {
    let mut statement = connection.prepare(&format!(
        "SELECT peo.observation_id, pe.event_at_utc_ms, {NAMES}, pe.track_duration_ms
         FROM play_event pe
         JOIN play_event_observation peo ON peo.play_event_id = pe.id AND peo.role = 'primary'
         JOIN history_observation o ON o.id = peo.observation_id AND o.kind = 'listen'
         JOIN recording r ON r.id = pe.recording_id
         LEFT JOIN track_appearance ta ON ta.id = pe.appearance_id
         {NAME_JOINS}
         WHERE pe.listener_id = ?1 AND pe.event_at_utc_ms >= ?3
           AND pe.counts = 1 AND pe.excluded = 0 AND pe.evidence_class = 'observed'
           AND pe.clock_basis = 'hub_age'
           AND NOT EXISTS (SELECT 1 FROM play_event_observation x
                           JOIN history_observation xo ON xo.id = x.observation_id
                           WHERE x.play_event_id = pe.id AND xo.endpoint_id = ?2)
           AND NOT EXISTS (SELECT 1 FROM history_delivery d
                           JOIN play_event_observation y ON y.observation_id = d.observation_id
                           WHERE y.play_event_id = pe.id AND d.endpoint_id = ?2
                             AND (d.observation_id = peo.observation_id
                                  OR d.state NOT IN ('pending', 'withdrawn')))
         ORDER BY pe.event_at_utc_ms"
    ))?;
    let rows = statement.query_map((listener, endpoint, from_ms), |row| {
        Ok((
            row.get(0)?,
            row.get(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get(4)?,
            row.get(5)?,
            row.get(6)?,
            row.get(7)?,
        ))
    })?;
    let mut found = Vec::new();
    for row in rows {
        let (observation, at, title, artist, album, album_artist, number, duration) = row?;
        // A play whose track shows no artist or no title cannot be sent.
        if let (Some(title), Some(artist)) = (title, artist) {
            found.push((
                observation,
                at,
                artist,
                title,
                album,
                album_artist,
                number,
                duration,
            ));
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
                        "SELECT {NAMES} FROM track_appearance ta
                         JOIN recording r ON r.id = ta.recording_id
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
                        ))
                    },
                )
                .optional()?;
            Ok(
                found.and_then(|(title, artist, album, album_artist, track_number)| {
                    Some(Playing {
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
                // Read again inside the transaction: what was eligible a
                // moment ago and is still.
                let found = eligible(connection, endpoint, listener, from_ms)?;
                let mut pending = 0;
                for (observation, at, artist, title, album, album_artist, number, duration) in
                    &found
                {
                    // A play another player already scrobbled to the account
                    // is recorded as withdrawn, so that it is not looked at
                    // again, and is never sent.
                    let seconds = at.div_euclid(1000);
                    let (state, detail) =
                        if already_scrobbled(connection, endpoint, seconds, artist, title)? {
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
                            observation,
                            endpoint,
                            seconds,
                            artist,
                            title,
                            album,
                            album_artist,
                            number,
                            duration,
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
    /// the time scrobbling was last turned on: one that may no longer be
    /// sent is `withdrawn`, with why, and is never sent. With none due it
    /// writes nothing.
    pub fn take_due_deliveries(
        &self,
        account: Id,
        from_ms: i64,
        limit: usize,
    ) -> Result<Vec<Due>, Error> {
        let now = self.clock_now_ms();
        let due = move |connection: &Connection| -> Result<Vec<Due>, Error> {
            let (endpoint, _) = endpoint_of(connection, account)?;
            let mut statement = connection.prepare_cached(
                "SELECT id, artist, title, album, album_artist, track_number, duration_ms,
                        sent_timestamp_s
                 FROM history_delivery
                 WHERE endpoint_id = ?1 AND state = 'pending'
                   AND (next_attempt_at_ms IS NULL OR next_attempt_at_ms <= ?2)
                 ORDER BY sent_timestamp_s, id",
            )?;
            let rows = statement.query_map((endpoint, now), |row| {
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
            })?;
            Ok(rows.collect::<Result<_, _>>()?)
        };
        if self.read(due)?.is_empty() {
            return Ok(Vec::new());
        }
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
                for row in due(connection)? {
                    if taken.len() == limit {
                        break;
                    }
                    if let Some(why) =
                        not_eligible(connection, row.delivery, endpoint, from_ms, now)?
                    {
                        connection
                            .prepare_cached(
                                "UPDATE history_delivery
                                 SET state = 'withdrawn', detail = ?2, updated_at_ms = ?3,
                                     rev = ?4
                                 WHERE id = ?1",
                            )?
                            .execute((row.delivery, why, write.now_ms(), write.rev()))?;
                        continue;
                    }
                    // The service ties an answer to a play by its time alone.
                    if !seconds.insert(row.timestamp_s) {
                        continue;
                    }
                    connection
                        .prepare_cached(
                            "UPDATE history_delivery
                             SET state = 'sending', attempts = attempts + 1, updated_at_ms = ?2,
                                 rev = ?3
                             WHERE id = ?1",
                        )?
                        .execute((row.delivery, write.now_ms(), write.rev()))?;
                    rederive_delivery(write, row.delivery)?;
                    taken.push(row.delivery);
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
                    connection
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

/// The setting that holds back every scrobble of a library restored from
/// a backup until an import has run. The service may have taken plays
/// after the backup was taken, which the library restored still has
/// waiting; an import brings them back, and they are then not sent again.
/// It is written into the backup's copy, never into the library copied,
/// so it is there only once that copy is put back. See [`hold_sending`].
pub(crate) const HELD_SETTING: &str = "scrobbling.held_until_import";

/// Writes [`HELD_SETTING`] into the library at `connection`, a backup's
/// copy, taken at `taken_at_ms`. Its revision is the copy's last, since
/// the copy is not a write of the library's own.
pub(crate) fn hold_sending(connection: &Connection, taken_at_ms: i64) -> Result<(), Error> {
    connection.execute(
        "INSERT INTO library_setting (key, value_json, rev)
         VALUES (?1, json_object('takenAtMs', ?2),
                 (SELECT coalesce(max(rev), 0) FROM library_revision))
         ON CONFLICT (key) DO UPDATE SET value_json = excluded.value_json",
        (HELD_SETTING, taken_at_ms),
    )?;
    Ok(())
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

fn held(connection: &Connection) -> Result<Option<Held>, Error> {
    Ok(connection
        .query_row(
            "SELECT json_extract(value_json, '$.takenAtMs'),
                    json_extract(value_json, '$.sinceMs')
             FROM library_setting WHERE key = ?1",
            [HELD_SETTING],
            |row| {
                Ok(Held {
                    taken_at_ms: row.get(0)?,
                    since_ms: row.get(1)?,
                })
            },
        )
        .optional()?)
}

impl LibraryStore {
    /// Whether scrobbling is held back until an import has run, as it is
    /// in a library restored from a backup.
    pub fn held_for_import(&self) -> Result<Option<Held>, Error> {
        self.read(held)
    }

    /// At start: a library restored from a backup is held from now on, if
    /// it was not already. Returns the hold, if there is one.
    pub fn opened_restored(&self) -> Result<Option<Held>, Error> {
        match self.held_for_import()? {
            Some(Held { since_ms: None, .. }) => {}
            other => return Ok(other),
        }
        self.write(
            WriteRequest {
                method: DELIVERY_METHOD,
                command: None,
                client: None,
            },
            |write| {
                write.connection().execute(
                    "UPDATE library_setting
                     SET value_json = json_set(value_json, '$.sinceMs', ?2), rev = ?3
                     WHERE key = ?1 AND json_extract(value_json, '$.sinceMs') IS NULL",
                    (HELD_SETTING, write.now_ms(), write.rev()),
                )?;
                Ok(String::new())
            },
        )?;
        self.held_for_import()
    }

    /// An import from the account has run and brought back what the
    /// service has: scrobbling is held back no longer. First, each play
    /// still waiting that the service took after the backup, which the
    /// import brought back at the very second it was to be sent with and
    /// under the names it was to be sent with, is recorded as accepted, so
    /// that the import joins its play as its echo and it is not sent again.
    /// Whether scrobbling was held.
    pub fn release_hold(&self, account: Id) -> Result<bool, Error> {
        if self.held_for_import()?.is_none() {
            return Ok(false);
        }
        self.write(
            WriteRequest {
                method: DELIVERY_METHOD,
                command: None,
                client: None,
            },
            move |write| {
                let connection = write.connection();
                let (endpoint, _) = endpoint_of(connection, account)?;
                let waiting: Vec<(i64, i64, String, String)> = connection
                    .prepare(
                        "SELECT id, sent_timestamp_s, artist, title FROM history_delivery
                         WHERE endpoint_id = ?1 AND state = 'pending'
                         ORDER BY sent_timestamp_s, id",
                    )?
                    .query_map([endpoint], |row| {
                        Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                    })?
                    .collect::<Result<_, _>>()?;
                for (delivery, seconds, artist, title) in waiting {
                    // An import already part of a play some listen made is
                    // that play's.
                    let found: Vec<(String, String)> = connection
                        .prepare_cached(
                            "SELECT i.artist, i.title FROM observation_import i
                             JOIN history_observation o ON o.id = i.observation_id
                             WHERE o.endpoint_id = ?1 AND i.played_at_utc_ms = ?2
                               AND NOT EXISTS (
                                 SELECT 1 FROM play_event_observation x
                                 JOIN play_event_observation y
                                   ON y.play_event_id = x.play_event_id
                                 JOIN history_observation yo
                                   ON yo.id = y.observation_id AND yo.kind = 'listen'
                                 WHERE x.observation_id = i.observation_id)
                             ORDER BY o.id",
                        )?
                        .query_map((endpoint, seconds * 1000), |row| {
                            Ok((row.get(0)?, row.get(1)?))
                        })?
                        .collect::<Result<_, _>>()?;
                    let Some((took_artist, took_title)) =
                        found.into_iter().find(|(their_artist, their_title)| {
                            same_track(&artist, &title, their_artist, their_title)
                        })
                    else {
                        continue;
                    };
                    connection
                        .prepare_cached(
                            "UPDATE history_delivery
                             SET state = 'accepted', accepted_artist = ?2, accepted_title = ?3,
                                 detail = ?4, next_attempt_at_ms = NULL, updated_at_ms = ?5,
                                 rev = ?6
                             WHERE id = ?1",
                        )?
                        .execute(rusqlite::params![
                            delivery,
                            took_artist,
                            took_title,
                            "the service had it already when the library was restored",
                            write.now_ms(),
                            write.rev(),
                        ])?;
                    rederive_delivery(write, delivery)?;
                }
                connection.execute("DELETE FROM library_setting WHERE key = ?1", [HELD_SETTING])?;
                Ok(String::new())
            },
        )?;
        Ok(true)
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
