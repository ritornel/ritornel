//! The rows of a plan or of a run, as a page reads them: grouped by album
//! or one for each track, filtered by kind and by state. A plan's rows are
//! its preview; a run's are its stored actions, so after a run the done,
//! skipped, failed and in-doubt rows are all still there to read.

use ritornel_contract::{
    SyncActionError, SyncActionKind, SyncActionRow, SyncActionState, SyncActions,
    SyncActionsParams, SyncGroupBy, SyncHow,
};

use super::plan::{Plan, Shown};
use super::store::{ActionRow, Stored};
use crate::Id;

/// One row before it is paged.
#[derive(Debug, Clone)]
pub struct Item {
    id: String,
    kind: SyncActionKind,
    reason: String,
    source: Option<Id>,
    shown: Shown,
    path: Option<String>,
    codec: Option<String>,
    how: Option<SyncHow>,
    bytes: u64,
    estimated: bool,
    state: SyncActionState,
    bytes_done: u64,
    error: Option<SyncActionError>,
}

/// A plan's rows: every one waiting.
pub fn of_plan(plan: &Plan) -> Vec<Item> {
    plan.actions
        .iter()
        .enumerate()
        .map(|(at, action)| Item {
            id: at.to_string(),
            kind: action.kind,
            reason: action.reason.to_owned(),
            source: action.source,
            shown: action.shown.clone(),
            path: action.path.as_ref().map(|path| path.as_str().to_owned()),
            codec: action.codec().map(str::to_owned),
            how: action.how(),
            bytes: action.bytes,
            estimated: action.estimated,
            state: SyncActionState::Waiting,
            bytes_done: 0,
            error: None,
        })
        .collect()
}

/// A run's rows. `live` says where an action being worked on stands and
/// how many of its bytes are sent, by its place in the run.
pub fn of_run(
    actions: Vec<ActionRow>,
    running: bool,
    live: impl Fn(i64) -> Option<(SyncActionState, u64)>,
) -> Vec<Item> {
    actions
        .into_iter()
        .map(|action| {
            let (state, bytes_done) = match action.state {
                Stored::Done => (SyncActionState::Done, action.bytes),
                Stored::Failed | Stored::Conflict => (SyncActionState::Failed, 0),
                Stored::Skipped => (SyncActionState::Skipped, 0),
                // Asked of the device by a run that has ended: in doubt
                // until the next read checks it.
                Stored::Issued if !running => (SyncActionState::InDoubt, 0),
                Stored::Planned if !running => (SyncActionState::Skipped, 0),
                Stored::Issued | Stored::Planned => {
                    live(action.seq).unwrap_or((SyncActionState::Waiting, 0))
                }
            };
            Item {
                id: action.seq.to_string(),
                kind: action.kind,
                reason: action.reason,
                source: action.source,
                shown: action.shown,
                path: action.path,
                codec: action.codec,
                how: action.how,
                bytes: action.bytes,
                estimated: action.estimated,
                state,
                bytes_done,
                error: action
                    .error
                    .map(|(code, detail)| SyncActionError { code, detail }),
            }
        })
        .collect()
}

fn track(item: &Item) -> SyncActionRow {
    SyncActionRow {
        action_id: Some(item.id.clone()),
        group_id: None,
        kind: item.kind,
        reason: item.reason.clone(),
        source_id: item.source,
        title: item.shown.title.clone(),
        artist: item.shown.artist.clone(),
        art_id: item.shown.art,
        tracks: None,
        album_tracks: None,
        codec: item.codec.clone(),
        how: item.how,
        bytes: item.bytes,
        estimated: item.estimated,
        state: item.state,
        bytes_done: item.bytes_done,
        error: item.error.clone(),
        path: item.path.clone(),
    }
}

