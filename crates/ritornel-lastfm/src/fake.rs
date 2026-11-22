//! A fake Last.fm on a loopback socket, for tests: the history of one
//! invented account, its loved tracks, the desktop sign-in, scrobbling and
//! "now playing", answered in the shapes the real service uses. A scrobble it accepts
//! joins the history, as on the real service, so a test can see its echo
//! come back through an import.
//!
//! It is small on purpose: one thread, one request a connection, and only
//! the methods this crate calls.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::{Value, json};

/// The API key a fake accepts.
pub const API_KEY: &str = "FAKE_API_KEY_00000000000000000000";
/// The API secret a test signs with.
pub const API_SECRET: &str = "FAKE_API_SECRET_000000000000000000";
/// The session key a fake hands out.
pub const SESSION_KEY: &str = "FAKE_SESSION_KEY_00000000000000000";
/// The token a fake hands out.
pub const TOKEN: &str = "FAKE_TOKEN_000000000000000000000000";

/// One scrobble of the fake's history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FakeScrobble {
    /// When, in seconds.
    pub at_s: u64,
    /// The artist.
    pub artist: String,
    /// The album, or empty for none.
    pub album: String,
    /// The title.
    pub title: String,
}

impl FakeScrobble {
    /// A scrobble of `title` by `artist` on `album`.
    pub fn new(at_s: u64, artist: &str, album: &str, title: &str) -> Self {
        Self {
            at_s,
            artist: artist.to_owned(),
            album: album.to_owned(),
            title: title.to_owned(),
        }
    }
}

/// One request the fake answered: its method and its parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seen {
    /// The Last.fm method.
    pub method: String,
    /// Every parameter, query and form together.
    pub params: HashMap<String, String>,
}

#[derive(Debug, Default)]
struct State {
    user: String,
    history: Vec<FakeScrobble>,
    loved: Vec<(String, String, u64)>,
    /// How many times `auth.getSession` answers "not approved yet" before
    /// it hands out the session.
    unapproved: u32,
    /// Plays sent to `track.scrobble`.
    scrobbled: Vec<FakeScrobble>,
    seen: Vec<Seen>,
    reply_timestamps: HashMap<usize, u64>,
    now_playing_error: Option<u32>,
}

/// A fake Last.fm, serving until it is dropped.
#[derive(Debug)]
pub struct FakeLastfm {
    addr: SocketAddr,
    state: Arc<Mutex<State>>,
}

fn lock(state: &Mutex<State>) -> std::sync::MutexGuard<'_, State> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

impl FakeLastfm {
    /// Starts serving `user`'s history.
    pub fn start(user: &str, history: Vec<FakeScrobble>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let addr = listener.local_addr().unwrap();
        let state = Arc::new(Mutex::new(State {
            user: user.to_owned(),
            history,
            ..State::default()
        }));
        let serving = Arc::clone(&state);
        std::thread::Builder::new()
            .name("fake-lastfm".to_owned())
            .spawn(move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else { continue };
                    let state = Arc::clone(&serving);
                    std::thread::spawn(move || answer(stream, &state));
                }
            })
            .unwrap();
        Self { addr, state }
    }

    /// The root to point a client at.
    pub fn base_url(&self) -> String {
        format!("http://{}/2.0/", self.addr)
    }

    /// Adds scrobbles to the history.
    pub fn add(&self, scrobbles: impl IntoIterator<Item = FakeScrobble>) {
        lock(&self.state).history.extend(scrobbles);
    }

    /// Removes the scrobbles at this time from the history, as an edit at
    /// Last.fm would.
    pub fn remove_at(&self, at_s: u64) {
        lock(&self.state).history.retain(|s| s.at_s != at_s);
    }

    /// Sets the loved tracks: artist, title and when.
    pub fn set_loved(&self, loved: Vec<(&str, &str, u64)>) {
        lock(&self.state).loved = loved
            .into_iter()
            .map(|(a, t, at)| (a.to_owned(), t.to_owned(), at))
            .collect();
    }

    /// Makes `auth.getSession` say "not approved yet" this many times.
    pub fn approve_after(&self, polls: u32) {
        lock(&self.state).unapproved = polls;
    }

    /// The plays sent to `track.scrobble`, in order.
    pub fn scrobbled(&self) -> Vec<FakeScrobble> {
        lock(&self.state).scrobbled.clone()
    }

    /// What each `track.updateNowPlaying` sent, in order.
    pub fn now_playing(&self) -> Vec<HashMap<String, String>> {
        lock(&self.state)
            .seen
            .iter()
            .filter(|seen| seen.method == "track.updateNowPlaying")
            .map(|seen| seen.params.clone())
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn set_reply_timestamps(&self, changes: &[(usize, u64)]) {
        lock(&self.state).reply_timestamps = changes.iter().copied().collect();
    }

    #[cfg(test)]
    pub(crate) fn refuse_now_playing(&self, code: u32) {
        lock(&self.state).now_playing_error = Some(code);
    }

    /// Every request answered, in order.
    pub fn seen(&self) -> Vec<Seen> {
        lock(&self.state).seen.clone()
    }

    /// How many scrobbles the history holds.
    pub fn total(&self) -> usize {
        lock(&self.state).history.len()
    }
}

fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("00");
                out.push(u8::from_str_radix(hex, 16).unwrap_or(b'?'));
                i += 2;
            }
            byte => out.push(byte),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn params(text: &str, into: &mut HashMap<String, String>) {
    for pair in text.split('&').filter(|pair| !pair.is_empty()) {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        into.insert(decode(name), decode(value));
    }
}

fn answer(stream: TcpStream, state: &Mutex<State>) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let target = line.split_whitespace().nth(1).unwrap_or("/").to_owned();
    let mut length = 0;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).is_err() || header.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.trim().eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0; length];
    if reader.read_exact(&mut body).is_err() {
        return;
    }
    let mut all = HashMap::new();
    if let Some((_, query)) = target.split_once('?') {
        params(query, &mut all);
    }
    params(&String::from_utf8_lossy(&body), &mut all);
    let (status, reply) = respond(state, all);
    let reply = reply.to_string();
    let mut stream = stream;
    let _ = write!(
        stream,
        "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
        reply.len()
    );
    let _ = stream.flush();
}

fn error(code: u32, message: &str) -> (u16, Value) {
    (200, json!({"error": code, "message": message}))
}

fn number(params: &HashMap<String, String>, name: &str) -> Option<u64> {
    params.get(name).and_then(|value| value.parse().ok())
}

fn respond(state: &Mutex<State>, params: HashMap<String, String>) -> (u16, Value) {
    let mut state = lock(state);
    let method = params.get("method").cloned().unwrap_or_default();
    state.seen.push(Seen {
        method: method.clone(),
        params: params.clone(),
    });
    if params.get("api_key").map(String::as_str) != Some(API_KEY) {
        return (403, json!({"error": 10, "message": "Invalid API key"}));
    }
    match method.to_ascii_lowercase().as_str() {
        "user.getrecenttracks" => recent(&state, &params),
        "user.getlovedtracks" => loved(&state, &params),
        "auth.gettoken" => (200, json!({"token": TOKEN})),
        "auth.getsession" => {
            if params.get("token").map(String::as_str) != Some(TOKEN) {
                return (
                    403,
                    json!({"error": 4, "message": "Invalid authentication token"}),
                );
            }
            if state.unapproved > 0 {
                state.unapproved -= 1;
                return error(14, "This token has not been authorized");
            }
            let user = state.user.clone();
            (
                200,
                json!({"session": {"name": user, "key": SESSION_KEY, "subscriber": 0}}),
            )
        }
        "track.scrobble" => {
            if params.get("sk").map(String::as_str) != Some(SESSION_KEY) {
                return error(9, "Invalid session key");
            }
            let mut rows = Vec::new();
            for i in 0..50 {
                let get = |name: &str| params.get(&format!("{name}[{i}]")).cloned();
                let (Some(artist), Some(title), Some(at)) =
                    (get("artist"), get("track"), get("timestamp"))
                else {
                    break;
                };
                let scrobble = FakeScrobble {
                    at_s: at.parse().unwrap_or(0),
                    artist: artist.clone(),
                    album: get("album").unwrap_or_default(),
                    title: title.clone(),
                };
                state.scrobbled.push(scrobble.clone());
                state.history.push(scrobble.clone());
                rows.push(json!({
                    "artist": {"corrected": "0", "#text": artist},
                    "album": {"corrected": "0", "#text": scrobble.album},
                    "track": {"corrected": "0", "#text": title},
                    "albumArtist": {"corrected": "0", "#text": get("albumArtist").unwrap_or_default()},
                    "ignoredMessage": {"code": "0", "#text": ""},
                    "timestamp": state.reply_timestamps.get(&i).map_or(at, u64::to_string),
                }));
            }
            let accepted = rows.len();
            (
                200,
                json!({"scrobbles": {"scrobble": rows, "@attr": {"accepted": accepted, "ignored": 0}}}),
            )
        }
        "track.updatenowplaying" => {
            if let Some(code) = state.now_playing_error {
                return error(code, "Authentication failed");
            }
            if params.get("sk").map(String::as_str) != Some(SESSION_KEY) {
                return error(9, "Invalid session key");
            }
            let text = |name: &str| params.get(name).cloned().unwrap_or_default();
            (
                200,
                json!({"nowplaying": {
                    "artist": {"corrected": "0", "#text": text("artist")},
                    "track": {"corrected": "0", "#text": text("track")},
                    "album": {"corrected": "0", "#text": text("album")},
                    "albumArtist": {"corrected": "0", "#text": text("albumArtist")},
                    "ignoredMessage": {"code": "0", "#text": ""},
                }}),
            )
        }
        _ => error(3, "Invalid Method"),
    }
}

