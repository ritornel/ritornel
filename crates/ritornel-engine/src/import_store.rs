//! Plays another service reported: the account they come from, the import
//! records and their cursor through the one ingest call, the text-only
//! items that name what each was of and their matching to the library, the
//! loved tracks, and the report on how much of it matched. See "Last.fm" in
//! docs/internals/history.md and "Scrobble matching" in
//! docs/internals/identity.md.
//!
//! A service's plays are observations of the account's own history
//! endpoint, each with an `observation_import` detail and the record
//! verbatim as its payload. What a play was of is a `scrobble_subject`
//! source item of the account's source instance, one for each distinct
//! artist, album and title, which binds to a recording only on a confident
//! match ([`ritornel_domain::scrobble`]) and otherwise stays unbound. A love
//! is a `listener_rating` row naming such an item, with no album.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use ritornel_domain::NORMALIZER_VERSION;
use ritornel_domain::scrobble::{
    Appearance, Catalog, MATCHER_VERSION, Recording, Rule, Scrobbled, Verdict,
};
use ritornel_source::{Batch, Love, LovesStored, Stored};
use rusqlite::{Connection, OptionalExtension};

use crate::history_store::{INGEST_METHOD, rebind, rederive};
use crate::history_streams::IN_FORCE;
use crate::library_store::Write;
use crate::{Error, Id, LibraryStore, WriteRequest};

/// The services an account can be at.
pub const SERVICES: &[&str] = &["lastfm"];

/// What a write that changes the loved tracks is called.
pub const LOVES_METHOD: &str = "history.loves";

/// What a write that binds scrobbled tracks after the library changed is
/// called.
pub const MATCH_METHOD: &str = "identity.matchScrobbles";

/// A subject item with its text: its row, artist, album and title.
type Subject = (i64, String, Option<String>, String);

/// What a subject is bound to: a recording, and an appearance where one is
/// known.
type Bound = (i64, Option<i64>);

/// What a track shows: its canonical recording's row and ID, its title,
/// artist and album.
type Shown = (
    i64,
    [u8; 16],
    Option<String>,
    Option<String>,
    Option<String>,
);

/// The item kind of what a scrobble names.
const SUBJECT: &str = "scrobble_subject";

/// One account at a service, as the library has it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceAccount {
    /// The account's ID.
    pub account: Id,
    /// The service: `lastfm`.
    pub service: String,
    /// The account's name, as the service spells it.
    pub name: String,
    /// Whose plays these are.
    pub listener: Id,
    /// The history endpoint its plays are observations of.
    pub endpoint: Id,
}

/// The rows an account is made of.
#[derive(Debug, Clone, Copy)]
struct Rows {
    endpoint: i64,
    instance: i64,
    listener: i64,
}

fn rows_of(connection: &Connection, account: Id) -> Result<Rows, Error> {
    connection
        .query_row(
            "SELECT endpoint_id, source_instance_id, listener_id FROM service_account
             WHERE public_id = ?1",
            [account.as_bytes()],
            |row| {
                Ok(Rows {
                    endpoint: row.get(0)?,
                    instance: row.get(1)?,
                    listener: row.get(2)?,
                })
            },
        )
        .optional()?
        .ok_or(Error::NotFound("no such service account"))
}

/// What a scrobbled track is keyed by among its account's items.
fn provider_key(artist: &str, album: Option<&str>, title: &str) -> String {
    serde_json::to_string(&(artist, album, title)).unwrap_or_default()
}

/// The text a subject's key holds.
fn subject_text(key: &str) -> Option<(String, Option<String>, String)> {
    serde_json::from_str(key).ok()
}

/// The subject item for this text, made if it is new. The second value says
/// whether it was made.
fn subject(
    write: &Write<'_>,
    instance: i64,
    artist: &str,
    album: Option<&str>,
    title: &str,
) -> Result<(i64, bool), Error> {
    let connection = write.connection();
    let key = provider_key(artist, album, title);
    let found: Option<i64> = connection
        .prepare_cached(
            "SELECT id FROM source_item WHERE source_instance_id = ?1 AND item_kind = ?2
               AND provider_key = ?3 AND retired_at_ms IS NULL",
        )?
        .query_row((instance, SUBJECT, &key), |row| row.get(0))
        .optional()?;
    if let Some(found) = found {
        return Ok((found, false));
    }
    connection
        .prepare_cached(
            "INSERT INTO source_item
               (public_id, source_instance_id, item_kind, provider_key, first_seen_at_ms, rev)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?
        .execute((
            write.new_id()?.as_bytes(),
            instance,
            SUBJECT,
            &key,
            write.now_ms(),
            write.rev(),
        ))?;
    Ok((connection.last_insert_rowid(), true))
}

/// The library as the matcher reads it: every canonical recording a file
/// reaches, with what it and each of its appearances show.
fn catalog(connection: &Connection) -> Result<Catalog, Error> {
    Ok(Catalog::new(catalog_recordings(connection)?))
}

/// The recordings of [`catalog`].
pub(crate) fn catalog_recordings(connection: &Connection) -> Result<Vec<Recording>, Error> {
    let mut recordings: BTreeMap<i64, Recording> = BTreeMap::new();
    let mut statement = connection.prepare(
        "SELECT r.id, r.display_title, ac.display_text FROM recording r
         LEFT JOIN artist_credit ac ON ac.id = r.display_artist_credit_id
         WHERE r.redirect_to_id IS NULL",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(Recording {
            recording: row.get(0)?,
            title: row.get(1)?,
            artist: row.get(2)?,
            appearances: Vec::new(),
        })
    })?;
    for recording in rows {
        let recording = recording?;
        recordings.insert(recording.recording, recording);
    }
    // Only what a file reaches: a song or a record whose files were all
    // retagged onto another keeps its rows, and a play is not bound to it.
    let mut statement = connection.prepare(&format!(
        "SELECT ta.id, ta.recording_id, r.canonical_id, ta.display_title, ac.display_text,
                rel.display_title
         FROM track_appearance ta
         JOIN recording r ON r.id = ta.recording_id
         LEFT JOIN artist_credit ac ON ac.id = ta.display_artist_credit_id
         LEFT JOIN release rel ON rel.id = ta.release_id
         WHERE ta.redirect_to_id IS NULL AND {}",
        crate::list::REACHED_TA
    ))?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, i64>(2)?,
            Appearance {
                appearance: row.get(0)?,
                recording: row.get(1)?,
                title: row.get(3)?,
                artist: row.get(4)?,
                album: row.get(5)?,
            },
        ))
    })?;
    for row in rows {
        let (canonical, appearance) = row?;
        if let Some(recording) = recordings.get_mut(&canonical) {
            recording.appearances.push(appearance);
        }
    }
    Ok(recordings
        .into_values()
        .filter(|recording| !recording.appearances.is_empty())
        .collect())
}

/// What the matcher would bind a subject item to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Wanted {
    recording: i64,
    appearance: Option<i64>,
    candidates: usize,
    rule: Rule,
}

/// Why the matcher did not bind a subject item (recorded on an `unbind`
/// decision when an earlier binding is released).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct UnbindReason {
    verdict: &'static str,
    doubt: Option<&'static str>,
    candidates: usize,
}

