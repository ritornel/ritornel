//! `identity.dryRun` and `identity.review` against `library.db`. See "The
//! proof" and "Resolver policy 1" in docs/design/identity.md.
//!
//! The dry run reads the library as the resolver sees it, with the claims
//! that the stored revisions gave, runs the pure resolver over it by the
//! policy scans use, by another named policy or by other values, and
//! compares each result with the assignment in force. It reads on the
//! read-only connection and writes nothing.
//!
//! The review listing reads the decisions in force that are marked for
//! review. A decision's stored evidence names the candidate it nearly
//! matched and the distance, and a match held back from binding with its
//! cause and recording; what made the distance up is not stored, so the
//! listing asks the resolver again, by the policy scans use, and takes the
//! penalties from its answer.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use ritornel_contract::{
    Binding, Count, DryRun, DryRunParams, FileFacts, Id, ItemChange, ItemOutcome, Nearest,
    Penalties as WirePenalties, PolicyOverrides, PolicyValues, Review, ReviewCause, ReviewList,
};
use ritornel_domain::library::{Candidate, Item, ItemOut, Library, Penalties, Ref, Resolution};
use ritornel_domain::{EXTRACTOR_VERSION, Policy};
use rusqlite::Connection;
use serde::Deserialize;

use crate::resolve_store::{Plan, Shown, load_in, rows};
use crate::{Error, LibraryStore};

/// A decision in force, as stored.
struct Stored {
    /// The version of the policy that made it.
    resolver_version: i64,
    score: Option<f64>,
    needs_review: bool,
    runner_up_gap: Option<f64>,
    evidence: String,
}

/// What the identity calls read beside the library as the resolver sees it.
struct Rows {
    revision: u64,
    claims_stale: bool,
    /// Each file's public ID and path under its root.
    items: HashMap<i64, (Id, String)>,
    recordings: HashMap<i64, Id>,
    /// Each release's public ID and title.
    releases: HashMap<i64, (Id, Option<String>)>,
    /// Each item's decision in force.
    decisions: HashMap<i64, Stored>,
}

/// A stored decision's evidence, as far as these calls read it.
#[derive(Debug, Default, Deserialize)]
struct StoredEvidence {
    #[serde(default)]
    candidates: Vec<StoredCandidate>,
    #[serde(default)]
    contradictions: Vec<String>,
    #[serde(default)]
    withheld: Option<StoredWithheld>,
}

#[derive(Debug, Deserialize)]
struct StoredCandidate {
    item: i64,
    distance: f64,
}

/// A match the resolver held back from binding.
#[derive(Debug, Deserialize)]
struct StoredWithheld {
    cause: String,
    recording: Option<i64>,
}

fn unsigned(number: i64) -> Option<u64> {
    u64::try_from(number).ok()
}

fn rows_in(connection: &Connection) -> Result<Rows, Error> {
    let revision: i64 =
        connection.query_row("SELECT rev FROM library_meta WHERE id = 1", [], |row| {
            row.get(0)
        })?;
    let claims_stale = connection.query_row(
        "SELECT EXISTS (SELECT 1 FROM metadata_claim WHERE extractor_version <> ?1)
             OR EXISTS (SELECT 1 FROM identifier_claim WHERE extractor_version <> ?1)",
        [EXTRACTOR_VERSION],
        |row| row.get(0),
    )?;
    let items = rows(
        connection,
        "SELECT id, public_id, provider_key FROM source_item WHERE item_kind = 'file'",
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                (Id::from_bytes(row.get(1)?), row.get::<_, String>(2)?),
            ))
        },
    )?;
    let recordings = rows(connection, "SELECT id, public_id FROM recording", |row| {
        Ok((row.get::<_, i64>(0)?, Id::from_bytes(row.get(1)?)))
    })?;
    let releases = rows(
        connection,
        "SELECT id, public_id, display_title FROM release",
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                (Id::from_bytes(row.get(1)?), row.get(2)?),
            ))
        },
    )?;
    let decisions = rows(
        connection,
        "SELECT a.source_item_id, d.score, d.needs_review, d.runner_up_gap, d.evidence_json,
                d.resolver_version
         FROM assignment a JOIN identity_decision d ON d.id = a.decision_id
         WHERE a.superseded_at_ms IS NULL",
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                Stored {
                    score: row.get(1)?,
                    needs_review: row.get(2)?,
                    runner_up_gap: row.get(3)?,
                    evidence: row.get(4)?,
                    resolver_version: row.get(5)?,
                },
            ))
        },
    )?;
    Ok(Rows {
        revision: unsigned(revision).unwrap_or(0),
        claims_stale,
        items: items.into_iter().collect(),
        recordings: recordings.into_iter().collect(),
        releases: releases.into_iter().collect(),
        decisions: decisions.into_iter().collect(),
    })
}

