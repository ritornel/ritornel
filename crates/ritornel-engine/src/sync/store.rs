//! Where a sync's records are kept. See "Storage" in
//! docs/internals/device-sync.md.
//!
//! Everything a sync must not lose is in `library.db`, so that it follows
//! the library when the hub moves, one transaction covers a baseline and
//! the action that earned it, and one backup holds both. A folder's path
//! on its own machine is the one thing in `server.db`.
//!
//! A run's own bookkeeping writes say they change nothing shown, so the
//! library's subscribers are not told of each window of a sync.

use ritornel_contract::{
    SyncActionKind, SyncDeviceKind, SyncFolderKind, SyncHow, SyncMadeFor, SyncOnConnect,
    SyncOutcome, SyncPlaylistKind, SyncSelector, SyncSetup, SyncSourceRole, SyncSourceSpec,
    SyncStep, SyncUnit, SyncWhenFull,
};
use ritornel_device::{RelPath, Transport};
use rusqlite::{Connection, OptionalExtension, params};

use super::plan::{Action, Baseline, Plan, ProjectionKey, Shown};
use super::policy::{FormatPolicy, Settings};
use super::select::{self, Matcher, SourceRow, TrackRow};
use crate::library_store::Write;
use crate::transcode::{Codec, GainTag, GainTags};
use crate::{Error, Id, LibraryStore, ServerStore, WriteRequest, Written};

/// The writer a run's own bookkeeping is logged as.
pub const RUN_METHOD: &str = "devices.run";

/// One target as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetRow {
    pub(crate) key: i64,
    pub id: Id,
    pub kind: SyncDeviceKind,
    pub family: String,
    pub transport: Transport,
    pub name: String,
    pub icon_key: Option<String>,
    pub setup: SyncSetup,
    pub settings: Settings,
    /// The write speed last measured.
    pub write_bps: Option<u64>,
    /// Its capacity and free bytes as last read, and when.
    pub seen: Option<(u64, u64, i64)>,
    /// The server it is attached to, and for a folder its root there.
    pub host: Option<Id>,
    pub root: Option<Id>,
    /// The lifetime of its storage that is open, and the root ID its
    /// marker holds.
    pub epoch: Option<(i64, Id)>,
    pub created_at_ms: i64,
    /// The library revision it last changed at.
    pub rev: i64,
}

fn unsigned(value: Option<i64>) -> Option<u64> {
    value.and_then(|value| u64::try_from(value).ok())
}

fn signed(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn id_at(row: &rusqlite::Row<'_>, at: usize) -> rusqlite::Result<Option<Id>> {
    Ok(row.get::<_, Option<[u8; 16]>>(at)?.map(Id::from_bytes))
}

const TARGET_COLUMNS: &str = "t.id, t.public_id, t.kind, t.family, t.name, t.icon_key, t.setup,
    t.format_json, t.cover_px, t.id3v1, t.music_root, t.reserve_bytes, t.limit_bytes, t.when_full,
    t.on_connect, t.read_plays, t.scrobble_plays, t.remind_after_days, t.write_bps,
    t.seen_capacity_bytes, t.seen_free_bytes, t.seen_at_ms, t.created_at_ms, t.rev,
    i.transport, hs.public_id, i.root_pid, t.made_for, t.folder_kind, t.codecs_json,
    t.playlist_kind,
    (SELECT e.epoch FROM sync_target_epoch e WHERE e.target_id = t.id AND e.ended_at_ms IS NULL),
    (SELECT e.marker_pid FROM sync_target_epoch e WHERE e.target_id = t.id AND e.ended_at_ms IS NULL)";

const TARGET_JOINS: &str = "FROM sync_target t
    LEFT JOIN sync_target_identity i ON i.id =
      (SELECT min(id) FROM sync_target_identity WHERE target_id = t.id)
    LEFT JOIN server hs ON hs.id = i.host_server_id";

fn target(row: &rusqlite::Row<'_>) -> rusqlite::Result<(TargetRow, String)> {
    let text = |at: usize| row.get::<_, String>(at);
    let seen = match (
        unsigned(row.get(19)?),
        unsigned(row.get(20)?),
        row.get::<_, Option<i64>>(21)?,
    ) {
        (Some(capacity), Some(free), Some(at)) => Some((capacity, free, at)),
        _ => None,
    };
    let epoch = match (row.get::<_, Option<i64>>(31)?, id_at(row, 32)?) {
        (Some(epoch), Some(marker)) => Some((epoch, marker)),
        _ => None,
    };
    Ok((
        TargetRow {
            key: row.get(0)?,
            id: Id::from_bytes(row.get(1)?),
            kind: match text(2)?.as_str() {
                "player" => SyncDeviceKind::Player,
                _ => SyncDeviceKind::Folder,
            },
            family: text(3)?,
            name: text(4)?,
            icon_key: row.get(5)?,
            setup: match text(6)?.as_str() {
                "ready" => SyncSetup::Ready,
                "ignored" => SyncSetup::Ignored,
                _ => SyncSetup::New,
            },
            settings: Settings {
                format: FormatPolicy::default(),
                cover_px: u32::try_from(row.get::<_, i64>(8)?).unwrap_or(0),
                id3v1: row.get(9)?,
                music_root: text(10)?,
                reserve_bytes: unsigned(row.get(11)?),
                limit_bytes: unsigned(row.get(12)?),
                folder: match (
                    row.get::<_, Option<String>>(27)?
                        .as_deref()
                        .and_then(made_for_named),
                    row.get::<_, Option<String>>(28)?,
                ) {
                    (Some(made_for), Some(kind)) => Some((
                        made_for,
                        match kind.as_str() {
                            "removable" => SyncFolderKind::Removable,
                            _ => SyncFolderKind::AlwaysHere,
                        },
                    )),
                    _ => None,
                },
                codecs: serde_json::from_str::<Vec<String>>(&text(29)?)
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|codec| Codec::named(codec))
                    .collect(),
                playlists: match text(30)?.as_str() {
                    "m3u" => SyncPlaylistKind::M3u,
                    _ => SyncPlaylistKind::M3u8,
                },
                when_full: match text(13)?.as_str() {
                    "fill" => SyncWhenFull::Fill,
                    _ => SyncWhenFull::Ask,
                },
                on_connect: match text(14)?.as_str() {
                    "sync" => SyncOnConnect::Sync,
                    _ => SyncOnConnect::Nothing,
                },
                read_plays: row.get(15)?,
                scrobble_plays: row.get(16)?,
                remind_after_days: u32::try_from(row.get::<_, i64>(17)?).unwrap_or(0),
            },
            write_bps: unsigned(row.get(18)?),
            seen,
            created_at_ms: row.get(22)?,
            rev: row.get(23)?,
            transport: match row.get::<_, Option<String>>(24)?.as_deref() {
                Some("mass_storage") => Transport::MassStorage,
                Some("mtp") => Transport::Mtp,
                _ => Transport::Folder,
            },
            host: id_at(row, 25)?,
            root: id_at(row, 26)?,
            epoch,
        },
        text(7)?,
    ))
}

fn targets(connection: &Connection, only: Option<Id>) -> Result<Vec<TargetRow>, Error> {
    let mut statement = connection.prepare_cached(&format!(
        "SELECT {TARGET_COLUMNS} {TARGET_JOINS}
         WHERE t.setup <> 'forgotten' AND (?1 IS NULL OR t.public_id = ?1)
         ORDER BY t.id"
    ))?;
    let rows = statement.query_map([only.map(|id| id.as_bytes().to_vec())], target)?;
    rows.map(|row| {
        let (mut target, format) = row?;
        target.settings.format = FormatPolicy::from_json(&format)?;
        Ok(target)
    })
    .collect()
}