/// What the matcher says of a subject's text: either a binding or why it
/// leaves the item unbound.
fn wanted(
    catalog: &Catalog,
    except: &HashSet<i64>,
    artist: &str,
    album: Option<&str>,
    title: &str,
) -> Result<Wanted, UnbindReason> {
    match catalog.judge_except(
        Scrobbled {
            artist,
            album,
            title,
        },
        except,
    ) {
        Verdict::Bind {
            recording,
            appearance,
            candidates,
            rule,
        } => Ok(Wanted {
            recording,
            appearance,
            candidates,
            rule,
        }),
        Verdict::Doubtful { candidates, doubt } => Err(UnbindReason {
            verdict: "doubtful",
            doubt: Some(doubt.as_str()),
            candidates: candidates.len(),
        }),
        Verdict::Absent => Err(UnbindReason {
            verdict: "absent",
            doubt: None,
            candidates: 0,
        }),
    }
}

/// The binding in force on an item, when the matcher made it or there is
/// none: its assignment, and the recording and appearance it ends at
/// through redirects. `Err` holds back an item the owner bound.
type Current = Option<(i64, i64, Option<i64>)>;

fn current(connection: &Connection, item: i64) -> Result<Result<Current, ()>, Error> {
    let found: Option<(i64, i64, Option<i64>, String)> = connection
        .prepare_cached(
            "SELECT a.id, r.canonical_id, ta.canonical_id, d.actor_kind FROM assignment a
             JOIN identity_decision d ON d.id = a.decision_id
             JOIN recording r ON r.id = a.recording_id
             LEFT JOIN track_appearance ta ON ta.id = a.appearance_id
             WHERE a.source_item_id = ?1 AND a.superseded_at_ms IS NULL",
        )?
        .query_row([item], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .optional()?;
    Ok(match found {
        Some((.., actor)) if actor != "resolver" => Err(()),
        Some((id, recording, appearance, _)) => Ok(Some((id, recording, appearance))),
        None => Ok(None),
    })
}

/// The recordings the owner said each item is not, as the canonical
/// recordings they end at, so that a merge cannot carry a binding past
/// one.
type Exclusions = HashMap<i64, HashSet<i64>>;

fn exclusions(connection: &Connection) -> Result<Exclusions, Error> {
    let mut statement = connection.prepare(
        "SELECT e.source_item_id, x.canonical_id FROM identity_exclusion e
         JOIN recording x ON x.id = e.recording_id
         JOIN source_item si ON si.id = e.source_item_id
         WHERE si.item_kind IN ('scrobble_subject', 'spotify_track')",
    )?;
    let mut found: Exclusions = HashMap::new();
    for row in statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))? {
        let (item, recording): (i64, i64) = row?;
        found.entry(item).or_default().insert(recording);
    }
    Ok(found)
}

/// Whether the binding in force, as the recording and appearance it ends at
/// through redirects, is what is wanted: both the recording its plays count
/// for and the appearance. A binding a merge carries there is kept, so that
/// undoing the merge gives it back. The matcher names a canonical
/// appearance, and that appearance's own recording.
fn holds(
    connection: &Connection,
    now: Option<Bound>,
    wanted: Option<Wanted>,
) -> Result<bool, Error> {
    Ok(match (now, wanted) {
        (None, None) => true,
        (Some((recording, appearance)), Some(wanted)) => {
            let canonical: i64 = connection
                .prepare_cached("SELECT canonical_id FROM recording WHERE id = ?1")?
                .query_row([wanted.recording], |row| row.get(0))?;
            recording == canonical && appearance == wanted.appearance
        }
        _ => false,
    })
}

/// Binds a subject item as the matcher says: a binding in force that
/// differs is superseded, and a new one is a decision of its own, the
/// assignment and the item's cached recording. With nothing wanted an
/// existing binding is superseded and recorded with an `unbind` decision.
/// Returns whether its binding changed.
fn settle(
    write: &Write<'_>,
    item: i64,
    text: (&str, Option<&str>, &str),
    outcome: Result<Wanted, UnbindReason>,
) -> Result<bool, Error> {
    let connection = write.connection();
    // The owner may have bound it since it was judged.
    let Ok(now) = current(connection, item)? else {
        return Ok(false);
    };
    let wanted = outcome.ok();
    if holds(
        connection,
        now.map(|(_, recording, appearance)| (recording, appearance)),
        wanted,
    )? {
        return Ok(false);
    }
    if let Some((old, ..)) = now {
        connection
            .prepare_cached("UPDATE assignment SET superseded_at_ms = ?2, rev = ?3 WHERE id = ?1")?
            .execute((old, write.now_ms(), write.rev()))?;
    }
    let (artist, album, title) = text;
    let wanted = match outcome {
        Ok(wanted) => wanted,
        Err(reason) => {
            let evidence = serde_json::json!({
                "matcher": MATCHER_VERSION,
                "artist": artist,
                "album": album,
                "title": title,
                "verdict": reason.verdict,
                "doubt": reason.doubt,
                "candidates": reason.candidates,
            });
            connection
                .prepare_cached(
                    "INSERT INTO identity_decision
                       (action, source_item_id, method, resolver_version, normalizer_version,
                        score, evidence_json, needs_review, actor_kind, decided_at_ms, rev)
                     VALUES ('unbind', ?1, 'scrobble_text', ?2, ?3, NULL, ?4, 0, 'resolver', ?5, ?6)",
                )?
                .execute(rusqlite::params![
                    item,
                    MATCHER_VERSION,
                    NORMALIZER_VERSION,
                    evidence.to_string(),
                    write.now_ms(),
                    write.rev(),
                ])?;
            connection
                .prepare_cached(
                    "UPDATE source_item SET current_recording_id = NULL, rev = ?2 WHERE id = ?1",
                )?
                .execute((item, write.rev()))?;
            return Ok(true);
        }
    };
    let evidence = serde_json::json!({
        "matcher": MATCHER_VERSION,
        "artist": artist,
        "album": album,
        "title": title,
        "candidates": wanted.candidates,
        "byAlbum": wanted.candidates > 1,
        "rule": wanted.rule.as_str(),
    });
    connection
        .prepare_cached(
            "INSERT INTO identity_decision
               (action, source_item_id, method, resolver_version, normalizer_version, score,
                evidence_json, needs_review, actor_kind, decided_at_ms, rev)
             VALUES ('bind', ?1, 'scrobble_text', ?2, ?3, 0, ?4, 0, 'resolver', ?5, ?6)",
        )?
        .execute(rusqlite::params![
            item,
            MATCHER_VERSION,
            NORMALIZER_VERSION,
            evidence.to_string(),
            write.now_ms(),
            write.rev(),
        ])?;
    let decision = connection.last_insert_rowid();
    connection
        .prepare_cached(
            "INSERT INTO assignment
               (source_item_id, recording_id, appearance_id, decision_id, effective_from_ms, rev)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?
        .execute((
            item,
            wanted.recording,
            wanted.appearance,
            decision,
            write.now_ms(),
            write.rev(),
        ))?;
    if let Some((old, ..)) = now {
        connection
            .prepare_cached("UPDATE assignment SET superseded_by_id = ?2 WHERE id = ?1")?
            .execute((old, connection.last_insert_rowid()))?;
    }
    connection
        .prepare_cached(
            "UPDATE source_item SET current_recording_id =
               (SELECT canonical_id FROM recording WHERE id = ?2), rev = ?3
             WHERE id = ?1",
        )?
        .execute((item, wanted.recording, write.rev()))?;
    Ok(true)
}

/// Which tracks [`changes`] judges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scope {
    /// Every account's scrobbled tracks, and Spotify's tracks.
    Library,
    /// Spotify's tracks alone: what a run of streams applied or undone can
    /// move.
    Spotify,
    /// One account's scrobbled tracks, by its source instance: what its
    /// sync can move.
    Account(i64),
}

