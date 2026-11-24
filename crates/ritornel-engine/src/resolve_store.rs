//! The resolver and the projector against `library.db`: reading the
//! library as the resolver sees it, and writing what its resolution asks
//! for. See "Recording resolution" and "Catalog projection and displayed
//! metadata" in docs/internals/identity.md.
//!
//! The resolution says what the catalog should be. [`Plan`] compares it
//! with what the rows hold and keeps only what differs, so a resolution
//! that changes nothing writes nothing, and takes no library revision.

use std::collections::{HashMap, HashSet};

use ritornel_domain::library::{
    ArtistKeyKind, Current, Evidence, ExistingArtistKey, ExistingCredit, ExistingGroup,
    ExistingRelease, Item, Kind, Library, Method, Ref, Resolution,
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
use crate::{Error, Id, LibraryStore, SubjectKind, WriteRequest, Written};

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
    artist_keys: HashMap<(ArtistKeyKind, String), (i64, Option<i64>)>,
    edited_artists: HashSet<i64>,
    stale_aliases: Vec<(i64, String, String, String, String)>,
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

/// The MusicBrainz ID each artist row owns in `artist_key`.
pub(crate) fn artist_mbids(connection: &Connection) -> Result<HashMap<i64, String>, Error> {
    let mut mbids = HashMap::new();
    for (artist, mbid) in rows(
        connection,
        "SELECT artist_id, key_value FROM artist_key WHERE key_kind = 'mbid' ORDER BY id",
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
    )? {
        mbids.entry(artist).or_insert(mbid);
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
    load_except(connection, &HashSet::new())
}

/// Reads the library as the resolver would see it with these answers
/// undone: their values, and what they put together, left out.
pub(crate) fn load_except(
    connection: &Connection,
    undone: &HashSet<i64>,
) -> Result<(Library, Shown), Error> {
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
            held: Vec::new(),
            current: current.get(&id).copied(),
            prior: None,
            prior_recording: None,
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
    for (id, canonical, recording, release, display) in rows(
        connection,
        "SELECT id, canonical_id, recording_id, release_id, display_title, display_artist_credit_id,
                display_disc_number, display_track_number, display_duration_ms, display_added_at_ms
         FROM track_appearance",
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<i64>>(3)?,
                (
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                ),
            ))
        },
    )? {
        if canonical != id {
            library.appearance_canonical.insert(id, canonical);
        }
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
    for (id, group, display) in rows(
        connection,
        "SELECT id, preferred_release_id, presentation IS NOT NULL, display_primary_type,
                display_secondary_types_json, display_first_date, display_first_year,
                display_release_id
         FROM release_group",
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                ExistingGroup {
                    preferred_release: row.get(1)?,
                    has_presentation: row.get(2)?,
                },
                (
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                ),
            ))
        },
    )? {
        library.groups.insert(id, group);
        shown.groups.insert(id, display);
    }
    // A row is found by its durable key in `artist_key`. When the library was
    // last resolved by another normaliser, stored name keys from that version
    // are migrated through `artist_keys_by` so an artist with no ID keeps its
    // row and public ID.
    library.artist_keys_by = connection
        .query_row(
            "SELECT json_extract(value_json, '$.normalizer') FROM library_setting WHERE key = ?1",
            [VERSIONS_SETTING],
            |row| row.get::<_, Option<i64>>(0),
        )
        .optional()?
        .flatten()
        .filter(|stored| *stored != NORMALIZER_VERSION);
    for (id, canonical, name, sort, match_name) in rows(
        connection,
        "SELECT id, canonical_id, display_name, display_sort_name, match_name
         FROM artist ORDER BY id",
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Text>(2)?,
                row.get::<_, Text>(3)?,
                row.get::<_, Text>(4)?,
            ))
        },
    )? {
        if canonical != id {
            library.artist_canonical.insert(id, canonical);
        }
        shown.artists.insert(id, (name, sort, match_name));
    }
    for (kind, value, artist, normalizer_version) in rows(
        connection,
        "SELECT key_kind, key_value, artist_id, normalizer_version
         FROM artist_key ORDER BY id",
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<i64>>(3)?,
            ))
        },
    )? {
        let kind = ArtistKeyKind::parse(&kind).ok_or(Error::Corrupt("an artist key's kind"))?;
        shown
            .artist_keys
            .insert((kind, value.clone()), (artist, normalizer_version));
        library.artist_keys.push(ExistingArtistKey {
            kind,
            value,
            artist,
            normalizer_version,
        });
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

    library.exclusions = rows(
        connection,
        "SELECT source_item_id, recording_id FROM identity_exclusion",
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?
    .into_iter()
    .collect();
    // Pairs the owner's own actions linked. What the resolver learned it
    // learns again, in the order it learned it. A pair written under an older
    // normaliser version is recomputed from its stored spellings (`spelling_a`,
    // `spelling_b`) and scheduled for upgrade to `NORMALIZER_VERSION`.
    let owner_aliases = rows(
        connection,
        "SELECT o.id, o.scope, o.name_a, o.name_b, o.spelling_a, o.spelling_b, o.normalizer_version
         FROM alias_observation o
         JOIN identity_decision d ON d.id = o.decision_id
         WHERE d.actor_kind <> 'resolver'
         ORDER BY o.id",
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, i64>(6)?,
            ))
        },
    )?;
    // Track only owner aliases already at `NORMALIZER_VERSION` so a resolver
    // observation never prevents an owner alias from upgrading in place. If
    // another owner row already holds the recomputed pair at the current
    // normaliser version, the alias still takes effect in memory without
    // creating a duplicate current owner row.
    let mut current_owner_aliases: HashSet<(String, String, String)> = owner_aliases
        .iter()
        .filter(|(_, _, _, _, _, _, ver)| *ver == NORMALIZER_VERSION)
        .map(|(_, scope, a, b, ..)| (scope.clone(), a.clone(), b.clone()))
        .collect();
    for (id, scope, a, b, sa, sb, ver) in owner_aliases {
        let kind = match scope.as_str() {
            "title" => Kind::Recording,
            "artist" => Kind::Artist,
            _ => continue,
        };
        if ver == NORMALIZER_VERSION {
            library.aliases.push((kind, a, b));
            continue;
        }
        let (Some(sa), Some(sb)) = (sa, sb) else {
            continue;
        };
        let (na, nb) = (normalize(&sa), normalize(&sb));
        if na.is_empty() || nb.is_empty() || na == nb {
            continue;
        }
        let (name_a, name_b, spelling_a, spelling_b) = if na < nb {
            (na, nb, sa, sb)
        } else {
            (nb, na, sb, sa)
        };
        library.aliases.push((kind, name_a.clone(), name_b.clone()));
        if current_owner_aliases.insert((scope, name_a.clone(), name_b.clone())) {
            shown
                .stale_aliases
                .push((id, name_a, name_b, spelling_a, spelling_b));
        }
    }
    // Active edits follow redirects through `canonical_id` where the kind has
    // one, ordered oldest-to-newest so the newest active edit across a merged
    // group wins while every row keeps the `entity_id` it originally named.
    for (kind, raw_entity, entity, field, value) in rows(
        connection,
        "SELECT m.entity_kind,
                m.entity_id,
                CASE m.entity_kind
                  WHEN 'recording'        THEN coalesce(rec.canonical_id, m.entity_id)
                  WHEN 'track_appearance' THEN coalesce(ta.canonical_id, m.entity_id)
                  WHEN 'release'          THEN coalesce(rel.canonical_id, m.entity_id)
                  WHEN 'artist'           THEN coalesce(art.canonical_id, m.entity_id)
                  ELSE m.entity_id
                END,
                m.field,
                m.value
         FROM metadata_edit m
         LEFT JOIN recording rec        ON m.entity_kind = 'recording'        AND rec.id = m.entity_id
         LEFT JOIN track_appearance ta  ON m.entity_kind = 'track_appearance' AND ta.id = m.entity_id
         LEFT JOIN release rel          ON m.entity_kind = 'release'          AND rel.id = m.entity_id
         LEFT JOIN artist art           ON m.entity_kind = 'artist'           AND art.id = m.entity_id
         WHERE m.withdrawn_at_ms IS NULL AND m.ordinal = 0
         ORDER BY m.id ASC",
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
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
        if kind == Kind::Artist && field == "name" {
            shown.edited_artists.insert(raw_entity);
        }
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
    // The tag values answers put on files, in place of their own.
    crate::answer_store::apply_in_force(connection, &mut library, undone)?;
    library.answer_redirects = crate::answer_store::answer_redirects(connection)?;
    library.answer_joins = crate::answer_store::joins_in_force(connection, undone)?;
    library.noes = crate::answer_store::noes_in_force(connection, undone)?;
    Ok((library, shown))
}