/// The library as the resolver sees it, and what these calls read beside
/// it, in one read transaction.
fn read(connection: &Connection) -> Result<(Library, Shown, Rows), Error> {
    let transaction = connection.unchecked_transaction()?;
    let (library, shown) = load_in(&transaction)?;
    let rows = rows_in(&transaction)?;
    Ok((library, shown, rows))
}

/// Whether a policy of this number exists for a dry run to name.
pub fn is_policy(version: u64) -> bool {
    i64::try_from(version)
        .ok()
        .and_then(Policy::of_version)
        .is_some()
}

/// The named policy `overrides` asks for, or the one scans use, with its
/// values in place of the policy's own. A version no policy has is the
/// call's to refuse; here it is taken as the one scans use.
fn policy_with(overrides: &PolicyOverrides) -> Policy {
    let base = overrides
        .version
        .and_then(|version| i64::try_from(version).ok())
        .and_then(Policy::of_version)
        .unwrap_or_else(Policy::current);
    Policy {
        bind_max: overrides.bind_max.unwrap_or(base.bind_max),
        min_gap: overrides.min_gap.unwrap_or(base.min_gap),
        review_below: overrides.review_below.unwrap_or(base.review_below),
        duration_grace_ms: overrides
            .duration_grace_ms
            .map_or(base.duration_grace_ms, |ms| {
                i64::try_from(ms).unwrap_or(i64::MAX)
            }),
        fold_accents: overrides.fold_accents.unwrap_or(base.fold_accents),
        discs_from_folders: overrides
            .discs_from_folders
            .unwrap_or(base.discs_from_folders),
        disc_from_album_title: overrides
            .disc_from_album_title
            .unwrap_or(base.disc_from_album_title),
        tags_bind_across_albums: overrides
            .tags_bind_across_albums
            .unwrap_or(base.tags_bind_across_albums),
        across_albums_within_ms: overrides
            .across_albums_within_ms
            .map_or(base.across_albums_within_ms, |ms| {
                i64::try_from(ms).unwrap_or(i64::MAX)
            }),
        tracks_of_one_release_differ: overrides
            .tracks_of_one_release_differ
            .unwrap_or(base.tracks_of_one_release_differ),
        marker_list: overrides
            .marker_list
            .and_then(|list| u8::try_from(list).ok())
            .unwrap_or(base.marker_list),
        ..base.clone()
    }
}

fn values(policy: &Policy) -> PolicyValues {
    PolicyValues {
        resolver_version: unsigned(policy.version).unwrap_or(0),
        exact: Policy::of_version(policy.version).as_ref() == Some(policy),
        bind_max: policy.bind_max,
        min_gap: policy.min_gap,
        review_below: policy.review_below,
        duration_grace_ms: unsigned(policy.duration_grace_ms).unwrap_or(0),
        fold_accents: policy.fold_accents,
        discs_from_folders: policy.discs_from_folders,
        disc_from_album_title: policy.disc_from_album_title,
        tags_bind_across_albums: policy.tags_bind_across_albums,
        across_albums_within_ms: unsigned(policy.across_albums_within_ms).unwrap_or(0),
        tracks_of_one_release_differ: policy.tracks_of_one_release_differ,
        marker_list: u64::from(policy.marker_list),
    }
}