/// A track whose binding the matcher would change: its source instance,
/// its item with its text, and what the matcher says of it.
type Change = (i64, Subject, Result<Wanted, UnbindReason>);

/// Every track in scope whose binding in force is not what the matcher
/// says, the owner's own bindings left out. A track of Spotify's that no
/// stream in force names is released, when the matcher bound it.
fn changes(connection: &Connection, scope: Scope) -> Result<Vec<Change>, Error> {
    // Spotify's tracks are judged but for one account's sync.
    let account_only = matches!(scope, Scope::Account(_));
    let mut judged = if account_only {
        Vec::new()
    } else {
        judged_tracks(connection)?
    };
    let instances: Vec<i64> = match scope {
        Scope::Library => connection
            .prepare("SELECT source_instance_id FROM service_account ORDER BY id")?
            .query_map([], |row| row.get(0))?
            .collect::<Result<_, _>>()?,
        Scope::Spotify => Vec::new(),
        Scope::Account(instance) => vec![instance],
    };
    for instance in instances {
        judged.extend(
            judged_subjects(connection, instance)?
                .into_iter()
                .map(|(subject, now)| (instance, subject, now)),
        );
    }
    let mut found = Vec::new();
    let unnamed = if account_only {
        Vec::new()
    } else {
        unnamed_tracks(connection)?
    };
    for (instance, item, now) in unnamed {
        if now.is_some() {
            let unnamed = UnbindReason {
                verdict: "unnamed",
                doubt: None,
                candidates: 0,
            };
            found.push((
                instance,
                (item, String::new(), None, String::new()),
                Err(unnamed),
            ));
        }
    }
    if judged.is_empty() {
        return Ok(found);
    }
    let catalog = catalog(connection)?;
    let except = exclusions(connection)?;
    let none = HashSet::new();
    for (instance, subject, now) in judged {
        let (item, artist, album, title) = &subject;
        let except = except.get(item).unwrap_or(&none);
        let wanted = wanted(&catalog, except, artist, album.as_deref(), title);
        if !holds(connection, now, wanted.as_ref().ok().copied())? {
            found.push((instance, subject, wanted));
        }
    }
    Ok(found)
}

/// Binds the tracks in scope as the matcher says, in the write, and derives
/// the plays of those that moved again. Returns how many bindings changed,
/// by source instance.
pub(crate) fn match_in(write: &Write<'_>, scope: Scope) -> Result<BTreeMap<i64, usize>, Error> {
    let changed = bind_in(write, scope)?;
    if !changed.is_empty() {
        rebind(write)?;
    }
    Ok(changed)
}

/// Binds the tracks in scope as the matcher says, in the write, and leaves
/// their plays as they are: for a write that derives every play next.
pub(crate) fn bind_in(write: &Write<'_>, scope: Scope) -> Result<BTreeMap<i64, usize>, Error> {
    let mut changed: BTreeMap<i64, usize> = BTreeMap::new();
    for (instance, (item, artist, album, title), outcome) in changes(write.connection(), scope)? {
        if settle(write, item, (&artist, album.as_deref(), &title), outcome)? {
            *changed.entry(instance).or_default() += 1;
        }
    }
    Ok(changed)
}

/// Judges these subject items and binds them as the matcher says. Returns
/// how many bindings changed.
fn match_items(write: &Write<'_>, catalog: &Catalog, items: &[Subject]) -> Result<usize, Error> {
    let except = exclusions(write.connection())?;
    let none = HashSet::new();
    let mut changed = 0;
    for (item, artist, album, title) in items {
        let except = except.get(item).unwrap_or(&none);
        let wanted = wanted(catalog, except, artist, album.as_deref(), title);
        if settle(write, *item, (artist, album.as_deref(), title), wanted)? {
            changed += 1;
        }
    }
    Ok(changed)
}

/// The keys among `keys` that the endpoint has stored.
fn stored_keys(
    connection: &Connection,
    endpoint: i64,
    keys: impl Iterator<Item = impl AsRef<[u8]>>,
) -> Result<HashSet<Vec<u8>>, Error> {
    let mut statement = connection.prepare_cached(
        "SELECT 1 FROM history_observation WHERE endpoint_id = ?1 AND source_key = ?2",
    )?;
    let mut found = HashSet::new();
    for key in keys {
        if statement.exists((endpoint, key.as_ref()))? {
            found.insert(key.as_ref().to_vec());
        }
    }
    Ok(found)
}

/// How many imports the endpoint holds in the batch's span that the batch
/// does not have.
fn gone(connection: &Connection, endpoint: i64, batch: &Batch) -> Result<u64, Error> {
    let keys: HashSet<&[u8]> = batch
        .records
        .iter()
        .map(|record| record.source_key.as_slice())
        .collect();
    let mut statement = connection.prepare_cached(
        "SELECT o.source_key FROM observation_import i
         JOIN history_observation o ON o.id = i.observation_id
         WHERE o.endpoint_id = ?1 AND i.played_at_utc_ms >= ?2 AND i.played_at_utc_ms < ?3",
    )?;
    let held: Vec<Vec<u8>> = statement
        .query_map((endpoint, batch.span.from_ms, batch.span.to_ms), |row| {
            row.get(0)
        })?
        .collect::<Result<_, _>>()?;
    Ok(held
        .iter()
        .filter(|key| !keys.contains(key.as_slice()))
        .count() as u64)
}

/// What an import set aside as a duplicate says, in its correction.
pub(crate) const DUPLICATE_NOTE: &str = "set aside by a full sync: the source lists as many \
     scrobbles in this second as its first read stored, with fewer of this one, as a read \
     whose pages shifted leaves";

/// What such a correction is marked with in its payload, as its `reason`:
/// how a later full sync knows it for its own, whatever its note says.
const SHIFTED_READ: &str = "shifted_read";

/// What a revocation of such a correction says, when the source lists the
/// import again.
const LISTED_AGAIN_NOTE: &str = "the source lists this scrobble again";

/// What a full read finds of what an earlier read stored wrong in the
/// batch's span.
#[derive(Debug, Default)]
struct Recheck {
    /// The imports to set aside.
    set_aside: Vec<i64>,
    /// The corrections that set aside an import the source lists again,
    /// to revoke.
    revoke: Vec<i64>,
}

