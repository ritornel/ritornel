//! `library.stats`: what the library holds and what the scans made of it.
//! Until there is anything to browse with, this is how the owner sees what
//! a scan produced.
//!
//! An entity is counted only if a file that is not retired is bound to it:
//! a recording or an appearance that every file has left is still a row,
//! and is not counted.

use ritornel_contract::{Count, LibraryStats};
use rusqlite::Connection;

use crate::{Error, LibraryStore};

/// Files that are not retired and are bound, and what they are bound to.
const LIVE: &str = "
WITH live AS (
  SELECT a.source_item_id AS item, a.recording_id AS recording, a.appearance_id AS appearance,
         a.decision_id AS decision
  FROM assignment a JOIN source_item si ON si.id = a.source_item_id
  WHERE a.superseded_at_ms IS NULL AND si.retired_at_ms IS NULL AND si.item_kind = 'file'),
apps AS (
  SELECT DISTINCT ta.canonical_id AS id FROM live JOIN track_appearance ta ON ta.id = live.appearance),
rels AS (
  SELECT DISTINCT rel.canonical_id AS id FROM apps
  JOIN track_appearance ta ON ta.id = apps.id JOIN release rel ON rel.id = ta.release_id),
grps AS (
  SELECT DISTINCT rel.release_group_id AS id FROM rels JOIN release rel ON rel.id = rels.id)";

fn count(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

fn counts(connection: &Connection, sql: &str) -> Result<Vec<Count>, Error> {
    let mut statement = connection.prepare(sql)?;
    let rows = statement.query_map([], |row| {
        Ok(Count {
            name: row
                .get::<_, Option<String>>(0)?
                .unwrap_or_else(|| "unknown".to_owned()),
            count: count(row.get(1)?),
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn stats(connection: &Connection) -> Result<LibraryStats, Error> {
    let transaction = connection.unchecked_transaction()?;
    let connection: &Connection = &transaction;
    let one = |sql: &str| -> Result<u64, Error> {
        Ok(count(
            connection
                .query_row(sql, [], |row| row.get::<_, Option<i64>>(0))?
                .unwrap_or(0),
        ))
    };
    let live = |select: &str| one(&format!("{LIVE} {select}"));
    const READABLE: &str =
        "FROM source_item si JOIN source_item_revision sr ON sr.id = si.current_revision_id
        WHERE si.item_kind = 'file' AND si.retired_at_ms IS NULL";
    Ok(LibraryStats {
        library_revision: one("SELECT rev FROM library_meta WHERE id = 1")?,
        files: one("SELECT count(*) FROM source_item WHERE item_kind = 'file' AND retired_at_ms IS NULL")?,
        file_bytes: one(
            "SELECT sum(size_bytes) FROM source_item WHERE item_kind = 'file' AND retired_at_ms IS NULL",
        )?,
        files_missing: one(
            "SELECT count(*) FROM source_item
             WHERE item_kind = 'file' AND retired_at_ms IS NULL AND missing_since_ms IS NOT NULL",
        )?,
        files_unreadable: one(&format!("SELECT count(*) {READABLE} AND sr.audio_asset_id IS NULL"))?,
        unreadable_reasons: counts(
            connection,
            &format!(
                "SELECT json_extract(sr.payload_json, '$.error.kind') || ': '
                        || json_extract(sr.payload_json, '$.error.message'), count(*)
                 {READABLE} AND sr.audio_asset_id IS NULL GROUP BY 1 ORDER BY 2 DESC, 1 LIMIT 8"
            ),
        )?,
        files_retired: one("SELECT count(*) FROM source_item WHERE item_kind = 'file' AND retired_at_ms IS NOT NULL")?,
        codecs: counts(
            connection,
            "SELECT aa.codec, count(*) FROM source_item si
             JOIN source_item_revision sr ON sr.id = si.current_revision_id
             JOIN audio_asset aa ON aa.id = sr.audio_asset_id
             WHERE si.item_kind = 'file' AND si.retired_at_ms IS NULL GROUP BY 1 ORDER BY 2 DESC, 1",
        )?,
        files_with_musicbrainz_id: one(
            "SELECT count(DISTINCT c.source_item_id) FROM identifier_claim c
             JOIN source_item si ON si.id = c.source_item_id
             WHERE c.namespace LIKE 'mb\\_%' ESCAPE '\\' AND si.retired_at_ms IS NULL",
        )?,
        unresolved: one(&format!(
            "SELECT count(*) {READABLE} AND sr.audio_asset_id IS NOT NULL AND si.current_recording_id IS NULL"
        ))?,
        artists: live(
            "SELECT count(DISTINCT n.artist_id) FROM artist_credit_name n WHERE n.artist_credit_id IN (
               SELECT ta.display_artist_credit_id FROM apps JOIN track_appearance ta ON ta.id = apps.id
               UNION SELECT rel.display_artist_credit_id FROM rels JOIN release rel ON rel.id = rels.id)",
        )?,
        release_groups: live("SELECT count(*) FROM grps")?,
        releases: live("SELECT count(*) FROM rels")?,
        recordings: live("SELECT count(DISTINCT r.canonical_id) FROM live JOIN recording r ON r.id = live.recording")?,
        appearances: live("SELECT count(*) FROM apps")?,
        loose_tracks: live(
            "SELECT count(*) FROM apps JOIN track_appearance ta ON ta.id = apps.id WHERE ta.release_id IS NULL",
        )?,
        decisions: counts(
            connection,
            &format!(
                "{LIVE} SELECT d.method, count(*) FROM live JOIN identity_decision d ON d.id = live.decision
                 GROUP BY 1 ORDER BY 2 DESC, 1"
            ),
        )?,
        needs_review: live("SELECT sum(d.needs_review) FROM live JOIN identity_decision d ON d.id = live.decision")?,
        groups_with_kind: live(
            "SELECT count(*) FROM grps JOIN release_group g ON g.id = grps.id WHERE g.display_primary_type IS NOT NULL",
        )?,
        kinds: counts(
            connection,
            &format!(
                "{LIVE} SELECT g.display_primary_type, count(*) FROM grps JOIN release_group g ON g.id = grps.id
                 WHERE g.display_primary_type IS NOT NULL GROUP BY 1 ORDER BY 2 DESC, 1"
            ),
        )?,
        groups_with_first_year: live(
            "SELECT count(*) FROM grps JOIN release_group g ON g.id = grps.id WHERE g.display_first_year IS NOT NULL",
        )?,
        // From a tag: a file of one of its releases carries an original
        // date that holds a year.
        groups_with_first_year_from_tag: live(
            "SELECT count(DISTINCT rel.release_group_id) FROM metadata_claim c
             JOIN live ON live.item = c.source_item_id
             JOIN track_appearance ta ON ta.id = live.appearance
             JOIN release rel ON rel.id = ta.release_id
             JOIN release_group g ON g.id = rel.release_group_id
             WHERE c.field = 'original_date' AND c.value GLOB '[12][0-9][0-9][0-9]*'
               AND g.display_first_year IS NOT NULL",
        )?,
        pictures: one("SELECT count(*) FROM picture")?,
        releases_with_cover: live(
            "SELECT count(*) FROM rels JOIN release rel ON rel.id = rels.id WHERE rel.display_picture_id IS NOT NULL",
        )?,
    })
}

impl LibraryStore {
    /// What the library holds, counted in one read transaction.
    pub fn stats(&self) -> Result<LibraryStats, Error> {
        self.read(stats)
    }
}
