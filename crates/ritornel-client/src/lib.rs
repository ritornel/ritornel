//! Rust client for the contract, plus find, start and stop of a local
//! server. Every Rust program that talks to a server does it through this
//! crate: `ritornelctl`, the desktop's shell, and the contract tests.
//!
//! - [`Client`] is a connection: a typed method for each call, and the
//!   events the server sends. It runs over a real socket
//!   ([`Client::connect`]) or over a pair of channels in memory
//!   ([`Client::over_memory`]), and encodes every frame as JSON on both.
//! - [`local`] finds the server on this machine through `server.json`.
//!   Starting and stopping one arrive with the desktop's launcher.
//! - [`descriptor`], [`health`], [`ticket`] and [`get`] are the HTTP routes.
//!
//! The client never opens a database: it does not contain the code to
//! (docs/design/server.md, decisions 1 and 7).

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

// Nothing here logs through `log`. The dependency is what compiles the
// TRACE lines out of the WebSocket library: see Cargo.toml.
use log as _;

mod client;
mod credential;
mod error;
mod http;
pub mod local;
mod transport;

pub use client::Client;
pub use credential::Credential;
pub use error::{Error, NotRunning};
pub use http::{Answer, descriptor, get, health, ticket};
/// The wire types, so that a program needs only this crate to use them.
pub use ritornel_contract as contract;