/// Finds, in the batch's span, what an earlier read stored wrong. A read
/// whose pages shifted between requests showed one scrobble of a second
/// twice and another not at all. That is the one shape taken: a second
/// whose first read, the write that first stored imports of it, stored as
/// many as the source lists now, but more of one track and fewer of
/// another. Of the track it stored more of, the imports the source does not
/// list are set aside, never so many that fewer of it count than the
/// source lists. A later read may have
/// stored the one missed since, which changes nothing here. A scrobble
/// deleted at the source, or one added to it later, changes how many the
/// second holds, and is never taken for one. An import whose play holds a
/// listen is never set aside, so that a play of this library's own that an
/// echo joined is never left out; one a stream joined may be, as a scrobble
/// set aside is offered to no stream. One set aside before, even if that was revoked since, is
/// not set aside again. An import set aside so whose key the source lists
/// again has its correction revoked, if it stands.
fn recheck(connection: &Connection, endpoint: i64, batch: &Batch) -> Result<Recheck, Error> {
    type Track = (String, Option<String>, String);
    let keys: HashSet<&[u8]> = batch
        .records
        .iter()
        .map(|record| record.source_key.as_slice())
        .collect();
    // Each second's scrobbles, as the source lists them and as the
    // library holds them, by track.
    let mut listed: HashMap<i64, HashMap<Track, usize>> = HashMap::new();
    for record in &batch.records {
        let track = &record.track;
        *listed
            .entry(record.played_at_ms)
            .or_default()
            .entry((
                track.artist.clone(),
                track.album.clone(),
                track.title.clone(),
            ))
            .or_default() += 1;
    }
    struct Held {
        observation: i64,
        rev: i64,
        listed: bool,
        track: Track,
        excluded: bool,
        takeable: bool,
        set_aside_by: Option<i64>,
    }
    let mut statement = connection.prepare_cached(
        "SELECT o.id, o.rev, o.source_key, i.played_at_utc_ms, i.artist, i.album, i.title,
                EXISTS (SELECT 1 FROM play_event_observation peo
                        JOIN play_event pe ON pe.id = peo.play_event_id
                        WHERE peo.observation_id = o.id AND pe.excluded = 1
                          AND NOT EXISTS (
                            SELECT 1 FROM play_event_observation primary_of
                            JOIN observation_import_stream s
                              ON s.observation_id = primary_of.observation_id
                            WHERE primary_of.play_event_id = pe.id
                              AND primary_of.role = 'primary')),
                NOT EXISTS (SELECT 1 FROM correction_target t
                            JOIN observation_correction c ON c.observation_id = t.correction_id
                            WHERE t.target_observation_id = o.id AND c.verb = 'exclude')
                AND NOT EXISTS (SELECT 1 FROM play_event_observation mine
                                JOIN play_event_observation other
                                  ON other.play_event_id = mine.play_event_id
                                JOIN observation_listen l ON l.observation_id = other.observation_id
                                WHERE mine.observation_id = o.id),
                (SELECT max(c.observation_id) FROM correction_target t
                 JOIN observation_correction c ON c.observation_id = t.correction_id
                 JOIN history_observation co ON co.id = c.observation_id
                 WHERE t.target_observation_id = o.id AND c.verb = 'exclude'
                   AND json_extract(co.payload_json, '$.reason') = ?4)
         FROM observation_import i
         JOIN history_observation o ON o.id = i.observation_id
         WHERE o.endpoint_id = ?1 AND i.played_at_utc_ms >= ?2 AND i.played_at_utc_ms < ?3
         ORDER BY o.id",
    )?;
    let mut rows = statement.query(rusqlite::params![
        endpoint,
        batch.span.from_ms,
        batch.span.to_ms,
        SHIFTED_READ
    ])?;
    let mut held: BTreeMap<i64, Vec<Held>> = BTreeMap::new();
    while let Some(row) = rows.next()? {
        let key: Vec<u8> = row.get(2)?;
        held.entry(row.get(3)?).or_default().push(Held {
            observation: row.get(0)?,
            rev: row.get(1)?,
            listed: keys.contains(key.as_slice()),
            track: (row.get(4)?, row.get(5)?, row.get(6)?),
            excluded: row.get(7)?,
            takeable: row.get(8)?,
            set_aside_by: row.get(9)?,
        });
    }
    // Which corrections stand, as derivation has it: a revocation that is
    // itself revoked lets its target stand again.
    let active = ritornel_domain::history::active_corrections(&crate::history_store::corrections(
        connection,
    )?);
    let mut found = Recheck::default();
    for (second, imports) in &held {
        found.revoke.extend(
            imports
                .iter()
                .filter(|import| import.listed)
                .filter_map(|import| import.set_aside_by)
                .filter(|correction| active.contains(correction)),
        );
        let Some(listed) = listed.get(second) else {
            continue;
        };
        let first = imports.iter().map(|import| import.rev).min();
        let first: Vec<&Held> = imports
            .iter()
            .filter(|import| Some(import.rev) == first)
            .collect();
        if first.len() != listed.values().sum::<usize>() {
            continue;
        }
        // How many of each track the first read stored, and how many of
        // those still count.
        let mut stored: BTreeMap<&Track, (usize, usize)> = BTreeMap::new();
        for import in &first {
            let (all, counting) = stored.entry(&import.track).or_default();
            *all += 1;
            *counting += usize::from(!import.excluded);
        }
        for (track, (stored, counting)) in stored {
            let lists = listed.get(track).copied().unwrap_or(0);
            if lists == 0 || stored <= lists || counting <= lists {
                continue;
            }
            found.set_aside.extend(
                first
                    .iter()
                    .filter(|import| &import.track == track && !import.listed && import.takeable)
                    .map(|import| import.observation)
                    .take(counting - lists),
            );
        }
    }
    Ok(found)
}

/// Stores the batch's new records, binds what they name where the matcher
/// is sure, derives their plays and moves the cursor, in the write's
/// transaction. Returns how many were new.
pub(crate) fn ingest_imports(
    write: &Write<'_>,
    account: Id,
    batch: &Batch,
    rule: ritornel_domain::history::PlayRule,
) -> Result<u64, Error> {
    let connection = write.connection();
    let rows = rows_of(connection, account)?;
    let held = stored_keys(
        connection,
        rows.endpoint,
        batch.records.iter().map(|record| &record.source_key),
    )?;
    let mut observations = BTreeSet::new();
    let mut new_items = Vec::new();
    for record in &batch.records {
        if held.contains(&record.source_key) {
            continue;
        }
        let track = &record.track;
        let (item, made) = subject(
            write,
            rows.instance,
            &track.artist,
            track.album.as_deref(),
            &track.title,
        )?;
        if made {
            new_items.push((
                item,
                track.artist.clone(),
                track.album.clone(),
                track.title.clone(),
            ));
        }
        connection
            .prepare_cached(
                "INSERT INTO history_observation
                   (endpoint_id, source_key, kind, source_item_id, listener_id, captured_at_utc_ms,
                    received_at_utc_ms, zone_basis, payload_json, rev)
                 VALUES (?1, ?2, 'import', ?3, ?4, ?5, ?5, 'none', ?6, ?7)",
            )?
            .execute(rusqlite::params![
                rows.endpoint,
                record.source_key,
                item,
                rows.listener,
                write.now_ms(),
                record.payload_json,
                write.rev(),
            ])?;
        let observation = connection.last_insert_rowid();
        connection
            .prepare_cached(
                "INSERT INTO observation_import
                   (observation_id, played_at_utc_ms, artist, album, title, artist_mbid,
                    album_mbid, track_mbid, rev)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            )?
            .execute(rusqlite::params![
                observation,
                record.played_at_ms,
                track.artist,
                track.album,
                track.title,
                record.ids.artist_mbid,
                record.ids.album_mbid,
                record.ids.track_mbid,
                write.rev(),
            ])?;
        observations.insert(observation);
    }
    if observations.is_empty() {
        return Ok(0);
    }
    if !new_items.is_empty() {
        match_items(write, &catalog(connection)?, &new_items)?;
    }
    rederive(write, &observations, rule)?;
    connection
        .prepare_cached(
            "INSERT INTO history_cursor (endpoint_id, cursor_json, updated_at_ms, rev)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (endpoint_id) DO UPDATE
               SET cursor_json = excluded.cursor_json, updated_at_ms = excluded.updated_at_ms,
                   rev = excluded.rev",
        )?
        .execute((rows.endpoint, &batch.cursor, write.now_ms(), write.rev()))?;
    Ok(observations.len() as u64)
}

