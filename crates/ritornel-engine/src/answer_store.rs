//! Answers: yes to a proposal's candidate, a no (not this candidate, stop
//! asking, or keep two albums apart), and the undo of an answer. See
//! "Answers" and "Saying no" in docs/internals/corrections.md.
//!
//! An answer is one library write: the answer rows, the tag values each
//! puts on its files, the catalog worked out again with them, plays bound
//! again, emptied rows redirected to where their files went, and the
//! proposals worked out again. An undo is an answer too: it withdraws the
//! values of the one it undoes, clears the redirects, and hands the
//! projector the rows each file was on before.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use ritornel_contract::{
    Answer, AnswerEvidence, AnswerGot, AnswerList, AnswerVerb, AnsweredCandidate, AnsweredLayout,
    AnsweredReply, AnswersParams, Confirmation, DeclineParams, LayoutMethod,
};
use ritornel_domain::layout::LAYOUT_VERSION;
use ritornel_domain::library::{Join, Library, Noes};
use ritornel_domain::propose::PROPOSER_VERSION;
use ritornel_domain::reply::READER_VERSION;
use ritornel_domain::values::{self, Value};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::evidence_store::{revision, unsigned};
use crate::library_store::Write;
use crate::proposal_store::StoredValue;
use crate::resolve_store::{resolve_in, rows};
use crate::{Error, Id, LibraryStore, SubjectKind, WriteRequest, Written};

/// Where one file was before an answer: its assignment's rows.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub(crate) struct Was {
    item: i64,
    pub(crate) recording: i64,
    pub(crate) appearance: Option<i64>,
    pub(crate) release: Option<i64>,
    /// The group that release was in.
    pub(crate) group: Option<i64>,
}

/// A redirect an answer set (`answer_redirect`): a row no file was on any
/// more, led to the row its files went to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Redirect {
    pub(crate) table: String,
    pub(crate) row: i64,
    to: i64,
}

/// What an answer keeps of the library before it, for its undo: where each
/// file it moved was.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Before {
    items: Vec<Was>,
}

/// Keeps the redirects an answer set.
fn keep_redirects(write: &Write<'_>, answer: i64, redirects: &[Redirect]) -> Result<(), Error> {
    for redirect in redirects {
        write
            .connection()
            .prepare_cached(
                "INSERT INTO answer_redirect (answer_id, entity, row_id, to_id, rev)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )?
            .execute((
                answer,
                &redirect.table,
                redirect.row,
                redirect.to,
                write.rev(),
            ))?;
    }
    Ok(())
}

/// The redirects an answer set.
fn redirects_of(connection: &Connection, answer: i64) -> Result<Vec<Redirect>, Error> {
    Ok(connection
        .prepare_cached(
            "SELECT entity, row_id, to_id FROM answer_redirect WHERE answer_id = ?1 ORDER BY id",
        )?
        .query_map([answer], |row| {
            Ok(Redirect {
                table: row.get(0)?,
                row: row.get(1)?,
                to: row.get(2)?,
            })
        })?
        .collect::<Result<_, _>>()?)
}

/// Puts the tag values in force in place of the files' own: for each file
/// and field, the values of the newest answer that wrote that field and
/// whose values are not withdrawn, leaving out those of the answers in
/// `undone`, as an undo of them would.
pub(crate) fn apply_in_force(
    connection: &Connection,
    library: &mut Library,
    undone: &HashSet<i64>,
) -> Result<(), Error> {
    let mut found = rows(
        connection,
        "SELECT answer_id, source_item_id, field, ordinal, value, origin FROM answer_value
         WHERE withdrawn_by_answer_id IS NULL",
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                StoredValue {
                    item: row.get(1)?,
                    field: row.get(2)?,
                    ordinal: row.get(3)?,
                    value: row.get(4)?,
                    origin: row.get(5)?,
                },
            ))
        },
    )?;
    found.retain(|(answer, _)| !undone.contains(answer));
    if found.is_empty() {
        return Ok(());
    }
    let mut newest: HashMap<(i64, &str), i64> = HashMap::new();
    for (answer, value) in &found {
        let at = newest
            .entry((value.item, value.field.as_str()))
            .or_insert(*answer);
        *at = (*at).max(*answer);
    }
    let in_force: Vec<Value> = found
        .iter()
        .filter(|(answer, value)| newest.get(&(value.item, value.field.as_str())) == Some(answer))
        .filter_map(|(_, value)| value.value())
        .collect();
    values::apply(library, &in_force);
    Ok(())
}

/// The release rows an answer led on that still lead where it led them.
pub(crate) fn answer_redirects(connection: &Connection) -> Result<HashSet<i64>, Error> {
    Ok(rows(
        connection,
        "SELECT DISTINCT r.row_id FROM answer_redirect r
         JOIN release rel ON rel.id = r.row_id AND rel.redirect_to_id = r.to_id
         WHERE r.entity = 'release'
           AND NOT EXISTS (SELECT 1 FROM answer u WHERE u.undoes_answer_id = r.answer_id)",
        |row| row.get(0),
    )?
    .into_iter()
    .collect())
}

/// What each standing answer put together (`answer_join`), each release
/// row read as the end of its redirect chain, but the answers in `undone`.
pub(crate) fn joins_in_force(
    connection: &Connection,
    undone: &HashSet<i64>,
) -> Result<Vec<Join>, Error> {
    let found = rows(
        connection,
        "SELECT j.answer_id, j.entity,
                CASE j.entity WHEN 'release'
                  THEN coalesce((SELECT canonical_id FROM release WHERE id = j.row_id), j.row_id)
                  ELSE j.row_id END,
                j.album_mbid
         FROM answer_join j
         WHERE NOT EXISTS (SELECT 1 FROM answer u WHERE u.undoes_answer_id = j.answer_id)
         ORDER BY j.answer_id, j.entity, j.row_id",
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
            ))
        },
    )?;
    let mut joins: BTreeMap<i64, Join> = BTreeMap::new();
    for (answer, entity, row, album) in found {
        if undone.contains(&answer) {
            continue;
        }
        let join = joins.entry(answer).or_default();
        join.album = album;
        match entity.as_str() {
            "release" => join.releases.push(row),
            "release_group" => join.groups.push(row),
            // An entity a later build joins is not this build's to count.
            _ => {}
        }
    }
    Ok(joins.into_values().collect())
}

/// What standing answers said no to, but the answers in `undone`.
pub(crate) fn noes_in_force(connection: &Connection, undone: &HashSet<i64>) -> Result<Noes, Error> {
    type Rule = (i64, i64, String, Option<String>, Option<String>);
    let rules: Vec<Rule> = rows(
        connection,
        "SELECT id, answer_id, kind, candidate, about FROM answer_rule
         WHERE withdrawn_by_answer_id IS NULL ORDER BY id",
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        },
    )?;
    let mut noes = Noes::default();
    for (rule, answer, kind, candidate, about) in rules {
        if undone.contains(&answer) {
            continue;
        }
        let items: Vec<(i64, i64)> = connection
            .prepare_cached(
                "SELECT source_item_id, side FROM answer_rule_item WHERE rule_id = ?1 ORDER BY source_item_id",
            )?
            .query_map([rule], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<_, _>>()?;
        let side = |which: i64| -> Vec<i64> {
            items
                .iter()
                .filter(|(_, side)| *side == which)
                .map(|(item, _)| *item)
                .collect()
        };
        match (kind.as_str(), candidate) {
            ("not_candidate", Some(candidate)) => noes.not_candidate.push((candidate, side(0))),
            ("stop_asking", _) => noes.stop_asking.push((about, side(0))),
            // Kept apart as albums; what a later build keeps apart as
            // something else is not this build's to heed.
            ("apart", _) if about.as_deref().is_none_or(|about| about == "album") => {
                noes.apart.push((side(0), side(1)));
            }
            _ => {}
        }
    }
    Ok(noes)
}