fn wire(penalties: &Penalties) -> WirePenalties {
    WirePenalties {
        title: penalties.title,
        artist: penalties.artist,
        duration: penalties.duration,
        album: penalties.album,
        shared_identifier: penalties.shared_identifier,
    }
}

/// Looks up what the comparison needs about the library.
struct Lookup<'a> {
    library: &'a Library,
    rows: &'a Rows,
    index: HashMap<i64, usize>,
}

impl<'a> Lookup<'a> {
    fn new(library: &'a Library, rows: &'a Rows) -> Self {
        let index = library
            .items
            .iter()
            .enumerate()
            .map(|(at, item)| (item.id, at))
            .collect();
        Self {
            library,
            rows,
            index,
        }
    }

    fn recording(&self, row: i64) -> i64 {
        self.library
            .recording_canonical
            .get(&row)
            .copied()
            .unwrap_or(row)
    }

    fn release(&self, row: i64) -> i64 {
        self.library
            .releases
            .get(&row)
            .map_or(row, |release| release.canonical)
    }

    /// The release an item's appearance in force is on.
    fn release_now(&self, item: &Item) -> Option<i64> {
        let appearance = item.current?.appearance?;
        let release = self.library.appearances.get(&appearance)?.1?;
        Some(self.release(release))
    }

    fn file(&self, item: &Item) -> Result<FileFacts, Error> {
        let (item_id, path) = self
            .rows
            .items
            .get(&item.id)
            .ok_or(Error::Corrupt("an item with no row"))?;
        let facts = &item.facts;
        Ok(FileFacts {
            item_id: *item_id,
            path: path.clone(),
            title: facts.title.clone().or_else(|| facts.title_inferred.clone()),
            artist: facts.artist_text(),
            album: facts.album.clone(),
            disc: facts.disc.and_then(unsigned),
            track: facts.track.and_then(unsigned),
            duration_ms: facts.duration_ms.and_then(unsigned),
        })
    }

    fn file_of(&self, id: i64) -> Result<Option<FileFacts>, Error> {
        match self
            .index
            .get(&id)
            .and_then(|at| self.library.items.get(*at))
        {
            Some(item) => self.file(item).map(Some),
            None => Ok(None),
        }
    }

    /// The public ID of a recording row, after redirects.
    fn recording_id(&self, row: i64) -> Option<Id> {
        self.rows.recordings.get(&self.recording(row)).copied()
    }

    /// The candidate an item was nearest to: the file, the distance and
    /// what made it up, and the recording it nearly matched. That is the
    /// one a held-back match names, where there is one (`Some(None)` for a
    /// recording that does not exist yet), or else the one the file is
    /// bound to now.
    fn nearest(
        &self,
        item: i64,
        distance: f64,
        penalties: Option<&Penalties>,
        held: Option<Option<i64>>,
    ) -> Result<Option<Nearest>, Error> {
        let recording = held.unwrap_or_else(|| {
            self.index
                .get(&item)
                .and_then(|at| self.library.items.get(*at))
                .and_then(|item| item.current)
                .map(|current| current.recording)
        });
        Ok(self.file_of(item)?.map(|file| Nearest {
            file,
            distance,
            penalties: penalties.map(wire),
            recording_id: recording.and_then(|row| self.recording_id(row)),
        }))
    }

    /// What an item is bound to now.
    fn now(&self, item: &Item) -> Result<Option<Binding>, Error> {
        let Some(current) = item.current else {
            return Ok(None);
        };
        let stored = self.rows.decisions.get(&item.id);
        let evidence: StoredEvidence = match stored {
            Some(stored) => serde_json::from_str(&stored.evidence)?,
            None => StoredEvidence::default(),
        };
        let held = evidence
            .withheld
            .as_ref()
            .map(|withheld| withheld.recording);
        let nearest = match evidence.candidates.first() {
            Some(candidate) => self.nearest(candidate.item, candidate.distance, None, held)?,
            None => None,
        };
        let release = self.release_now(item);
        let shown = release.and_then(|row| self.rows.releases.get(&row));
        Ok(Some(Binding {
            recording_id: self
                .rows
                .recordings
                .get(&self.recording(current.recording))
                .copied(),
            method: Some(current.method.as_str().to_owned()),
            score: stored.and_then(|stored| stored.score),
            needs_review: stored.is_some_and(|stored| stored.needs_review),
            release_id: shown.map(|(id, _)| *id),
            release_title: shown.and_then(|(_, title)| title.clone()),
            disc: item.facts.disc.and_then(unsigned),
            nearest,
        }))
    }
}