/// One scrobbled track the matcher was not sure of, with how often it was
/// played and what it nearly matched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Doubtful {
    /// The artist, as the service spells it.
    pub artist: String,
    /// The album.
    pub album: Option<String>,
    /// The title.
    pub title: String,
    /// Its plays.
    pub plays: i64,
    /// Why it was not bound: `several`, `album` or `near`.
    pub doubt: &'static str,
    /// What it nearly matched: title, artist credit.
    pub candidates: Vec<(Option<String>, Option<String>)>,
}

/// How much of an account's history matched the library.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportReport {
    /// Import records stored.
    pub imported: i64,
    /// Plays derived from them, echoes of our own and scrobbles joined to a
    /// stream not counted.
    pub plays: i64,
    /// Of those, the ones bound to a recording.
    pub matched_plays: i64,
    /// Records that are echoes of plays this library sent.
    pub echoes: i64,
    /// Records joined to a Spotify stream, which speaks for their play.
    pub joined_to_streams: i64,
    /// Distinct tracks the history names.
    pub tracks: i64,
    /// Of those, the ones bound.
    pub matched_tracks: i64,
    /// Tracks the matcher was not sure of, and their plays.
    pub doubtful_tracks: i64,
    /// See `doubtful_tracks`.
    pub doubtful_plays: i64,
    /// Tracks nothing in the library comes close to, and their plays.
    pub absent_tracks: i64,
    /// See `absent_tracks`.
    pub absent_plays: i64,
    /// The doubtful tracks, most played first, as many as were asked for.
    pub doubtful: Vec<Doubtful>,
    /// Tracks loved now, and of those the ones bound.
    pub loved: i64,
    /// See `loved`.
    pub loved_matched: i64,
    /// The earliest and latest play.
    pub first_ms: Option<i64>,
    /// See `first_ms`.
    pub last_ms: Option<i64>,
}

/// One love that reaches a track, by whoever said it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoveRow {
    /// Who said it: the service and account, as `lastfm:name`, or `you`.
    pub by: String,
    /// When.
    pub at_ms: i64,
}

/// One track, as `ritornelctl show` gives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackDetail {
    /// The appearance.
    pub appearance: Id,
    /// Its recording.
    pub recording: Id,
    /// What it shows.
    pub title: Option<String>,
    /// See `title`.
    pub artist: Option<String>,
    /// See `title`.
    pub album: Option<String>,
    /// Plays of the recording that count and are not excluded, from every
    /// source.
    pub plays: i64,
    /// The latest of them.
    pub last_played_ms: Option<i64>,
    /// The loves in force that reach the recording.
    pub loves: Vec<LoveRow>,
}