/// Keeps a no an answer said, with the files it is about, each on its side.
fn keep_rule(
    write: &Write<'_>,
    answer: i64,
    kind: &str,
    candidate: Option<&str>,
    about: Option<&str>,
    sides: [&[i64]; 2],
) -> Result<(), Error> {
    let connection = write.connection();
    connection
        .prepare_cached(
            "INSERT INTO answer_rule (answer_id, kind, candidate, about, rev)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )?
        .execute((answer, kind, candidate, about, write.rev()))?;
    let rule = connection.last_insert_rowid();
    for (side, items) in (0..).zip(sides) {
        for item in items {
            connection
                .prepare_cached(
                    "INSERT INTO answer_rule_item (rule_id, source_item_id, side, rev)
                     VALUES (?1, ?2, ?3, ?4)",
                )?
                .execute((rule, item, side, write.rev()))?;
        }
    }
    Ok(())
}

/// Keeps what an answer put together.
fn keep_join(write: &Write<'_>, answer: i64, join: &Join) -> Result<(), Error> {
    let rows = join
        .releases
        .iter()
        .map(|row| ("release", row))
        .chain(join.groups.iter().map(|row| ("release_group", row)));
    for (entity, row) in rows {
        write
            .connection()
            .prepare_cached(
                "INSERT INTO answer_join (answer_id, entity, row_id, album_mbid, rev)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )?
            .execute((answer, entity, row, &join.album, write.rev()))?;
    }
    Ok(())
}

/// Where every file is now: its current assignment's rows. Only the
/// resolver moves a file, and it moves nothing else.
fn where_now(connection: &Connection) -> Result<HashMap<i64, Was>, Error> {
    Ok(rows(
        connection,
        "SELECT a.source_item_id, a.recording_id, a.appearance_id, ta.release_id, rel.release_group_id
         FROM assignment a
         JOIN source_item si ON si.id = a.source_item_id
         LEFT JOIN track_appearance ta ON ta.id = a.appearance_id
         LEFT JOIN release rel ON rel.id = ta.release_id
         WHERE a.superseded_at_ms IS NULL AND si.item_kind = 'file'",
        |row| {
            Ok(Was {
                item: row.get(0)?,
                recording: row.get(1)?,
                appearance: row.get(2)?,
                release: row.get(3)?,
                group: row.get(4)?,
            })
        },
    )?
    .into_iter()
    .map(|was| (was.item, was))
    .collect())
}

/// Where each file a write moved was before it, by the file.
fn moved(before: &HashMap<i64, Was>, after: &HashMap<i64, Was>) -> BTreeMap<i64, Was> {
    let rows = |was: &Was| (was.recording, was.appearance, was.release, was.group);
    before
        .values()
        .filter(|was| after.get(&was.item).map(rows) != Some(rows(was)))
        .map(|was| (was.item, *was))
        .collect()
}

/// Makes every row's canonical ID the end of its redirect chain again.
fn recanonicalize(write: &Write<'_>, table: &str) -> Result<(), Error> {
    let connection = write.connection();
    let redirects: HashMap<i64, i64> = rows(
        connection,
        &format!("SELECT id, redirect_to_id FROM {table} WHERE redirect_to_id IS NOT NULL"),
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
    )?
    .into_iter()
    .collect();
    for id in redirects.keys() {
        let mut end = *id;
        let mut seen = HashSet::new();
        while let Some(next) = redirects.get(&end) {
            if !seen.insert(end) {
                break;
            }
            end = *next;
        }
        connection
            .prepare_cached(&format!(
                "UPDATE {table} SET canonical_id = ?2, rev = ?3 WHERE id = ?1 AND canonical_id <> ?2"
            ))?
            .execute((id, end, write.rev()))?;
    }
    connection
        .prepare_cached(&format!(
            "UPDATE {table} SET canonical_id = id, rev = ?1
             WHERE redirect_to_id IS NULL AND canonical_id <> id"
        ))?
        .execute([write.rev()])?;
    Ok(())
}

/// Whether no live file's current assignment is on this row. A scrobble's
/// subject, a Spotify track or a retired file bound to it follows it where
/// it leads.
fn emptied(connection: &Connection, table: &str, row: i64) -> Result<bool, Error> {
    let on = match table {
        "track_appearance" => "a.appearance_id = ?1",
        "release" => "a.appearance_id IN (SELECT id FROM track_appearance WHERE release_id = ?1)",
        _ => "a.recording_id = ?1",
    };
    let sql = format!(
        "SELECT NOT EXISTS (SELECT 1 FROM assignment a JOIN source_item si ON si.id = a.source_item_id
                            WHERE {on} AND a.superseded_at_ms IS NULL
                              AND si.item_kind = 'file' AND si.retired_at_ms IS NULL)"
    );
    Ok(connection
        .prepare_cached(&sql)?
        .query_row([row], |found| found.get(0))?)
}

/// Redirects each row a file was on (`been`) that the write emptied to where
/// its files went: a
/// recording, an appearance or a release that no file's assignment is on
/// any more, and that is not redirected already. A rating, a queue entry or
/// a correction naming it then follows. It returns the redirects it set.
fn redirect_emptied<'a>(
    write: &Write<'_>,
    been: impl IntoIterator<Item = &'a Was>,
) -> Result<Vec<Redirect>, Error> {
    let now = where_now(write.connection())?;
    // Each emptied row leads to where most of its files went, each file
    // counted once for it.
    let mut counted: HashSet<(&'static str, i64, i64)> = HashSet::new();
    let mut targets: BTreeMap<(&'static str, i64), BTreeMap<i64, usize>> = BTreeMap::new();
    for was in been {
        let Some(is) = now.get(&was.item) else {
            continue;
        };
        let moves = [
            ("recording", Some(was.recording), Some(is.recording)),
            ("track_appearance", was.appearance, is.appearance),
            ("release", was.release, is.release),
        ];
        for (table, from, to) in moves {
            if let (Some(from), Some(to)) = (from, to)
                && from != to
                && counted.insert((table, from, was.item))
            {
                *targets
                    .entry((table, from))
                    .or_default()
                    .entry(to)
                    .or_default() += 1;
            }
        }
    }
    let mut set = Vec::new();
    for ((table, row), went) in targets {
        let gone_to: Vec<i64> = went.keys().copied().collect();
        let Some(to) = went
            .into_iter()
            .max_by_key(|(to, count)| (*count, std::cmp::Reverse(*to)))
            .map(|(to, _)| to)
        else {
            continue;
        };
        if redirect_if_emptied(write, table, row, to)?
            || lead_on_if_astray(write, table, row, to, &gone_to)?
        {
            set.push(Redirect {
                table: table.to_owned(),
                row,
                to,
            });
        }
    }
    for table in set
        .iter()
        .map(|redirect| redirect.table.as_str())
        .collect::<BTreeSet<_>>()
    {
        recanonicalize(write, table)?;
    }
    Ok(set)
}

/// Leads a row to another when no file is on it and it leads nowhere yet.
/// The canonical IDs of rows that led to it are settled by
/// [`recanonicalize`].
fn redirect_if_emptied(write: &Write<'_>, table: &str, row: i64, to: i64) -> Result<bool, Error> {
    let connection = write.connection();
    // Free, and not reached by following the row it would lead to.
    let free: bool = connection.query_row(
        &format!(
            "WITH RECURSIVE chain (id) AS (
               SELECT ?2
               UNION SELECT t.redirect_to_id FROM {table} t JOIN chain ON t.id = chain.id
               WHERE t.redirect_to_id IS NOT NULL
             )
             SELECT redirect_to_id IS NULL AND NOT EXISTS (SELECT 1 FROM chain WHERE id = ?1)
             FROM {table} WHERE id = ?1"
        ),
        [row, to],
        |found| found.get(0),
    )?;
    if !free || !emptied(connection, table, row)? {
        return Ok(false);
    }
    connection
        .prepare_cached(&format!(
            "UPDATE {table} SET redirect_to_id = ?2,
               canonical_id = (SELECT canonical_id FROM {table} WHERE id = ?2), rev = ?3
             WHERE id = ?1"
        ))?
        .execute((row, to, write.rev()))?;
    Ok(true)
}

/// Leads a row an answer led elsewhere to where its files went, when no
/// file is on it and none of those it was on before is where it leads: as
/// a disc an undo parted again from the album a later answer moved it
/// with, whose old release still led to that album. Says whether it did.
fn lead_on_if_astray(
    write: &Write<'_>,
    table: &str,
    row: i64,
    to: i64,
    gone_to: &[i64],
) -> Result<bool, Error> {
    let connection = write.connection();
    let led: Option<i64> = connection
        .prepare_cached(&format!(
            "SELECT redirect_to_id FROM {table} WHERE id = ?1 AND redirect_to_id IS NOT NULL"
        ))?
        .query_row([row], |found| found.get(0))
        .optional()?;
    let Some(at) = led else {
        return Ok(false);
    };
    // Where its chain ends, and where the files' rows' chains end: a row
    // that already leads to where they went, by any chain, is not astray.
    let end = |of: i64| -> Result<i64, Error> {
        Ok(connection
            .prepare_cached(&format!("SELECT canonical_id FROM {table} WHERE id = ?1"))?
            .query_row([of], |found| found.get(0))
            .optional()?
            .unwrap_or(of))
    };
    let leads_to = end(row)?;
    let mut went: Vec<i64> = vec![end(to)?];
    for gone in gone_to {
        went.push(end(*gone)?);
    }
    if went.contains(&leads_to) || !emptied(connection, table, row)? {
        return Ok(false);
    }
    let by_an_answer = standing_redirect(connection, table, row, at)?;
    // Not reached by following the row it would lead to.
    let free: bool = connection.query_row(
        &format!(
            "WITH RECURSIVE chain (id) AS (
               SELECT ?2
               UNION SELECT t.redirect_to_id FROM {table} t JOIN chain ON t.id = chain.id
               WHERE t.redirect_to_id IS NOT NULL
             )
             SELECT NOT EXISTS (SELECT 1 FROM chain WHERE id = ?1)"
        ),
        [row, to],
        |found| found.get(0),
    )?;
    if !by_an_answer || !free {
        return Ok(false);
    }
    connection
        .prepare_cached(&format!(
            "UPDATE {table} SET redirect_to_id = ?2,
               canonical_id = (SELECT canonical_id FROM {table} WHERE id = ?2), rev = ?3
             WHERE id = ?1"
        ))?
        .execute((row, to, write.rev()))?;
    Ok(true)
}

