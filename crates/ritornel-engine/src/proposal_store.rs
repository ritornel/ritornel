//! Album proposals: derived from the disc layouts, the current replies and
//! the files, and read by the corrections calls. See "Album proposals" in
//! docs/internals/corrections.md.
//!
//! Proposals are worked out again in the same transaction as any write that
//! changes what they rest on: a derivation of the layouts, and a lookup's
//! replies. Nothing the projector reads is written here.

use std::collections::{BTreeMap, HashMap, HashSet};

use ritornel_contract::{
    AlbumShape, Choice, CorrectionsSummary, FileValue, LayoutMethod, PreviewShown, Proposal,
    ProposalCandidate, ProposalChange, ProposalEvidence, ProposalList,
};
use ritornel_domain::Policy;
use ritornel_domain::propose::{DiscLayout, FolderDiscs, PROPOSER_VERSION, ReplyRelease, propose};
use ritornel_domain::values::{self, Value};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::evidence_store::{revision, unsigned};
use crate::library_store::Write;
use crate::lookup_store::credit_text;
use crate::resolve_store::{load_in, rows};
use crate::{Error, Id, LibraryStore};

/// The setting that holds the proposer version the stored proposals were
/// worked out by.
const PROPOSER_SETTING: &str = "corrections.proposer_version";

/// One value, as `values_json` holds it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct StoredValue {
    pub(crate) item: i64,
    pub(crate) field: String,
    pub(crate) ordinal: u32,
    /// `None` clears the field.
    pub(crate) value: Option<String>,
    /// `catalogue` or `library`.
    pub(crate) origin: String,
}

impl StoredValue {
    /// The value, when its field and origin are ones this build knows.
    pub(crate) fn value(&self) -> Option<Value> {
        Some(Value {
            item: self.item,
            field: values::field(&self.field)?,
            ordinal: self.ordinal,
            value: self.value.clone(),
            origin: values::Origin::named(&self.origin)?,
        })
    }
}

/// Whether answers or choices said together disagree about a file: two
/// that give one field of it different values, all its values taken
/// together.
pub(crate) fn disagree(said: &[Vec<Value>]) -> bool {
    type Said<'a> = Vec<(u32, &'a Option<String>)>;
    let mut given: HashMap<(i64, &str), Said<'_>> = HashMap::new();
    for one in said {
        let mut fields: std::collections::BTreeMap<(i64, &str), Said<'_>> =
            std::collections::BTreeMap::new();
        for value in one {
            fields
                .entry((value.item, value.field))
                .or_default()
                .push((value.ordinal, &value.value));
        }
        for (field, mut values) in fields {
            values.sort_unstable();
            if given
                .insert(field, values.clone())
                .is_some_and(|had| had != values)
            {
                return true;
            }
        }
    }
    false
}

/// An album's shape as the contract shows it.
pub(crate) fn album_shape(shape: values::AlbumShape) -> AlbumShape {
    AlbumShape {
        title: shape.title,
        artist: shape.artist,
        primary_type: shape.primary_type,
        first_year: shape.first_year.and_then(|year| u32::try_from(year).ok()),
        releases: shape.releases,
        discs: shape.discs,
        songs: shape.songs,
    }
}

fn year(date: Option<&str>) -> Option<u32> {
    date?.get(..4)?.parse().ok()
}

