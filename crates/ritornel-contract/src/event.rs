//! Events and subscriptions. See decisions 11 and 12 and "Events and
//! subscriptions" in docs/design/contract.md.
//!
//! An event is a notification whose method is [`EVENT_METHOD`] and whose
//! params are an [`Event`]: a `topic`, and what that topic carries. It
//! never carries a delta. It is a full copy of one small resource stamped
//! with its revision, so missing one, or seeing two out of order, is
//! harmless. There is no replay.

use serde::{Deserialize, Serialize};

use crate::{Job, ServerInfo};

/// The method of every event notification.
pub const EVENT_METHOD: &str = "event";

/// What a client can subscribe to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum Topic {
    /// One server's full status, whenever it changes.
    Servers,
    /// One job's full summary, whenever it changes: at most four times a
    /// second for one job.
    Jobs,
    /// The library changed: a hint to read again what is on screen that is
    /// not a view.
    Library,
    /// A view this session holds may be out of date. Holding a view is the
    /// subscription: no session subscribes to this topic to get it.
    List,
}

impl Topic {
    /// Every topic.
    pub const ALL: &[Self] = &[Self::Servers, Self::Jobs, Self::Library, Self::List];

    /// The topic as it is written on the wire.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Servers => "servers",
            Self::Jobs => "jobs",
            Self::Library => "library",
            Self::List => "list",
        }
    }
}

/// The params of an event notification: the topic, and what it carries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(tag = "topic", rename_all = "snake_case")]
pub enum Event {
    /// One server's status changed.
    Servers(ServersEvent),
    /// One job changed.
    Jobs(Box<JobsEvent>),
    /// The library changed.
    Library(LibraryEvent),
    /// A view may be out of date.
    List(ListEvent),
}

impl Event {
    /// The topic the event is on.
    pub const fn topic(&self) -> Topic {
        match self {
            Self::Servers(_) => Topic::Servers,
            Self::Jobs(_) => Topic::Jobs,
            Self::Library(_) => Topic::Library,
            Self::List(_) => Topic::List,
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
}

impl LibraryChange {
    /// Every kind of change, in order.
    pub const ALL: &[Self] = &[Self::Membership, Self::Metadata, Self::Stats];
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
    /// The topics to add or to remove. Naming one twice, adding one the
    /// session has, or removing one it has not, changes nothing.
    pub topics: Vec<Topic>,
}

/// The result of `events.subscribe` and `events.unsubscribe`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct Subscriptions {
    /// Every topic the session is subscribed to after the call.
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