/// Whether a yes still standing, or an undo, led this row where it leads:
/// a redirect a person made by hand is never one an answer moves or clears.
fn standing_redirect(
    connection: &Connection,
    table: &str,
    row: i64,
    to: i64,
) -> Result<bool, Error> {
    Ok(connection
        .prepare_cached(
            "SELECT EXISTS (SELECT 1 FROM answer_redirect r
                            WHERE r.entity = ?1 AND r.row_id = ?2 AND r.to_id = ?3
                              AND NOT EXISTS (SELECT 1 FROM answer u WHERE u.undoes_answer_id = r.answer_id))",
        )?
        .query_row((table, row, to), |found| found.get(0))?)
}

/// Clears these redirects where they still lead where their answer led
/// them, and says whether it cleared any.
fn clear_redirects(write: &Write<'_>, redirects: &[Redirect]) -> Result<bool, Error> {
    let connection = write.connection();
    let mut cleared = 0;
    for redirect in redirects {
        cleared += connection
            .prepare_cached(&format!(
                "UPDATE {} SET redirect_to_id = NULL, canonical_id = id, rev = ?3
                 WHERE id = ?1 AND redirect_to_id = ?2",
                redirect.table
            ))?
            .execute((redirect.row, redirect.to, write.rev()))?;
    }
    for table in ["recording", "track_appearance", "release"] {
        recanonicalize(write, table)?;
    }
    Ok(cleared > 0)
}

/// Those of `releases` whose files, of `files`, share a release with
/// another's after a write, from where each file was (`was`) to where it
/// is (`now`); one release alone is kept as it is.
fn put_together(
    releases: &[i64],
    files: impl Iterator<Item = i64>,
    was: &HashMap<i64, Was>,
    now: &HashMap<i64, Was>,
) -> Vec<i64> {
    if releases.len() < 2 {
        return releases.to_vec();
    }
    let mut on: BTreeMap<i64, BTreeSet<i64>> = BTreeMap::new();
    for file in files {
        if let (Some(Some(from)), Some(Some(to))) = (
            was.get(&file).map(|was| was.release),
            now.get(&file).map(|now| now.release),
        ) && releases.contains(&from)
        {
            on.entry(to).or_default().insert(from);
        }
    }
    let joined: BTreeSet<i64> = on
        .into_values()
        .filter(|from| from.len() > 1)
        .flatten()
        .collect();
    joined.into_iter().collect()
}

/// Those of `releases` whose files, of `files`, share a release key with
/// another's files (`keys`, by file).
fn alike(
    releases: &[i64],
    files: impl Iterator<Item = i64>,
    was: &HashMap<i64, Was>,
    keys: &HashMap<i64, String>,
) -> Vec<i64> {
    if releases.len() < 2 {
        return releases.to_vec();
    }
    let mut on: BTreeMap<&str, BTreeSet<i64>> = BTreeMap::new();
    for file in files {
        if let (Some(Some(release)), Some(key)) =
            (was.get(&file).map(|was| was.release), keys.get(&file))
            && releases.contains(&release)
        {
            on.entry(key.as_str()).or_default().insert(release);
        }
    }
    let alike: BTreeSet<i64> = on
        .into_values()
        .filter(|rows| rows.len() > 1)
        .flatten()
        .collect();
    alike.into_iter().collect()
}

/// The files each answer of a write moved besides its own: a file no
/// answer of it is about moved with those whose own files are in the album
/// it went to or came from, or, where none is, with every answer of the
/// write.
fn moved_with(
    all: &[Chosen],
    moved: &BTreeMap<i64, Was>,
    was: &HashMap<i64, Was>,
    now: &HashMap<i64, Was>,
) -> Vec<BTreeSet<i64>> {
    let own: HashSet<i64> = all
        .iter()
        .flat_map(|one| one.items.iter().copied())
        .collect();
    let albums: Vec<HashSet<i64>> = all
        .iter()
        .map(|one| {
            one.items
                .iter()
                .flat_map(|item| [was.get(item), now.get(item)])
                .filter_map(|at| at?.group)
                .collect()
        })
        .collect();
    let mut with = vec![BTreeSet::new(); all.len()];
    for (item, then) in moved.iter().filter(|(item, _)| !own.contains(item)) {
        let groups = [then.group, now.get(item).and_then(|is| is.group)];
        let mut found = false;
        for (theirs, album) in with.iter_mut().zip(&albums) {
            if groups.iter().flatten().any(|group| album.contains(group)) {
                theirs.insert(*item);
                found = true;
            }
        }
        if !found {
            for theirs in &mut with {
                theirs.insert(*item);
            }
        }
    }
    with
}

/// The redirects of rows these files were on; a redirect of a row none of
/// the write's files (`all`) was on goes with every answer.
fn redirects_of_rows<'a>(
    redirects: &[Redirect],
    files: impl Iterator<Item = &'a Was>,
    all: &[Was],
) -> Vec<Redirect> {
    let on = |was: &Was, redirect: &Redirect| match redirect.table.as_str() {
        "recording" => was.recording == redirect.row,
        "track_appearance" => was.appearance == Some(redirect.row),
        "release" => was.release == Some(redirect.row),
        _ => false,
    };
    let files: Vec<&Was> = files.collect();
    redirects
        .iter()
        .filter(|redirect| {
            files.iter().any(|was| on(was, redirect)) || !all.iter().any(|was| on(was, redirect))
        })
        .cloned()
        .collect()
}

/// Makes a resolution still owed, such as one a stopped scan left, in the
/// answer's write before the answer is worked out, so that what it moves
/// is not the answer's to keep or to give back. Says whether it changed
/// anything shown.
fn settle_owed(write: &Write<'_>) -> Result<bool, Error> {
    if !crate::resolve_store::owes_resolution(write.connection())? {
        return Ok(false);
    }
    let (_, shown) = resolve_in(write, |_| {})?;
    // A resolution that asks for nothing still completes what was owed.
    crate::resolve_store::settle_pending_in(write)?;
    Ok(shown)
}

/// What a write's projection did: where each file it moved was, the
/// redirects it set, and whether anything shown changed.
struct Projected {
    moved: BTreeMap<i64, Was>,
    redirects: Vec<Redirect>,
    shown: bool,
}

/// Works the catalog out with `adjust`, and the first time with `first`
/// too, from files that were where `was` says. What `first` hands the
/// files, such as the rows an undo gives back, is taken or not in that
/// first pass; the passes after it keep what it made. Each row a file was on and is no longer, in `was`, in a pass
/// below or before the answers an undo undoes (`earlier`), that no file is
/// on now, is led to where most of its files are; and the catalog is
/// worked out again with those redirects, so that edits and plays on the
/// rows follow them, until no more is emptied.
fn project(
    write: &Write<'_>,
    was: &HashMap<i64, Was>,
    earlier: &[Was],
    adjust: impl Fn(&mut Library),
    first: impl Fn(&mut Library),
) -> Result<Projected, Error> {
    let connection = write.connection();
    let (_, mut shown) = resolve_in(write, |library| {
        adjust(library);
        first(library);
    })?;
    let mut redirects: Vec<Redirect> = Vec::new();
    // Every place a file has been in this write, and before it.
    let mut been: Vec<Was> = earlier.to_vec();
    let mut then = was.clone();
    // Each pass can only lead on rows the files were on, each once, so
    // the passes come to an end; one that does not is refused.
    for _ in 0..=3 * (was.len() + earlier.len()) {
        let now = where_now(connection)?;
        been.extend(moved(&then, &now).into_values());
        let more = redirect_emptied(write, &been)?;
        if more.is_empty() {
            // A row led on twice in one write is kept once, where it leads
            // last.
            let mut last: BTreeMap<(String, i64), Redirect> = BTreeMap::new();
            for redirect in redirects {
                last.insert((redirect.table.clone(), redirect.row), redirect);
            }
            return Ok(Projected {
                moved: moved(was, &now),
                redirects: last.into_values().collect(),
                shown,
            });
        }
        then = now;
        redirects.extend(more);
        shown = true;
        let led: HashSet<i64> = redirects
            .iter()
            .filter(|redirect| redirect.table == "release")
            .map(|redirect| redirect.row)
            .collect();
        resolve_in(write, |library| {
            adjust(library);
            library.answer_redirects.extend(&led);
        })?;
        crate::history_store::catalog_changed(write)?;
    }
    Err(Error::Corrupt("the redirects of an answer did not settle"))
}

