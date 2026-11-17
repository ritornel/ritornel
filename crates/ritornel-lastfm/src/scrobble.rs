//! Scrobbling: sending plays to Last.fm, once each, and saying what became
//! of them. What to send and when is the core's (`history_delivery`); this
//! sends one batch and reads the answer. And "now playing": telling Last.fm
//! what is playing, once, with nothing kept.

use std::time::Duration;

use scrobl::client::Play;
use scrobl::{ApiErrorCode, Delivery, Retry, SessionKey};

use crate::Lastfm;

/// The most plays one call sends.
pub const MAX_BATCH: usize = scrobl::client::MAX_BATCH;

/// One play to send, as it is recorded before it is sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToScrobble {
    /// The artist.
    pub artist: String,
    /// The title.
    pub title: String,
    /// The album.
    pub album: Option<String>,
    /// The album's artist, where it is not the track's.
    pub album_artist: Option<String>,
    /// The track's place on the album.
    pub track_number: Option<u32>,
    /// The track's length.
    pub duration_ms: Option<i64>,
    /// When it started, in seconds since the Unix epoch.
    pub timestamp_s: u64,
}

/// What Last.fm did with one play.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// It is recorded, under these names, which are those sent unless
    /// Last.fm corrected them.
    Accepted {
        /// The artist as Last.fm took it.
        artist: String,
        /// The title as Last.fm took it.
        title: String,
    },
    /// It is not recorded, for this reason. Last.fm's codes are kept as
    /// sent and are not to be relied on.
    Ignored {
        /// The code.
        code: u32,
        /// What Last.fm said.
        message: String,
    },
}

/// What a batch that was answered came to: one outcome for each play, in
/// the order the plays were given.
pub type Sent = Vec<Outcome>;

/// Why a batch got no answer that says what became of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unsent {
    /// Nothing left this process, or Last.fm refused it in a way worth
    /// trying again later. Nothing was recorded.
    Later(String),
    /// Last.fm refused the session: the account has to be signed in again.
    /// Nothing was recorded.
    SignInAgain(String),
    /// Last.fm refused it, and sending it again would be refused too.
    Refused(String),
    /// It was sent, or may have been, and no answer says whether it was
    /// recorded. It is never sent again.
    Unknown(String),
}

/// A track that has begun to play, to show as playing now. Its names are
/// those its scrobble would carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Playing {
    /// The artist.
    pub artist: String,
    /// The title.
    pub title: String,
    /// The album.
    pub album: Option<String>,
    /// The album's artist, where it is not the track's.
    pub album_artist: Option<String>,
    /// The track's place on the album.
    pub track_number: Option<u32>,
    /// The track's length.
    pub duration_ms: Option<i64>,
}

/// Why Last.fm was not told what is playing. Nothing is sent again: a
/// later "now playing" is simply a newer one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotShown {
    /// Last.fm refused the session: the account has to be signed in again.
    SignInAgain(String),
    /// Anything else.
    Failed(String),
}

fn play(scrobble: &ToScrobble) -> Play {
    let mut play = Play::new(&scrobble.artist, &scrobble.title, scrobble.timestamp_s);
    if let Some(album) = &scrobble.album {
        play = play.album(album);
    }
    if let Some(artist) = &scrobble.album_artist {
        play = play.album_artist(artist);
    }
    if let Some(number) = scrobble.track_number {
        play = play.track_number(number);
    }
    if let Some(ms) = scrobble.duration_ms.and_then(|ms| u64::try_from(ms).ok()) {
        play = play.duration(Duration::from_millis(ms));
    }
    play
}

impl Lastfm {
    /// Tells Last.fm that the account whose session this is is playing
    /// `playing` now, once.
    pub fn now_playing(&self, session: &SessionKey, playing: &Playing) -> Result<(), NotShown> {
        let writer = self.writer.with_session(session.clone());
        let mut query = writer.now_playing(&playing.artist, &playing.title);
        if let Some(album) = &playing.album {
            query = query.album(album);
        }
        if let Some(artist) = &playing.album_artist {
            query = query.album_artist(artist);
        }
        if let Some(number) = playing.track_number {
            query = query.track_number(number);
        }
        if let Some(ms) = playing.duration_ms.and_then(|ms| u64::try_from(ms).ok()) {
            query = query.duration(Duration::from_millis(ms));
        }
        match self.block(query.send()) {
            Ok(_) => Ok(()),
            Err(error) if error.api_code() == Some(ApiErrorCode::INVALID_SESSION_KEY) => {
                Err(NotShown::SignInAgain(error.to_string()))
            }
            Err(error) => Err(NotShown::Failed(error.to_string())),
        }
    }

    /// Sends up to [`MAX_BATCH`] plays as the account whose session this
    /// is, once. Their times must differ: Last.fm's answer ties an outcome
    /// to a play by time alone.
    pub fn scrobble(&self, session: &SessionKey, plays: &[ToScrobble]) -> Result<Sent, Unsent> {
        let writer = self.writer.with_session(session.clone());
        let batch: Vec<Play> = plays.iter().map(play).collect();
        let report = match self.block(writer.scrobble(&batch).send()) {
            Ok(report) => report.into_value(),
            Err(error) => {
                let message = error.to_string();
                return Err(match (error.delivery(), error.retry()) {
                    (Some(Delivery::Unknown), _) => Unsent::Unknown(message),
                    (_, Retry::AfterReauthentication) => Unsent::SignInAgain(message),
                    (Some(Delivery::NotSent), _) | (_, Retry::Later | Retry::AfterBackoff) => {
                        Unsent::Later(message)
                    }
                    _ => Unsent::Refused(message),
                });
            }
        };
        let mut outcomes: Vec<Option<&scrobl::model::Outcome>> =
            report.outcomes().iter().map(Some).collect();
        let mut sent = Vec::with_capacity(plays.len());
        for scrobble in plays {
            let found = outcomes.iter_mut().find(|outcome| {
                outcome.is_some_and(|outcome| outcome.timestamp() == Some(scrobble.timestamp_s))
            });
            let Some(outcome) = found.and_then(Option::take) else {
                return Err(Unsent::Unknown(
                    "Last.fm's answer does not account for every play".to_owned(),
                ));
            };
            sent.push(match outcome.ignored() {
                Some(ignored) => Outcome::Ignored {
                    code: ignored.code().get(),
                    message: ignored.message().to_owned(),
                },
                None => Outcome::Accepted {
                    artist: outcome.artist().name().to_owned(),
                    title: outcome.track().name().to_owned(),
                },
            });
        }
        Ok(sent)
    }
}
