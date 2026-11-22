//! The resolver and the projector against `library.db`: reading the
//! library as the resolver sees it, and writing what its resolution asks
//! for. See steps 7 and 8 of "Scanning one file", "Displayed metadata" and
//! "The proof" in docs/design/identity.md.
//!
//! The resolution says what the catalog should be. [`Plan`] compares it
//! with what the rows hold and keeps only what differs, so a resolution
//! that changes nothing writes nothing, and takes no library revision.

use std::collections::{HashMap, HashSet};

use ritornel_domain::library::{
    Current, Evidence, ExistingCredit, ExistingRelease, Item, Kind, Library, Method, Ref,
    Resolution,
};
use ritornel_domain::normalize::normalize;
use ritornel_domain::{
    CATALOG_VERSION, Claim, EXTRACTOR_VERSION, Facts, NORMALIZER_VERSION, Policy, RESOLVER_VERSION,
    Scope, Source,
};
use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;

use crate::library_store::Write;
use crate::scan_store::{PENDING_SETTING, VERSIONS_SETTING};
use crate::walk::folder_of;
use crate::{Error, LibraryStore, WriteRequest, Written};

type Text = Option<String>;

/// What a release group shows: kind, marks, first date, first year, and
/// the release shown.
type GroupShown = (Text, String, Text, Option<i64>, Option<i64>);
/// What a release shows: group, title, credit, date, year and picture.
type ReleaseShown = (i64, Text, Option<i64>, Text, Option<i64>, Option<i64>);
/// What an appearance shows: title, credit, disc, track, duration, added.
type AppearanceShown = (
    Text,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
);

/// What the rows show now, for the plan to compare with.
#[derive(Debug, Default)]
pub(crate) struct Shown {
    artists: HashMap<i64, (Text, Text, Text)>,
    recordings: HashMap<i64, (Text, Option<i64>)>,
    groups: HashMap<i64, GroupShown>,
    releases: HashMap<i64, ReleaseShown>,
    appearances: HashMap<i64, AppearanceShown>,
    search: HashMap<i64, (String, String, String)>,
    versions: Text,
    /// The library revision all of this was read at.
    revision: i64,
}

/// The versions a library resolved by this resolver version records: the
/// next resolution after any of them changes is owed.
fn versions(resolver: i64) -> String {
    format!(
        "{{\"extractor\":{EXTRACTOR_VERSION},\"resolver\":{resolver},\"normalizer\":{NORMALIZER_VERSION},\"catalog\":{CATALOG_VERSION}}}"
    )
}

/// A decision's evidence as it is stored: what the resolver gave, and a
/// match it held back with the recording that match names as a row.
#[derive(Serialize)]
struct StoredEvidence<'a> {
    #[serde(flatten)]
    evidence: &'a Evidence,
    #[serde(skip_serializing_if = "Option::is_none")]
    withheld: Option<StoredWithheld>,
}

#[derive(Serialize)]
struct StoredWithheld {
    cause: &'static str,
    item: i64,
    recording: Option<i64>,
}

pub(crate) fn rows<T>(
    connection: &Connection,
    sql: &str,
    read: impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
) -> Result<Vec<T>, Error> {
    let mut statement = connection.prepare(sql)?;
    let rows = statement.query_map([], read)?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// The ID each artist row is known by: the one the files whose credits
/// name it claim at its place in the credit, counted only where a file
/// claims as many IDs as the credit has names, which is when the credit
/// was made from the IDs. The most claimed wins, then the least.
pub(crate) fn artist_mbids(connection: &Connection) -> Result<HashMap<i64, String>, Error> {
    let mut votes: HashMap<i64, HashMap<String, i64>> = HashMap::new();
    for (row, mbid, count) in rows(
        connection,
        "WITH placed AS (
           SELECT a.source_item_id AS item, ta.display_artist_credit_id AS credit, 'mb_artist' AS namespace
           FROM assignment a JOIN track_appearance ta ON ta.id = a.appearance_id
           WHERE a.superseded_at_ms IS NULL
           UNION ALL
           SELECT a.source_item_id, rel.display_artist_credit_id, 'mb_album_artist'
           FROM assignment a JOIN track_appearance ta ON ta.id = a.appearance_id
           JOIN release rel ON rel.id = ta.release_id
           WHERE a.superseded_at_ms IS NULL)
         SELECT n.artist_id, c.value, count(*)
         FROM placed p
         JOIN artist_credit_name n ON n.artist_credit_id = p.credit
         JOIN identifier_claim c ON c.source_item_id = p.item AND c.namespace = p.namespace
              AND c.ordinal = n.position
         WHERE (SELECT count(*) FROM identifier_claim o
                WHERE o.source_item_id = p.item AND o.namespace = p.namespace)
             = (SELECT count(*) FROM artist_credit_name o WHERE o.artist_credit_id = p.credit)
         GROUP BY 1, 2",
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        },
    )? {
        votes.entry(row).or_default().insert(mbid, count);
    }
    let mut mbids = HashMap::new();
    for (row, counted) in votes {
        if let Some((mbid, _)) = counted
            .into_iter()
            .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(&a.0)))
        {
            mbids.insert(row, mbid);
        }
    }
    Ok(mbids)
}

/// Reads the library as the resolver sees it, in one read transaction.
pub(crate) fn load(connection: &Connection) -> Result<(Library, Shown), Error> {
    let transaction = connection.unchecked_transaction()?;
    load_in(&transaction)
}