impl LibraryStore {
    /// The account `name` at `service`, made if the library has none: its
    /// history endpoint and source instance, for the library's first
    /// listener.
    pub fn ensure_service_account(&self, service: &str, name: &str) -> Result<Id, Error> {
        if !SERVICES.contains(&service) {
            return Err(Error::Invalid("no such service"));
        }
        let (service, name) = (service.to_owned(), name.to_owned());
        let find = |connection: &Connection, service: &str, name: &str| {
            connection
                .query_row(
                    "SELECT public_id FROM service_account
                     WHERE service = ?1 AND account_name = ?2 COLLATE NOCASE",
                    (service, name),
                    |row| row.get::<_, [u8; 16]>(0),
                )
                .optional()
        };
        let (s, n) = (service.clone(), name.clone());
        if let Some(found) = self.read(move |connection| Ok(find(connection, &s, &n)?))? {
            return Ok(Id::from_bytes(found));
        }
        let written = self.write(
            WriteRequest {
                method: "history.addAccount",
                command: None,
                client: None,
            },
            move |write| {
                let connection = write.connection();
                if let Some(found) = find(connection, &service, &name)? {
                    return Ok(Id::from_bytes(found).to_string());
                }
                let listener: i64 = connection
                    .query_row("SELECT id FROM listener ORDER BY id LIMIT 1", [], |row| {
                        row.get(0)
                    })
                    .optional()?
                    .ok_or(Error::Corrupt("a library with no listener"))?;
                connection.execute(
                    "INSERT INTO history_endpoint (public_id, kind, name, rev)
                     VALUES (?1, 'service', ?2, ?3)",
                    (
                        write.new_id()?.as_bytes(),
                        format!("{service}:{name}"),
                        write.rev(),
                    ),
                )?;
                let endpoint = connection.last_insert_rowid();
                connection.execute(
                    "INSERT INTO source_instance (public_id, kind, name, created_at_ms, rev)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    (
                        write.new_id()?.as_bytes(),
                        &service,
                        &name,
                        write.now_ms(),
                        write.rev(),
                    ),
                )?;
                let instance = connection.last_insert_rowid();
                let account = write.new_id()?;
                connection.execute(
                    "INSERT INTO service_account
                       (public_id, service, account_name, listener_id, endpoint_id,
                        source_instance_id, created_at_ms, rev)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    rusqlite::params![
                        account.as_bytes(),
                        service,
                        name,
                        listener,
                        endpoint,
                        instance,
                        write.now_ms(),
                        write.rev(),
                    ],
                )?;
                Ok(account.to_string())
            },
        )?;
        written
            .result_json
            .parse()
            .map_err(|_| Error::Corrupt("an account's ID"))
    }

    /// The account with this ID.
    pub fn service_account(&self, account: Id) -> Result<Option<ServiceAccount>, Error> {
        self.read(move |connection| {
            Ok(connection
                .query_row(
                    "SELECT sa.service, sa.account_name, l.public_id, e.public_id
                     FROM service_account sa
                     JOIN listener l ON l.id = sa.listener_id
                     JOIN history_endpoint e ON e.id = sa.endpoint_id
                     WHERE sa.public_id = ?1",
                    [account.as_bytes()],
                    |row| {
                        Ok(ServiceAccount {
                            account,
                            service: row.get(0)?,
                            name: row.get(1)?,
                            listener: Id::from_bytes(row.get(2)?),
                            endpoint: Id::from_bytes(row.get(3)?),
                        })
                    },
                )
                .optional()?)
        })
    }

    /// How many import records the account's endpoint holds.
    pub fn imported_count(&self, account: Id) -> Result<i64, Error> {
        self.read(move |connection| {
            let rows = rows_of(connection, account)?;
            Ok(connection.query_row(
                "SELECT count(*) FROM history_observation WHERE endpoint_id = ?1 AND kind = 'import'",
                [rows.endpoint],
                |row| row.get(0),
            )?)
        })
    }

    /// The cursor the account's last stored batch left.
    pub fn history_cursor(&self, account: Id) -> Result<Option<String>, Error> {
        self.read(move |connection| {
            let rows = rows_of(connection, account)?;
            Ok(connection
                .query_row(
                    "SELECT cursor_json FROM history_cursor WHERE endpoint_id = ?1",
                    [rows.endpoint],
                    |row| row.get(0),
                )
                .optional()?)
        })
    }

    /// Takes in a batch of import records through the one ingest call (see
    /// [`LibraryStore::ingest`]): every record that is new, the plays
    /// derived from them and the batch's cursor, in one transaction. A
    /// batch with nothing new moves no cursor, and writes nothing unless a
    /// full read finds something to set aside or lift. Of a
    /// `full` read, the same transaction sets aside what an earlier read
    /// stored twice in the batch's span, each with an `exclude` correction
    /// that says [`DUPLICATE_NOTE`], and revokes such a correction whose
    /// import the source lists again ([`recheck`]).
    pub(crate) fn ingest_import_batch(
        &self,
        account: Id,
        batch: &Batch,
        full: bool,
        rule: ritornel_domain::history::PlayRule,
    ) -> Result<Stored, Error> {
        // Read first: a batch that is all stored already, with nothing to
        // set aside or lift, writes nothing and takes no library revision.
        let (new, gone, rechecked) = {
            let batch = batch.clone();
            self.read(move |connection| {
                let rows = rows_of(connection, account)?;
                let held = stored_keys(
                    connection,
                    rows.endpoint,
                    batch.records.iter().map(|record| &record.source_key),
                )?;
                let rechecked = if full {
                    let found = recheck(connection, rows.endpoint, &batch)?;
                    found.set_aside.len() + found.revoke.len()
                } else {
                    0
                };
                Ok((
                    batch.records.len() - held.len(),
                    gone(connection, rows.endpoint, &batch)?,
                    rechecked,
                ))
            })?
        };
        if new == 0 && rechecked == 0 {
            return Ok(Stored {
                new: 0,
                gone,
                set_aside: 0,
            });
        }
        let batch = batch.clone();
        let written = self.write(
            WriteRequest {
                method: INGEST_METHOD,
                command: None,
                client: None,
            },
            move |write| {
                // Before the new records go in: what the library held is
                // what is compared.
                let found = if full {
                    let endpoint = rows_of(write.connection(), account)?.endpoint;
                    recheck(write.connection(), endpoint, &batch)?
                } else {
                    Recheck::default()
                };
                let new = ingest_imports(write, account, &batch, rule)?;
                crate::history_store::exclude(
                    write,
                    &found.set_aside,
                    DUPLICATE_NOTE,
                    Some(SHIFTED_READ),
                    rule,
                )?;
                for correction in &found.revoke {
                    crate::history_store::revoke(write, *correction, LISTED_AGAIN_NOTE, rule)?;
                }
                Ok(serde_json::to_string(&(new, found.set_aside.len()))?)
            },
        )?;
        let (new, set_aside): (u64, u64) = serde_json::from_str(&written.result_json)?;
        Ok(Stored {
            new,
            gone,
            set_aside,
        })
    }

    /// Makes the account's loves the list as it stands now: a love row for
    /// each track loved that is not loved here, and a row that clears it
    /// for each loved here that the list no longer has. Nothing is written
    /// when they agree.
    pub fn replace_loves(&self, account: Id, loves: &[Love]) -> Result<LovesStored, Error> {
        let mut wanted: BTreeMap<(String, String), i64> = BTreeMap::new();
        for love in loves {
            wanted
                .entry((love.artist.clone(), love.title.clone()))
                .or_insert(love.loved_at_ms);
        }
        let loved_now = self.read(move |connection| loved_now(connection, account))?;
        let new: Vec<((String, String), i64)> = wanted
            .iter()
            .filter(|(key, _)| !loved_now.contains_key(*key))
            .map(|(key, at)| (key.clone(), *at))
            .collect();
        let cleared: Vec<i64> = loved_now
            .iter()
            .filter(|(key, _)| !wanted.contains_key(*key))
            .map(|(_, item)| *item)
            .collect();
        let stored = LovesStored {
            loved: wanted.len() as u64,
            new: new.len() as u64,
            cleared: cleared.len() as u64,
        };
        if new.is_empty() && cleared.is_empty() {
            return Ok(stored);
        }
        self.write(
            WriteRequest {
                method: LOVES_METHOD,
                command: None,
                client: None,
            },
            move |write| {
                let connection = write.connection();
                let rows = rows_of(connection, account)?;
                let mut made = Vec::new();
                let love = |item: i64, cleared: bool, at: i64| -> Result<(), Error> {
                    connection
                        .prepare_cached(
                            "INSERT INTO listener_rating
                               (listener_id, source_item_id, kind, cleared, endpoint_id,
                                rated_at_ms, rev)
                             VALUES (?1, ?2, 'love', ?3, ?4, ?5, ?6)",
                        )?
                        .execute((rows.listener, item, cleared, rows.endpoint, at, write.rev()))?;
                    Ok(())
                };
                for ((artist, title), at) in &new {
                    let (item, fresh) = subject(write, rows.instance, artist, None, title)?;
                    if fresh {
                        made.push((item, artist.clone(), None, title.clone()));
                    }
                    love(item, false, *at)?;
                }
                for item in cleared {
                    love(item, true, write.now_ms())?;
                }
                if !made.is_empty() {
                    match_items(write, &catalog(connection)?, &made)?;
                }
                Ok(String::new())
            },
        )?;
        Ok(stored)
    }

    /// Judges again every track another service named, bound or not, but
    /// those the owner bound: every account's scrobbled tracks, and each of
    /// Spotify's tracks by the names of its newest stream in force. Binds
    /// those the matcher is now sure of, moves or releases a binding that no
    /// longer holds, and derives their plays again, in one write. Run after
    /// a resolve that changed what the library holds; a sync judges its own
    /// account with [`match_library_for`](Self::match_library_for).
    /// Returns how many bindings changed; with none, nothing is written.
    pub fn match_library(&self) -> Result<usize, Error> {
        Ok(self.match_scope(Scope::Library)?.values().sum())
    }

    /// As [`match_library`](Self::match_library), and returns how many of
    /// the bindings that changed are the account's: what its sync says.
    /// Only the account's tracks are judged: a scan and a run of streams
    /// judge the rest of the library when they change it.
    pub fn match_library_for(&self, account: Id) -> Result<usize, Error> {
        let instance = self.read(move |connection| Ok(rows_of(connection, account)?.instance))?;
        Ok(self
            .match_scope(Scope::Account(instance))?
            .get(&instance)
            .copied()
            .unwrap_or(0))
    }

    /// Matches the tracks in scope, by source instance. Whether anything
    /// differs is read first, so that nothing is written when nothing
    /// does; what is written is judged again inside the write, against what
    /// the library holds then.
    fn match_scope(&self, scope: Scope) -> Result<BTreeMap<i64, usize>, Error> {
        let differ =
            self.read_snapshot(move |connection| Ok(!changes(connection, scope)?.is_empty()))?;
        if !differ {
            return Ok(BTreeMap::new());
        }
        let written = self.write(
            WriteRequest {
                method: MATCH_METHOD,
                command: None,
                client: None,
            },
            move |write| Ok(serde_json::to_string(&match_in(write, scope)?)?),
        )?;
        Ok(serde_json::from_str(&written.result_json)?)
    }

    /// How much of the account's history matched the library, and the
    /// tracks the matcher was not sure of, most played first, at most
    /// `limit` of them.
    pub fn import_report(&self, account: Id, limit: usize) -> Result<ImportReport, Error> {
        self.read(move |connection| report(connection, account, limit))
    }

    /// One track, by its appearance: what it shows, its plays, and the loves
    /// in force that reach its recording.
    pub fn track_detail(&self, appearance: Id) -> Result<Option<TrackDetail>, Error> {
        self.read(move |connection| track_detail(connection, appearance))
    }
}