fn target_key(connection: &Connection, id: Id) -> Result<i64, Error> {
    connection
        .query_row(
            "SELECT id FROM sync_target WHERE public_id = ?1 AND setup <> 'forgotten'",
            [id.as_bytes()],
            |row| row.get(0),
        )
        .optional()?
        .ok_or(Error::NotFound("no such device"))
}

fn made_for_named(name: &str) -> Option<SyncMadeFor> {
    Some(match name {
        "car_stereo" => SyncMadeFor::CarStereo,
        "phone" => SyncMadeFor::Phone,
        "other_player" => SyncMadeFor::OtherPlayer,
        "plain_copy" => SyncMadeFor::PlainCopy,
        _ => return None,
    })
}

fn made_for_text(made_for: SyncMadeFor) -> &'static str {
    match made_for {
        SyncMadeFor::CarStereo => "car_stereo",
        SyncMadeFor::Phone => "phone",
        SyncMadeFor::OtherPlayer => "other_player",
        SyncMadeFor::PlainCopy => "plain_copy",
    }
}

fn folder_kind_text(kind: SyncFolderKind) -> &'static str {
    match kind {
        SyncFolderKind::Removable => "removable",
        SyncFolderKind::AlwaysHere => "always_here",
    }
}

fn playlists_text(kind: SyncPlaylistKind) -> &'static str {
    match kind {
        SyncPlaylistKind::M3u => "m3u",
        SyncPlaylistKind::M3u8 => "m3u8",
    }
}

fn codecs_json(codecs: &[Codec]) -> String {
    let names: Vec<&str> = codecs.iter().map(|codec| codec.extension()).collect();
    // A list of plain words always writes.
    serde_json::to_string(&names).unwrap_or_else(|_| "[]".to_owned())
}

fn role_text(role: SyncSourceRole) -> &'static str {
    match role {
        SyncSourceRole::Rule => "rule",
        SyncSourceRole::Always => "always",
        SyncSourceRole::LeaveOut => "leave_out",
    }
}

/// A selector as it is stored, with the version it was written in.
#[derive(serde::Serialize, serde::Deserialize)]
struct StoredSelector {
    v: u32,
    #[serde(flatten)]
    selector: SyncSelector,
}

fn insert_sources(write: &Write<'_>, target: i64, sources: &[SyncSourceSpec]) -> Result<(), Error> {
    let connection = write.connection();
    connection.execute("DELETE FROM sync_source WHERE target_id = ?1", [target])?;
    for (position, source) in sources.iter().enumerate() {
        let selector = serde_json::to_string(&StoredSelector {
            v: 1,
            selector: source.selector,
        })
        .map_err(Error::Json)?;
        connection.execute(
            "INSERT INTO sync_source (public_id, target_id, role, position, selector_json, unit, rev)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                write.new_id()?.as_bytes(),
                target,
                role_text(source.role),
                position as i64,
                selector,
                match source.unit {
                    SyncUnit::Albums => "albums",
                    SyncUnit::Tracks => "tracks",
                },
                write.rev(),
            ],
        )?;
    }
    Ok(())
}

fn sources(connection: &Connection, target: i64) -> Result<Vec<SourceRow>, Error> {
    let mut statement = connection.prepare_cached(
        "SELECT public_id, role, selector_json, unit FROM sync_source
         WHERE target_id = ?1 ORDER BY position",
    )?;
    let rows = statement.query_map([target], |row| {
        Ok((
            Id::from_bytes(row.get(0)?),
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;
    let mut found = Vec::new();
    for row in rows {
        let (id, role, selector, unit) = row?;
        let stored: StoredSelector = serde_json::from_str(&selector).map_err(Error::Json)?;
        let (label, detail, art) = select::label(connection, stored.selector)?;
        found.push(SourceRow {
            id,
            role: match role.as_str() {
                "always" => SyncSourceRole::Always,
                "leave_out" => SyncSourceRole::LeaveOut,
                _ => SyncSourceRole::Rule,
            },
            selector: stored.selector,
            unit: match unit.as_str() {
                "tracks" => SyncUnit::Tracks,
                _ => SyncUnit::Albums,
            },
            label,
            detail,
            art,
        });
    }
    Ok(found)
}

/// What `devices.set` changes of a target. Each field left out stays.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TargetChanges {
    pub name: Option<String>,
    /// `Some(None)` goes back to the family's drawing.
    pub icon_key: Option<Option<String>>,
    pub ignored: Option<bool>,
    pub sources: Option<Vec<SyncSourceSpec>>,
    pub format: Option<FormatPolicy>,
    pub cover_px: Option<u32>,
    pub id3v1: Option<bool>,
    /// `Some(None)` goes back to the default.
    pub reserve_bytes: Option<Option<u64>>,
    /// `Some(None)` takes the limit away.
    pub limit_bytes: Option<Option<u64>>,
    pub when_full: Option<SyncWhenFull>,
    pub on_connect: Option<SyncOnConnect>,
    /// For a folder, what it is made for: it sets the codecs, the cover's
    /// size, the playlist kind and the ID3v1 trailer to that purpose's
    /// own, where the same change does not say otherwise.
    pub made_for: Option<SyncMadeFor>,
    pub folder_kind: Option<SyncFolderKind>,
    pub codecs: Option<Vec<Codec>>,
    pub playlists: Option<SyncPlaylistKind>,
}

/// One run as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRow {
    pub(crate) key: i64,
    pub id: Id,
    pub job: Option<Id>,
    pub epoch: i64,
    pub plan_revision: String,
    pub step: SyncStep,
    pub adopted: bool,
    pub outcome: Option<SyncOutcome>,
    pub error: Option<String>,
    pub actions_total: u64,
    pub actions_done: u64,
    pub actions_failed: u64,
    pub copied: u64,
    pub removed: u64,
    pub bytes_total: u64,
    pub bytes_done: u64,
    pub started_at_ms: i64,
    pub ended_at_ms: Option<i64>,
}

const RUN_COLUMNS: &str = "id, public_id, job_pid, epoch, plan_revision, step, adopted, outcome,
    error, actions_total, actions_done, actions_failed, copied, removed, bytes_total, bytes_done,
    started_at_ms, ended_at_ms";

fn run(row: &rusqlite::Row<'_>) -> rusqlite::Result<RunRow> {
    let count = |at: usize| Ok::<_, rusqlite::Error>(unsigned(row.get(at)?).unwrap_or(0));
    Ok(RunRow {
        key: row.get(0)?,
        id: Id::from_bytes(row.get(1)?),
        job: id_at(row, 2)?,
        epoch: row.get(3)?,
        plan_revision: row.get(4)?,
        step: match row.get::<_, String>(5)?.as_str() {
            "plan" => SyncStep::Plan,
            "adopt" => SyncStep::Adopt,
            "copy" => SyncStep::Copy,
            "finish" => SyncStep::Finish,
            _ => SyncStep::Read,
        },
        adopted: row.get(6)?,
        outcome: row
            .get::<_, Option<String>>(7)?
            .map(|outcome| match outcome.as_str() {
                "complete" => SyncOutcome::Complete,
                "cut_off" => SyncOutcome::CutOff,
                "failed_in_part" => SyncOutcome::FailedInPart,
                "stopped" => SyncOutcome::Stopped,
                _ => SyncOutcome::Failed,
            }),
        error: row.get(8)?,
        actions_total: count(9)?,
        actions_done: count(10)?,
        actions_failed: count(11)?,
        copied: count(12)?,
        removed: count(13)?,
        bytes_total: count(14)?,
        bytes_done: count(15)?,
        started_at_ms: row.get(16)?,
        ended_at_ms: row.get(17)?,
    })
}

fn outcome_text(outcome: SyncOutcome) -> &'static str {
    match outcome {
        SyncOutcome::Complete => "complete",
        SyncOutcome::CutOff => "cut_off",
        SyncOutcome::FailedInPart => "failed_in_part",
        SyncOutcome::Failed => "failed",
        SyncOutcome::Stopped => "stopped",
    }
}

fn step_text(step: SyncStep) -> &'static str {
    match step {
        SyncStep::Read => "read",
        SyncStep::Plan => "plan",
        SyncStep::Adopt => "adopt",
        SyncStep::Copy => "copy",
        SyncStep::Finish => "finish",
    }
}