/// Reads the library as the resolver sees it, in the caller's transaction.
pub(crate) fn load_in(connection: &Connection) -> Result<(Library, Shown), Error> {
    let mut library = Library::default();
    let mut shown = Shown::default();

    let mut claims: HashMap<i64, Vec<Claim>> = HashMap::new();
    for (identifier, table, column) in [
        (false, "metadata_claim", "field"),
        (true, "identifier_claim", "namespace"),
    ] {
        let sql = format!(
            "SELECT c.source_item_id, c.scope, c.{column}, c.value, c.ordinal, s.kind
             FROM {table} c JOIN metadata_source s ON s.id = c.metadata_source_id"
        );
        let found = rows(connection, &sql, |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, u32>(4)?,
                row.get::<_, String>(5)?,
            ))
        })?;
        for (item, scope, name, value, ordinal, source) in found {
            let (Some(scope), Some(source)) = (Scope::parse(&scope), Source::parse(&source)) else {
                continue;
            };
            claims.entry(item).or_default().push(Claim {
                identifier,
                scope,
                name,
                value,
                ordinal,
                source,
            });
        }
    }

    // The cover of a file: its embedded front cover, or else its folder's
    // image, or else the first picture it carries.
    let mut covers: HashMap<i64, (u8, i64)> = HashMap::new();
    let pictures = rows(
        connection,
        "SELECT si.id, rp.picture_id, rp.source_kind = 'embedded', rp.picture_type
         FROM revision_picture rp JOIN source_item si ON si.current_revision_id = rp.revision_id
         ORDER BY si.id, rp.position",
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, bool>(2)?,
                row.get::<_, Option<i64>>(3)?,
            ))
        },
    )?;
    for (item, picture, embedded, kind) in pictures {
        let rank = match (embedded, kind) {
            (true, Some(3)) => 0,
            (false, _) => 1,
            (true, _) => 2,
        };
        let cover = covers.entry(item).or_insert((rank, picture));
        if rank < cover.0 {
            *cover = (rank, picture);
        }
    }

    // The bindings of files: a scrobble's text-only item is bound by the
    // scrobble matcher, never by this resolver, and its method is not one
    // of the resolver's.
    let mut current: HashMap<i64, Current> = HashMap::new();
    let assignments = rows(
        connection,
        "SELECT a.source_item_id, a.recording_id, a.appearance_id, d.method, d.actor_kind,
                d.needs_review, json_extract(d.evidence_json, '$.across_albums') IS NOT NULL
         FROM assignment a JOIN identity_decision d ON d.id = a.decision_id
         JOIN source_item si ON si.id = a.source_item_id
         WHERE a.superseded_at_ms IS NULL AND si.item_kind = 'file'",
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Option<i64>>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, bool>(5)?,
                row.get::<_, bool>(6)?,
            ))
        },
    )?;
    for (item, recording, appearance, method, actor, needs_review, across_albums) in assignments {
        current.insert(
            item,
            Current {
                recording,
                appearance,
                method: Method::parse(&method).ok_or(Error::Corrupt("a decision's method"))?,
                by_user: actor != "resolver",
                needs_review,
                across_albums,
            },
        );
    }

    let items = rows(
        connection,
        "SELECT si.id, si.source_instance_id, si.provider_key, sr.audio_asset_id, coalesce(aa.lossless, 0),
                si.retired_at_ms IS NOT NULL, si.first_seen_at_ms
         FROM source_item si
         LEFT JOIN source_item_revision sr ON sr.id = si.current_revision_id
         LEFT JOIN audio_asset aa ON aa.id = sr.audio_asset_id
         WHERE si.item_kind = 'file'
         ORDER BY si.first_seen_at_ms, si.id",
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<i64>>(3)?,
                row.get::<_, bool>(4)?,
                row.get::<_, bool>(5)?,
                row.get::<_, i64>(6)?,
            ))
        },
    )?;
    for (id, source, path, audio, lossless, retired, first_seen_ms) in items {
        library.items.push(Item {
            id,
            folder: format!("{source}/{}", folder_of(&path)),
            audio,
            lossless,
            retired,
            first_seen_ms,
            cover: covers.get(&id).map(|(_, picture)| *picture),
            facts: Facts::from_claims(claims.get(&id).map_or(&[], Vec::as_slice)),
            current: current.get(&id).copied(),
        });
    }

    for (id, canonical, title, credit) in rows(
        connection,
        "SELECT id, canonical_id, display_title, display_artist_credit_id FROM recording",
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get(2)?,
                row.get(3)?,
            ))
        },
    )? {
        if canonical != id {
            library.recording_canonical.insert(id, canonical);
        }
        shown.recordings.insert(id, (title, credit));
    }
    for (id, recording, release, display) in rows(
        connection,
        "SELECT id, recording_id, release_id, display_title, display_artist_credit_id, display_disc_number,
                display_track_number, display_duration_ms, display_added_at_ms
         FROM track_appearance",
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Option<i64>>(2)?,
                (row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?, row.get(8)?),
            ))
        },
    )? {
        library.appearances.insert(id, (recording, release));
        shown.appearances.insert(id, display);
    }
    for (id, existing, display) in rows(
        connection,
        "SELECT id, canonical_id, chosen_release_group_id, release_group_id, display_title,
                display_artist_credit_id, display_date, display_year, display_picture_id
         FROM release",
        |row| {
            let group: i64 = row.get(3)?;
            Ok((
                row.get::<_, i64>(0)?,
                ExistingRelease {
                    canonical: row.get(1)?,
                    chosen_group: row.get(2)?,
                    group,
                },
                (
                    group,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                ),
            ))
        },
    )? {
        library.releases.insert(id, existing);
        shown.releases.insert(id, display);
    }
    for (id, preferred, display) in rows(
        connection,
        "SELECT id, preferred_release_id, display_primary_type, display_secondary_types_json,
                display_first_date, display_first_year, display_release_id
         FROM release_group",
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Option<i64>>(1)?,
                (
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ),
            ))
        },
    )? {
        library.groups.insert(id, preferred);
        shown.groups.insert(id, display);
    }
    // A row is found by the key the normaliser made of its names. When the
    // library was last resolved by another normaliser, the stored keys are
    // that one's, and the resolver looks rows up by them instead, or an
    // artist with no ID would take a new row and a new public ID.
    library.artist_keys_by = connection
        .query_row(
            "SELECT json_extract(value_json, '$.normalizer') FROM library_setting WHERE key = ?1",
            [VERSIONS_SETTING],
            |row| row.get::<_, Option<i64>>(0),
        )
        .optional()?
        .flatten()
        .filter(|stored| *stored != NORMALIZER_VERSION);
    for (id, redirected, name, sort, match_name) in rows(
        connection,
        "SELECT id, redirect_to_id IS NOT NULL, display_name, display_sort_name, match_name
         FROM artist ORDER BY id",
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, bool>(1)?,
                row.get::<_, Text>(2)?,
                row.get::<_, Text>(3)?,
                row.get::<_, Text>(4)?,
            ))
        },
    )? {
        // A row given no key is still found by its ID.
        if !redirected {
            library
                .artists
                .push((id, match_name.clone().unwrap_or_default()));
        }
        shown.artists.insert(id, (name, sort, match_name));
    }
    let mut credits: HashMap<i64, ExistingCredit> = HashMap::new();
    for (id, text) in rows(
        connection,
        "SELECT id, display_text FROM artist_credit",
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
    )? {
        credits.insert(
            id,
            ExistingCredit {
                id,
                text,
                names: Vec::new(),
            },
        );
    }
    for (credit, artist, name, join) in rows(
        connection,
        "SELECT artist_credit_id, artist_id, credited_name, join_phrase
         FROM artist_credit_name ORDER BY artist_credit_id, position",
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        },
    )? {
        if let Some(credit) = credits.get_mut(&credit) {
            credit.names.push((artist, name, join));
        }
    }
    library.credits = credits.into_values().collect();
    library.credits.sort_by_key(|credit| credit.id);

    library.artist_mbids = artist_mbids(connection)?;

    library.exclusions = rows(
        connection,
        "SELECT source_item_id, recording_id FROM identity_exclusion",
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?
    .into_iter()
    .collect();
    // Pairs the owner's own actions linked. What the resolver learned it
    // learns again, in the order it learned it. A pair is of keys, and a key
    // made by another version of the normaliser is not comparable with
    // today's, so such a pair is not read: the call that teaches one keeps
    // its spellings, to make its keys again.
    for (scope, a, b) in rows(
        connection,
        &format!(
            "SELECT o.scope, o.name_a, o.name_b FROM alias_observation o
             JOIN identity_decision d ON d.id = o.decision_id
             WHERE d.actor_kind <> 'resolver' AND o.normalizer_version = {NORMALIZER_VERSION}"
        ),
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        },
    )? {
        match scope.as_str() {
            "title" => library.aliases.push((Kind::Recording, a, b)),
            "artist" => library.aliases.push((Kind::Artist, a, b)),
            _ => {}
        }
    }
    for (kind, entity, field, value) in rows(
        connection,
        "SELECT entity_kind, entity_id, field, value FROM metadata_edit
         WHERE withdrawn_at_ms IS NULL AND ordinal = 0",
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        },
    )? {
        let kind = match kind.as_str() {
            "recording" => Kind::Recording,
            "track_appearance" => Kind::TrackAppearance,
            "release" => Kind::Release,
            "release_group" => Kind::ReleaseGroup,
            _ => Kind::Artist,
        };
        library.edits.insert((kind, entity, field), value);
    }
    for (row, title, artist, album) in rows(
        connection,
        "SELECT rowid, title, artist, album FROM search_text",
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Text>(1)?,
                row.get::<_, Text>(2)?,
                row.get::<_, Text>(3)?,
            ))
        },
    )? {
        shown.search.insert(
            row,
            (
                title.unwrap_or_default(),
                artist.unwrap_or_default(),
                album.unwrap_or_default(),
            ),
        );
    }
    shown.revision =
        connection.query_row("SELECT rev FROM library_meta WHERE id = 1", [], |row| {
            row.get(0)
        })?;
    shown.versions = connection
        .query_row(
            "SELECT value_json FROM library_setting WHERE key = ?1",
            [VERSIONS_SETTING],
            |row| row.get(0),
        )
        .optional()?;
    Ok((library, shown))
}

