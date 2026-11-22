//! When a hub keeps its machine from sleeping. A sleeping hub is one its
//! satellites cannot reach, but a satellite or a player stays linked for
//! good, so being linked is not reason enough: the hub stays awake while a
//! linked server is active, and lets the machine sleep otherwise.
//! [`Activity::keeps_awake`] is the rule, and the one place it is written.
//! See "Keeping a hub awake" in docs/internals/server.md.

use std::collections::HashMap;
use std::time::Duration;

use ritornel_contract::Id;
use tracing::{info, warn};

/// How long after a linked server's last call the hub stays awake, unless
/// `awake_minutes` says otherwise.
pub(crate) const DEFAULT_WINDOW: Duration = Duration::from_secs(30 * 60);

/// How long a linked server's word that it plays holds without another. A
/// satellite says so every ten seconds, so this is three missed.
const PLAYING_HOLDS_MS: i64 = 30_000;

/// How often the hub asks whether it may let its machine sleep again.
pub(crate) const CHECK_EVERY: Duration = Duration::from_secs(60);

/// How long the hub waits to ask the system again after it refused.
const ASK_AGAIN_MS: i64 = 60_000;

/// Whether a linked server's call to `method` is activity. Saying hello,
/// subscribing to events and pinging are how a link keeps itself, and a
/// satellite does them whenever its connection comes back, so they keep
/// nothing awake.
pub(crate) fn is_activity(method: &str) -> bool {
    !matches!(
        method,
        "server.hello" | "events.subscribe" | "events.unsubscribe" | "link.ping"
    )
}

/// What a hub knows of its linked servers' activity.
#[derive(Debug)]
pub(crate) struct Activity {
    window_ms: i64,
    /// Each linked server's last call, but for its pings, in Unix
    /// milliseconds.
    called: HashMap<Id, i64>,
    /// Until when each linked server that said it plays is taken to play.
    playing: HashMap<Id, i64>,
}

impl Activity {
    pub(crate) fn new(window: Duration) -> Self {
        Self {
            window_ms: i64::try_from(window.as_millis()).unwrap_or(i64::MAX),
            called: HashMap::new(),
            playing: HashMap::new(),
        }
    }

    /// The linked server `server` made a call at `now_ms`.
    pub(crate) fn called(&mut self, server: Id, now_ms: i64) {
        self.called.insert(server, now_ms);
    }

    /// The linked server `server` said at `now_ms` whether it plays.
    pub(crate) fn pinged(&mut self, server: Id, playing: bool, now_ms: i64) {
        if playing {
            self.playing
                .insert(server, now_ms.saturating_add(PLAYING_HOLDS_MS));
        } else {
            self.playing.remove(&server);
        }
    }

    /// The rule: the hub stays awake while a linked server plays, or has
    /// made a call within the window.
    pub(crate) fn keeps_awake(&self, now_ms: i64) -> bool {
        self.playing.values().any(|until| now_ms < *until)
            || self
                .called
                .values()
                .any(|at| now_ms.saturating_sub(*at) < self.window_ms)
    }
}

/// A hub's activity, and what keeps its machine awake while the rule says
/// so.
#[derive(Debug)]
pub(crate) struct Awake {
    pub(crate) activity: Activity,
    held: Option<crate::platform::Awake>,
    /// When the system last refused to keep the machine awake.
    refused_at_ms: Option<i64>,
}

impl Awake {
    pub(crate) fn new(window: Duration) -> Self {
        Self {
            activity: Activity::new(window),
            held: None,
            refused_at_ms: None,
        }
    }

    /// Holds the machine awake, or lets it go, as the rule says at
    /// `now_ms`, and says whether it is held.
    pub(crate) fn follow_the_rule(&mut self, now_ms: i64) -> bool {
        // A helper the system refused has ended, and holds nothing.
        if let Some(held) = &mut self.held
            && !held.holds()
        {
            self.held = None;
            self.refused_at_ms = Some(now_ms);
            warn!(
                "the system did not keep this machine from sleeping for a linked server: keep it \
                 awake by its own settings"
            );
        }
        let wanted = self.activity.keeps_awake(now_ms);
        let may_ask = self
            .refused_at_ms
            .is_none_or(|at| now_ms.saturating_sub(at) >= ASK_AGAIN_MS);
        if wanted && self.held.is_none() && may_ask {
            match crate::platform::stay_awake("a linked server is active") {
                Ok(held) => {
                    info!(
                        "a linked server is active: the system is asked to keep this machine from sleeping"
                    );
                    self.held = Some(held);
                }
                Err(error) => {
                    self.refused_at_ms = Some(now_ms);
                    warn!(
                        %error,
                        "a linked server is active and this machine may sleep: keep it awake by its own settings"
                    );
                }
            }
        } else if !wanted && let Some(held) = self.held.take() {
            drop(held);
            info!("no linked server is active: the machine may sleep");
        }
        self.held.is_some()
    }
}

/// Lets a hub's machine sleep again once no linked server is active, for
/// as long as the server runs.
pub(crate) async fn check(core: std::sync::Arc<crate::dispatch::Core>) {
    loop {
        tokio::time::sleep(CHECK_EVERY).await;
        core.check_awake();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINUTE: i64 = 60_000;

    fn server(n: u8) -> Id {
        Id::from_bytes([n; 16])
    }

    #[test]
    fn keeping_the_link_is_not_activity() {
        for kept in ["server.hello", "events.subscribe", "link.ping"] {
            assert!(!is_activity(kept), "{kept}");
        }
        for used in ["list.open", "link.copies", "link.listen", "servers.list"] {
            assert!(is_activity(used), "{used}");
        }
    }

    #[test]
    fn a_call_keeps_the_hub_awake_for_the_window_and_no_longer() {
        let mut activity = Activity::new(Duration::from_secs(30 * 60));
        assert!(!activity.keeps_awake(0), "linked alone is not active");
        activity.called(server(1), 1_000);
        assert!(activity.keeps_awake(1_000 + 30 * MINUTE - 1));
        assert!(!activity.keeps_awake(1_000 + 30 * MINUTE));
        activity.called(server(2), 20 * MINUTE);
        assert!(activity.keeps_awake(1_000 + 30 * MINUTE));
    }

    #[test]
    fn a_server_that_plays_keeps_the_hub_awake_while_it_says_so() {
        let mut activity = Activity::new(Duration::ZERO);
        activity.pinged(server(1), true, 0);
        assert!(activity.keeps_awake(PLAYING_HOLDS_MS - 1));
        activity.pinged(server(1), true, PLAYING_HOLDS_MS - 1);
        assert!(activity.keeps_awake(2 * PLAYING_HOLDS_MS - 2));
        // A server that stops saying so stops keeping it, and one that
        // says it has stopped does at once.
        assert!(!activity.keeps_awake(2 * PLAYING_HOLDS_MS));
        let mut activity = Activity::new(Duration::ZERO);
        activity.pinged(server(2), true, 0);
        activity.pinged(server(2), false, 1);
        assert!(!activity.keeps_awake(2));
    }
}