/// What the current reply about each disc ID says of each release: the
/// newest reply MusicBrainz gave to a question about it.
pub(crate) fn current_releases(
    connection: &Connection,
) -> Result<HashMap<String, Vec<ReplyRelease>>, Error> {
    /// One release's facts, by field, with their positions.
    type Fields = BTreeMap<String, Vec<(i64, String)>>;
    // By disc ID and release.
    let mut facts: BTreeMap<(String, String), Fields> = BTreeMap::new();
    for (key, release, field, ordinal, value) in rows(
        connection,
        "SELECT lr.request_key, ec.release_mbid, ec.field, ec.ordinal, ec.value
         FROM evidence_claim ec JOIN lookup_response lr ON lr.id = ec.lookup_response_id
         WHERE lr.id IN (SELECT max(id) FROM lookup_response
                         WHERE catalogue = 'musicbrainz' AND request_kind = 'disc'
                         GROUP BY request_key)",
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        },
    )? {
        facts
            .entry((key, release))
            .or_default()
            .entry(field)
            .or_default()
            .push((ordinal, value));
    }
    // The current reply about each disc ID.
    let reply_of: HashMap<String, i64> = rows(
        connection,
        "SELECT request_key, max(id) FROM lookup_response
         WHERE catalogue = 'musicbrainz' AND request_kind = 'disc' GROUP BY request_key",
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
    )?
    .into_iter()
    .collect();
    let mut replies: HashMap<String, Vec<ReplyRelease>> = HashMap::new();
    // A disc MusicBrainz does not know has a current reply that names
    // nothing.
    for key in rows(
        connection,
        "SELECT DISTINCT request_key FROM lookup_response
         WHERE catalogue = 'musicbrainz' AND request_kind = 'disc'",
        |row| row.get::<_, String>(0),
    )? {
        replies.entry(key).or_default();
    }
    for ((key, release), mut fields) in facts {
        for values in fields.values_mut() {
            values.sort();
        }
        let first = |field: &str| {
            fields
                .get(field)
                .and_then(|values| values.first())
                .map(|(_, value)| value.clone())
        };
        let Some(group) = first("mb_release_group") else {
            continue;
        };
        // The album's own credit, or the edition's if it has none.
        let all = |field: &str| fields.get(field).cloned().unwrap_or_default();
        let artist = if fields.contains_key("group_artist") {
            credit_text(&all("group_artist"), &all("group_artist_join"))
        } else {
            credit_text(&all("album_artist"), &all("album_artist_join"))
        };
        let reply = reply_of.get(&key).copied().unwrap_or_default();
        replies.entry(key).or_default().push(ReplyRelease {
            reply,
            release,
            group,
            group_title: first("release_group_title"),
            artist,
            primary_type: first("primary_type"),
            first_date: first("first_release_date"),
            positions: fields
                .get("disc_number")
                .into_iter()
                .flatten()
                .filter_map(|(_, value)| value.parse().ok())
                .collect(),
        });
    }
    Ok(replies)
}

/// Whether the stored proposals were worked out by another proposer version
/// than this one, in a library that has disc layouts.
pub(crate) fn proposals_are_stale(connection: &Connection) -> Result<bool, Error> {
    let any: bool =
        connection.query_row("SELECT EXISTS (SELECT 1 FROM evidence_disc)", [], |row| {
            row.get(0)
        })?;
    let stored: Option<String> = connection
        .query_row(
            "SELECT value_json FROM library_setting WHERE key = ?1",
            [PROPOSER_SETTING],
            |row| row.get(0),
        )
        .optional()?;
    Ok(any && stored != Some(PROPOSER_VERSION.to_string()))
}