/// What a resolution changes: the resolution, and for each entry whether
/// it differs from what its row holds.
#[derive(Debug)]
pub(crate) struct Plan {
    resolution: Resolution,
    canonical: HashMap<i64, i64>,
    artists: Vec<bool>,
    recordings: Vec<bool>,
    groups: Vec<bool>,
    group_shows: Vec<bool>,
    releases: Vec<bool>,
    appearances: Vec<bool>,
    search: Vec<Option<(String, String, String)>>,
    /// Appearances in the search index that no file is bound to any more.
    unsearchable: Vec<i64>,
    /// Artist rows no file credits now, keyed again after the normaliser
    /// changed, so that their key is today's when their files return, or
    /// given none.
    rekeyed: Vec<(i64, Option<String>)>,
    versions: bool,
    /// The version of the policy that made the resolution, which its
    /// decisions record.
    resolver: i64,
    /// The library revision the plan was made from. A write at any other
    /// is refused, because what the plan supersedes may have changed.
    based_on: i64,
}

/// What a resolution wrote.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Resolved {
    /// Assignments written.
    pub assignments: u64,
    /// Recordings created.
    pub recordings: u64,
    /// Releases created.
    pub releases: u64,
    /// Rows whose display changed.
    pub displayed: u64,
}

fn as_row(r: Ref) -> Option<i64> {
    match r {
        Ref::Row(row) => Some(row),
        Ref::New(_) => None,
    }
}