fn kind_word(kind: SyncActionKind) -> &'static str {
    match kind {
        SyncActionKind::Add => "add",
        SyncActionKind::Replace => "replace",
        SyncActionKind::Remove => "remove",
        SyncActionKind::Conflict => "conflict",
        SyncActionKind::Blocked => "blocked",
    }
}

/// A group's state: the first that holds of working, failed, waiting, in
/// doubt, done, skipped.
fn group_state(members: &[&Item]) -> SyncActionState {
    let any = |state: SyncActionState| members.iter().any(|item| item.state == state);
    [
        SyncActionState::Copying,
        SyncActionState::Preparing,
        SyncActionState::Failed,
        SyncActionState::InDoubt,
    ]
    .into_iter()
    .find(|state| any(*state))
    .unwrap_or_else(|| {
        if any(SyncActionState::Waiting) {
            // Part way through an album it is being worked on.
            if any(SyncActionState::Done) {
                SyncActionState::Copying
            } else {
                SyncActionState::Waiting
            }
        } else if any(SyncActionState::Done) {
            SyncActionState::Done
        } else {
            SyncActionState::Skipped
        }
    })
}

/// One page of `items`, as `devices.actions` asks for it.
pub fn page(items: &[Item], params: &SyncActionsParams) -> SyncActions {
    let wanted: Vec<&Item> = items
        .iter()
        .filter(|item| params.kind.is_none_or(|kind| item.kind == kind))
        .filter(|item| params.state.is_none_or(|state| item.state == state))
        .collect();
    let group_id = |item: &Item| format!("{}:{}", kind_word(item.kind), item.shown.group_key);
    let rows: Vec<SyncActionRow> = match (&params.parent_id, params.group_by) {
        (Some(parent), _) => wanted
            .iter()
            .filter(|item| group_id(item) == *parent)
            .map(|item| track(item))
            .collect(),
        (None, SyncGroupBy::Track) => wanted.iter().map(|item| track(item)).collect(),
        (None, SyncGroupBy::Album) => {
            // Groups in the order their first track comes.
            let mut order: Vec<String> = Vec::new();
            let mut groups: std::collections::HashMap<String, Vec<&Item>> =
                std::collections::HashMap::new();
            for item in &wanted {
                let id = group_id(item);
                let members = groups.entry(id.clone()).or_default();
                if members.is_empty() {
                    order.push(id);
                }
                members.push(item);
            }
            order
                .into_iter()
                .filter_map(|id| {
                    let members = groups.remove(&id)?;
                    let first = members.first()?;
                    let same = |pick: fn(&Item) -> Option<&str>| {
                        let value = pick(first);
                        members
                            .iter()
                            .all(|item| pick(item) == value)
                            .then(|| value.map(str::to_owned))
                            .flatten()
                    };
                    Some(SyncActionRow {
                        action_id: None,
                        group_id: Some(id),
                        kind: first.kind,
                        reason: same(|item| Some(item.reason.as_str())).unwrap_or_default(),
                        source_id: first.source,
                        title: first.shown.album.clone(),
                        artist: first.shown.album_artist.clone(),
                        art_id: first.shown.art,
                        tracks: u32::try_from(members.len()).ok(),
                        album_tracks: Some(first.shown.album_tracks),
                        codec: same(|item| item.codec.as_deref()),
                        how: first
                            .how
                            .filter(|how| members.iter().all(|item| item.how == Some(*how))),
                        bytes: members.iter().map(|item| item.bytes).sum(),
                        estimated: members.iter().any(|item| item.estimated),
                        state: group_state(&members),
                        bytes_done: members.iter().map(|item| item.bytes_done).sum(),
                        error: members.iter().find_map(|item| item.error.clone()),
                        path: None,
                    })
                })
                .collect()
        }
    };
    let total = rows.len() as u64;
    let limit = params.limit.clamp(1, 500) as usize;
    SyncActions {
        total,
        offset: params.offset,
        rows: rows
            .into_iter()
            .skip(params.offset as usize)
            .take(limit)
            .collect(),
    }
}
