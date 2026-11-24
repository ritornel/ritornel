//! What a catalogue replied about the library's discs, kept as it came,
//! and the facts read from it; and the switch that lets the library be
//! asked about at all. See "MusicBrainz lookups" in
//! docs/internals/corrections.md.
//!
//! The engine does no networking: the server asks the catalogue, and hands
//! the replies here to keep.

use std::collections::{BTreeMap, HashMap};

use ritornel_contract::CatalogueAlbum;
use ritornel_domain::reply::{READER_VERSION, read_disc_reply};
use rusqlite::{Connection, OptionalExtension};

use crate::library_store::Write;
use crate::resolve_store::rows;
use crate::walk::folder_of;
use crate::{Error, Id, LibraryStore, WriteRequest, Written};

/// The setting that says whether the library may be asked about. Absent
/// is off.
const LOOKUPS_SETTING: &str = "corrections.lookups";

/// The setting that holds the reader version the stored facts were read
/// by.
const READER_SETTING: &str = "corrections.reader_version";

/// The write that reads the kept replies again when the reader changed.
pub const FACTS_METHOD: &str = "corrections.facts";

/// One reply to keep: what was asked about, its status and its body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeptReply {
    /// The disc ID.
    pub key: String,
    /// 200, or 404 for no such disc.
    pub status: u16,
    /// The body as it came.
    pub body: String,
}

/// Whether a catalogue's reply reads as an answer to a lookup of this disc:
/// the releases it is on, or MusicBrainz's own "no such disc". Only such a
/// reply is kept.
pub fn reads_as_disc_reply(disc_id: &str, status: u16, body: &str) -> bool {
    read_disc_reply(disc_id, status, body).is_ok()
}

/// Which catalogue was asked what kind of question, with what inclusions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Asked<'a> {
    /// The catalogue's name: `musicbrainz`.
    pub catalogue: &'a str,
    /// The kind of question: `disc`.
    pub kind: &'a str,
    /// What it asked to be included.
    pub includes: &'a str,
}

/// The same, owned, for a closure that outlives the call.
#[derive(Debug, Clone)]
struct AskedOwned {
    catalogue: String,
    kind: String,
    includes: String,
}

impl AskedOwned {
    fn of(asked: Asked<'_>) -> Self {
        Self {
            catalogue: asked.catalogue.to_owned(),
            kind: asked.kind.to_owned(),
            includes: asked.includes.to_owned(),
        }
    }

    fn borrow(&self) -> Asked<'_> {
        Asked {
            catalogue: &self.catalogue,
            kind: &self.kind,
            includes: &self.includes,
        }
    }
}

fn setting(connection: &Connection, key: &str) -> Result<Option<String>, Error> {
    Ok(connection
        .query_row(
            "SELECT value_json FROM library_setting WHERE key = ?1",
            [key],
            |row| row.get(0),
        )
        .optional()?)
}