/// Works the proposals out again and writes what differs from the stored
/// ones, in the write's transaction.
pub(crate) fn derive_proposals(write: &Write<'_>) -> Result<(), Error> {
    let (connection, rev) = (write.connection(), write.rev());
    let (library, _) = load_in(connection)?;
    let mut layouts: BTreeMap<String, Vec<DiscLayout>> = BTreeMap::new();
    let mut placed: HashMap<i64, Vec<i64>> = HashMap::new();
    for (disc, item) in rows(
        connection,
        "SELECT evidence_disc_id, source_item_id FROM evidence_disc_item ORDER BY evidence_disc_id, position",
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
    )? {
        placed.entry(disc).or_default().push(item);
    }
    let stored = rows(
        connection,
        "SELECT id, source_instance_id, folder, disc_id, provisional, found_json FROM evidence_disc
         ORDER BY source_instance_id, folder, disc_id",
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, bool>(4)?,
                row.get::<_, String>(5)?,
            ))
        },
    )?;
    // How a disc ID's layouts were found, every copy of it included, as
    // a proposal shows them and an answer keeps them.
    let mut found: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for (_, source, folder, disc_id, _, ways) in &stored {
        found
            .entry(disc_id.as_str())
            .or_default()
            .push(format!("{source}/{folder}: {ways}"));
    }
    let found: HashMap<&str, String> = found
        .into_iter()
        .map(|(disc_id, ways)| (disc_id, ways.join("\n")))
        .collect();
    for (id, source, folder, disc_id, provisional, _) in &stored {
        layouts
            .entry(format!("{source}/{folder}"))
            .or_default()
            .push(DiscLayout {
                disc_id: disc_id.clone(),
                provisional: *provisional,
                found: found.get(disc_id.as_str()).cloned().unwrap_or_default(),
                items: placed.remove(id).unwrap_or_default(),
            });
    }
    let folders: Vec<FolderDiscs> = layouts
        .into_iter()
        .map(|(folder, layouts)| FolderDiscs { folder, layouts })
        .collect();
    let public_ids: HashMap<i64, [u8; 16]> = rows(
        connection,
        "SELECT id, public_id FROM source_item WHERE item_kind = 'file'",
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, [u8; 16]>(1)?)),
    )?
    .into_iter()
    .collect();
    let replies = current_releases(connection)?;
    let derived = propose(
        &library,
        &folders,
        &replies,
        &public_ids,
        &Policy::current(),
    );

    let stored: HashMap<[u8; 16], (i64, String, i64)> = rows(
        connection,
        "SELECT public_id, id, fingerprint, proposer_version FROM proposal",
        |row| {
            Ok((
                row.get::<_, [u8; 16]>(0)?,
                (row.get(1)?, row.get(2)?, row.get(3)?),
            ))
        },
    )?
    .into_iter()
    .collect();
    let mut keep: HashMap<[u8; 16], ()> = HashMap::new();
    for proposal in &derived {
        let same = stored
            .get(&proposal.id)
            .is_some_and(|(_, fingerprint, version)| {
                *fingerprint == proposal.fingerprint && *version == i64::from(PROPOSER_VERSION)
            });
        if same {
            keep.insert(proposal.id, ());
        }
    }
    for (public_id, (id, _, _)) in &stored {
        if keep.contains_key(public_id) {
            continue;
        }
        for table in ["proposal_item", "proposal_candidate", "proposal_evidence"] {
            connection
                .prepare_cached(&format!("DELETE FROM {table} WHERE proposal_id = ?1"))?
                .execute([id])?;
        }
        connection
            .prepare_cached("DELETE FROM proposal WHERE id = ?1")?
            .execute([id])?;
    }
    for proposal in derived
        .iter()
        .filter(|proposal| !keep.contains_key(&proposal.id))
    {
        let (source, folder) = proposal
            .folder
            .split_once('/')
            .unwrap_or((&proposal.folder, ""));
        let group = public_ids.get(&proposal.group).copied().unwrap_or_default();
        let group_id = ritornel_domain::propose::proposal_id(proposal.kind, Some(&group));
        connection
            .prepare_cached(
                "INSERT INTO proposal
                   (public_id, kind, anchor_item_id, group_public_id, source_instance_id, folder,
                    fingerprint, proposer_version, rev)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            )?
            .execute((
                proposal.id,
                proposal.kind,
                proposal.anchor,
                group_id,
                source.parse::<i64>().unwrap_or_default(),
                folder,
                &proposal.fingerprint,
                PROPOSER_VERSION,
                rev,
            ))?;
        let row = connection.last_insert_rowid();
        for item in &proposal.items {
            connection
                .prepare_cached("INSERT INTO proposal_item (proposal_id, source_item_id, rev) VALUES (?1, ?2, ?3)")?
                .execute((row, item, rev))?;
        }
        // Each reply once, whatever layouts of its disc came with it.
        let mut kept = HashSet::new();
        for rest in &proposal.rests_on {
            if !kept.insert((rest.disc_id.as_str(), rest.reply)) {
                continue;
            }
            connection
                .prepare_cached(
                    "INSERT INTO proposal_evidence (proposal_id, role, disc_id, lookup_response_id, rev)
                     VALUES (?1, 'rests_on', ?2, ?3, ?4)",
                )?
                .execute((row, &rest.disc_id, rest.reply, rev))?;
        }
        for (ordinal, candidate) in (0..).zip(&proposal.candidates) {
            let stored: Vec<StoredValue> = candidate
                .values
                .iter()
                .map(|value| StoredValue {
                    item: value.item,
                    field: value.field.to_owned(),
                    ordinal: value.ordinal,
                    value: value.value.clone(),
                    origin: value.origin.name().to_owned(),
                })
                .collect();
            connection
                .prepare_cached(
                    "INSERT INTO proposal_candidate
                       (proposal_id, ordinal, release_group_mbid, title, artist, primary_type, first_date,
                        releases, change, values_json, onto_json, rev)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                )?
                .execute((
                    row,
                    ordinal,
                    &candidate.release_group,
                    &candidate.title,
                    &candidate.artist,
                    &candidate.primary_type,
                    &candidate.first_date,
                    candidate.releases,
                    candidate.change.as_str(),
                    serde_json::to_string(&stored)?,
                    serde_json::to_string(&candidate.onto)?,
                    rev,
                ))?;
        }
    }
    crate::evidence_store::set_setting(
        connection,
        PROPOSER_SETTING,
        &PROPOSER_VERSION.to_string(),
        rev,
    )
}