/// Where each appearance of a resolution sits: its release and disc.
type Placed = HashMap<Ref, (Option<Ref>, Option<i64>)>;

fn placed(resolution: &Resolution) -> Placed {
    resolution
        .appearances
        .iter()
        .map(|appearance| {
            let release = appearance
                .release
                .and_then(|at| resolution.releases.get(at))
                .map(|release| release.r);
            (appearance.r, (release, appearance.disc))
        })
        .collect()
}

/// The release a result puts an item on.
fn release_after(lookup: &Lookup<'_>, placed: &Placed, out: &ItemOut) -> Option<Ref> {
    let appearance = out.appearance?;
    match placed.get(&appearance) {
        Some((release, _)) => *release,
        // Held where it is.
        None => match appearance {
            Ref::Row(row) => {
                let release = lookup.library.appearances.get(&row)?.1?;
                Some(Ref::Row(lookup.release(release)))
            }
            Ref::New(_) => None,
        },
    }
}

/// What a result binds an item to.
fn after(
    lookup: &Lookup<'_>,
    resolution: &Resolution,
    placed: &Placed,
    item: &Item,
    out: &ItemOut,
) -> Result<Option<Binding>, Error> {
    let Some(recording) = out.recording else {
        return Ok(None);
    };
    let release = release_after(lookup, placed, out);
    // The title is the one the run would show, which for a release that is
    // there may differ from the one it shows now.
    let projected = release.and_then(|release| {
        resolution
            .releases
            .iter()
            .find(|out| out.r == release)
            .map(|out| out.title.clone())
    });
    let (release_id, release_title) = match release {
        Some(Ref::Row(row)) => match lookup.rows.releases.get(&row) {
            Some((id, title)) => (Some(*id), projected.unwrap_or_else(|| title.clone())),
            None => (None, projected.flatten()),
        },
        Some(Ref::New(_)) => (None, projected.flatten()),
        None => (None, None),
    };
    let decision = out.decision.as_ref();
    let nearest = match decision.and_then(|decision| decision.evidence.candidates.first()) {
        Some(Candidate {
            item,
            distance,
            penalties,
        }) => {
            let held = decision
                .and_then(|decision| decision.evidence.withheld)
                .map(|withheld| match withheld.recording {
                    Some(Ref::Row(row)) => Some(row),
                    _ => None,
                });
            lookup.nearest(*item, *distance, penalties.as_ref(), held)?
        }
        None => None,
    };
    let disc = match out
        .appearance
        .and_then(|appearance| placed.get(&appearance))
    {
        Some((_, disc)) => *disc,
        None => item.facts.disc,
    };
    Ok(Some(Binding {
        recording_id: match recording {
            Ref::Row(row) => lookup.rows.recordings.get(&lookup.recording(row)).copied(),
            Ref::New(_) => None,
        },
        method: decision
            .map(|decision| decision.method.as_str().to_owned())
            .or_else(|| {
                item.current
                    .map(|current| current.method.as_str().to_owned())
            }),
        score: decision.map(|decision| decision.score),
        needs_review: match decision {
            Some(decision) => decision.needs_review,
            None => lookup
                .rows
                .decisions
                .get(&item.id)
                .is_some_and(|stored| stored.needs_review),
        },
        release_id,
        release_title,
        disc: disc.and_then(unsigned),
        nearest,
    }))
}