/// The current reply to one request: the newest. Its row, status and the
/// hash of its body.
fn current(
    connection: &Connection,
    asked: Asked<'_>,
    key: &str,
) -> Result<Option<(i64, u16, [u8; 32])>, Error> {
    Ok(connection
        .prepare_cached(
            "SELECT id, status, body_hash FROM lookup_response
             WHERE catalogue = ?1 AND request_kind = ?2 AND request_key = ?3 AND includes = ?4
             ORDER BY id DESC LIMIT 1",
        )?
        .query_row((asked.catalogue, asked.kind, key, asked.includes), |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .optional()?)
}

/// Writes the facts one reply states. A reply kept by an older build that
/// this reader cannot read states nothing.
fn read_into_claims(
    write: &Write<'_>,
    reply: i64,
    key: &str,
    status: u16,
    body: &str,
) -> Result<(), Error> {
    let facts = read_disc_reply(key, status, body).unwrap_or_default();
    let mut insert = write.connection().prepare_cached(
        "INSERT INTO evidence_claim
           (lookup_response_id, release_mbid, field, value, ordinal, reader_version, rev)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?;
    for fact in facts {
        insert.execute((
            reply,
            &fact.release,
            fact.field,
            &fact.value,
            fact.ordinal,
            READER_VERSION,
            write.rev(),
        ))?;
    }
    Ok(())
}

/// Whether the stored facts were read by another reader version than this
/// one, in a library that has replies.
fn facts_are_stale(connection: &Connection) -> Result<bool, Error> {
    let any: bool =
        connection.query_row("SELECT EXISTS (SELECT 1 FROM lookup_response)", [], |row| {
            row.get(0)
        })?;
    Ok(any && setting(connection, READER_SETTING)? != Some(READER_VERSION.to_string()))
}

/// Reads every kept reply to a disc lookup again, if the stored facts were
/// read by another reader version.
fn refresh_facts(write: &Write<'_>) -> Result<(), Error> {
    let connection = write.connection();
    if setting(connection, READER_SETTING)? == Some(READER_VERSION.to_string()) {
        return Ok(());
    }
    connection.execute("DELETE FROM evidence_claim", [])?;
    for (id, key, status, body) in rows(
        connection,
        "SELECT id, request_key, status, body FROM lookup_response
         WHERE catalogue = 'musicbrainz' AND request_kind = 'disc'",
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, u16>(2)?,
                row.get::<_, String>(3)?,
            ))
        },
    )? {
        read_into_claims(write, id, &key, status, &body)?;
    }
    crate::evidence_store::set_setting(
        write.connection(),
        READER_SETTING,
        &READER_VERSION.to_string(),
        write.rev(),
    )
}

