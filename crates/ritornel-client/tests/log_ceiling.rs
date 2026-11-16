//! The credential stays out of whatever a program that links this client
//! logs, at any level it chooses.
//!
//! The WebSocket library logs the whole upgrade request at TRACE, through
//! the `log` crate, and the request carries the credential. Marking the
//! header sensitive does not stop it. `ritornel-client` therefore sets a
//! static maximum level of DEBUG on `log`, which compiles that line out of
//! every program built with it (Cargo.toml).
//!
//! This test is a binary of its own because a process has one logger, set
//! once. It sets one that keeps everything at the most verbose level.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::sync::Mutex;
use std::time::Duration;

use futures_util::StreamExt;
use log::{LevelFilter, Log, Metadata, Record};
use ritornel_client::{Client, Credential};
use tokio::net::TcpListener;

/// Stands in for the credential. It is made of what a token is made of, so
/// that it travels as one would.
const SENTINEL: &str = "5e471e15e471e15e471e15e471e15e471e15e471e15e471e15e471e15e471e10";

/// Keeps every line any crate logs.
struct Everything(Mutex<Vec<String>>);

impl Log for Everything {
    fn enabled(&self, _: &Metadata<'_>) -> bool {
        true
    }

    fn log(&self, record: &Record<'_>) {
        self.0.lock().unwrap().push(format!(
            "{} {}: {}",
            record.level(),
            record.target(),
            record.args()
        ));
    }

    fn flush(&self) {}
}

static LOGGED: Everything = Everything(Mutex::new(Vec::new()));

#[tokio::test]
async fn the_credential_is_in_nothing_that_is_logged_at_any_level() {
    log::set_logger(&LOGGED).unwrap();
    log::set_max_level(LevelFilter::Trace);

    // A listener on this machine that completes the upgrade, as a server
    // would, and reads until the client goes.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = listener.local_addr().unwrap();
    let serving = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        while let Some(Ok(_)) = socket.next().await {}
    });

    let exchange = async {
        let client = Client::connect(endpoint, &Credential::new(SENTINEL))
            .await
            .unwrap();
        // A frame each way is logged too, at the levels that are left.
        drop(client);
        serving.await.unwrap();
    };
    tokio::time::timeout(Duration::from_secs(60), exchange)
        .await
        .expect("the upgrade and the goodbye did not finish within a minute");

    let logged = LOGGED.0.lock().unwrap().clone();
    // The logger was in place, and the WebSocket library did log through
    // it: this is not passing because nothing was listening.
    assert!(
        logged
            .iter()
            .any(|line| line.contains("tungstenite") && line.contains("handshake done")),
        "{logged:#?}"
    );
    let leaked: Vec<_> = logged
        .iter()
        .filter(|line| line.contains(SENTINEL))
        .collect();
    assert!(
        leaked.is_empty(),
        "the credential was logged. Is the `log` ceiling in ritornel-client's Cargo.toml still \
         there? {leaked:#?}"
    );
    // The ceiling itself: nothing more verbose than DEBUG is compiled in.
    assert_eq!(log::STATIC_MAX_LEVEL, LevelFilter::Debug);
    assert!(logged.iter().all(|line| !line.starts_with("TRACE")));
}
