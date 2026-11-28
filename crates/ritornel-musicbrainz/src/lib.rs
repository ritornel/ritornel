//! MusicBrainz as a [`Catalogue`]: asks its web service about a CD by its
//! disc ID and hands the reply over exactly as it came. See "MusicBrainz
//! lookups" in docs/internals/corrections.md.
//!
//! It only fetches. The core keeps each reply whole and reads it itself, so
//! that a reply kept once can be read again without this crate or the
//! network. What leaves the machine is the disc ID and the fixed inclusions,
//! under a User-Agent that names Ritornel and its version and nothing about
//! who runs it.

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

#[cfg(any(test, feature = "fake"))]
pub mod fake;

use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use ritornel_source::{Catalogue, Error, Question, Reply};

/// The User-Agent every request carries, as MusicBrainz asks: the program,
/// its version and where to learn about it.
pub const USER_AGENT: &str = concat!(
    "Ritornel/",
    env!("CARGO_PKG_VERSION"),
    " ( https://ritornel.app )"
);

/// Where MusicBrainz's web service is.
const BASE_URL: &str = "https://musicbrainz.org/ws/2/";

/// What a disc lookup asks to be included: each release's credits, its
/// album, and its tracks' recordings.
pub const DISC_INCLUDES: &str = "artist-credits+release-groups+recordings";

/// The least time between two requests, as MusicBrainz asks of a client.
const PACE: Duration = Duration::from_secs(1);

/// How long one request may take.
const TIMEOUT: Duration = Duration::from_secs(30);

/// The most a reply may hold. One about a disc, with every release that
/// holds it and their recordings, is far smaller; a larger one is not
/// kept.
const MAX_REPLY_BYTES: usize = 16 * 1024 * 1024;

/// MusicBrainz, with a runtime for reqwest's async client.
#[derive(Debug)]
pub struct MusicBrainz {
    /// Always there until the client is dropped, which takes it.
    runtime: Option<tokio::runtime::Runtime>,
    client: reqwest::Client,
    base_url: String,
    /// The least time between two requests: none for a test's fake.
    pace: Duration,
    /// When the last request was sent.
    last: Mutex<Option<Instant>>,
    stopped: tokio::sync::watch::Sender<bool>,
}

impl Drop for MusicBrainz {
    /// Lets the runtime go without waiting for it, so that the client may
    /// be dropped from any thread.
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

impl MusicBrainz {
    /// Builds the client. Nothing is sent. `test_url` sends every request
    /// to a loopback address instead, with no pause between them; any
    /// other address is refused.
    pub fn new(test_url: Option<&str>) -> Result<Self, Error> {
        let (base_url, pace) = match test_url {
            None => (BASE_URL.to_owned(), PACE),
            Some(url) => (loopback(url)?, Duration::ZERO),
        };
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("ritornel-musicbrainz")
            .enable_all()
            .build()
            .map_err(|error| {
                Error::source(format!("cannot start MusicBrainz's runtime: {error}"))
            })?;
        // One request is one request: no redirect is followed and nothing is
        // sent again behind the pacing's back. A test's loopback address
        // takes no proxy.
        let mut builder = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never());
        if test_url.is_some() {
            builder = builder.no_proxy();
        }
        let client = builder
            .build()
            .map_err(|error| Error::source(format!("cannot make MusicBrainz's client: {error}")))?;
        Ok(Self {
            runtime: Some(runtime),
            client,
            base_url,
            pace,
            last: Mutex::new(None),
            stopped: tokio::sync::watch::Sender::new(false),
        })
    }
}

/// A test's address, if it is plain HTTP to a loopback host with no
/// credentials in it, ending in `/`.
fn loopback(url: &str) -> Result<String, Error> {
    let refused = || Error::source("a test address must be plain HTTP on loopback");
    let parsed = reqwest::Url::parse(url).map_err(|_| refused())?;
    let on_loopback = match parsed.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        Some(url::Host::Domain(name)) => name == "localhost",
        None => false,
    };
    if parsed.scheme() != "http"
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || !on_loopback
    {
        return Err(refused());
    }
    let mut url = parsed.to_string();
    if !url.ends_with('/') {
        url.push('/');
    }
    Ok(url)
}

