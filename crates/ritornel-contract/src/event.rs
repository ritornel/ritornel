//! Events and subscriptions. See "Events and subscriptions" in
//! docs/internals/contract.md.
//!
//! An event is a notification whose method is [`EVENT_METHOD`] and whose
//! params are an [`Event`]: a `topic`, and what that topic carries. It
//! never carries a delta. It is a full copy of one small resource stamped
//! with its revision, so missing one, or seeing two out of order, is
//! harmless. There is no replay.

use serde::{Deserialize, Serialize};

use crate::{
    Id, Job, Permission, PlaybackEvent, ProgressEvent, QueueEvent, ServerInfo, SyncDevicesEvent,
};

/// The method of every event notification.
pub const EVENT_METHOD: &str = "event";

/// What a client can subscribe to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum Topic {
    /// One server's full status, whenever it changes.
    Servers,
    /// A place to play changed. Read `players.list` again.
    Players,
    /// One job's full summary, whenever it changes: at most four times a
    /// second for one job. Subscribing requires `status:read`.
    Jobs,
    /// The library changed: a hint to read again what is on screen that is
    /// not a view.
    Library,
    /// A view this session holds may be out of date. Holding a view is the
    /// subscription: no session subscribes to this topic to get it.
    List,
    /// The player's state, whole, whenever it changes.
    Playback,
    /// Where the playing entry is, about once a second.
    Progress,
    /// A hint that the queue changed, with its revision.
    Queue,
    /// A player or folder the library syncs to changed: a hint to read it
    /// again, at most four times a second for one target.
    Devices,
}

impl Topic {
    /// Whether this topic can be followed for a particular player.
    pub const fn is_per_player(self) -> bool {
        matches!(self, Self::Playback | Self::Progress | Self::Queue)
    }

    /// Every topic.
    pub const ALL: &[Self] = &[
        Self::Servers,
        Self::Players,
        Self::Jobs,
        Self::Library,
        Self::List,
        Self::Playback,
        Self::Progress,
        Self::Queue,
        Self::Devices,
    ];

    /// Whether a session with `permissions` may subscribe to this topic.
    pub const fn any_permission(self) -> &'static [Permission] {
        match self {
            Self::Servers => &[Permission::StatusRead, Permission::LibraryRead],
            Self::Players => &[
                Permission::LibraryRead,
                Permission::PlaybackControl,
                Permission::StatusRead,
            ],
            Self::Jobs => &[Permission::StatusRead],
            Self::Library | Self::List | Self::Devices => &[Permission::LibraryRead],
            Self::Playback | Self::Progress | Self::Queue => &[Permission::PlaybackControl],
        }
    }

    /// The topic as it is written on the wire.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Servers => "servers",
            Self::Players => "players",
            Self::Jobs => "jobs",
            Self::Library => "library",
            Self::List => "list",
            Self::Playback => "playback",
            Self::Progress => "progress",
            Self::Queue => "queue",
            Self::Devices => "devices",
        }
    }
}

/// The params of an event notification: the topic, and what it carries.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(tag = "topic", rename_all = "snake_case")]
pub enum Event {
    /// One server's status changed.
    Servers(ServersEvent),
    /// A hint to read the places to play again.
    Players,
    /// One job changed.
    Jobs(Box<JobsEvent>),
    /// The library changed.
    Library(LibraryEvent),
    /// A view may be out of date.
    List(ListEvent),
    /// The player's state changed.
    Playback(Box<PlaybackEvent>),
    /// Where the playing entry is.
    Progress(ProgressEvent),
    /// The queue changed.
    Queue(QueueEvent),
    /// A sync target changed.
    Devices(Box<SyncDevicesEvent>),
}

impl Event {
    /// The player named on this event, or none for a local or unscoped event.
    pub fn player(&self) -> Option<Id> {
        match self {
            Self::Playback(e) => e.player,
            Self::Progress(e) => e.player,
            Self::Queue(e) => e.player,
            _ => None,
        }
    }

    /// Expresses a player event relative to the server delivering it.
    pub fn set_player(&mut self, player: Option<Id>) {
        match self {
            Self::Playback(e) => e.player = player,
            Self::Progress(e) => e.player = player,
            Self::Queue(e) => e.player = player,
            _ => {}
        }
    }

    /// The topic the event is on.
    pub const fn topic(&self) -> Topic {
        match self {
            Self::Players => Topic::Players,
            Self::Servers(_) => Topic::Servers,
            Self::Jobs(_) => Topic::Jobs,
            Self::Library(_) => Topic::Library,
            Self::List(_) => Topic::List,
            Self::Playback(_) => Topic::Playback,
            Self::Progress(_) => Topic::Progress,
            Self::Queue(_) => Topic::Queue,
            Self::Devices(_) => Topic::Devices,
        }
    }
}

