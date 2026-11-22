//! Last.fm: a history source, the sign-in that gives a session, and the
//! place plays are scrobbled to and what is playing is shown. It stands on the `scrobl` crate,
//! which makes every request, checks every page of a history against the
//! others, and keeps credentials out of every error and `Debug`. See
//! "Last.fm" in docs/internals/history.md and "Shipped crates and Cargo
//! features" in docs/internals/plugins.md.
//!
//! This crate does no storage. It reads and sends, and hands what it reads
//! to the core through [`ritornel_source::Sink`]. A server built without
//! the `lastfm` feature links none of it.
//!
//! What it divides with `scrobl`: the library reads one window of a
//! history and checks it; which windows to read, and when, is this crate's
//! ([`source`]). That follows `scrobl`'s own command-line tool: the history
//! is cut into spans of at most a few thousand scrobbles by asking Last.fm
//! how many each holds, each span is read as one checked scan, and every
//! sync reads again the fortnight before where the last one stopped.

// Tests may unwrap and index. Everything else is held to the workspace's
// lints, which CI denies.
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

mod credentials;
#[cfg(any(test, feature = "fake"))]
pub mod fake;
mod platform;
mod scrobble;
mod source;

use std::future::Future;
use std::time::Duration;

use scrobl::Client;

pub use credentials::{ApiCredentials, CREDENTIALS_FILE, KEY_VARIABLE, SECRET_VARIABLE};
pub use scrobble::{DAILY_LIMIT, MAX_BATCH, NotShown, Outcome, Playing, Sent, ToScrobble, Unsent};
/// The session key and the sign-in token, as `scrobl` keeps them: no
/// `Display`, and a redacted `Debug`.
pub use scrobl::{AuthToken, SessionKey};
pub use source::LastfmSource;

/// The user agent every request is sent with. It names the program and its
/// version and nothing about whoever runs it.
pub const USER_AGENT: &str = concat!("ritornel/", env!("CARGO_PKG_VERSION"));

/// How this crate talks to Last.fm. The defaults are for the real service;
/// the tests shrink them.
#[derive(Debug, Clone)]
pub struct Settings {
    /// For tests alone: a loopback address to send everything to instead
    /// of Last.fm. `scrobl` refuses any other.
    pub base_url: Option<String>,
    /// The least time between two requests. One second, `scrobl`'s
    /// default.
    pub min_interval: Duration,
    /// A span in which Last.fm counts more scrobbles than this is halved,
    /// unless it is one second.
    pub chunk: u64,
    /// Rows asked for on each page of a scan, 1 to 200.
    pub page_size: u32,
    /// How far back from where the last sync stopped every sync reads
    /// again, in seconds: a scrobble can arrive this long after it was
    /// played.
    pub recent_s: u64,
}

impl Default for Settings {
    /// Spans of at most 5000 scrobbles, pages of 200, and a fortnight read
    /// again, as `scrobl`'s tool does: Last.fm ignores a scrobble dated
    /// more than about two weeks back.
    fn default() -> Self {
        Self {
            base_url: None,
            min_interval: Duration::from_secs(1),
            chunk: 5_000,
            page_size: 200,
            recent_s: 14 * 86_400,
        }
    }
}

/// Why a call to Last.fm failed. `Display` is one line that is safe to log
/// and to show: `scrobl` scrubs every credential it holds from its errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(String);