/// A credit as written: each name with the join phrase after it, by
/// position. With no join phrase stated at all, the names are listed.
pub(crate) fn credit_text(names: &[(i64, String)], joins: &[(i64, String)]) -> String {
    if joins.is_empty() {
        return names
            .iter()
            .map(|(_, name)| name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
    }
    names
        .iter()
        .map(|(ordinal, name)| {
            let join = joins
                .iter()
                .find(|(at, _)| at == ordinal)
                .map_or("", |(_, join)| join.as_str());
            format!("{name}{join}")
        })
        .collect()
}

/// The albums the current reply about each key names, with each album's
/// own credit.
fn albums(
    connection: &Connection,
    asked: Asked<'_>,
    keys: &[String],
) -> Result<HashMap<String, Vec<CatalogueAlbum>>, Error> {
    /// One release's facts, by field, with their positions.
    type Facts = HashMap<String, Vec<(i64, String)>>;
    let mut by_key = HashMap::new();
    let mut statement = connection.prepare_cached(
        "SELECT release_mbid, field, ordinal, value FROM evidence_claim
         WHERE lookup_response_id = ?1 ORDER BY release_mbid, field, ordinal",
    )?;
    for key in keys {
        let Some((reply, _, _)) = current(connection, asked, key)? else {
            continue;
        };
        let mut releases: BTreeMap<String, Facts> = BTreeMap::new();
        let facts = statement.query_map([reply], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        for fact in facts {
            let (release, field, ordinal, value) = fact?;
            releases
                .entry(release)
                .or_default()
                .entry(field)
                .or_default()
                .push((ordinal, value));
        }
        let mut found: Vec<CatalogueAlbum> = Vec::new();
        for facts in releases.values() {
            let all = |field: &str| facts.get(field).cloned().unwrap_or_default();
            let first = |field: &str| all(field).into_iter().next().map(|(_, value)| value);
            let Some(group) = first("mb_release_group") else {
                continue;
            };
            if let Some(album) = found.iter_mut().find(|album| album.release_group == group) {
                album.releases += 1;
                continue;
            }
            // The album's own credit, or the edition's if it has none.
            let (names, joins) = if facts.contains_key("group_artist") {
                (all("group_artist"), all("group_artist_join"))
            } else {
                (all("album_artist"), all("album_artist_join"))
            };
            found.push(CatalogueAlbum {
                release_group: group,
                title: first("release_group_title")
                    .or_else(|| first("title"))
                    .unwrap_or_default(),
                artist: credit_text(&names, &joins),
                first_year: first("first_release_date")
                    .and_then(|date| date.get(..4).and_then(|year| year.parse().ok())),
                releases: 1,
            });
        }
        by_key.insert(key.clone(), found);
    }
    Ok(by_key)
}

impl LibraryStore {
    /// Whether the library may be asked about.
    pub fn lookups_enabled(&self) -> Result<bool, Error> {
        Ok(self.lookups_set()? == Some(true))
    }

    /// Whether the library may be asked about, as a call last set it:
    /// `None` when none has, and it may not.
    pub fn lookups_set(&self) -> Result<Option<bool>, Error> {
        self.read(|connection| {
            Ok(setting(connection, LOOKUPS_SETTING)?.map(|value| value == "true"))
        })
    }

    /// Says whether the library may be asked about. `receipt` is given the
    /// revision, and returns the result to store and answer with.
    pub fn set_lookups<R>(
        &self,
        request: WriteRequest,
        enabled: bool,
        receipt: R,
    ) -> Result<Written, Error>
    where
        R: FnOnce(i64) -> String + Send + 'static,
    {
        self.write(request, move |write| {
            crate::evidence_store::set_setting(
                write.connection(),
                LOOKUPS_SETTING,
                if enabled { "true" } else { "false" },
                write.rev(),
            )?;
            Ok(receipt(write.rev()))
        })
    }

    /// The disc IDs of one album, each once, in the order of their folders:
    /// those of the folders a release's files are in, or of one folder of
    /// a root, named by its name or else the only root that has it.
    /// [`Error::NotFound`] if the release or the root is not the library's,
    /// and [`Error::Invalid`] if the folder is in more than one root and no
    /// root is named.
    pub fn discs_of(
        &self,
        album: Option<Id>,
        root: Option<String>,
        folder: Option<String>,
    ) -> Result<Vec<String>, Error> {
        self.read_snapshot(move |connection| {
            // The folders, each with its source.
            let mut folders: Vec<(i64, String)> = Vec::new();
            if let Some(album) = album {
                let release: Option<i64> = connection
                    .query_row(
                        "SELECT canonical_id FROM release WHERE public_id = ?1",
                        [album.as_bytes()],
                        |row| row.get(0),
                    )
                    .optional()?;
                let release = release.ok_or(Error::NotFound("no such release"))?;
                let mut statement = connection.prepare(
                    "SELECT DISTINCT si.source_instance_id, si.provider_key FROM release rel
                     JOIN track_appearance ta ON ta.release_id = rel.id
                     JOIN assignment a ON a.appearance_id = ta.id AND a.superseded_at_ms IS NULL
                     JOIN source_item si ON si.id = a.source_item_id
                     WHERE rel.canonical_id = ?1 AND si.item_kind = 'file'
                       AND si.retired_at_ms IS NULL",
                )?;
                let paths = statement.query_map([release], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                })?;
                for path in paths {
                    let (source, path) = path?;
                    folders.push((source, folder_of(&path).to_owned()));
                }
            }
            if let Some(folder) = folder {
                let mut statement = connection.prepare(
                    "SELECT DISTINCT ed.source_instance_id, src.name FROM evidence_disc ed
                     JOIN source_instance src ON src.id = ed.source_instance_id
                     WHERE ed.folder = ?1 AND ed.provisional = 0",
                )?;
                let roots: Vec<(i64, String)> = statement
                    .query_map([&folder], |row| Ok((row.get(0)?, row.get(1)?)))?
                    .collect::<Result<_, _>>()?;
                let named: Vec<i64> = roots
                    .iter()
                    .filter(|(_, name)| root.as_ref().is_none_or(|root| root == name))
                    .map(|(source, _)| *source)
                    .collect();
                match named.as_slice() {
                    [] if root.is_some() => {
                        return Err(Error::NotFound("no such folder in that root"));
                    }
                    [_, _, ..] => {
                        return Err(Error::Invalid(
                            "the folder is in more than one root: name the root",
                        ));
                    }
                    _ => folders.extend(named.into_iter().map(|source| (source, folder.clone()))),
                }
            }
            // A provisional layout, such as a CUE sheet that disagrees with
            // the disc ID it prints, is proposed from never, so it is not
            // asked about.
            let mut discs: Vec<String> = Vec::new();
            for (source, path, disc) in rows(
                connection,
                "SELECT source_instance_id, folder, disc_id FROM evidence_disc
                 WHERE provisional = 0
                 ORDER BY folder, disc_id, source_instance_id",
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )? {
                let wanted = folders
                    .iter()
                    .any(|(at, folder)| *at == source && *folder == path);
                if wanted && !discs.contains(&disc) {
                    discs.push(disc);
                }
            }
            Ok(discs)
        })
    }

    /// Whether this key has a reply to this request already.
    pub fn replied(&self, asked: Asked<'_>, key: &str) -> Result<bool, Error> {
        let (asked, key) = (AskedOwned::of(asked), key.to_owned());
        self.read(move |connection| Ok(current(connection, asked.borrow(), &key)?.is_some()))
    }

    /// Keeps the replies to one request each, in one transaction, and reads
    /// the facts each states. A reply the same as the current one for its
    /// request is not kept again; one that differs becomes the current one.
    /// Only a reply the reader can read as an answer to its disc lookup is
    /// kept. `finish` is given the revision and the albums the current
    /// reply about each of `keys` names, and returns the result to store
    /// and answer with.
    pub fn keep_replies<F>(
        &self,
        request: WriteRequest,
        asked: Asked<'_>,
        replies: Vec<KeptReply>,
        keys: Vec<String>,
        finish: F,
    ) -> Result<Written, Error>
    where
        F: FnOnce(i64, HashMap<String, Vec<CatalogueAlbum>>) -> String + Send + 'static,
    {
        let asked = AskedOwned::of(asked);
        self.write(request, move |write| {
            let asked = asked.borrow();
            let connection = write.connection();
            refresh_facts(write)?;
            for reply in &replies {
                if read_disc_reply(&reply.key, reply.status, &reply.body).is_err() {
                    return Err(Error::Invalid("only a reply to a disc lookup is kept"));
                }
                let hash = blake3::hash(reply.body.as_bytes());
                let same =
                    current(connection, asked, &reply.key)?.is_some_and(|(_, status, had)| {
                        status == reply.status && had == *hash.as_bytes()
                    });
                if same {
                    continue;
                }
                connection
                    .prepare_cached(
                        "INSERT INTO lookup_response
                           (catalogue, request_kind, request_key, includes, status, body, body_hash,
                            received_at_ms, rev)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    )?
                    .execute((
                        asked.catalogue,
                        asked.kind,
                        &reply.key,
                        asked.includes,
                        reply.status,
                        &reply.body,
                        hash.as_bytes(),
                        write.now_ms(),
                        write.rev(),
                    ))?;
                let id = connection.last_insert_rowid();
                read_into_claims(write, id, &reply.key, reply.status, &reply.body)?;
            }
            let found = albums(connection, asked, &keys)?;
            Ok(finish(write.rev(), found))
        })
    }

    /// Reads every kept reply again if the stored facts were read by
    /// another reader version, with no network: what a server does when it
    /// starts. `None`, and nothing written, if they were not.
    pub fn refresh_corrections(&self, request: WriteRequest) -> Result<Option<Written>, Error> {
        if !self.read(facts_are_stale)? {
            return Ok(None);
        }
        let written = self.write(request, |write| {
            refresh_facts(write)?;
            Ok(String::new())
        })?;
        Ok(Some(written))
    }
}