/// Each love item of the endpoint `?1` with its newest love row: the
/// account's answer for it.
pub(crate) const LOVED_NOW: &str =
    "SELECT si.id, si.provider_key, lr.cleared FROM listener_rating lr
     JOIN source_item si ON si.id = lr.source_item_id
     WHERE lr.endpoint_id = ?1 AND lr.kind = 'love'
       AND lr.id = (SELECT max(x.id) FROM listener_rating x
                    WHERE x.source_item_id = lr.source_item_id AND x.endpoint_id = ?1
                      AND x.kind = 'love')";

/// The account's love items that are loved now, by artist and title: for
/// each item, its newest love row is the account's answer.
fn loved_now(
    connection: &Connection,
    account: Id,
) -> Result<HashMap<(String, String), i64>, Error> {
    let rows = rows_of(connection, account)?;
    let mut statement = connection.prepare(LOVED_NOW)?;
    let found = statement.query_map([rows.endpoint], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, bool>(2)?,
        ))
    })?;
    let mut loved = HashMap::new();
    for row in found {
        let (item, key, cleared) = row?;
        if let (false, Some((artist, _, title))) = (cleared, subject_text(&key)) {
            loved.insert((artist, title), item);
        }
    }
    Ok(loved)
}

/// The account's subject items the matcher judges, which are all but those
/// the owner bound, with their text and the recording and appearance they
/// are bound to now, through redirects.
fn judged_subjects(
    connection: &Connection,
    instance: i64,
) -> Result<Vec<(Subject, Option<Bound>)>, Error> {
    let mut statement = connection.prepare(
        "SELECT si.id, si.provider_key, r.canonical_id, ta.canonical_id FROM source_item si
         LEFT JOIN assignment a ON a.source_item_id = si.id AND a.superseded_at_ms IS NULL
         LEFT JOIN identity_decision d ON d.id = a.decision_id
         LEFT JOIN recording r ON r.id = a.recording_id
         LEFT JOIN track_appearance ta ON ta.id = a.appearance_id
         WHERE si.source_instance_id = ?1 AND si.item_kind = ?2 AND si.retired_at_ms IS NULL
           AND (a.id IS NULL OR d.actor_kind = 'resolver')",
    )?;
    let rows = statement.query_map((instance, SUBJECT), |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<i64>>(2)?,
            row.get::<_, Option<i64>>(3)?,
        ))
    })?;
    let mut found = Vec::new();
    for row in rows {
        let (item, key, recording, appearance) = row?;
        if let Some((artist, album, title)) = subject_text(&key) {
            found.push((
                (item, artist, album, title),
                recording.map(|recording| (recording, appearance)),
            ));
        }
    }
    Ok(found)
}

