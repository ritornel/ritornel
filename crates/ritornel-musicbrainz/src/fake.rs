//! A fake MusicBrainz on a loopback socket, for tests: recorded replies to
//! disc lookups, by disc ID, and what it was asked.
//!
//! It is small on purpose: one request a connection, and only disc
//! lookups. A disc it has no reply for is not found.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex, PoisonError};

/// One request the fake answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seen {
    /// The path and query.
    pub path: String,
    /// The User-Agent it came with.
    pub user_agent: String,
}

#[derive(Debug, Default)]
struct State {
    /// Each disc's status, content type and body.
    replies: HashMap<String, (u16, String, String)>,
    seen: Vec<Seen>,
}

/// A fake MusicBrainz. Its thread serves until the test process ends.
#[derive(Debug)]
pub struct FakeMusicBrainz {
    addr: SocketAddr,
    state: Arc<Mutex<State>>,
}

fn lock(state: &Mutex<State>) -> std::sync::MutexGuard<'_, State> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

impl FakeMusicBrainz {
    /// Starts serving, with no replies yet.
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let addr = listener.local_addr().unwrap();
        let state = Arc::new(Mutex::new(State::default()));
        let serving = Arc::clone(&state);
        std::thread::Builder::new()
            .name("fake-musicbrainz".to_owned())
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
        format!("http://{}/ws/2/", self.addr)
    }

    /// Answers a lookup of `disc_id` with this status and JSON body from
    /// now on.
    pub fn reply(&self, disc_id: &str, status: u16, body: &str) {
        self.reply_as(disc_id, status, "application/json", body);
    }

    /// Answers a lookup of `disc_id` with this status, content type and
    /// body from now on.
    pub fn reply_as(&self, disc_id: &str, status: u16, content_type: &str, body: &str) {
        lock(&self.state).replies.insert(
            disc_id.to_owned(),
            (status, content_type.to_owned(), body.to_owned()),
        );
    }

    /// Every request answered so far, in order.
    pub fn seen(&self) -> Vec<Seen> {
        lock(&self.state).seen.clone()
    }
}

fn answer(stream: TcpStream, state: &Mutex<State>) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut first = String::new();
    if reader.read_line(&mut first).is_err() {
        return;
    }
    let path = first
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_owned();
    let mut user_agent = String::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).is_err() || line.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("user-agent")
        {
            user_agent = value.trim().to_owned();
        }
    }
    let disc = path
        .strip_prefix("/ws/2/discid/")
        .and_then(|rest| rest.split('?').next())
        .unwrap_or_default();
    let (status, content_type, body) = {
        let mut state = lock(state);
        state.seen.push(Seen {
            path: path.clone(),
            user_agent,
        });
        state.replies.get(disc).cloned().unwrap_or_else(|| {
            let not_found = r#"{"error":"Not Found"}"#.to_owned();
            (404, "application/json".to_owned(), not_found)
        })
    };
    let mut stream = stream;
    let _ = write!(
        stream,
        "HTTP/1.1 {status} X\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
}