fn recent(state: &State, params: &HashMap<String, String>) -> (u16, Value) {
    if params.get("user") != Some(&state.user) {
        return (404, json!({"error": 6, "message": "User not found"}));
    }
    let from = number(params, "from").unwrap_or(0);
    let to = number(params, "to").unwrap_or(u64::MAX);
    let limit = number(params, "limit").unwrap_or(50).max(1);
    let page = number(params, "page").unwrap_or(1).max(1);
    let mut rows: Vec<&FakeScrobble> = state
        .history
        .iter()
        .filter(|s| s.at_s >= from && s.at_s < to)
        .collect();
    // Newest first; a stable order among scrobbles of one second.
    rows.sort_by_key(|s| std::cmp::Reverse(s.at_s));
    let total = rows.len() as u64;
    let pages = total.div_ceil(limit);
    let start = usize::try_from((page - 1) * limit).unwrap_or(usize::MAX);
    let tracks: Vec<Value> = rows
        .iter()
        .skip(start)
        .take(usize::try_from(limit).unwrap_or(usize::MAX))
        .map(|s| {
            json!({
                "artist": {"mbid": "", "#text": s.artist},
                "streamable": "0",
                "image": [{"size": "small", "#text": ""}],
                "mbid": "",
                "album": {"mbid": "", "#text": s.album},
                "name": s.title,
                "url": "https://example.invalid/track",
                "date": {"uts": s.at_s.to_string(), "#text": ""},
            })
        })
        .collect();
    (
        200,
        json!({"recenttracks": {"track": tracks, "@attr": {
            "user": state.user, "totalPages": pages.to_string(), "page": page.to_string(),
            "total": total.to_string(), "perPage": limit.to_string(),
        }}}),
    )
}

fn loved(state: &State, params: &HashMap<String, String>) -> (u16, Value) {
    let limit = number(params, "limit").unwrap_or(50).max(1);
    let page = number(params, "page").unwrap_or(1).max(1);
    let total = state.loved.len() as u64;
    let start = usize::try_from((page - 1) * limit).unwrap_or(usize::MAX);
    let tracks: Vec<Value> = state
        .loved
        .iter()
        .skip(start)
        .take(usize::try_from(limit).unwrap_or(usize::MAX))
        .map(|(artist, title, at)| {
            json!({
                "artist": {"url": "https://example.invalid/artist", "name": artist, "mbid": ""},
                "date": {"uts": at.to_string(), "#text": ""},
                "mbid": "",
                "url": "https://example.invalid/track",
                "name": title,
                "image": [{"size": "small", "#text": ""}],
                "streamable": {"fulltrack": "0", "#text": "0"},
            })
        })
        .collect();
    (
        200,
        json!({"lovedtracks": {"track": tracks, "@attr": {
            "user": state.user, "page": page.to_string(), "perPage": limit.to_string(),
            "totalPages": total.div_ceil(limit).max(1).to_string(), "total": total.to_string(),
        }}}),
    )
}