impl Error {
    fn of(error: &scrobl::Error) -> Self {
        Self(error.to_string())
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

/// The runtime is taken only by `drop`, after which nothing is called.
#[cold]
fn unreachable_runtime() -> ! {
    unreachable!("the client's runtime is gone")
}

impl Drop for Lastfm {
    /// Lets the runtime go without waiting for it, so that the client may
    /// be dropped anywhere, an async task included, where dropping a
    /// runtime that waits would panic.
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

/// A sign-in in progress: the address where the listener approves it, and
/// the token that is exchanged for a session once they have. Its `Debug`
/// shows neither.
#[derive(Clone)]
pub struct SignIn {
    /// Where to approve it. It carries the API key and the token, as
    /// Last.fm's desktop flow has it, so it is shown to the person signing
    /// in and never logged.
    pub approval_url: String,
    /// The token. Its `Debug` is redacted.
    pub token: AuthToken,
}

impl std::fmt::Debug for SignIn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SignIn(<redacted>)")
    }
}

/// A Last.fm account this server is signed in to.
#[derive(Debug, Clone)]
pub struct Account {
    /// The account's name, as Last.fm spells it.
    pub name: String,
    /// The session key. Its `Debug` is redacted, and it has no `Display`.
    pub key: SessionKey,
    /// Whether the account is a subscriber's.
    pub subscriber: bool,
}

/// Last.fm, with this server's API key and secret: a runtime for
/// `scrobl`'s async client, and the client.
#[derive(Debug)]
pub struct Lastfm {
    /// Always there until the client is dropped, which takes it.
    runtime: Option<tokio::runtime::Runtime>,
    client: Client,
    writer: scrobl::Writer,
    settings: Settings,
}

impl Lastfm {
    /// Builds the client. Nothing is sent.
    pub fn new(credentials: ApiCredentials, settings: Settings) -> Result<Self, Error> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("ritornel-lastfm")
            .enable_all()
            .build()
            .map_err(|error| Error(format!("cannot start Last.fm's runtime: {error}")))?;
        let (key, secret) = credentials.into_parts();
        let mut builder = Client::builder(key)
            .secret(secret)
            .user_agent(USER_AGENT)
            .min_interval(settings.min_interval);
        if let Some(url) = &settings.base_url {
            builder = builder.base_url(url.clone());
        }
        let writer = builder.build_writer().map_err(|error| Error::of(&error))?;
        // The read-only client inside the writer shares its pacing clock.
        let client = Client::clone(&writer);
        Ok(Self {
            runtime: Some(runtime),
            client,
            writer,
            settings,
        })
    }

    /// Runs a future on this crate's runtime and waits for it, from a
    /// thread that is not one of that runtime's.
    fn block<T>(&self, future: impl Future<Output = T>) -> T {
        match &self.runtime {
            Some(runtime) => runtime.block_on(future),
            None => unreachable_runtime(),
        }
    }

    /// Starts the desktop sign-in: asks Last.fm for a token, and makes the
    /// address where the listener approves it.
    pub fn begin_sign_in(&self) -> Result<SignIn, Error> {
        let auth = self.client.auth();
        let token = self
            .block(auth.token().send())
            .map_err(|error| Error::of(&error))?
            .into_value();
        Ok(SignIn {
            approval_url: auth.approval_url(&token),
            token,
        })
    }

    /// Exchanges an approved token for the listener's session. `None`
    /// while they have not approved it yet: the same call can be made
    /// again.
    pub fn finish_sign_in(&self, token: &AuthToken) -> Result<Option<Account>, Error> {
        let session = self
            .block(self.client.auth().session(token).send())
            .map_err(|error| Error::of(&error))?;
        Ok(session.map(|session| {
            let session = session.into_value();
            Account {
                name: session.name().to_owned(),
                subscriber: session.subscriber(),
                key: session.into_key(),
            }
        }))
    }

    /// The history of `account`, as a source.
    pub fn source(&self, account: &str) -> LastfmSource<'_> {
        LastfmSource::new(self, account)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{API_KEY, API_SECRET, FakeLastfm, SESSION_KEY, TOKEN};

    fn lastfm(fake: &FakeLastfm) -> Lastfm {
        Lastfm::new(
            ApiCredentials::new(API_KEY, API_SECRET),
            Settings {
                base_url: Some(fake.base_url()),
                min_interval: Duration::ZERO,
                ..Settings::default()
            },
        )
        .unwrap()
    }

