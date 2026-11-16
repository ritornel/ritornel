//! Events and subscriptions. See decisions 11 and 12 and "Events and
//! subscriptions" in docs/design/contract.md.
//!
//! An event is a notification whose method is [`EVENT_METHOD`] and whose
//! params are an [`Event`]: a `topic`, and what that topic carries. It
//! never carries a delta. It is a full copy of one small resource stamped
//! with its revision, so missing one, or seeing two out of order, is
//! harmless. There is no replay.

use serde::{Deserialize, Serialize};

use crate::ServerInfo;

/// The method of every event notification.
pub const EVENT_METHOD: &str = "event";

/// What a client can subscribe to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum Topic {
    /// One server's full status, whenever it changes.
    Servers,
}

impl Topic {
    /// Every topic.
    pub const ALL: &[Self] = &[Self::Servers];

    /// The topic as it is written on the wire.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Servers => "servers",
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
}

impl Event {
    /// The topic the event is on.
    pub const fn topic(&self) -> Topic {
        match self {
            Self::Servers(_) => Topic::Servers,
        }
    }
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
}