/// Whether text is a disc ID: 28 characters of MusicBrainz's alphabet.
fn is_disc_id(text: &str) -> bool {
    text.len() == 28
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

impl Catalogue for MusicBrainz {
    fn name(&self) -> &'static str {
        "musicbrainz"
    }

    fn disc(&self, disc_id: &str) -> Question {
        Question {
            kind: "disc",
            key: disc_id.to_owned(),
            includes: DISC_INCLUDES,
        }
    }

    fn ask(&self, question: &Question) -> Result<Reply, Error> {
        if question.kind != "disc"
            || question.includes != DISC_INCLUDES
            || !is_disc_id(&question.key)
        {
            return Err(Error::source("MusicBrainz is asked only about a disc ID"));
        }
        // CD stubs are left out: they are not releases.
        let url = format!(
            "{}discid/{}?inc={}&cdstubs=no&fmt=json",
            self.base_url, question.key, question.includes
        );
        let Some(runtime) = &self.runtime else {
            return Err(Error::source("MusicBrainz's client is closed"));
        };
        // The turn is held until the reply is in, so no two requests are
        // ever under way at once, and each starts a second after the last.
        let mut last = self.last.lock().unwrap_or_else(PoisonError::into_inner);
        let mut stopped = self.stopped.subscribe();
        runtime.block_on(async {
            tokio::select! {
                biased;
                _ = stopped.wait_for(|stopped| *stopped) => Err(Error::source("MusicBrainz lookup stopped")),
                result = async {
                    if let Some(sent) = *last {
                        tokio::time::sleep(self.pace.saturating_sub(sent.elapsed())).await;
                    }
                    *last = Some(Instant::now());
                    self.fetch(&url).await
                } => result,
            }
        })
    }

    fn stop(&self) {
        self.stopped.send_replace(true);
    }
}