    #[test]
    fn signing_in_waits_for_approval_and_gives_the_account_and_a_hidden_key() {
        let fake = FakeLastfm::start("someone", Vec::new());
        fake.approve_after(2);
        let lastfm = lastfm(&fake);
        let sign_in = lastfm.begin_sign_in().unwrap();
        assert!(sign_in.approval_url.contains(TOKEN));
        assert!(!format!("{sign_in:?}").contains(TOKEN));
        assert!(lastfm.finish_sign_in(&sign_in.token).unwrap().is_none());
        assert!(lastfm.finish_sign_in(&sign_in.token).unwrap().is_none());
        let account = lastfm.finish_sign_in(&sign_in.token).unwrap().unwrap();
        assert_eq!(account.name, "someone");
        assert_eq!(account.key.expose(), SESSION_KEY);
        assert!(!format!("{account:?}").contains(SESSION_KEY));
        // Both calls are signed with the secret, which is never sent.
        for seen in fake.seen() {
            assert!(seen.params.contains_key("api_sig"), "{}", seen.method);
            assert!(!seen.params.values().any(|value| value == API_SECRET));
        }
    }

    #[test]
    fn a_batch_is_sent_once_and_each_play_gets_its_own_outcome() {
        let fake = FakeLastfm::start("someone", Vec::new());
        let lastfm = lastfm(&fake);
        let plays = [
            ToScrobble {
                artist: "Invented Artist".to_owned(),
                title: "First".to_owned(),
                album: Some("Invented Album".to_owned()),
                album_artist: None,
                track_number: Some(1),
                duration_ms: Some(200_000),
                timestamp_s: 1_800_000_000,
            },
            ToScrobble {
                artist: "Invented Artist".to_owned(),
                title: "Second".to_owned(),
                album: None,
                album_artist: None,
                track_number: None,
                duration_ms: None,
                timestamp_s: 1_800_000_300,
            },
        ];
        let sent = lastfm
            .scrobble(&SessionKey::new(SESSION_KEY), &plays)
            .unwrap();
        assert_eq!(
            sent,
            vec![
                Outcome::Accepted {
                    artist: "Invented Artist".to_owned(),
                    title: "First".to_owned()
                },
                Outcome::Accepted {
                    artist: "Invented Artist".to_owned(),
                    title: "Second".to_owned()
                },
            ]
        );
        assert_eq!(fake.scrobbled().len(), 2);
        let refused = lastfm
            .scrobble(&SessionKey::new("WRONG"), &plays)
            .unwrap_err();
        assert!(matches!(refused, Unsent::SignInAgain(_)), "{refused:?}");
        assert_eq!(fake.scrobbled().len(), 2);
        fake.set_reply_timestamps(&[(0, 42)]);
        let partial = lastfm
            .scrobble(&SessionKey::new(SESSION_KEY), &plays)
            .unwrap();
        assert!(matches!(&partial[1], Outcome::Accepted { title, .. } if title == "Second"));
        assert!(matches!(partial[0], Outcome::Unknown));
        fake.set_reply_timestamps(&[(0, plays[1].timestamp_s)]);
        let ambiguous = lastfm
            .scrobble(&SessionKey::new(SESSION_KEY), &plays)
            .unwrap();
        assert_eq!(ambiguous, [Outcome::Unknown, Outcome::Unknown]);
        fake.set_reply_timestamps(&[(0, plays[1].timestamp_s), (1, 42)]);
        let shifted = lastfm
            .scrobble(&SessionKey::new(SESSION_KEY), &plays)
            .unwrap();
        assert_eq!(shifted, [Outcome::Unknown, Outcome::Unknown]);
    }