/// The answers that undoing `answers` undoes: those, and every standing
/// answer that rests on one of them (`answer_dependency`), and on those in
/// turn, oldest first.
pub(crate) fn with_dependents(connection: &Connection, answers: &[i64]) -> Result<Vec<i64>, Error> {
    let edges = rows(
        connection,
        "SELECT d.answer_id, d.depends_on_answer_id FROM answer_dependency d
         WHERE NOT EXISTS (SELECT 1 FROM answer u WHERE u.undoes_answer_id = d.answer_id)",
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
    )?;
    let mut all: BTreeSet<i64> = answers.iter().copied().collect();
    loop {
        let more: Vec<i64> = edges
            .iter()
            .filter(|(answer, on)| all.contains(on) && !all.contains(answer))
            .map(|(answer, _)| *answer)
            .collect();
        if more.is_empty() {
            return Ok(all.into_iter().collect());
        }
        all.extend(more);
    }
}

/// What an undo of the answers in `undoing` hands each file back: where
/// the latest yes that moved it and still stands left it, which is where it
/// was before the first undone one after that; or, when none that moved it
/// stands, the rows it was on before the first of a run of answers that
/// moved it. A file whose latest mover stands is handed nothing.
///
/// An answer moved a file when it gave it values, or when the file moved in
/// its write and no answer of that write gave it values: such a file moved
/// with every answer of the write.
pub(crate) fn handed(
    connection: &Connection,
    undoing: &HashSet<i64>,
) -> Result<(HashMap<i64, Was>, Vec<Was>), Error> {
    let confirms = rows(
        connection,
        "SELECT a.id, a.rev, a.before_json,
                (SELECT u.rev FROM answer u WHERE u.undoes_answer_id = a.id)
         FROM answer a WHERE a.verb <> 'undo' ORDER BY a.id",
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<i64>>(3)?,
            ))
        },
    )?;
    let mut subject: HashMap<i64, HashSet<i64>> = HashMap::new();
    for (answer, item) in rows(
        connection,
        "SELECT DISTINCT answer_id, source_item_id FROM answer_value",
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
    )? {
        subject.entry(answer).or_default().insert(item);
    }
    let mut of_write: HashMap<i64, HashSet<i64>> = HashMap::new();
    for (answer, rev, _, _) in &confirms {
        if let Some(items) = subject.get(answer) {
            of_write.entry(*rev).or_default().extend(items);
        }
    }
    // The files of the answers undone now, and of those undone earlier
    // while one of these stood and about some of the same files, which may
    // have kept them from where they were.
    let befores: Vec<(i64, i64, Option<i64>, Before)> = confirms
        .into_iter()
        .map(|(answer, rev, before, undone_at)| {
            Ok((
                answer,
                rev,
                undone_at,
                serde_json::from_str::<Before>(&before)?,
            ))
        })
        .collect::<Result<_, Error>>()?;
    let undoing_since: Vec<i64> = befores
        .iter()
        .filter(|(answer, ..)| undoing.contains(answer))
        .map(|(_, rev, _, _)| *rev)
        .collect();
    let undoing_files: HashSet<i64> = befores
        .iter()
        .filter(|(answer, ..)| undoing.contains(answer))
        .flat_map(|(_, _, _, before)| before.items.iter().map(|was| was.item))
        .collect();
    let seen: Vec<Was> = befores
        .iter()
        .filter(|(answer, _, undone_at, before)| {
            undoing.contains(answer)
                || (undone_at.is_some_and(|at| undoing_since.iter().any(|since| at > *since))
                    && before
                        .items
                        .iter()
                        .any(|was| undoing_files.contains(&was.item)))
        })
        .flat_map(|(_, _, _, before)| before.items.iter().copied())
        .collect();
    let wanted: HashSet<i64> = seen.iter().map(|was| was.item).collect();
    // Each file's history: a yes that moved it, at its revision, and its
    // undo, at its own; those undone now, last.
    let mut events: HashMap<i64, Vec<(i64, i64, Option<Was>)>> = HashMap::new();
    for (answer, rev, undone_at, before) in &befores {
        let undone_at = if undoing.contains(answer) {
            Some(i64::MAX)
        } else {
            *undone_at
        };
        for was in before.items.iter().copied() {
            if !wanted.contains(&was.item) {
                continue;
            }
            let own = subject
                .get(answer)
                .is_some_and(|items| items.contains(&was.item));
            let anyone = of_write
                .get(rev)
                .is_some_and(|items| items.contains(&was.item));
            if !own && anyone {
                continue;
            }
            let history = events.entry(was.item).or_default();
            history.push((*rev, *answer, Some(was)));
            if let Some(at) = undone_at {
                history.push((at, *answer, None));
            }
        }
    }
    let mut handed = HashMap::new();
    for (item, mut history) in events {
        history.sort_by_key(|(at, answer, was)| (*at, was.is_none(), *answer));
        // Where a yes that moved it still stands, the file goes back to
        // where the latest such yes left it: where it was before the first
        // undone one after it. A rescan, a retag or a re-rip keeps an
        // answered file on its rows, so between two answers only another
        // answer moves it, and that one is then the latest to move it.
        let undone: HashSet<i64> = history
            .iter()
            .filter(|(_, _, was)| was.is_none())
            .map(|(_, answer, _)| *answer)
            .collect();
        let movers: Vec<(i64, Was)> = history
            .iter()
            .filter_map(|(_, answer, was)| was.map(|was| (*answer, was)))
            .collect();
        if let Some(last) = movers
            .iter()
            .rposition(|(answer, ..)| !undone.contains(answer))
        {
            if let Some((_, was)) = movers.get(last + 1) {
                handed.insert(item, *was);
            }
            continue;
        }
        let mut standing = 0usize;
        let mut base: Option<Was> = None;
        for (_, _, was) in history {
            match was {
                Some(was) => {
                    if standing == 0 {
                        base = Some(was);
                    }
                    standing += 1;
                }
                None => standing = standing.saturating_sub(1),
            }
        }
        if standing == 0
            && let Some(base) = base
        {
            handed.insert(item, base);
        }
    }
    Ok((handed, seen))
}

/// The group each release the files are handed back was in, by the first
/// of its files, so that two files handed one release from different times
/// give it one group, the same every time.
pub(crate) fn prior_groups(hints: &HashMap<i64, Was>) -> HashMap<i64, i64> {
    let mut handed: Vec<&Was> = hints.values().collect();
    handed.sort_by_key(|was| was.item);
    let mut groups = HashMap::new();
    for was in handed {
        if let (Some(release), Some(group)) = (was.release, was.group) {
            groups.entry(release).or_insert(group);
        }
    }
    groups
}

/// The redirects an answer, or an undo, set on the rows these files are
/// handed back, where they still lead where they were led: cleared, so
/// that the files can be on those rows again.
pub(crate) fn redirects_into(
    connection: &Connection,
    hints: &HashMap<i64, Was>,
) -> Result<Vec<Redirect>, Error> {
    let mut rows: BTreeSet<(&str, i64)> = BTreeSet::new();
    for was in hints.values() {
        rows.insert(("recording", was.recording));
        rows.extend(was.appearance.map(|row| ("track_appearance", row)));
        rows.extend(was.release.map(|row| ("release", row)));
    }
    let mut found = Vec::new();
    for (table, row) in rows {
        let led: Option<i64> = connection
            .prepare_cached(&format!(
                "SELECT redirect_to_id FROM {table} WHERE id = ?1 AND redirect_to_id IS NOT NULL"
            ))?
            .query_row([row], |found| found.get(0))
            .optional()?;
        let Some(to) = led else {
            continue;
        };
        let by_an_answer = standing_redirect(connection, table, row, to)?;
        if by_an_answer {
            found.push(Redirect {
                table: table.to_owned(),
                row,
                to,
            });
        }
    }
    Ok(found)
}

/// Who gave an answer, as the ledger records it.
fn actor(request: &WriteRequest) -> (&'static str, Option<[u8; 16]>) {
    let kind = match request.client.map(|client| client.kind) {
        Some(SubjectKind::Agent) => "agent",
        _ => "person",
    };
    (
        kind,
        request.client.map(|client| *client.session.as_bytes()),
    )
}

/// The proposal an answer answered, as it was shown: its kind, its public
/// ID and fingerprint, and the candidate's own ID (an album's release
/// group, or an edition's release), if it named one.
#[derive(Clone, Copy)]
struct Answering<'a> {
    kind: &'a str,
    proposal: [u8; 16],
    fingerprint: &'a str,
    candidate: Option<&'a str>,
}

impl Chosen {
    fn answering(&self) -> Answering<'_> {
        Answering {
            kind: &self.kind,
            proposal: self.proposal,
            fingerprint: &self.fingerprint,
            candidate: Some(self.release.as_deref().unwrap_or(&self.candidate)),
        }
    }
}