/// What kind of change a library write made, for a client deciding what to
/// read again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum LibraryChange {
    /// Something was added or taken away.
    Membership,
    /// What something says changed: a title, a year, a cover.
    Metadata,
    /// Counts changed, such as plays.
    Stats,
    /// What the listener says of a song changed: its stars.
    Ratings,
}

impl LibraryChange {
    /// Every kind of change, in order.
    pub const ALL: &[Self] = &[Self::Membership, Self::Metadata, Self::Stats, Self::Ratings];
}

/// What an event on the `library` topic carries. Events that were not yet
/// sent are merged: the highest revision, and every kind of change either
/// had.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct LibraryEvent {
    /// The library revision after the change. A client that has read
    /// something at this revision or a later one may keep it.
    pub library_revision: u64,
    /// What kind of change it was, in order and each once.
    pub changed: Vec<LibraryChange>,
}

/// What an event on the `list` topic carries: one view this session holds
/// may be out of date.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ListEvent {
    /// The view.
    pub view: String,
    /// The library revision after the change.
    pub library_revision: u64,
    /// `false`: only what rows say changed, and the client reads the
    /// windows on screen again in place. `true`: which rows the view should
    /// hold, or their order, may have changed, and the client opens a view
    /// that replaces it.
    pub reorder: bool,
}

/// What an event on the `servers` topic carries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ServersEvent {
    /// The server's status, whole. The client replaces its copy if this
    /// one's revision is higher.
    pub server: ServerInfo,
}

/// What an event on the `jobs` topic carries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct JobsEvent {
    /// The job, whole, with its target. The client replaces its copy.
    pub job: Job,
}

/// The params of `events.subscribe` and `events.unsubscribe`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct TopicsParams {
    /// The selected player. Omitted for this server's own player.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub player: Option<Id>,
    /// The topics to add or to remove. Naming one twice, adding one the
    /// session has, or removing one it has not, changes nothing.
    pub topics: Vec<Topic>,
}

/// The result of `events.subscribe` and `events.unsubscribe`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct Subscriptions {
    /// The player ID from the request, if supplied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub player: Option<Id>,
    /// Topics held for the requested player, or local topics when the request omitted the player.
    pub topics: Vec<Topic>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{Id, ServerRole};

    #[test]
    fn an_event_is_its_topic_and_a_full_copy_of_what_changed() {
        let event = Event::Servers(ServersEvent {
            server: ServerInfo {
                server_id: Id::from_bytes([0x11; 16]),
                name: "desk".to_owned(),
                role: ServerRole::Unassigned,
                is_this_server: true,
                reachable: true,
                revision: 3,
            },
        });
        let wire = json!({
            "topic": "servers",
            "server": {
                "serverId": "11111111-1111-1111-1111-111111111111",
                "name": "desk",
                "role": "unassigned",
                "isThisServer": true,
                "reachable": true,
                "revision": 3,
            },
        });
        assert_eq!(serde_json::to_value(&event).unwrap(), wire);
        assert_eq!(serde_json::from_value::<Event>(wire).unwrap(), event);
        assert_eq!(event.topic(), Topic::Servers);
    }

    #[test]
    fn topics_are_written_in_snake_case() {
        for topic in Topic::ALL {
            assert_eq!(serde_json::to_value(topic).unwrap(), topic.as_str());
        }
        let params = TopicsParams {
            player: None,
            topics: vec![Topic::Servers],
        };
        assert_eq!(
            serde_json::to_value(&params).unwrap(),
            json!({ "topics": ["servers"] })
        );
        assert!(serde_json::from_value::<TopicsParams>(json!({ "topics": ["weather"] })).is_err());
    }

    #[test]
    fn a_library_event_says_what_changed_and_a_list_event_names_its_view() {
        let library = Event::Library(LibraryEvent {
            library_revision: 4711,
            changed: vec![LibraryChange::Membership, LibraryChange::Metadata],
        });
        assert_eq!(
            serde_json::to_value(&library).unwrap(),
            json!({ "topic": "library", "libraryRevision": 4711, "changed": ["membership", "metadata"] })
        );
        let list = Event::List(ListEvent {
            view: "v7".to_owned(),
            library_revision: 4711,
            reorder: true,
        });
        let wire =
            json!({ "topic": "list", "view": "v7", "libraryRevision": 4711, "reorder": true });
        assert_eq!(serde_json::to_value(&list).unwrap(), wire);
        assert_eq!(serde_json::from_value::<Event>(wire).unwrap(), list);
        assert_eq!(list.topic(), Topic::List);
    }
}
