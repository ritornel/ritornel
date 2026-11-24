//! What the app says while the computer its library is on does not answer:
//! the server it runs with is a satellite, and its hub has gone.
//!
//! It says nothing for a few seconds, since a hub that is back at once
//! needs no word. Then a notice says so, once, and another when it is
//! back. Meanwhile the toolbar's connection shows it, and hovering it says
//! what is wrong, since when and what still works. The library is never
//! emptied: a wall already drawn stays, and one that could not be read
//! says why in its place.

use ritornel_contract::HubStatus;

use crate::net::Net;
use crate::settings::words::{Clock, Style, when};

/// How long the hub may be away before anything is said.
pub(crate) const QUIET_MS: i64 = 5_000;

/// The hub, away.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Away {
    /// The hub's name.
    pub hub: String,
    /// Since when it has not answered, in Unix milliseconds.
    pub since_ms: i64,
}

/// Now, read in this machine's zone and locale.
pub(crate) fn clock() -> Clock {
    Clock {
        now: now_ms(),
        zone: jiff::tz::TimeZone::system(),
        style: Style::of_locale(&crate::platform::locale().unwrap_or_else(|| "en-US".to_owned())),
    }
}

/// Now, in Unix milliseconds.
pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| i64::try_from(since.as_millis()).unwrap_or(0))
}

/// The hub of the server `net` talks to, if it has one and it does not
/// answer.
pub(crate) fn away(net: &Net) -> Option<Away> {
    away_of(net.hub.as_ref())
}

/// Whether the hub, away in `before`, answers in `now`: what its absence
/// kept from being read is read again, once.
pub(crate) fn back(before: &Net, now: &Net) -> bool {
    away(before).is_some() && away(now).is_none()
}

fn away_of(hub: Option<&HubStatus>) -> Option<Away> {
    let hub = hub?;
    (hub.answers == Some(false)).then(|| Away {
        hub: hub.name.clone(),
        since_ms: hub.since_ms.unwrap_or(0),
    })
}

/// What the notice says of `hub` at `now_ms`, if anything, given the
/// absence it has said already (the time the hub went), and the absence
/// said after. One absence is said once, however often this is asked, and
/// its end once.
pub(crate) fn notice(
    said: Option<i64>,
    hub: Option<&HubStatus>,
    now_ms: i64,
) -> (Option<String>, Option<i64>) {
    match away_of(hub) {
        Some(away) if away.said(now_ms) && said != Some(away.since_ms) => {
            (Some(away.toast()), Some(away.since_ms))
        }
        None if said.is_some() => (hub.map(|hub| back_toast(&hub.name)), None),
        _ => (None, said),
    }
}

/// The same, once it has been away long enough to say so.
pub(crate) fn said(net: &Net) -> Option<Away> {
    away(net).filter(|away| away.said(now_ms()))
}

impl Away {
    /// Whether it has been away long enough to say so at `now_ms`.
    pub(crate) fn said(&self, now_ms: i64) -> bool {
        now_ms.saturating_sub(self.since_ms) >= QUIET_MS
    }

    /// How long until it is said, in milliseconds, if it is not yet.
    pub(crate) fn wait_ms(&self, now_ms: i64) -> Option<u64> {
        let left = QUIET_MS - now_ms.saturating_sub(self.since_ms);
        u64::try_from(left).ok().filter(|left| *left > 0)
    }

    /// Since when, as a person reads it: a time today, or a day and a time.
    fn since(&self, clock: &Clock) -> String {
        let at = when(self.since_ms, clock);
        match at.strip_prefix("today at ") {
            Some(time) => time.to_owned(),
            None => at,
        }
    }

    /// The notice when the hub has gone.
    pub(crate) fn toast(&self) -> String {
        format!("Can’t reach {}. Waiting for it to come back.", self.hub)
    }

    /// What hovering the toolbar's connection says: what is wrong, then
    /// since when and what still works.
    pub(crate) fn card(&self, clock: &Clock) -> (String, String) {
        (
            format!("Can’t reach {}", self.hub),
            format!(
                "No connection since {}. What’s playing keeps going, and your listens are \
                 saved until it’s back.",
                self.since(clock)
            ),
        )
    }

    /// What the library says in its place when it could not be read.
    pub(crate) fn blank(&self) -> (String, String) {
        (
            format!("Can’t reach {}.", self.hub),
            format!("Your library will be here as soon as {} is back.", self.hub),
        )
    }
}

/// The notice when the hub is back.
pub(crate) fn back_toast(hub: &str) -> String {
    format!("{hub} is back.")
}

/// What hovering the toolbar's connection says while the hub answers.
pub(crate) fn fine_card(hub: &str) -> (String, String) {
    (
        format!("Connected to {hub}"),
        "Your library and listening history are kept there.".to_owned(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_is_said_for_the_first_seconds() {
        let away = Away {
            hub: "Studio Mac".to_owned(),
            since_ms: 1_000_000,
        };
        assert!(!away.said(1_000_000 + QUIET_MS - 1));
        assert_eq!(away.wait_ms(1_000_000 + 1_000), Some(4_000));
        assert!(away.said(1_000_000 + QUIET_MS));
        assert_eq!(away.wait_ms(1_000_000 + QUIET_MS), None);
    }

    fn hub(answers: bool, since_ms: i64) -> HubStatus {
        HubStatus {
            server_id: ritornel_contract::Id::from_bytes([1; 16]),
            name: "Studio Mac".to_owned(),
            library_id: ritornel_contract::Id::from_bytes([2; 16]),
            epoch: 1,
            routes: Vec::new(),
            linked_at_ms: 0,
            answers: Some(answers),
            since_ms: Some(since_ms),
            route: None,
            problem: None,
        }
    }

    #[test]
    fn one_absence_is_said_once_and_its_end_once_across_reconnecting() {
        let went = 1_000_000;
        let later = went + QUIET_MS;
        let away = hub(false, went);
        assert_eq!(notice(None, Some(&away), went + 1), (None, None));
        let (word, said) = notice(None, Some(&away), later);
        assert_eq!(
            word.as_deref(),
            Some("Can’t reach Studio Mac. Waiting for it to come back.")
        );
        assert_eq!(said, Some(went));
        // The app's own connection came back while the hub is still away.
        assert_eq!(notice(said, Some(&away), later + 1), (None, said));
        // It came back after the hub did.
        let back = hub(true, later + 2);
        let (word, said) = notice(said, Some(&back), later + 3);
        assert_eq!(word.as_deref(), Some("Studio Mac is back."));
        assert_eq!(said, None);
        // Another absence is another word.
        let again = hub(false, later + 10);
        assert_eq!(
            notice(None, Some(&again), later + 10 + QUIET_MS).1,
            Some(later + 10)
        );
    }

    #[tokio::test]
    async fn the_hub_is_back_only_from_away_to_answering() {
        let (send, _calls) = tokio::sync::mpsc::channel(1);
        let (_answers, receive) = tokio::sync::mpsc::channel(1);
        let client = std::sync::Arc::new(ritornel_client::Client::over_memory(send, receive));
        let session = |answers| Net::satellite(std::sync::Arc::clone(&client), answers);
        assert!(back(&session(false), &session(true)));
        assert!(!back(&session(true), &session(true)));
        assert!(!back(&session(false), &session(false)));
        assert!(!back(&session(true), &session(false)));
    }
}