fn change(text: &str) -> ProposalChange {
    match text {
        "moves" => ProposalChange::Moves,
        "gains" => ProposalChange::Gains,
        _ => ProposalChange::Knows,
    }
}

/// Each source item's public ID, root and path, by row.
pub(crate) fn paths(connection: &Connection) -> Result<HashMap<i64, (Id, String, String)>, Error> {
    Ok(rows(
        connection,
        "SELECT si.id, si.public_id, src.name, si.provider_key
         FROM source_item si JOIN source_instance src ON src.id = si.source_instance_id
         WHERE si.item_kind = 'file'",
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                (
                    Id::from_bytes(row.get::<_, [u8; 16]>(1)?),
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ),
            ))
        },
    )?
    .into_iter()
    .collect())
}

/// The public ID, root and path of each of these source items, by row,
/// reading those alone.
pub(crate) fn paths_of(
    connection: &Connection,
    items: impl IntoIterator<Item = i64>,
) -> Result<HashMap<i64, (Id, String, String)>, Error> {
    let mut paths = HashMap::new();
    for item in items {
        let found: Option<([u8; 16], String, String)> = connection
            .prepare_cached(
                "SELECT si.public_id, src.name, si.provider_key
                 FROM source_item si JOIN source_instance src ON src.id = si.source_instance_id
                 WHERE si.id = ?1",
            )?
            .query_row([item], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .optional()?;
        if let Some((id, root, path)) = found {
            paths.insert(item, (Id::from_bytes(id), root, path));
        }
    }
    Ok(paths)
}

/// One value as the contract shows it, with what the file says for it now
/// when that is known.
pub(crate) fn file_value(
    paths: &HashMap<i64, (Id, String, String)>,
    item: i64,
    field: String,
    ordinal: u32,
    value: Option<String>,
    now: Option<String>,
) -> FileValue {
    let (file_id, root, path) = paths
        .get(&item)
        .cloned()
        .unwrap_or_else(|| (Id::from_bytes([0; 16]), String::new(), String::new()));
    FileValue {
        file_id,
        root,
        path,
        field,
        ordinal,
        value,
        now,
    }
}

/// What a file says now for one value of one field, as the catalog reads it.
pub(crate) fn now_of(facts: &ritornel_domain::Facts, field: &str, ordinal: u32) -> Option<String> {
    let at = |values: &Vec<String>| values.get(usize::try_from(ordinal).ok()?).cloned();
    // A field of one value has it at the first position only.
    let one = |value: Option<String>| value.filter(|_| ordinal == 0);
    match field {
        "album" => one(facts.album.clone()),
        "album_artist" => at(&facts.album_artist),
        "mb_album_artist" => at(&facts.album_artist_mbids),
        "disc_number" => one(facts.disc.map(|disc| disc.to_string())),
        "mb_release_group" => one(facts.release_group_mbid.clone()),
        "primary_type" => one(facts.primary_type.clone()),
        "original_date" => one(facts.original_date.clone()),
        _ => None,
    }
}

/// How many values a file says for a field.
pub(crate) fn values_of(facts: &ritornel_domain::Facts, field: &str) -> u32 {
    let count = |values: &Vec<String>| u32::try_from(values.len()).unwrap_or(u32::MAX);
    match field {
        "album_artist" => count(&facts.album_artist),
        "mb_album_artist" => count(&facts.album_artist_mbids),
        _ => u32::from(now_of(facts, field, 0).is_some()),
    }
}

/// The stored proposals, brief, or one whole.
fn read_proposals(connection: &Connection, one: Option<Id>) -> Result<Vec<Proposal>, Error> {
    let mut statement = connection.prepare(
        "SELECT p.id, p.public_id, p.kind, p.group_public_id, src.name, p.folder, p.fingerprint,
                (SELECT count(*) FROM proposal_item pi WHERE pi.proposal_id = p.id)
         FROM proposal p JOIN source_instance src ON src.id = p.source_instance_id
         WHERE ?1 IS NULL OR p.public_id = ?1
         ORDER BY p.group_public_id, src.name, p.folder",
    )?;
    let found = statement.query_map([one.map(|id| id.as_bytes().to_vec())], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, [u8; 16]>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, [u8; 16]>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, String>(6)?,
            row.get::<_, i64>(7)?,
        ))
    })?;
    let whole = one.is_some();
    let paths = if whole {
        paths(connection)?
    } else {
        HashMap::new()
    };
    let mut proposals = Vec::new();
    for found in found {
        let (row, id, kind, group, root, folder, fingerprint, count) = found?;
        let mut candidates = Vec::new();
        let mut statement = connection.prepare_cached(
            "SELECT release_group_mbid, title, artist, primary_type, first_date, releases, change, values_json
             FROM proposal_candidate WHERE proposal_id = ?1 ORDER BY ordinal",
        )?;
        let listed = statement.query_map([row], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, u32>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
            ))
        })?;
        for listed in listed {
            let (release_group, title, artist, primary_type, first_date, releases, said, values) =
                listed?;
            let values: Vec<StoredValue> = if whole {
                serde_json::from_str(&values)?
            } else {
                Vec::new()
            };
            candidates.push(ProposalCandidate {
                release_group,
                title,
                artist,
                primary_type,
                first_year: year(first_date.as_deref()),
                releases,
                change: change(&said),
                values: values
                    .into_iter()
                    .map(|value| {
                        file_value(
                            &paths,
                            value.item,
                            value.field,
                            value.ordinal,
                            value.value,
                            None,
                        )
                    })
                    .collect(),
            });
        }
        let mut files = Vec::new();
        let mut rests_on = Vec::new();
        if whole {
            let mut statement = connection.prepare(
                "SELECT si.provider_key FROM proposal_item pi JOIN source_item si ON si.id = pi.source_item_id
                 WHERE pi.proposal_id = ?1 ORDER BY si.provider_key",
            )?;
            files = statement
                .query_map([row], |row| row.get::<_, String>(0))?
                .collect::<Result<_, _>>()?;
            let mut statement = connection.prepare(
                "SELECT pe.disc_id,
                        (SELECT ed.found_json FROM evidence_disc ed WHERE ed.disc_id = pe.disc_id
                         ORDER BY ed.source_instance_id = p.source_instance_id AND ed.folder = p.folder DESC
                         LIMIT 1),
                        (SELECT count(DISTINCT ec.release_mbid) FROM evidence_claim ec
                         WHERE ec.lookup_response_id = lr.id),
                        lr.received_at_ms
                 FROM proposal_evidence pe
                 JOIN proposal p ON p.id = pe.proposal_id
                 JOIN lookup_response lr ON lr.id = pe.lookup_response_id
                 WHERE pe.proposal_id = ?1 ORDER BY pe.disc_id",
            )?;
            let evidence = statement.query_map([row], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, u32>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })?;
            for evidence in evidence {
                let (disc_id, found, releases, at) = evidence?;
                let ways: Vec<serde_json::Value> = found
                    .and_then(|json| serde_json::from_str(&json).ok())
                    .unwrap_or_default();
                let mut methods: Vec<LayoutMethod> = ways
                    .iter()
                    .filter_map(|way| {
                        way.get("method")?
                            .as_str()
                            .and_then(crate::evidence_store::method)
                    })
                    .collect();
                methods.dedup();
                rests_on.push(ProposalEvidence {
                    disc_id,
                    methods,
                    releases,
                    replied_at_ms: unsigned(at),
                });
            }
        }
        proposals.push(Proposal {
            proposal_id: Id::from_bytes(id),
            kind,
            group_id: Id::from_bytes(group),
            root,
            folder,
            file_count: u32::try_from(count).unwrap_or(u32::MAX),
            candidates,
            fingerprint,
            files,
            rests_on,
        });
    }
    Ok(proposals)
}