/// Resolves the library in the write's own transaction, with `adjust`
/// given the library first, and writes what differs; plays follow their
/// files. It says what was written, and whether anything shown changed.
pub(crate) fn resolve_in(
    write: &Write<'_>,
    adjust: impl FnOnce(&mut Library),
) -> Result<(Resolved, bool), Error> {
    let (mut library, mut shown) = load_in(write.connection())?;
    adjust(&mut library);
    shown.revision = write.rev() - 1;
    let policy = Policy::current();
    let resolution = ritornel_domain::resolve(&library, &policy);
    let plan = Plan::new(resolution, &library, &shown, policy.version);
    if plan.is_empty() {
        return Ok((Resolved::default(), false));
    }
    let shown = plan.changes_shown();
    let resolved = plan.apply(write)?;
    crate::history_store::catalog_changed(write)?;
    Ok((resolved, shown))
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
    /// Durable artist keys (`artist_key`) to register for resolved artists
    /// (index into `resolution.artists`, key kind, key value).
    artist_key_writes: Vec<(usize, ArtistKeyKind, String)>,
    /// Stale `artist_key` name rows (`(artist_id, old_key_value)`) to remove
    /// before inserting updated keys for the current normaliser version.
    stale_artist_key_deletes: Vec<(i64, String)>,
    /// Owner `alias_observation` rows recomputed from their raw spellings for
    /// the current normaliser version.
    rekeyed_aliases: Vec<(i64, String, String, String, String)>,
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
            let fold = crate::list::fold::fold;
            let text = (
                fold(out.title.as_deref().unwrap_or_default()),
                out.credit
                    .and_then(|at| resolution.credits.get(at))
                    .map(|credit| fold(&credit.text))
                    .unwrap_or_default(),
                out.release
                    .and_then(|at| resolution.releases.get(at))
                    .and_then(|release| release.title.as_deref())
                    .map(fold)
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
        // Durable `artist_key` writes for resolved artists, and removal of any
        // stale earlier-normaliser name keys on those rows.
        let mut artist_key_writes: Vec<(usize, ArtistKeyKind, String)> = Vec::new();
        let mut stale_artist_key_deletes: Vec<(i64, String)> = Vec::new();
        let mut reached_keyed_rows: HashSet<i64> = HashSet::new();
        for (idx, out) in resolution.artists.iter().enumerate() {
            let Some((kind, value)) = &out.key else {
                continue;
            };
            if value.is_empty() {
                continue;
            }
            let expected_ver = (*kind == ArtistKeyKind::Name).then_some(NORMALIZER_VERSION);
            let already = as_row(out.r).is_some_and(|row| {
                shown.artist_keys.get(&(*kind, value.clone())) == Some(&(row, expected_ver))
            });
            if !already {
                artist_key_writes.push((idx, *kind, value.clone()));
            }
            if let Ref::Row(row) = out.r {
                reached_keyed_rows.insert(row);
            }
        }
        for entry in &library.artist_keys {
            if entry.kind == ArtistKeyKind::Name
                && reached_keyed_rows.contains(&entry.artist)
                && entry.normalizer_version != Some(NORMALIZER_VERSION)
            {
                stale_artist_key_deletes.push((entry.artist, entry.value.clone()));
            }
        }

        // After the normaliser changed, a row with no ID that the
        // resolution does not reach with a key is keyed again from its name,
        // so its files find it when they return. It is given no key, and is
        // never found again, when that is not safe: its name is the owner's
        // edit, which its files never gave, or the key is one an artist of the
        // resolution holds, or another such row would take it too.
        let mut rekeyed: Vec<(i64, Option<String>)> = Vec::new();
        let has_stale_name_keys = library.artist_keys.iter().any(|entry| {
            entry.kind == ArtistKeyKind::Name
                && entry.normalizer_version != Some(NORMALIZER_VERSION)
        });
        if has_stale_name_keys {
            let mut held: HashSet<&str> = resolution
                .artists
                .iter()
                .filter(|out| out.key.is_some())
                .map(|out| out.match_name.as_str())
                .collect();
            for entry in &library.artist_keys {
                if entry.kind == ArtistKeyKind::Name
                    && entry.normalizer_version == Some(NORMALIZER_VERSION)
                {
                    held.insert(entry.value.as_str());
                }
            }
            // A name is trusted only when it gives back the key the row
            // holds: an edit, in force or withdrawn, gives another.
            let left: Vec<(i64, &String, Option<String>)> = library
                .artist_keys
                .iter()
                .filter(|entry| {
                    entry.kind == ArtistKeyKind::Name
                        && !reached_keyed_rows.contains(&entry.artist)
                        && entry.normalizer_version != Some(NORMALIZER_VERSION)
                })
                .map(|entry| {
                    let row = entry.artist;
                    let stored = &entry.value;
                    let version = entry.normalizer_version.or(library.artist_keys_by);
                    let edited = shown.edited_artists.contains(&row)
                        || library
                            .edits
                            .contains_key(&(Kind::Artist, row, "name".to_owned()));
                    let name = shown
                        .artists
                        .get(&row)
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
                    (row, stored, name.map(normalize))
                })
                .collect();
            let mut wanted: HashMap<&str, usize> = HashMap::new();
            for (_, _, key) in &left {
                if let Some(key) = key {
                    *wanted.entry(key.as_str()).or_default() += 1;
                }
            }
            for (row, stored, key) in &left {
                stale_artist_key_deletes.push((*row, (*stored).clone()));
                let key = key.as_ref().filter(|key| {
                    !key.is_empty()
                        && !held.contains(key.as_str())
                        && wanted.get(key.as_str()) == Some(&1)
                });
                rekeyed.push((*row, key.cloned()));
            }
        }
        let mut plan = Self {
            unsearchable,
            rekeyed,
            artist_key_writes,
            stale_artist_key_deletes,
            rekeyed_aliases: shown.stale_aliases.clone(),
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
            && self.artist_key_writes.is_empty()
            && self.stale_artist_key_deletes.is_empty()
            && self.rekeyed_aliases.is_empty()
    }

    /// Whether it changes anything shown: a file's rows, what a row shows
    /// or the search index. A version stamp or an artist key alone is not.
    pub(crate) fn changes_shown(&self) -> bool {
        let any = |flags: &[bool]| flags.iter().any(|flag| *flag);
        self.resolution.items.iter().any(|item| item.changed)
            || self
                .resolution
                .credits
                .iter()
                .any(|credit| as_row(credit.r).is_none())
            || any(&self.artists)
            || any(&self.recordings)
            || any(&self.groups)
            || any(&self.group_shows)
            || any(&self.releases)
            || any(&self.appearances)
            || self.search.iter().any(Option::is_some)
            || !self.unsearchable.is_empty()
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
        for (row, old_value) in &self.stale_artist_key_deletes {
            connection
                .prepare_cached(
                    "DELETE FROM artist_key
                     WHERE key_kind = 'name' AND artist_id = ?1 AND key_value = ?2",
                )?
                .execute((row, old_value))?;
        }
        for (idx, kind, value) in &self.artist_key_writes {
            let row = artist_rows.get(*idx).copied().ok_or_else(missing)?;
            let normalizer_version = (*kind == ArtistKeyKind::Name).then_some(NORMALIZER_VERSION);
            connection
                .prepare_cached(
                    "INSERT INTO artist_key
                       (key_kind, key_value, artist_id, normalizer_version, decision_id, rev)
                     VALUES (?1, ?2, ?3, ?4, NULL, ?5)",
                )?
                .execute((kind.as_str(), value, row, normalizer_version, rev))?;
        }
        for (row, key) in &self.rekeyed {
            connection
                .prepare_cached("UPDATE artist SET match_name = ?2, rev = ?3 WHERE id = ?1")?
                .execute((row, key, rev))?;
            if let Some(key) = key {
                connection
                    .prepare_cached(
                        "INSERT INTO artist_key
                           (key_kind, key_value, artist_id, normalizer_version, decision_id, rev)
                         VALUES ('name', ?1, ?2, ?3, NULL, ?4)",
                    )?
                    .execute((key, row, NORMALIZER_VERSION, rev))?;
            }
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

        for (id, name_a, name_b, spelling_a, spelling_b) in &self.rekeyed_aliases {
            connection
                .prepare_cached(
                    "UPDATE alias_observation
                     SET name_a = ?2, name_b = ?3, spelling_a = ?4, spelling_b = ?5,
                         normalizer_version = ?6, rev = ?7
                     WHERE id = ?1",
                )?
                .execute((
                    id,
                    name_a,
                    name_b,
                    spelling_a,
                    spelling_b,
                    NORMALIZER_VERSION,
                    rev,
                ))?;
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
            for alias in &decision.aliases {
                let scope = if alias.kind == Kind::Artist {
                    "artist"
                } else {
                    "title"
                };
                connection.prepare_cached(
                    "INSERT INTO alias_observation
                       (scope, name_a, name_b, spelling_a, spelling_b, normalizer_version,
                        decision_id, observed_at_ms, rev)
                     SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9
                     WHERE NOT EXISTS (SELECT 1 FROM alias_observation
                                       WHERE scope = ?1 AND name_a = ?2 AND name_b = ?3 AND normalizer_version = ?6)",
                )?
                .execute((
                    scope,
                    &alias.name_a,
                    &alias.name_b,
                    &alias.spelling_a,
                    &alias.spelling_b,
                    NORMALIZER_VERSION,
                    decided,
                    now_ms,
                    rev,
                ))?;
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

/// Removes the mark that a resolution is owed, in the write's transaction,
/// once one has been made.
pub(crate) fn settle_pending_in(write: &Write<'_>) -> Result<(), Error> {
    write.connection().execute(
        "DELETE FROM library_setting WHERE key = ?1",
        [PENDING_SETTING],
    )?;
    Ok(())
}

/// Whether anything is waiting on the resolver (see
/// [`LibraryStore::needs_resolving`]).
pub(crate) fn owes_resolution(connection: &Connection) -> Result<bool, Error> {
    let stored: Text = connection
        .query_row(
            "SELECT value_json FROM library_setting WHERE key = ?1",
            [VERSIONS_SETTING],
            |row| row.get(0),
        )
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
        self.read(owes_resolution)
    }

    /// Resolves the whole library by the policy scans use and writes what
    /// differs, in one transaction. It returns `None`, and writes nothing, when the
    /// resolution asks for no change.
    /// A plan that something else's write made stale is made again, a few
    /// times.
    pub fn resolve(&self, request: WriteRequest) -> Result<Option<(Written, Resolved)>, Error> {
        self.resolve_with(request, None)
    }

    pub(crate) fn resolve_with(
        &self,
        request: WriteRequest,
        cancel: Option<crate::JobCancel>,
    ) -> Result<Option<(Written, Resolved)>, Error> {
        let mut tries = 0;
        loop {
            if let Some(cancel) = &cancel {
                cancel.check()?;
            }
            let Some(plan) = self.plan()? else {
                return self.settle_pending(request, cancel);
            };
            match self.apply_plan_with(request, plan, cancel.clone()) {
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
    fn settle_pending(
        &self,
        request: WriteRequest,
        cancel: Option<crate::JobCancel>,
    ) -> Result<Option<(Written, Resolved)>, Error> {
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
        let written = self.write_guarded(request, cancel, |write| {
            settle_pending_in(write)?;
            Ok(serde_json::to_string(&Resolved::default())?)
        })?;
        Ok(Some((written, Resolved::default())))
    }

    /// Writes what a plan asks for, in one transaction.
    #[cfg(test)]
    pub(crate) fn apply_plan(
        &self,
        request: WriteRequest,
        plan: Plan,
    ) -> Result<(Written, Resolved), Error> {
        self.apply_plan_with(request, plan, None)
    }

    fn apply_plan_with(
        &self,
        request: WriteRequest,
        plan: Plan,
        cancel: Option<crate::JobCancel>,
    ) -> Result<(Written, Resolved), Error> {
        let written = self.write_guarded(request, cancel, move |write| {
            let resolved = plan.apply(write)?;
            // Plays follow their source items' new bindings, and what each
            // was heard as follows the catalog.
            crate::history_store::catalog_changed(write)?;
            Ok(serde_json::to_string(&resolved)?)
        })?;
        let resolved = serde_json::from_str(&written.result_json)?;
        Ok((written, resolved))
    }

    /// Clears every derived display column on live catalog rows and the search
    /// index, then re-projects the whole catalog from claims, edits, durable
    /// keys and redirects in one transaction. Rows reachable only through
    /// retired/unreadable files or history keep their last-known display.
    pub fn rebuild_display(&self, request: WriteRequest) -> Result<(Written, Resolved), Error> {
        let written = self.write(request, move |write| {
            let connection = write.connection();
            let rev = write.rev();
            let policy = Policy::current();

            // Migrate any stale earlier-normaliser artist keys BEFORE wiping
            // display columns, so unreached rows still have their stored
            // display_name for safe rekeying.
            {
                let (library, mut shown) = load_in(connection)?;
                let has_stale = library.artist_keys_by.is_some()
                    || library.artist_keys.iter().any(|entry| {
                        entry.kind == ArtistKeyKind::Name
                            && entry.normalizer_version != Some(NORMALIZER_VERSION)
                    });
                if has_stale {
                    shown.revision = rev - 1;
                    let resolution = ritornel_domain::resolve(&library, &policy);
                    let plan = Plan::new(resolution, &library, &shown, policy.version);
                    if !plan.is_empty() {
                        plan.apply(write)?;
                    }
                }
            }

            // Clear derived display columns only on entities reached by live,
            // readable files (including canonical merge targets of those
            // artists and releases), leaving history-only and retired-file
            // rows intact.
            let live_apps = "SELECT a.appearance_id
                FROM assignment a
                JOIN source_item si ON si.id = a.source_item_id
                JOIN source_item_revision sr ON sr.id = si.current_revision_id
                WHERE a.superseded_at_ms IS NULL
                  AND si.item_kind = 'file'
                  AND si.retired_at_ms IS NULL
                  AND sr.audio_asset_id IS NOT NULL
                  AND a.appearance_id IS NOT NULL";
            let live_rels = format!(
                "SELECT ta.release_id FROM track_appearance ta
                 WHERE ta.id IN ({live_apps}) AND ta.release_id IS NOT NULL
                 UNION
                 SELECT rel.canonical_id FROM track_appearance ta
                 JOIN release rel ON rel.id = ta.release_id
                 WHERE ta.id IN ({live_apps})"
            );
            connection.execute(
                &format!(
                    "UPDATE artist
                     SET display_name = NULL, display_sort_name = NULL, match_name = NULL, rev = ?1
                     WHERE id IN (
                       SELECT acn.artist_id FROM artist_credit_name acn
                       WHERE acn.artist_credit_id IN (
                         SELECT ta.display_artist_credit_id FROM track_appearance ta WHERE ta.id IN ({live_apps})
                         UNION
                         SELECT rel.display_artist_credit_id FROM release rel WHERE rel.id IN ({live_rels})
                       )
                       UNION
                       SELECT a.canonical_id FROM artist a
                       JOIN artist_credit_name acn ON acn.artist_id = a.id
                       WHERE acn.artist_credit_id IN (
                         SELECT ta.display_artist_credit_id FROM track_appearance ta WHERE ta.id IN ({live_apps})
                         UNION
                         SELECT rel.display_artist_credit_id FROM release rel WHERE rel.id IN ({live_rels})
                       )
                     )"
                ),
                [rev],
            )?;
            connection.execute(
                "UPDATE recording
                 SET display_title = NULL, display_artist_credit_id = NULL, rev = ?1
                 WHERE EXISTS (
                   SELECT 1 FROM assignment a
                   JOIN source_item si ON si.id = a.source_item_id
                   JOIN source_item_revision sr ON sr.id = si.current_revision_id
                   JOIN recording r ON r.id = a.recording_id
                   WHERE a.superseded_at_ms IS NULL
                     AND si.item_kind = 'file'
                     AND si.retired_at_ms IS NULL
                     AND sr.audio_asset_id IS NOT NULL
                     AND r.canonical_id = recording.id
                 )",
                [rev],
            )?;
            connection.execute(
                &format!(
                    "UPDATE release_group
                     SET display_primary_type = NULL, display_secondary_types_json = '[]',
                         display_first_date = NULL, display_first_year = NULL,
                         display_release_id = NULL, rev = ?1
                     WHERE id IN (
                       SELECT release_group_id FROM release WHERE id IN ({live_rels})
                     )"
                ),
                [rev],
            )?;
            connection.execute(
                &format!(
                    "UPDATE release
                     SET display_title = NULL, display_artist_credit_id = NULL,
                         display_date = NULL, display_year = NULL,
                         display_picture_id = NULL, rev = ?1
                     WHERE id IN ({live_rels})"
                ),
                [rev],
            )?;
            connection.execute(
                &format!(
                    "UPDATE track_appearance
                     SET display_title = NULL, display_artist_credit_id = NULL,
                         display_disc_number = NULL, display_track_number = NULL,
                         display_duration_ms = NULL, display_added_at_ms = NULL, rev = ?1
                     WHERE id IN ({live_apps})"
                ),
                [rev],
            )?;
            connection.execute("DELETE FROM search_text", [])?;

            let (library, mut shown) = load_in(connection)?;
            shown.revision = rev - 1;
            let resolution = ritornel_domain::resolve(&library, &policy);
            let plan = Plan::new(resolution, &library, &shown, policy.version);
            let resolved = plan.apply(write)?;
            crate::history_store::catalog_changed(write)?;
            Ok(serde_json::to_string(&resolved)?)
        })?;
        let resolved = serde_json::from_str(&written.result_json)?;
        Ok((written, resolved))
    }

    /// Records a metadata edit on the exact entity `entity` named (keeping its
    /// own `id` even if it currently redirects to another row), withdraws any
    /// prior active edit on that same named `(kind, entity, field)`, and
    /// re-projects the catalog in the same transaction.
    ///
    /// Supported `(kind, field)` pairs:
    /// - `Artist`: `"name"`
    /// - `Recording`: `"title"`
    /// - `TrackAppearance`: `"title"`
    /// - `Release`: `"album"`, `"date"` (`YYYY`, `YYYY-MM`, or `YYYY-MM-DD`)
    /// - `ReleaseGroup`: `"primary_type"` (`album`, `single`, `ep`, `broadcast`, `other`),
    ///   `"original_date"` (`YYYY`, `YYYY-MM`, or `YYYY-MM-DD`)
    pub fn set_metadata_edit(
        &self,
        request: WriteRequest,
        kind: Kind,
        entity: Id,
        field: &str,
        value: &str,
    ) -> Result<Written, Error> {
        if value.is_empty() {
            return Err(Error::Invalid("an edit value may not be empty"));
        }
        match (kind, field) {
            (Kind::Artist, "name")
            | (Kind::Recording, "title")
            | (Kind::TrackAppearance, "title")
            | (Kind::Release, "album") => {}
            (Kind::Release, "date") | (Kind::ReleaseGroup, "original_date") => {
                if ritornel_domain::extract::parse_date(value).is_none() {
                    return Err(Error::Invalid(
                        "a date edit must be YYYY, YYYY-MM, or YYYY-MM-DD",
                    ));
                }
            }
            (Kind::ReleaseGroup, "primary_type") => {
                if !matches!(value, "album" | "single" | "ep" | "broadcast" | "other") {
                    return Err(Error::Invalid("unknown release-group primary_type"));
                }
            }
            _ => {
                return Err(Error::Invalid(
                    "unsupported metadata edit field for entity kind",
                ));
            }
        }
        let field = field.to_owned();
        let value = value.to_owned();
        let actor = match request
            .client
            .map_or(SubjectKind::Person, |client| client.kind)
        {
            SubjectKind::Agent => "agent",
            SubjectKind::Person | SubjectKind::Server => "person",
        };
        let session = request.client.map(|client| client.session);
        self.write(request, move |write| {
            let connection = write.connection();
            let (kind_str, table) = match kind {
                Kind::Recording => ("recording", "recording"),
                Kind::TrackAppearance => ("track_appearance", "track_appearance"),
                Kind::Release => ("release", "release"),
                Kind::ReleaseGroup => ("release_group", "release_group"),
                Kind::Artist => ("artist", "artist"),
            };
            // Store the named row's own ID, never its canonical target, so
            // undoing a merge leaves the edit on the entity that was edited.
            let entity_id: Option<i64> = connection
                .query_row(
                    &format!("SELECT id FROM {table} WHERE public_id = ?1"),
                    [entity.as_bytes()],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(entity_id) = entity_id else {
                return Err(Error::NotFound("no such entity"));
            };
            let now_ms = write.now_ms();
            let rev = write.rev();
            connection.execute(
                "UPDATE metadata_edit
                 SET withdrawn_at_ms = ?4, rev = ?5
                 WHERE entity_kind = ?1 AND entity_id = ?2 AND field = ?3
                   AND ordinal = 0 AND withdrawn_at_ms IS NULL",
                (kind_str, entity_id, &field, now_ms, rev),
            )?;
            connection.execute(
                "INSERT INTO metadata_edit
                   (entity_kind, entity_id, field, value, ordinal, actor_kind,
                    actor_session_pid, edited_at_ms, rev)
                 VALUES (?1, ?2, ?3, ?4, 0, ?5, ?6, ?7, ?8)",
                rusqlite::params![
                    kind_str,
                    entity_id,
                    &field,
                    &value,
                    actor,
                    session.as_ref().map(Id::as_bytes),
                    now_ms,
                    rev,
                ],
            )?;
            let (library, mut shown) = load_in(connection)?;
            shown.revision = rev - 1;
            let policy = Policy::current();
            let resolution = ritornel_domain::resolve(&library, &policy);
            let plan = Plan::new(resolution, &library, &shown, policy.version);
            if !plan.is_empty() {
                plan.apply(write)?;
                crate::history_store::catalog_changed(write)?;
            }
            Ok(serde_json::to_string(&rev)?)
        })
    }

    /// Withdraws the active metadata edit on the exact entity `entity` named
    /// and re-projects the catalog in the same transaction. Returns `Ok(None)`
    /// without taking a revision when that named row has no active edit on
    /// `field`.
    ///
    /// Withdrawing affects only the named row: if another row in the same
    /// merged group still carries an active edit on `field`, that edit shows
    /// through until it is also withdrawn or the merge is undone.
    pub fn withdraw_metadata_edit(
        &self,
        request: WriteRequest,
        kind: Kind,
        entity: Id,
        field: &str,
    ) -> Result<Option<Written>, Error> {
        if field.is_empty() {
            return Err(Error::Invalid("an edit must name a field"));
        }
        let field = field.to_owned();
        let (kind_str, table) = match kind {
            Kind::Recording => ("recording", "recording"),
            Kind::TrackAppearance => ("track_appearance", "track_appearance"),
            Kind::Release => ("release", "release"),
            Kind::ReleaseGroup => ("release_group", "release_group"),
            Kind::Artist => ("artist", "artist"),
        };
        let check_field = field.clone();
        let has_active: bool = self.read(move |connection| {
            let entity_id: Option<i64> = connection
                .query_row(
                    &format!("SELECT id FROM {table} WHERE public_id = ?1"),
                    [entity.as_bytes()],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(entity_id) = entity_id else {
                return Err(Error::NotFound("no such entity"));
            };
            Ok(connection.query_row(
                "SELECT EXISTS (
                   SELECT 1 FROM metadata_edit
                   WHERE entity_kind = ?1 AND entity_id = ?2 AND field = ?3
                     AND ordinal = 0 AND withdrawn_at_ms IS NULL
                 )",
                (kind_str, entity_id, &check_field),
                |row| row.get(0),
            )?)
        })?;
        if !has_active {
            return Ok(None);
        }
        let written = self.write(request, move |write| {
            let connection = write.connection();
            let entity_id: Option<i64> = connection
                .query_row(
                    &format!("SELECT id FROM {table} WHERE public_id = ?1"),
                    [entity.as_bytes()],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(entity_id) = entity_id else {
                return Err(Error::NotFound("no such entity"));
            };
            let rev = write.rev();
            connection.execute(
                "UPDATE metadata_edit
                 SET withdrawn_at_ms = ?4, rev = ?5
                 WHERE entity_kind = ?1 AND entity_id = ?2 AND field = ?3
                   AND ordinal = 0 AND withdrawn_at_ms IS NULL",
                (kind_str, entity_id, &field, write.now_ms(), rev),
            )?;
            let (library, mut shown) = load_in(connection)?;
            shown.revision = rev - 1;
            let policy = Policy::current();
            let resolution = ritornel_domain::resolve(&library, &policy);
            let plan = Plan::new(resolution, &library, &shown, policy.version);
            if !plan.is_empty() {
                plan.apply(write)?;
                crate::history_store::catalog_changed(write)?;
            }
            Ok(serde_json::to_string(&rev)?)
        })?;
        Ok(Some(written))
    }
}