/// Compares a resolution with the assignments in force.
fn compare(
    library: &Library,
    resolution: &Resolution,
    rows: &Rows,
    policy: &Policy,
    would_write: bool,
    detail: bool,
) -> Result<DryRun, Error> {
    let lookup = Lookup::new(library, rows);
    let placed = placed(resolution);
    let mut run = DryRun {
        policy: values(policy),
        library_revision: rows.revision,
        claims_stale: rows.claims_stale,
        would_write,
        items: library.items.len() as u64,
        unchanged: 0,
        would_change: 0,
        recordings_changed: 0,
        pinned: 0,
        blocked: 0,
        merges: 0,
        splits: 0,
        reviews_now: 0,
        reviews_after: 0,
        reviews_cleared: 0,
        reviews_added: 0,
        releases_now: 0,
        releases_after: 0,
        changes: Vec::new(),
    };
    let mut joined: HashMap<Ref, BTreeSet<i64>> = HashMap::new();
    let mut parted: BTreeMap<i64, HashSet<Ref>> = BTreeMap::new();
    let (mut releases_now, mut releases_after) = (HashSet::new(), HashSet::new());
    for (item, out) in library.items.iter().zip(&resolution.items) {
        let outcome = if out.pinned {
            ItemOutcome::Pinned
        } else if out.changed {
            ItemOutcome::WouldChange
        } else if out.blocked {
            ItemOutcome::Blocked
        } else {
            ItemOutcome::Unchanged
        };
        let now = item
            .current
            .map(|current| lookup.recording(current.recording));
        let next = out.recording.map(|recording| match recording {
            Ref::Row(row) => Ref::Row(lookup.recording(row)),
            Ref::New(at) => Ref::New(at),
        });
        match outcome {
            ItemOutcome::Unchanged => run.unchanged += 1,
            ItemOutcome::WouldChange => {
                run.would_change += 1;
                if next != now.map(Ref::Row) {
                    run.recordings_changed += 1;
                }
            }
            ItemOutcome::Pinned => run.pinned += 1,
            ItemOutcome::Blocked => run.blocked += 1,
        }
        if item.retired {
            continue;
        }
        let reviewed_now = item.current.is_some()
            && rows
                .decisions
                .get(&item.id)
                .is_some_and(|stored| stored.needs_review);
        // An item the resolver decided nothing about keeps what it has.
        let reviewed_after = out
            .decision
            .as_ref()
            .map_or(reviewed_now, |decision| decision.needs_review);
        run.reviews_now += u64::from(reviewed_now);
        run.reviews_after += u64::from(reviewed_after);
        run.reviews_cleared += u64::from(reviewed_now && !reviewed_after);
        run.reviews_added += u64::from(reviewed_after && !reviewed_now);
        if let (Some(now), Some(next)) = (now, next) {
            joined.entry(next).or_default().insert(now);
            parted.entry(now).or_default().insert(next);
        }
        releases_now.extend(lookup.release_now(item));
        releases_after.extend(release_after(&lookup, &placed, out));
        if detail && outcome != ItemOutcome::Unchanged {
            run.changes.push(ItemChange {
                outcome,
                file: lookup.file(item)?,
                now: lookup.now(item)?,
                after: after(&lookup, resolution, &placed, item, out)?,
            });
        }
    }
    let less_one = |count: usize| count.saturating_sub(1) as u64;
    run.merges = joined.values().map(|now| less_one(now.len())).sum();
    run.splits = parted.values().map(|next| less_one(next.len())).sum();
    run.releases_now = releases_now.len() as u64;
    run.releases_after = releases_after.len() as u64;
    Ok(run)
}

