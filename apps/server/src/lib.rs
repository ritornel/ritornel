//! `ritornel-server`: config, lock, listener, and routing from contract calls
//! to engine methods. The binary is a thin `main` over this library, so
//! contract tests can run a real listener inside the test process.
//!
//! A run takes a data directory and its lock ([`HeldDir`]), then starts a
//! [`Server`]: it opens `server.db` through the engine, and `library.db` if
//! it is a library's hub, listens on loopback, says where it is in
//! `server.json`, and answers the contract until it is asked to stop. See
//! "Process lifecycle" and "Data directory" in docs/internals/server.md, and
//! docs/internals/contract.md.
//!
//! The dispatcher (`dispatch`) takes a decoded frame and a session and knows
//! nothing of transports. The socket (`listener`) and the in-memory
//! transport that tests use (`memory`) are thin codecs over it.

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

mod awake;
mod backup;
mod bounds;
mod cli;
mod config;
mod credential;
mod data_dir;
mod dispatch;
mod error;
mod hub_catalog;
mod imports;
mod invent;
mod jobs;
mod lastfm;
mod library;
mod link;
mod listener;
mod lock;
mod logs;
mod media;
#[cfg(feature = "media-session")]
mod media_session;
mod memory;
mod platform;
mod playback;
mod preparing;
mod serve;
mod server;
mod server_json;
mod service;
mod session;
#[cfg(test)]
mod testing;
mod ticket;
mod tls;
mod transport;
mod uplink;
mod views;
mod watch;

pub use bounds::{
    Bounds, HEADER_WAIT, HUB_CALL_WAIT, MAX_CONNECTIONS, MAX_SESSIONS, STOP_GRACE, TICKET_LIFE,
};
pub use cli::run;
pub use config::{ConfigError, Listen, Origin, Origins, TranscodeConfig};
pub use data_dir::HeldDir;
pub use error::{EXIT_FAILED, EXIT_IN_USE, EXIT_USAGE, Error};
pub use jobs::Run;
pub use lastfm::LastfmRun;
pub use playback::PlaybackRun;
/// How the program reads an environment variable. It is passed in, so that a
/// test can say what the environment holds without changing its own.
pub use ritornel_contract::local::Env;
/// The outputs a player can render through.
pub use ritornel_engine::playback::Output;
pub use server::Server;
pub use watch::WatchRun;

/// Opens a TLS connection to the hub at `address` and checks that it holds
/// the identity key `key`, as a satellite does before it sends anything of
/// its link. The integration tests use it to stand in for a satellite.
pub async fn dial_hub(
    address: std::net::SocketAddr,
    key: [u8; 32],
) -> Result<impl ritornel_client::Stream, String> {
    tls::dial(address, key, link::CONNECT_WAIT)
        .await
        .map_err(|error| error.to_string())
}

/// The handshake of [`dial_hub`], on a connection to the hub already made:
/// for a test that holds a connection open before it begins.
pub async fn handshake_with_hub(
    tcp: tokio::net::TcpStream,
    key: [u8; 32],
) -> Result<impl ritornel_client::Stream, String> {
    tls::handshake(tcp, key)
        .await
        .map_err(|error| error.to_string())
}

/// The TLS a hub whose identity key is made from `seed` answers with. The
/// integration tests' relay presents it to stand between a satellite and
/// its hub and read what passes.
pub fn hub_tls(seed: [u8; 32]) -> Result<tokio_rustls::TlsAcceptor, String> {
    tls::acceptor(std::sync::Arc::new(ritornel_engine::Signer::from_seed(
        seed,
    )))
    .map_err(|error| error.to_string())
}
