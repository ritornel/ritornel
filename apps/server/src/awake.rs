//! When a hub keeps its machine from sleeping. A sleeping hub is one its
//! satellites cannot reach, but a satellite or a player stays linked for
//! good, so being linked is not reason enough. The owner chooses, with
//! [`Awake`]: while a linked server is active, the default; always; or
//! never. [`keeps_awake`] is the rule, and the one place it is written.
//! See "Keeping a hub awake" in docs/internals/server.md.

use std::collections::HashMap;
use std::time::Duration;

use ritornel_contract::{Awake, Id};
use tracing::{info, warn};

/// How long a linked server's word that it plays holds without another. A
/// satellite says so every ten seconds, so this is three missed.
const PLAYING_HOLDS_MS: i64 = 30_000;

/// How often the hub asks whether it may let its machine sleep again.
pub(crate) const CHECK_EVERY: Duration = Duration::from_secs(60);

/// How long the hub waits to ask the system again after it refused.
const ASK_AGAIN_MS: i64 = 60_000;

/// The rule: whether the machine is kept awake at `now_ms`, as `setting`
/// says, given what the linked servers have done.
pub(crate) fn keeps_awake(setting: Awake, activity: &Activity, now_ms: i64) -> bool {
    match setting {
        Awake::WhileActive { minutes } => {
            activity.plays(now_ms) || activity.called_within(i64::from(minutes) * 60_000, now_ms)
        }
        Awake::Always => true,
        Awake::Never => false,
    }
}

/// Whether a linked server's call to `method` is activity. Saying hello,
/// subscribing to events and pinging are how a link keeps itself, and a
/// satellite does them whenever its connection comes back, so they keep
/// nothing awake.
pub(crate) fn is_activity(method: &str) -> bool {
    !matches!(
        method,
        "server.hello" | "events.subscribe" | "events.unsubscribe" | "link.ping" | "link.event"
    )
}

/// What a hub knows of its linked servers' activity.
#[derive(Debug, Default)]
pub(crate) struct Activity {
    /// Each linked server's last call that is activity, in Unix
    /// milliseconds.
    called: HashMap<Id, i64>,
    /// Until when each linked server that said it plays is taken to play.
    playing: HashMap<Id, i64>,
}

impl Activity {
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

    fn plays(&self, now_ms: i64) -> bool {
        self.playing.values().any(|until| now_ms < *until)
    }

    fn called_within(&self, window_ms: i64, now_ms: i64) -> bool {
        self.called
            .values()
            .any(|at| now_ms.saturating_sub(*at) < window_ms)
    }
}

/// A hub's wake setting, its linked servers' activity, and what keeps the
/// machine awake while the rule says so.
#[derive(Debug, Default)]
pub(crate) struct KeptAwake {
    pub(crate) setting: Awake,
    /// Whether `setting` is one a call stored, and not the default.
    pub(crate) set_by_call: bool,
    pub(crate) activity: Activity,
    held: Option<crate::platform::Awake>,
    /// Whether this run asks the system at all (`Run::keep_awake`).
    pub(crate) asks: bool,
    /// When the system last refused to keep the machine awake.
    refused_at_ms: Option<i64>,
}

impl KeptAwake {
    /// Whether the machine is kept awake now.
    pub(crate) fn held(&self) -> bool {
        self.held.is_some()
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
                "the system did not keep this machine from sleeping: keep it awake by its own \
                 settings"
            );
        }
        let wanted = keeps_awake(self.setting, &self.activity, now_ms);
        let may_ask = self
            .refused_at_ms
            .is_none_or(|at| now_ms.saturating_sub(at) >= ASK_AGAIN_MS);
        if wanted && self.held.is_none() && may_ask && self.asks {
            match crate::platform::stay_awake(
                "Ritornel keeps this hub awake for its linked servers",
            ) {
                Ok(held) => {
                    info!(setting = ?self.setting, "the system is asked to keep this machine from sleeping");
                    self.held = Some(held);
                }
                Err(error) => {
                    self.refused_at_ms = Some(now_ms);
                    warn!(
                        %error,
                        "this machine may sleep: keep it awake by its own settings"
                    );
                }
            }
        } else if !wanted && let Some(held) = self.held.take() {
            drop(held);
            info!(setting = ?self.setting, "the machine may sleep");
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
    const WHILE_ACTIVE: Awake = Awake::WhileActive { minutes: 30 };

    fn server(n: u8) -> Id {
        Id::from_bytes([n; 16])
    }

    #[test]
    fn keeping_the_link_is_not_activity() {
        for kept in [
            "server.hello",
            "events.subscribe",
            "link.ping",
            "link.event",
        ] {
            assert!(!is_activity(kept), "{kept}");
        }
        for used in ["list.open", "link.copies", "link.listen", "servers.list"] {
            assert!(is_activity(used), "{used}");
        }
    }

    #[test]
    fn the_default_is_while_active_for_thirty_minutes() {
        assert_eq!(Awake::default(), WHILE_ACTIVE);
    }

    #[test]
    fn a_call_keeps_the_hub_awake_for_the_window_and_no_longer() {
        let mut activity = Activity::default();
        assert!(
            !keeps_awake(WHILE_ACTIVE, &activity, 0),
            "linked alone is not active"
        );
        activity.called(server(1), 1_000);
        assert!(keeps_awake(
            WHILE_ACTIVE,
            &activity,
            1_000 + 30 * MINUTE - 1
        ));
        assert!(!keeps_awake(WHILE_ACTIVE, &activity, 1_000 + 30 * MINUTE));
        activity.called(server(2), 20 * MINUTE);
        assert!(keeps_awake(WHILE_ACTIVE, &activity, 1_000 + 30 * MINUTE));
        let five = Awake::WhileActive { minutes: 5 };
        assert!(!keeps_awake(five, &activity, 25 * MINUTE));
    }

    #[test]
    fn a_server_that_plays_keeps_the_hub_awake_while_it_says_so() {
        let only_playing = Awake::WhileActive { minutes: 0 };
        let mut activity = Activity::default();
        activity.pinged(server(1), true, 0);
        assert!(keeps_awake(only_playing, &activity, PLAYING_HOLDS_MS - 1));
        activity.pinged(server(1), true, PLAYING_HOLDS_MS - 1);
        assert!(keeps_awake(
            only_playing,
            &activity,
            2 * PLAYING_HOLDS_MS - 2
        ));
        // A server that stops saying so stops keeping it, and one that
        // says it has stopped does at once.
        assert!(!keeps_awake(only_playing, &activity, 2 * PLAYING_HOLDS_MS));
        let mut activity = Activity::default();
        activity.pinged(server(2), true, 0);
        activity.pinged(server(2), false, 1);
        assert!(!keeps_awake(only_playing, &activity, 2));
    }

    #[test]
    fn a_run_that_does_not_ask_holds_nothing() {
        let mut kept = KeptAwake {
            setting: Awake::Always,
            ..KeptAwake::default()
        };
        assert!(!kept.asks);
        assert!(!kept.follow_the_rule(0));
        assert!(!kept.held());
    }

    #[test]
    fn always_and_never_do_not_ask_what_linked_servers_do() {
        let mut activity = Activity::default();
        assert!(keeps_awake(Awake::Always, &activity, 0));
        activity.pinged(server(1), true, 0);
        activity.called(server(1), 0);
        assert!(!keeps_awake(Awake::Never, &activity, 1));
    }
}
