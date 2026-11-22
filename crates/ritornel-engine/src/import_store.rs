//! Plays another service reported: the account they come from, the import
//! records and their cursor through the one ingest call, the text-only
//! items that name what each was of and their matching to the library, the
//! loved tracks, and the report on how much of it matched. See "Last.fm" in
//! docs/design/history.md and "Scrobbles" in docs/design/identity.md.
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
    Appearance, Catalog, MATCHER_VERSION, Recording, Scrobbled, Verdict,
};
use ritornel_source::{Batch, Love, LovesStored, Stored};
use rusqlite::{Connection, OptionalExtension};

use crate::history_store::{INGEST_METHOD, rebind, rederive};
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
         WHERE ta.redirect_to_id IS NULL AND ta.id IN ({})",
        crate::list::REACHED
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

/// Binds a subject item as the matcher says, unless it is bound already:
/// a decision of its own, the assignment, and the item's cached recording.
fn bind(
    write: &Write<'_>,
    item: i64,
    text: (&str, Option<&str>, &str),
    recording: i64,
    appearance: Option<i64>,
    candidates: usize,
) -> Result<bool, Error> {
    let connection = write.connection();
    let bound: bool = connection
        .prepare_cached(
            "SELECT EXISTS (SELECT 1 FROM assignment
                            WHERE source_item_id = ?1 AND superseded_at_ms IS NULL)",
        )?
        .query_row([item], |row| row.get(0))?;
    if bound {
        return Ok(false);
    }
    let (artist, album, title) = text;
    let evidence = serde_json::json!({
        "matcher": MATCHER_VERSION,
        "artist": artist,
        "album": album,
        "title": title,
        "candidates": candidates,
        "byAlbum": candidates > 1,
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
            recording,
            appearance,
            decision,
            write.now_ms(),
            write.rev(),
        ))?;
    connection
        .prepare_cached(
            "UPDATE source_item SET current_recording_id =
               (SELECT canonical_id FROM recording WHERE id = ?2), rev = ?3
             WHERE id = ?1",
        )?
        .execute((item, recording, write.rev()))?;
    Ok(true)
}

/// Matches these subject items and binds those the matcher is sure of.
/// Returns how many it bound.
fn match_items(write: &Write<'_>, catalog: &Catalog, items: &[Subject]) -> Result<usize, Error> {
    let mut bound = 0;
    for (item, artist, album, title) in items {
        let verdict = catalog.judge(Scrobbled {
            artist,
            album: album.as_deref(),
            title,
        });
        if let Verdict::Bind {
            recording,
            appearance,
            candidates,
        } = verdict
            && bind(
                write,
                *item,
                (artist, album.as_deref(), title),
                recording,
                appearance,
                candidates,
            )?
        {
            bound += 1;
        }
    }
    Ok(bound)
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
    /// Why it was not bound: `several` or `near`.
    pub doubt: &'static str,
    /// What it nearly matched: title, artist credit.
    pub candidates: Vec<(Option<String>, Option<String>)>,
}

/// How much of an account's history matched the library.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportReport {
    /// Import records stored.
    pub imported: i64,
    /// Plays derived from them, echoes of our own not counted.
    pub plays: i64,
    /// Of those, the ones bound to a recording.
    pub matched_plays: i64,
    /// Records that are echoes of plays this library sent.
    pub echoes: i64,
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
    /// batch with nothing new writes nothing, its cursor included.
    pub(crate) fn ingest_import_batch(
        &self,
        account: Id,
        batch: &Batch,
        rule: ritornel_domain::history::PlayRule,
    ) -> Result<Stored, Error> {
        // Read first: a batch that is all stored already writes nothing,
        // and takes no library revision.
        let (new, gone) = {
            let batch = batch.clone();
            self.read(move |connection| {
                let rows = rows_of(connection, account)?;
                let held = stored_keys(
                    connection,
                    rows.endpoint,
                    batch.records.iter().map(|record| &record.source_key),
                )?;
                Ok((
                    batch.records.len() - held.len(),
                    gone(connection, rows.endpoint, &batch)?,
                ))
            })?
        };
        if new == 0 {
            return Ok(Stored { new: 0, gone });
        }
        let batch = batch.clone();
        let written = self.write(
            WriteRequest {
                method: INGEST_METHOD,
                command: None,
                client: None,
            },
            move |write| Ok(ingest_imports(write, account, &batch, rule)?.to_string()),
        )?;
        Ok(Stored {
            new: written.result_json.parse().unwrap_or(0),
            gone,
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

    /// Matches the account's unbound tracks to the library again, binds
    /// those the matcher is now sure of, and derives their plays again.
    /// Run after a sync, since the library may have grown since a track was
    /// first seen. Returns how many it bound; with none, nothing is written.
    pub fn match_scrobbles(&self, account: Id) -> Result<usize, Error> {
        let sure = self.read(move |connection| {
            let rows = rows_of(connection, account)?;
            let catalog = catalog(connection)?;
            let unbound = unbound_subjects(connection, rows.instance)?;
            Ok(unbound
                .into_iter()
                .filter(|(_, artist, album, title)| {
                    matches!(
                        catalog.judge(Scrobbled {
                            artist,
                            album: album.as_deref(),
                            title,
                        }),
                        Verdict::Bind { .. }
                    )
                })
                .collect::<Vec<_>>())
        })?;
        if sure.is_empty() {
            return Ok(0);
        }
        let written = self.write(
            WriteRequest {
                method: MATCH_METHOD,
                command: None,
                client: None,
            },
            move |write| {
                let bound = match_items(write, &catalog(write.connection())?, &sure)?;
                rebind(write)?;
                Ok(bound.to_string())
            },
        )?;
        Ok(written.result_json.parse().unwrap_or(0))
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

/// The account's love items that are loved now, by artist and title: for
/// each item, its newest love row is the account's answer.
fn loved_now(
    connection: &Connection,
    account: Id,
) -> Result<HashMap<(String, String), i64>, Error> {
    let rows = rows_of(connection, account)?;
    let mut statement = connection.prepare(
        "SELECT si.id, si.provider_key, lr.cleared FROM listener_rating lr
         JOIN source_item si ON si.id = lr.source_item_id
         WHERE lr.endpoint_id = ?1 AND lr.kind = 'love'
           AND lr.id = (SELECT max(x.id) FROM listener_rating x
                        WHERE x.source_item_id = lr.source_item_id AND x.endpoint_id = ?1
                          AND x.kind = 'love')",
    )?;
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
         WHERE o.endpoint_id = ?1",
        [rows.endpoint],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    report.echoes = connection.query_row(
        "SELECT count(*) FROM play_event_observation peo
         JOIN history_observation o ON o.id = peo.observation_id
         WHERE o.endpoint_id = ?1 AND peo.role = 'corroborating'",
        [rows.endpoint],
        |row| row.get(0),
    )?;
    let mut plays_of: HashMap<i64, i64> = HashMap::new();
    let mut statement = connection.prepare(
        "SELECT source_item_id, count(*) FROM history_observation
         WHERE endpoint_id = ?1 AND kind = 'import' GROUP BY source_item_id",
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
        match catalog.judge(Scrobbled {
            artist: &artist,
            album: album.as_deref(),
            title: &title,
        }) {
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
mod tests;