impl LibraryStore {
    /// `corrections.summary`.
    pub fn corrections_summary(&self) -> Result<CorrectionsSummary, Error> {
        self.read_snapshot(|connection| {
            let lookups = connection
                .query_row(
                    "SELECT value_json FROM library_setting WHERE key = 'corrections.lookups'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
                .as_deref()
                == Some("true");
            let (open, groups): (i64, i64) = connection.query_row(
                "SELECT count(*), count(DISTINCT group_public_id) FROM proposal",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            let mut counted: HashMap<String, i64> = HashMap::new();
            for (said, count) in rows(
                connection,
                "SELECT change, count(*) FROM proposal_candidate WHERE ordinal = 0 GROUP BY change",
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            )? {
                counted.insert(said, count);
            }
            let count = |said: &str| unsigned(counted.get(said).copied().unwrap_or(0));
            Ok(CorrectionsSummary {
                library_revision: revision(connection)?,
                lookups,
                open: unsigned(open),
                groups: unsigned(groups),
                moves: count("moves"),
                gains: count("gains"),
                knows: count("knows"),
            })
        })
    }

    /// `corrections.list`.
    pub fn proposals(&self) -> Result<ProposalList, Error> {
        self.read_snapshot(|connection| {
            Ok(ProposalList {
                library_revision: revision(connection)?,
                proposals: read_proposals(connection, None)?,
            })
        })
    }

    /// `corrections.get`: one proposal whole, at the revision it was read.
    pub fn proposal(&self, id: Id) -> Result<(u64, Proposal), Error> {
        self.read_snapshot(move |connection| {
            let found = read_proposals(connection, Some(id))?;
            let proposal = found
                .into_iter()
                .next()
                .ok_or(Error::NotFound("no such proposal"))?;
            Ok((revision(connection)?, proposal))
        })
    }

    /// `corrections.preview`: the projector run on the library as it is,
    /// and with the values the choices would put on the files, read-only.
    pub fn preview(&self, choices: &[Choice]) -> Result<PreviewShown, Error> {
        let choices = choices.to_vec();
        self.read_snapshot(move |connection| {
            let mut each: Vec<Vec<Value>> = Vec::new();
            // What each choice would put together: its files, the first
            // disc it joins them onto, and its album.
            let mut joining: Vec<(Vec<i64>, Vec<i64>, String)> = Vec::new();
            let mut seen = std::collections::HashSet::new();
            for choice in &choices {
                if !seen.insert(choice.proposal_id) {
                    return Err(Error::Invalid(
                        "a proposal is chosen once, with one candidate",
                    ));
                }
                let row: Option<i64> = connection
                    .query_row(
                        "SELECT id FROM proposal WHERE public_id = ?1",
                        [choice.proposal_id.as_bytes()],
                        |row| row.get(0),
                    )
                    .optional()?;
                let row = row.ok_or(Error::NotFound("no such proposal"))?;
                let found: Option<(String, String, String)> = connection
                    .query_row(
                        "SELECT values_json, onto_json, release_group_mbid FROM proposal_candidate
                         WHERE proposal_id = ?1 AND (?2 IS NULL OR release_group_mbid = ?2)
                         ORDER BY ordinal LIMIT 1",
                        (row, &choice.release_group),
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )
                    .optional()?;
                let (json, onto, release_group) =
                    found.ok_or(Error::NotFound("no such candidate"))?;
                let items: Vec<i64> = connection
                    .prepare_cached(
                        "SELECT source_item_id FROM proposal_item WHERE proposal_id = ?1",
                    )?
                    .query_map([row], |row| row.get(0))?
                    .collect::<Result<_, _>>()?;
                joining.push((
                    items,
                    serde_json::from_str::<Vec<i64>>(&onto)?,
                    release_group,
                ));
                let stored: Vec<StoredValue> = serde_json::from_str(&json)?;
                each.push(stored.iter().filter_map(StoredValue::value).collect());
            }
            // Two choices that give one field of one file different values
            // cannot both be said, as a confirm of them together refuses.
            if disagree(&each) {
                return Err(Error::Invalid("the choices disagree about a file"));
            }
            let mut chosen: Vec<Value> = each.into_iter().flatten().collect();
            chosen.sort();
            chosen.dedup();
            let (library, _) = load_in(connection)?;
            // What each choice puts together, with every choice's values in
            // place, as a confirm of them together works it out.
            let joins: Vec<_> = {
                let mut chosen_in = library.clone();
                values::apply(&mut chosen_in, &chosen);
                joining
                    .iter()
                    .map(|(items, onto, group)| values::join_of(&chosen_in, items, onto, group))
                    .collect()
            };
            let shown = values::preview(&library, &chosen, &joins, &Policy::current());
            let shape = album_shape;
            let paths = paths(connection)?;
            let facts: HashMap<i64, &ritornel_domain::Facts> = library
                .items
                .iter()
                .map(|item| (item.id, &item.facts))
                .collect();
            Ok(PreviewShown {
                library_revision: revision(connection)?,
                now: shown.now.into_iter().map(shape).collect(),
                after: shown.after.into_iter().map(shape).collect(),
                songs_joined: shown.songs_joined,
                songs_parted: shown.songs_parted,
                undoes: Vec::new(),
                values: chosen
                    .into_iter()
                    .map(|value| {
                        let now = facts
                            .get(&value.item)
                            .and_then(|facts| now_of(facts, value.field, value.ordinal));
                        file_value(
                            &paths,
                            value.item,
                            value.field.to_owned(),
                            value.ordinal,
                            value.value,
                            now,
                        )
                    })
                    .collect(),
            })
        })
    }

    /// `corrections.preview` of an undo: the undo itself, rehearsed in a
    /// write that is rolled back, so what it shows is what the undo does.
    /// It gives the albums the files it touches are on now and after, each
    /// value it withdraws with what the file says now and would say after,
    /// and every answer it undoes.
    pub fn preview_undo(&self, answers: &[Id]) -> Result<PreviewShown, Error> {
        let answers = answers.to_vec();
        let rehearsed = self.rehearse(move |write| {
            let connection = write.connection();
            // The revision before the rehearsal's, which is rolled back.
            let library_revision = unsigned(write.rev() - 1);
            let asked = crate::answer_store::asked_to_undo(connection, &answers)?;
            let all = crate::answer_store::with_dependents(connection, &asked)?;
            // What the answers said, and every file they are about: those
            // they gave values, and those they moved.
            let mut said: Vec<(i64, String, u32)> = Vec::new();
            let mut touched: std::collections::BTreeSet<i64> = std::collections::BTreeSet::new();
            for answer in &all {
                said.extend(
                    connection
                        .prepare_cached(
                            "SELECT source_item_id, field, ordinal FROM answer_value
                             WHERE answer_id = ?1 AND withdrawn_by_answer_id IS NULL",
                        )?
                        .query_map([answer], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
                        .collect::<Result<Vec<_>, _>>()?,
                );
                let before: String = connection.query_row(
                    "SELECT before_json FROM answer WHERE id = ?1",
                    [answer],
                    |row| row.get(0),
                )?;
                let before: serde_json::Value = serde_json::from_str(&before)?;
                touched.extend(
                    before
                        .get("items")
                        .and_then(serde_json::Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(|was| was.get("item")?.as_i64()),
                );
            }
            touched.extend(said.iter().map(|(item, _, _)| *item));
            let mut undoes = Vec::new();
            for answer in &all {
                let public: [u8; 16] = connection.query_row(
                    "SELECT public_id FROM answer WHERE id = ?1",
                    [answer],
                    |row| row.get(0),
                )?;
                undoes.push(Id::from_bytes(public));
            }
            let (now, _) = load_in(connection)?;
            let request = crate::WriteRequest {
                method: "corrections.preview",
                command: None,
                client: None,
            };
            crate::answer_store::undo_in(write, &request, &answers)?;
            let (after, _) = load_in(connection)?;
            let paths = paths_of(connection, touched.iter().copied())?;
            Ok((library_revision, said, touched, undoes, now, after, paths))
        })?;
        // Worked out once the rehearsal is rolled back, so the writer is
        // held no longer than the undo itself takes.
        let (library_revision, said, touched, undoes, now, after, paths) = rehearsed;
        let shown = values::preview_between(&now, &after, &touched, &Policy::current());
        let shape = album_shape;
        let facts_of =
            |library: &ritornel_domain::library::Library| -> HashMap<i64, ritornel_domain::Facts> {
                library
                    .items
                    .iter()
                    .filter(|item| touched.contains(&item.id))
                    .map(|item| (item.id, item.facts.clone()))
                    .collect()
            };
        let (facts_now, facts_after) = (facts_of(&now), facts_of(&after));
        // Each field of each file, with how many values the answers
        // gave it.
        let mut by_field: std::collections::BTreeMap<(i64, String), u32> =
            std::collections::BTreeMap::new();
        for (item, field, ordinal) in said {
            let count = by_field.entry((item, field)).or_default();
            *count = (*count).max(ordinal + 1);
        }
        let values = by_field
            .into_iter()
            .flat_map(|((item, field), ordinals)| {
                let read = |facts: &HashMap<i64, ritornel_domain::Facts>, ordinal| {
                    facts
                        .get(&item)
                        .and_then(|facts| now_of(facts, &field, ordinal))
                };
                // Every value of the field, now or after: an answer that
                // put one where the file had two gives both back.
                let count = |facts: &HashMap<i64, ritornel_domain::Facts>| {
                    facts.get(&item).map_or(0, |facts| values_of(facts, &field))
                };
                let shown = ordinals.max(count(&facts_now)).max(count(&facts_after));
                (0..shown)
                    .map(|ordinal| {
                        let (was, will) = (read(&facts_now, ordinal), read(&facts_after, ordinal));
                        file_value(&paths, item, field.clone(), ordinal, will, was)
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
        Ok(PreviewShown {
            library_revision,
            now: shown.now.into_iter().map(shape).collect(),
            after: shown.after.into_iter().map(shape).collect(),
            songs_joined: shown.songs_joined,
            songs_parted: shown.songs_parted,
            undoes,
            values,
        })
    }
}