impl MusicBrainz {
    async fn fetch(&self, url: &str) -> Result<Reply, Error> {
        let mut response = self
            .client
            .get(url)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|error| {
                let what = if error.is_timeout() {
                    "did not answer in time"
                } else {
                    "could not be reached"
                };
                Error::source(format!("MusicBrainz {what}"))
            })?;
        let status = response.status().as_u16();
        match status {
            200 | 404 => {}
            503 | 429 => {
                return Err(Error::source(format!(
                    "MusicBrainz is busy ({status}): ask again later"
                )));
            }
            400..=499 => {
                return Err(Error::source(format!(
                    "MusicBrainz will not take this request ({status})"
                )));
            }
            _ => {
                return Err(Error::source(format!("MusicBrainz failed ({status})")));
            }
        }
        // Only JSON is a reply: a page from something between here and
        // MusicBrainz is a failure.
        let json = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.to_ascii_lowercase().starts_with("application/json"));
        if !json {
            return Err(Error::source(format!(
                "MusicBrainz's reply ({status}) was not JSON"
            )));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| Error::source("MusicBrainz's reply was cut off"))?
        {
            if bytes.len() + chunk.len() > MAX_REPLY_BYTES {
                return Err(Error::source("MusicBrainz's reply was too large"));
            }
            bytes.extend_from_slice(&chunk);
        }
        let body = String::from_utf8(bytes)
            .map_err(|_| Error::source("MusicBrainz's reply was not UTF-8"))?;
        Ok(Reply { status, body })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::FakeMusicBrainz;

    const DISC: &str = "49HHV7Eb8UKF3aQiNmu1GR8vKTY-";

    #[tokio::test]
    async fn stopping_interrupts_a_request_waiting_for_the_response() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let catalogue = std::sync::Arc::new(MusicBrainz::new(Some(&url)).unwrap());
        let asking = catalogue.clone();
        let request = tokio::task::spawn_blocking(move || asking.ask(&asking.disc(DISC)));
        let (_unanswered, _) = tokio::time::timeout(Duration::from_secs(60), listener.accept())
            .await
            .expect("the catalogue never connected")
            .unwrap();
        catalogue.stop();
        assert_eq!(
            request.await.unwrap().unwrap_err().to_string(),
            "MusicBrainz lookup stopped"
        );
    }

    #[test]
    fn a_disc_is_asked_for_once_with_its_inclusions_and_the_reply_comes_back_whole() {
        let fake = FakeMusicBrainz::start();
        fake.reply(
            DISC,
            200,
            r#"{"id":"49HHV7Eb8UKF3aQiNmu1GR8vKTY-","releases":[]}"#,
        );
        let musicbrainz = MusicBrainz::new(Some(&fake.base_url())).unwrap();
        let question = musicbrainz.disc(DISC);
        let reply = musicbrainz.ask(&question).unwrap();
        assert_eq!(reply.status, 200);
        assert_eq!(
            reply.body,
            r#"{"id":"49HHV7Eb8UKF3aQiNmu1GR8vKTY-","releases":[]}"#
        );
        let seen = fake.seen();
        assert_eq!(seen.len(), 1);
        assert_eq!(
            seen[0].path,
            format!(
                "/ws/2/discid/{DISC}?inc=artist-credits+release-groups+recordings&cdstubs=no&fmt=json"
            )
        );
        assert_eq!(seen[0].user_agent, USER_AGENT);
        assert!(
            USER_AGENT.starts_with("Ritornel/") && USER_AGENT.ends_with("( https://ritornel.app )")
        );
    }

    #[test]
    fn a_reply_too_large_to_keep_is_a_failure() {
        let fake = FakeMusicBrainz::start();
        let large = format!(
            r#"{{"id":"{DISC}","pad":"{}"}}"#,
            "x".repeat(MAX_REPLY_BYTES)
        );
        fake.reply(DISC, 200, &large);
        let musicbrainz = MusicBrainz::new(Some(&fake.base_url())).unwrap();
        let failed = musicbrainz.ask(&musicbrainz.disc(DISC)).unwrap_err();
        assert_eq!(failed.to_string(), "MusicBrainz's reply was too large");
    }

    #[test]
    fn no_such_disc_is_a_reply_and_a_busy_service_is_a_failure() {
        let fake = FakeMusicBrainz::start();
        fake.reply(DISC, 404, r#"{"error":"Not Found"}"#);
        let musicbrainz = MusicBrainz::new(Some(&fake.base_url())).unwrap();
        let reply = musicbrainz.ask(&musicbrainz.disc(DISC)).unwrap();
        assert_eq!(
            (reply.status, reply.body.as_str()),
            (404, r#"{"error":"Not Found"}"#)
        );

        fake.reply(DISC, 503, "slow down");
        let busy = musicbrainz.ask(&musicbrainz.disc(DISC)).unwrap_err();
        assert_eq!(
            busy.to_string(),
            "MusicBrainz is busy (503): ask again later"
        );
        fake.reply(DISC, 400, "bad");
        let refused = musicbrainz.ask(&musicbrainz.disc(DISC)).unwrap_err();
        assert_eq!(
            refused.to_string(),
            "MusicBrainz will not take this request (400)"
        );
        // A page from something in the way is no reply.
        fake.reply_as(
            DISC,
            200,
            "text/html",
            "<html>Sign in to the network</html>",
        );
        let page = musicbrainz.ask(&musicbrainz.disc(DISC)).unwrap_err();
        assert_eq!(page.to_string(), "MusicBrainz's reply (200) was not JSON");
    }

    #[test]
    fn only_a_disc_id_is_sent_and_only_to_loopback_in_a_test() {
        let fake = FakeMusicBrainz::start();
        let musicbrainz = MusicBrainz::new(Some(&fake.base_url())).unwrap();
        let wrong = Question {
            kind: "disc",
            key: "../../somewhere?x=1".to_owned(),
            includes: DISC_INCLUDES,
        };
        assert!(musicbrainz.ask(&wrong).is_err());
        let other = Question {
            includes: "everything",
            ..musicbrainz.disc(DISC)
        };
        assert!(musicbrainz.ask(&other).is_err());
        assert!(fake.seen().is_empty());
        for url in [
            "https://example.com/ws/2/",
            "http://127.0.0.1:@example.com/ws/2/",
            "http://localhost:1@example.com/ws/2/",
            "http://user:secret@127.0.0.1:9/ws/2/",
            "https://127.0.0.1:9/ws/2/",
        ] {
            assert!(MusicBrainz::new(Some(url)).is_err(), "{url}");
        }
        assert_eq!(
            loopback("http://[::1]:9/ws/2").unwrap(),
            "http://[::1]:9/ws/2/"
        );
    }

    #[test]
    fn requests_to_musicbrainz_itself_are_a_second_apart() {
        let fake = FakeMusicBrainz::start();
        let mut musicbrainz = MusicBrainz::new(Some(&fake.base_url())).unwrap();
        musicbrainz.pace = Duration::from_millis(200);
        fake.reply(DISC, 404, "{}");
        let started = Instant::now();
        for _ in 0..3 {
            musicbrainz.ask(&musicbrainz.disc(DISC)).unwrap();
        }
        assert!(started.elapsed() >= Duration::from_millis(400));
        assert_eq!(MusicBrainz::new(None).unwrap().pace, PACE);
    }
}