/// Writes one answer row and returns its row and public ID.
#[allow(clippy::too_many_arguments)]
fn insert_answer(
    write: &Write<'_>,
    request: &WriteRequest,
    verb: &str,
    proposal: Option<Answering<'_>>,
    undoes: Option<i64>,
    evidence: &serde_json::Value,
    before: &Before,
) -> Result<(i64, Id), Error> {
    let connection = write.connection();
    let id = write.new_id()?;
    let (kind, session) = actor(request);
    connection
        .prepare_cached(
            "INSERT INTO answer
               (public_id, verb, kind, proposal_public_id, proposal_fingerprint, candidate,
                undoes_answer_id, actor_kind, actor_session_pid, answered_at_ms, layout_version,
                reader_version, proposer_version, evidence_json, before_json, rev)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
        )?
        .execute(rusqlite::params![
            id.as_bytes(),
            verb,
            proposal.map(|one| one.kind),
            proposal.map(|one| one.proposal.to_vec()),
            proposal.map(|one| one.fingerprint),
            proposal.and_then(|one| one.candidate),
            undoes,
            kind,
            session.map(|session| session.to_vec()),
            write.now_ms(),
            LAYOUT_VERSION,
            READER_VERSION,
            PROPOSER_VERSION,
            evidence.to_string(),
            serde_json::to_string(before)?,
            write.rev(),
        ])?;
    Ok((connection.last_insert_rowid(), id))
}

/// What a confirm of one proposal reads first.
struct Chosen {
    proposal: [u8; 16],
    /// `album` or `edition`.
    kind: String,
    fingerprint: String,
    /// The album said yes to: for an edition, the album of the release.
    candidate: String,
    /// For an edition: the release said yes to.
    release: Option<String>,
    items: Vec<i64>,
    /// The files of the first disc a yes joins these onto, whose title and
    /// credit it takes.
    onto: Vec<i64>,
    values: Vec<StoredValue>,
    evidence: serde_json::Value,
}