    #[test]
    fn album_details_distinguish_two_plays_of_the_same_song() {
        let fake = FakeLastfm::start("someone", Vec::new());
        let lastfm = lastfm(&fake);
        let play = ToScrobble {
            artist: "Invented Artist".into(),
            title: "Invented Song".into(),
            album: Some("First Record".into()),
            album_artist: Some("First Ensemble".into()),
            track_number: None,
            duration_ms: None,
            timestamp_s: 1_800_000_000,
        };
        for second in [
            ToScrobble {
                album: Some("Second Record".into()),
                ..play.clone()
            },
            ToScrobble {
                album_artist: Some("Second Ensemble".into()),
                ..play.clone()
            },
        ] {
            let second = ToScrobble {
                timestamp_s: play.timestamp_s + 300,
                ..second
            };
            fake.set_reply_timestamps(&[(0, second.timestamp_s), (1, play.timestamp_s)]);
            let sent = lastfm
                .scrobble(&SessionKey::new(SESSION_KEY), &[play.clone(), second])
                .unwrap();
            assert_eq!(sent, [Outcome::Unknown, Outcome::Unknown]);
        }
    }

    #[test]
    fn a_batch_rejected_before_sending_is_not_retried() {
        let fake = FakeLastfm::start("someone", Vec::new());
        let result = lastfm(&fake).scrobble(&SessionKey::new(SESSION_KEY), &[]);
        assert!(matches!(result, Err(Unsent::Refused(_))), "{result:?}");
        assert!(fake.seen().is_empty());
    }

    #[test]
    fn reads_and_writes_observe_one_interval() {
        let fake = FakeLastfm::start("someone", Vec::new());
        let interval = Duration::from_millis(200);
        let service = Lastfm::new(
            ApiCredentials::new(API_KEY, API_SECRET),
            Settings {
                base_url: Some(fake.base_url()),
                min_interval: interval,
                ..Settings::default()
            },
        )
        .unwrap();
        let started = std::time::Instant::now();
        service.begin_sign_in().unwrap();
        let play = Playing {
            artist: "Invented".into(),
            title: "One".into(),
            album: None,
            album_artist: None,
            track_number: None,
            duration_ms: None,
        };
        service
            .now_playing(&SessionKey::new(SESSION_KEY), &play)
            .unwrap();
        assert!(started.elapsed() >= interval);
    }

    #[test]
    fn now_playing_is_sent_once_with_the_names_a_scrobble_carries() {
        let fake = FakeLastfm::start("someone", Vec::new());
        let lastfm = lastfm(&fake);
        let playing = Playing {
            artist: "Invented Artist".to_owned(),
            title: "First".to_owned(),
            album: Some("Invented Album".to_owned()),
            album_artist: Some("Various Invented".to_owned()),
            track_number: Some(1),
            duration_ms: Some(200_400),
        };
        lastfm
            .now_playing(&SessionKey::new(SESSION_KEY), &playing)
            .unwrap();
        let sent = fake.now_playing();
        assert_eq!(sent.len(), 1);
        let field = |name: &str| sent[0].get(name).map(String::as_str);
        assert_eq!(field("artist"), Some("Invented Artist"));
        assert_eq!(field("track"), Some("First"));
        assert_eq!(field("album"), Some("Invented Album"));
        assert_eq!(field("albumArtist"), Some("Various Invented"));
        assert_eq!(field("trackNumber"), Some("1"));
        assert_eq!(field("duration"), Some("200"));
        assert_eq!(field("timestamp"), None);
        assert!(fake.scrobbled().is_empty(), "nothing is scrobbled");

        let refused = lastfm
            .now_playing(&SessionKey::new("WRONG"), &playing)
            .unwrap_err();
        assert!(matches!(refused, NotShown::SignInAgain(_)), "{refused:?}");
        fake.refuse_now_playing(4);
        let revoked = lastfm.now_playing(&SessionKey::new(SESSION_KEY), &playing);
        assert!(
            matches!(revoked, Err(NotShown::SignInAgain(_))),
            "{revoked:?}"
        );
        assert_eq!(fake.now_playing().len(), 3, "sent once, never again");
    }
}