fn kind_text(kind: SyncActionKind) -> &'static str {
    match kind {
        SyncActionKind::Add => "add",
        SyncActionKind::Replace => "replace",
        SyncActionKind::Remove => "remove",
        SyncActionKind::Conflict => "conflict",
        SyncActionKind::Blocked => "blocked",
    }
}

/// Where a stored action stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stored {
    /// Frozen, and not yet asked of the device.
    Planned,
    /// Written down as asked of the device, with no result recorded.
    Issued,
    Done,
    Failed,
    Conflict,
    Skipped,
}

/// One action of a run as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionRow {
    pub seq: i64,
    pub kind: SyncActionKind,
    pub reason: String,
    pub source: Option<Id>,
    pub shown: Shown,
    pub path: Option<String>,
    pub codec: Option<String>,
    pub how: Option<SyncHow>,
    pub bytes: u64,
    pub estimated: bool,
    pub state: Stored,
    pub error: Option<(String, String)>,
}

/// What a file a put left is known by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Left {
    pub path: RelPath,
    pub size: u64,
    pub sampled: [u8; 32],
    pub digest: [u8; 32],
}

/// What became of one action that was asked of the device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// It was carried out. An add or a replace says what it left.
    Done(Option<Left>),
    /// It failed, with a word for why and the whole reason.
    Failed(String, String),
    /// The device's file was not the one recorded. Nothing was touched.
    Conflict(String),
    /// Left for another day.
    Skipped,
}

/// One action's result, with what a baseline needs.
#[derive(Debug, Clone)]
pub struct Recorded {
    pub seq: i64,
    pub outcome: Outcome,
    /// The baseline the action replaced or removed, closed when it is done.
    pub old: Option<i64>,
    /// Whether the old file is known to be gone though the action failed:
    /// its baseline is closed all the same.
    pub old_gone: bool,
}

/// One action about to be asked of the device, and for one that writes,
/// what it must leave: the length, the digest and the sampled hash of its
/// prepared transfer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issued {
    pub seq: i64,
    pub leaves: Option<(u64, [u8; 32], [u8; 32])>,
}

impl LibraryStore {
    /// Every target that is not forgotten, in the order they were first
    /// seen.
    pub fn sync_targets(&self) -> Result<Vec<TargetRow>, Error> {
        self.read(|connection| targets(connection, None))
    }

    /// One target, or none.
    pub fn sync_target(&self, id: Id) -> Result<Option<TargetRow>, Error> {
        self.read(|connection| Ok(targets(connection, Some(id))?.into_iter().next()))
    }

    /// A target's sources, in order, each with its label.
    pub fn sync_sources(&self, target: &TargetRow) -> Result<Vec<SourceRow>, Error> {
        let key = target.key;
        self.read(move |connection| sources(connection, key))
    }

    /// Everything a plan reads of the library in one snapshot: every track
    /// the hub can make a file of, the target's sources with what each
    /// selects, and its open baselines.
    #[allow(clippy::type_complexity)]
    pub fn sync_inputs(
        &self,
        target: &TargetRow,
    ) -> Result<(Vec<TrackRow>, Vec<(SourceRow, Matcher)>, Vec<Baseline>, i64), Error> {
        let (key, hub, now) = (target.key, self.hub(), self.clock_now_ms());
        self.read_snapshot(move |connection| {
            let tracks = select::tracks(connection, hub)?;
            let sources = sources(connection, key)?
                .into_iter()
                .map(|source| {
                    let matcher = select::matcher(connection, source.selector, now)?;
                    Ok((source, matcher))
                })
                .collect::<Result<Vec<_>, Error>>()?;
            let rev: i64 =
                connection.query_row("SELECT rev FROM library_meta WHERE id = 1", [], |row| {
                    row.get(0)
                })?;
            Ok((tracks, sources, baselines(connection, key)?, rev))
        })
    }