/// Reads one confirmation's proposal and candidate, checking it was shown
/// as it is now.
fn chosen(connection: &Connection, confirmation: &Confirmation) -> Result<Chosen, Error> {
    let found: Option<(i64, String, String)> = connection
        .query_row(
            "SELECT id, fingerprint, kind FROM proposal WHERE public_id = ?1",
            [confirmation.proposal_id.as_bytes()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let (row, fingerprint, kind) = found.ok_or(Error::NotFound("no such proposal"))?;
    if (kind == "edition") != confirmation.release.is_some() {
        return Err(Error::Invalid(
            "a yes to an edition names its release, and a yes to an album none",
        ));
    }
    if fingerprint != confirmation.fingerprint {
        return Err(Error::PlanStale);
    }
    /// A candidate's values, title, artist, kind, first date, releases and
    /// the first disc it joins its own onto.
    type Row = (
        String,
        Option<String>,
        String,
        Option<String>,
        Option<String>,
        u32,
        String,
    );
    let candidate: Option<Row> = connection
        .query_row(
            "SELECT values_json, title, artist, primary_type, first_date, releases, onto_json
             FROM proposal_candidate
             WHERE proposal_id = ?1 AND release_group_mbid = ?2 AND release_mbid IS ?3",
            (row, &confirmation.release_group, &confirmation.release),
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )
        .optional()?;
    let (values_json, title, artist, primary_type, first_date, releases, onto) =
        candidate.ok_or(Error::NotFound("no such candidate"))?;
    let items: Vec<i64> = connection
        .prepare_cached("SELECT source_item_id FROM proposal_item WHERE proposal_id = ?1 ORDER BY source_item_id")?
        .query_map([row], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    let rests_on: Vec<serde_json::Value> = connection
        .prepare_cached(
            "SELECT pe.disc_id, pe.lookup_response_id, lr.received_at_ms FROM proposal_evidence pe
             JOIN lookup_response lr ON lr.id = pe.lookup_response_id
             WHERE pe.proposal_id = ?1 ORDER BY pe.disc_id",
        )?
        .query_map([row], |row| {
            Ok(serde_json::json!({
                "discId": row.get::<_, String>(0)?,
                "lookupResponseId": row.get::<_, i64>(1)?,
                "repliedAtMs": row.get::<_, i64>(2)?,
            }))
        })?
        .collect::<Result<_, _>>()?;
    // Each layout of those discs, with the sidecar revisions it was read
    // from, which are kept as they were.
    let mut layouts: Vec<serde_json::Value> = Vec::new();
    for disc in rests_on
        .iter()
        .filter_map(|rest| rest.get("discId")?.as_str())
    {
        layouts.extend(
            connection
                .prepare_cached(
                    "SELECT folder, found_json, provisional FROM evidence_disc
                     WHERE disc_id = ?1 ORDER BY source_instance_id, folder",
                )?
                .query_map([disc], |row| {
                    let found: String = row.get(1)?;
                    Ok(serde_json::json!({
                        "discId": disc,
                        "folder": row.get::<_, String>(0)?,
                        "found": serde_json::from_str::<serde_json::Value>(&found)
                            .unwrap_or(serde_json::Value::Null),
                        "provisional": row.get::<_, bool>(2)?,
                    }))
                })?
                .collect::<Result<Vec<_>, _>>()?,
        );
    }
    // The candidates it was chosen among, in the order shown.
    let shown: Vec<serde_json::Value> = connection
        .prepare_cached(
            "SELECT release_group_mbid, title, release_mbid FROM proposal_candidate
             WHERE proposal_id = ?1 ORDER BY ordinal",
        )?
        .query_map([row], |row| {
            Ok(serde_json::json!({
                "releaseGroup": row.get::<_, String>(0)?,
                "title": row.get::<_, Option<String>>(1)?,
                "release": row.get::<_, Option<String>>(2)?,
            }))
        })?
        .collect::<Result<_, _>>()?;
    let values: Vec<StoredValue> = serde_json::from_str(&values_json)?;
    let evidence = serde_json::json!({
        "kind": kind,
        "candidate": {
            "releaseGroup": confirmation.release_group,
            "release": confirmation.release,
            "title": title,
            "artist": artist,
            "primaryType": primary_type,
            "firstDate": first_date,
            "releases": releases,
        },
        "restsOn": rests_on,
        "layouts": layouts,
        "candidates": shown,
        "values": values,
    });
    Ok(Chosen {
        proposal: *confirmation.proposal_id.as_bytes(),
        kind,
        fingerprint,
        candidate: confirmation.release_group.clone(),
        release: confirmation.release.clone(),
        items,
        onto: serde_json::from_str(&onto)?,
        values,
        evidence,
    })
}

/// A page of the ledger, when the call names no size, and the most it
/// gives.
const ANSWERS_PAGE: u32 = 50;
const ANSWERS_PAGE_MAX: u32 = 500;

/// What `answer_row` reads.
const ANSWER_SELECT: &str =
    "SELECT a.id, a.public_id, a.verb, a.proposal_public_id, a.candidate, a.evidence_json,
        (SELECT u.public_id FROM answer u WHERE u.id = a.undoes_answer_id),
        (SELECT u.public_id FROM answer u WHERE u.undoes_answer_id = a.id),
        (SELECT count(*) FROM (SELECT source_item_id FROM answer_value v WHERE v.answer_id = a.id
                               UNION SELECT ri.source_item_id FROM answer_rule_item ri
                               JOIN answer_rule r ON r.id = ri.rule_id WHERE r.answer_id = a.id)),
        a.answered_at_ms, a.kind
 FROM answer a";

/// One answer of the ledger, and its evidence as kept, from
/// [`ANSWER_SELECT`].
fn answer_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<(Answer, String)> {
    let kept: String = row.get(5)?;
    let evidence = serde_json::from_str::<serde_json::Value>(&kept).unwrap_or_default();
    let said = |path: &str| {
        evidence
            .pointer(path)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    };
    let verb: String = row.get(2)?;
    Ok((
        Answer {
            answer_id: Id::from_bytes(row.get(1)?),
            verb: match verb.as_str() {
                "undo" => AnswerVerb::Undo,
                "decline" => AnswerVerb::Decline,
                "stop_asking" => AnswerVerb::StopAsking,
                "apart" => AnswerVerb::KeepApart,
                _ => AnswerVerb::Confirm,
            },
            proposal_id: row.get::<_, Option<[u8; 16]>>(3)?.map(Id::from_bytes),
            release_group: said("/candidate/releaseGroup").or(row.get(4)?),
            kind: row.get(10)?,
            release: said("/candidate/release"),
            title: said("/candidate/title"),
            undoes: row.get::<_, Option<[u8; 16]>>(6)?.map(Id::from_bytes),
            undone_by: row.get::<_, Option<[u8; 16]>>(7)?.map(Id::from_bytes),
            files: row.get(8)?,
            answered_at_ms: unsigned(row.get(9)?),
        },
        kept,
    ))
}

/// An answer's evidence as `evidence_json` keeps it.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Kept {
    candidate: Option<KeptCandidate>,
    candidates: Vec<KeptCandidate>,
    rests_on: Vec<KeptReply>,
    layouts: Vec<KeptLayout>,
    values: Vec<StoredValue>,
    /// For an undo of an answer resting on others: those it was undone
    /// with.
    rests_on_answers: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct KeptCandidate {
    release_group: String,
    release: Option<String>,
    title: Option<String>,
    artist: Option<String>,
    primary_type: Option<String>,
    first_date: Option<String>,
    releases: Option<u32>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct KeptReply {
    disc_id: String,
    replied_at_ms: i64,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct KeptLayout {
    disc_id: String,
    folder: String,
    found: serde_json::Value,
    provisional: bool,
}

/// The answers an undo names, by row: each one that stands and is no undo.
pub(crate) fn asked_to_undo(connection: &Connection, answers: &[Id]) -> Result<Vec<i64>, Error> {
    if answers.is_empty() {
        return Err(Error::Invalid("undo at least one answer"));
    }
    let mut found = Vec::new();
    for answer in answers {
        let one: Option<(i64, String, bool)> = connection
            .query_row(
                "SELECT id, verb,
                        EXISTS (SELECT 1 FROM answer u WHERE u.undoes_answer_id = answer.id)
                 FROM answer WHERE public_id = ?1",
                [answer.as_bytes()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let (row, verb, undone) = one.ok_or(Error::NotFound("no such answer"))?;
        if verb == "undo" {
            return Err(Error::Invalid(
                "an undo is not undone; to say it again, answer again",
            ));
        }
        if undone || found.contains(&row) {
            return Err(Error::Invalid("that answer is undone already"));
        }
        found.push(row);
    }
    Ok(found)
}

/// Undoes answers in `write`, with every standing answer that rests on one
/// of them: what [`LibraryStore::undo`] does, and its preview rehearses. It
/// gives each undo's ID with the answer it undid, and whether anything
/// shown changed.
pub(crate) fn undo_in(
    write: &Write<'_>,
    request: &WriteRequest,
    answers: &[Id],
) -> Result<(Vec<(Id, Id)>, bool), Error> {
    let connection = write.connection();
    let asked = asked_to_undo(connection, answers)?;
    let settled = settle_owed(write)?;
    let all = with_dependents(connection, &asked)?;
    let undoing: HashSet<i64> = all.iter().copied().collect();
    let (hints, seen) = handed(connection, &undoing)?;
    let mut written = Vec::new();
    let mut cleared: Vec<Redirect> = Vec::new();
    let mut earlier: Vec<Was> = Vec::new();
    let mut undos = Vec::new();
    for row in all {
        let (answer, before): ([u8; 16], String) = connection.query_row(
            "SELECT public_id, before_json FROM answer WHERE id = ?1",
            [row],
            |found| Ok((found.get(0)?, found.get(1)?)),
        )?;
        let answer = Id::from_bytes(answer);
        let before: Before = serde_json::from_str(&before)?;
        let mut evidence = serde_json::Map::new();
        evidence.insert("undoes".to_owned(), serde_json::json!(answer.to_string()));
        if !asked.contains(&row) {
            // Undone because it rests on one asked for.
            let on: Vec<String> = connection
                .prepare_cached(
                    "SELECT a.id, a.public_id FROM answer_dependency d
                     JOIN answer a ON a.id = d.depends_on_answer_id
                     WHERE d.answer_id = ?1 ORDER BY a.id",
                )?
                .query_map([row], |found| {
                    Ok((found.get::<_, i64>(0)?, found.get::<_, [u8; 16]>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .filter(|(on, _)| undoing.contains(on))
                .map(|(_, public)| Id::from_bytes(public).to_string())
                .collect();
            evidence.insert("restsOnAnswers".to_owned(), serde_json::json!(on));
        }
        let evidence = serde_json::Value::Object(evidence);
        let (undo, id) = insert_answer(
            write,
            request,
            "undo",
            None,
            Some(row),
            &evidence,
            &Before::default(),
        )?;
        for table in ["answer_value", "answer_rule"] {
            connection
                .prepare_cached(&format!(
                    "UPDATE {table} SET withdrawn_by_answer_id = ?2, rev = ?3
                     WHERE answer_id = ?1 AND withdrawn_by_answer_id IS NULL"
                ))?
                .execute((row, undo, write.rev()))?;
        }
        earlier.extend(&before.items);
        cleared.extend(redirects_of(connection, row)?);
        undos.push(undo);
        written.push((id, answer));
    }
    // Every place the files these undo were, and the rows they are
    // handed back freed of what an answer led them to.
    earlier.extend(seen);
    cleared.extend(redirects_into(connection, &hints)?);
    let prior_groups = prior_groups(&hints);
    let was = where_now(connection)?;
    let cleared = clear_redirects(write, &cleared)?;
    let Projected {
        redirects, shown, ..
    } = project(
        write,
        &was,
        &earlier,
        |_| {},
        |library| {
            for item in &mut library.items {
                if let Some(was) = hints.get(&item.id) {
                    item.prior = was.appearance;
                    item.prior_recording = Some(was.recording);
                }
            }
            library.prior_groups.clone_from(&prior_groups);
        },
    )?;
    for undo in &undos {
        keep_redirects(write, *undo, &redirects)?;
    }
    // A redirect cleared moves what a play counts for even when no file
    // moves.
    if cleared {
        crate::history_store::catalog_changed(write)?;
    }
    let shown = shown || cleared || settled;
    if !shown {
        write.changes_nothing_shown();
    }
    Ok((written, shown))
}

impl LibraryStore {
    /// Says yes to proposals, each with its candidate and the fingerprint
    /// it was shown, in one write. A proposal changed since is refused with
    /// [`Error::PlanStale`] and nothing is written. `finish` is given the
    /// revision, each answer's ID with its proposal's, and whether anything
    /// shown changed, and returns the result to store and answer with.
    pub fn confirm<F>(
        &self,
        request: WriteRequest,
        confirmations: Vec<Confirmation>,
        finish: F,
    ) -> Result<Written, Error>
    where
        F: FnOnce(i64, Vec<(Id, Id)>, bool) -> String + Send + 'static,
    {
        self.write(request, move |write| {
            let connection = write.connection();
            if confirmations.is_empty() {
                return Err(Error::Invalid("confirm at least one proposal"));
            }
            // A scan may have changed what the proposals rest on since they
            // were shown: they are worked out first, and checked against.
            if crate::evidence_store::layouts_are_stale(connection)? {
                crate::evidence_store::apply(write)?;
            }
            let settled = settle_owed(write)?;
            let mut seen = HashSet::new();
            let mut all = Vec::new();
            for confirmation in &confirmations {
                if !seen.insert(confirmation.proposal_id) {
                    return Err(Error::Invalid(
                        "a proposal is answered once, with one candidate",
                    ));
                }
                all.push(chosen(connection, confirmation)?);
            }
            // A batch's albums are said before its editions, which rest on
            // them, so that the ledger holds them in the order they could be
            // said one after another; each answer is still given back where
            // it was asked.
            let mut asked: Vec<(usize, Chosen)> = all.into_iter().enumerate().collect();
            asked.sort_by_key(|(_, one)| one.kind == "edition");
            let (positions, all): (Vec<usize>, Vec<Chosen>) = asked.into_iter().unzip();
            // Two answers that give one field of one file different values
            // cannot both be said.
            let each: Vec<Vec<Value>> = all
                .iter()
                .map(|one| one.values.iter().filter_map(StoredValue::value).collect())
                .collect();
            if crate::proposal_store::disagree(&each) {
                return Err(Error::Invalid("the answers disagree about a file"));
            }
            // An edition is said with or after the album of its discs:
            // each of its files is of that album by a yes standing, or by
            // one given here.
            for one in all.iter().filter(|one| one.kind == "edition") {
                for item in &one.items {
                    let here = all.iter().any(|other| {
                        other.values.iter().any(|value| {
                            value.item == *item
                                && value.field == "mb_release_group"
                                && value.value.as_deref() == Some(one.candidate.as_str())
                        })
                    });
                    let standing: bool = connection.query_row(
                        "SELECT EXISTS (SELECT 1 FROM answer_value
                                        WHERE source_item_id = ?1 AND field = 'mb_release_group'
                                          AND value = ?2 AND withdrawn_by_answer_id IS NULL)",
                        (item, &one.candidate),
                        |row| row.get(0),
                    )?;
                    if !here && !standing {
                        return Err(Error::Invalid(
                            "an edition is said with or after the album of its discs",
                        ));
                    }
                }
            }
            // Worked out with the values said here above those in force.
            // Two answers that give a file the same values give them once.
            let mut said: Vec<Value> = all
                .iter()
                .flat_map(|one| &one.values)
                .filter_map(StoredValue::value)
                .collect();
            said.sort();
            said.dedup();
            // What each yes puts together, from where its files are now and
            // with the values said here, so answers given together join
            // what they would one after another: worked out from the
            // library the projector's first pass loads, before anything
            // moves, and kept for the passes after it.
            let joins: std::cell::RefCell<Option<Vec<Join>>> = std::cell::RefCell::new(None);
            // Said with an edition, an album's yes joins only the releases
            // its own values would put together: the release each file is
            // on with the albums' values alone.
            let albums_said: Vec<Value> = all
                .iter()
                .filter(|one| one.kind == "album")
                .flat_map(|one| &one.values)
                .filter_map(StoredValue::value)
                .collect();
            let with_edition = all.iter().any(|one| one.kind == "edition");
            let album_keys: std::cell::RefCell<Option<HashMap<i64, String>>> =
                std::cell::RefCell::new(None);
            let was = where_now(connection)?;
            let Projected {
                moved,
                redirects,
                shown,
            } = project(
                write,
                &was,
                &[],
                |library| {
                    if with_edition && album_keys.borrow().is_none() {
                        let mut alone = library.clone();
                        values::apply(&mut alone, &albums_said);
                        *album_keys.borrow_mut() = Some(values::release_keys(&alone));
                    }
                    values::apply(library, &said);
                    let mut known = joins.borrow_mut();
                    let these = known.get_or_insert_with(|| {
                        all.iter()
                            .map(|one| {
                                values::join_of(library, &one.items, &one.onto, &one.candidate)
                            })
                            .collect()
                    });
                    library.answer_joins.extend(these.iter().cloned());
                },
                |_| {},
            )?;
            // A yes keeps as joined only the releases it put together: two
            // its files' own release IDs keep apart are not, so a retag
            // later does not join them by it.
            let now = where_now(connection)?;
            let joins: Vec<Join> = joins
                .into_inner()
                .unwrap_or_default()
                .into_iter()
                .zip(&all)
                .map(|(mut join, one)| {
                    let files = one.items.iter().chain(&one.onto).copied();
                    join.releases = put_together(&join.releases, files.clone(), &was, &now);
                    if let (Some(keys), "album") = (album_keys.borrow().as_ref(), one.kind.as_str())
                    {
                        join.releases = alike(&join.releases, files, &was, keys);
                    }
                    join
                })
                .collect();
            let shown = shown || settled;
            if !shown {
                write.changes_nothing_shown();
            }
            let mut written = Vec::new();
            let mut answered = Vec::new();
            let moved_with = moved_with(&all, &moved, &was, &now);
            // Where every file the write moved was, for a redirect no
            // answer's files lead to.
            let all_items: Vec<Was> = moved.values().copied().collect();
            for ((one, join), theirs) in all.iter().zip(&joins).zip(&moved_with) {
                // Its own files, and the other files the write moved with
                // it, and the redirects of rows they were on.
                let mut items: BTreeMap<i64, Was> = one
                    .items
                    .iter()
                    .filter_map(|item| was.get(item).map(|was| (*item, *was)))
                    .collect();
                items.extend(
                    theirs
                        .iter()
                        .filter_map(|item| moved.get(item).map(|was| (*item, *was))),
                );
                let redirects = redirects_of_rows(&redirects, items.values(), &all_items);
                let before = Before {
                    items: items.into_values().collect(),
                };
                let (row, id) = insert_answer(
                    write,
                    &request,
                    "confirm",
                    Some(one.answering()),
                    None,
                    &one.evidence,
                    &before,
                )?;
                keep_redirects(write, row, &redirects)?;
                keep_join(write, row, join)?;
                for value in &one.values {
                    connection
                        .prepare_cached(
                            "INSERT INTO answer_value
                               (answer_id, source_item_id, field, ordinal, value, origin, rev)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                        )?
                        .execute((
                            row,
                            value.item,
                            &value.field,
                            value.ordinal,
                            &value.value,
                            &value.origin,
                            write.rev(),
                        ))?;
                }
                written.push((id, Id::from_bytes(one.proposal)));
                answered.push(row);
            }
            // A yes rests on the answers it took from: one that took its
            // album's title or credit from the disc it joins its own onto,
            // which an answer gave them, rests on that answer, and an
            // edition on the yeses to its discs' album and, for discs
            // already said, to the release they were said to be. Undoing
            // one undoes it.
            for (one, row) in all.iter().zip(&answered) {
                let edition_files: Vec<i64>;
                let (items, sql) = if one.kind == "edition" {
                    edition_files = one.items.iter().chain(&one.onto).copied().collect();
                    (
                        &edition_files,
                        "SELECT DISTINCT answer_id FROM answer_value
                         WHERE source_item_id = ?1 AND withdrawn_by_answer_id IS NULL
                           AND field IN ('mb_release_group', 'mb_release')",
                    )
                } else if one.values.iter().any(|value| value.origin == "library") {
                    (
                        &one.onto,
                        "SELECT DISTINCT answer_id FROM answer_value
                         WHERE source_item_id = ?1 AND withdrawn_by_answer_id IS NULL
                           AND field IN ('album', 'album_artist', 'mb_album_artist')",
                    )
                } else {
                    continue;
                };
                let mut on: BTreeSet<i64> = BTreeSet::new();
                for item in items {
                    on.extend(
                        connection
                            .prepare_cached(sql)?
                            .query_map([item], |found| found.get::<_, i64>(0))?
                            .collect::<Result<Vec<_>, _>>()?,
                    );
                }
                on.remove(row);
                for depends_on in on {
                    connection
                        .prepare_cached(
                            "INSERT INTO answer_dependency (answer_id, depends_on_answer_id, rev)
                             VALUES (?1, ?2, ?3)",
                        )?
                        .execute((row, depends_on, write.rev()))?;
                }
            }
            crate::proposal_store::derive_proposals(write)?;
            let mut in_order: Vec<(usize, (Id, Id))> = positions.into_iter().zip(written).collect();
            in_order.sort_by_key(|(at, _)| *at);
            let written = in_order.into_iter().map(|(_, said)| said).collect();
            Ok(finish(write.rev(), written, shown))
        })
    }

    /// Undoes answers, in one write, with every standing answer that rests
    /// on one of them: an undo answer for each, its values withdrawn, the
    /// redirects it set cleared, and each file handed the rows it was on
    /// before (see [`handed`]). `finish` is given the revision, each undo's
    /// ID with the answer it undid, and whether anything shown changed.
    pub fn undo<F>(
        &self,
        request: WriteRequest,
        answers: Vec<Id>,
        finish: F,
    ) -> Result<Written, Error>
    where
        F: FnOnce(i64, Vec<(Id, Id)>, bool) -> String + Send + 'static,
    {
        let asked_by = request;
        self.write(request, move |write| {
            let (written, shown) = undo_in(write, &asked_by, &answers)?;
            crate::proposal_store::derive_proposals(write)?;
            Ok(finish(write.rev(), written, shown))
        })
    }

    /// `corrections.decline`: not this candidate for a proposal's files, or
    /// ask no more about them, in one write, refused if the proposal has
    /// changed since it was shown. Nothing shown changes; the proposals are
    /// worked out again.
    pub fn decline<F>(
        &self,
        request: WriteRequest,
        params: DeclineParams,
        finish: F,
    ) -> Result<Written, Error>
    where
        F: FnOnce(i64, Vec<(Id, Id)>, bool) -> String + Send + 'static,
    {
        self.write(request, move |write| {
            let connection = write.connection();
            // As a yes does, against what a scan may have changed since the
            // proposal was shown.
            if crate::evidence_store::layouts_are_stale(connection)? {
                crate::evidence_store::apply(write)?;
            }
            let found: Option<(i64, String, String)> = connection
                .query_row(
                    "SELECT id, fingerprint, kind FROM proposal WHERE public_id = ?1",
                    [params.proposal_id.as_bytes()],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()?;
            let (row, fingerprint, kind) = found.ok_or(Error::NotFound("no such proposal"))?;
            if fingerprint != params.fingerprint {
                return Err(Error::PlanStale);
            }
            let mut shown: Option<(Option<String>, String, String)> = None;
            let candidate = if params.stop_asking {
                if params.release_group.is_some() || params.release.is_some() {
                    return Err(Error::Invalid(
                        "say no to a candidate, or stop asking, not both",
                    ));
                }
                None
            } else {
                if params.release_group.is_none() && params.release.is_none() {
                    return Err(Error::Invalid("name the candidate to say no to"));
                }
                // An album's candidate by its release group; an edition's
                // by its release, its group or not.
                let offered: Option<(Option<String>, String, String)> = connection
                    .query_row(
                        "SELECT title, artist, release_group_mbid FROM proposal_candidate
                         WHERE proposal_id = ?1 AND (?2 IS NULL OR release_group_mbid = ?2)
                           AND release_mbid IS ?3",
                        (row, &params.release_group, &params.release),
                        |found| Ok((found.get(0)?, found.get(1)?, found.get(2)?)),
                    )
                    .optional()?;
                let (title, artist, group) =
                    offered.ok_or(Error::NotFound("no such candidate"))?;
                shown = Some((title, artist, group.clone()));
                // An edition's candidate is its release.
                Some(params.release.clone().unwrap_or(group))
            };
            let items: Vec<i64> = connection
                .prepare_cached(
                    "SELECT source_item_id FROM proposal_item WHERE proposal_id = ?1 ORDER BY source_item_id",
                )?
                .query_map([row], |found| found.get(0))?
                .collect::<Result<_, _>>()?;
            let verb = if params.stop_asking {
                "stop_asking"
            } else {
                "decline"
            };
            // The candidate said no to, as it was shown; none for a stop.
            let evidence = serde_json::json!({
                "kind": kind,
                "candidate": shown.map(|(title, artist, group)| serde_json::json!({
                    "releaseGroup": group,
                    "release": params.release,
                    "title": title,
                    "artist": artist,
                })),
                "files": items.len(),
            });
            let answering = Answering {
                kind: &kind,
                proposal: *params.proposal_id.as_bytes(),
                fingerprint: &fingerprint,
                candidate: candidate.as_deref(),
            };
            let (answer, id) = insert_answer(
                write,
                &request,
                verb,
                Some(answering),
                None,
                &evidence,
                &Before::default(),
            )?;
            if params.stop_asking {
                keep_rule(write, answer, "stop_asking", None, Some(&kind), [&items, &[]])?;
            } else {
                keep_rule(write, answer, "not_candidate", candidate.as_deref(), None, [&items, &[]])?;
            }
            write.changes_nothing_shown();
            crate::proposal_store::derive_proposals(write)?;
            Ok(finish(write.rev(), vec![(id, params.proposal_id)], false))
        })
    }

    /// `corrections.keepApart`: the files on two releases now are never on
    /// one release or in one release group, in one write. Where the
    /// projector had them together, they part, and the rows they leave
    /// lead to where they went.
    pub fn keep_apart<F>(
        &self,
        request: WriteRequest,
        releases: Vec<Id>,
        finish: F,
    ) -> Result<Written, Error>
    where
        F: FnOnce(i64, Id, bool) -> String + Send + 'static,
    {
        self.write(request, move |write| {
            let connection = write.connection();
            let [one, other] = releases.as_slice() else {
                return Err(Error::Invalid("name two releases to keep apart"));
            };
            let settled = settle_owed(write)?;
            // Each release's live files, and the group chosen for it, if any.
            let files_of = |release: &Id| -> Result<(Vec<i64>, Option<i64>), Error> {
                let row: Option<(i64, Option<i64>)> = connection
                    .query_row(
                        "SELECT rel.id, rel.chosen_release_group_id FROM release rel
                         WHERE rel.id = (SELECT canonical_id FROM release WHERE public_id = ?1)",
                        [release.as_bytes()],
                        |found| Ok((found.get(0)?, found.get(1)?)),
                    )
                    .optional()?;
                let (row, chosen) = row.ok_or(Error::NotFound("no such release"))?;
                let files = connection
                    .prepare_cached(
                        "SELECT a.source_item_id FROM assignment a
                         JOIN source_item si ON si.id = a.source_item_id
                         JOIN track_appearance ta ON ta.id = a.appearance_id
                         JOIN release rel ON rel.id = ta.release_id
                         WHERE a.superseded_at_ms IS NULL AND rel.canonical_id = ?1
                           AND si.item_kind = 'file' AND si.retired_at_ms IS NULL
                         ORDER BY a.source_item_id",
                    )?
                    .query_map([row], |found| found.get(0))?
                    .collect::<Result<_, _>>()?;
                Ok((files, chosen))
            };
            let ((left, left_chosen), (right, right_chosen)) = (files_of(one)?, files_of(other)?);
            if left.is_empty() || right.is_empty() || left == right {
                return Err(Error::Invalid(
                    "keep apart two releases that each have files of their own",
                ));
            }
            // A group chosen for both holds them together whatever a no
            // says, so the no would be one that does nothing.
            if left_chosen.is_some() && left_chosen == right_chosen {
                return Err(Error::Invalid(
                    "the two releases are of a group chosen for both, so they stay together",
                ));
            }
            // Worked out with the pair kept apart, then kept as an answer
            // with where each file it moved was, for its undo.
            let was = where_now(connection)?;
            let pair = (left.clone(), right.clone());
            let Projected {
                moved,
                redirects,
                shown,
            } = project(
                write,
                &was,
                &[],
                |library| library.noes.apart.push(pair.clone()),
                |_| {},
            )?;
            let before = Before {
                items: moved.into_values().collect(),
            };
            let evidence = serde_json::json!({
                "releases": [one.to_string(), other.to_string()],
                "files": [left.len(), right.len()],
            });
            let (answer, id) =
                insert_answer(write, &request, "apart", None, None, &evidence, &before)?;
            keep_rule(write, answer, "apart", None, Some("album"), [&left, &right])?;
            keep_redirects(write, answer, &redirects)?;
            let shown = shown || settled;
            if !shown {
                write.changes_nothing_shown();
            }
            crate::proposal_store::derive_proposals(write)?;
            Ok(finish(write.rev(), id, shown))
        })
    }

    /// `corrections.answers`: one page of the ledger, newest first.
    pub fn answers(&self, params: AnswersParams) -> Result<AnswerList, Error> {
        self.read_snapshot(move |connection| {
            let revision = revision(connection)?;
            let before: Option<i64> = match params.before {
                Some(answer) => Some(
                    connection
                        .query_row(
                            "SELECT id FROM answer WHERE public_id = ?1",
                            [answer.as_bytes()],
                            |row| row.get(0),
                        )
                        .optional()?
                        .ok_or(Error::NotFound("no such answer"))?,
                ),
                None => None,
            };
            let limit = params
                .limit
                .unwrap_or(ANSWERS_PAGE)
                .clamp(1, ANSWERS_PAGE_MAX);
            // Read one past the page, to know whether there is more. A
            // cursor seeks by the key, however deep the page.
            let more = i64::from(limit) + 1;
            let mut listed: Vec<(Answer, String)> = match before {
                Some(before) => connection
                    .prepare_cached(&format!(
                        "{ANSWER_SELECT} WHERE a.id < ?1 ORDER BY a.id DESC LIMIT ?2"
                    ))?
                    .query_map((before, more), answer_row)?
                    .collect::<Result<_, _>>()?,
                None => connection
                    .prepare_cached(&format!("{ANSWER_SELECT} ORDER BY a.id DESC LIMIT ?1"))?
                    .query_map([more], answer_row)?
                    .collect::<Result<_, _>>()?,
            };
            let next = if listed.len() > usize::try_from(limit).unwrap_or(usize::MAX) {
                listed.pop();
                listed.last().map(|(answer, _)| answer.answer_id)
            } else {
                None
            };
            Ok(AnswerList {
                library_revision: revision,
                answers: listed.into_iter().map(|(answer, _)| answer).collect(),
                next,
            })
        })
    }

    /// `corrections.answer`: one answer, with what it rested on and said,
    /// as it was kept when it was given.
    pub fn answer(&self, id: Id) -> Result<AnswerGot, Error> {
        self.read_snapshot(move |connection| {
            let revision = revision(connection)?;
            let (answer, evidence) = connection
                .prepare_cached(&format!("{ANSWER_SELECT} WHERE a.public_id = ?1"))?
                .query_row([id.as_bytes()], answer_row)
                .optional()?
                .ok_or(Error::NotFound("no such answer"))?;
            let evidence: Kept = serde_json::from_str(&evidence)?;
            let paths = crate::proposal_store::paths_of(
                connection,
                evidence.values.iter().map(|value| value.item),
            )?;
            let methods = |found: &serde_json::Value| -> Vec<LayoutMethod> {
                found
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|way| way.get("method")?.as_str())
                    .filter_map(crate::evidence_store::method)
                    .collect()
            };
            Ok(AnswerGot {
                library_revision: revision,
                answer,
                evidence: AnswerEvidence {
                    candidate: evidence.candidate.map(|candidate| AnsweredCandidate {
                        release_group: candidate.release_group,
                        release: candidate.release,
                        title: candidate.title,
                        artist: candidate.artist,
                        primary_type: candidate.primary_type,
                        first_date: candidate.first_date,
                        releases: candidate.releases,
                    }),
                    candidates: evidence
                        .candidates
                        .into_iter()
                        .map(|shown| AnsweredCandidate {
                            release_group: shown.release_group,
                            release: shown.release,
                            title: shown.title,
                            artist: None,
                            primary_type: None,
                            first_date: None,
                            releases: None,
                        })
                        .collect(),
                    replies: evidence
                        .rests_on
                        .into_iter()
                        .map(|rest| AnsweredReply {
                            disc_id: rest.disc_id,
                            replied_at_ms: unsigned(rest.replied_at_ms),
                        })
                        .collect(),
                    layouts: evidence
                        .layouts
                        .into_iter()
                        .map(|layout| AnsweredLayout {
                            methods: methods(&layout.found),
                            disc_id: layout.disc_id,
                            folder: layout.folder,
                            provisional: layout.provisional,
                        })
                        .collect(),
                    values: evidence
                        .values
                        .into_iter()
                        .map(|value| {
                            crate::proposal_store::file_value(
                                &paths,
                                value.item,
                                value.field,
                                value.ordinal,
                                value.value,
                                None,
                            )
                        })
                        .collect(),
                    rests_on: evidence
                        .rests_on_answers
                        .iter()
                        .filter_map(|text| text.parse().ok())
                        .collect(),
                },
            })
        })
    }
}