/// Each of Spotify's tracks that the owner has not bound and a stream in
/// force names, with its source instance, the names of its newest stream
/// in force, and what it is bound to now. Of two streams that ended at the
/// same moment the names decide, so which archive came first never does.
fn judged_tracks(connection: &Connection) -> Result<Vec<(i64, Subject, Option<Bound>)>, Error> {
    let mut statement = connection.prepare(&format!(
        "WITH newest AS (
           SELECT o.id AS observation, o.source_item_id AS item,
                  row_number() OVER (
                    PARTITION BY o.source_item_id
                    ORDER BY s.ended_at_utc_ms DESC, i.title, i.artist, coalesce(i.album, ''),
                             o.source_key) AS rank
             FROM ({IN_FORCE}) f
             JOIN history_observation o ON o.id = f.observation_id
             JOIN observation_import_stream s ON s.observation_id = o.id
             JOIN observation_import i ON i.observation_id = o.id)
         SELECT si.source_instance_id, si.id, i.artist, i.album, i.title, r.canonical_id,
                ta.canonical_id
           FROM newest n
           JOIN observation_import i ON i.observation_id = n.observation
           JOIN source_item si ON si.id = n.item
           LEFT JOIN assignment a ON a.source_item_id = si.id AND a.superseded_at_ms IS NULL
           LEFT JOIN identity_decision d ON d.id = a.decision_id
           LEFT JOIN recording r ON r.id = a.recording_id
           LEFT JOIN track_appearance ta ON ta.id = a.appearance_id
          WHERE n.rank = 1 AND si.item_kind = 'spotify_track' AND si.retired_at_ms IS NULL
            AND (a.id IS NULL OR d.actor_kind = 'resolver')"
    ))?;
    let rows = statement.query_map([], |row| {
        let appearance: Option<i64> = row.get(6)?;
        Ok((
            row.get::<_, i64>(0)?,
            (
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, String>(4)?,
            ),
            row.get::<_, Option<i64>>(5)?
                .map(|recording| (recording, appearance)),
        ))
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Each of Spotify's tracks that no stream in force names, with its source
/// instance and the binding the matcher gave it, if any.
fn unnamed_tracks(connection: &Connection) -> Result<Vec<(i64, i64, Option<Bound>)>, Error> {
    let mut statement = connection.prepare(&format!(
        "SELECT si.source_instance_id, si.id, r.canonical_id, ta.canonical_id
           FROM source_item si
           JOIN assignment a ON a.source_item_id = si.id AND a.superseded_at_ms IS NULL
           JOIN identity_decision d ON d.id = a.decision_id AND d.actor_kind = 'resolver'
           JOIN recording r ON r.id = a.recording_id
           LEFT JOIN track_appearance ta ON ta.id = a.appearance_id
          WHERE si.item_kind = 'spotify_track' AND si.retired_at_ms IS NULL
            AND si.id NOT IN (SELECT o.source_item_id FROM ({IN_FORCE}) f
                              JOIN history_observation o ON o.id = f.observation_id)"
    ))?;
    let rows = statement.query_map([], |row| {
        let appearance: Option<i64> = row.get(3)?;
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1)?,
            Some((row.get::<_, i64>(2)?, appearance)),
        ))
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// The account's subject items that no assignment binds, with their text.
fn unbound_subjects(connection: &Connection, instance: i64) -> Result<Vec<Subject>, Error> {
    let mut statement = connection.prepare(
        "SELECT si.id, si.provider_key FROM source_item si
         WHERE si.source_instance_id = ?1 AND si.item_kind = ?2 AND si.retired_at_ms IS NULL
           AND NOT EXISTS (SELECT 1 FROM assignment a
                           WHERE a.source_item_id = si.id AND a.superseded_at_ms IS NULL)",
    )?;
    let rows = statement.query_map((instance, SUBJECT), |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut found = Vec::new();
    for row in rows {
        let (item, key) = row?;
        if let Some((artist, album, title)) = subject_text(&key) {
            found.push((item, artist, album, title));
        }
    }
    Ok(found)
}

fn report(connection: &Connection, account: Id, limit: usize) -> Result<ImportReport, Error> {
    let rows = rows_of(connection, account)?;
    let mut report = ImportReport::default();
    (report.imported, report.first_ms, report.last_ms) = connection.query_row(
        "SELECT count(*), min(i.played_at_utc_ms), max(i.played_at_utc_ms)
         FROM history_observation o JOIN observation_import i ON i.observation_id = o.id
         WHERE o.endpoint_id = ?1",
        [rows.endpoint],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    (report.plays, report.matched_plays) = connection.query_row(
        "SELECT count(*), count(pe.recording_id) FROM play_event pe
         JOIN play_event_observation peo ON peo.play_event_id = pe.id AND peo.role = 'primary'
         JOIN history_observation o ON o.id = peo.observation_id
         WHERE o.endpoint_id = ?1 AND pe.excluded = 0",
        [rows.endpoint],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    (report.echoes, report.joined_to_streams) = connection.query_row(
        "SELECT count(*) FILTER (WHERE peo.joined_by = 'echo'),
                count(*) FILTER (WHERE peo.joined_by IN ('start', 'during', 'renamed', 'pin'))
         FROM play_event_observation peo
         JOIN history_observation o ON o.id = peo.observation_id
         WHERE o.endpoint_id = ?1 AND peo.role = 'corroborating'",
        [rows.endpoint],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;

    // The plays the account's scrobbles are the primary of, as its plays
    // are counted: not those another record speaks for.
    let mut plays_of: HashMap<i64, i64> = HashMap::new();
    let mut statement = connection.prepare(
        "SELECT o.source_item_id, count(*) FROM history_observation o
         JOIN play_event_observation peo ON peo.observation_id = o.id AND peo.role = 'primary'
         JOIN play_event pe ON pe.id = peo.play_event_id
         WHERE o.endpoint_id = ?1 AND o.kind = 'import' AND pe.excluded = 0
         GROUP BY o.source_item_id",
    )?;
    for row in statement.query_map([rows.endpoint], |row| Ok((row.get(0)?, row.get(1)?)))? {
        let (item, plays) = row?;
        plays_of.insert(item, plays);
    }
    // Tracks are the items plays name; an item only a love names is not one.
    let bound: HashSet<i64> = {
        let mut statement = connection.prepare(
            "SELECT si.id FROM source_item si JOIN assignment a
               ON a.source_item_id = si.id AND a.superseded_at_ms IS NULL
             WHERE si.source_instance_id = ?1 AND si.item_kind = ?2",
        )?;
        statement
            .query_map((rows.instance, SUBJECT), |row| row.get(0))?
            .collect::<Result<_, _>>()?
    };
    report.tracks = plays_of.len() as i64;
    report.matched_tracks = plays_of.keys().filter(|item| bound.contains(item)).count() as i64;

    let catalog = catalog(connection)?;
    let except = exclusions(connection)?;
    let none = HashSet::new();
    let shows = |recording: i64| -> Result<(Option<String>, Option<String>), Error> {
        Ok(connection.query_row(
            "SELECT r.display_title, ac.display_text FROM recording r
             LEFT JOIN artist_credit ac ON ac.id = r.display_artist_credit_id WHERE r.id = ?1",
            [recording],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?)
    };
    let mut doubtful = Vec::new();
    for (item, artist, album, title) in unbound_subjects(connection, rows.instance)? {
        let Some(&plays) = plays_of.get(&item) else {
            continue;
        };
        match catalog.judge_except(
            Scrobbled {
                artist: &artist,
                album: album.as_deref(),
                title: &title,
            },
            except.get(&item).unwrap_or(&none),
        ) {
            Verdict::Doubtful { doubt, candidates } => {
                report.doubtful_tracks += 1;
                report.doubtful_plays += plays;
                doubtful.push(Doubtful {
                    artist,
                    album,
                    title,
                    plays,
                    doubt: doubt.as_str(),
                    candidates: candidates
                        .into_iter()
                        .map(&shows)
                        .collect::<Result<_, _>>()?,
                });
            }
            Verdict::Absent => {
                report.absent_tracks += 1;
                report.absent_plays += plays;
            }
            // Bound at the next match: counted as neither.
            Verdict::Bind { .. } => {}
        }
    }
    doubtful.sort_by(|a, b| {
        b.plays
            .cmp(&a.plays)
            .then_with(|| a.artist.cmp(&b.artist))
            .then_with(|| a.title.cmp(&b.title))
    });
    doubtful.truncate(limit);
    report.doubtful = doubtful;

    let loved = loved_now(connection, account)?;
    report.loved = loved.len() as i64;
    report.loved_matched = loved.values().filter(|item| bound.contains(item)).count() as i64;
    Ok(report)
}

fn track_detail(connection: &Connection, appearance: Id) -> Result<Option<TrackDetail>, Error> {
    let found: Option<Shown> = connection
        .query_row(
            "SELECT r.canonical_id, rc.public_id,
                        coalesce(ta.display_title, rc.display_title), ac.display_text,
                        rel.display_title
                 FROM track_appearance ta
                 JOIN recording r ON r.id = ta.recording_id
                 JOIN recording rc ON rc.id = r.canonical_id
                 LEFT JOIN artist_credit ac
                   ON ac.id = coalesce(ta.display_artist_credit_id, rc.display_artist_credit_id)
                 LEFT JOIN release rel ON rel.id = ta.release_id
                 WHERE ta.public_id = ?1",
            [appearance.as_bytes()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()?;
    let Some((recording, recording_id, title, artist, album)) = found else {
        return Ok(None);
    };
    let (plays, last_played_ms) = connection.query_row(
        "SELECT count(*), max(event_at_utc_ms) FROM play_event
         WHERE listener_id = (SELECT id FROM listener ORDER BY id LIMIT 1)
           AND recording_id = ?1 AND counts = 1 AND excluded = 0",
        [recording],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    // Every love row that reaches the recording, directly or through a
    // source item bound to it; each source's newest row for each item is its
    // answer.
    let mut statement = connection.prepare(
        "SELECT coalesce(sa.service || ':' || sa.account_name, 'you'),
                coalesce(lr.endpoint_id, -1), coalesce(lr.source_item_id, -1),
                lr.cleared, lr.rated_at_ms
         FROM listener_rating lr
         LEFT JOIN assignment a ON a.source_item_id = lr.source_item_id
                               AND a.superseded_at_ms IS NULL
         LEFT JOIN recording ra ON ra.id = a.recording_id
         LEFT JOIN recording rr ON rr.id = lr.recording_id
         LEFT JOIN service_account sa ON sa.endpoint_id = lr.endpoint_id
         WHERE lr.kind = 'love' AND coalesce(ra.canonical_id, rr.canonical_id) = ?1
         ORDER BY lr.id",
    )?;
    let mut newest: BTreeMap<(i64, i64), (String, bool, i64)> = BTreeMap::new();
    for row in statement.query_map([recording], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, bool>(3)?,
            row.get::<_, i64>(4)?,
        ))
    })? {
        let (by, endpoint, item, cleared, at) = row?;
        newest.insert((endpoint, item), (by, cleared, at));
    }
    let mut loves: BTreeMap<String, i64> = BTreeMap::new();
    for (by, cleared, at) in newest.into_values() {
        if !cleared {
            let earliest = loves.entry(by).or_insert(at);
            *earliest = (*earliest).min(at);
        }
    }
    Ok(Some(TrackDetail {
        appearance,
        recording: Id::from_bytes(recording_id),
        title,
        artist,
        album,
        plays,
        last_played_ms,
        loves: loves
            .into_iter()
            .map(|(by, at_ms)| LoveRow { by, at_ms })
            .collect(),
    }))
}

#[cfg(test)]
pub(crate) mod tests;