/// Why a decision was marked for review: the cause its evidence names for
/// a match held back, or else what made up the distance to the candidate
/// it nearly matched.
fn cause(
    policy: &Policy,
    evidence: &StoredEvidence,
    distance: Option<f64>,
    penalties: Option<&Penalties>,
) -> ReviewCause {
    if !evidence.contradictions.is_empty() {
        return ReviewCause::Contradiction;
    }
    match evidence
        .withheld
        .as_ref()
        .map(|withheld| withheld.cause.as_str())
    {
        Some("other_album") => return ReviewCause::OtherAlbum,
        Some("same_release") => return ReviewCause::SameRelease,
        _ => {}
    }
    let Some(distance) = distance else {
        return ReviewCause::Other;
    };
    if distance <= policy.bind_max {
        return ReviewCause::CloseRunnerUp;
    }
    match penalties {
        Some(penalties) if penalties.artist > 0.0 => ReviewCause::ArtistSpelling,
        Some(penalties) if penalties.title > 0.0 => ReviewCause::TitleNote,
        Some(penalties) if penalties.duration > 0.0 && penalties.album > 0.0 => {
            ReviewCause::LengthOtherAlbum
        }
        Some(penalties) if penalties.duration > 0.0 => ReviewCause::LengthSameAlbum,
        _ => ReviewCause::Other,
    }
}

fn review(library: &Library, rows: &Rows) -> Result<ReviewList, Error> {
    // Each decision is read by the policy that made it, which a library
    // resolved before the policy changed still has for most of its items.
    let mut fresh: BTreeMap<i64, Resolution> = BTreeMap::new();
    let lookup = Lookup::new(library, rows);
    let mut reviews = Vec::new();
    for (index, item) in library.items.iter().enumerate() {
        let Some(stored) = rows.decisions.get(&item.id) else {
            continue;
        };
        if item.retired || item.current.is_none() || !stored.needs_review {
            continue;
        }
        let policy = Policy::of_version(stored.resolver_version).unwrap_or_else(Policy::current);
        let out = fresh
            .entry(policy.version)
            .or_insert_with(|| ritornel_domain::resolve(library, &policy))
            .items
            .get(index);
        let evidence: StoredEvidence = serde_json::from_str(&stored.evidence)?;
        let first = evidence.candidates.first();
        // The penalties the resolver gives the same candidate now.
        let penalties = first.and_then(|first| {
            out?.decision
                .as_ref()?
                .evidence
                .candidates
                .iter()
                .find(|candidate| candidate.item == first.item)?
                .penalties
        });
        let held = evidence
            .withheld
            .as_ref()
            .map(|withheld| withheld.recording);
        let nearest = match first {
            Some(first) => lookup.nearest(first.item, first.distance, penalties.as_ref(), held)?,
            None => None,
        };
        reviews.push(Review {
            cause: cause(
                &policy,
                &evidence,
                first.map(|first| first.distance),
                penalties.as_ref(),
            ),
            file: lookup.file(item)?,
            nearest,
            runner_up_gap: stored.runner_up_gap,
            candidates: evidence.candidates.len() as u64,
            contradictions: evidence.contradictions,
        });
    }
    reviews.sort_by(|a, b| {
        a.cause
            .cmp(&b.cause)
            .then_with(|| a.file.path.cmp(&b.file.path))
    });
    let mut causes: Vec<Count> = Vec::new();
    for review in &reviews {
        match causes.last_mut() {
            Some(count) if count.name == review.cause.as_str() => count.count += 1,
            _ => causes.push(Count {
                name: review.cause.as_str().to_owned(),
                count: 1,
            }),
        }
    }
    Ok(ReviewList {
        library_revision: rows.revision,
        causes,
        reviews,
    })
}

impl LibraryStore {
    /// `identity.dryRun`: resolves the library again by the policy scans
    /// use, or by the policy and values `params` names, and compares each
    /// result with the assignment in force. It writes nothing.
    pub fn dry_run(&self, params: &DryRunParams) -> Result<DryRun, Error> {
        let (library, shown, rows) = self.read(read)?;
        let policy = policy_with(&params.policy);
        let resolution = ritornel_domain::resolve(&library, &policy);
        let plan = Plan::new(resolution, &library, &shown, policy.version);
        compare(
            &library,
            plan.resolution(),
            &rows,
            &policy,
            !plan.is_empty(),
            params.detail,
        )
    }

    /// `identity.review`: every binding in force that is marked for
    /// review, with the candidate it nearly matched, grouped by cause.
    pub fn review(&self) -> Result<ReviewList, Error> {
        let (library, _, rows) = self.read(read)?;
        review(&library, &rows)
    }
}