    /// The files in a target's music root that Ritornel did not write, as
    /// the last inventory a run recorded found them.
    pub fn sync_unmanaged(
        &self,
        target: &TargetRow,
    ) -> Result<Vec<(String, u64, Option<i64>)>, Error> {
        let key = target.key;
        self.read(move |connection| {
            let mut statement = connection.prepare_cached(
                "SELECT path, size_bytes, modified_ms FROM sync_unmanaged
                 WHERE target_id = ?1 ORDER BY path",
            )?;
            let rows = statement.query_map([key], |row| {
                Ok((row.get(0)?, unsigned(row.get(1)?).unwrap_or(0), row.get(2)?))
            })?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
    }

    /// The ReplayGain figures of a master, from the tags the scan stored
    /// with its revision: no file is opened.
    pub fn sync_gain(&self, revision: i64) -> Result<GainTags, Error> {
        self.read(move |connection| {
            let payload: Option<String> = connection
                .query_row(
                    "SELECT payload_json FROM source_item_revision WHERE id = ?1",
                    [revision],
                    |row| row.get(0),
                )
                .optional()?;
            Ok(payload.map_or_else(GainTags::default, |payload| gain_of(&payload)))
        })
    }

    /// Adds a folder as a target, not yet set up, whose source is the whole
    /// library. `root` names its path in `host`'s `server.db`. What it is
    /// made for supplies its codecs, cover size, playlist kind and first
    /// format.
    #[allow(clippy::too_many_arguments)]
    pub fn sync_add_folder<F>(
        &self,
        request: WriteRequest,
        name: String,
        root: Id,
        host: Id,
        limit_bytes: Option<u64>,
        made_for: SyncMadeFor,
        kind: SyncFolderKind,
        answer: F,
    ) -> Result<Written, Error>
    where
        F: FnOnce(i64, Id) -> String + Send + 'static,
    {
        self.write(request, move |write| {
            let connection = write.connection();
            let listener: Option<i64> =
                connection.query_row("SELECT min(id) FROM listener", [], |row| row.get(0))?;
            let listener = listener.ok_or(Error::Invalid("the library has no listener"))?;
            let id = write.new_id()?;
            let preset = super::policy::preset(made_for);
            connection.execute(
                "INSERT INTO sync_target
                   (public_id, kind, family, name, listener_id, format_json, limit_bytes,
                    made_for, folder_kind, codecs_json, playlist_kind, cover_px, id3v1,
                    read_plays, scrobble_plays, created_at_ms, rev)
                 VALUES (?1, 'folder', 'folder', ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 0, 0,
                         ?12, ?13)",
                params![
                    id.as_bytes(),
                    name,
                    listener,
                    Settings::format_for(preset.codecs).to_json()?,
                    limit_bytes.filter(|limit| *limit > 0).map(signed),
                    made_for_text(made_for),
                    folder_kind_text(kind),
                    codecs_json(preset.codecs),
                    playlists_text(preset.playlists),
                    i64::from(preset.cover_px),
                    preset.id3v1,
                    write.now_ms(),
                    write.rev(),
                ],
            )?;
            let target = connection.last_insert_rowid();
            connection.execute(
                "INSERT INTO sync_target_identity (target_id, transport, host_server_id, root_pid, rev)
                 VALUES (?1, 'folder', (SELECT id FROM server WHERE public_id = ?2), ?3, ?4)",
                params![target, host.as_bytes(), root.as_bytes(), write.rev()],
            )?;
            insert_sources(
                write,
                target,
                &[SyncSourceSpec {
                    role: SyncSourceRole::Rule,
                    selector: SyncSelector::Everything,
                    unit: SyncUnit::Albums,
                }],
            )?;
            Ok(answer(write.rev(), id))
        })
    }

    /// Changes a target's name, drawing, sources or settings, or whether it
    /// is ignored.
    pub fn sync_set<F>(
        &self,
        request: WriteRequest,
        id: Id,
        changes: TargetChanges,
        answer: F,
    ) -> Result<Written, Error>
    where
        F: FnOnce(i64) -> String + Send + 'static,
    {
        if let Some(format) = changes.format {
            format.validate()?;
        }
        if changes
            .name
            .as_ref()
            .is_some_and(|name| name.trim().is_empty() || name.chars().count() > 100)
        {
            return Err(Error::Invalid("a device's name is 1 to 100 characters"));
        }
        if changes.cover_px.is_some_and(|px| px > 2000) {
            return Err(Error::Invalid("a cover is at most 2000 pixels"));
        }
        if changes.codecs.as_ref().is_some_and(Vec::is_empty) {
            return Err(Error::Invalid("a device plays at least one format"));
        }
        if changes
            .sources
            .as_ref()
            .is_some_and(|sources| sources.len() > 500)
        {
            return Err(Error::Invalid("a device has at most 500 sources"));
        }
        self.write(request, move |write| {
            let connection = write.connection();
            let target = target_key(connection, id)?;
            let set = |column: &str, value: &dyn rusqlite::ToSql| {
                connection
                    .execute(
                        &format!("UPDATE sync_target SET {column} = ?1, rev = ?2 WHERE id = ?3"),
                        params![value, write.rev(), target],
                    )
                    .map(|_| ())
            };
            if let Some(name) = &changes.name {
                set("name", &name.trim())?;
            }
            if let Some(icon) = &changes.icon_key {
                set("icon_key", icon)?;
            }
            if let Some(ignored) = changes.ignored {
                // A target that was set up stays Ritornel's when it is no
                // longer ignored; one that never was goes back to new.
                let ready: bool = connection.query_row(
                    "SELECT EXISTS (SELECT 1 FROM sync_target_epoch
                                    WHERE target_id = ?1 AND ended_at_ms IS NULL)",
                    [target],
                    |row| row.get(0),
                )?;
                let setup = match (ignored, ready) {
                    (true, _) => "ignored",
                    (false, true) => "ready",
                    (false, false) => "new",
                };
                set("setup", &setup)?;
            }
            let mut plans = false;
            // What a folder is made for sets its purpose's own values
            // first, so that anything else in the same change stands.
            if let Some(made_for) = changes.made_for {
                let preset = super::policy::preset(made_for);
                set("made_for", &made_for_text(made_for))?;
                set("codecs_json", &codecs_json(preset.codecs))?;
                set("cover_px", &i64::from(preset.cover_px))?;
                set("playlist_kind", &playlists_text(preset.playlists))?;
                set("id3v1", &preset.id3v1)?;
                plans = true;
            }
            if let Some(kind) = changes.folder_kind {
                set("folder_kind", &folder_kind_text(kind))?;
                plans = true;
            }
            if let Some(codecs) = &changes.codecs {
                set("codecs_json", &codecs_json(codecs))?;
                plans = true;
            }
            if let Some(playlists) = changes.playlists {
                set("playlist_kind", &playlists_text(playlists))?;
            }
            if let Some(format) = changes.format {
                set("format_json", &format.to_json()?)?;
                plans = true;
            }
            if let Some(px) = changes.cover_px {
                set("cover_px", &i64::from(px))?;
                plans = true;
            }
            if let Some(id3v1) = changes.id3v1 {
                set("id3v1", &id3v1)?;
                plans = true;
            }
            if let Some(reserve) = changes.reserve_bytes {
                set("reserve_bytes", &reserve.map(signed))?;
                plans = true;
            }
            if let Some(limit) = changes.limit_bytes {
                set("limit_bytes", &limit.map(signed))?;
                plans = true;
            }
            if let Some(when_full) = changes.when_full {
                set(
                    "when_full",
                    &match when_full {
                        SyncWhenFull::Ask => "ask",
                        SyncWhenFull::Fill => "fill",
                    },
                )?;
            }
            if let Some(on_connect) = changes.on_connect {
                set(
                    "on_connect",
                    &match on_connect {
                        SyncOnConnect::Nothing => "nothing",
                        SyncOnConnect::Sync => "sync",
                    },
                )?;
            }
            if let Some(sources) = &changes.sources {
                insert_sources(write, target, sources)?;
                plans = true;
            }
            // A folder that no longer plays what its format makes takes
            // the first format it does play.
            if changes.made_for.is_some() || changes.codecs.is_some() {
                let now = targets(connection, Some(id))?
                    .into_iter()
                    .next()
                    .ok_or(Error::NotFound("no such device"))?;
                if !now.settings.plays_its_format() {
                    set(
                        "format_json",
                        &Settings::format_for(&now.settings.codecs).to_json()?,
                    )?;
                }
            }
            if plans {
                connection.execute(
                    "UPDATE sync_target SET settings_rev = settings_rev + 1, rev = ?1 WHERE id = ?2",
                    params![write.rev(), target],
                )?;
            }
            // A target is no part of what the library's lists show.
            write.changes_nothing_shown();
            Ok(answer(write.rev()))
        })
    }

    /// Ends a target: its baselines and its epoch are closed, and its runs
    /// stay, since they are history. A target with a run open is refused.
    /// Answers the folder root it had, for its path to be forgotten too.
    pub fn sync_forget<F>(&self, request: WriteRequest, id: Id, answer: F) -> Result<Written, Error>
    where
        F: FnOnce(i64) -> String + Send + 'static,
    {
        self.write(request, move |write| {
            let connection = write.connection();
            let target = target_key(connection, id)?;
            if open_run(connection, target)?.is_some() {
                return Err(Error::Invalid("the device is syncing: stop it first"));
            }
            let (rev, now) = (write.rev(), write.now_ms());
            connection.execute(
                "UPDATE sync_object SET ended_at_ms = ?1, end_reason = 'forgotten', rev = ?2
                 WHERE target_id = ?3 AND ended_at_ms IS NULL",
                params![now, rev, target],
            )?;
            connection.execute(
                "UPDATE sync_target_epoch SET ended_at_ms = ?1, rev = ?2
                 WHERE target_id = ?3 AND ended_at_ms IS NULL",
                params![now, rev, target],
            )?;
            // Its root is free for a folder to be added there again.
            connection.execute(
                "DELETE FROM sync_target_identity WHERE target_id = ?1",
                [target],
            )?;
            connection.execute("DELETE FROM sync_unmanaged WHERE target_id = ?1", [target])?;
            connection.execute(
                "UPDATE sync_target SET setup = 'forgotten', rev = ?1 WHERE id = ?2",
                params![rev, target],
            )?;
            write.changes_nothing_shown();
            Ok(answer(rev))
        })
    }

    /// The run of a target that is open, if one is.
    pub fn sync_open_run(&self, target: &TargetRow) -> Result<Option<RunRow>, Error> {
        let key = target.key;
        self.read(move |connection| open_run(connection, key))
    }

    /// The newest run of a target, open or not.
    pub fn sync_last_run(&self, target: &TargetRow) -> Result<Option<RunRow>, Error> {
        let key = target.key;
        self.read(move |connection| {
            Ok(connection
                .query_row(
                    &format!(
                        "SELECT {RUN_COLUMNS} FROM sync_run WHERE target_id = ?1
                         ORDER BY id DESC LIMIT 1"
                    ),
                    [key],
                    run,
                )
                .optional()?)
        })
    }

    /// One run of a target by its ID.
    pub fn sync_run(&self, target: &TargetRow, id: Id) -> Result<Option<RunRow>, Error> {
        let key = target.key;
        self.read(move |connection| {
            Ok(connection
                .query_row(
                    &format!(
                        "SELECT {RUN_COLUMNS} FROM sync_run WHERE target_id = ?1 AND public_id = ?2"
                    ),
                    params![key, id.as_bytes()],
                    run,
                )
                .optional()?)
        })
    }

    /// A run's actions, in the order they are carried out.
    pub fn sync_actions(&self, run: &RunRow) -> Result<Vec<ActionRow>, Error> {
        let key = run.key;
        self.read(move |connection| {
            let mut statement = connection.prepare_cached(
                "SELECT seq, kind, reason, source_pid, title, artist, album, album_artist, art_pid,
                        group_key, album_tracks, object_path, codec, how, bytes, estimated, state,
                        error_code, error_detail
                 FROM sync_action WHERE run_id = ?1 ORDER BY seq",
            )?;
            let rows = statement.query_map([key], |row| {
                Ok(ActionRow {
                    seq: row.get(0)?,
                    kind: match row.get::<_, String>(1)?.as_str() {
                        "add" => SyncActionKind::Add,
                        "replace" => SyncActionKind::Replace,
                        "remove" => SyncActionKind::Remove,
                        "conflict" => SyncActionKind::Conflict,
                        _ => SyncActionKind::Blocked,
                    },
                    reason: row.get(2)?,
                    source: id_at(row, 3)?,
                    shown: Shown {
                        title: row.get(4)?,
                        artist: row.get(5)?,
                        album: row.get(6)?,
                        album_artist: row.get(7)?,
                        art: id_at(row, 8)?,
                        group_key: row.get(9)?,
                        album_tracks: u32::try_from(row.get::<_, i64>(10)?).unwrap_or(0),
                    },
                    path: row.get(11)?,
                    codec: row.get(12)?,
                    how: row
                        .get::<_, Option<String>>(13)?
                        .map(|how| match how.as_str() {
                            "encode" => SyncHow::Encode,
                            _ => SyncHow::AsIs,
                        }),
                    bytes: unsigned(row.get(14)?).unwrap_or(0),
                    estimated: row.get(15)?,
                    state: match row.get::<_, String>(16)?.as_str() {
                        "issued" => Stored::Issued,
                        "done" => Stored::Done,
                        "failed" => Stored::Failed,
                        "conflict" => Stored::Conflict,
                        "skipped" => Stored::Skipped,
                        _ => Stored::Planned,
                    },
                    error: match (row.get::<_, Option<String>>(17)?, row.get(18)?) {
                        (Some(code), detail) => {
                            Some((code, Option::<String>::unwrap_or_default(detail)))
                        }
                        _ => None,
                    },
                })
            })?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
    }

    /// Opens a run for a target, before anything is read of it. A second
    /// open run for a target is refused, so the open run is the lease: no
    /// table holds one.
    pub fn sync_start_run(
        &self,
        target: &TargetRow,
        job: Id,
        plan_revision: String,
    ) -> Result<RunRow, Error> {
        let (key, host) = (target.key, target.host);
        let request = WriteRequest {
            method: RUN_METHOD,
            command: None,
            client: None,
        };
        let written = self.write(request, move |write| {
            let connection = write.connection();
            if open_run(connection, key)?.is_some() {
                return Err(Error::Invalid("the device is syncing already"));
            }
            let id = write.new_id()?;
            connection.execute(
                "INSERT INTO sync_run
                   (public_id, target_id, job_pid, host_server_id, epoch, plan_revision,
                    library_rev, started_at_ms, rev)
                 VALUES (?1, ?2, ?3, (SELECT id FROM server WHERE public_id = ?4),
                         coalesce((SELECT epoch FROM sync_target_epoch
                                   WHERE target_id = ?2 AND ended_at_ms IS NULL), 0),
                         ?5, ?6, ?7, ?6)",
                params![
                    id.as_bytes(),
                    key,
                    job.as_bytes(),
                    host.map(|host| host.as_bytes().to_vec()),
                    plan_revision,
                    write.rev(),
                    write.now_ms(),
                ],
            )?;
            write.changes_nothing_shown();
            Ok(id.to_string())
        })?;
        let id: Id = written
            .result_json
            .parse()
            .map_err(|_| Error::Corrupt("a run's ID could not be read back"))?;
        self.sync_run(target, id)?
            .ok_or(Error::Corrupt("a run just opened is not there"))
    }

    /// Freezes a run's actions at `plan`, in one transaction, before
    /// anything on the target changes. The baselines whose files are gone
    /// and unwanted are closed, and the files that are not Ritornel's are
    /// recorded as the inventory found them. A run that adopts starts the
    /// target's next epoch under `marker` and makes the target ready; any
    /// other needs the epoch open.
    ///
    /// `skip` names the actions left for another day, by their index in
    /// the plan.
    pub fn sync_freeze(
        &self,
        target: &TargetRow,
        run: &RunRow,
        plan: &Plan,
        adopt: Option<Id>,
        skip: Vec<usize>,
        library_rev: i64,
    ) -> Result<RunRow, Error> {
        let (key, run_key, run_id) = (target.key, run.key, run.id);
        let frozen: Vec<Action> = plan.actions.clone();
        let gone = plan.gone.clone();
        let unmanaged: Vec<(String, u64, Option<i64>)> = plan
            .unmanaged
            .iter()
            .map(|object| {
                (
                    object.path.as_str().to_owned(),
                    object.size,
                    object.modified_ms,
                )
            })
            .collect();
        let seen = (plan.room.capacity_bytes, plan.room.free_bytes);
        self.sync_bookkeeping(move |write| {
            let connection = write.connection();
            let (rev, now) = (write.rev(), write.now_ms());
            let open: Option<i64> = connection
                .query_row(
                    "SELECT epoch FROM sync_target_epoch
                     WHERE target_id = ?1 AND ended_at_ms IS NULL",
                    [key],
                    |row| row.get(0),
                )
                .optional()?;
            let epoch = match (adopt, open) {
                (Some(marker), _) => {
                    // A new lifetime: what the last one recorded is closed,
                    // so everything there is a file Ritornel did not write.
                    connection.execute(
                        "UPDATE sync_object SET ended_at_ms = ?1, end_reason = 'released', rev = ?2
                         WHERE target_id = ?3 AND ended_at_ms IS NULL",
                        params![now, rev, key],
                    )?;
                    connection.execute(
                        "UPDATE sync_target_epoch SET ended_at_ms = ?1, rev = ?2
                         WHERE target_id = ?3 AND ended_at_ms IS NULL",
                        params![now, rev, key],
                    )?;
                    let next: i64 = connection.query_row(
                        "SELECT coalesce(max(epoch), 0) + 1 FROM sync_target_epoch
                         WHERE target_id = ?1",
                        [key],
                        |row| row.get(0),
                    )?;
                    connection.execute(
                        "INSERT INTO sync_target_epoch (target_id, epoch, marker_pid, started_at_ms, rev)
                         VALUES (?1, ?2, ?3, ?4, ?5)",
                        params![key, next, marker.as_bytes(), now, rev],
                    )?;
                    connection.execute(
                        "UPDATE sync_target SET setup = 'ready', rev = ?1 WHERE id = ?2",
                        params![rev, key],
                    )?;
                    next
                }
                (None, Some(epoch)) => epoch,
                (None, None) => return Err(Error::Invalid("the device is not set up")),
            };
            for baseline in &gone {
                connection.execute(
                    "UPDATE sync_object SET ended_at_ms = ?1, end_reason = 'missing', rev = ?2
                     WHERE id = ?3 AND target_id = ?4 AND ended_at_ms IS NULL",
                    params![now, rev, baseline, key],
                )?;
            }
            connection.execute("DELETE FROM sync_unmanaged WHERE target_id = ?1", [key])?;
            {
                let mut insert = connection.prepare_cached(
                    "INSERT INTO sync_unmanaged (target_id, path, size_bytes, modified_ms, rev)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                )?;
                for (path, size, modified) in &unmanaged {
                    insert.execute(params![key, path, signed(*size), modified, rev])?;
                }
            }
            connection.execute(
                "UPDATE sync_target SET seen_capacity_bytes = ?1, seen_free_bytes = ?2,
                        seen_at_ms = ?3, rev = ?4 WHERE id = ?5",
                params![signed(seen.0), signed(seen.1), now, rev, key],
            )?;
            // Only what a run carries out is counted; the rest are rows
            // that say why nothing is done.
            let running = |action: &Action, at: usize| action.runs() && !skip.contains(&at);
            let total = frozen
                .iter()
                .enumerate()
                .filter(|(at, action)| running(action, *at))
                .count();
            let bytes: u64 = frozen
                .iter()
                .enumerate()
                .filter(|(at, action)| running(action, *at) && action.make.is_some())
                .map(|(_, action)| action.bytes)
                .sum();
            connection.execute(
                "UPDATE sync_run SET epoch = ?1, library_rev = ?2, adopted = ?3,
                        actions_total = ?4, bytes_total = ?5, rev = ?6
                 WHERE id = ?7",
                params![
                    epoch,
                    library_rev,
                    adopt.is_some(),
                    total as i64,
                    signed(bytes),
                    rev,
                    run_key,
                ],
            )?;
            let mut insert = connection.prepare_cached(
                "INSERT INTO sync_action
                   (run_id, seq, kind, reason, appearance_id, source_pid, group_key, title, artist,
                    album, album_artist, art_pid, album_tracks, object_path, projection_key, codec,
                    how, bytes, estimated, expect_old_size, expect_old_sampled, state, rev)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                         ?17, ?18, ?19, ?20, ?21, ?22, ?23)",
            )?;
            for (at, action) in frozen.iter().enumerate() {
                insert.execute(params![
                    run_key,
                    at as i64,
                    kind_text(action.kind),
                    action.reason,
                    action.appearance,
                    action.source.map(|source| source.as_bytes().to_vec()),
                    action.shown.group_key,
                    action.shown.title,
                    action.shown.artist,
                    action.shown.album,
                    action.shown.album_artist,
                    action.shown.art.map(|art| art.as_bytes().to_vec()),
                    i64::from(action.shown.album_tracks),
                    action.path.as_ref().map(RelPath::as_str),
                    action.make.as_ref().map(|make| make.key.0.to_vec()),
                    action.codec(),
                    action.how().map(|how| match how {
                        SyncHow::AsIs => "as_is",
                        SyncHow::Encode => "encode",
                    }),
                    signed(action.bytes),
                    action.estimated,
                    action.old.as_ref().map(|old| signed(old.size)),
                    action.old.as_ref().map(|old| old.sampled.to_vec()),
                    if !action.runs() {
                        // A conflict stands as one; a blocked track was
                        // never going to be asked of the device.
                        match action.kind {
                            SyncActionKind::Conflict => "conflict",
                            _ => "skipped",
                        }
                    } else if skip.contains(&at) {
                        "skipped"
                    } else {
                        "planned"
                    },
                    rev,
                ])?;
            }
            Ok(())
        })?;
        self.sync_run(target, run_id)?
            .ok_or(Error::Corrupt("a run just frozen is not there"))
    }

    /// Says which step a run is at.
    pub fn sync_step(&self, run: &RunRow, step: SyncStep) -> Result<(), Error> {
        let key = run.key;
        self.sync_bookkeeping(move |write| {
            write.connection().execute(
                "UPDATE sync_run SET step = ?1, rev = ?2 WHERE id = ?3",
                params![step_text(step), write.rev(), key],
            )?;
            Ok(())
        })
    }

    /// Records the results of the actions just carried out and writes down
    /// the next ones before the device is asked to do them, in one
    /// transaction: so what a crash leaves in doubt is one window of
    /// actions, each with the digest it meant to leave. A baseline is
    /// written with the action that earned it.
    pub fn sync_window(
        &self,
        target: &TargetRow,
        run: &RunRow,
        plan: &Plan,
        results: Vec<Recorded>,
        next: Vec<Issued>,
    ) -> Result<(), Error> {
        let (target, run_key, epoch) = (target.key, run.key, run.epoch);
        // What each finished action's baseline is made from.
        let made: Vec<_> = results
            .iter()
            .map(|result| {
                plan.actions
                    .get(usize::try_from(result.seq).unwrap_or(usize::MAX))
                    .map(|action| {
                        (
                            action.kind,
                            action.appearance,
                            action.make.as_ref().map(|make| {
                                (
                                    make.copy.source_item,
                                    make.copy.revision_no,
                                    make.key,
                                    make.sent.codec().extension(),
                                    matches!(make.sent, super::policy::Sent::Encode(..)),
                                )
                            }),
                        )
                    })
            })
            .collect();
        self.sync_bookkeeping(move |write| {
            let connection = write.connection();
            let (rev, now) = (write.rev(), write.now_ms());
            for (result, made) in results.iter().zip(&made) {
                let Some((kind, appearance, make)) = made else {
                    continue;
                };
                let close = |reason: &str| {
                    result.old.map_or(Ok(0), |old| {
                        connection.execute(
                            "UPDATE sync_object SET ended_at_ms = ?1, end_reason = ?2, rev = ?3
                             WHERE id = ?4 AND ended_at_ms IS NULL",
                            params![now, reason, rev, old],
                        )
                    })
                };
                let (state, code, detail, done_bytes) = match &result.outcome {
                    Outcome::Done(left) => {
                        close(match kind {
                            SyncActionKind::Remove => "removed",
                            SyncActionKind::Replace => "replaced",
                            _ => "missing",
                        })?;
                        if let (Some(left), Some((item, revision, key, codec, encoded)), Some(appearance)) =
                            (left, make, appearance)
                        {
                            connection.execute(
                                "INSERT INTO sync_object
                                   (target_id, epoch, appearance_id, source_item_id, revision_no,
                                    object_path, projection_key, codec, how, size_bytes,
                                    sampled_hash, file_hash, written_at_ms, rev)
                                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                                params![
                                    target,
                                    epoch,
                                    appearance,
                                    item,
                                    revision,
                                    left.path.as_str(),
                                    key.0.as_slice(),
                                    codec,
                                    if *encoded { "encode" } else { "as_is" },
                                    signed(left.size),
                                    left.sampled.as_slice(),
                                    left.digest.as_slice(),
                                    now,
                                    rev,
                                ],
                            )?;
                        }
                        let bytes = left.as_ref().map_or(0, |left| left.size);
                        ("done", None, None, bytes)
                    }
                    Outcome::Failed(code, detail) => {
                        if result.old_gone {
                            close("missing")?;
                        }
                        ("failed", Some(code.clone()), Some(detail.clone()), 0)
                    }
                    Outcome::Conflict(detail) => (
                        "conflict",
                        Some("changed_on_device".to_owned()),
                        Some(detail.clone()),
                        0,
                    ),
                    Outcome::Skipped => ("skipped", None, None, 0),
                };
                connection.execute(
                    "UPDATE sync_action SET state = ?1, error_code = ?2, error_detail = ?3,
                            bytes = CASE WHEN ?4 > 0 THEN ?4 ELSE bytes END,
                            estimated = CASE WHEN ?4 > 0 THEN 0 ELSE estimated END, rev = ?5
                     WHERE run_id = ?6 AND seq = ?7",
                    params![state, code, detail, signed(done_bytes), rev, run_key, result.seq],
                )?;
                let done = matches!(result.outcome, Outcome::Done(_));
                connection.execute(
                    "UPDATE sync_run SET
                       actions_done = actions_done + 1,
                       actions_failed = actions_failed + ?1,
                       copied = copied + ?2, removed = removed + ?3,
                       bytes_done = bytes_done + ?4, rev = ?5
                     WHERE id = ?6",
                    params![
                        matches!(
                            result.outcome,
                            Outcome::Failed(..) | Outcome::Conflict(_)
                        ),
                        done && *kind != SyncActionKind::Remove,
                        done && *kind == SyncActionKind::Remove,
                        signed(done_bytes),
                        rev,
                        run_key,
                    ],
                )?;
            }
            for issued in &next {
                let (size, digest, sampled) = match &issued.leaves {
                    Some((size, digest, sampled)) => {
                        (Some(signed(*size)), Some(digest.to_vec()), Some(sampled.to_vec()))
                    }
                    None => (None, None, None),
                };
                connection.execute(
                    "UPDATE sync_action SET state = 'issued', expect_new_size = ?1,
                            expect_new_hash = ?2, expect_new_sampled = ?3, rev = ?4
                     WHERE run_id = ?5 AND seq = ?6 AND state = 'planned'",
                    params![size, digest, sampled, rev, run_key, issued.seq],
                )?;
            }
            Ok(())
        })
    }

    /// Closes a run with how it ended, and keeps the write speed it
    /// measured for the next estimate.
    pub fn sync_finish_run(
        &self,
        target: &TargetRow,
        run: &RunRow,
        outcome: SyncOutcome,
        error: Option<String>,
        write_bps: Option<u64>,
    ) -> Result<(), Error> {
        let (target, key) = (target.key, run.key);
        self.sync_bookkeeping(move |write| {
            let connection = write.connection();
            connection.execute(
                "UPDATE sync_run SET outcome = ?1, error = ?2, ended_at_ms = ?3, step = 'finish',
                        rev = ?4
                 WHERE id = ?5 AND ended_at_ms IS NULL",
                params![
                    outcome_text(outcome),
                    error,
                    write.now_ms(),
                    write.rev(),
                    key
                ],
            )?;
            // An older run's actions have served: the next run's are what
            // is read now. Failed and conflict rows stay until then.
            connection.execute(
                "DELETE FROM sync_action WHERE run_id IN
                   (SELECT id FROM sync_run WHERE target_id = ?1 AND id < ?2)",
                params![target, key],
            )?;
            if let Some(bps) = write_bps {
                connection.execute(
                    "UPDATE sync_target SET write_bps = ?1, rev = ?2 WHERE id = ?3",
                    params![signed(bps), write.rev(), target],
                )?;
            }
            Ok(())
        })
    }

    /// Closes every run a dead run of the server left open: an open run
    /// belongs to a session some earlier boot held in memory. Its issued
    /// actions stay, for the next read of its target to check. Called once
    /// at every start, so a target is never left busy by a run no one
    /// holds.
    pub fn sync_close_stale_runs(&self) -> Result<u64, Error> {
        let open: i64 = self.read(|connection| {
            Ok(connection.query_row(
                "SELECT count(*) FROM sync_run WHERE ended_at_ms IS NULL",
                [],
                |row| row.get(0),
            )?)
        })?;
        if open == 0 {
            return Ok(0);
        }
        self.sync_bookkeeping(|write| {
            write.connection().execute(
                "UPDATE sync_run SET outcome = 'cut_off', ended_at_ms = ?1, rev = ?2,
                        error = 'The server stopped while this sync was running.'
                 WHERE ended_at_ms IS NULL",
                params![write.now_ms(), write.rev()],
            )?;
            Ok(())
        })?;
        Ok(u64::try_from(open).unwrap_or(0))
    }

    /// The actions of a target's last run that were asked of the device
    /// with no result recorded: what a crash or a pulled cable left in
    /// doubt. Each is checked against the device at the next read.
    pub fn sync_in_doubt(&self, target: &TargetRow) -> Result<Vec<InDoubt>, Error> {
        let key = target.key;
        self.read(move |connection| {
            let mut statement = connection.prepare_cached(
                "SELECT a.id, r.epoch, a.kind, a.appearance_id, a.object_path, a.projection_key,
                        a.codec, a.how, a.expect_new_size, a.expect_new_hash, a.expect_new_sampled
                 FROM sync_action a JOIN sync_run r ON r.id = a.run_id
                 WHERE r.target_id = ?1 AND r.ended_at_ms IS NOT NULL AND a.state = 'issued'
                 ORDER BY a.id",
            )?;
            let rows = statement.query_map([key], |row| {
                Ok(InDoubt {
                    action: row.get(0)?,
                    epoch: row.get(1)?,
                    writes: row.get::<_, String>(2)? != "remove",
                    appearance: row.get(3)?,
                    path: row.get(4)?,
                    key: row.get(5)?,
                    codec: row.get(6)?,
                    how: row.get(7)?,
                    size: unsigned(row.get(8)?),
                    digest: row.get(9)?,
                    sampled: row.get(10)?,
                })
            })?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
    }

    /// Settles actions left in doubt, each by what the device showed: a
    /// write whose file is there as it was meant is done, and earns its
    /// baseline; anything else was not done, and the next plan takes it up.
    pub fn sync_settle(
        &self,
        target: &TargetRow,
        settled: Vec<(InDoubt, bool)>,
    ) -> Result<(), Error> {
        let target = target.key;
        self.sync_bookkeeping(move |write| {
            let connection = write.connection();
            let (rev, now) = (write.rev(), write.now_ms());
            for (doubt, landed) in &settled {
                if *landed
                    && doubt.writes
                    && let (Some(appearance), Some(path), Some(key), Some(codec), Some(how)) = (
                        doubt.appearance,
                        &doubt.path,
                        &doubt.key,
                        &doubt.codec,
                        &doubt.how,
                    )
                    && let (Some(size), Some(digest), Some(sampled)) =
                        (doubt.size, &doubt.digest, &doubt.sampled)
                {
                    // Whatever held the path before was replaced by it.
                    connection.execute(
                        "UPDATE sync_object SET ended_at_ms = ?1, end_reason = 'replaced', rev = ?2
                         WHERE target_id = ?3 AND ended_at_ms IS NULL
                           AND (object_path = ?4 OR appearance_id = ?5)",
                        params![now, rev, target, path, appearance],
                    )?;
                    connection.execute(
                        "INSERT INTO sync_object
                           (target_id, epoch, appearance_id, object_path, projection_key, codec,
                            how, size_bytes, sampled_hash, file_hash, written_at_ms, rev)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                        params![
                            target,
                            doubt.epoch,
                            appearance,
                            path,
                            key,
                            codec,
                            how,
                            signed(size),
                            sampled,
                            digest,
                            now,
                            rev
                        ],
                    )?;
                }
                connection.execute(
                    "UPDATE sync_action SET state = ?1, error_code = ?2, error_detail = ?3, rev = ?4
                     WHERE id = ?5",
                    params![
                        if *landed { "done" } else { "failed" },
                        (!*landed).then_some("cut_off"),
                        (!*landed).then_some("The sync was cut off before this file was finished."),
                        rev,
                        doubt.action,
                    ],
                )?;
            }
            Ok(())
        })
    }

    fn sync_bookkeeping<F>(&self, work: F) -> Result<(), Error>
    where
        F: FnOnce(&Write<'_>) -> Result<(), Error> + Send + 'static,
    {
        let request = WriteRequest {
            method: RUN_METHOD,
            command: None,
            client: None,
        };
        self.write(request, move |write| {
            work(write)?;
            write.changes_nothing_shown();
            Ok(String::new())
        })
        .map(|_| ())
    }
}

/// An action asked of the device with no result recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InDoubt {
    action: i64,
    epoch: i64,
    /// Whether it writes a file, rather than only removing one.
    pub writes: bool,
    appearance: Option<i64>,
    /// Where its file is or goes.
    pub path: Option<String>,
    key: Option<Vec<u8>>,
    codec: Option<String>,
    how: Option<String>,
    /// What it must leave: the length, and the sampled hash.
    pub size: Option<u64>,
    digest: Option<Vec<u8>>,
    pub sampled: Option<Vec<u8>>,
}

fn open_run(connection: &Connection, target: i64) -> Result<Option<RunRow>, Error> {
    Ok(connection
        .query_row(
            &format!(
                "SELECT {RUN_COLUMNS} FROM sync_run WHERE target_id = ?1 AND ended_at_ms IS NULL"
            ),
            [target],
            run,
        )
        .optional()?)
}

fn baselines(connection: &Connection, target: i64) -> Result<Vec<Baseline>, Error> {
    let mut statement = connection.prepare_cached(
        "SELECT o.id, ta.id, o.object_path, o.projection_key, o.codec, o.size_bytes, o.sampled_hash,
                ta.display_title, tac.display_text, rel.display_title, relc.display_text,
                pic.public_id, rel.id,
                (SELECT count(*) FROM track_appearance n
                 WHERE n.release_id = rel.id AND n.redirect_to_id IS NULL)
         FROM sync_object o
         JOIN track_appearance named ON named.id = o.appearance_id
         JOIN track_appearance ta ON ta.id = named.canonical_id
         LEFT JOIN artist_credit tac ON tac.id = ta.display_artist_credit_id
         LEFT JOIN release own ON own.id = ta.release_id
         LEFT JOIN release rel ON rel.id = own.canonical_id
         LEFT JOIN artist_credit relc ON relc.id = rel.display_artist_credit_id
         LEFT JOIN picture pic ON pic.id = rel.display_picture_id
         WHERE o.target_id = ?1 AND o.ended_at_ms IS NULL AND o.object_path IS NOT NULL
         ORDER BY o.id",
    )?;
    let mut rows = statement.query([target])?;
    let mut found = Vec::new();
    while let Some(row) = rows.next()? {
        let appearance: i64 = row.get(1)?;
        let path = RelPath::new(row.get::<_, String>(2)?)
            .map_err(|_| Error::Corrupt("a baseline's path leaves its root"))?;
        let artist: Option<String> = row.get(8)?;
        let album_artist: Option<String> = row.get(10)?;
        let release: Option<i64> = row.get(12)?;
        found.push(Baseline {
            id: row.get(0)?,
            appearance,
            path,
            key: ProjectionKey(row.get(3)?),
            codec: row.get(4)?,
            size: unsigned(row.get(5)?).unwrap_or(0),
            sampled: row.get(6)?,
            shown: Shown {
                title: row.get::<_, Option<String>>(7)?.unwrap_or_default(),
                album_artist: album_artist
                    .clone()
                    .or_else(|| artist.clone())
                    .unwrap_or_default(),
                artist: artist.or(album_artist).unwrap_or_default(),
                album: row.get::<_, Option<String>>(9)?.unwrap_or_default(),
                art: id_at(row, 11)?,
                group_key: match release {
                    Some(release) => format!("r{release}"),
                    None => format!("t{appearance}"),
                },
                album_tracks: u32::try_from(row.get::<_, i64>(13)?).unwrap_or(0),
            },
        });
    }
    Ok(found)
}

/// A ReplayGain value as taggers write it: "-6.02 dB", "+3.1", "0.988".
fn gain_number(text: &str) -> Option<f32> {
    let text = text.trim();
    let text = text
        .strip_suffix("dB")
        .or_else(|| text.strip_suffix("db"))
        .unwrap_or(text)
        .trim();
    text.strip_prefix('+')
        .unwrap_or(text)
        .parse()
        .ok()
        .filter(|value: &f32| value.is_finite())
}

/// The ReplayGain figures among a stored payload's tags. A Vorbis comment
/// is named for the field; an ID3v2 `TXXX` frame names it in its
/// description.
fn gain_of(payload: &str) -> GainTags {
    let mut tags = GainTags::default();
    let Ok(payload) = serde_json::from_str::<ritornel_domain::payload::Payload>(payload) else {
        return tags;
    };
    let set = |slot: &mut Option<f32>, value: Option<&String>| {
        *slot = slot.or_else(|| value.and_then(|value| gain_number(value)));
    };
    for tag in &payload.tags {
        let name = tag.sub.as_deref().unwrap_or(&tag.key).to_ascii_uppercase();
        let value = tag.values.first();
        match name.as_str() {
            "REPLAYGAIN_TRACK_GAIN" => set(&mut tags.track.gain_db, value),
            "REPLAYGAIN_TRACK_PEAK" => set(&mut tags.track.peak, value),
            "REPLAYGAIN_ALBUM_GAIN" => set(&mut tags.album.gain_db, value),
            "REPLAYGAIN_ALBUM_PEAK" => set(&mut tags.album.peak, value),
            _ => {}
        }
    }
    // A gain and its peak are measured together: a peak alone says nothing.
    let whole = |tag: GainTag| {
        if tag.gain_db.is_some() {
            tag
        } else {
            GainTag::default()
        }
    };
    GainTags {
        track: whole(tags.track),
        album: whole(tags.album),
    }
}

impl ServerStore {
    /// Records where a folder a library syncs to is on this machine.
    pub fn add_sync_root(&self, root: Id, abs_path: &str) -> Result<(), Error> {
        let abs_path = abs_path.to_owned();
        self.transact(move |transaction, now_ms, _| {
            transaction.execute(
                "INSERT INTO sync_root (root_id, abs_path, created_at_ms) VALUES (?1, ?2, ?3)
                 ON CONFLICT(root_id) DO UPDATE SET abs_path = excluded.abs_path",
                params![root.as_bytes(), abs_path, now_ms],
            )?;
            Ok(())
        })
    }

    /// Every folder a library syncs to on this machine, by its root ID.
    pub fn sync_roots(&self) -> Result<Vec<(Id, String)>, Error> {
        self.read(|connection| {
            let mut statement =
                connection.prepare_cached("SELECT root_id, abs_path FROM sync_root ORDER BY id")?;
            let rows =
                statement.query_map([], |row| Ok((Id::from_bytes(row.get(0)?), row.get(1)?)))?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
    }

    /// Forgets where a folder was.
    pub fn remove_sync_root(&self, root: Id) -> Result<(), Error> {
        self.transact(move |transaction, _, _| {
            transaction.execute(
                "DELETE FROM sync_root WHERE root_id = ?1",
                [root.as_bytes()],
            )?;
            Ok(())
        })
    }
}