impl Plan {
    /// The plan for a resolution that the policy of version `resolver`
    /// made.
    pub(crate) fn new(
        resolution: Resolution,
        library: &Library,
        shown: &Shown,
        resolver: i64,
    ) -> Self {
        let credit = |at: Option<usize>| {
            at.and_then(|at| resolution.credits.get(at))
                .map(|credit| credit.r)
        };
        // A reference that is a row and equals what the row holds.
        let same = |wanted: Option<Ref>, held: Option<i64>| match wanted {
            None => held.is_none(),
            Some(wanted) => as_row(wanted).is_some() && as_row(wanted) == held,
        };
        let artists = resolution
            .artists
            .iter()
            .map(|out| {
                let wanted = (
                    Some(out.name.clone()),
                    Some(out.sort_name.clone()),
                    Some(out.match_name.clone()),
                );
                as_row(out.r).and_then(|row| shown.artists.get(&row)) != Some(&wanted)
            })
            .collect();
        let recordings = resolution
            .recordings
            .iter()
            .map(
                |out| match as_row(out.r).and_then(|row| shown.recordings.get(&row)) {
                    Some((title, held)) => {
                        out.projected && (*title != out.title || !same(credit(out.credit), *held))
                    }
                    None => true,
                },
            )
            .collect();
        let release = |at: Option<usize>| {
            at.and_then(|at| resolution.releases.get(at))
                .map(|release| release.r)
        };
        let (mut groups, mut group_shows) = (Vec::new(), Vec::new());
        for out in &resolution.groups {
            let marks =
                serde_json::to_string(&out.secondary_types).unwrap_or_else(|_| "[]".to_owned());
            match as_row(out.r).and_then(|row| shown.groups.get(&row)) {
                Some((kind, held_marks, date, year, shows)) => {
                    groups.push(
                        *kind != out.primary_type
                            || *held_marks != marks
                            || *date != out.first_date
                            || *year != out.first_year,
                    );
                    group_shows.push(!same(release(out.display_release), *shows));
                }
                None => {
                    groups.push(true);
                    group_shows.push(out.display_release.is_some());
                }
            }
        }
        let releases = resolution
            .releases
            .iter()
            .map(|out| {
                let group = resolution.groups.get(out.group).map(|group| group.r);
                match as_row(out.r).and_then(|row| shown.releases.get(&row)) {
                    Some((held_group, title, held_credit, date, year, picture)) => {
                        !same(group, Some(*held_group))
                            || *title != out.title
                            || !same(credit(out.credit), *held_credit)
                            || *date != out.date
                            || *year != out.year
                            || *picture != out.picture
                    }
                    None => true,
                }
            })
            .collect();
        let (mut appearances, mut search) = (Vec::new(), Vec::new());
        for out in &resolution.appearances {
            let held = as_row(out.r).and_then(|row| shown.appearances.get(&row));
            appearances.push(match held {
                Some((title, held_credit, disc, track, duration, added)) => {
                    *title != out.title
                        || !same(credit(out.credit), *held_credit)
                        || *disc != out.disc
                        || *track != out.track
                        || *duration != out.duration_ms
                        || *added != out.added_at_ms
                }
                None => true,
            });
            let text = (
                out.title.clone().unwrap_or_default(),
                out.credit
                    .and_then(|at| resolution.credits.get(at))
                    .map(|credit| credit.text.clone())
                    .unwrap_or_default(),
                out.release
                    .and_then(|at| resolution.releases.get(at))
                    .and_then(|release| release.title.clone())
                    .unwrap_or_default(),
            );
            let indexed = as_row(out.r).and_then(|row| shown.search.get(&row));
            search.push((indexed != Some(&text)).then_some(text));
        }
        let bound: std::collections::HashSet<i64> = resolution
            .items
            .iter()
            .filter_map(|item| item.appearance.and_then(as_row))
            .collect();
        let mut unsearchable: Vec<i64> = shown
            .search
            .keys()
            .filter(|row| !bound.contains(row))
            .copied()
            .collect();
        unsearchable.sort_unstable();
        // After the normaliser changed, a row with no ID that the
        // resolution does not reach is keyed again from its name, so its
        // files find it when they return. It is given no key, and is never
        // found again, when that is not safe: its name is the owner's edit,
        // which its files never gave, or the key is one an artist of the
        // resolution holds, or another such row would take it too.
        let mut rekeyed: Vec<(i64, Option<String>)> = Vec::new();
        if library.artist_keys_by.is_some() {
            let reached: HashSet<i64> = resolution
                .artists
                .iter()
                .filter_map(|out| as_row(out.r))
                .collect();
            let held: HashSet<&str> = resolution
                .artists
                .iter()
                .map(|out| out.match_name.as_str())
                .collect();
            // A name is trusted only when it gives back the key the row
            // holds: an edit, in force or withdrawn, gives another.
            let version = library.artist_keys_by;
            let left: Vec<(i64, &String, Option<String>)> = library
                .artists
                .iter()
                .filter(|(row, _)| !reached.contains(row))
                .map(|(row, stored)| {
                    let edited =
                        library
                            .edits
                            .contains_key(&(Kind::Artist, *row, "name".to_owned()));
                    let name = shown
                        .artists
                        .get(row)
                        .and_then(|(name, _, _)| name.as_deref())
                        .filter(|name| {
                            !edited
                                && version
                                    .and_then(|version| {
                                        ritornel_domain::normalize::normalize_by(version, name)
                                    })
                                    .as_ref()
                                    == Some(stored)
                        });
                    (*row, stored, name.map(normalize))
                })
                .collect();
            let mut wanted: HashMap<&str, usize> = HashMap::new();
            for (_, _, key) in &left {
                if let Some(key) = key {
                    *wanted.entry(key.as_str()).or_default() += 1;
                }
            }
            for (row, stored, key) in &left {
                let key = key.as_ref().filter(|key| {
                    !held.contains(key.as_str()) && wanted.get(key.as_str()) == Some(&1)
                });
                let now = Some(*stored).filter(|stored| !stored.is_empty());
                if key != now {
                    rekeyed.push((*row, key.cloned()));
                }
            }
        }
        let mut plan = Self {
            unsearchable,
            rekeyed,
            canonical: library.recording_canonical.clone(),
            artists,
            recordings,
            groups,
            group_shows,
            releases,
            appearances,
            search,
            versions: false,
            resolver,
            based_on: shown.revision,
            resolution,
        };
        // The versions are written with the first thing written, and again
        // only when they change.
        let stale = shown
            .versions
            .as_ref()
            .is_some_and(|stored| *stored != versions(resolver));
        plan.versions = stale || (shown.versions.is_none() && !plan.is_empty());
        plan
    }

    /// The resolution the plan was made from.
    pub(crate) fn resolution(&self) -> &Resolution {
        &self.resolution
    }

    /// Whether the resolution asks for nothing: no write is made, and no
    /// library revision is taken.
    pub(crate) fn is_empty(&self) -> bool {
        let none = |flags: &[bool]| flags.iter().all(|flag| !flag);
        !self.versions
            && self.resolution.items.iter().all(|item| !item.changed)
            && self
                .resolution
                .credits
                .iter()
                .all(|credit| as_row(credit.r).is_some())
            && none(&self.artists)
            && none(&self.recordings)
            && none(&self.groups)
            && none(&self.group_shows)
            && none(&self.releases)
            && none(&self.appearances)
            && self.search.iter().all(Option::is_none)
            && self.unsearchable.is_empty()
            && self.rekeyed.is_empty()
    }

    /// Writes what differs, in the write's transaction.
    pub(crate) fn apply(self, write: &Write<'_>) -> Result<Resolved, Error> {
        // The write takes the revision after the one it was planned at, or
        // something was written in between.
        if write.rev() != self.based_on + 1 {
            return Err(Error::PlanStale);
        }
        let (connection, rev, now_ms) = (write.connection(), write.rev(), write.now_ms());
        let resolution = &self.resolution;
        let mut done = Resolved::default();
        // A row that names itself as the end of its own redirect chain is
        // given its key before it is inserted.
        let next = |table: &str| -> Result<i64, Error> {
            Ok(connection.query_row(
                &format!("SELECT coalesce(max(id), 0) + 1 FROM {table}"),
                [],
                |row| row.get(0),
            )?)
        };
        let missing = || Error::Corrupt("a resolution that names an entry it does not have");

        let mut next_artist = next("artist")?;
        let mut artist_rows = Vec::with_capacity(resolution.artists.len());
        for (out, dirty) in resolution.artists.iter().zip(&self.artists) {
            let row = match out.r {
                Ref::Row(row) => {
                    if *dirty {
                        connection.prepare_cached(
                            "UPDATE artist SET display_name = ?2, display_sort_name = ?3, match_name = ?4, rev = ?5
                             WHERE id = ?1",
                        )?
                        .execute((row, &out.name, &out.sort_name, &out.match_name, rev))?;
                        done.displayed += 1;
                    }
                    row
                }
                Ref::New(_) => {
                    connection.prepare_cached(
                        "INSERT INTO artist (id, public_id, canonical_id, display_name, display_sort_name, match_name, rev)
                         VALUES (?1, ?2, ?1, ?3, ?4, ?5, ?6)",
                    )?
                    .execute((next_artist, write.new_id()?.as_bytes(), &out.name, &out.sort_name, &out.match_name, rev))?;
                    next_artist += 1;
                    next_artist - 1
                }
            };
            artist_rows.push(row);
        }
        for (row, key) in &self.rekeyed {
            connection
                .prepare_cached("UPDATE artist SET match_name = ?2, rev = ?3 WHERE id = ?1")?
                .execute((row, key, rev))?;
        }

        let mut credit_rows = Vec::with_capacity(resolution.credits.len());
        for out in &resolution.credits {
            let row = match out.r {
                Ref::Row(row) => row,
                Ref::New(_) => {
                    connection
                        .prepare_cached(
                            "INSERT INTO artist_credit (display_text, rev) VALUES (?1, ?2)",
                        )?
                        .execute((&out.text, rev))?;
                    let row = connection.last_insert_rowid();
                    for (position, (artist, name, join)) in out.names.iter().enumerate() {
                        connection.prepare_cached(
                            "INSERT INTO artist_credit_name
                               (artist_credit_id, position, artist_id, credited_name, join_phrase, rev)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                        )?
                        .execute((row, position as i64, artist_rows.get(*artist).ok_or_else(missing)?, name, join, rev))?;
                    }
                    row
                }
            };
            credit_rows.push(row);
        }
        let credit = |at: Option<usize>| -> Result<Option<i64>, Error> {
            at.map(|at| credit_rows.get(at).copied().ok_or_else(missing))
                .transpose()
        };

        let mut next_recording = next("recording")?;
        let mut recording_rows = Vec::with_capacity(resolution.recordings.len());
        for (out, dirty) in resolution.recordings.iter().zip(&self.recordings) {
            let row = match out.r {
                Ref::Row(row) => {
                    if *dirty {
                        connection.prepare_cached(
                            "UPDATE recording SET display_title = ?2, display_artist_credit_id = ?3, rev = ?4 WHERE id = ?1",
                        )?
                        .execute((row, &out.title, credit(out.credit)?, rev))?;
                        done.displayed += 1;
                    }
                    row
                }
                Ref::New(_) => {
                    connection.prepare_cached(
                        "INSERT INTO recording (id, public_id, canonical_id, display_title, display_artist_credit_id, rev)
                         VALUES (?1, ?2, ?1, ?3, ?4, ?5)",
                    )?
                    .execute((next_recording, write.new_id()?.as_bytes(), &out.title, credit(out.credit)?, rev))?;
                    next_recording += 1;
                    done.recordings += 1;
                    next_recording - 1
                }
            };
            recording_rows.push(row);
        }
        let recording = |r: Ref| -> Result<i64, Error> {
            match r {
                Ref::Row(row) => Ok(row),
                Ref::New(at) => recording_rows.get(at).copied().ok_or_else(missing),
            }
        };

        let mut group_rows = Vec::with_capacity(resolution.groups.len());
        for (out, dirty) in resolution.groups.iter().zip(&self.groups) {
            let marks = serde_json::to_string(&out.secondary_types)?;
            let row = match out.r {
                Ref::Row(row) => {
                    if *dirty {
                        connection
                            .prepare_cached(
                                "UPDATE release_group
                             SET display_primary_type = ?2, display_secondary_types_json = ?3,
                                 display_first_date = ?4, display_first_year = ?5, rev = ?6
                             WHERE id = ?1",
                            )?
                            .execute((
                                row,
                                &out.primary_type,
                                &marks,
                                &out.first_date,
                                out.first_year,
                                rev,
                            ))?;
                        done.displayed += 1;
                    }
                    row
                }
                Ref::New(_) => {
                    connection.prepare_cached(
                        "INSERT INTO release_group
                           (public_id, display_primary_type, display_secondary_types_json, display_first_date,
                            display_first_year, rev)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    )?
                    .execute((write.new_id()?.as_bytes(), &out.primary_type, &marks, &out.first_date, out.first_year, rev))?;
                    connection.last_insert_rowid()
                }
            };
            group_rows.push(row);
        }

        let mut next_release = next("release")?;
        let mut release_rows = Vec::with_capacity(resolution.releases.len());
        for (out, dirty) in resolution.releases.iter().zip(&self.releases) {
            let group = group_rows.get(out.group).copied().ok_or_else(missing)?;
            let row = match out.r {
                Ref::Row(row) => {
                    if *dirty {
                        connection.prepare_cached(
                            "UPDATE release
                             SET release_group_id = ?2, display_title = ?3, display_artist_credit_id = ?4,
                                 display_date = ?5, display_year = ?6, display_picture_id = ?7, rev = ?8
                             WHERE id = ?1",
                        )?
                        .execute((row, group, &out.title, credit(out.credit)?, &out.date, out.year, out.picture, rev))?;
                        done.displayed += 1;
                    }
                    row
                }
                Ref::New(_) => {
                    connection.prepare_cached(
                        "INSERT INTO release
                           (id, public_id, release_group_id, canonical_id, display_title, display_artist_credit_id,
                            display_date, display_year, display_picture_id, rev)
                         VALUES (?1, ?2, ?3, ?1, ?4, ?5, ?6, ?7, ?8, ?9)",
                    )?
                    .execute((
                        next_release,
                        write.new_id()?.as_bytes(),
                        group,
                        &out.title,
                        credit(out.credit)?,
                        &out.date,
                        out.year,
                        out.picture,
                        rev,
                    ))?;
                    next_release += 1;
                    done.releases += 1;
                    next_release - 1
                }
            };
            release_rows.push(row);
        }
        let release = |at: Option<usize>| -> Result<Option<i64>, Error> {
            at.map(|at| release_rows.get(at).copied().ok_or_else(missing))
                .transpose()
        };
        for ((out, row), dirty) in resolution
            .groups
            .iter()
            .zip(&group_rows)
            .zip(&self.group_shows)
        {
            if *dirty {
                connection
                    .prepare_cached(
                        "UPDATE release_group SET display_release_id = ?2, rev = ?3 WHERE id = ?1",
                    )?
                    .execute((row, release(out.display_release)?, rev))?;
            }
        }

        let mut next_appearance = next("track_appearance")?;
        let mut appearance_rows = Vec::with_capacity(resolution.appearances.len());
        for ((out, dirty), search) in resolution
            .appearances
            .iter()
            .zip(&self.appearances)
            .zip(&self.search)
        {
            let row = match out.r {
                Ref::Row(row) => {
                    if *dirty {
                        connection.prepare_cached(
                            "UPDATE track_appearance
                             SET display_title = ?2, display_artist_credit_id = ?3, display_disc_number = ?4,
                                 display_track_number = ?5, display_duration_ms = ?6, display_added_at_ms = ?7, rev = ?8
                             WHERE id = ?1",
                        )?
                        .execute((
                            row,
                            &out.title,
                            credit(out.credit)?,
                            out.disc,
                            out.track,
                            out.duration_ms,
                            out.added_at_ms,
                            rev,
                        ))?;
                        done.displayed += 1;
                    }
                    row
                }
                Ref::New(_) => {
                    connection
                        .prepare_cached(
                            "INSERT INTO track_appearance
                           (id, public_id, recording_id, release_id, canonical_id, display_title,
                            display_artist_credit_id, display_disc_number, display_track_number,
                            display_duration_ms, display_added_at_ms, rev)
                         VALUES (?1, ?2, ?3, ?4, ?1, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                        )?
                        .execute((
                            next_appearance,
                            write.new_id()?.as_bytes(),
                            recording(out.recording)?,
                            release(out.release)?,
                            &out.title,
                            credit(out.credit)?,
                            out.disc,
                            out.track,
                            out.duration_ms,
                            out.added_at_ms,
                            rev,
                        ))?;
                    next_appearance += 1;
                    next_appearance - 1
                }
            };
            // The text search index, one row per appearance.
            if let Some((title, artist, album)) = search {
                connection
                    .prepare_cached("DELETE FROM search_text WHERE rowid = ?1")?
                    .execute([row])?;
                connection
                    .prepare_cached("INSERT INTO search_text (rowid, title, artist, album) VALUES (?1, ?2, ?3, ?4)")?
                    .execute((row, title, artist, album))?;
            }
            appearance_rows.push(row);
        }

        for row in &self.unsearchable {
            connection
                .prepare_cached("DELETE FROM search_text WHERE rowid = ?1")?
                .execute([row])?;
        }

        // Bindings: a decision and an assignment for each item whose result
        // differs. The row it replaces is superseded, never updated.
        for out in resolution.items.iter().filter(|out| out.changed) {
            let (Some(bound), Some(decision)) = (out.recording, &out.decision) else {
                continue;
            };
            let bound = recording(bound)?;
            let withheld = match &decision.evidence.withheld {
                Some(withheld) => Some(StoredWithheld {
                    cause: withheld.cause.as_str(),
                    item: withheld.item,
                    recording: withheld.recording.map(&recording).transpose()?,
                }),
                None => None,
            };
            let evidence = StoredEvidence {
                evidence: &decision.evidence,
                withheld,
            };
            let appearance = match out.appearance {
                Some(Ref::Row(row)) => Some(row),
                Some(Ref::New(at)) => Some(appearance_rows.get(at).copied().ok_or_else(missing)?),
                None => None,
            };
            connection.prepare_cached(
                "INSERT INTO identity_decision
                   (action, source_item_id, method, resolver_version, normalizer_version, score, runner_up_gap,
                    penalty_vector_json, evidence_json, needs_review, actor_kind, decided_at_ms, rev)
                 VALUES ('bind', ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'resolver', ?10, ?11)",
            )?
            .execute((
                out.item,
                decision.method.as_str(),
                self.resolver,
                NORMALIZER_VERSION,
                decision.score,
                decision.runner_up_gap,
                decision.penalties.as_ref().map(serde_json::to_string).transpose()?,
                serde_json::to_string(&evidence)?,
                decision.needs_review,
                now_ms,
                rev,
            ))?;
            let decided = connection.last_insert_rowid();
            let superseded: Option<i64> = connection
                .prepare_cached("SELECT id FROM assignment WHERE source_item_id = ?1 AND superseded_at_ms IS NULL")?
                .query_row([out.item], |row| row.get(0))
                .optional()?;
            if let Some(old) = superseded {
                connection
                    .prepare_cached(
                        "UPDATE assignment SET superseded_at_ms = ?2, rev = ?3 WHERE id = ?1",
                    )?
                    .execute((old, now_ms, rev))?;
            }
            connection.prepare_cached(
                "INSERT INTO assignment (source_item_id, recording_id, appearance_id, decision_id, effective_from_ms, rev)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?
            .execute((out.item, bound, appearance, decided, now_ms, rev))?;
            if let Some(old) = superseded {
                connection
                    .prepare_cached("UPDATE assignment SET superseded_by_id = ?2 WHERE id = ?1")?
                    .execute((old, connection.last_insert_rowid()))?;
            }
            connection
                .prepare_cached(
                    "UPDATE source_item SET current_recording_id = ?2, rev = ?3 WHERE id = ?1",
                )?
                .execute((
                    out.item,
                    self.canonical.get(&bound).copied().unwrap_or(bound),
                    rev,
                ))?;
            for (kind, a, b) in &decision.aliases {
                let scope = if *kind == Kind::Artist {
                    "artist"
                } else {
                    "title"
                };
                connection.prepare_cached(
                    "INSERT INTO alias_observation (scope, name_a, name_b, normalizer_version, decision_id, observed_at_ms, rev)
                     SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7
                     WHERE NOT EXISTS (SELECT 1 FROM alias_observation
                                       WHERE scope = ?1 AND name_a = ?2 AND name_b = ?3 AND normalizer_version = ?4)",
                )?
                .execute((scope, a, b, NORMALIZER_VERSION, decided, now_ms, rev))?;
            }
            done.assignments += 1;
        }

        // The resolution that was owed has been made.
        connection.execute(
            "DELETE FROM library_setting WHERE key = ?1",
            [PENDING_SETTING],
        )?;
        if self.versions {
            connection.execute(
                "INSERT INTO library_setting (key, value_json, rev) VALUES (?1, ?2, ?3)
                 ON CONFLICT (key) DO UPDATE SET value_json = excluded.value_json, rev = excluded.rev",
                (VERSIONS_SETTING, versions(self.resolver), rev),
            )?;
        }
        Ok(done)
    }
}

impl LibraryStore {
    /// Reads the library as the resolver sees it.
    pub(crate) fn load(&self) -> Result<(Library, Shown), Error> {
        self.read(load)
    }

    /// Whether anything is waiting on the resolver: a readable file with
    /// no binding, or a library last resolved by another version of the
    /// extractor, the resolver, the normaliser or the catalog. A new resolver version
    /// makes the next scan resolve every item again by the policy in force,
    /// and write a decision of that version for each whose result differs.
    pub(crate) fn needs_resolving(&self) -> Result<bool, Error> {
        self.read(|connection| {
            let stored: Text = connection
                .query_row("SELECT value_json FROM library_setting WHERE key = ?1", [VERSIONS_SETTING], |row| {
                    row.get(0)
                })
                .optional()?;
            if stored.is_some_and(|stored| stored != versions(RESOLVER_VERSION)) {
                return Ok(true);
            }
            let pending: bool = connection.query_row(
                "SELECT EXISTS (SELECT 1 FROM library_setting WHERE key = ?1)",
                [PENDING_SETTING],
                |row| row.get(0),
            )?;
            if pending {
                return Ok(true);
            }
            Ok(connection.query_row(
                "SELECT EXISTS (
                   SELECT 1 FROM source_item si JOIN source_item_revision sr ON sr.id = si.current_revision_id
                   WHERE sr.audio_asset_id IS NOT NULL AND si.retired_at_ms IS NULL
                     AND si.current_recording_id IS NULL)",
                [],
                |row| row.get(0),
            )?)
        })
    }

    /// Resolves the whole library by the policy scans use and writes what
    /// differs, in one transaction. It returns `None`, and writes nothing, when the
    /// resolution asks for no change.
    /// A plan that something else's write made stale is made again, a few
    /// times.
    pub fn resolve(&self, request: WriteRequest) -> Result<Option<(Written, Resolved)>, Error> {
        let mut tries = 0;
        loop {
            let Some(plan) = self.plan()? else {
                return self.settle_pending(request);
            };
            match self.apply_plan(request, plan) {
                Err(Error::PlanStale) if tries < 5 => tries += 1,
                done => return done.map(Some),
            }
        }
    }

    /// Resolves the whole library by the policy scans use and plans what
    /// differs. `None` when nothing does.
    pub(crate) fn plan(&self) -> Result<Option<Plan>, Error> {
        self.plan_by(&Policy::current())
    }

    /// Resolves the whole library by a policy and plans what differs. Only
    /// tests apply a policy other than the current one: to make a library
    /// as an earlier version left it.
    pub(crate) fn plan_by(&self, policy: &Policy) -> Result<Option<Plan>, Error> {
        let (library, shown) = self.load()?;
        let resolution = ritornel_domain::resolve(&library, policy);
        let plan = Plan::new(resolution, &library, &shown, policy.version);
        Ok((!plan.is_empty()).then_some(plan))
    }

    /// A resolution that asks for nothing still completes what was owed:
    /// the pending mark is removed, which is a write. With no mark there is
    /// nothing to write.
    fn settle_pending(&self, request: WriteRequest) -> Result<Option<(Written, Resolved)>, Error> {
        let pending: bool = self.read(|connection| {
            Ok(connection.query_row(
                "SELECT EXISTS (SELECT 1 FROM library_setting WHERE key = ?1)",
                [PENDING_SETTING],
                |row| row.get(0),
            )?)
        })?;
        if !pending {
            return Ok(None);
        }
        let written = self.write(request, |write| {
            write.connection().execute(
                "DELETE FROM library_setting WHERE key = ?1",
                [PENDING_SETTING],
            )?;
            Ok(serde_json::to_string(&Resolved::default())?)
        })?;
        Ok(Some((written, Resolved::default())))
    }

    /// Writes what a plan asks for, in one transaction.
    pub(crate) fn apply_plan(
        &self,
        request: WriteRequest,
        plan: Plan,
    ) -> Result<(Written, Resolved), Error> {
        let written = self.write(request, move |write| {
            let resolved = plan.apply(write)?;
            // Plays follow their source items' new bindings.
            crate::history_store::rebind(write)?;
            Ok(serde_json::to_string(&resolved)?)
        })?;
        let resolved = serde_json::from_str(&written.result_json)?;
        Ok((written, resolved))
    }
}
